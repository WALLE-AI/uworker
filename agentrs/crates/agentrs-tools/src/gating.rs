//! Explanations for tools that are compiled in but not registered.
//!
//! Several tools only appear once the workspace configures them. When one is
//! missing, both discovery paths — a direct call and a `ToolSearch` lookup —
//! otherwise dead-end on a message that cannot distinguish a typo from a
//! feature that merely needs switching on. Both consult this module so they
//! give the same answer.

/// Names that are gated behind configuration rather than absent from the build.
const GATED_TOOLS: [&str; 3] = ["WebFetch", "WebSearch", "TodoWrite"];

/// Web-family members. Their hints depend on which siblings are registered,
/// so they are resolved as a group rather than one name at a time.
const WEB_TOOLS: [&str; 2] = ["WebFetch", "WebSearch"];

const TODO_DISABLED_HINT: &str = "TodoWrite is compiled in but disabled: set `enabled = true` under \
`[todo]` in the agentrs config file (`agentrs config path` prints its location).";

const WEB_DISABLED_HINT: &str = "The web tools are compiled in but disabled: set `enabled = true` under \
`[web]` in the agentrs config file (`agentrs config path` prints its location).";

const SEARCH_UNCONFIGURED_HINT: &str = "WebSearch is compiled in but was not registered because no search \
backend is configured. Under `[web.search]` set `backend`: \"duckduckgo\" needs no key and works \
immediately (it scrapes an HTML page, so treat it as best-effort); \"brave\" and \"tavily\" need a key in \
the environment variable named by `api_key_env`; \"searxng\" needs `base_url` for an instance you host. \
Run `agentrs config path` to find the file.";

/// Why `name` is missing, if it is a configuration-gated tool.
///
/// `is_registered` reports whether some other tool is present, which is what
/// separates "the whole feature is off" from "this one needs a backend".
/// Returns `None` for names that are simply unknown, so a genuine typo does not
/// collect misleading configuration advice.
pub fn missing_tool_hint(name: &str, is_registered: impl Fn(&str) -> bool) -> Option<&'static str> {
    if !GATED_TOOLS.contains(&name) {
        return None;
    }
    // TodoWrite has a single on/off switch, so reaching here is already the
    // whole answer; no sibling needs consulting.
    if name == "TodoWrite" {
        return Some(TODO_DISABLED_HINT);
    }
    if !WEB_TOOLS.iter().any(|tool| is_registered(tool)) {
        return Some(WEB_DISABLED_HINT);
    }
    match name {
        "WebSearch" => Some(SEARCH_UNCONFIGURED_HINT),
        // WebFetch is registered whenever the web feature is on, so reaching
        // here means the caller asked about a tool that is actually present.
        _ => None,
    }
}

#[cfg(test)]
#[path = "gating_test.rs"]
mod gating_test;
