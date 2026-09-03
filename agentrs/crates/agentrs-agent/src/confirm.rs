use std::collections::HashSet;
use std::io::{self, BufRead, Write};

use serde_json::Value;

pub struct ToolConfirmer {
    auto_approve: bool,
    allow_list: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmResult {
    Approved,
    Denied,
    Quit,
}

impl ToolConfirmer {
    pub fn new(auto_approve: bool, allow_list: Vec<String>) -> Self {
        Self {
            auto_approve,
            allow_list: allow_list.into_iter().collect(),
        }
    }

    /// Returns whether auto-approve is enabled
    pub fn is_auto_approve(&self) -> bool {
        self.auto_approve
    }

    /// Add a tool name to the allow list at runtime.
    /// Used by skill context modifiers to grant auto-approval for specified tools.
    pub fn add_to_allow_list(&mut self, name: &str) {
        self.allow_list.insert(name.to_string());
    }

    /// Whether an allow-list entry covers this call.
    ///
    /// A bare tool name allows every call. Network tools additionally accept
    /// `WebFetch:domain:example.com`, so a user can grant one host without
    /// opening up arbitrary fetching.
    fn is_allowed(&self, tool_name: &str, input: &Value) -> bool {
        if self.allow_list.contains(tool_name) {
            return true;
        }
        let prefix = format!("{tool_name}:domain:");
        // The host is read from the structured input, not from the rendered
        // display string: matching a domain anywhere in the rendered JSON would
        // let an unrelated field — a prompt mentioning the domain — unlock a
        // fetch of somewhere else entirely.
        let Some(host) = target_host(input) else {
            return false;
        };
        self.allow_list.iter().any(|rule| {
            rule.strip_prefix(&prefix).is_some_and(|domain| {
                let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
                !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
            })
        })
    }

    /// Check if the tool needs confirmation. Returns the user's decision.
    pub fn check(&mut self, tool_name: &str, input: &Value, tool_input_display: &str) -> ConfirmResult {
        if self.auto_approve || self.is_allowed(tool_name, input) {
            return ConfirmResult::Approved;
        }

        eprint!(
            "\n[tool] {}({})\nAllow? [y]es / [n]o / [a]lways / [q]uit > ",
            tool_name, tool_input_display
        );
        io::stderr().flush().unwrap();

        let mut input = String::new();
        if io::stdin().lock().read_line(&mut input).is_err() {
            return ConfirmResult::Denied;
        }

        match input.trim().to_lowercase().as_str() {
            "y" | "yes" | "" => ConfirmResult::Approved,
            "a" | "always" => {
                self.allow_list.insert(tool_name.to_string());
                ConfirmResult::Approved
            }
            "q" | "quit" => ConfirmResult::Quit,
            _ => ConfirmResult::Denied,
        }
    }
}

/// Host targeted by a tool call, for domain-scoped allow-list rules.
fn target_host(input: &Value) -> Option<String> {
    let url = input.get("url").and_then(Value::as_str)?;
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(|host| host.to_ascii_lowercase())
}

#[cfg(test)]
#[path = "confirm_test.rs"]
mod confirm_test;
