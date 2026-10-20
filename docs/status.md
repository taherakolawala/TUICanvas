# Project status

Last updated: 2026-10-20. Update this file whenever a milestone moves.

## Where things stand

How the pieces fit together is described in [architecture.md](architecture.md). Milestones 1 and 2 of [the build plan](build-plan.md) are done. Milestones 3–6 have not started, apart from the saved copies that came with Milestone 2.

| Milestone | State | What exists |
| --- | --- | --- |
| 1. Sign-in and request experiment | **Done** | Terminal sign-in (GatorLink + Duo); live-verified native REST and GraphQL reads; failure classification; [request policy](request-policy.md) |
| 2. Reader vertical slice | **Done** | Courses → assignments → assignment detail, with saved copy + Syncing, refresh, back, filter, open in browser |
| 3. Rendering and network baseline | Not started | `--dump` and `--diag` record render and request timings; no browser comparison yet |
| 4. Selective resource reuse | Partly | Saved copies for courses, assignment lists and details only (always re-read on entry) |
| 5. Prediction experiment | Not started | |
| 6. Hardening and delivery | Not started | |

## What works today

- **Sign-in.** `tools/terminal_login.py` runs UF's normal login in a hidden Edge window, driven from the terminal.
  - You type your GatorLink password and approve Duo (plain push or Verified Push), with progress shown on one spinner line.
  - The session is encrypted with Windows DPAPI under `%LOCALAPPDATA%\TUICanvas`.
  - The app starts sign-in itself if the saved session has expired at launch. Mid-session expiry shows a banner and waits for `L`.
- **Reader** (`target\release\tuicanvas.exe`).
  - Shows course list, assignment list (due date, status, points) and assignment detail (deadline, availability, status, attempts, description with links).
  - Each screen shows its saved copy at once, with **Syncing** at the top, and is labelled **Outdated** if the fresh read fails.
  - Colours mark deadline urgency and status.
- **Reads**, measured live on one account, 17 courses, 157 assignments:
  - Course list: REST, about 300 ms and 22 KB.
  - Assignment list: GraphQL, median about 300 ms per course. The REST equivalent is about 600 ms with a 9.6 s worst case, and its 944 KB total drops to 64–75 KB. The data matched REST on every assignment.
  - Assignment detail: REST, about 210–250 ms and 4 KB.
- **Checks.** 42 Rust tests (Windows) and 29 Python tests pass. `tuicanvas --smoke` re-verifies live reads; `tuicanvas --dump DIR` renders each screen headlessly.

## Known issues and limits

- **Sessions don't last long.** Two data points so far: both expired within a day (under 22 h), and each needed a password and Duo again.
- **Sign-in depends on Python.** It needs the project's Python environment, Playwright and Microsoft Edge. The reader itself is a single Rust binary.
- **Windows only** for now: session protection uses DPAPI.
- **Order of undated assignments** differs from Canvas in 1 of 17 courses (cosmetic).
- **Back re-reads the list.** Returning to a list from a detail re-reads it, per the approved "fresh on every entry" rule. This costs one request per Back.
- **Git Bash** may print sign-in progress as separate lines instead of one updating line. PowerShell and Windows Terminal show the spinner properly.
- **No performance comparison against the Canvas website yet.** The only browser capture is one assignment page (`benchmarks/initial-findings.md`).
- **Out of scope for v1:** files and attachments (links are shown, not opened), modules, discussions, inbox, calendar, submitting.

## What's next

1. **Milestone 3, baseline comparison.**
   - Record the browser workflow (dashboard → GPU Computing → Lab-2) 5–10 times with DevTools Performance and an exported HAR, following `benchmarks/README.md`. *Needs you: browser captures.*
   - Time the same workflow in the reader with `--diag` and `--dump`.
   - Report medians and ranges for time to usable content, request counts and bytes.
2. **Session lifetime.** Run with `--diag` over several days to learn how long sessions last, and whether choosing "remember this device" in Duo skips the push.
3. **Milestone 4.** Decide per resource whether anything beyond the three saved copies is worth reusing (for example, the course list, which rarely changes).
4. **Milestone 5.** Prefetch an assignment's detail after its row stays selected for about 150 ms, under the approved 2-second freshness rule. Measure hits and wasted requests with prediction on and off.
5. **Milestone 6.**
   - Hardening: throttling, resizing, terminal restore on errors, install instructions.
   - Possibly a Rust-native sign-in to remove the Python dependency (a decision for later).

## How to check the current state

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1 -Tasks "pytest build test smoke"
target\release\tuicanvas.exe --dump .tools\dev
```

`--smoke` writes `benchmarks/private/smoke.json` (metadata only); `--dump` writes the rendered screens as text.
