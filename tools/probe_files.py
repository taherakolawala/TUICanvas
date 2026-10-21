"""Read-only probe of Canvas Files behaviour, to design the local file library.

Reports counts, statuses and host names only (no file names, URLs or
contents): per-course folder/file listing sizes and pagination, whether the
Files area is hidden from students, and the redirect chain of one download
(fetching only its first byte). Output: benchmarks/private/files-probe.json
"""

from collections import Counter
from datetime import datetime, timezone
import json
from pathlib import Path
import time
import urllib.error
import urllib.parse
import urllib.request

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/files-probe.json"


class NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def main():
    from tools.auth_session import CANVAS, ORIGIN, NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first.")
    client = NativeCanvas(saved["browser_state"])

    def pages(path, limit=20):
        """Follow Link rel=next; returns (items, pages, status_or_kind, ms)."""
        items, count, started = [], 0, time.perf_counter()
        while path and count < limit:
            req = urllib.request.Request(ORIGIN + path, headers={"Accept": "application/json"})
            client.jar.add_cookie_header(req)
            try:
                with client.opener.open(req, timeout=30) as resp:
                    count += 1
                    raw = resp.read()
                    items.extend(json.loads(raw.removeprefix(b"while(1);")))
                    link = resp.headers.get("Link") or ""
                    nxt = [p.split(";")[0].strip().strip("<>") for p in link.split(",") if 'rel="next"' in p]
                    path = nxt[0].removeprefix(ORIGIN) if nxt else None
            except urllib.error.HTTPError as exc:
                return items, count, exc.code, (time.perf_counter() - started) * 1000
        return items, count, "ok", (time.perf_counter() - started) * 1000

    courses = client.get("/api/v1/courses?enrollment_state=active&per_page=100").get("data") or []
    courses = [c for c in courses if not c.get("access_restricted_by_date")]
    per_course, sample_file = [], None
    for c in courses:
        cid = int(c["id"])
        folders, fpages, fstatus, fms = pages(f"/api/v1/courses/{cid}/folders?per_page=100")
        files, xpages, xstatus, xms = pages(f"/api/v1/courses/{cid}/files?per_page=100")
        entry = {
            "folders": {"status": fstatus, "count": len(folders), "pages": fpages, "ms": round(fms),
                        "max_depth": max((f.get("full_name", "").count("/") for f in folders), default=0),
                        "hidden_or_locked": sum(1 for f in folders if f.get("hidden_for_user") or f.get("locked_for_user"))},
            "files": {"status": xstatus, "count": len(files), "pages": xpages, "ms": round(xms),
                      "total_mb": round(sum(f.get("size") or 0 for f in files) / 1048576, 1),
                      "locked_for_user": sum(1 for f in files if f.get("locked_for_user")),
                      "without_url": sum(1 for f in files if not f.get("url")),
                      "fields_present": sorted({k for f in files[:1] for k in f.keys()})},
        }
        per_course.append(entry)
        if sample_file is None:
            sample_file = next((f for f in files if f.get("url") and not f.get("locked_for_user")
                                and (f.get("size") or 0) < 5_000_000), None)

    chain = []
    if sample_file:
        url, use_cookies = sample_file["url"], True
        opener = urllib.request.build_opener(NoRedirects())
        for _ in range(4):
            parts = urllib.parse.urlsplit(url)
            req = urllib.request.Request(url, headers={"Range": "bytes=0-0"})
            if use_cookies and parts.hostname == CANVAS:
                client.jar.add_cookie_header(req)
            step = {"host": parts.hostname, "sent_cookies": bool(req.get_header("Cookie"))}
            try:
                with opener.open(req, timeout=30) as resp:
                    step.update(status=resp.status, content_range=bool(resp.headers.get("Content-Range")),
                                content_type=(resp.headers.get("Content-Type") or "").split(";")[0])
                    chain.append(step)
                    break
            except urllib.error.HTTPError as exc:
                step["status"] = exc.code
                location = exc.headers.get("Location")
                chain.append(step)
                if exc.code in (301, 302, 303, 307, 308) and location:
                    url = urllib.parse.urljoin(url, location)
                    use_cookies = urllib.parse.urlsplit(url).hostname == CANVAS
                    continue
                break

    report = {
        "version": 1,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "courses": len(courses),
        "listing_status": dict(Counter(f"folders {e['folders']['status']} / files {e['files']['status']}" for e in per_course)),
        "per_course": per_course,
        "download_redirect_chain": chain,
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in report.items() if k != "per_course"}, indent=2))
    for e in per_course:
        print(e["folders"]["status"], e["folders"]["count"], "folders /", e["files"]["count"], "files,",
              e["files"]["pages"], "pages,", e["files"]["ms"], "ms,", e["files"]["total_mb"], "MB, depth", e["folders"]["max_depth"])


if __name__ == "__main__":
    main()
