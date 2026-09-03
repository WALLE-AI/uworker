use super::to_markdown;

const HTML: &str = "text/html; charset=utf-8";

// --- TC-1.3-01 through TC-1.3-04: structural elements survive conversion ---

#[test]
fn converts_headings_and_paragraphs() {
    let markdown = to_markdown(HTML, "<h1>Title</h1><p>body text</p>");
    assert!(markdown.contains("# Title"), "got: {markdown}");
    assert!(markdown.contains("body text"), "got: {markdown}");
}

#[test]
fn converts_links_to_markdown_syntax() {
    let markdown = to_markdown(HTML, r#"<a href="/x">label</a>"#);
    assert!(markdown.contains("[label](/x)"), "got: {markdown}");
}

#[test]
fn converts_unordered_lists() {
    let markdown = to_markdown(HTML, "<ul><li>a</li><li>b</li></ul>");
    assert!(markdown.contains("a"), "got: {markdown}");
    assert!(markdown.contains("b"), "got: {markdown}");
    assert!(
        markdown.lines().any(|line| line.trim_start().starts_with(['-', '*'])),
        "list items should render as markdown bullets, got: {markdown}"
    );
}

#[test]
fn preserves_code_blocks() {
    let markdown = to_markdown(HTML, "<pre><code>fn main() {}</code></pre>");
    assert!(markdown.contains("fn main() {}"), "got: {markdown}");
    assert!(markdown.contains("```"), "code should stay fenced, got: {markdown}");
}

// --- TC-1.3-05: non-content elements are dropped ---

#[test]
fn drops_script_and_style_bodies() {
    let markdown = to_markdown(
        HTML,
        "<style>.a{color:red}</style><script>alert('x')</script><p>keep</p>",
    );
    assert!(markdown.contains("keep"));
    assert!(!markdown.contains("color:red"), "style body leaked: {markdown}");
    assert!(!markdown.contains("alert"), "script body leaked: {markdown}");
}

// --- TC-1.3-06 / TC-1.3-07: non-HTML passes through untouched ---

#[test]
fn plain_text_is_passed_through_verbatim() {
    let body = "# already markdown\n\n- a\n- b\n";
    assert_eq!(to_markdown("text/plain", body), body);
    assert_eq!(to_markdown("text/markdown", body), body);
}

#[test]
fn json_is_passed_through_so_it_stays_parseable() {
    let body = r#"{"a": 1, "b": ["<p>not html</p>"]}"#;
    assert_eq!(to_markdown("application/json", body), body);
}

#[test]
fn html_detection_is_case_insensitive() {
    let markdown = to_markdown("TEXT/HTML", "<h1>T</h1>");
    assert!(markdown.contains("# T"), "got: {markdown}");
}

// --- TC-1.3-08 through TC-1.3-10: hostile input ---

#[test]
fn malformed_html_does_not_panic() {
    let markdown = to_markdown(HTML, "<div><p>unclosed<span>text");
    assert!(markdown.contains("unclosed"), "got: {markdown}");
}

#[test]
fn multibyte_content_is_not_truncated_mid_character() {
    let body = "<p>中文内容 🎉 emoji</p>";
    let markdown = to_markdown(HTML, body);
    assert!(markdown.contains("中文内容"), "got: {markdown}");
    assert!(markdown.contains('🎉'), "got: {markdown}");
    assert!(markdown.is_char_boundary(markdown.len()));
}

// The converter walks the DOM recursively, so without a depth guard this
// aborts the whole process rather than returning an error.
#[test]
fn deeply_nested_markup_does_not_overflow_the_stack() {
    let depth = 5000;
    let body = format!("{}deep{}", "<div>".repeat(depth), "</div>".repeat(depth));
    let markdown = to_markdown(HTML, &body);
    assert!(
        markdown.contains("deep"),
        "the text should still reach the model, got {} chars",
        markdown.len()
    );
}

#[test]
fn moderately_nested_markup_is_still_converted() {
    let depth = 50;
    let body = format!("{}<h1>T</h1>{}", "<div>".repeat(depth), "</div>".repeat(depth));
    let markdown = to_markdown(HTML, &body);
    assert!(
        markdown.contains("# T"),
        "the guard must not trip on ordinary page depth, got: {markdown}"
    );
}

#[test]
fn void_elements_do_not_count_toward_nesting_depth() {
    // 2000 sibling <br> tags are flat; treating them as nested would wrongly
    // push a normal page past the guard.
    let body = format!("<div>{}<h1>T</h1></div>", "<br>".repeat(2000));
    let markdown = to_markdown(HTML, &body);
    assert!(markdown.contains("# T"), "got: {markdown}");
}

#[test]
fn empty_body_yields_empty_output() {
    assert_eq!(to_markdown(HTML, "").trim(), "");
    assert_eq!(to_markdown("text/plain", ""), "");
}
