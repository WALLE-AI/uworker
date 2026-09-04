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
| **TodoWrite** | Maintain the session task checklist | No |
| **WebFetch** | Fetch a URL, convert it to Markdown, and answer a prompt about it | Yes |
| **WebSearch** | Search the web through a configured provider | Yes |
| **Spawn** | Spawn sub-agents for parallel tasks | No |
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
enabled = true                      # register the tool at all
allow_parallel_in_progress = false  # allow several in_progress tasks at once
reminder_turns = 10                 # turns without a call before nudging; 0 disables
```

`allow_parallel_in_progress` drives both the validation and the wording of the tool description, so the instructions the model reads always match the rule it is judged against. Sub-agents spawned via **Spawn** get their own `TodoWrite` backed by their own store, so a sub-agent plans and tracks its own multi-step work without any way to observe or overwrite the checklist that spawned it. A fork override that narrows the child's tools can take `TodoWrite` away like any other tool.

Changes to the checklist are published to the UI: the TUI shows a live panel above the composer and names the active task in its status line (using `activeForm` when present), and `/todos` prints the full list. Hosts on the JSON stream protocol receive a [`todo_updated`](json-stream-protocol.md#114-todo_updated) event carrying the whole list.

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
- Non-concurrent tools (Write, Edit, ExecCommand) execute sequentially
- Tool output is auto-truncated to prevent context window overflow
- Network tools observe a per-turn cancellation token, so an interrupted turn does not wait out a request timeout
- Tool output can be compacted (see [Output Compaction](advanced.md#output-compaction))

## Tool Descriptions

Each built-in tool includes a detailed description and usage guidance that is injected into the system prompt. These descriptions help the LLM select the right tool and use it effectively — for example, preferring Grep over ExecCommand for content search, or using Edit instead of Write for modifications.
