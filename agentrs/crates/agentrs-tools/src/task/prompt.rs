//! Model-facing text for the task-graph tools.
//!
//! Each description is short and states only what that tool does, because
//! graph mode ships four tools and a shared preamble on every one would cost
//! four times over in every request.

pub(crate) const CREATE: &str = "\
Add tasks to the session's task list. Use this when work has 3 or more distinct steps, when the \
user hands you several things to do, or when new instructions arrive mid-task.

Give each task an imperative `subject` (\"Add the --verbose flag\"), and where it helps an \
`activeForm` in the present continuous (\"Adding the --verbose flag\") for display while it runs. \
Use `blockedBy` when a task genuinely cannot start until another finishes; it may name a task \
created earlier in the same call. Do not invent dependencies for work that merely happens to be \
sequential, because a blocked task cannot be started until its blockers are completed.

Tasks are returned with the ids you use for every later call. Skip the list entirely for a single \
straightforward task.";

pub(crate) const LIST: &str = "\
List the session's tasks with their state, owner and dependencies. Filter by `status` to find what \
is left, or by `owner` to see one agent's work. Call this when you need the ids, or to re-orient \
after a long stretch of other work.";

pub(crate) const GET: &str = "\
Read one task in full, including what it blocks and what blocks it. Use it before starting a task \
to check nothing is still in its way.";

pub(crate) const UPDATE: &str = "\
Change one task, or delete it with `delete: true`.

Mark a task `in_progress` before you start it and `completed` the moment it is genuinely done; \
never batch completions and never mark work complete because you intend to finish it. If you are \
blocked or only partly done, leave it `in_progress` and add a task describing the blocker. Failing \
tests, a partial implementation, or an unresolved error all mean not completed.

Starting or completing a task is refused while an unfinished task still blocks it: finish that \
work first, or drop the dependency with `removeBlockedBy` if it was never real. Adding a \
dependency that would make a task wait on itself is refused too. Delete tasks that turn out to be \
irrelevant rather than leaving them pending forever.";
