//! Assignment description HTML -> plain terminal lines.

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

/// Render description HTML to wrapped lines with link footnotes. Scripts and
/// styles are not rendered; attachment links stay visible as references.
pub fn render(html: &str, width: usize, origin: &str) -> Vec<String> {
    let width = width.clamp(20, 160);
    let html = absolutize(html, origin);
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
        assert!(joined.contains("https://ufl.instructure.com/courses/1/files/2/download"));
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
    fn empty_description() {
        assert!(render("", 40, "https://x.edu").is_empty());
    }
}
