# Request policy ledger

Every network read the reader makes is listed here with its purpose, trigger, and freshness rule. So is every request the Canvas web page made for the same task, with the reason it's omitted from the reader. Nothing is fetched speculatively yet; prediction is Milestone 5.

Evidence:
- The web page's requests come from the analysed HAR (`benchmarks/initial-findings.md`), one assignment-detail page.
- The reader's measurements come from `tuicanvas --smoke` (`benchmarks/private/smoke.json`, kept local). Live figures are filled in under "Measurements" once collected.

## Reads the reader performs

| Read | Request | Fields used | Trigger | Freshness / reuse |
| --- | --- | --- | --- | --- |
| Course list | `GET /api/v1/courses?enrollment_state=active&include[]=term&include[]=favorites&per_page=100` (all pages) | id, name, course_code, term name, is_favorite, access_restricted_by_date | Each entry to the course list, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. Courses restricted by date are hidden. |
| Assignment list | GraphQL `TuiAssignments`: `course.assignmentsConnection(first: 100)` with `_id name dueAt unlockAt lockAt pointsPossible submissionTypes htmlUrl position lockInfo{isLocked}` and `submissionsConnection(first: 1, filter: {states: [unsubmitted, submitted, pending_review, graded, ungraded]})`, all cursor pages. Falls back to `GET /api/v1/courses/:id/assignments?include[]=submission&order_by=due_at&per_page=100` if Canvas rejects the query. | The fields named in the query (no descriptions) | Each entry, including Back; `r` | Saved copy shown with Syncing; fresh read on every entry. A failed page fails the whole list rather than showing a partial one. Sorted by due date (undated last), then case-insensitive name, then id. |
| Assignment detail | `GET /api/v1/courses/:id/assignments/:id?include[]=submission` | The list fields plus description (HTML), lock_explanation, allowed_attempts | Each entry; `r` | Saved copy shown with Syncing; fresh read on every entry. |
| Profile | `GET /api/v1/users/self/profile` | id | Only when the saved session lacks an account id | Not cached; scopes saved copies to the account. |

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
- **Saved copies.** Stored only for these three resources, scoped by Canvas origin and account id. Logout deletes them.

## Web-page requests omitted from the reader

| Observed request (assignment page) | Purpose | Decision for the reading workflow |
| --- | --- | --- |
| Assignment page HTML (875 ms) | Server-rendered shell that bootstraps the JS app | Replaced by the JSON reads above |
| ~140 scripts, 4 stylesheets, fonts, theme images | Web UI | Not needed by a terminal UI |
| GraphQL `GetStudentAssignment` (2.2 KB, started ~3.3 s after navigation) | Assignment, rubric, submission and draft for the web submission view | Replaced by the documented REST detail read, which supplies the displayed fields without needing a submission relay id. Its rubric, drafts and media fields are deferred. A minimal GraphQL detail query was only ~10% faster (median 183 vs 213–252 ms), not enough to justify switching. |
| GraphQL `ExternalTools` | LTI tools offered for submission | Omitted: no submission in v1 |
| GraphQL `NextSubmission` | Submission history excluding the current attempt | Deferred: current status comes from `include[]=submission` |
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
| Session lifetime | A session saved at 4:13 PM was rejected 22 h later; reuse within the same day worked | An expired session at startup opens sign-in directly |

## Open questions

- **Back navigation.** Re-entering a list after viewing a detail costs one list read under the current rule. Revisit only with the user.
- **Session lifetime.** One data point so far (< 22 h). Track it with `--diag` over several days.
