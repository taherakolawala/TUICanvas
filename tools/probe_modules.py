"""Read-only probe of course tabs, modules and pages, to design modules
support and hide what instructors hid. Counts, sizes, timings, ids of tabs
and item types only (no titles or text).
Output: benchmarks/private/modules-probe.json

    .venv\\Scripts\\python.exe -m tools.probe_modules
"""

from collections import Counter
from datetime import datetime, timezone
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/modules-probe.json"


def main():
    from tools.auth_session import ORIGIN, NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first.")
    client = NativeCanvas(saved["browser_state"])

    def pages(path, limit=20):
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
    rows, item_types, item_fields, module_fields, states = [], Counter(), set(), set(), Counter()
    page_probe, folder_probe = None, Counter()
    for c in courses:
        cid = int(c["id"])
        tabs, _, tab_bytes, tab_status, tab_ms = pages(f"/api/v1/courses/{cid}/tabs")
        mods, n, size, status, ms = pages(f"/api/v1/courses/{cid}/modules?include[]=items&include[]=content_details&per_page=50")
        missing_items = 0
        for m in mods:
            module_fields.update(m.keys())
            states[str(m.get("state"))] += 1
            if "items" not in m or m.get("items") is None:
                missing_items += 1
            for it in m.get("items") or []:
                item_types[it.get("type")] += 1
                item_fields.update(it.keys())
                if page_probe is None and it.get("type") == "Page" and it.get("page_url"):
                    _, _, psize, pstatus, pms = pages(f"/api/v1/courses/{cid}/pages/{it['page_url']}")
                    page_probe = {"status": pstatus, "bytes": psize, "ms": round(pms)}
        files_hidden = not any(t.get("id") == "files" for t in tabs)
        if files_hidden:
            # Can a module file's details and folder be read when Files is hidden?
            for m in mods:
                for it in m.get("items") or []:
                    if it.get("type") == "File" and it.get("content_id") and sum(folder_probe.values()) < 6:
                        f = client.get(f"/api/v1/files/{int(it['content_id'])}")
                        if f.get("kind") != "json":
                            folder_probe[f"file {f.get('status')}"] += 1
                            continue
                        fid = (f.get("data") or {}).get("folder_id")
                        d = client.get(f"/api/v1/folders/{int(fid)}") if fid else {"kind": "none"}
                        folder_probe[f"file ok, folder {d.get('status') or d.get('kind')}"] += 1
        rows.append({
            "tabs_status": tab_status, "tabs": [t.get("id") for t in tabs], "tabs_hidden_flags": sum(1 for t in tabs if t.get("hidden")),
            "tabs_ms": round(tab_ms), "tabs_bytes": tab_bytes,
            "modules_status": status, "modules": len(mods), "pages": n, "bytes": size, "ms": round(ms),
            "items": sum(len(m.get("items") or []) for m in mods), "modules_without_inline_items": missing_items,
        })
    report = {
        "version": 1, "observed_at": datetime.now(timezone.utc).isoformat(), "courses": len(courses),
        "per_course": rows, "item_types": dict(item_types), "module_states": dict(states),
        "module_fields": sorted(module_fields), "item_fields": sorted(item_fields),
        "page_read": page_probe, "hidden_files_folder_access": dict(folder_probe),
        "tab_ids_seen": dict(Counter(t for r in rows for t in r["tabs"])),
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in report.items() if k != "per_course"}, indent=1))
    for r in rows:
        print(r)


if __name__ == "__main__":
    main()
