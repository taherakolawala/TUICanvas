# Authentication experiment

User decision: terminal-first username/password and Duo. Browser login is an accepted fallback. Later launches should reuse valid protected session state without prompting where UF/Canvas policy permits.

Verified without credentials: a standard HTTPS client reaches UF's Shibboleth session-storage initialization page and then the normal `j_username` / `j_password` form. The native probe correctly declares no browser localStorage support. No MFA or access-control checks are skipped.

The Duo stage cannot be inspected meaningfully without a user login. UF's public documentation describes Duo Universal Prompt and Verified Push. We cannot simply use Duo's integration API: those APIs require integration credentials owned by the institution/application, which this client does not have.

## Current probe

`tools/auth_probe.py` is temporary development tooling, not the final Rust app. It uses Python's standard library. Default mode reaches the public credential form without submitting a username/password:

```powershell
python tools/auth_probe.py
```

Run the next experiment yourself in an interactive terminal:

```powershell
python tools/auth_probe.py --login
```

The username/password are entered locally; password input is hidden. The probe submits once to the exact UF HTTPS origin, then stops to identify the next stage. This may initiate the usual Duo flow, but the probe does not yet complete Duo or persist the session. It does not retry rejected credentials.

To see password characters while typing, use `python tools/auth_probe.py --login --show-password`. This explicit option displays the password in your terminal; it does not add the password to the diagnostic report. Hidden input remains the default.

The output `benchmarks/private/auth-stage.json` contains only the stage, host, and form field names/types. No field values, cookies, URL parameters, raw HTML, or passwords are saved. Keep credentials out of chat. The probe refuses non-interactive credential input, unknown redirect origins, and preserved-body redirects that could forward a credential POST.

Next: use the stage report to determine whether terminal Duo interaction can be implemented or whether the accepted browser fallback is needed. Implement persistent OS-protected session reuse and native Canvas reads after the login path is established. Do not call this a complete login system yet.

## Probe v2 correction

The first credentialed run returned to the username/password form. Inspection identified a concrete client bug: UF's `_eventId_proceed` control is a `<button type="submit">`, but the first parser recorded only `<input>` elements. The login POST therefore omitted the clicked submit action. This is not evidence of incorrect credentials or unsupported terminal authentication.

The regression test reproduced the missing POST field. Version 2 records button controls, includes only the selected login submitter, and refuses to send credentials if that action is absent or disabled. Reports include a version and timestamp so the next credentialed result can be distinguished from the old report. Live public-form verification is performed without transmitting credentials; a user-run retry is necessary to inspect the post-login Duo stage.

## References

- UF authentication changes: https://it.ufl.edu/get-connected/multi-factor-authentication/upcoming-changes/
- UF remembered-device behavior: https://it.ufl.edu/get-connected/multi-factor-authentication/remember-me-feature/
- Duo integration requirements: https://duo.com/docs/duoweb

## Terminal browser-assisted login experiment

The credentialed probe v2 reached `duo_challenge` on Duo's origin with no HTML forms. The user then completed the terminal helper's Verified Push and remembered-device choice. Canvas login succeeded, and a native profile read returned HTTP 200. A subsequent saved-session check also succeeded without launching a browser. Session lifetime beyond this immediate reuse remains unmeasured.

The helper renders the normal login using an isolated headless Edge engine. Known Duo stages have a concise terminal presentation: loading updates automatically, Verified Push shows the code and instructions, remembered-device choice uses `y`/`n`, and success continues automatically. While waiting for Verified Push, `o` opens other methods, `m` exposes the full prompt controls, and `q` cancels. Unknown challenges preserve their instructions and numbered controls. Prompt text and verification codes are not written to diagnostic reports. Use the visible-browser fallback if a prompt requires browser capabilities.

```powershell
.\.venv\Scripts\python.exe -m tools.terminal_login --show-password
```

Hidden password input is the default if `--show-password` is omitted. Playwright 1.64.0 is installed in the project's isolated environment; the helper uses the existing Edge installation. `--channel chrome` selects the installed Chrome instead. `--browser` uses the accepted visible-browser login fallback.

After Canvas login, the helper tests a native HTTPS profile read. Successful authentication state is encrypted with Windows DPAPI for the current user and stored at `%LOCALAPPDATA%\TUICanvas\ufl.session.dpapi`. If browser login succeeds but native reads do not, the protected browser state is still retained and the diagnostic report records the distinction. No assertion of native authentication is made from the page URL alone.

Check session reuse without launching a browser:

```powershell
.\.venv\Scripts\python.exe -m tools.terminal_login --check
```

The helper uses stored sessions first on normal runs as well. `--fresh` ignores saved state for a new login. Reuse still depends on UF/Canvas expiry and MFA policy; exported cookie/localStorage state does not promise preservation of every browser remembered-device signal.

The new diagnostic report is `benchmarks/private/auth-login-result.json`; it contains stage/status metadata only. The old probe remains useful for isolated form inspection. The Rust reader reads this same protected session and runs the helper whenever sign-in is needed (see [status.md](status.md)).

Automated checks cover cookie scope, expiration/path matching, rotated/deleted cookie handling, native origin restrictions, CSRF scope, terminal-control filtering, response redaction, Windows encryption round trips, and replay of the observed Duo screen sequence. The streamlined presentation has been tested against synthetic versions of the observed screens; a new cold login has not been triggered solely to retest its appearance. The original helper's terminal Duo completion and native reads were live-verified by the user's run, followed by our successful native saved-session check.
