"""Windows-protected authentication state and scoped native Canvas reads."""

import ctypes
from ctypes import wintypes
import http.cookiejar
from http.cookies import SimpleCookie
import json
import os
from pathlib import Path
import time
import urllib.error
import urllib.parse
import urllib.request

CANVAS = "ufl.instructure.com"
ORIGIN = "https://" + CANVAS
PROFILE = "/api/v1/users/self/profile"


class SessionError(Exception):
    """Safe error messages only: never include URLs, cookies, or response bodies."""


class Blob(ctypes.Structure):
    _fields_ = [("length", wintypes.DWORD), ("data", ctypes.POINTER(ctypes.c_ubyte))]


def protect(data, decrypt=False):
    if os.name != "nt":
        raise SessionError("Session protection currently supports Windows only.")
    crypt = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.LocalFree.restype = ctypes.c_void_p
    buf = ctypes.create_string_buffer(data)
    source = Blob(len(data), ctypes.cast(buf, ctypes.POINTER(ctypes.c_ubyte)))
    target = Blob()
    function = crypt.CryptUnprotectData if decrypt else crypt.CryptProtectData
    # The second parameter is a description on encrypt and an optional output on decrypt.
    function.argtypes = [ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.POINTER(Blob),
                         ctypes.c_void_p, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(Blob)]
    function.restype = wintypes.BOOL
    if not function(ctypes.byref(source), None, None, None, None, 1, ctypes.byref(target)):
        raise SessionError("Windows could not unlock this session." if decrypt else "Windows could not protect this session.")
    try:
        return ctypes.string_at(target.data, target.length)
    finally:
        kernel.LocalFree(ctypes.cast(target.data, ctypes.c_void_p))


def session_path():
    base = os.environ.get("LOCALAPPDATA")
    if not base:
        raise SessionError("LOCALAPPDATA is unavailable.")
    return Path(base) / "TUICanvas" / "ufl.session.dpapi"


def save_session(browser_state, user_id, path=None):
    path = Path(path) if path is not None else session_path()
    # Only the encrypted payload ever reaches disk.
    payload = json.dumps({"version": 1, "origin": ORIGIN, "user_id": user_id,
                          "saved_at": time.time(), "browser_state": browser_state}).encode()
    protected = protect(payload)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_bytes(protected)
    os.replace(temporary, path)


def load_session(path=None):
    path = Path(path) if path is not None else session_path()
    if not path.exists():
        return None
    try:
        payload = json.loads(protect(path.read_bytes(), decrypt=True))
        if (payload.get("version") != 1 or payload.get("origin") != ORIGIN
                or not isinstance(payload.get("browser_state", {}).get("cookies"), list)):
            raise ValueError()
        return payload
    except (ValueError, OSError, TypeError, AttributeError) as exc:
        raise SessionError("Stored session is invalid; use --fresh to sign in again.") from None


def canvas_jar(browser_state):
    jar = http.cookiejar.CookieJar()
    for item in browser_state.get("cookies", []):
        domain = item.get("domain", "")
        # Include only the Canvas host or a genuine parent-domain cookie.
        host_only = not domain.startswith(".")
        match = domain in {CANVAS, "." + CANVAS, ".instructure.com"}
        if not match or not domain:
            continue
        expires = item.get("expires", -1)
        expires = int(expires) if isinstance(expires, (int, float)) and expires > 0 else None
        jar.set_cookie(http.cookiejar.Cookie(
            version=0, name=item["name"], value=item["value"], port=None, port_specified=False,
            domain=domain, domain_specified=not host_only, domain_initial_dot=domain.startswith("."),
            path=item.get("path", "/"), path_specified=True, secure=item.get("secure", True),
            expires=expires, discard=expires is None, comment=None, comment_url=None,
            rest={"HttpOnly": item.get("httpOnly", False)}, rfc2109=False))
    return jar


class NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class NativeCanvas:
    def __init__(self, browser_state):
        self.state = browser_state
        self.jar = canvas_jar(browser_state)
        self.opener = urllib.request.build_opener(NoRedirects(), urllib.request.HTTPCookieProcessor(self.jar))

    def get(self, path):
        parsed = urllib.parse.urlsplit(path)
        if parsed.scheme or parsed.netloc or not parsed.path.startswith("/api/v1/") or "\\" in path:
            raise SessionError("Native reads must target the configured Canvas API.")
        request = urllib.request.Request(ORIGIN + path, headers={"Accept": "application/json", "X-Requested-With": "XMLHttpRequest"})
        # Match the browser's Canvas CSRF header using only cookies eligible for
        # this exact HTTPS request; identity-provider cookies never participate.
        self.jar.add_cookie_header(request)
        cookies = SimpleCookie()
        cookies.load(request.get_header("Cookie", ""))
        if "_csrf_token" in cookies:
            token = urllib.parse.unquote(cookies["_csrf_token"].value)
            if "\r" not in token and "\n" not in token:
                request.add_header("X-CSRF-Token", token)
        try:
            with self.opener.open(request, timeout=20) as response:
                if "json" not in response.headers.get("Content-Type", "").lower():
                    return {"kind": "non_json", "status": response.status}
                raw = response.read(8_000_001)
                if len(raw) > 8_000_000:
                    return {"kind": "oversize", "status": response.status}
                # Some Canvas JSON responses include an anti-hijacking prefix.
                if raw.startswith(b"while(1);"):
                    raw = raw[len(b"while(1);"):]
                return {"kind": "json", "status": response.status, "data": json.loads(raw)}
        except urllib.error.HTTPError as exc:
            status = exc.code
            exc.close()
            return {"kind": "unauthenticated" if status in (301, 302, 303, 307, 308, 401) else "http_error", "status": status}
        except (urllib.error.URLError, TimeoutError):
            return {"kind": "network_error", "status": None}
        except (ValueError, UnicodeError):
            return {"kind": "invalid_json", "status": None}

    def updated_state(self):
        # Preserve non-Canvas login state, and incorporate rotated Canvas cookies.
        result = dict(self.state)
        original_canvas_keys = {(c.name, c.domain, c.path) for c in canvas_jar(self.state)}
        original = {(c["name"], c["domain"], c.get("path", "/")): c
                    for c in self.state.get("cookies", [])}
        existing = {key: value for key, value in original.items() if key not in original_canvas_keys}
        for cookie in self.jar:
            key = (cookie.name, cookie.domain, cookie.path)
            if cookie.is_expired():
                existing.pop(key, None)
                continue
            previous = original.get(key, {})
            existing[key] = {"name": cookie.name, "value": cookie.value, "domain": cookie.domain,
                             "path": cookie.path, "expires": cookie.expires if cookie.expires is not None else -1,
                             "httpOnly": previous.get("httpOnly", cookie.has_nonstandard_attr("HttpOnly")),
                             "secure": cookie.secure, "sameSite": previous.get("sameSite", "Lax")}
        result["cookies"] = list(existing.values())
        return result


def profile_id(result):
    if result.get("kind") == "json" and isinstance(result.get("data"), dict):
        value = result["data"].get("id")
        if isinstance(value, int) and not isinstance(value, bool) and value > 0:
            return value
    return None
