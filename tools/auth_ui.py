"""Small presentation/state layer for the authentication experiment."""

from dataclasses import dataclass
import os
import re
import sys


def clean(text):
    text = re.sub(r"\x1b\][^\x07]*(?:\x07|\x1b\\)", "", str(text))
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    return "".join(c for c in text if c in "\n\t" or (ord(c) >= 32 and not 127 <= ord(c) <= 159))


def label(info):
    return " ".join(clean(info["label"]).split())


@dataclass(frozen=True)
class LoginView:
    kind: str
    title: str
    lines: tuple = ()
    # (single key, original control index, visible label)
    actions: tuple = ()
    automatic: bool = True


def duo_view(text, controls):
    lines = [" ".join(line.split()) for line in clean(text).splitlines() if line.strip()]
    labels = {label(info).lower(): index for index, (_, info, _) in enumerate(controls)}
    lower = "\n".join(lines).lower()
    other = labels.get("other options")
    other_action = (("o", other, "Other methods"),) if other is not None else ()

    # Never auto-accept remembered-device or other consent choices.
    yes = labels.get("yes, this is my device")
    no = labels.get("no, other people use this device")
    if "is this your device?" in lower and yes is not None and no is not None:
        return LoginView("remember", "Remember this device?",
                         ("Choose Yes only if you are the only person who uses this device.",),
                         (("y", yes, "Yes, remember this device"), ("n", no, "No, this device is shared")))
    if "success!" in lower and "logging you in" in lower:
        return LoginView("connecting", "Connecting to Canvas...")
    if "enter code in duo mobile" in lower:
        codes = [line for line in lines if re.fullmatch(r"\d{3}", line)]
        if len(set(codes)) == 1:
            return LoginView("verified_push", "Verify with Duo",
                             (f"Enter {codes[0]} in the Duo Mobile app on your phone.",
                              "Waiting for approval. This updates automatically."), other_action)
        # A heading can arrive before its code; keep polling instead of asking
        # the user to refresh. Unexpected layouts still have a manual fallback.
        if not codes:
            return LoginView("code_loading", "Waiting for your Duo verification code...", actions=other_action)
    # Ignore screen-reader announcements, branding, and help/navigation labels.
    body = []
    for line in lines:
        value = line.lower()
        if (value.startswith("now on page called,") or value in labels
                or value in {"secured by duo", "need help?", "need help?secured by duo"}):
            continue
        if line not in body:
            body.append(line)
    usable = [(index, label(info)) for index, (_, info, _) in enumerate(controls)
              if label(info).lower() not in {"need help?", "help"}]
    if not usable and (not body or all(line.lower() in {"loading", "loading...", "loading…"} for line in body)):
        return LoginView("loading", "Waiting for Duo...")
    if not usable:
        return LoginView("waiting", "Waiting for sign-in...", tuple(body))
    # Unknown prompts remain interactive and preserve their instructions. We
    # must not invent a meaning for a new MFA challenge or silently dismiss it.
    actions = tuple((str(n), index, name) for n, (index, name) in enumerate(usable, 1))
    return LoginView("other", "Duo needs your input", tuple(body), actions, automatic=False)


def render_view(view):
    print("\n" + view.title)
    for line in view.lines:
        print("  " + line)
    if view.actions:
        print("  " + "   ".join(f"[{key}] {name}" for key, _, name in view.actions))
    if view.automatic:
        print("  [q] Cancel" + ("   [m] Show all prompt controls" if view.kind in {"verified_push", "code_loading"} else ""))


def poll_key():
    """Read optional input without blocking browser progress or phone approval."""
    if os.name == "nt":
        import msvcrt
        while msvcrt.kbhit():
            char = msvcrt.getwch()
            if char in {"\x00", "\xe0"}:
                if msvcrt.kbhit():
                    msvcrt.getwch()
                continue
            if char == "\x03":
                raise KeyboardInterrupt()
            if char not in {"\r", "\n"}:
                return char.lower()
        return None
    import select
    if select.select([sys.stdin], [], [], 0)[0]:
        value = sys.stdin.readline()
        if value == "":
            raise EOFError()
        return value.strip().lower() or None
    return None
