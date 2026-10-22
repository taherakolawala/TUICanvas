# Request policy ledger

Every network read the reader makes is listed here with its purpose, trigger, and freshness rule. So is every request the Canvas web page made for the same task, with the reason it's omitted from the reader. Nothing is fetched speculatively yet; prediction is Milestone 5. The one automatic download (images in an assignment the user opened) is listed under file downloads.

Evidence:
- The web page's requests come from the analysed HAR (`benchmarks/initial-findings.md`), one assignment-detail page.
- The reader's measurements come from `tuicanvas --smoke` (`benchmarks/private/smoke.json`, kept local). Live figures are filled in under "Measurements" once collected.

## Reads the reader performs

| Read | Request | Fields used | Trigger | Freshness / reuse |
| --- | --- | --- | --- | --- |
| Course list | `GET /api/v1/courses?enrollment_state=active&include[]=term&include[]=favorites&per_page=100` (all pages) | id, name, course_code, term name, is_favorite, access_restricted_by_date | Each entry to the course list, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. Courses restricted by date are hidden. |
| Assignment list | GraphQL `TuiAssignments`: `course.assignmentsConnection(first: 100)` with `_id name dueAt unlockAt lockAt pointsPossible submissionTypes htmlUrl position lockInfo{isLocked}` and `submissionsConnection(first: 1, filter: {states: [unsubmitted, submitted, pending_review, graded, ungraded]})`, all cursor pages. Falls back to `GET /api/v1/courses/:id/assignments?include[]=submission&order_by=due_at&per_page=100` if Canvas rejects the query. | The fields named in the query (no descriptions) | Each entry, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. A failed page fails the whole list rather than showing a partial one. Sorted by due date (undated last), then case-insensitive name, then id. |
| Assignment detail | `GET /api/v1/courses/:id/assignments/:id?include[]=submission` | The list fields plus description (HTML), lock_explanation, allowed_attempts | Each entry; `r` | Saved copy shown with Syncing; fresh read on every entry. |
| Past submissions | `GET /api/v1/courses/:id/assignments/:id/submissions/self?include[]=submission_history` | Attempt numbers, dates, type, status, grade when visible, submitted text/URL and file names/sizes | Only `h` from an assignment; `r` while viewing history | One fresh request per visit. Prior response may remain visible while syncing, but submission history is kept in memory only and never saved to disk. |
| Announcement list | GraphQL `TuiAnnouncements`: `discussionsConnection(first: 100, filter: {isAnnouncement: true})`, all cursor pages (at most 20) | id, title, posted/delayed/created dates, author short name, participant read state, reply count; no message bodies | Each entry, including Back; `r` | Saved copy with Syncing; every visit re-reads. Sort all metadata by posting date, then delayed date, then creation date (missing dates last). The connection is created-time ordered, so sorting just its first page cannot guarantee the latest published items. Schema rejection falls back to the paginated REST discussion list with `only_announcements=true`. Other failures retain an Outdated saved copy. Missing/repeated cursors and page-limit exhaustion fail the entire refresh. |
| Announcement detail | `GET /api/v1/courses/:id/discussion_topics/:id` | Summary fields, message HTML, attachments, locked-for-user/reason, is-announcement | Each entry; `r` | Saved copy with Syncing. Validate the returned ID/type. Render the message returned by Canvas even when comments are closed; Canvas itself substitutes an access explanation when reading is restricted. `lock_info.can_view` controls link/attachment visibility, not `locked_for_user`. Reading marks it read locally, without a Canvas write; replies remain in the browser. |
| Files chosen from announcements | `GET /api/v1/files/:id`, followed by the existing download flow if needed | Current file metadata | Only `Enter`, `d`, or `D` on announcement files (including inline images) | Refresh metadata on each choice; a recent in-flight read may be shared. Validate the current version before reusing a local copy. No speculative file reads or automatic image downloads for announcements. |
| Course files | `GET /api/v1/courses/:id/folders?per_page=100` (at most 20 pages) and `GET /api/v1/courses/:id/files?per_page=100` (at most 50 pages), run together | Folders: id, name, full_name, parent_folder_id, locked/hidden for user. Files: id, display_name, filename, size, content-type, updated_at, modified_at, url, folder_id, locked_for_user, lock_explanation | Each entry to a course's Files tab, including Back to it from outside the tab; `r`. Moving between folders and searching read nothing. | Saved copy shown with Syncing; fresh read on every entry. A 403 on the files list means the instructor hid Files: the tab says so and the folders are not shown. |
| File details | `GET /api/v1/files/:id` | As for the files list | For each file linked from an assignment, each time that assignment's detail is read; also when a linked file is opened and its details are unknown | Saved copy kept for the list's display; fresh read with every detail read. Downloads wait for an in-flight read. |
| Profile | `GET /api/v1/users/self/profile` | id | Only when the saved session lacks an account id | Not cached; scopes saved copies to the account. |

## File downloads

| Download | Request | Trigger | Rules |
| --- | --- | --- | --- |
| A file the user chose | The file's `url` from its details (`/files/:id/download?download_frd=1&verifier=…` on the Canvas origin), then Canvas's redirects: `*.canvas-user-content.com` → `inst-fs-*.inscloudgate.net` → `cdn.inst-fs-*.inscloudgate.net` | `Enter` (download if needed, then open), `d`, `D` (all files of a folder and its subfolders, or of an assignment) | Not downloaded again while the saved copy matches Canvas's version (modified time and size). Locked files are not requested. |
| Images in an assignment | Same | When the assignment's detail is read (which the user started by opening it), after the image's details arrive | Stored in the assignment's folder; removed after 30 days unused unless the user opened or downloaded it |

- **Credentials.** Canvas cookies go only to the Canvas origin, on the first hop. Every later hop must be HTTPS on port 443 to one of Canvas's two storage domains and gets no cookies (its address is signed). Anything else stops the download.
- **Limits.** At most 2 downloads at once, separate from the 4 read slots; at most 6 redirects; at most 2 GB per file. The body streams to `.tuicanvas\partial` and is installed only when complete and the length matches.
- **After download.** The file gets the "downloaded from the internet" mark (Zone 3) that browsers add. It opens in its default app; programs and scripts are shown in File Explorer instead of being opened.

Shared rules:
- **Freshness.** A response may satisfy an access only if its request began no more than 2 s before that access (an in-flight read is promoted, not duplicated). Explicit refresh always issues a new read. A response from an older request never replaces data requested later.
- **Failures.**
  - The displayed copy stays on screen and is labelled Outdated.
  - An authentication failure (401 "authorization required", redirect to `/login`, or CSRF rejection) asks the user to sign in.
  - A permission failure (401 "unauthorized" or 403) is shown as no access, without prompting a sign-in.
  - Rate limiting (403 "Rate Limit Exceeded" or 429) backs off and retries twice.
- **Requests.**
  - At most 4 network operations at once, over one pooled HTTPS client.
  - GraphQL is used only for the reviewed, read-only list query above.
  - Canvas cookies only: identity-provider cookies are never sent to Canvas.
  - The CSRF token is sent only on GraphQL POSTs.
  - Paths are restricted to `/api/v1/` and `/api/graphql`, and pagination links must stay on the same origin.
- **Saved copies.** Stored for the course list, assignment lists, assignment details, course file listings and file details, scoped by Canvas origin and account id. Logout deletes them. Downloaded files are not saved copies: they stay in the file library until you delete them (automatic images: 30 days unused).

## Web-page requests omitted from the reader

| Observed request (assignment page) | Purpose | Decision for the reading workflow |
| --- | --- | --- |
| Assignment page HTML (875 ms) | Server-rendered shell that bootstraps the JS app | Replaced by the JSON reads above |
| ~140 scripts, 4 stylesheets, fonts, theme images | Web UI | Not needed by a terminal UI |
| GraphQL `GetStudentAssignment` (2.2 KB, started ~3.3 s after navigation) | Assignment, rubric, submission and draft for the web submission view | Replaced by the documented REST detail read, which supplies the displayed fields without needing a submission relay id. Its rubric, drafts and media fields are deferred. A minimal GraphQL detail query saved only 30–70 ms (median 183 ms vs 213–252 ms), not enough to justify leaving the documented read. |
| GraphQL `ExternalTools` | LTI tools offered for submission | Omitted: no submission in v1 |
| GraphQL `NextSubmission` | Submission history excluding the current attempt | Replaced by the documented REST single-submission read with `include[]=submission_history`, requested only when the student presses `h` |
| GraphQL `GetUserGroups` | The user's groups, for group assignments | Deferred until group assignments are displayed specially |
| `module_item_sequence` (3 requests) | Previous/next module navigation | Omitted: module navigation is out of scope for v1 |
| `lti_apps/launch_definitions` | LTI placements on the assignment view | Omitted: no LTI launches |
| `release_notes/unread_count` | Canvas "what's new" badge | Omitted |
| `pages/account-settings` (course page read) | Institution-specific page script | Omitted |
| `web-app-manifest`, thumbnails, telemetry endpoints | Browser and analytics | Omitted |

## Decisions from measurements

Live read-only runs of `tuicanvas --smoke` on 2026-10-16 (one account, 17 active courses, 157 assignments, Windows, home connection):

| Question | Result | Decision |
| --- | --- | --- |
| Do native reads work outside the browser? | Profile, courses, assignment lists and details all return 200 using the saved session | Native transport; no browser bridge |
| Assignment list: REST vs field-selected GraphQL | REST 944 KB total, median 520–980 ms per course (slowest 9.6 s). GraphQL 64–75 KB, median 290–440 ms (slowest 1.9 s). | Use GraphQL, with a REST fallback |
| Same data? | After requesting unsubmitted submissions explicitly (the filter's default hides them, and with them the **missing** flag), the two match on every assignment: ids, due dates, points, status, locks, submission types, links | Required before switching; met |
| Same order? | Canvas orders equal or undated due dates by name. A case-insensitive name tie-break reproduces REST order on 16/17 courses; the other differs only among undated assignments | Accepted as cosmetic |
| Assignment detail: REST vs GraphQL | Median 213–252 ms vs 183 ms; 3.9 KB vs 0.7 KB | Keep REST (documented; margin small) |
| Is CSRF needed for GraphQL reads? | Yes: without `X-CSRF-Token`, Canvas answers HTTP 422 "An error occurred." | Send it; treat a 422 on a GraphQL read as a session problem |
| Failure classes | No session → 401 "user authorization required"; course not enrolled → 404; rate-limit headers report 700 remaining, cost ≈0.1–0.3 per read | Classified as sign-in needed vs not found vs no access |
| Course Files listings (2026-10-21) | Folders list in all 17 courses; the files list is refused (403) in 8, where the instructor hid Files. Largest course: 133 files over 2 pages; folders at most 3 deep. One course's listing: 2 requests, 14 KB, about 310–340 ms. | Read both lists together; treat 403 on files as hidden |
| Files linked from assignments (2026-10-21) | 63 distinct across all courses: 59 readable, 1 locked, 2 refused (403), 1 not found. In one course, a past lab's file reads as locked ("This file is currently locked.") even though its details include an address. | Respect `locked_for_user`; show Canvas's reason; offer the browser for unreadable files |
| Download path (2026-10-21) | Canvas origin (cookies) → `canvas-user-content.com` → `inst-fs-iad-prod.inscloudgate.net` → `cdn.inst-fs-iad-prod.inscloudgate.net`, which serves the file (and supports ranges). A 660 KB PDF took about 1.0 s, a 908 KB PDF 1.1 s; sizes matched Canvas. Hard links work on the user's drive. | Manual redirects with the host rules above |
| Session lifetime | A session saved at 4:13 PM was rejected 22 h later; reuse within the same day worked | An expired session at startup opens sign-in directly |

## Open questions

- **Back navigation.** Re-entering a list after viewing a detail costs one list read under the current rule. Revisit only with the user.
- **Session lifetime.** One data point so far (< 22 h). Track it with `--diag` over several days.

## Announcements decision (2026-10-21)

The saved probes cover 17 courses and 1,380 announcements. In the largest course, REST fetched 3.54 MB over seven pages in about 24 seconds; much of this was message HTML that the list does not need. A 25-row GraphQL metadata sample was about 8.6 KB/329 ms versus REST's 182 KB/1,131 ms. These are probe measurements, not a promise about the full reader's latency.

GraphQL's first 50/100 nodes matched the REST top-25 set in these samples after local sorting, but that alone does not establish correctness for future delayed posts. The implementation reads all small metadata pages before sorting; full bodies remain on demand. Unknown participant state is shown as Unknown unless you have read the announcement locally. There is no all-courses polling/feed or mark-read mutation.

Reference: [Canvas Discussion Topics API](https://developerdocs.instructure.com/services/canvas/resources/discussion_topics). Local evidence: `benchmarks/private/announcements-probe.json`, `announcements-parity.json`; these contain counts/timings/schema only. The Rust live check is `cargo test --release live_announcements -- --ignored --nocapture` and writes `announcements-live.json` in the same private directory.

The Rust live check passed on all 17 courses (1,380 announcements, 13 detail reads): list median 237 ms, total list data 415,299 bytes. The largest course (674 items) needed seven requests and 5.43 s / 204,572 bytes. This is a remaining latency cost of fetching complete metadata; saved copies display throughout the refresh. Measurements are single runs, not a statistical benchmark.

Local announcement reads are stored separately from network snapshots in `local-announcement-reads.json`, scoped by origin/account and keyed by course/topic ID. An open detail with an available message and view permission marks Read; listing, failed/denied reads, and responses arriving after leaving the detail do not. Cached bodies displayed on entry count as read. Later server refreshes cannot reset this local indicator. Logout clears it with that account’s saved copies. No network calls are added.
