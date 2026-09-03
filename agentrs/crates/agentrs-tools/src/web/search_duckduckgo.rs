use std::rc::Rc;

use async_trait::async_trait;
use html5ever::tendril::TendrilSink;
use html5ever::{ParseOpts, parse_document};
use markup5ever_rcdom::{Node, NodeData, RcDom};
use reqwest::Client;
use url::Url;

use crate::web::search_backend::{DomainFilter, SearchBackend, SearchHit};
use crate::web::search_brave::describe_transport_error;

const DEFAULT_BASE_URL: &str = "https://html.duckduckgo.com/html/";

/// Sent because the endpoint serves a stripped page (or nothing) to clients it
/// does not recognise as a browser.
const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Keyless backend that scrapes DuckDuckGo's no-JavaScript HTML endpoint.
///
/// This exists so `WebSearch` can work without the user registering for an API
/// key. It is the least robust backend by construction:
///
/// - It parses a page meant for humans. DuckDuckGo can change that markup at
///   any time, and does so without notice.
/// - Automated access is not something DuckDuckGo's terms invite. Operators who
///   need a supported path should use `brave` or `tavily`, or self-host
///   `searxng`.
///
/// Because of the first point the parser fails loudly: markup it cannot read is
/// reported as an error rather than as "no results", which would otherwise look
/// to the model like the web genuinely had nothing to say.
pub struct DuckDuckGoBackend {
    client: Client,
    base_url: String,
}

impl DuckDuckGoBackend {
    pub fn new(client: Client, base_url: String) -> Self {
        let base_url = if base_url.trim().is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            base_url
        };
        Self { client, base_url }
    }
}

#[async_trait]
impl SearchBackend for DuckDuckGoBackend {
    fn name(&self) -> &str {
        "duckduckgo"
    }

    async fn search(&self, query: &str, _filter: &DomainFilter, limit: usize) -> Result<Vec<SearchHit>, String> {
        let response = self
            .client
            .get(&self.base_url)
            .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
            .header(reqwest::header::ACCEPT, "text/html")
            .query(&[("q", query)])
            .send()
            .await
            .map_err(describe_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("DuckDuckGo search failed with HTTP {}", status.as_u16()));
        }

        let body = response
            .text()
            .await
            .map_err(|_| "DuckDuckGo returned a body that could not be decoded".to_string())?;

        parse_results(&body, limit)
    }
}

/// Extract results from a DuckDuckGo HTML result page.
///
/// Returns `Ok(vec![])` only when the page says outright that it found nothing.
/// A page that yields no results without saying so is treated as a parse
/// failure — that is the shape both a markup change and a bot challenge take,
/// and silently reporting "no results" for either would be a lie.
fn parse_results(html: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(html);

    let mut hits = Vec::new();
    collect_hits(&dom.document, false, &mut hits);

    if hits.is_empty() {
        if declares_no_results(html) {
            return Ok(Vec::new());
        }
        return Err(
            "DuckDuckGo returned a page this build could not parse. Its markup may have changed, or the \
             request may have been rate-limited or challenged. Configure a supported backend \
             (`brave`, `tavily`, or a self-hosted `searxng`) under [web.search] for a stable path."
                .to_string(),
        );
    }

    hits.truncate(limit);
    Ok(hits)
}

/// Whether the page states it found nothing, as opposed to failing to parse.
fn declares_no_results(html: &str) -> bool {
    let lowered = html.to_lowercase();
    lowered.contains("no results found")
        || lowered.contains("no results for")
        || lowered.contains(r#"class="no-results"#)
}

/// Walk the document collecting result anchors and their snippets.
///
/// `in_ad` is carried down so an ad block's anchor is skipped along with
/// everything nested inside it.
fn collect_hits(node: &Rc<Node>, in_ad: bool, hits: &mut Vec<SearchHit>) {
    let mut in_ad = in_ad;

    if let NodeData::Element { name, attrs, .. } = &node.data {
        let classes = attribute(attrs.borrow().as_slice(), "class").unwrap_or_default();
        if has_class(&classes, "result--ad") || has_class(&classes, "results--ad") {
            in_ad = true;
        }

        if !in_ad {
            if name.local.as_ref() == "a" && has_class(&classes, "result__a") {
                if let Some(url) =
                    attribute(attrs.borrow().as_slice(), "href").and_then(|href| resolve_result_url(&href))
                {
                    let title = text_of(node).trim().to_string();
                    if !title.is_empty() {
                        hits.push(SearchHit {
                            title,
                            url,
                            snippet: None,
                        });
                    }
                }
            } else if has_class(&classes, "result__snippet") {
                // The snippet follows its anchor inside the same result block,
                // so it belongs to the most recent hit.
                if let Some(hit) = hits.last_mut()
                    && hit.snippet.is_none()
                {
                    let snippet = text_of(node).split_whitespace().collect::<Vec<_>>().join(" ");
                    if !snippet.is_empty() {
                        hit.snippet = Some(snippet);
                    }
                }
            }
        }
    }

    for child in node.children.borrow().iter() {
        collect_hits(child, in_ad, hits);
    }
}

fn attribute(attrs: &[html5ever::Attribute], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attr| attr.name.local.as_ref() == name)
        .map(|attr| attr.value.to_string())
}

/// Whitespace-delimited class match, so `result__a` does not match
/// `result__a-something`.
fn has_class(class_attr: &str, wanted: &str) -> bool {
    class_attr.split_whitespace().any(|class| class == wanted)
}

/// Concatenated text of a subtree. html5ever has already decoded entities and
/// dropped the `<b>` highlight tags' markup, leaving just their text.
fn text_of(node: &Rc<Node>) -> String {
    let mut out = String::new();
    push_text(node, &mut out);
    out
}

fn push_text(node: &Rc<Node>, out: &mut String) {
    if let NodeData::Text { contents } = &node.data {
        out.push_str(&contents.borrow());
    }
    for child in node.children.borrow().iter() {
        push_text(child, out);
    }
}

/// Turn a result href into the destination URL.
///
/// Results are wrapped in a DuckDuckGo redirector
/// (`//duckduckgo.com/l/?uddg=<encoded>`); handing that wrapper to the model
/// would make every citation point back at the search engine.
fn resolve_result_url(href: &str) -> Option<String> {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };
    let parsed = Url::parse(&absolute).ok()?;

    if let Some((_, target)) = parsed.query_pairs().find(|(key, _)| key == "uddg") {
        return Url::parse(&target).ok().map(|url| url.to_string());
    }

    match parsed.scheme() {
        "http" | "https" => Some(parsed.to_string()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "search_duckduckgo_test.rs"]
mod search_duckduckgo_test;
