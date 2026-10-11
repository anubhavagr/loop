//! Minimal HTML-to-text conversion for [`super::web_fetch`].
//!
//! Dependency-free extraction of the readable text: script/style/head/svg
//! subtrees are dropped, block boundaries become newlines, list items are
//! prefixed with `- `, entities are decoded, and whitespace is collapsed.
//! It is intentionally not a layout engine — just enough structure for an LLM
//! to read a page.

const SKIP_ELEMENTS: &[&str] = &[
    "script", "style", "noscript", "template", "head", "svg", "title", "iframe", "canvas",
];

const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "dd",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "html",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
    "ul",
];

/// Convert an HTML document to readable plain text.
pub fn html_to_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len() / 2);
    let mut chars = input.char_indices().peekable();
    let mut skip_until: Option<String> = None; // closing tag of a dropped subtree
    let mut at_line_start = true;

    while let Some((i, c)) = chars.next() {
        if let Some(close) = &skip_until {
            // Inside a dropped subtree: only look for its closing tag.
            if c == '<' && closing_tag_matches(input, i, close) {
                skip_until = None;
                skip_tag(&mut chars);
            }
            continue;
        }
        match c {
            '<' if input[i..].starts_with("<!--") => {
                // Comment: drop up to `-->`.
                match input[i + 4..].find("-->") {
                    Some(end) => {
                        let resume = i + 4 + end + 3;
                        while chars.next().is_some_and(|(j, _)| j < resume - 1) {}
                    }
                    None => break,
                }
            }
            '<' => {
                let Some((name, is_close, consumed_to)) = read_tag(input, i) else {
                    continue;
                };
                let name = name.to_ascii_lowercase();
                // Advance the iterator past the tag.
                while chars.next().is_some_and(|(j, _)| j < consumed_to - 1) {}
                if SKIP_ELEMENTS.contains(&name.as_str()) {
                    if !is_close {
                        skip_until = Some(format!("/{name}"));
                    }
                    continue;
                }
                if name == "br" && !is_close {
                    // `<br><br>` is a paragraph gap; keep every break.
                    out.push('\n');
                    at_line_start = true;
                } else if name == "li" && !is_close {
                    if !at_line_start {
                        out.push('\n');
                    }
                    out.push_str("- ");
                    at_line_start = false;
                } else if BLOCK_ELEMENTS.contains(&name.as_str()) {
                    // Runs of block boundaries collapse to one blank line later.
                    out.push('\n');
                    at_line_start = true;
                }
            }
            '&' => {
                if let Some(decoded) = decode_entity(&input[i..]) {
                    out.push_str(&decoded);
                    let consumed = input[i..].find(';').expect("entity ends with ';'") + 1;
                    while chars.next().is_some_and(|(j, _)| j < i + consumed - 1) {}
                    at_line_start = false;
                } else {
                    out.push('&');
                }
            }
            _ => {
                out.push(c);
                if !c.is_whitespace() {
                    at_line_start = false;
                }
            }
        }
    }
    collapse_whitespace(&out)
}

/// Consume through the `>` of the current tag.
fn skip_tag(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    while chars.next().is_some_and(|(_, c)| c != '>') {}
}

/// Whether the tag starting at `i` is `</{close}>` (case-insensitive).
fn closing_tag_matches(input: &str, i: usize, close: &str) -> bool {
    let rest = &input[i..];
    let Some(tag_body) = rest.strip_prefix("</") else {
        return false;
    };
    let name_end = tag_body
        .find(|c: char| c == '>' || c.is_whitespace())
        .unwrap_or(tag_body.len());
    tag_body[..name_end].eq_ignore_ascii_case(close.trim_start_matches('/'))
}

/// Read the tag starting at `<` index `i`.
/// Returns `(name, is_closing, index_after_tag)`.
fn read_tag(input: &str, i: usize) -> Option<(String, bool, usize)> {
    let rest = &input[i + 1..];
    let is_close = rest.starts_with('/');
    let body = rest.trim_start_matches('/');
    let name_len = body
        .find(|c: char| c == '>' || c == '/' || c.is_whitespace())
        .unwrap_or(body.len());
    if name_len == 0 {
        return None;
    }
    let name: String = body[..name_len]
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if name.is_empty() {
        return None;
    }
    // Find the end of the tag, tolerating quoted attribute values containing `>`.
    let mut quote: Option<char> = None;
    let end = rest.char_indices().find_map(|(j, c)| match quote {
        Some(q) if c == q => {
            quote = None;
            None
        }
        Some(_) => None,
        None => match c {
            '"' | '\'' => {
                quote = Some(c);
                None
            }
            '>' => Some(i + 1 + j + 1),
            _ => None,
        },
    })?;
    Some((name, is_close, end))
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    let mut newlines = 0usize;
    for c in text.chars() {
        match c {
            '\n' => {
                pending_space = false;
                newlines += 1;
            }
            c if c == ' ' || c == '\t' || c == '\r' => {
                // Skip indentation after a newline and at the very start.
                if !out.is_empty() && !out.ends_with('\n') {
                    pending_space = true;
                }
            }
            _ => {
                if newlines > 0 {
                    out.push_str(if newlines >= 2 { "\n\n" } else { "\n" });
                    newlines = 0;
                } else if pending_space {
                    out.push(' ');
                }
                pending_space = false;
                out.push(c);
            }
        }
    }
    out.trim().to_string()
}

/// Decode one HTML entity at the start of `s` (which begins with `&`).
fn decode_entity(s: &str) -> Option<String> {
    let end = s.find(';')?;
    if end == 0 || end > 10 {
        return None;
    }
    let body = &s[1..end];
    if let Some(num) = body.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(String::from);
    }
    let decoded = match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" | "rsquo" => '\'',
        "ldquo" | "rdquo" => '"',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "laquo" => '«',
        "raquo" => '»',
        _ => return None,
    };
    Some(decoded.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_style_and_comments() {
        let html = r#"<html><head><title>T</title><style>.a{color:red}</style></head>
            <body><!-- note --><script>if (1 < 2) alert("x")</script>
            <p>Hello</p><style>a > b {}</style></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Hello"), "got: {text:?}");
        assert!(!text.contains("alert"));
        assert!(!text.contains("color"));
        assert!(!text.contains("note"));
    }

    #[test]
    fn block_tags_break_lines() {
        let text = html_to_text("<h1>Title</h1><p>One</p><p>Two</p>");
        assert_eq!(text, "Title\n\nOne\n\nTwo");
    }

    #[test]
    fn lists_become_dashes() {
        let text = html_to_text("<ul><li>Alpha</li><li>Beta</li></ul>");
        assert_eq!(text, "- Alpha\n- Beta");
    }

    #[test]
    fn entities_decode() {
        let text = html_to_text("<p>a &amp; b &lt;c&gt; &quot;d&quot; &#65;&#x42;</p>");
        assert_eq!(text, "a & b <c> \"d\" AB");
    }

    #[test]
    fn whitespace_collapses() {
        let text = html_to_text("<p>  spaced   out\ttext  </p><div>\n\n more </div>");
        assert_eq!(text, "spaced out text\n\nmore");
    }

    #[test]
    fn line_breaks_and_inline_tags() {
        let text = html_to_text("<p>a<br>b <strong>bold</strong> c</p>");
        assert_eq!(text, "a\nb bold c");
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(html_to_text("just text & nothing"), "just text & nothing");
    }
}
