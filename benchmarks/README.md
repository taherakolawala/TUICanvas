# Canvas baseline capture

Status: workflow inspected and one assignment-detail HAR analyzed (see initial-findings.md). Repeated end-to-end baseline and usable-content timings remain pending.

## Selected workflow

Start from the loaded Canvas dashboard. Open GPU Computing's Assignments shortcut, then Lab-2 Assignment.

Completion A: the assignment list shows the Lab-2 link and its deadline.
Completion B: the assignment page shows its title, deadline, and the description linking to Lab-2 slides.
Completion C (separate attachment phase): click Lab-2 slides and wait for the PDF to be readable. The assignment requirements live in this file, so B alone is not the full reading task. **Currently blocked:** the preview reports "This file is currently locked." Record this as an access failure, not successful completion C. Select an accessible attachment later for a successful document-reading benchmark.

Do not submit an assignment or change any course state during the benchmark.

## First capture: normal browser cache

Use Chrome or Edge where you can open DevTools. Sign in to https://ufl.instructure.com/ before recording. If using a different browser from the initial inspection, record that fact and use it consistently for later comparisons.

1. Start on the loaded dashboard. Open DevTools → Network.
2. Enable Preserve log, leave Disable cache unchecked, select No throttling, and clear the request list.
3. Follow the selected workflow through completion B. Stop recording promptly after the content appears; export **HAR (sanitized)** to `benchmarks/private/warm-01.har`.
4. Optionally capture the attachment phase separately as `attachment-warm-01.har` so its viewer requests do not obscure the assignment-page costs. The currently locked preview is a failure-path capture only.

HAR contains network evidence, not a reliable time-to-usable-content metric. Pair later timed runs with a DevTools Performance recording with screenshots enabled. Identify each initiating click and the first frame meeting the specified completion condition. The screenshot sampling interval limits precision. Record any manual delay between navigation steps separately.

Keep raw captures local. Even sanitized exports can contain course content, signed URLs, and private response data. The private directory and HAR files are ignored by Git.

## Analysis

```powershell
python tools/analyze_har.py benchmarks/private/warm-01.har --output benchmarks/private/warm-01-summary.json
```

The analyzer uses only Python's standard library. It reports request counts, status counts, exporter-provided transfer size, request timings, validator presence, and repeated GET URLs. It emits no header values, request/response bodies, query strings, or original path identifiers. Unknown path segments are replaced with placeholders. This intentionally sacrifices some endpoint detail; inspect raw captures locally when needed.

For the eventual baseline, perform 5–10 runs in each condition: normal cache and browser HTTP cache disabled. Log browser version, machine, connection, recording settings, run order, and whether the PDF was previously opened. Report medians and ranges, not tail-percentile claims from a small sample. Cache-disabled does not mean a cold server, DNS cache, or connection.
