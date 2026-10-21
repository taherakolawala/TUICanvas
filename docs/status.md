# Project status

Last updated: 2026-10-21. Update this file whenever a milestone moves.

## Where things stand

How the pieces fit together is described in [architecture.md](architecture.md). Milestones 1–3 of [the build plan](build-plan.md) are done, and so are Milestones 7 and 8 of version 2 (the file library, Class Files and assignment attachments). Milestone 4 is partly done through the saved copies that came with Milestone 2; Milestones 5 and 6 have not started.

| Milestone | State | What exists |
| --- | --- | --- |
| 1. Sign-in and request experiment | **Done** | Terminal sign-in (GatorLink + Duo); live-verified native REST and GraphQL reads; failure classification; [request policy](request-policy.md) |
| 2. Reader vertical slice | **Done** | Courses → assignments → assignment detail, with saved copy + Syncing, refresh, back, filter, open in browser |
| 3. Rendering and network baseline | **Done** | Network, parse and draw timed separately (`--diag`, `--bench`); automated website baseline; the reader shows fresh data 6–10× sooner than the website ([baseline](../benchmarks/baseline-2026-10-20.md)) |
| 4. Selective resource reuse | Partly | Saved copies for courses, assignment lists and details only (always re-read on entry) |
| 5. Prediction experiment | Not started | |
| 6. Hardening and delivery | Not started | |
| 7. Local file library | **Done** | `%USERPROFILE%\TUICanvas\<course>\Class Files` and `Assignment Files`; one stored copy per file shown in several places through hard links; change detection; safe names; 30-day clean-up of automatic images; downloads restricted to Canvas and its storage hosts. Verified live: download, install, hard link, internet mark. |
| 8. Class Files and assignment attachments | **Done** | Files tab per course (folders, search across the course, download one, a folder or all, show in Explorer); files & links list on each assignment, numbered like the text; images downloaded with the assignment; other assignments open in the reader, everything else in the browser |
| 9. Sign-in for the Canvas Lite test site | Waiting on the test accounts | |
| 10. Submissions (file upload, text entry) | After 9 | Tested only on the Canvas Lite course |

## What works today

- **Sign-in.** `tools/terminal_login.py` runs UF's normal login in a hidden Edge window, driven from the terminal.
  - You type your GatorLink password and approve Duo (plain push or Verified Push), with progress shown on one spinner line.
  - The session is encrypted with Windows DPAPI under `%LOCALAPPDATA%\TUICanvas`.
  - The app starts sign-in itself if the saved session has expired at launch. Mid-session expiry shows a banner and waits for `L`.
- **Reader** (`target\release\tuicanvas.exe`).
  - Shows course list, assignment list (due date, status, points) and assignment detail (deadline, availability, status, attempts, description with links).
  - **Files.** Each course has Assignments and Files tabs (`Tab`). Files mirrors the course's Canvas folders; `Enter` opens a file in its default app (downloading it first if needed), `d` downloads, `D` downloads a whole folder, `e` shows it in File Explorer, `/` searches every folder. Each file shows whether it is saved, newer on Canvas, downloading or locked.
  - **Assignment files & links.** A numbered list under each description, matching the `[n]` marks in the text. `Tab` or `1`–`9` selects, `Enter` opens (files in their app, other assignments in the reader, anything else in the browser), `d`/`D` download, `e` shows the folder, `o` opens the selected item's page in the browser.
  - Files go to `C:\Users\<you>\TUICanvas` (`e` on the course list opens it). Images in an assignment download when you open it and are removed after 30 days unused.
  - Each screen shows its saved copy at once, with **Syncing** at the top, and is labelled **Outdated** if the fresh read fails.
  - Colours mark deadline urgency and status.
- **Reads**, measured live on one account, 17 courses, 157 assignments:
  - Course list: REST, about 300 ms and 22 KB.
  - Assignment list: GraphQL, median about 300 ms per course. The REST equivalent is about 600 ms with a 9.6 s worst case, and its 944 KB total drops to 64–75 KB. The data matched REST on every assignment.
  - Assignment detail: REST, about 210–250 ms and 4 KB.
- **Measured against the website** ([baseline](../benchmarks/baseline-2026-10-20.md)), same machine and session, median of 10 runs:
  - Fresh content: 0.27–0.33 s per step in the reader vs 2.1–2.7 s on the website.
  - Requests: 1 per screen vs 130–190.
  - Saved copies appear in about 1 ms.
  - The reader's own work (parse, update, draw) takes 1–3 ms; the rest is Canvas's response time.
  - All three build-plan performance goals are met.
- **Downloads**, measured live: a 908 KB PDF in 1.1 s through the full redirect chain, saved, linked and marked as downloaded from the internet.
- **Checks.** 62 Rust tests (Windows) and 32 Python tests pass. `tuicanvas --smoke` re-verifies live reads, file access and one download; `tuicanvas --dump DIR` renders each screen headlessly, including the Files tab, and downloads one file into a scratch library.

## Known issues and limits

- **Sessions don't last long.** Two data points so far: both expired within a day (under 22 h), and each needed a password and Duo again.
- **Sign-in depends on Python.** It needs the project's Python environment, Playwright and Microsoft Edge. The reader itself is a single Rust binary.
- **Windows only** for now: session protection uses DPAPI.
- **Order of undated assignments** differs from Canvas in 1 of 17 courses (cosmetic).
- **Back re-reads the list.** Returning to a list from a detail re-reads it, per the approved "fresh on every entry" rule. This costs one request per Back.
- **Git Bash** may print sign-in progress as separate lines instead of one updating line. PowerShell and Windows Terminal show the spinner properly.
- **The baseline is narrow:** one machine, one evening, 10 runs, with the browser headless. Good for medians, not for tail latency.
- **Images open in an outside app.** Showing images inside the terminal is not done yet (Windows Terminal can display them; a possible later step).
- **Locked and hidden files.** Files Canvas marks as locked are not downloaded (one past lab here). In 8 of 17 courses the instructor hides Files, so the Files tab is empty there; attachments linked from assignments still work. Two linked files return 403 and can only be opened in the browser.
- **Out of scope for now:** modules, pages, discussions, inbox and calendar (links to them open in the browser); submitting comes in Milestone 10. Quizzes and assessments are permanently out of scope.

## What's next

**Now: version 2, submissions** (milestones 9–10 above). Milestone 9 waits for the Canvas Lite test accounts. A survey of all 161 assignments in your 17 courses shaped the plan:

- **How work is handed in:** file upload 63, Canvas quizzes 52, external tools 34, text entry 18.
- **What assignments link to:** 63 distinct Canvas files (32 PDFs, 9 images, 6 Word files), 18 Canvas pages, 28 modules and 49 outside sites.

Quizzes and assessments are permanently out of scope.

After version 2, the remaining version 1 milestones follow. The baseline shows that almost all remaining time is Canvas's network response (≈260–410 ms per read), so the next wins come from overlapping or avoiding reads rather than from faster local code.

1. **Milestone 4.** Decide per resource whether anything beyond the three saved copies is worth reusing. For example, the course list (21 KB, rarely changes) could be shown from its saved copy and refreshed less often. Each change needs your approval under the freshness rules.
2. **Milestone 5.** Prefetch an assignment's detail after its row stays selected for about 150 ms, under the approved 2-second freshness rule. That could hide most of the ≈270 ms detail read. Measure hits and wasted requests with prediction on and off, using `--bench` and `--diag`.
3. **Session lifetime.** Run with `--diag` over several days to learn how long sessions last, and whether choosing "remember this device" in Duo skips the push.
4. **Milestone 6.**
   - Hardening: throttling, resizing, terminal restore on errors, install instructions.
   - Possibly a Rust-native sign-in to remove the Python dependency (a decision for later).

## How to check the current state

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1 -Tasks "pytest build test smoke"
target\release\tuicanvas.exe --dump .tools\dev
target\release\tuicanvas.exe --bench 10
.venv\Scripts\python.exe -m tools.browser_bench --runs 10
```

`--smoke` writes `benchmarks/private/smoke.json` (metadata only). `--dump` writes the rendered screens as text. The two benchmarks write `benchmarks/private/tui-bench.json` and `browser-bench.json`.
