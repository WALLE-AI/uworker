use serde_json::Value;

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_approve_always_allows() {
        let mut confirmer = ToolConfirmer::new(true, vec![]);
        assert_eq!(
            confirmer.check("ExecCommand", &Value::Null, "echo hello"),
            ConfirmResult::Approved
        );
        assert_eq!(
            confirmer.check("Read", &Value::Null, "/tmp/file"),
            ConfirmResult::Approved
        );
        assert_eq!(
            confirmer.check("Write", &Value::Null, "/tmp/out"),
            ConfirmResult::Approved
        );
    }

    #[test]
    fn test_allowlist_contains_tool() {
        let mut confirmer = ToolConfirmer::new(false, vec!["Read".into(), "Write".into()]);
        assert_eq!(
            confirmer.check("Read", &Value::Null, "/tmp/file"),
            ConfirmResult::Approved
        );
        assert_eq!(
            confirmer.check("Write", &Value::Null, "/tmp/out"),
            ConfirmResult::Approved
        );
    }

    #[test]
    fn test_allowlist_approves_even_when_auto_approve_is_false() {
        let mut confirmer = ToolConfirmer::new(false, vec!["Read".into()]);
        assert_eq!(
            confirmer.check("Read", &Value::Null, "/some/path"),
            ConfirmResult::Approved
        );
    }

    // Phase 6: add_to_allow_list() grants runtime approval
    #[test]
    fn test_add_to_allow_list_grants_approval() {
        let mut confirmer = ToolConfirmer::new(false, vec![]);
        // Before: tool not in list (would prompt — skip interactive check, just verify membership)
        confirmer.add_to_allow_list("Write");
        // After: auto-approved without interactive prompt
        assert_eq!(
            confirmer.check("Write", &Value::Null, "file.txt"),
            ConfirmResult::Approved
        );
    }

    // Phase 6: add_to_allow_list() is idempotent — adding twice has no bad effect
    #[test]
    fn test_add_to_allow_list_idempotent() {
        let mut confirmer = ToolConfirmer::new(false, vec![]);
        confirmer.add_to_allow_list("ExecCommand");
        confirmer.add_to_allow_list("ExecCommand"); // duplicate — HashSet, no panic
        assert_eq!(
            confirmer.check("ExecCommand", &Value::Null, "echo hi"),
            ConfirmResult::Approved
        );
    }

    // Phase 6: add_to_allow_list() does not affect unrelated tools
    #[test]
    fn test_add_to_allow_list_does_not_affect_other_tools() {
        let mut confirmer = ToolConfirmer::new(false, vec![]);
        confirmer.add_to_allow_list("Read");
        // Write is not in the list — check returns non-Approved for non-interactive
        // (we cannot test interactive input; verify Read is approved and Write is not in list)
        assert_eq!(
            confirmer.check("Read", &Value::Null, "file.txt"),
            ConfirmResult::Approved
        );
        // We can't test the Denied path without stdin, but we verify allow_list state:
        assert!(confirmer.allow_list.contains("Read"));
        assert!(!confirmer.allow_list.contains("Write"));
    }

    // --- Domain-scoped rules (TC-1.6-03 through TC-1.6-05) ---

    fn fetch(url: &str) -> Value {
        serde_json::json!({ "url": url, "prompt": "summarize" })
    }

    #[test]
    fn domain_rule_approves_the_named_host_and_its_subdomains() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch:domain:example.com".into()]);

        assert!(confirmer.is_allowed("WebFetch", &fetch("https://example.com/a")));
        assert!(confirmer.is_allowed("WebFetch", &fetch("https://docs.example.com/a")));
    }

    #[test]
    fn domain_rule_does_not_approve_another_host() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch:domain:example.com".into()]);

        assert!(!confirmer.is_allowed("WebFetch", &fetch("https://other.com/a")));
        assert!(
            !confirmer.is_allowed("WebFetch", &fetch("https://notexample.com/a")),
            "matching must anchor on a dot boundary"
        );
    }

    // The rule is matched against the parsed URL, not the rendered input, so a
    // prompt that merely mentions the domain cannot unlock a different host.
    #[test]
    fn domain_rule_ignores_the_domain_appearing_in_other_fields() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch:domain:example.com".into()]);
        let input = serde_json::json!({
            "url": "https://attacker.test/steal",
            "prompt": "compare this with example.com",
        });

        assert!(!confirmer.is_allowed("WebFetch", &input));
    }

    #[test]
    fn domain_rule_is_scoped_to_its_own_tool() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch:domain:example.com".into()]);

        assert!(!confirmer.is_allowed("WebSearch", &fetch("https://example.com/a")));
    }

    #[test]
    fn a_bare_tool_rule_still_approves_every_call() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch".into()]);

        assert!(confirmer.is_allowed("WebFetch", &fetch("https://anything.test/a")));
        assert!(
            confirmer.is_allowed("WebFetch", &Value::Null),
            "existing name-only rules must keep working unchanged"
        );
    }

    #[test]
    fn a_domain_rule_does_not_approve_a_call_without_a_url() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch:domain:example.com".into()]);

        assert!(!confirmer.is_allowed("WebFetch", &Value::Null));
        assert!(!confirmer.is_allowed("WebFetch", &fetch("not a url")));
    }

    #[test]
    fn domain_rules_are_case_insensitive_and_tolerate_a_leading_dot() {
        let confirmer = ToolConfirmer::new(false, vec!["WebFetch:domain:.Example.COM".into()]);

        assert!(confirmer.is_allowed("WebFetch", &fetch("https://API.example.com/a")));
    }
}
