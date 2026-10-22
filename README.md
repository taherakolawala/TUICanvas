# TUICanvas

A fast, keyboard-driven terminal interface for [Canvas LMS](https://www.instructure.com/canvas), built first for the University of Florida's Canvas (`ufl.instructure.com`).

The Canvas web page for one assignment makes 183 requests (1.4 MB) before the assignment shows up. TUICanvas skips the web app and makes only the reads each screen needs:

```
Canvas › GPU Computing                                          Updated 4:54 PM
Due                  Assignment                       Status             Points
Sun Oct 18 11:59 PM  Lab-2 Assignment                 Not submitted      10 pts
Mon Oct 5 11:59 PM   Lab-1 Assignment                 Graded 9/10        10 pts
```

## Status

Current progress, known issues and next steps: [docs/status.md](docs/status.md). How it works and which optimizations are in place: [docs/architecture.md](docs/architecture.md). Measured against the Canvas website: [benchmarks/baseline-2026-10-20.md](benchmarks/baseline-2026-10-20.md).

It covers sign in (GatorLink + Duo) → course list → assignment list → assignment details (deadline, submission status, description), course files, and announcements.

- Every screen shows a saved copy right away, with **Syncing…** at the top while it fetches fresh data. If the fetch fails, the screen says **Outdated** instead of pretending the old copy is current.
- **Files.** Each course has a Files tab that mirrors its Canvas folders, and each assignment lists its attachments and links. Files download into `C:\Users\<you>\TUICanvas\<course>\Class Files` or `Assignment Files\<assignment>` and open in their default app. A file used in both places is stored once.
- **Announcements.** A per-course tab lists all announcements newest-first, with author, posting date and Canvas read status. Open one to read its message and attachments; replies open in the browser. Opening a readable message marks it Read locally in the TUI, remembered across restarts. Canvas read status is unchanged. Files are fetched only when you choose them and go in `Announcement Files\<announcement-id>`.
- File submissions are available from an assignment that accepts uploads. Text entry, modules, pages and discussions open in the browser for now. Quizzes and assessments are out of scope (see `docs/build-plan.md`).
- To use a separate Canvas site such as Canvas Lite, set `TUICANVAS_ORIGIN` to its HTTPS origin before launching. Its sign-in opens a visible browser and saves a separate, protected session.

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
tuicanvas --bench [N]     Time the workflow N times -> benchmarks/private/tui-bench.json
```

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k`, PgUp/PgDn, `g` `G` | Move or scroll |
| `Enter` / `→` | Open (a file opens in its default app, downloading it first if needed) |
| `E` | From the course list, edit the home screen. `Enter` hides or restores a selected course; `Esc` finishes editing. Hidden courses remain visible while editing. |
| `s` | On an assignment, start a file submission in your Windows user folder. Type a name or paste a full path; matching local files and folders appear as you type. Use `↑`/`↓` and `Tab` to complete, then `Enter` to add each file. `Ctrl+U` clears the path. After adding files, press `Enter` at the starting folder to review, `y` to submit, or `Esc` to cancel. Files are checked against Canvas's allowed extensions and upload size rules. |
| `h` | On an assignment, open past submissions. Use `←`/`→` to switch attempts and `↑`/`↓` to scroll the selected attempt. `r` refreshes; `Esc` returns to the assignment. |
| `Esc` / `←` / Backspace | Back (or clear the filter or selection) |
| `Tab` / `Shift+Tab` | In a course: cycle Assignments, Files and Announcements forward/backward. In an assignment or announcement: select the next/previous file or link (`1`–`9` jump to one) |
| `d` / `D` | Download the selected file (or folder) / everything here |
| `e` | Show the file or its folder in File Explorer (on the course list: your TUICanvas folder) |
| `/` | Filter the current list (in Files: search the whole course) |
| `r` | Refresh now |
| `o` | Open the current item, or the selected link, in your browser |
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
- Downloaded files live in `%USERPROFILE%\TUICanvas` (set `TUICANVAS_LIBRARY` to move it). Canvas cookies are sent only to Canvas, never to its file storage hosts.
