"""Small presentation/state layer for the authentication experiment."""

from dataclasses import dataclass
import os
import re
import shutil
import sys
import threading


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
    other_action = (("o", other, "other options"),) if other is not None else ()

    # Never auto-accept remembered-device or other consent choices.
    yes = labels.get("yes, this is my device")
    no = labels.get("no, other people use this device")
    if "is this your device?" in lower and yes is not None and no is not None:
        return LoginView("remember", "Remember this device? Say yes only if nobody else uses it", (),
                         (("y", yes, "yes"), ("n", no, "no")))
    if "success!" in lower and "logging you in" in lower:
        return LoginView("connecting", "Finishing sign-in")
    if "enter code in duo mobile" in lower:
        codes = [line for line in lines if re.fullmatch(r"\d{3}", line)]
        if len(set(codes)) == 1:
            return LoginView("verified_push", f"Enter {codes[0]} in Duo Mobile to approve", (), other_action)
        # A heading can arrive before its code; keep polling instead of asking
        # the user to refresh. Unexpected layouts still have a manual fallback.
        if not codes:
            return LoginView("code_loading", "Waiting for your Duo code", actions=other_action)
    if "check for a duo push" in lower or "approving the notification" in lower:
        device = re.search(r'sent to "([^"]{1,40})"', "\n".join(lines), re.IGNORECASE)
        where = f" ({device.group(1)})" if device else ""
        return LoginView("push", f"Approve the Duo Push on your phone{where}", (), other_action)
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
        return LoginView("loading", "Waiting for Duo")
    if not usable:
        return LoginView("waiting", "Waiting for sign-in", tuple(body))
    # Unknown prompts remain interactive and preserve their instructions. We
    # must not invent a meaning for a new MFA challenge or silently dismiss it.
    actions = tuple((str(n), index, name) for n, (index, name) in enumerate(usable, 1))
    return LoginView("other", "Sign-in needs your input", tuple(body), actions, automatic=False)


UF_HOST = "login.ufl.edu"


def transition_view(host, text, controls, seconds_on_page):
    """Short status for UF pages that normally pass by on their own.

    Returns None when the page should be shown in full: it has real controls,
    or it has stayed long enough that its text may explain a problem.
    """
    if host != UF_HOST:
        return None
    lower = clean(text).lower()
    if "saving session information" in lower or "saving login session" in lower:
        # UF's own page says to get help after a minute; show it in full then.
        return LoginView("uf_saving", "Finishing sign-in") if seconds_on_page < 60 else None
    usable = [c for c in controls if label(c[1]).lower() not in {"need help?", "help"}]
    if not usable and seconds_on_page < 6:
        return LoginView("uf_waiting", "Finishing sign-in")
    return None


def render_view(view):
    """A prompt that needs a typed choice: its instructions and numbered options."""
    print("\n" + view.title)
    for line in view.lines:
        print("  " + line)
    if view.actions:
        print("  " + "   ".join(f"[{key}] {name}" for key, _, name in view.actions))


class Status:
    """One status line with a spinner, updated in place.

    When output is not a terminal, each new status is printed once instead.
    """

    def __init__(self, stream=None):
        self.stream = stream if stream is not None else sys.stdout
        try:
            self.tty = self.stream.isatty()
        except (AttributeError, ValueError):
            self.tty = False
        encoding = (getattr(self.stream, "encoding", None) or "").lower().replace("-", "")
        fancy = encoding.startswith("utf")
        self.frames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏" if fancy else "|/-\\"
        self.ok_mark, self.info_mark = ("✓", "•") if fancy else ("OK", "-")
        self.sep = " · " if fancy else " - "
        self.ellipsis = "…" if fancy else "..."
        self._text = None
        self._hint = ""
        self._width = 0
        self._frame = 0
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = None

    def hint(self, view):
        parts = [f"{key} {name}" for key, _, name in view.actions] + ["q cancel"]
        return self.sep.join(parts)

    def show(self, text, hint=""):
        with self._lock:
            if (text, hint) == (self._text, self._hint):
                return
            self._text, self._hint = text, hint
            if not self.tty:
                self.stream.write(f"{text}{self.ellipsis}" + (f"  ({hint})" if hint else "") + "\n")
                self.stream.flush()
                return
            self._draw()
        if self._thread is None:
            self._thread = threading.Thread(target=self._spin, daemon=True)
            self._thread.start()

    def _draw(self):
        columns = shutil.get_terminal_size((80, 24)).columns - 1
        line = f"{self.frames[self._frame % len(self.frames)]} {self._text}{self.ellipsis}"
        if self._hint:
            line += f"   ({self._hint})"
        line = line[:columns]
        self.stream.write("\r" + line + " " * max(0, self._width - len(line)))
        self.stream.flush()
        self._width = len(line)

    def _spin(self):
        while not self._stop.wait(0.1):
            with self._lock:
                if self._text is not None and self.tty:
                    self._frame += 1
                    self._draw()

    def clear(self):
        """Remove the status line before printing anything else or prompting."""
        with self._lock:
            if self.tty and self._width:
                self.stream.write("\r" + " " * self._width + "\r")
                self.stream.flush()
            self._width = 0
            self._text = None
            self._hint = ""

    def done(self, text, ok=True):
        self.clear()
        with self._lock:
            self.stream.write(f"{self.ok_mark if ok else self.info_mark} {text}\n")
            self.stream.flush()

    def close(self):
        self._stop.set()
        self.clear()


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
