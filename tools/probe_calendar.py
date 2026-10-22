"""Read-only probe of Canvas calendar and planner reads, to design the
home-screen calendar. Reports counts, sizes, timings, field names and item
types only (no titles or descriptions).
Output: benchmarks/private/calendar-probe.json

    .venv\\Scripts\\python.exe -m tools.probe_calendar
"""

from collections import Counter
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/calendar-probe.json"


def main():
    from tools.auth_session import ORIGIN, NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first.")
    client = NativeCanvas(saved["browser_state"])

    def pages(path, limit=10):
        items, count, size, started = [], 0, 0, time.perf_counter()
        while path and count < limit:
            req = urllib.request.Request(ORIGIN + path, headers={"Accept": "application/json"})
            client.jar.add_cookie_header(req)
            try:
                with client.opener.open(req, timeout=30) as resp:
                    raw = resp.read()
                    count += 1
                    size += len(raw)
                    data = json.loads(raw.removeprefix(b"while(1);"))
                    items.extend(data if isinstance(data, list) else [data])
                    link = resp.headers.get("Link") or ""
                    nxt = [p.split(";")[0].strip().strip("<>") for p in link.split(",") if 'rel="next"' in p]
                    path = nxt[0].removeprefix(ORIGIN) if nxt else None
            except urllib.error.HTTPError as exc:
                return items, count, size, exc.code, (time.perf_counter() - started) * 1000
        return items, count, size, "ok", (time.perf_counter() - started) * 1000

    courses = client.get("/api/v1/courses?enrollment_state=active&per_page=100").get("data") or []
    courses = [c for c in courses if not c.get("access_restricted_by_date")]
    now = datetime.now(timezone.utc)
    start, end = now.date().isoformat(), (now + timedelta(days=14)).date().isoformat()
    codes = "&".join(f"context_codes[]=course_{int(c['id'])}" for c in courses)
    me = client.get("/api/v1/users/self/profile").get("data") or {}
    user_code = f"&context_codes[]=user_{int(me['id'])}" if me.get("id") else ""

    def summary(items, status, n, size, ms, type_key=None):
        return {
            "status": status, "items": len(items), "pages": n, "bytes": size, "ms": round(ms),
            "fields": sorted({k for i in items for k in i.keys()}),
            "types": dict(Counter(str(i.get(type_key)) for i in items)) if type_key else None,
        }

    report = {"version": 1, "observed_at": now.isoformat(), "courses": len(courses), "window_days": 14}
    # Calendar events (the Canvas calendar): events and assignments are separate types.
    # Canvas caps context_codes per request (10 by default), so read in chunks.
    chunks = [courses[i:i + 9] for i in range(0, len(courses), 9)]
    for kind in ("event", "assignment"):
        total, size_all, ms_all, n_all, statuses = [], 0, 0.0, 0, []
        for i, chunk in enumerate(chunks):
            c = "&".join(f"context_codes[]=course_{int(x['id'])}" for x in chunk) + (user_code if i == 0 else "")
            items, n, size, status, ms = pages(
                f"/api/v1/calendar_events?type={kind}&start_date={start}&end_date={end}&per_page=100&{c}")
            total += items
            size_all += size
            ms_all += ms
            n_all += n
            statuses.append(status)
        report[f"calendar_{kind}s"] = summary(total, statuses, n_all, size_all, ms_all)
        report[f"calendar_{kind}s"]["requests"] = len(chunks)
    # Planner (the student "to do" view): one read across all courses.
    items, n, size, status, ms = pages(f"/api/v1/planner/items?start_date={start}&end_date={end}&per_page=100")
    report["planner"] = summary(items, status, n, size, ms, "plannable_type")
    report["planner"]["with_submission_state"] = sum(1 for i in items if isinstance(i.get("submissions"), dict))
    report["planner"]["context_types"] = dict(Counter(str(i.get("context_type")) for i in items))
    report["planner"]["plannable_fields"] = sorted({k for i in items for k in (i.get("plannable") or {}).keys()})
    # Upcoming events (what the dashboard sidebar shows).
    items, n, size, status, ms = pages("/api/v1/users/self/upcoming_events")
    report["upcoming_events"] = summary(items, status, n, size, ms, "type")
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
