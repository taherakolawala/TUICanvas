"""Temporary UF terminal-auth experiment; never saves passwords or session cookies.

Default: reach the public username/password form without credentials.
--login: ask locally for credentials, submit once, and report the next stage.
This is a diagnostic probe, not yet the application's complete Duo login.
"""

import argparse
import getpass
import http.cookiejar
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from html.parser import HTMLParser
from pathlib import Path


START = "https://ufl.instructure.com/login/saml/355"
UF = "login.ufl.edu"
CANVAS = "ufl.instructure.com"
REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/auth-stage.json"


class UnsupportedFlow(Exception):
    pass


def trusted_url(url):
    parsed = urllib.parse.urlsplit(url)
    host = parsed.hostname or ""
    try:
        port = parsed.port
    except ValueError:
        return False
    return (parsed.scheme == "https" and port in (None, 443)
            and not parsed.username and not parsed.password
            and (host in {UF, CANVAS} or host.endswith(".duosecurity.com")))


class ScopedRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not trusted_url(newurl):
            raise UnsupportedFlow("Redirect outside the known UF/Canvas/Duo login flow.")
        # Do not forward a credential-bearing POST body to another origin.
        if req.data and code in (307, 308):
            raise UnsupportedFlow("A preserved-body redirect requires review.")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


@dataclass
class Form:
    action: str
    method: str
    inputs: list = field(default_factory=list)

    def fields(self):
        result = {}
        for item in self.inputs:
            name = item.get("name")
            kind = item.get("type", "text").lower()
            if not name or "disabled" in item or kind in {"file", "reset", "button", "submit", "image"}:
                continue
            if kind in {"checkbox", "radio"} and "checked" not in item:
                continue
            result[name] = item.get("value", "")
        return result


class Page(HTMLParser):
    def __init__(self, url, html):
        super().__init__()
        self.url = url
        self.forms = []
        self.current = None
        self.iframe_hosts = set()
        self.feed(html)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "form":
            self.current = Form(urllib.parse.urljoin(self.url, attrs.get("action", "")),
                                attrs.get("method", "get").lower())
            self.forms.append(self.current)
        elif tag in {"input", "button"} and self.current is not None:
            # HTML buttons default to submit; only the clicked submitter is sent.
            if tag == "button":
                attrs.setdefault("type", "submit")
            self.current.inputs.append(attrs)
        elif tag == "iframe":
            host = urllib.parse.urlsplit(urllib.parse.urljoin(self.url, attrs.get("src", ""))).hostname
            if host and (host in {UF, CANVAS} or host.endswith(".duosecurity.com")):
                self.iframe_hosts.add(host)

    def handle_endtag(self, tag):
        if tag == "form":
            self.current = None

    def password_form(self):
        return next((f for f in self.forms if {"j_username", "j_password"} <= f.fields().keys()), None)

    def report(self):
        host = urllib.parse.urlsplit(self.url).hostname
        if self.password_form():
            stage = "username_password_form"
        elif (host or "").endswith(".duosecurity.com") or any(h.endswith(".duosecurity.com") for h in self.iframe_hosts):
            stage = "duo_challenge"
        elif any("SAMLResponse" in f.fields() for f in self.forms):
            stage = "saml_return_form"
        elif host == CANVAS:
            stage = "canvas_return_unverified"
        else:
            stage = "intermediate_login_step"
        return {"stage": stage, "host": host, "iframe_hosts": sorted(self.iframe_hosts),
                "forms": [{"method": f.method,
                           "action_host": urllib.parse.urlsplit(f.action).hostname,
                           "fields": [{"name": i["name"], "type": i.get("type", "text")}
                                      for i in f.inputs if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]{0,80}", i.get("name", ""))]}
                          for f in self.forms]}


class Probe:
    def __init__(self):
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(ScopedRedirects(), urllib.request.HTTPCookieProcessor(self.cookies))

    def fetch(self, url, fields=None):
        if not trusted_url(url):
            raise UnsupportedFlow("Unrecognized login destination.")
        data = urllib.parse.urlencode(fields).encode() if fields is not None else None
        request = urllib.request.Request(url, data=data)
        with self.opener.open(request, timeout=30) as response:
            if not trusted_url(response.url):
                raise UnsupportedFlow("Unrecognized response destination.")
            html = response.read(4_000_001)
            if len(html) > 4_000_000:
                raise UnsupportedFlow("Login response exceeded the probe limit.")
            return Page(response.url, html.decode("utf-8", errors="replace"))

    def prepare(self):
        page = self.fetch(START)
        # Shibboleth first checks browser localStorage. This native client has none.
        for _ in range(3):
            if page.password_form():
                return page
            storage = next((f for f in page.forms if "shib_idp_ls_supported" in f.fields()), None)
            if not storage or storage.method != "post" or urllib.parse.urlsplit(storage.action).hostname != UF:
                return page
            values = storage.fields()
            values["shib_idp_ls_supported"] = "false"
            page = self.fetch(storage.action, values)
        return page

    def login(self, page, username, password):
        form = page.password_form()
        if (not form or form.method != "post" or not trusted_url(form.action)
                or urllib.parse.urlsplit(page.url).hostname != UF
                or urllib.parse.urlsplit(form.action).hostname != UF):
            raise UnsupportedFlow("Expected UF username/password form was not found.")
        fields = form.fields()
        fields.update(j_username=username, j_password=password)
        submit = next((i for i in form.inputs if i.get("name") == "_eventId_proceed"
                       and i.get("type", "").lower() == "submit" and "disabled" not in i), None)
        if submit is None:
            fields.clear()
            raise UnsupportedFlow("UF login submit action was not found; credentials were not sent.")
        fields["_eventId_proceed"] = submit.get("value", "")
        try:
            return self.fetch(form.action, fields)
        finally:
            fields.clear()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--login", action="store_true", help="Submit credentials entered locally once; inspect the next step")
    parser.add_argument("--show-password", action="store_true", help="Show password characters in the terminal while entering them (requires --login)")
    args = parser.parse_args()
    if args.show_password and not args.login:
        parser.error("--show-password requires --login")
    if args.login and not sys.stdin.isatty():
        parser.exit(2, "Run --login in your own interactive terminal. Credentials must not be piped or passed as arguments.\n")
    try:
        probe = Probe()
        page = probe.prepare()
        if args.login:
            if not page.password_form():
                raise UnsupportedFlow("UF did not present the expected credential form.")
            print("UF terminal-auth experiment: sends credentials only to https://login.ufl.edu.")
            print("This probe inspects the next Duo/SSO stage; complete login and session persistence are not implemented yet.")
            username = input("GatorLink username: ").strip()
            password = (input("GatorLink password (visible): ") if args.show_password
                        else getpass.getpass("GatorLink password: "))
            if not username or not password:
                parser.exit(2, "Username and password are required. Nothing was submitted.\n")
            page = probe.login(page, username, password)
            del password, username
        report = page.report()
        report["probe_version"] = 2
        report["observed_at"] = datetime.now(timezone.utc).isoformat()
        report["credentials_submitted"] = args.login
        if args.login and report["stage"] == "username_password_form":
            report["result"] = "Login returned to the credential form; no automatic retry was attempted."
        REPORT.parent.mkdir(parents=True, exist_ok=True)
        REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print("Reached:", report["stage"])
        print("Saved field names/types only (no values, cookies, response body, or URL parameters):", REPORT)
        print("The in-memory session ends with this probe. This is not a persisted application login.")
    except (urllib.error.URLError, UnsupportedFlow, TimeoutError) as exc:
        # Exception text can contain sensitive redirect URLs; expose only a safe category.
        print("Probe stopped:", type(exc).__name__, "(no credentials or raw responses logged).", file=sys.stderr)
        return 1
    except (KeyboardInterrupt, EOFError):
        print("\nCancelled.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
