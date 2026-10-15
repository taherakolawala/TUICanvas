# TUICanvas discovery

## Agreed direction

- Build an exceptionally responsive terminal interface for Canvas.
- Initial user: the project owner at one institution. Other users may follow.
- Personal access tokens are ruled out.
- Investigate the institution's actual browser requests and authentication before choosing a transport.
- Benchmark the existing Canvas web interface before implementation.
- Explain optimization options and consult the user on architecture and design decisions.
- Latest decision: assignment lists/details, deadlines, and submission status may show a cached initial snapshot with Syncing at the top while requesting fresh data on access. If sync fails, retain an explicit outdated/error state. This supersedes the earlier rejection of cached initial assignment display.
- Choose fetch, defer, omit, or cache behavior per request/resource after examining its purpose, volatility, and dependencies. There is no blanket content-cache policy.
- Prioritize fewer necessary API calls and a shorter dependency chain, simpler rendering, and selective caching of data that rarely changes across logins.
- User approved prediction rules in docs/build-plan.md, including the two-second single-use result window and bounded speculation.
- User approved building with amendments: terminal-first username/password plus Duo login, browser fallback if necessary, seamless valid-session reuse, cached initial assignment display with sync, and no file handling or module navigation in v1.

## Proposed baseline experiment

Use the same account, courses, machine, and connection throughout. Record browser/version and test conditions. Separate login time from time spent using an authenticated app.

Candidate tasks: dashboard to course; course to assignment details and due date; modules to a content page; return to recently viewed content. Select the tasks based on the user's actual routine.

For each task, define a visible completion condition: the requested information is readable and relevant controls work. Measure action-to-usable-content separately from navigation/load events and request duration. Network idle is not the completion condition.

Collect an initial 5–10 trials per workflow and condition; report individual measurements, median, and range. Do not claim reliable tail percentiles from this small sample.

Conditions:

1. Browser HTTP cache disabled, retaining the authenticated session. This does not imply a cold server cache or a fresh connection.
2. Normal browser cache enabled for repeat visits.
3. Later, equivalent TUI tasks with empty and populated local caches; separately measure time to cached content and time to verified-fresh content.

Capture request method, redacted endpoint template, status, timing phases, transfer size, dependency/initiator, pagination, and cache headers where available. Classify each request as required data, optional data, presentation assets, or unrelated/background work. Inspect payload shape locally only as needed. Keep credentials and private course payloads out of committed benchmark artifacts; sanitized HAR files may still contain private URLs and response content.

Use a browser performance trace alongside the network waterfall to distinguish server/network delay from script execution, layout, and rendering. Tool orchestration latency must not be counted as app latency.

## Architecture hypotheses, not decisions

- Render cached data only for approved resource types. Assignment snapshots are now explicitly approved for initial display during a fresh request; never hide sync failures or skip required refreshes.
- Keep network and parsing work off the UI event loop.
- Persistent storage and any local search index must respect per-resource freshness rules. Their scope and implementation are undecided.
- Fetch metadata before bodies/attachments; prioritize visible content.
- Deduplicate in-flight requests and bound concurrency.
- Use conditional requests only where the server actually supports validators.
- Record a decision for each observed request: purpose, fields needed by the view, dependencies, volatility, fetch trigger, cache eligibility/invalidation, and evidence still needed. Omission decisions are specific to a workflow, not universal endpoint bans.
- Evaluate browser-session-based transport, a browser bridge, and institution-supported OAuth. Determine session expiry and reauthentication behavior experimentally. Do not assume captured requests will work outside the browser.
- Rust/Ratatui/Tokio/reqwest are approved; prove authentication before expanding the UI. A small Python auth probe is a temporary development tool.

## Discovery status

The installation is https://ufl.instructure.com/. Opening it in the connected browser redirected to https://elearning.ufl.edu/. The GatorLink link points to `/login/saml/355` on the Canvas installation and redirects to UF's SAML login at login.ufl.edu. The user completed sign-in. An initial user-provided HAR has been analyzed; it captures one assignment-detail navigation rather than the full selected workflow. See benchmarks/initial-findings.md. Repeated baseline and usable-content measurements are still pending.

This establishes the interactive login entry point, but does not yet establish session lifetime, cookie behavior, API access through the session, or whether requests can be made outside the browser.

The terminal-driven browser-engine helper completed a live Duo Verified Push login with user input. DPAPI-protected session storage and a native Canvas profile read succeeded. A later native check reused the session without launching a browser. The terminal UX now automatically advances known loading/approval/success stages and presents a clear code and y/n remembered-device choice. See docs/authentication.md for commands and remaining limits.

The user selected dashboard → GPU Computing assignments → assignment instructions and due date. Inspected Lab-2 Assignment: the list displays the deadline; the detail page displays the deadline and links to slides for requirements. The slides open a file-preview overlay that reports "This file is currently locked." The successful baseline currently ends at assignment details; document access is a separate blocked phase. See benchmarks/README.md for capture steps.

The connected browser's documented inspection interfaces provide page state, screenshots, and console logs; its advertised capabilities do not include a network recorder or performance tracer. Do not substitute tool-call duration for page performance. Full network benchmarking needs an appropriate capture method, such as a locally exported DevTools HAR paired with a performance trace.

## Reference documentation

- Canvas pagination: https://developerdocs.instructure.com/services/canvas/basics/file.pagination
- Canvas authentication: https://canvas.instructure.com/doc/api/file.oauth.html
- Chrome network inspection: https://developer.chrome.com/docs/devtools/network/reference/
