# Initial Canvas capture findings

Source: local `ufl.instructure.com.har`, analyzed on 2026-10-15. Raw capture is ignored by Git. Aggregate output is in `benchmarks/private/warm-01-summary.json`.

## Scope and limits

The HAR contains one recorded page, corresponding to the selected GPU Computing Lab-2 assignment. It does not contain the full dashboard → assignment-list → detail sequence. This is a single observation, not a median baseline or a reliable speedup estimate. Browser version, cache-toggle setting, and throttling setting were not recorded independently. Cache markers show that some responses were served from cache.

No usable-content timestamp is available. Browser load events and request completion are not equivalent to visible, usable content. No CPU/rendering trace was captured.

## Observations

| Metric | Observed value |
| --- | ---: |
| HAR entries | 183 |
| Entries classified by browser as script requests | 120 |
| Responses with JavaScript MIME types (different classification) | 140 |
| Reported transferred bytes across all entries | 1,405,665 |
| Entries with explicit cache markers | 101 |
| Requests to the Canvas installation host | 14 |
| Assignment HTML request duration | 875.3 ms |
| Assignment HTML waiting phase | 868.9 ms |
| DOMContentLoaded event relative to navigation | 1,902.0 ms |
| Load event relative to navigation | 3,678.3 ms |
| GetStudentAssignment request start relative to first document request | 3,305 ms |
| GetStudentAssignment duration | 480.6 ms |
| GetStudentAssignment waiting phase | 471.9 ms |
| GetStudentAssignment reported transfer size | 2,247 bytes |

The core assignment response therefore finished approximately 3.79 seconds after the first document request started. Most of the time before its completion preceded the data request itself. This is a scheduling/dependency opportunity; the HAR alone cannot attribute that preceding interval to script execution, download, initialization, or other causes.

The successful `GetStudentAssignment` GraphQL response contains assignment `name`, `description`, `dueAt`, `pointsPossible`, availability/lock fields, and submission information. It is a concrete candidate for populating the proposed assignment-detail view. Whether a smaller REST or GraphQL request would be preferable remains untested.

Other observed operations include `ExternalTools`, `NextSubmission`, and `GetUserGroups`; REST calls include release-note unread counts, module sequence, and LTI launch definitions. These can be investigated for deferral in a reading-focused view, but are not proven universally unnecessary. The identical module-sequence GET URL occurs three times; the last is much later, so not all repeats should be treated as redundant concurrent work.

The capture also includes static assets, integration services, and telemetry endpoints. Cached scripts still may incur processing costs, but those costs require a performance trace to measure.

## Authentication evidence

The captured GraphQL requests have a CSRF-header name and successfully returned authenticated content. Cookie and Authorization values are absent from the sanitized export. Their absence is not evidence that the requests work anonymously. The capture does not establish that the same requests can be replayed by a standalone client, session lifetime, or reauthentication behavior. Do not request a credential-bearing HAR simply to fill this gap.

## Recommended next decisions

1. Latest user decision: fetch assignments on access, allowing a cached initial view with Syncing at the top. Deadlines/status may be shown from that snapshot while refresh runs; failures must remain visible. This supersedes the earlier prohibition on cached initial assignment display.
2. Analyze request-by-request whether to fetch on access, defer until needed, omit for a particular view, or cache because the data rarely changes. Assignment snapshot display is explicitly approved; it does not remove refresh requirements.
3. Test an authenticated transport separately from rendering. Browser-assisted login is still a candidate; direct requests versus a browser bridge remain unresolved.
4. Use an eventual performance trace and repeated captures to quantify usable-content latency. Complete dashboard/list coverage before calling this an end-to-end baseline.

Potential gain: bypass the browser page's dependency sequence, reduce requests to those needed for the chosen view, simplify rendering, and selectively cache stable resources. Assignment access still requires a fresh request. No achieved TUI speedup has been measured yet.
