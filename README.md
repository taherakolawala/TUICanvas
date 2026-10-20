# TUICanvas

A fast, keyboard-driven terminal reader for [Canvas LMS](https://www.instructure.com/canvas), built first for the University of Florida's Canvas (`ufl.instructure.com`).

The Canvas web page for one assignment makes 183 requests (1.4 MB) before the assignment shows up. TUICanvas skips the web app and makes only the reads each screen needs:

```
Canvas › GPU Computing                                          Updated 4:54 PM
Due                  Assignment                       Status             Points
Sun Oct 18 11:59 PM  Lab-2 Assignment                 Not submitted      10 pts
Mon Oct 5 11:59 PM   Lab-1 Assignment                 Graded 9/10        10 pts
```

## Status

Current progress, known issues and next steps: [docs/status.md](docs/status.md). How it works and which optimizations are in place: [docs/architecture.md](docs/architecture.md).

Version 1 is in progress. It covers sign in (GatorLink + Duo) → course list → assignment list → assignment details (deadline, submission status, description).

- Every screen shows a saved copy right away, with **Syncing…** at the top while it fetches fresh data. If the fetch fails, the screen says **Outdated** instead of pretending the old copy is current.
- Keys: refresh, back, filter, and open-in-browser.
- Files, modules, discussions, the inbox and submitting work are out of scope for v1 (see `docs/build-plan.md`).

## How sign-in works

Personal access tokens aren't used. A small helper (`tools/terminal_login.py`) runs the normal UF login in a hidden Microsoft Edge window. You type your GatorLink username and password and approve Duo, all inside the terminal.

The resulting session is encrypted with Windows DPAPI for your Windows user and stored at `%LOCALAPPDATA%\TUICanvas\ufl.session.dpapi`. Later runs reuse it without a browser until Canvas or UF expire it.

Passwords are never saved or logged.

## Setup (Windows)

Requirements: Windows 10/11, Microsoft Edge, Python 3.12+, and Rust. The GNU toolchain is fine; Visual Studio isn't needed.

```powershell
# Rust (once)
Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $env:TEMP\rustup-init.exe
& $env:TEMP\rustup-init.exe -y --default-host x86_64-pc-windows-gnu --profile minimal

# Sign-in helper (once)
py -m venv .venv
.venv\Scripts\python.exe -m pip install -r tools\requirements-auth.txt

# Build and run
cargo build --release
target\release\tuicanvas.exe
```

## Usage

```
tuicanvas                 Open the reader (signs in first if needed)
tuicanvas --login         Sign in again
tuicanvas --logout        Delete the saved session and this account's saved copies
tuicanvas --diag          Log request timings (no content) to the data folder
tuicanvas --smoke         Read-only live check -> benchmarks/private/smoke.json
```

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k`, PgUp/PgDn, `g` `G` | Move or scroll |
| `Enter` / `→` | Open |
| `Esc` / `←` / Backspace | Back (or clear the filter) |
| `/` | Filter the current list |
| `r` | Refresh now |
| `o` | Open the current item in your browser |
| `L` | Sign in again |
| `q` | Quit |

## Layout

| Path | Contents |
| --- | --- |
| `src/` | Rust app: Ratatui UI, Canvas client, session and snapshot store |
| `tools/` | Python sign-in helper, login probe, HAR analyzer (with tests) |
| `docs/` | Build plan, discovery notes, authentication notes, request policy |
| `benchmarks/` | Baseline capture method and findings (raw captures stay in the ignored `private/` folder) |
| `scripts/` | Development check and watcher scripts |

## Privacy

- Raw HAR captures, session files and private reports are ignored by git.
- Diagnostics record only endpoint templates, statuses, sizes and timings. They never record URLs with IDs, cookies or response bodies.
- Saved copies of course data live under `%LOCALAPPDATA%\TUICanvas\cache`, separated by Canvas origin and account.
