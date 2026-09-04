//! Model-facing text for `TodoWrite`.
//!
//! Deliberately compact. agentrs targets providers whose context windows vary
//! by an order of magnitude, and this description is resident in every request,
//! so it keeps only the three rules that change model behaviour: when to reach
//! for the list, when not to, and what actually counts as done.

/// Shared opening: the replacement contract, stated before anything else
/// because a model that treats the call as a patch corrupts the list silently.
const HEAD: &str = "\
Create and maintain a structured task list for the current session. Send the ENTIRE list on \
every call: it REPLACES the previous list. There are no partial updates and no per-item edits.

## When to use it
- The work needs 3 or more distinct steps (distinct steps, not 3 tool calls for one step)
- The user gave several tasks, numbered or comma-separated, or asked for a todo list
- New instructions arrive: capture them as todos immediately
- You are starting a task: mark it in_progress before you begin the work
- You finished a task: mark it completed, and add any follow-ups you discovered

## When not to use it
- A single straightforward task, or fewer than 3 trivial steps
- Purely informational or conversational requests
- Anything where tracking adds no organizational value

## Status
- pending: not started
- in_progress: being worked on now
- completed: finished successfully
";

/// Active-status clause for `allow_parallel_in_progress = false`.
const ACTIVE_SINGLE: &str = "\
- Keep AT MOST ONE task in_progress at a time. While work remains, exactly one task should be \
in_progress. A call marking more than one is rejected.
";

/// Active-status clause for `allow_parallel_in_progress = true`.
const ACTIVE_PARALLEL: &str = "\
- Mark every task you are actively working on as in_progress: several at once when work genuinely \
runs in parallel, one for sequential work. While work remains, at least one task should be \
in_progress.
";

/// Shared closing: the completion bar, which is where models are most likely to
/// cheat, and the housekeeping rules.
const TAIL: &str = "\
- Update status in real time. Mark a task completed the moment it is done; never batch completions.
- Mark completed only when the work is genuinely finished, including any verification the task \
called for. Never mark completed because you intend to finish it.
- If you are blocked or only partly done, leave the task in_progress and add a follow-up task \
naming the blocker. Failing tests, a partial implementation, or an unresolved error all mean \
not completed.
- Drop tasks that are no longer relevant instead of leaving them pending forever.
- Keep entries specific and actionable, and preserve any command the user gave you verbatim.

Provide `content` in the imperative (\"Run the test suite\") and, where useful, `activeForm` in the \
present continuous (\"Running the test suite\") for display while the task is in progress.";

/// The full description for one activation.
///
/// The active-status clause is the only part that varies, because the parallel
/// policy is the only thing that changes what the model should do. Generating
/// it rather than shipping one static paragraph keeps the instructions and the
/// runtime validation from drifting apart.
pub(crate) fn description(allow_parallel_in_progress: bool) -> String {
    let active = if allow_parallel_in_progress {
        ACTIVE_PARALLEL
    } else {
        ACTIVE_SINGLE
    };
    format!("{HEAD}\n## Rules\n{active}{TAIL}")
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod prompt_test;
