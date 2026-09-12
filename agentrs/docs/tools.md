# Bundled Tools

The agent includes a core local tool suite and agent-level helpers. The LLM automatically selects and invokes them based on the task. MCP servers can add more tools at runtime.

| Tool | Function | Concurrent |
|------|----------|------------|
| **Read** | Read file contents (with line numbers) | Yes |
| **Write** | Write files (auto-creates directories) | No |
| **Edit** | Precise string replacement | No |
| **ExecCommand** | Execute shell commands | No |
| **Grep** | Regex search file contents (via ripgrep) | Yes |
| **Glob** | Find files by pattern matching | Yes |
| **ViewImage** | Load a local JPEG, PNG, GIF, or WebP image for model inspection | Yes |
| **TodoWrite** | Maintain the session task checklist (list mode) | No |
| **TaskCreate** / **TaskList** / **TaskGet** / **TaskUpdate** | Maintain a task graph with dependencies (graph mode) | Reads only |
| **WebFetch** | Fetch a URL, convert it to Markdown, and answer a prompt about it | Yes |
| **WebSearch** | Search the web through a configured provider | Yes |
| **Spawn** | Spawn sub-agents for parallel tasks | No |
| **TeamCreate** | Create an in-process team for persistent sub-agents | No |
| **TeamDelete** | Delete a team after its members shut down | No |
| **SendMessage** | Send or broadcast a message within the active team | No |
| **ToolSearch** | Load schemas for deferred tools | Yes |

---

## Read

Read file contents with line numbers, similar to `cat -n`.

- Supports `offset` and `limit` parameters for reading file slices
- Auto-detects binary files
- Output format: line-numbered text

## Write

Write content to a file atomically.

- Atomic write: writes to a temp file first, then renames
- Auto-creates parent directories

## Edit

Find and replace exact strings in a file.

- Matches `old_string` exactly and replaces with `new_string`
- Requires a unique match by default; errors on multiple matches
- Use `replace_all` to replace all occurrences

## ExecCommand

Execute a shell command and return the result.

- Default timeout: 120 seconds, max 600 seconds
- Returns exit code, stdout, and stderr

## Grep

Search file contents with regular expressions.

- Uses `rg` (ripgrep) when available, falls back to `grep -rn`
- Supports glob filtering and case-insensitive search
- Results limited to 250 lines

## Glob

Find files matching a glob pattern.

- Standard glob patterns (e.g., `**/*.rs`)
- Results sorted by modification time (newest first)
- Returns up to 100 files

## ViewImage

Load a supported local image and attach it to the next model turn.

- Accepts an absolute file path
- Supports JPEG, PNG, GIF, and WebP files up to 20 MB
- Validates that the file content matches its extension

## TodoWrite

Maintain a structured task checklist for the current session, so multi-step work stays tracked and its progress is visible to the user.

- Takes `todos`, the **complete** list. Each call replaces the previous list: there are no partial updates and no per-item edits.
- Each entry has `content` (imperative, e.g. `Run the test suite`), a `status` of `pending`, `in_progress` or `completed`, and an optional `activeForm` (present continuous, e.g. `Running the test suite`) shown while the task is in progress.
- Entries are rejected when `content` is blank or duplicated, and — unless the deployment allows parallel work — when more than one task is `in_progress`. A rejected call leaves the previous list untouched.
- Returns a count summary rather than the list itself, since the list is already in the call arguments.
- Categorized as an `Info` tool, so it stays available in plan mode and needs no approval.

The checklist is rebuilt automatically when a session is resumed or forked, by replaying the last successful `TodoWrite` call out of the conversation. A session forked at an earlier turn therefore gets the checklist that was live at that turn. When a full compaction has folded the history away, the snapshot stored in the session file is used instead.

A checklist whose entries are all `completed` is retired when the next user turn opens, so the finished list stays visible through the end of the turn that finished it. An unfinished list survives across turns.

If the tool goes unused for `reminder_turns` assistant turns, the engine injects a one-off reminder carrying the current list. The reminder rides along on that single request and is never written into the session history.

```toml
[todo]
enabled = true                      # register task tracking at all
mode = "list"                       # "list" (TodoWrite) or "graph" (Task* tools)
allow_parallel_in_progress = false  # allow several in_progress tasks at once
reminder_turns = 10                 # turns without a call before nudging; 0 disables
```

`allow_parallel_in_progress` drives both the validation and the wording of the tool description, so the instructions the model reads always match the rule it is judged against. Sub-agents spawned via **Spawn** get their own `TodoWrite` backed by their own store, so a sub-agent plans and tracks its own multi-step work without any way to observe or overwrite the checklist that spawned it. A fork override that narrows the child's tools can take `TodoWrite` away like any other tool. (In graph mode the child shares the workspace graph instead — see below.)

Changes to the checklist are published to the UI: the TUI shows a live panel above the composer and names the active task in its status line (using `activeForm` when present), and `/todos` prints the full list. Hosts on the JSON stream protocol receive a [`todo_updated`](json-stream-protocol.md#114-todo_updated) event carrying the whole list.

## TaskCreate / TaskList / TaskGet / TaskUpdate

The task **graph**, selected with `mode = "graph"` under `[todo]`. It replaces `TodoWrite` rather than supplementing it — the two modes are alternatives, and only one family is ever registered.

Reach for it when steps genuinely depend on each other. It costs four tool descriptions instead of one, so work that merely runs in order is better served by the flat checklist.

- **TaskCreate** adds tasks and returns the ids used by every later call. Each has a `subject` (imperative), optional `description`, `activeForm`, `owner`, and `blockedBy`. A task may name a sibling created earlier in the same call.
- **TaskList** returns the graph, optionally filtered by `status` or `owner`.
- **TaskGet** returns one task including both sides of its dependencies.
- **TaskUpdate** changes one task, or removes it with `delete: true`. Dependencies are edited with `addBlockedBy` / `removeBlockedBy`.

The store enforces what a schema cannot:

- Starting or completing a task is refused while an unfinished task still blocks it. The error names the blockers and points at `removeBlockedBy`. A blocked task may always be left `pending`.
- Dependency edges are mirrored: recording that A blocks B updates both sides, and deleting a task strips every edge naming it, so no task is left waiting on something that no longer exists.
- A dependency that would make a task wait on itself is refused, and the error prints the loop it would have closed. A diamond — two branches converging — is not a loop and is allowed.
- Status is judged against the graph the call *leaves behind*, so dropping a dependency and starting the task in one call works.
- Ids are never reused after a delete, since a recycled id would silently re-point anything still naming it.

Tasks live in `.agentrs/tasks/tasks.json` under the workspace, and every write is a read-modify-write under an exclusive lock on that file. They are durable across sessions and are **not** rebuilt from the conversation or mirrored into the session file — `TaskList` is how the model re-reads its own state. Unlike the flat checklist, a completed graph is never retired automatically: tasks are addressable by id, so dropping one behind the model's back would strand every dependency naming it.

Sub-agents follow the configured mode too, but share rather than isolate: a graph-mode child is pointed at the same workspace graph as its parent. That is what the file-backed, lockable store is for — tasks are addressable by id and carry an `owner`, so a child can claim work the parent planned instead of keeping a private copy the parent can never see. A fork override that denies the `Task*` tools leaves the child untracked.

Graph mode drives the same UI as list mode. Tasks reach the TUI panel and the `todo_updated` protocol event with their id prefixed to the subject (`#2 Build it`), so a "blocked by 1" message can be followed to the task it names.

## WebFetch

Fetch a URL, reduce the page to Markdown, and answer a prompt about its contents.

- Requires `url` and `prompt`; `http` is upgraded to `https` for public hosts
- HTML is converted to Markdown; other content types are passed through unchanged
- Cross-host redirects are **not** followed — the tool reports the target so the
  model can re-issue the call, which re-runs the host policy on the new URL
- Binary payloads (PDF, images, archives) are saved under `.agentrs/webfetch/`
  and noted in the result
- Responses are cached per URL for 15 minutes by default
- When a summarization model is available the page is reduced through it;
  otherwise the truncated page text is returned fenced in
  `<untrusted-page-content source="...">`, because nothing reviewed it
- Pages outside `preapproved_domains` are summarized under a quoting limit;
  preapproved documentation is summarized without one so code samples survive

**Refused destinations.** Loopback, link-local (including the cloud metadata
address `169.254.169.254`), RFC1918, and unique-local addresses are rejected,
as are `localhost`, `*.local`, and `*.internal`. Hostnames are re-checked after
DNS resolution, so a public name whose record points inward is refused too. Set
`web.allow_private_network = true` to permit them.

**Preapproved hosts.** `web.preapproved_domains` entries skip the summarizer, so
their matching is deliberately stricter than the deny/allow rules:

| Rule | Matches |
|------|---------|
| `docs.rs` | that host exactly — **not** `a.docs.rs` |
| `*.rust-lang.org` | the host and its subdomains |
| `github.com/anthropics` | that path and everything under it, on segment boundaries |

A bare rule does not cover subdomains: one attacker-controlled subdomain of a
trusted site would otherwise inherit permission to put raw text into the
transcript.

**Permissions.** Beyond a bare `WebFetch` entry, the allow list accepts
`WebFetch:domain:example.com`, which approves that host and its subdomains only.

Registration is skipped entirely when `web.enabled = false`.

## WebSearch

Search the web through a configured provider and return titles and URLs.

- Takes `query` (2 characters or more), plus optionally `allowed_domains` or
  `blocked_domains` — the two are mutually exclusive
- Backends, set via `web.search.backend`:

  | Backend | Credential | Notes |
  |---------|-----------|-------|
  | `duckduckgo` | none | Works immediately. **Scrapes a human-facing HTML page** — the markup can change without notice, and automated access is not something DuckDuckGo's terms invite. Use it to try things out; prefer another backend for anything you depend on. When the page cannot be parsed the tool reports an error rather than "no results", so a broken scraper is never mistaken for an empty web. |
  | `brave` | API key via `api_key_env` | Supported API |
  | `tavily` | API key via `api_key_env` | Supported API; filters domains server-side |
  | `searxng` | none, but you set `base_url` | An instance you host |

- Providers that support domain filtering receive it directly; the rest are
  filtered locally
- The tool description states the current month and year, so date-sensitive
  queries are not anchored to the model's training cutoff
- The tool is **not registered** when no backend is configured, or when the
  configured backend's API key is missing — the model never sees a tool it
  cannot use

Search is provider-neutral by design: it calls a search API over HTTP rather
than relying on any LLM vendor's server-side search tool, so it behaves the same
across Anthropic, OpenAI, Bedrock, and Vertex.

## Spawn

See [Sub-Agent Spawning](advanced.md#sub-agent-spawning) in the Advanced Features guide.

Set `persistent: true` on a Spawn task after calling **TeamCreate** to make the
named child an addressable teammate. Persistent tasks use shared isolation and
return immediately while the member processes its initial prompt.

## TeamCreate / TeamDelete / SendMessage

These tools implement opt-in, in-process agent teams:

- **TeamCreate** creates one active team and its `team-lead@<team>` identity.
  Conflicting on-disk names receive a numeric suffix.
- **SendMessage** sends to a member name or broadcasts with `to: "*"`. The
  sender is bound by the runtime and cannot be supplied by the model. Messages
  are injected at a conversation boundary as `<teammate-message>` blocks.
- **TeamDelete** is idempotent, but refuses to remove a team while a non-lead
  member is active. Send `type: "shutdown_request"`; the teammate can answer
  with `type: "shutdown_response"`, the same `request_id`, and `approve: true`.

Team state is mirrored under `<session.directory>/teams/<team>/`, including
per-recipient inbox JSON for inspection. Runtime delivery remains in memory,
so a full process restart does not recreate teammate tasks automatically.

## ToolSearch

Load full schemas for deferred tools so the LLM can invoke them. Deferred bundled or MCP tools are registered without their full parameter schemas until the LLM calls ToolSearch.

- Search by tool name or a keyword from its description
- Returns the full schemas of all matching deferred tools

Skills are exposed through the **Skill** tool. When plan mode is enabled, **EnterPlanMode** and **ExitPlanMode** are also registered. See [Skills](skills.md) and [Plan Mode](advanced.md#plan-mode) for details.

---

## How It Works

```
User input → Build request (system prompt + history + tool definitions)
           → Stream LLM API response
           → Output text to stdout in real-time
           → If LLM returns tool_use → confirm → execute → send result back
           → Loop until LLM stops calling tools
           → Output final reply → save session
```

- Concurrent-safe tools (Read, Grep, Glob, ViewImage, WebFetch, WebSearch) execute in parallel
- Non-concurrent tools (Write, Edit, ExecCommand, TeamCreate, TeamDelete, SendMessage) execute sequentially
- Tool output is auto-truncated to prevent context window overflow
- Network tools observe a per-turn cancellation token, so an interrupted turn does not wait out a request timeout
- Tool output can be compacted (see [Output Compaction](advanced.md#output-compaction))

## Tool Descriptions

Each built-in tool includes a detailed description and usage guidance that is injected into the system prompt. These descriptions help the LLM select the right tool and use it effectively — for example, preferring Grep over ExecCommand for content search, or using Edit instead of Write for modifications.
