use htmd::HtmlToMarkdown;

/// Convert a response body to the text handed to the model.
///
/// HTML is reduced to Markdown; everything else is passed through untouched so
/// JSON, plain text, and Markdown stay byte-for-byte parseable. Conversion
/// failures degrade to the raw body rather than failing the fetch — partial
/// text is more useful to the model than an error.
pub fn to_markdown(content_type: &str, body: &str) -> String {
    if !is_html(content_type) {
        return body.to_string();
    }
    if exceeds_nesting_limit(body) {
        // The converter walks the DOM recursively, so pathologically nested
        // markup overflows the stack and aborts the process — not something a
        // fetched page should ever be able to do. Bail out to the raw body.
        tracing::warn!(
            target: "agentrs_tools",
            limit = MAX_HTML_NESTING_DEPTH,
            "HTML nesting exceeds the safe conversion depth; returning the raw body"
        );
        return body.to_string();
    }
    match converter().convert(body) {
        Ok(markdown) => markdown,
        Err(error) => {
            tracing::warn!(
                target: "agentrs_tools",
                %error,
                "HTML to Markdown conversion failed; falling back to the raw body"
            );
            body.to_string()
        }
    }
}

/// Deepest element nesting the recursive converter is trusted with.
///
/// Real documents sit well under this; anything above it is either generated
/// or hostile.
const MAX_HTML_NESTING_DEPTH: usize = 256;

/// Elements that never open a nesting level.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

/// Cheap upper bound on element nesting depth.
///
/// Deliberately a byte scan rather than a parse: the whole point is to decide
/// whether it is safe to build a tree at all.
fn exceeds_nesting_limit(html: &str) -> bool {
    let bytes = html.as_bytes();
    let mut depth: usize = 0;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        let rest = &bytes[index + 1..];
        let closing = rest.first() == Some(&b'/');
        let name_start = index + 1 + usize::from(closing);
        let name_end = bytes[name_start..]
            .iter()
            .position(|byte| !byte.is_ascii_alphanumeric())
            .map_or(bytes.len(), |offset| name_start + offset);
        if name_end == name_start {
            // `<!--`, `<!DOCTYPE`, or a stray `<`.
            index += 1;
            continue;
        }
        let name = html[name_start..name_end].to_ascii_lowercase();
        let tag_end = bytes[name_end..]
            .iter()
            .position(|byte| *byte == b'>')
            .map_or(bytes.len(), |offset| name_end + offset);
        let self_closing = tag_end > 0 && bytes.get(tag_end - 1) == Some(&b'/');

        if closing {
            depth = depth.saturating_sub(1);
        } else if !self_closing && !VOID_ELEMENTS.contains(&name.as_str()) {
            depth += 1;
            if depth > MAX_HTML_NESTING_DEPTH {
                return true;
            }
        }
        index = tag_end + 1;
    }
    false
}

fn is_html(content_type: &str) -> bool {
    let lowered = content_type.to_ascii_lowercase();
    lowered.contains("text/html") || lowered.contains("application/xhtml+xml")
}

/// Script and style bodies carry no readable content and would otherwise be
/// emitted as text.
fn converter() -> HtmlToMarkdown {
    HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript"])
        .build()
}

#[cfg(test)]
#[path = "html_to_md_test.rs"]
mod html_to_md_test;
