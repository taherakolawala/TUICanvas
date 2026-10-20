"""Browser baseline for the reader's workflow, using the saved session.

Opens the Canvas web pages in a headless Microsoft Edge (Playwright) with the
same saved session the reader uses, and times each step from navigation start
to the moment the content you would read is visible:

  dashboard        the selected course's link is visible
  assignment list  the selected assignment's link is visible
  assignment page  the start of its description is visible

Requests and transferred bytes are counted with the browser's own network
events, both up to that moment and until the page goes quiet. Runs alternate
between a normal (warm) browser cache and a disabled cache.

Writes benchmarks/private/browser-bench.json with timings and counts only:
no page content, URLs or cookies. Read-only: it only opens pages.

    .venv\\Scripts\\python.exe -m tools.browser_bench --runs 10
"""

import argparse
from datetime import datetime, timezone
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import statistics
import sys
import time

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/browser-bench.json"
VISIBLE_TIMEOUT_MS = 30_000
SETTLE_TIMEOUT_MS = 15_000


class _Text(HTMLParser):
    def __init__(self):
        super().__init__()
        self.parts = []
        self.skip = 0

    def handle_starttag(self, tag, attrs):
        if tag in {"script", "style"}:
            self.skip += 1

    def handle_endtag(self, tag):
        if tag in {"script", "style"} and self.skip:
            self.skip -= 1

    def handle_data(self, data):
        if not self.skip:
            self.parts.append(data)


def description_snippet(html, words=6):
    """The first few words of the rendered description: what a reader sees first."""
    parser = _Text()
    parser.feed(html or "")
    text = " ".join(" ".join(parser.parts).split())
    tokens = [t for t in text.split(" ") if t]
    # Prefer a run of ordinary words, which render the same everywhere.
    snippet = " ".join(tokens[:words])
    return snippet[:60].rsplit(" ", 1)[0] if len(snippet) > 60 else snippet


def find_targets(course_needle, assignment_needle):
    from tools.auth_session import NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first (run tuicanvas, or python -m tools.terminal_login).")
    client = NativeCanvas(saved["browser_state"])

    def get(path):
        result = client.get(path)
        if result.get("kind") != "json":
            raise SystemExit(f"Canvas read failed ({result.get('kind')} {result.get('status')}). Sign in again if the session expired.")
        return result["data"]

    courses = get("/api/v1/courses?enrollment_state=active&per_page=100")
    course = next((c for c in courses if course_needle.lower() in (c.get("name") or "").lower()), None)
    if not course:
        raise SystemExit("Course not found.")
    assignments = get(f"/api/v1/courses/{int(course['id'])}/assignments?per_page=100&order_by=due_at")
    assignment = next((a for a in assignments if assignment_needle.lower() in (a.get("name") or "").lower()), None)
    if not assignment:
        raise SystemExit("Assignment not found.")
    detail = get(f"/api/v1/courses/{int(course['id'])}/assignments/{int(assignment['id'])}")
    return saved["browser_state"], {
        "course_id": int(course["id"]),
        "assignment_id": int(assignment["id"]),
        "assignment_name": assignment.get("name") or "",
        "snippet": description_snippet(detail.get("description")),
    }


class Network:
    """Counts requests and transferred bytes from Chrome DevTools Protocol events."""

    def __init__(self, cdp):
        self.requests = 0
        self.bytes = 0
        self.cached = 0
        cdp.on("Network.requestWillBeSent", self._sent)
        cdp.on("Network.loadingFinished", self._finished)
        cdp.on("Network.requestServedFromCache", self._cached)

    def _sent(self, _event):
        self.requests += 1

    def _finished(self, event):
        self.bytes += int(event.get("encodedDataLength") or 0)

    def _cached(self, _event):
        self.cached += 1

    def snapshot(self):
        return self.requests, self.bytes, self.cached


# Completion checks run inside the page on every animation frame and return the
# page's own clock (ms since its navigation started) on the first frame where
# the content is present and visible. Text checks skip scripts, so data embedded
# in the page source does not count as visible content.
LINK_TO_COURSE = """(course) => {
    for (const a of document.querySelectorAll(`a[href$="/courses/${course}"], a[href*="/courses/${course}/"]`))
        if (a.getClientRects().length) return performance.now();
    return false;
}"""
LINK_NAMED = """(name) => {
    const want = name.replace(/\\s+/g, ' ').trim();
    for (const a of document.querySelectorAll('a'))
        if (a.textContent.replace(/\\s+/g, ' ').includes(want) && a.getClientRects().length) return performance.now();
    return false;
}"""
TEXT_SHOWN = """(snippet) => {
    const want = snippet.replace(/\\s+/g, ' ').trim();
    const root = document.body;
    if (!root) return false;
    const skip = /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|TEXTAREA)$/;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
        acceptNode: (n) => skip.test(n.parentNode && n.parentNode.nodeName) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT });
    let text = '';
    for (let n = walker.nextNode(); n; n = walker.nextNode()) text += n.nodeValue;
    if (!text.replace(/\\s+/g, ' ').includes(want)) return false;
    // Present in the document; confirm it is actually rendered (innerText honours CSS).
    return root.innerText.replace(/\\s+/g, ' ').includes(want) ? performance.now() : false;
}"""


def timed_step(page, network, url, check, arg):
    """Navigation start to content visible, then on to network quiet."""
    r0, b0, c0 = network.snapshot()
    started = time.perf_counter()
    page.goto(url, wait_until="commit", timeout=VISIBLE_TIMEOUT_MS)
    visible_ms = page.wait_for_function(check, arg=arg, polling="raf", timeout=VISIBLE_TIMEOUT_MS).json_value()
    wall_ms = (time.perf_counter() - started) * 1000
    r1, b1, _ = network.snapshot()
    try:
        page.wait_for_load_state("networkidle", timeout=SETTLE_TIMEOUT_MS)
    except Exception:
        pass
    r2, b2, c2 = network.snapshot()
    timing = page.evaluate("""() => {
        const n = performance.getEntriesByType('navigation')[0];
        return n ? {response_end: n.responseEnd, dom_content_loaded: n.domContentLoadedEventEnd, load: n.loadEventEnd} : {};
    }""")
    return {
        "visible_ms": round(visible_ms, 1),
        "wall_ms": round(wall_ms, 1),
        "html_response_end_ms": round(timing.get("response_end") or 0, 1),
        "dom_content_loaded_ms": round(timing.get("dom_content_loaded") or 0, 1),
        "load_event_ms": round(timing.get("load") or 0, 1),
        "requests_until_visible": r1 - r0,
        "bytes_until_visible": b1 - b0,
        "requests_total": r2 - r0,
        "bytes_total": b2 - b0,
        "served_from_cache": c2 - c0,
    }


def run_workflow(page, network, origin, targets):
    course, assignment = targets["course_id"], targets["assignment_id"]
    steps = []
    plan = [
        ("dashboard", f"{origin}/", LINK_TO_COURSE, course),
        ("assignment list", f"{origin}/courses/{course}/assignments", LINK_NAMED, targets["assignment_name"]),
        ("assignment page", f"{origin}/courses/{course}/assignments/{assignment}",
         TEXT_SHOWN, targets["snippet"] or targets["assignment_name"]),
    ]
    for name, url, check, arg in plan:
        try:
            result = timed_step(page, network, url, check, arg)
            result["step"] = name
        except Exception as exc:
            result = {"step": name, "error": type(exc).__name__}
        steps.append(result)
    return steps


def spread(values):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    return {"median": round(statistics.median(values), 1), "min": round(values[0], 1), "max": round(values[-1], 1)}


def summarize(runs):
    summary = {}
    for condition in ("warm cache", "cache disabled"):
        rows = [r for r in runs if r["condition"] == condition]
        if not rows:
            continue
        steps = {}
        for name in ("dashboard", "assignment list", "assignment page"):
            done = [s for r in rows for s in r["steps"] if s["step"] == name and "error" not in s]
            failed = sum(1 for r in rows for s in r["steps"] if s["step"] == name and "error" in s)
            steps[name] = {key: spread([s[key] for s in done]) for key in (
                "visible_ms", "wall_ms", "html_response_end_ms", "dom_content_loaded_ms", "load_event_ms",
                "requests_until_visible", "bytes_until_visible", "requests_total", "bytes_total")}
            steps[name]["failed_runs"] = failed
        summary[condition] = {"runs": len(rows), "steps": steps}
    return summary


def print_summary(summary):
    for condition, data in summary.items():
        print(f"\n{condition} ({data['runs']} runs) — median [min–max]")
        print(f"  {'step':<16} {'content visible ms':>24} {'requests (to visible / total)':>32} {'KB transferred (total)':>24}")
        for name, s in data["steps"].items():
            v = s["visible_ms"]
            if not v:
                print(f"  {name:<16} failed in all runs")
                continue
            print(f"  {name:<16} {v['median']:>10} [{v['min']}–{v['max']}]".ljust(44)
                  + f"{s['requests_until_visible']['median']:>12} / {s['requests_total']['median']:<10}"
                  + f"{s['bytes_total']['median'] / 1024:>14.0f}"
                  + (f"   ({s['failed_runs']} failed)" if s["failed_runs"] else ""))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--course", default="GPU")
    parser.add_argument("--assignment", default="Lab-2")
    parser.add_argument("--channel", default="msedge", help="msedge, chrome, or chromium (bundled)")
    parser.add_argument("--headed", action="store_true", help="Show the browser window")
    # Development only: run against a local test site with fixed targets.
    parser.add_argument("--origin", help=argparse.SUPPRESS)
    parser.add_argument("--targets", help=argparse.SUPPRESS)
    args = parser.parse_args()

    if args.targets:
        state, targets = None, json.loads(args.targets)
        origin = args.origin
    else:
        from tools.auth_session import ORIGIN
        state, targets = find_targets(args.course, args.assignment)
        origin = ORIGIN

    from playwright.sync_api import sync_playwright

    runs = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel=None if args.channel == "chromium" else args.channel,
                                             headless=not args.headed)
        context = browser.new_context(storage_state=state, viewport={"width": 1366, "height": 800})
        page = context.new_page()
        cdp = context.new_cdp_session(page)
        cdp.send("Network.enable")
        network = Network(cdp)
        version = browser.version
        try:
            # Unrecorded warm-up: opens connections and fills the HTTP cache.
            run_workflow(page, network, origin, targets)
            for index in range(1, args.runs + 1):
                for condition, disabled in (("warm cache", False), ("cache disabled", True)):
                    cdp.send("Network.setCacheDisabled", {"cacheDisabled": disabled})
                    steps = run_workflow(page, network, origin, targets)
                    runs.append({"condition": condition, "run": index, "steps": steps})
                    print(f"run {index} {condition}: " + ", ".join(
                        f"{s['step']} {s.get('visible_ms', s.get('error'))}" for s in steps), flush=True)
        finally:
            context.close()
            browser.close()

    summary = summarize(runs)
    report = {
        "version": 1,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "browser": f"{args.channel} {version}, {'headed' if args.headed else 'headless'}, 1366x800",
        "completion": {
            "dashboard": "selected course link visible",
            "assignment list": "selected assignment link visible",
            "assignment page": "first words of the description visible",
        },
        "summary": summary,
        "runs": runs,
    }
    if not args.targets:
        REPORT.parent.mkdir(parents=True, exist_ok=True)
        REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print_summary(summary)
    print(f"\nReport written to {REPORT.relative_to(REPORT.parents[2]) if not args.targets else '(not written in test mode)'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
