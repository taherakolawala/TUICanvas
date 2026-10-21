"""Read-only survey of what assignments contain, to plan the full assignment view.

Counts only: submission types, what descriptions embed or link to, the file
types behind file links, and external tool hosts. No names, text, URLs or
file contents are written. Output: benchmarks/private/assignment-survey.json

    .venv\\Scripts\\python.exe -m tools.survey_assignments
"""

from collections import Counter
from datetime import datetime, timezone
from html.parser import HTMLParser
import json
from pathlib import Path
import re
from urllib.parse import urlsplit

REPORT = Path(__file__).resolve().parents[1] / "benchmarks/private/assignment-survey.json"
MAX_FILE_LOOKUPS = 80
FILE_LINK = re.compile(r"/files/(\d+)")


class Elements(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links, self.images, self.frames, self.media, self.tables, self.equations = [], [], [], 0, 0, 0

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "a" and a.get("href"):
            self.links.append(a["href"])
        elif tag == "img" and a.get("src"):
            if "/equation_images/" in a["src"] or "equation_image" in (a.get("class") or ""):
                self.equations += 1
            else:
                self.images.append(a["src"])
        elif tag == "iframe" and a.get("src"):
            self.frames.append(a["src"])
        elif tag in {"video", "audio"}:
            self.media += 1
        elif tag == "table":
            self.tables += 1


def classify_link(href, canvas_host):
    parts = urlsplit(href)
    host = (parts.hostname or canvas_host).lower()
    if parts.scheme in {"mailto", "tel"}:
        return "mailto/tel"
    if host != canvas_host and not host.endswith(".instructure.com"):
        return "external"
    path = parts.path
    if FILE_LINK.search(path):
        return "canvas file"
    for kind in ("pages", "assignments", "quizzes", "discussion_topics", "modules", "external_tools", "media_objects"):
        if f"/{kind}" in path:
            return f"canvas {kind}"
    return "canvas other"


def kind_of(content_type, name=""):
    ct = (content_type or "").lower()
    ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
    if "pdf" in ct or ext == "pdf":
        return "pdf"
    if ct.startswith("image/"):
        return "image"
    if "word" in ct or ext in {"doc", "docx"}:
        return "word"
    if "presentation" in ct or "powerpoint" in ct or ext in {"ppt", "pptx"}:
        return "powerpoint"
    if "sheet" in ct or "excel" in ct or ext in {"xls", "xlsx", "csv"}:
        return "spreadsheet"
    if "zip" in ct or ext in {"zip", "tar", "gz", "7z"}:
        return "archive"
    if ct.startswith("text/") or ext in {"txt", "md", "py", "c", "cpp", "cu", "h", "java", "rs", "js", "ipynb"}:
        return "text/code"
    if ct.startswith("video/") or ct.startswith("audio/"):
        return "video/audio"
    return "other"


def main():
    from tools.auth_session import CANVAS, NativeCanvas, load_session

    saved = load_session()
    if not saved:
        raise SystemExit("No saved session. Sign in first.")
    client = NativeCanvas(saved["browser_state"])

    def get(path):
        result = client.get(path)
        return result.get("data") if result.get("kind") == "json" else None

    courses = get("/api/v1/courses?enrollment_state=active&per_page=100") or []
    courses = [c for c in courses if not c.get("access_restricted_by_date")]
    submission_types, link_kinds, external_hosts, image_kinds, frame_hosts, tool_hosts = (Counter() for _ in range(6))
    per_assignment = Counter()
    file_ids = []
    assignments_total = 0
    for course in courses:
        listing = get(f"/api/v1/courses/{int(course['id'])}/assignments?per_page=100&include[]=submission") or []
        for a in listing:
            assignments_total += 1
            for t in a.get("submission_types") or []:
                submission_types[t] += 1
            if "external_tool" in (a.get("submission_types") or []):
                url = (a.get("external_tool_tag_attributes") or {}).get("url") or ""
                tool_hosts[(urlsplit(url).hostname or "unknown").lower()] += 1
            if a.get("locked_for_user"):
                per_assignment["locked for you"] += 1
            sub = a.get("submission") or {}
            if sub.get("submitted_at"):
                per_assignment["already submitted"] += 1
            if a.get("rubric"):
                per_assignment["has rubric"] += 1
            html = a.get("description") or ""
            if not html.strip():
                per_assignment["no description"] += 1
                continue
            e = Elements()
            e.feed(html)
            kinds = Counter(classify_link(h, CANVAS) for h in e.links)
            link_kinds.update(kinds)
            for h in e.links:
                if classify_link(h, CANVAS) == "external":
                    external_hosts[(urlsplit(h).hostname or "").lower().removeprefix("www.")] += 1
                m = FILE_LINK.search(urlsplit(h).path)
                if m and m.group(1) not in file_ids:
                    file_ids.append(m.group(1))
            for src in e.images:
                image_kinds["canvas file image" if FILE_LINK.search(src) else "external image"] += 1
                m = FILE_LINK.search(urlsplit(src).path)
                if m and m.group(1) not in file_ids:
                    file_ids.append(m.group(1))
            for src in e.frames:
                frame_hosts[(urlsplit(src).hostname or CANVAS).lower().removeprefix("www.")] += 1
            flags = {"links anything": bool(e.links), "links a Canvas file": kinds["canvas file"] > 0,
                     "links outside Canvas": kinds["external"] > 0, "shows images": bool(e.images),
                     "embeds frames (video, docs)": bool(e.frames), "has video/audio": e.media > 0,
                     "has tables": e.tables > 0, "has equations": e.equations > 0}
            per_assignment.update(k for k, v in flags.items() if v)

    file_kinds, file_access, sizes = Counter(), Counter(), []
    for fid in file_ids[:MAX_FILE_LOOKUPS]:
        result = client.get(f"/api/v1/files/{int(fid)}")
        if result.get("kind") != "json":
            file_access[f"not readable ({result.get('kind')} {result.get('status')})"] += 1
            continue
        f = result["data"]
        file_kinds[kind_of(f.get("content-type"), f.get("display_name") or f.get("filename") or "")] += 1
        file_access["locked for you" if f.get("locked_for_user") else "available"] += 1
        if isinstance(f.get("size"), int):
            sizes.append(f["size"])

    sizes.sort()
    report = {
        "version": 1,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "courses": len(courses),
        "assignments": assignments_total,
        "submission_types": dict(submission_types.most_common()),
        "external_tool_hosts": dict(tool_hosts.most_common()),
        "assignments_that": dict(per_assignment.most_common()),
        "links_by_kind": dict(link_kinds.most_common()),
        "external_link_hosts": dict(external_hosts.most_common(15)),
        "images": dict(image_kinds.most_common()),
        "embedded_frame_hosts": dict(frame_hosts.most_common(10)),
        "linked_files": {
            "distinct": len(file_ids),
            "looked_up": min(len(file_ids), MAX_FILE_LOOKUPS),
            "by_type": dict(file_kinds.most_common()),
            "access": dict(file_access.most_common()),
            "size_kb": {"median": round(sizes[len(sizes) // 2] / 1024) if sizes else None,
                        "max": round(sizes[-1] / 1024) if sizes else None},
        },
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
