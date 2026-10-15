"""Summarize a local HAR without emitting cookies, bodies, or URL parameters."""

import argparse
import json
from collections import Counter, defaultdict
from pathlib import Path
from statistics import median
from urllib.parse import urlsplit


SAFE_SEGMENTS = set("api v1 courses assignments assignment_groups users self files preview download dashboard planner items submissions enrollments modules pages folders announcements discussion_topics graphql".split())


def endpoint(url, canvas_host):
    parsed = urlsplit(url)
    if parsed.hostname != canvas_host:
        return "external/[path omitted]"
    parts = []
    for part in parsed.path.split("/"):
        if not part:
            continue
        if part in SAFE_SEGMENTS:
            parts.append(part)
        elif part.isdigit():
            parts.append(":id")
        else:
            parts.append(":other")
    return "canvas/" + "/".join(parts)


def known_number(value):
    return isinstance(value, (int, float)) and value >= 0


def summarize(har, canvas_host):
    entries = har["log"]["entries"]
    groups = defaultdict(list)
    exact_gets = Counter()
    statuses = Counter()
    transfer = []
    validators = 0
    cache_markers = 0
    for entry in entries:
        request, response = entry["request"], entry["response"]
        method = request.get("method", "?")
        if method not in {"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"}:
            method = "OTHER"
        mime = response.get("content", {}).get("mimeType", "").lower()
        category = ("data" if "json" in mime else
                    "document" if "html" in mime else
                    "pdf" if "pdf" in mime else
                    "script" if "javascript" in mime else
                    "style" if "css" in mime else
                    "image" if mime.startswith("image/") else "other")
        groups[(method, endpoint(request["url"], canvas_host), category)].append(entry)
        if method == "GET":
            exact_gets[request["url"]] += 1
        status = response.get("status")
        statuses[str(status) if isinstance(status, int) else "unknown"] += 1
        size = response.get("_transferSize")
        if known_number(size):
            transfer.append(size)
        headers = {h["name"].lower() for h in response.get("headers", [])}
        validators += bool(headers & {"etag", "last-modified"})
        cache_markers += bool(entry.get("_fromCache") or response.get("_fromCache") or
                             response.get("_fromDiskCache") or response.get("_fromMemoryCache"))

    rows = []
    for (method, route, category), members in groups.items():
        durations = [e["time"] for e in members if known_number(e.get("time"))]
        waits = [e.get("timings", {}).get("wait") for e in members]
        waits = [v for v in waits if known_number(v)]
        rows.append({
            "method": method, "endpoint_template": route, "category_hint": category,
            "requests": len(members),
            "median_request_ms": round(median(durations), 2) if durations else None,
            "max_request_ms": round(max(durations), 2) if durations else None,
            "sum_request_ms": round(sum(durations), 2) if durations else None,
            "median_wait_ms": round(median(waits), 2) if waits else None,
        })
    rows.sort(key=lambda r: r["sum_request_ms"] or 0, reverse=True)
    return {
        "request_count": len(entries), "status_counts": dict(statuses),
        "reported_transfer_bytes": sum(transfer),
        "requests_with_known_transfer_size": len(transfer),
        "responses_with_validator_header": validators,
        "explicit_cache_markers": cache_markers,
        "repeated_exact_gets_after_first": sum(n - 1 for n in exact_gets.values()),
        "notes": [
            "Request durations overlap; their sum is not page or workflow latency.",
            "HAR alone does not measure usable-content or rendering completion.",
            "Category hints use MIME types, not evidence of necessity.",
            "Repeated GET URLs may be intentional refreshes or separate page visits.",
            "Endpoint templates collapse distinct IDs and omit query parameters.",
            "Cache markers and transfer size are exporter-dependent; missing is unknown.",
            "A validator header does not prove conditional requests are supported.",
        ],
        "request_groups": rows,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("har", type=Path)
    parser.add_argument("--canvas-host", default="ufl.instructure.com")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = summarize(json.loads(args.har.read_text(encoding="utf-8-sig")), args.canvas_host)
    encoded = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded, encoding="utf-8")
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()
