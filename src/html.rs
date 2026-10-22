//! Assignment description HTML -> plain terminal lines.

use std::collections::HashMap;

/// Remove terminal control sequences and characters that can reorder or hide text.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // Drop CSI (ESC [ ... final) and OSC (ESC ] ... BEL/ST) sequences entirely.
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        for n in chars.by_ref() {
                            if ('@'..='~').contains(&n) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(n) = chars.next() {
                            if n == '\u{7}' || (n == '\u{1b}' && chars.peek() == Some(&'\\')) {
                                if n == '\u{1b}' {
                                    chars.next();
                                }
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            '\t' => out.push_str("    "),
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Make root-relative links absolute so footnotes show where they lead.
fn absolutize(html: &str, origin: &str) -> String {
    html.replace("href=\"/", &format!("href=\"{origin}/"))
        .replace("src=\"/", &format!("src=\"{origin}/"))
}

/// Render description HTML to wrapped lines. Links appear as `[text][n]`,
/// where `n` is the link's number in the assignment's files & links list
/// (`links`); images appear as `[image: alt][n]`. Footnotes are not kept,
/// since the list shows where each link leads. Scripts and styles are dropped.
pub fn render(html: &str, width: usize, origin: &str) -> Vec<String> {
    let width = width.clamp(20, 160);
    let listed = links(html, origin);
    let html = absolutize(&images_as_links(html), origin);
    let text = html2text::config::plain()
        .link_footnotes(true)
        .allow_width_overflow()
        .string_from_read(html.as_bytes(), width)
        .unwrap_or_else(|_| strip_tags(&html));
    let mut lines: Vec<String> = text
        .lines()
        .map(sanitize)
        .map(|l| l.trim_end().to_string())
        .collect();
    // Footnotes come last, as `[n]: url`. Map their numbers to the list's.
    let mut numbers: HashMap<String, Option<usize>> = HashMap::new();
    while let Some(last) = lines.last() {
        if last.is_empty() {
            lines.pop();
            continue;
        }
        let Some((n, url)) = footnote(last) else {
            break;
        };
        let target = classify(url, origin);
        let position = listed.iter().position(|l| match (&l.target, &target) {
            (Target::File { id: a }, Target::File { id: b }) => a == b,
            _ => l.url == url,
        });
        numbers.insert(n.to_string(), position.map(|i| i + 1));
        lines.pop();
    }
    for line in &mut lines {
        *line = renumber(line, &numbers);
    }
    // Collapse runs of blank lines.
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    lines
}

/// `[12]: https://...` -> ("12", "https://...").
fn footnote(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('[')?;
    let close = rest.find("]: ")?;
    let n = &rest[..close];
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| (n, &rest[close + 3..]))
}

/// Rewrite `[text][n]` with the list's number for footnote `n`; links that are
/// not listed (page anchors) become plain text.
fn renumber(line: &str, numbers: &HashMap<String, Option<usize>>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut open: Option<usize> = None;
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '[' {
            open = Some(out.len());
            out.push(c);
            rest = &rest[1..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("][")
            && let Some(o) = open
        {
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && after[digits..].starts_with(']') {
                match numbers.get(&after[..digits]) {
                    Some(Some(k)) => out.push_str(&format!("][{k}]")),
                    Some(None) => {
                        out.remove(o);
                    }
                    None => out.push_str(&rest[..digits + 3]),
                }
                rest = &after[digits + 1..];
                open = None;
                continue;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Turn `<img>` tags (except rendered equations) into links labelled
/// `image: alt`, so they are numbered like other links in the text.
fn images_as_links(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    let mut copied = 0;
    while let Some(pos) = lower[at..].find("<img") {
        let start = at + pos;
        let Some(close) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start + 1..start + close];
        at = start + close + 1;
        if !tag[3..].starts_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(src) = attribute(tag, "src") else {
            continue;
        };
        if src.contains("/equation_images/") {
            continue;
        }
        let alt = attribute(tag, "alt")
            .map(|a| inline_text(&a))
            .filter(|a| !a.is_empty());
        out.push_str(&html[copied..start]);
        out.push_str(&format!(
            "<a href=\"{}\">image: {}</a>",
            escape_attribute(&src),
            escape_attribute(alt.as_deref().unwrap_or("untitled"))
        ));
        copied = at;
    }
    out.push_str(&html[copied..]);
    out
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Where a link in a description leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Canvas file (attachment or embedded image).
    File {
        id: String,
    },
    /// Another assignment; opens in the reader.
    Assignment {
        course_id: String,
        id: String,
    },
    /// A course wiki page; opens in the reader.
    Page {
        course_id: String,
        url: String,
    },
    /// Other Canvas content (modules, quizzes, discussions, tools); opens in the browser.
    Canvas,
    /// Outside Canvas; opens in the browser.
    Web,
    Mail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub text: String,
    /// Absolute URL.
    pub url: String,
    pub target: Target,
    /// From an `<img>` rather than an `<a>`.
    pub image: bool,
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Value of `name="..."` (or single-quoted, or bare) in a tag's attribute text.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let at = from + pos;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value_start = tag.len() - after.len() + 1;
        let value = tag[value_start..].trim_start();
        let parsed = match value.chars().next()? {
            q @ ('"' | '\'') => value[1..].split(q).next()?.to_string(),
            _ => value
                .split(|c: char| c.is_ascii_whitespace() || c == '>')
                .next()?
                .to_string(),
        };
        return Some(decode_entities(&parsed));
    }
    None
}

fn inline_text(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn number_after<'a>(path: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &path[path.find(marker)? + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '~')
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// Classify an absolute URL relative to the Canvas origin.
pub fn classify(url: &str, origin: &str) -> Target {
    if url.starts_with("mailto:") {
        return Target::Mail;
    }
    let Some(path) = url
        .strip_prefix(origin)
        .filter(|p| p.is_empty() || p.starts_with('/'))
    else {
        return Target::Web;
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    if let Some(id) = number_after(path, "/files/") {
        return Target::File { id: id.to_string() };
    }
    if let Some(course) = number_after(path, "/courses/")
        && let Some(slug) = path
            .strip_prefix(&format!("/courses/{course}/pages/"))
            .map(|rest| rest.trim_end_matches('/'))
            .filter(|slug| crate::modules::valid_slug(slug))
    {
        return Target::Page {
            course_id: course.to_string(),
            url: slug.to_string(),
        };
    }
    if let (Some(course), Some(id)) = (
        number_after(path, "/courses/"),
        number_after(path, "/assignments/"),
    ) {
        return Target::Assignment {
            course_id: course.to_string(),
            id: id.to_string(),
        };
    }
    Target::Canvas
}

fn absolute(href: &str, origin: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with("https://") || href.starts_with("http://") || href.starts_with("mailto:") {
        Some(href.to_string())
    } else if href.starts_with('/') && !href.starts_with("//") {
        Some(format!("{origin}{href}"))
    } else {
        None // anchors, javascript:, relative paths
    }
}

/// Links and images in description HTML, in document order, one entry per distinct URL.
pub fn links(html: &str, origin: &str) -> Vec<Link> {
    let mut found: Vec<Link> = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(pos) = lower[at..].find('<') {
        let start = at + pos;
        let Some(close) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start + 1..start + close];
        let tag_lower = &lower[start + 1..start + close];
        at = start + close + 1;
        let (url, text, image) = if tag_lower.starts_with("a ") || tag_lower.starts_with("a\n") {
            let Some(url) = attribute(tag, "href").and_then(|h| absolute(&h, origin)) else {
                continue;
            };
            let end = lower[at..].find("</a").map(|e| at + e).unwrap_or(at);
            let text = inline_text(&html[at..end]);
            (url, text, false)
        } else if tag_lower.starts_with("img ") || tag_lower.starts_with("img\n") {
            let Some(url) = attribute(tag, "src").and_then(|h| absolute(&h, origin)) else {
                continue;
            };
            let text = attribute(tag, "alt")
                .map(|a| inline_text(&a))
                .unwrap_or_default();
            (url, text, true)
        } else {
            continue;
        };
        let target = classify(&url, origin);
        // Equation images are rendered maths, not files.
        if image && url.contains("/equation_images/") {
            continue;
        }
        let duplicate = found.iter().any(|l| match (&l.target, &target) {
            (Target::File { id: a }, Target::File { id: b }) => a == b,
            _ => l.url == url,
        });
        if !duplicate {
            found.push(Link {
                text: sanitize(&text),
                url,
                target,
                image,
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escape_sequences_and_bidi_controls() {
        let evil = "ok\u{1b}[31mred\u{1b}]0;title\u{7}\u{202e}x\u{0}y\u{9b}z";
        assert_eq!(sanitize(evil), "okredxyz");
    }

    #[test]
    fn renders_lists_links_and_skips_scripts() {
        let html = r#"<h2>Lab 2</h2><p>Read the <a href="/courses/1/files/2/download">Lab-2 slides</a>.</p>
            <ul><li>First</li><li>Second</li></ul><script>alert(1)</script><p>Done &amp; dusted</p>"#;
        let lines = render(html, 60, "https://ufl.instructure.com");
        let joined = lines.join("\n");
        assert!(joined.contains("Lab 2"));
        assert!(joined.contains("First") && joined.contains("Second"));
        assert!(joined.contains("[Lab-2 slides][1]"), "{joined}");
        assert!(
            !joined.contains("]: https://"),
            "footnotes are replaced by the list"
        );
        assert!(!joined.contains("alert"));
        assert!(joined.contains("Done & dusted"));
        assert!(
            lines
                .iter()
                .all(|l| unicode_width::UnicodeWidthStr::width(l.as_str()) <= 60
                    || l.contains("http"))
        );
    }

    #[test]
    fn course_pages_open_in_the_reader() {
        let origin = "https://ufl.instructure.com";
        assert_eq!(
            classify(
                &format!("{origin}/courses/5/pages/week-1-notes?module_item_id=3"),
                origin
            ),
            Target::Page {
                course_id: "5".into(),
                url: "week-1-notes".into()
            }
        );
        assert_eq!(
            classify(&format!("{origin}/courses/5/pages"), origin),
            Target::Canvas
        );
        assert_eq!(
            classify(&format!("{origin}/courses/5/pages/a/b"), origin),
            Target::Canvas
        );
        assert_eq!(
            classify("https://other.edu/courses/5/pages/x", origin),
            Target::Web
        );
    }

    #[test]
    fn finds_links_and_images_with_targets() {
        let origin = "https://ufl.instructure.com";
        let html = r##"<p>See the <a class="instructure_file_link" href="/courses/5/files/22/download?verifier=x&amp;wrap=1" data-api-returntype="File">Lab-2 <b>slides</b></a>,
            <a href='https://ufl.instructure.com/courses/5/assignments/77'>Lab 1</a>,
            <a href="/courses/5/modules/3">Module 3</a>, <a href="https://github.com/org/repo">repo</a>,
            <a href="mailto:ta@ufl.edu">email the TA</a>, <a href="#top">top</a>, <a href="javascript:x()">x</a>
            and again <a href="/courses/5/files/22">slides</a>.</p>
            <img src="/courses/5/files/33/preview" alt="Timing &amp; speedup"><img src="/equation_images/x%5E2" alt="x^2">"##;
        let found = links(html, origin);
        let summary: Vec<(&str, &Target, bool)> = found
            .iter()
            .map(|l| (l.text.as_str(), &l.target, l.image))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Lab-2 slides", &Target::File { id: "22".into() }, false),
                (
                    "Lab 1",
                    &Target::Assignment {
                        course_id: "5".into(),
                        id: "77".into()
                    },
                    false
                ),
                ("Module 3", &Target::Canvas, false),
                ("repo", &Target::Web, false),
                ("email the TA", &Target::Mail, false),
                ("Timing & speedup", &Target::File { id: "33".into() }, true),
            ]
        );
        assert_eq!(
            found[0].url,
            "https://ufl.instructure.com/courses/5/files/22/download?verifier=x&wrap=1"
        );
        assert_eq!(
            classify("https://evil.example/courses/5/files/9", origin),
            Target::Web
        );
        assert_eq!(
            classify("https://ufl.instructure.com.evil.example/files/9", origin),
            Target::Web
        );
    }

    #[test]
    fn text_numbers_match_the_list_and_images_are_numbered() {
        let origin = "https://ufl.instructure.com";
        let html = r##"<p><a href="#top">Top</a> Read <a href="https://github.com/a/b">the repo</a> then
            <a href="/courses/5/files/22/download?verifier=x&amp;wrap=1">the slides</a>.</p>
            <p><img src="/courses/5/files/33/preview" alt="Speedup chart"> and <img src="/equation_images/x%5E2" alt="x^2"></p>"##;
        let listed = links(html, origin);
        assert_eq!(listed.len(), 3);
        let text = render(html, 80, origin).join("\n");
        assert!(text.contains("Top Read [the repo][1]"), "{text}");
        assert!(text.contains("[the slides][2]"), "{text}");
        assert!(text.contains("[image: Speedup chart][3]"), "{text}");
        assert!(text.contains("x^2"), "equations keep their text: {text}");
        assert!(!text.contains("]: "), "{text}");
    }

    #[test]
    fn entities_decode() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#39;d&#x27; &bogus; &"),
            "a & b <c> 'd' &bogus; &"
        );
    }

    #[test]
    fn empty_description() {
        assert!(render("", 40, "https://x.edu").is_empty());
    }
}
