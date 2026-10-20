# TUICanvas build plan

Status: approved by the user on 2026-10-15 with the amendments incorporated below. Milestones 1 and 2 are complete; see [status.md](status.md) for current progress and next steps.

## Objective and boundaries

Build a Windows-first, keyboard-driven Canvas reader for the user's UF account, with portable core code and a configurable Canvas origin. Optimize the number and scheduling of necessary requests, the amount of data processed, and terminal rendering. Cache only resources whose individual policies justify reuse. Personal access tokens are excluded.

First complete slice: terminal sign in with username/password and Duo → course list → GPU Computing assignment list → assignment details, deadline, and current submission status. Expose refresh, back, filtering of the loaded list, error/retry state, and open-in-browser for the assignment. Course-wide full-text search, global calendar, inbox, discussions, module navigation, native assignment submissions, quiz taking, file opening/downloading/preview, and broad institution support are later work. No assignment writes or speculative mutations in the first version. Authentication form submissions are required for login.

File handling is explicitly deferred to v2, including PDF, DOC/DOCX, TXT, and other attachments. Preserve attachment references in description text, but do not implement a file viewer, downloader, or system-viewer launch in v1. The user has a design idea for that later version.

## Evidence and missing evidence

The existing HAR covers one assignment-detail navigation: 183 entries, 1,405,665 transferred bytes, assignment HTML taking 875 ms, and GetStudentAssignment starting about 3,305 ms after navigation and taking 481 ms. Its 2,247-byte response contains the main data needed for our detail view. These are one-run observations, not a measured TUI improvement.

Missing: complete course/list request coverage, usable-content timing, repeatability, authenticated request behavior outside the page, session renewal, and the actual benefit/cost of prefetch. Gather this during the first experiments; do not delay all implementation for an exhaustive site crawl.

## Authentication experiment comes first

Prefer a complete terminal GatorLink/SAML login: prompt for username and masked password, then display/handle the Duo challenge when the actual flow permits it. Keep passwords in memory only and do not log them. The user explicitly accepts normal browser login as fallback if terminal login is unreliable or unsupported. Validate one minimal authenticated read before committing to a transport. A browser helper is optional, not the default user interface.

Cold login must support username/password plus Duo; later launches should restore valid session state without prompting. Preserve session continuity as Canvas/UF permit, including normal remembered-device behavior where supported. Server-enforced expiry or renewed MFA can still require interaction. Persist session secrets with OS protection, independently of the content cache.

Preferred outcome: transfer only the required Canvas-origin session material through local memory to the native client, preserving cookie scope/expiry and the CSRF behavior actually required. Verify both an appropriate REST read and the observed read-only GraphQL operation. Do not assume a successful browser login guarantees native API access. Do not copy a user's unrelated browser profile or request credential-bearing HAR exports.

Check redirects to login, 401/403 responses, session expiry, logout, and GraphQL error payloads even when HTTP status is 200. An authentication failure must lead to a clear sign-in flow rather than endless retries. Distinguish expired authentication from lack of access to a particular resource.

If native authenticated reads work, keep the browser out of routine navigation. If they do not, evaluate a narrowly scoped local browser bridge that performs allowlisted Canvas reads inside the authenticated context. Report the bridge's memory/startup/installation cost and consult the user before making it the permanent transport. An extension is another possible delivery mechanism, not an up-front requirement.

Any helper uses authenticated local IPC, bound to the current user/session, not an unauthenticated HTTP proxy. Credentials stay out of logs, the content store, and source control. Use Windows credential protection for any persistent native session secrets; session persistence is a separate implementation choice from content caching. External attachment redirects must not receive Canvas authorization/CSRF headers indiscriminately.

Exit criterion: a reproducible fresh read for the selected assignment, plus predictable sign-in/error handling. If that cannot be established, resolve the transport issue before building a large UI around an assumption.

## Proposed implementation

Rust + Ratatui/Crossterm for the terminal UI; Tokio for asynchronous work; reqwest with one reused client for native HTTP. Pin compatible versions at implementation. Rust/Cargo were not found on PATH during planning, so official toolchain setup is part of the approved build work. Browser login may use a small Node helper if the experiment needs it; Node is available. The existing Python HAR analyzer remains a development tool.

Use one application process and internal modules, not separate background services:

- UI: selection, navigation, visible lists, text formatting, loading/error states.
- Application/model: typed course/assignment views and navigation generations.
- Request policies and scheduler: demand/speculation priority, coalescing, expiry, retries, cancellation, resource policy.
- Canvas client: typed REST/GraphQL reads, pagination, response validation, HTML-to-terminal text conversion.
- Transport/session: native HTTP or a proven browser bridge behind a small interface.
- Approved resource store: only explicitly cacheable resources and settings. Start with a small versioned file store; add SQLite if measured storage/query needs warrant it.
- Diagnostics: timing, request counts, bytes, cache decisions, prediction outcomes, no credentials or private response bodies.

Ratatui diffs terminal cells; it does not eliminate widget computation automatically. Trigger rendering on input, state changes, resize, and a limited loading indicator tick. Render only visible rows; avoid reparsing rich content on every keypress. Keep networking and expensive parsing off the UI event loop. Convert HTML to readable text with headings/lists/links, suppress scripts, and strip terminal control sequences from remote content. Preserve links to content that needs a browser or document viewer.

## Request-by-request policy ledger

Before implementing a request, record its fields/purpose, workflow dependency, observed latency/bytes, volatility, access requirements, fetch trigger, reuse/invalidation rules, and why it can be omitted or deferred. Keep an unclassified bucket until each observed request has an explanation. Static asset families may share a justified policy, but preserve per-entry traceability in local diagnostics. Request counts alone do not establish critical-path impact.

Initial proposals:

| Resource or observed operation | Proposed treatment |
| --- | --- |
| Assignment list | Show cached snapshot initially with a top-level Syncing indicator, then fetch fresh on every entry; respect pagination and personalized dates |
| GetStudentAssignment / equivalent supported read | Show cached snapshot while fetching fresh per access; benchmark minimal fields versus the observed operation |
| Current submission status and effective deadline | May initially display cached values during sync; mark outdated on failure and replace atomically after successful refresh |
| ExternalTools, launch definitions | Omit integration menus/submission configuration from v1; retain external-assignment identity/link using assignment data where available |
| NextSubmission | Determine whether the main detail response suffices for the status we display; defer history/drafts until requested if independent |
| GetUserGroups | Keep if required to interpret this assignment's group behavior; otherwise defer |
| module_item_sequence | Module navigation is out of scope; omit for the list/detail route after verifying independence |
| Release-note count, badges unrelated to the task | Omit from the first reading workflow |
| Web JS/CSS/fonts/theme images and web telemetry | Do not load for the native detail view; browser-login assets remain part of login cost |
| Course labels, navigation metadata, profile preferences | Cache candidates, not blanket approvals; enrollments/access changes need validation and invalidation rules |
| Documents | File opening/downloading/preview is out of scope for v1; no speculative file requests |

Never treat all POSTs as writes or all GETs as harmless prediction targets. Register exact, reviewed read operations, including GraphQL queries. Prefer documented REST where it supplies equivalent correct data; reuse observed GraphQL only when justified. Do not assume fewer GraphQL fields or fewer requests necessarily produce lower server latency—measure.

## Approved assignment display and prediction policy

The user's latest instruction permits cached assignment lists, details, deadlines, and submission status for the initial display, with Syncing at the top while a fresh request runs. A failed sync must leave an explicit outdated/error indicator; do not falsely mark the snapshot current. With no cache, show a loading view. Do not overwrite good snapshots with failed or partial responses. Scope snapshots to the Canvas origin and account; logout/account changes must not leak the previous account's content.

The user accepted the rest of the plan: a response from the current navigation prediction may satisfy one upcoming access if its request began no more than two seconds before that open, belongs to the current account/course/navigation context, and no invalidation occurred. Older predictions cannot satisfy the fresh read (though an available snapshot may still be displayed during sync). Requests with an old start timestamp do not become eligible merely because they complete recently. A qualifying in-flight prediction may be promoted to the demand request. A second visit requires another fresh read. Explicit refresh always fetches again.

The diagnostics must distinguish initial cached display from successful synchronization and from a timely prediction hit. Stable-resource cache policies remain case-by-case; permission to display assignment snapshots does not authorize skipping their refresh.

Begin with deterministic intent signals: after a row remains selected for roughly 150 ms, consider fetching that row's details. Do not fetch every row while scrolling. Do not speculate writes, file downloads, LTI launches, or resource reads known to cause meaningful side effects. Prioritize intentional navigation over speculation.

Initial experimental budget: at most one speculative request in flight and a small bounded result slot. Start with at most four concurrent network operations overall, reserving demand capacity. These are tunable starting values. Stop scheduling obsolete predictions; cancel local work where useful, while recognizing cancellation cannot undo server work already started. Account/course/navigation generation checks prevent late results from appearing in the wrong view.

Track useful hits, misses, expired/discarded results, bytes and requests wasted, time saved on the demand path, throttling, and demand interference. Compare prediction enabled versus disabled. Keep prediction only when the measured benefit warrants its request cost. Later consider a small local transition-frequency model if deterministic selection is insufficient; no ML service is needed initially.

## Additional optimizations

Reuse HTTP connections; launch independent required reads concurrently; coalesce equivalent in-flight reads only within the same freshness context; filter existing list rows locally; defer attachments and integrations; avoid unnecessary response fields; use conditional reads only for individually approved resources where supported; back off under throttling. Never hide failed fresh reads behind old assignment data. A page of results is not a complete course list—track pagination explicitly.

## Milestones and verification

1. Auth and request experiment: prove login, fresh assignment read, response shape, and failure behavior. Capture missing course/list requests. Deliver the initial policy ledger and chosen transport evidence.
2. Reader vertical slice with prediction disabled: course/list/detail navigation, cached initial display plus Syncing indicator, deadlines/status, description text, refresh/back/filter, assignment-page browser fallback. Validate effective dates, time zones, pagination, locked assignments, and missing fields against Canvas. File opening and module navigation are excluded.
3. Rendering and network baseline: event-driven visible-row rendering, pooled client, concurrency where independent, eligible in-flight deduplication. Instrument navigation, first required request, response, parse completion, and first useful frame separately.
4. Selective resource reuse: implement only individually decided stable-resource policies, including invalidation and account separation. Verify fresh assignment reads continue on every access.
5. Prediction experiment: implement the approved freshness mode and one-target predictor, compare enabled/disabled runs, tune or remove ineffective speculation.
6. Hardening and delivery: expiry/relogin, failures/throttling, back navigation during requests, Unicode/terminal resizing, terminal restoration after errors, clean builds, Windows run/install instructions, and redacted diagnostics.

Use meaningful tests for scheduler freshness/coalescing/races, cross-account separation, session failure classification, parser/terminal escaping, pagination, and invalidation. Use sanitized synthetic fixtures and an injectable clock/transport; exercise read-only live integration separately. No live submissions as tests.

Performance goals are provisional targets: release-build first shell frame within 200 ms excluding login/network, local key-to-frame work within 16 ms for typical screens, and response-to-usable-view processing within 30 ms for typical assignment payloads. Measure terminal presentation separately when tooling permits. No fixed network latency promise from one HAR.

Compare 5–10 initial runs of equivalent browser/TUI workflows with identical account, data, machine, connection, and documented cache/prediction conditions. Report individual results, median/range, request count, bytes, CPU/memory where available, and prediction waste. More samples are required for defensible tail percentiles. Include browser-helper overhead when relevant. Acceptance requires fresh correct content, responsive input during network waits, understood request policies, and demonstrated workflow improvement—not a claimed speedup from request counts.

## Approval and remaining consultation

The user approved building with Rust, the narrowed reader scope, authentication experiment, milestone order, and the prediction rules, amended to permit cached initial assignment display with Syncing. Cold login should be entirely in the terminal if feasible, with browser fallback accepted. Later login should reuse valid session state. File handling and module navigation are excluded from v1. Broader features and any permanent browser-bridge tradeoff remain consultation points.

## Technical references

- Ratatui rendering: https://ratatui.rs/concepts/rendering/under-the-hood/
- reqwest client reuse: https://docs.rs/reqwest/latest/reqwest/struct.Client.html
- Canvas throttling: https://dhs.instructure.com/doc/api/file.throttling.html
