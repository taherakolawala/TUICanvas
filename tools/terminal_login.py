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

from tools.auth_probe import START, UF, trusted_url
from tools.auth_ui import clean, duo_view, label, poll_key, render_view, LoginView
from tools.auth_session import (CANVAS, ORIGIN, PROFILE, NativeCanvas, SessionError,
                               load_session, profile_id, save_session)

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/auth-login-result.json"


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
        if not trusted_url(frame.url):
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
        print("The login screen changed; refreshing the options.")
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
        print("That control changed; refreshing.")
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


def browser_login(args, saved):
    from playwright.sync_api import sync_playwright

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel=args.channel, headless=not args.browser)
        context = browser.new_context(storage_state=saved["browser_state"] if saved else None)
        page = context.new_page()
        page.goto(START, wait_until="domcontentloaded", timeout=60000)
        deadline = time.monotonic() + 600
        submitted = False
        last_message = None
        last_view = None
        manual_prompt = False
        try:
            while time.monotonic() < deadline:
                page.wait_for_timeout(400)
                host = urlsplit(page.url).hostname
                if not trusted_url(page.url):
                    raise SessionError("Login reached an unexpected origin. Browser fallback may need review.")
                if host == CANVAS and page.locator("#global_nav_profile_link").count():
                    state = context.storage_state()
                    ok, outcome = verify_and_save(state)
                    if ok:
                        write_report(stage="authenticated", native_profile=outcome, session_saved=True, mode="browser" if args.browser else "terminal")
                        print("\nSigned in to Canvas. Your session is saved for next time.")
                        return 0
                    # Preserve the proven browser session so a failed native transport
                    # experiment does not force another username/password/Duo cycle.
                    save_session(state, None)
                    write_report(stage="browser_authenticated_native_pending", native_profile=outcome, session_saved=True)
                    print("Browser login succeeded and its protected session was saved.")
                    print("Native Canvas read did not succeed; report contains the status, not response data.")
                    return 2
                if args.browser:
                    if last_message != host:
                        print("Complete the normal login in the browser. Waiting at", clean(host))
                        last_message = host
                    continue
                username_input = page.locator('input[name="j_username"]')
                password_input = page.locator('input[name="j_password"]')
                if host == UF and username_input.count() and password_input.count() and username_input.is_visible():
                    if submitted:
                        print("UF still shows the login form. No automatic credential retry.")
                        if input("Enter r to retry, or q to stop: ").strip().lower() != "r":
                            return 1
                    print("\nSign in to UF")
                    username = input("GatorLink username: ").strip()
                    password = input("GatorLink password (visible): ") if args.show_password else getpass.getpass("GatorLink password: ")
                    if not username or not password:
                        print("Username/password required; nothing submitted.")
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
                    page.wait_for_timeout(1200)
                    continue
                text, controls = describe_controls(page)
                view = duo_view(text, controls)
                if manual_prompt:
                    view = LoginView("manual", "Duo prompt", tuple(clean(text).splitlines()),
                                     tuple((str(n), n - 1, label(info)) for n, (_, info, _) in enumerate(controls, 1)), False)
                    manual_prompt = False
                if view != last_view:
                    render_view(view)
                    last_view = view
                snapshot_url = page.url
                if view.automatic:
                    key = poll_key()
                    if key == "q":
                        return 1
                    if key == "m" and view.kind in {"verified_push", "code_loading"}:
                        manual_prompt = True
                        continue
                    chosen = next((index for action, index, _ in view.actions if action == key), None)
                    choice = str(chosen + 1) if chosen is not None else ""
                else:
                    key = input("Choose an option, Enter to check again, or q to cancel: ").strip().lower()
                    if key == "q":
                        return 1
                    chosen = next((index for action, index, _ in view.actions if action == key), None)
                    choice = str(chosen + 1) if chosen is not None else ""
                if choice:
                    try:
                        interact(page, snapshot_url, controls, choice, args.show_password)
                    except Exception:
                        print("The selected control changed or could not be used. Refreshing; no automatic retry.")
            write_report(stage="login_timeout", session_saved=False)
            print("Login timed out. Run again, or add --browser for the fallback.")
            return 1
        finally:
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
    if args.fresh and args.check:
        parser.error("--fresh and --check cannot be combined")
    try:
        saved = None if args.fresh else load_session()
        if saved:
            ok, outcome = verify_and_save(saved["browser_state"], saved.get("user_id"))
            if ok:
                print("Signed in to Canvas using your saved session.")
                write_report(stage="session_reused", native_profile=outcome, session_saved=True)
                return 0
            print("Saved session native check:", outcome["kind"], outcome["status"])
            if outcome["kind"] == "network_error":
                write_report(stage="network_error", native_profile=outcome, session_saved=True)
                print("Session retained. Retry when the connection is available.")
                return 1
        if args.check:
            print("No usable native session yet. Run without --check to sign in.")
            return 1
        if not sys.stdin.isatty():
            parser.exit(2, "Run this command in your own interactive terminal.\n")
        print("Connecting to UF...")
        return browser_login(args, saved)
    except SessionError as exc:
        print(str(exc), file=sys.stderr)
        write_report(stage="session_error", session_saved=False)
        return 1
    except ImportError:
        print("Run using .venv\\Scripts\\python.exe; Playwright is required for login.", file=sys.stderr)
        return 1
    except (KeyboardInterrupt, EOFError):
        print("\nCancelled.")
        return 1
    except Exception as exc:
        # Browser exceptions may contain credential-bearing URLs. Never print them.
        print("Login stopped (" + type(exc).__name__ + "). Try --browser if this challenge needs a visible browser.", file=sys.stderr)
        write_report(stage="login_error", error_type=type(exc).__name__)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
