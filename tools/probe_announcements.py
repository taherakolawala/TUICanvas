"""Read-only probe of Canvas announcements, to design announcement support.

Reports counts, sizes, timings, field names and read-state values only (no
titles, messages, names or URLs): per-course announcement listings, the
cross-course /api/v1/announcements endpoint, one single-topic read, and the
GraphQL schema for discussions. Output: benchmarks/private/announcements-probe.json

    .venv\\Scripts\\python.exe -m tools.probe_announcements
"""

from collections import Counter
from datetime import datetime, timedelta, timezone
from html.parser import HTMLParser
from http.cookies import SimpleCookie
import json
from pathlib import Path
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/announcements-probe.json"


class Elements(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = self.images = self.file_links = self.frames = 0

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "a" and a.get("href"):
            self.links += 1
            self.file_links += "/files/" in a["href"]
        elif tag == "img" and a.get("src") and "/equation_images/" not in a["src"]:
            self.images += 1
        elif tag == "iframe":
            self.frames += 1


def main():
    from tools.auth_session import ORIGIN, NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first.")
    client = NativeCanvas(saved["browser_state"])

    def timed_pages(path, limit=10):
        """Follow Link rel=next; returns (items, pages, bytes, status, ms)."""
        items, pages, size, started = [], 0, 0, time.perf_counter()
        while path and pages < limit:
            req = urllib.request.Request(ORIGIN + path, headers={"Accept": "application/json"})
            client.jar.add_cookie_header(req)
            try:
                with client.opener.open(req, timeout=30) as resp:
                    raw = resp.read()
                    pages += 1
                    size += len(raw)
                    data = json.loads(raw.removeprefix(b"while(1);"))
                    items.extend(data if isinstance(data, list) else [data])
                    link = resp.headers.get("Link") or ""
                    nxt = [p.split(";")[0].strip().strip("<>") for p in link.split(",") if 'rel="next"' in p]
                    path = nxt[0].removeprefix(ORIGIN) if nxt else None
            except urllib.error.HTTPError as exc:
                return items, pages, size, exc.code, (time.perf_counter() - started) * 1000
        return items, pages, size, "ok", (time.perf_counter() - started) * 1000

    def graphql(query):
        req = urllib.request.Request(ORIGIN + "/api/graphql", data=json.dumps({"query": query}).encode(),
                                     headers={"Accept": "application/json", "Content-Type": "application/json"})
        client.jar.add_cookie_header(req)
        cookies = SimpleCookie()
        cookies.load(req.get_header("Cookie", ""))
        if "_csrf_token" in cookies:
            req.add_header("X-CSRF-Token", urllib.parse.unquote(cookies["_csrf_token"].value))
        try:
            with client.opener.open(req, timeout=30) as resp:
                return json.loads(resp.read())
        except urllib.error.HTTPError as exc:
            return {"http_error": exc.code}

    courses = client.get("/api/v1/courses?enrollment_state=active&per_page=100").get("data") or []
    courses = [c for c in courses if not c.get("access_restricted_by_date")]
    if "--parity" in sys.argv:
        return parity(courses, timed_pages, graphql)

    per_course, fields, read_states, sample = [], set(), Counter(), None
    attachment_fields = set()
    for c in courses:
        cid = int(c["id"])
        items, pages, size, status, ms = timed_pages(
            f"/api/v1/courses/{cid}/discussion_topics?only_announcements=true&per_page=100")
        messages = [i.get("message") or "" for i in items]
        e = Elements()
        for m in messages:
            e.feed(m)
        for i in items:
            fields.update(i.keys())
            read_states[str(i.get("read_state"))] += 1
            for att in i.get("attachments") or []:
                attachment_fields.update(att.keys())
        dates = sorted(i.get("posted_at") or "" for i in items)
        per_course.append({
            "status": status, "count": len(items), "pages": pages, "bytes": size, "ms": round(ms),
            "message_bytes_max": max((len(m) for m in messages), default=0),
            "message_bytes_total": sum(len(m) for m in messages),
            "with_attachments": sum(1 for i in items if i.get("attachments")),
            "attachments": sum(len(i.get("attachments") or []) for i in items),
            "allows_replies": sum(1 for i in items if not i.get("locked")),
            "with_replies": sum(1 for i in items if (i.get("discussion_subentry_count") or 0) > 0),
            "unread": sum(1 for i in items if i.get("read_state") == "unread"),
            "delayed": sum(1 for i in items if i.get("delayed_post_at")),
            "links": e.links, "file_links": e.file_links, "images": e.images, "frames": e.frames,
            "oldest_days": (datetime.now(timezone.utc) - datetime.fromisoformat(dates[0].replace("Z", "+00:00"))).days
            if dates and dates[0] else None,
        })
        if sample is None and items:
            sample = (cid, items[0]["id"])

    # Cross-course endpoint: default window (14 days) and a whole term.
    codes = "&".join(f"context_codes[]=course_{int(c['id'])}" for c in courses)
    cross = {}
    for label, extra in (("default_window", ""),
                         ("last_120_days", "&start_date=" + (datetime.now(timezone.utc) - timedelta(days=120)).date().isoformat()
                          + "&end_date=" + (datetime.now(timezone.utc) + timedelta(days=1)).date().isoformat())):
        items, pages, size, status, ms = timed_pages(f"/api/v1/announcements?{codes}&per_page=100{extra}")
        cross[label] = {"status": status, "count": len(items), "pages": pages, "bytes": size, "ms": round(ms),
                        "fields": sorted({k for i in items for k in i.keys()}),
                        "read_state": dict(Counter(str(i.get("read_state")) for i in items))}

    single = None
    if sample:
        items, pages, size, status, ms = timed_pages(f"/api/v1/courses/{sample[0]}/discussion_topics/{int(sample[1])}")
        single = {"status": status, "bytes": size, "ms": round(ms), "fields": sorted({k for i in items for k in i.keys()})}

    schema = graphql("""query {
      course: __type(name: "Course") { fields { name args { name type { name kind ofType { name } } } } }
      discussion: __type(name: "Discussion") { fields { name } }
      filter: __type(name: "DiscussionFilter") { inputFields { name } }
    }""")
    gql = {}
    if "data" in schema:
        d = schema["data"]
        gql["course_discussion_fields"] = [
            {"name": f["name"], "args": [a["name"] for a in f["args"]]}
            for f in (d.get("course") or {}).get("fields") or [] if "iscussion" in f["name"]]
        gql["discussion_fields"] = sorted(f["name"] for f in (d.get("discussion") or {}).get("fields") or [])
        gql["discussion_filter"] = [f["name"] for f in (d.get("filter") or {}).get("inputFields") or []]
    else:
        gql["error"] = schema

    # Ordering and cost of a short first page, REST and GraphQL, in the
    # three largest courses.
    def order(dates):
        dates = [d for d in dates if d]
        return ("newest_first" if dates == sorted(dates, reverse=True)
                else "oldest_first" if dates == sorted(dates) else "mixed")

    participant = graphql('query { p: __type(name: "DiscussionParticipant") { fields { name } } '
                          'c: __type(name: "DiscussionEntryCounts") { fields { name } } }')
    gql["participant_fields"] = sorted(f["name"] for f in (((participant.get("data") or {}).get("p") or {}).get("fields") or []))
    gql["entry_count_fields"] = sorted(f["name"] for f in (((participant.get("data") or {}).get("c") or {}).get("fields") or []))
    largest = sorted(range(len(courses)), key=lambda i: -per_course[i]["count"])[:3]
    short_pages = []
    for i in largest:
        cid = int(courses[i]["id"])
        entry = {"count": per_course[i]["count"]}
        items, pages, size, status, ms = timed_pages(
            f"/api/v1/courses/{cid}/discussion_topics?only_announcements=true&per_page=25", limit=1)
        entry["rest_25"] = {"status": status, "bytes": size, "ms": round(ms), "n": len(items),
                            "posted_order": order([x.get("posted_at") for x in items]),
                            "future_posts": sum(1 for x in items if (x.get("delayed_post_at") or "") > datetime.now(timezone.utc).isoformat())}
        for direction in ("first", "last"):
            started = time.perf_counter()
            q = ('query { course(id: "%d") { discussionsConnection(%s: 25, filter: {isAnnouncement: true}) '
                 '{ pageInfo { hasNextPage hasPreviousPage } nodes { _id title postedAt delayedPostAt createdAt '
                 'author { shortName } participant { read } entryCounts { unreadCount repliesCount } } } } }') % (cid, direction)
            result = graphql(q)
            ms = (time.perf_counter() - started) * 1000
            nodes = ((((result.get("data") or {}).get("course") or {}).get("discussionsConnection") or {}).get("nodes")) or []
            entry[f"graphql_{direction}_25"] = {
                "ms": round(ms), "bytes": len(json.dumps(result)), "n": len(nodes),
                "errors": [e.get("message", "")[:120] for e in result.get("errors") or []] if "errors" in result else None,
                "posted_order": order([n.get("postedAt") for n in nodes]),
                "created_order": order([n.get("createdAt") for n in nodes]),
                "read_values": dict(Counter(str((n.get("participant") or {}).get("read")) for n in nodes)),
            }
        short_pages.append(entry)
    gql["short_pages"] = short_pages

    report = {
        "version": 1,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "courses": len(courses),
        "topic_fields": sorted(fields),
        "attachment_fields": sorted(attachment_fields),
        "read_state_values": dict(read_states),
        "per_course": per_course,
        "cross_course": cross,
        "single_topic": single,
        "graphql": gql,
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in report.items() if k not in ("per_course", "topic_fields", "cross_course", "single_topic")}, indent=2))
    for e in per_course:
        print(e)


NODE = ("_id title postedAt delayedPostAt createdAt author { shortName } participant { read } "
        "attachment { _id } entryCounts { unreadCount repliesCount }")


def parity(courses, timed_pages, graphql):
    """Does a field-selected GraphQL list, sorted by postedAt, show the same
    newest announcements as REST's own order? Also: one query for all courses."""
    def when(text):
        return datetime.fromisoformat(text.replace("Z", "+00:00")) if text else None

    rows, now = [], datetime.now(timezone.utc)
    for c in courses:
        cid = int(c["id"])
        items, _, rest_bytes, status, rest_ms = timed_pages(
            f"/api/v1/courses/{cid}/discussion_topics?only_announcements=true&per_page=25", limit=1)
        row = {"count_rest_page": len(items), "rest_ms": round(rest_ms), "rest_bytes": rest_bytes}
        rest_ids = [str(i["id"]) for i in items]
        rest_posted = {str(i["id"]): when(i.get("posted_at")) for i in items}
        for first in (50, 100):
            started = time.perf_counter()
            result = graphql('query { course(id: "%d") { discussionsConnection(first: %d, filter: {isAnnouncement: true}) '
                             '{ pageInfo { hasNextPage } nodes { %s } } } }' % (cid, first, NODE))
            ms = (time.perf_counter() - started) * 1000
            conn = ((result.get("data") or {}).get("course") or {}).get("discussionsConnection") or {}
            nodes = conn.get("nodes") or []
            key = lambda n: when(n.get("postedAt")) or when(n.get("delayedPostAt")) or when(n.get("createdAt"))
            ordered = sorted(nodes, key=lambda n: (key(n), int(n["_id"])), reverse=True)
            top = [n["_id"] for n in ordered][:len(items)]
            missing = [i for i in rest_ids if i not in {n["_id"] for n in nodes}]
            extra = [n for n in ordered[:len(items)] if n["_id"] not in rest_ids]
            row[f"gql_{first}"] = {
                "ms": round(ms), "bytes": len(json.dumps(result)), "nodes": len(nodes),
                "has_more": (conn.get("pageInfo") or {}).get("hasNextPage"),
                "posted_null": sum(1 for n in nodes if not n.get("postedAt")),
                "posted_future": sum(1 for n in nodes if n.get("postedAt") and when(n["postedAt"]) > now),
                "same_order": top == rest_ids,
                "same_order_by_time": [key(n) for n in ordered[:len(items)]] == [rest_posted[i] for i in rest_ids],
                "same_set": set(top) == set(rest_ids),
                "rest_missing_from_gql": len(missing),
                "missing_rest_rank": [rest_ids.index(i) for i in missing][:5],
                "extra_in_gql_top": [{"posted_null": not n.get("postedAt"), "delayed": bool(n.get("delayedPostAt")),
                                      "future": bool(n.get("postedAt")) and when(n["postedAt"]) > now} for n in extra][:5],
            }
        rows.append(row)
    aliased = " ".join('c%d: course(id: "%d") { discussionsConnection(first: 20, filter: {isAnnouncement: true}) { nodes { %s } } }'
                       % (i, int(c["id"]), NODE) for i, c in enumerate(courses))
    started = time.perf_counter()
    result = graphql("query { %s }" % aliased)
    all_courses = {"ms": round((time.perf_counter() - started) * 1000), "bytes": len(json.dumps(result)),
                   "errors": [e.get("message", "")[:160] for e in result.get("errors") or []],
                   "nodes": sum(len(((v or {}).get("discussionsConnection") or {}).get("nodes") or [])
                                for v in (result.get("data") or {}).values())}
    codes = "&".join(f"context_codes[]=course_{int(c['id'])}" for c in courses)
    start = (datetime.now(timezone.utc) - timedelta(days=30)).date().isoformat()
    items, pages, size, status, ms = timed_pages(f"/api/v1/announcements?{codes}&per_page=100&start_date={start}")
    rest_30 = {"status": status, "count": len(items), "pages": pages, "bytes": size, "ms": round(ms)}
    out = {"per_course": rows, "graphql_all_courses_20_each": all_courses, "rest_announcements_30_days": rest_30}
    print(json.dumps(out, indent=1))
    path = REPORT.with_name("announcements-parity.json")
    path.write_text(json.dumps(out, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
