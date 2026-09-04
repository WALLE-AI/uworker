use super::description;

#[test]
fn states_the_replacement_contract_up_front() {
    let text = description(false);
    assert!(
        text.contains("REPLACES the previous list"),
        "the model must be told this is not a patch"
    );
    assert!(text.contains("no partial updates"));
}

#[test]
fn single_mode_asks_for_at_most_one_active_task() {
    let text = description(false);
    assert!(text.contains("AT MOST ONE task in_progress"));
    assert!(
        !text.contains("several at once"),
        "the parallel clause must not leak into single mode"
    );
}

#[test]
fn parallel_mode_permits_several_active_tasks() {
    let text = description(true);
    assert!(text.contains("several at once"));
    assert!(
        !text.contains("AT MOST ONE task in_progress"),
        "the single clause must not leak into parallel mode"
    );
}

#[test]
fn both_modes_share_the_completion_bar() {
    for allow_parallel in [false, true] {
        let text = description(allow_parallel);
        assert!(
            text.contains("Never mark completed because you intend to finish it"),
            "completion bar missing (allow_parallel={allow_parallel})"
        );
        assert!(text.contains("3 or more distinct steps"));
        assert!(text.contains("When not to use it"));
    }
}

#[test]
fn stays_compact_enough_to_ship_on_every_request() {
    // Guards against the description drifting back toward Claude Code's
    // 184-line prompt, which is resident cost on every provider call.
    let text = description(false);
    assert!(
        text.lines().count() < 45,
        "description grew to {} lines",
        text.lines().count()
    );
}
