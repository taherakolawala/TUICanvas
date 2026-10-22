"""Terminal-driven Duo experiment with protected session reuse.

The browser engine runs only during authentication. Use --browser for the
accepted visible-browser fallback. No browser state or screenshots are logged.
"""

import argparse
from datetime import datetime, timezone
import getpass
import json
from pathlib import Path
import sys
import time
from urllib.parse import urlsplit

from tools.auth_probe import START as UF_START, UF, trusted_url as uf_trusted_url
from tools.auth_ui import clean, duo_view, label, poll_key, render_view, transition_view, LoginView, Status
from tools.auth_session import (CANVAS, ORIGIN, PROFILE, NativeCanvas, SessionError,
                               load_session, profile_id, save_session)

START = UF_START if CANVAS == "ufl.instructure.com" else ORIGIN


def trusted_url(url):
    if CANVAS == "ufl.instructure.com":
        return uf_trusted_url(url)
    parsed = urlsplit(url)
    return parsed.scheme == "https" and parsed.hostname == CANVAS

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private" / ("auth-login-result.json" if CANVAS == "ufl.instructure.com" else f"auth-login-{CANVAS}.json")
# Once the browser is back on Canvas, confirm sign-in with a native API read at
# most this often, and give up on the native read after this long.
NATIVE_CHECK_INTERVAL = 1.5
CANVAS_SETTLE_LIMIT = 20


def write_report(**values):
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps({"version": 1, "observed_at": datetime.now(timezone.utc).isoformat(),
                                  **values}, indent=2) + "\n", encoding="utf-8")


def safe_result(result):
    return {"kind": result.get("kind"), "status": result.get("status")}


def verify_and_save(state, expected_id=None):
    client = NativeCanvas(state)
    result = client.get(PROFILE)
    user_id = profile_id(result)
    if user_id is None:
        return False, safe_result(result)
    if expected_id is not None and user_id != expected_id:
        raise SessionError("Saved session identity changed; use --fresh to sign in explicitly.")
    save_session(client.updated_state(), user_id)
    return True, safe_result(result)


def describe_controls(page):
    """Expose only currently visible controls, with handles bound to this snapshot."""
    controls = []
    messages = []
    for frame in page.frames:
        # Canvas pages are never presented as sign-in prompts.
        if not trusted_url(frame.url) or urlsplit(frame.url).hostname == CANVAS:
            continue
        try:
            messages.append(frame.locator("body").inner_text(timeout=2000)[:12000])
            for handle in frame.locator("button,input:not([type=hidden]),select,a[href],[role=button]").element_handles():
                if not handle.is_visible() or not handle.is_enabled():
                    continue
                info = handle.evaluate("""e => ({
                    tag: e.tagName.toLowerCase(), type: e.type || '',
                    label: e.getAttribute('aria-label') || Array.from(e.labels || []).map(l => l.innerText).join(' ') || e.innerText || e.getAttribute('placeholder') || e.name || e.id || 'control',
                    href: e.tagName === 'A' ? e.href : null,
                    options: e.tagName === 'SELECT' ? Array.from(e.options).map(o => ({label:o.text, value:o.value, disabled:o.disabled})) : []
                })""")
                if info["href"] and not trusted_url(info["href"]):
                    continue
                controls.append((handle, info, frame.url))
        except Exception:
            # Navigation may replace a frame while collecting its controls.
            continue
    return "\n".join(messages), controls


def interact(page, snapshot_url, controls, choice, show_password):
    if page.url != snapshot_url:
        # The screen changed since it was read; the caller re-reads it.
        return
    try:
        index = int(choice) - 1
        if index < 0 or index >= len(controls):
            raise ValueError()
    except ValueError:
        print("Choose an option number, Enter to refresh, w to wait, or q to quit.")
        return
    handle, info, frame_url = controls[index]
    if not trusted_url(frame_url) or not handle.is_visible() or not handle.is_enabled():
        return
    # Handles never retarget an unrelated replacement element.
    if info["tag"] == "select":
        choices = [option for option in info["options"] if not option["disabled"]]
        for n, option in enumerate(choices, 1):
            print(f"  {n}. {clean(option['label'])}")
        try:
            index = int(input("Selection: ")) - 1
            if 0 <= index < len(choices):
                handle.select_option(choices[index]["value"])
        except ValueError:
            print("Invalid selection.")
    elif info["tag"] == "input" and info["type"] not in {"button", "submit", "reset", "checkbox", "radio", "file", "image"}:
        prompt = clean(info["label"])[:160] + ": "
        value = getpass.getpass(prompt) if info["type"] == "password" and not show_password else input(prompt)
        handle.fill(value)
        del value
    elif info["type"] in {"checkbox", "radio"}:
        handle.set_checked(not handle.is_checked() if info["type"] == "checkbox" else True)
    elif info["type"] != "file":
        # A link's target may have changed since the snapshot.
        href = handle.evaluate("e => e.tagName === 'A' ? e.href : null")
        if href and not trusted_url(href):
            print("Link leaves the authentication flow; use browser mode if needed.")
            return
        handle.click(timeout=5000)


def browser_login(args, saved, status=None):
    from playwright.sync_api import sync_playwright

    status = status or Status()
    status.show("Connecting to Canvas")
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel=args.channel, headless=not args.browser)
        context = browser.new_context(storage_state=saved["browser_state"] if saved else None)
        page = context.new_page()
        page.goto(START, wait_until="domcontentloaded", timeout=60000)
        deadline = time.monotonic() + 600
        submitted = False
        last_view = None
        manual_prompt = False
        canvas_since = None
        last_native = 0.0
        outcome = {"kind": "not_checked", "status": None}
        url_since = (None, 0.0)
        try:
            while time.monotonic() < deadline:
                page.wait_for_timeout(250)
                url = page.url
                parsed = urlsplit(url)
                host = parsed.hostname
                if not trusted_url(url):
                    raise SessionError("Login reached an unexpected origin. Browser fallback may need review.")
                if url != url_since[0]:
                    url_since = (url, time.monotonic())
                if host == CANVAS:
                    # Back on Canvas. Never present Canvas's own page controls as a
                    # login prompt; /login/* pages are SAML hand-off steps.
                    if not args.browser and poll_key() == "q":
                        return 1
                    if parsed.path.startswith("/login"):
                        continue
                    status.show("Opening Canvas")
                    # Confirm with a native API read rather than page markup,
                    # which Canvas changes.
                    now = time.monotonic()
                    if canvas_since is None:
                        canvas_since = now
                    if now - last_native >= NATIVE_CHECK_INTERVAL:
                        last_native = now
                        ok, outcome = verify_and_save(context.storage_state())
                        if ok:
                            write_report(stage="authenticated", native_profile=outcome, session_saved=True, mode="browser" if args.browser else "terminal")
                            status.done("Signed in. Your session is saved for next time.")
                            return 0
                    if now - canvas_since < CANVAS_SETTLE_LIMIT:
                        continue
                    # Preserve the browser session so a failed native transport
                    # experiment does not force another username/password/Duo cycle.
                    save_session(context.storage_state(), None)
                    write_report(stage="browser_authenticated_native_pending", native_profile=outcome, session_saved=True)
                    status.done("Canvas opened, but reading it directly failed. The browser session was saved.", ok=False)
                    return 2
                canvas_since = None
                if args.browser:
                    status.show("Finish signing in in the browser window")
                    continue
                username_input = page.locator('input[name="j_username"]')
                password_input = page.locator('input[name="j_password"]')
                if host == UF and username_input.count() and password_input.count() and username_input.is_visible():
                    status.clear()
                    if submitted:
                        print("UF did not accept that sign-in. Nothing is retried automatically.")
                        if input("Press r to try again, or Enter to stop: ").strip().lower() != "r":
                            return 1
                    print("Sign in with your GatorLink account")
                    username = input("  Username: ").strip()
                    password = input("  Password (visible): ") if args.show_password else getpass.getpass("  Password: ")
                    if not username or not password:
                        print("Username and password are both required; nothing was sent.")
                        continue
                    if urlsplit(page.url).hostname != UF:
                        raise SessionError("Login origin changed before credential entry.")
                    await_url = page.url
                    username_input.fill(username)
                    password_input.fill(password)
                    del username, password
                    # The actual UF submit button was verified by the auth probe.
                    if page.url != await_url:
                        raise SessionError("Login page changed before submission.")
                    page.locator('[name="_eventId_proceed"]').click()
                    submitted = True
                    status.show("Checking your password")
                    continue
                text, controls = describe_controls(page)
                if page.url != url:
                    # The page moved on while it was being read; never describe
                    # one page's content as another's.
                    continue
                on_page = time.monotonic() - url_since[1]
                view = transition_view(host, text, controls, on_page) or duo_view(text, controls)
                if view.kind == "waiting" and on_page < 8:
                    # Pages without controls normally pass by on their own; show
                    # their text only if one lingers.
                    view = LoginView("loading", "Waiting for Duo")
                if manual_prompt:
                    view = LoginView("manual", "Duo prompt", tuple(clean(text).splitlines()),
                                     tuple((str(n), n - 1, label(info)) for n, (_, info, _) in enumerate(controls, 1)), False)
                    manual_prompt = False
                if view.automatic:
                    status.show(view.title, status.hint(view))
                    last_view = view
                    key = poll_key()
                    if key == "q":
                        return 1
                    if key == "m" and view.kind in {"verified_push", "code_loading", "push"}:
                        manual_prompt = True
                        continue
                    chosen = next((index for action, index, _ in view.actions if action == key), None)
                else:
                    status.clear()
                    if view != last_view:
                        render_view(view)
                        last_view = view
                    key = input("Choose an option, press Enter to check again, or q to cancel: ").strip().lower()
                    if key == "q":
                        return 1
                    chosen = next((index for action, index, _ in view.actions if action == key), None)
                if chosen is not None:
                    try:
                        interact(page, url, controls, str(chosen + 1), args.show_password)
                    except Exception:
                        status.clear()
                        print("That option changed before it could be used; showing the current screen.")
            write_report(stage="login_timeout", session_saved=False)
            status.done("Sign-in timed out. Run again, or add --browser to sign in in a visible window.", ok=False)
            return 1
        finally:
            status.clear()
            context.close()
            browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--show-password", action="store_true")
    parser.add_argument("--browser", action="store_true", help="Use the visible-browser fallback for login")
    parser.add_argument("--fresh", action="store_true", help="Ignore saved state for this login")
    parser.add_argument("--check", action="store_true", help="Only test saved session using native HTTP; no browser or prompts")
    parser.add_argument("--channel", choices=["msedge", "chrome"], default="msedge")
    args = parser.parse_args()
    if CANVAS != "ufl.instructure.com":
        args.browser = True
    if args.fresh and args.check:
        parser.error("--fresh and --check cannot be combined")
    status = Status()
    try:
        saved = None if args.fresh else load_session()
        if saved:
            status.show("Checking your saved session")
            ok, outcome = verify_and_save(saved["browser_state"], saved.get("user_id"))
            if ok:
                status.done("Signed in with your saved session.")
                write_report(stage="session_reused", native_profile=outcome, session_saved=True)
                return 0
            if outcome["kind"] == "network_error":
                write_report(stage="network_error", native_profile=outcome, session_saved=True)
                status.done("Could not reach Canvas. Your session was kept; try again when you are online.", ok=False)
                return 1
            status.done("Your Canvas session has expired. Sign in to continue.", ok=False)
        if args.check:
            status.done("No usable saved session. Run without --check to sign in.", ok=False)
            return 1
        if not sys.stdin.isatty():
            parser.exit(2, "Run this command in your own interactive terminal.\n")
        return browser_login(args, saved, status)
    except SessionError as exc:
        status.clear()
        print(str(exc), file=sys.stderr)
        write_report(stage="session_error", session_saved=False)
        return 1
    except ImportError:
        status.clear()
        print("Run using .venv\\Scripts\\python.exe; Playwright is required for login.", file=sys.stderr)
        return 1
    except (KeyboardInterrupt, EOFError):
        status.clear()
        print("\nCancelled.")
        return 1
    except Exception as exc:
        status.clear()
        # Browser exceptions may contain credential-bearing URLs. Never print them.
        print("Sign-in stopped (" + type(exc).__name__ + "). Try --browser if this challenge needs a visible browser.", file=sys.stderr)
        write_report(stage="login_error", error_type=type(exc).__name__)
        return 1
    finally:
        status.close()

if __name__ == "__main__":
    raise SystemExit(main())
