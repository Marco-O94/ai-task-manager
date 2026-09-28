# Claude Code CLI as a subprocess: technical reference for a Rust "Vibe Kanban"-style app

Target: Claude Code 2.1.283. The docs fetched today describe features gated at v2.1.283 (for example the `plugin_errors[].path` field and auto mode as the interactive default), so they match the installed version.

## 0. What I could and couldn't verify

- **Bash was denied in this session**, so I could not run `claude --help`, `claude auth --help`, `claude setup-token --help` or `claude mcp --help`. Every flag below comes from the official docs, not from your local binary. Run `claude --help` locally to confirm the rows marked ⚠.
- **Only the Anthropic docs domains loaded.** github.com, raw.githubusercontent.com, crates.io and WebSearch were all denied. The stdin control protocol, Vibe Kanban's internals and the Rust crate list therefore come from my prior knowledge and are marked **UNVERIFIED**.
- ⚠ **Three rows in the fetched CLI reference disagree with how I remember `claude --help`.** The fetch went through a summarizing model, which may have garbled them:
  - `--replay-user-messages`: docs say "Replay user messages from session history… requires --resume/--continue". I remember "Re-emit user messages from stdin back on stdout for acknowledgment (only with `--input-format stream-json` and `--output-format stream-json`)".
  - `--setting-sources`: docs say "print effective settings and exit". I remember "comma-separated list of sources to load: `user,project,local`".
  - `-v`: docs list it as a short form of `--verbose`. I remember `-v` as `--version`, with no short form for `--verbose`.

---

## 1. Headless invocation

### 1.1 Flags

Source: https://code.claude.com/docs/en/cli-reference.md

| Flag | Semantics |
|---|---|
| `-p`, `--print` | Non-interactive (Agent SDK via CLI). Exit code 0 on success, non-zero on failure. Invalid flags go to stderr before the run starts. Failures inside the run (for example missing auth) are printed as the result on stdout. |
| `--output-format text\|json\|stream-json` | Print mode only. `stream-json` is NDJSON, one event per line. |
| `--input-format text\|stream-json` | Print mode only. `stream-json` lets you write NDJSON user messages to stdin while the process runs. Pair it with `--output-format stream-json`. Whether that pairing is enforced is UNVERIFIED. |
| `--verbose` | "Emit additional debugging output in stream-json and text formats". Every docs example pairs it with `stream-json`. Historically `-p --output-format stream-json` refused to run without it; still true on 2.1.283 is UNVERIFIED. |
| `--include-partial-messages` | Emits `stream_event` token deltas. Requires `--print` and `--output-format stream-json`. |
| `--forward-subagent-text` | Also emits subagent text and thinking blocks, tagged with `parent_tool_use_id`. Needs v2.1.211+. |
| `--include-hook-events` | Hook lifecycle events in the stream. |
| `--session-id <id>` | Set the ID of a new session (print mode). Lets your DB own the ID. Whether it must be a UUID is UNVERIFIED; I believe it must. |
| `--resume`, `-r <id\|name\|/abs/path.jsonl>` | Resume a session. Since v2.1.223 the lookup searches every project on the machine, not only the cwd. |
| `--continue`, `-c` | Most recent conversation in the cwd. With `-p` this includes `-p` and SDK sessions. |
| `--fork-session` | With `--resume`/`--continue`, creates a new session ID instead of appending. |
| `--permission-mode <m>` | `default` (UI label "Manual"; alias `manual`), `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions`. For `-p` the built-in start mode is `default`. |
| `--dangerously-skip-permissions` | Same as `--permission-mode bypassPermissions`. Refuses root/sudo on Unix outside a recognized sandbox. |
| `--allowedTools`, `--allowed-tools` | Auto-approve rules, e.g. `"Bash(git diff *)" "Read" "Edit"`. The space before `*` matters. Unanchored `*` or `mcp__*` is ignored with a warning. |
| `--disallowedTools`, `--disallowed-tools` | A bare name removes the tool from context (`"*"` removes everything, `"mcp__*"` all MCP tools). A scoped rule such as `Bash(rm *)` denies in every mode. |
| `--tools` | Restricts the available tool set. ⚠ The fetched table says it can't be combined with allowed/disallowed tools (UNVERIFIED). |
| `--max-turns N` | Print mode. Counts tool-use turns. Exits with an error result (`error_max_turns`). With stream-json input, a queued message starts a new turn with a fresh limit. |
| `--max-budget-usd X` | Print mode. Includes subagent spend. |
| `--model <alias\|id>` / `--fallback-model a,b` / `--effort low\|medium\|high\|xhigh\|max\|ultracode` | Model and effort for the session. |
| `--append-system-prompt[-file]` / `--system-prompt[-file]` | Append to, or replace, the default system prompt. |
| `--append-subagent-system-prompt[-file]` | Only with `-p`. |
| `--add-dir <paths…>` | Grants file access. Does not discover most `.claude/` config in those directories. |
| `--mcp-config <file\|json…>` | With `-p`, waits up to `MCP_TIMEOUT` (default 30 s) for pending servers. Invalid entries are skipped and listed in `system/init.mcp_server_errors`. |
| `--strict-mcp-config` | Exit with an error if an MCP server fails to connect (v2.1.221+). |
| `--permission-prompt-tool <mcp_tool>` | An MCP tool that answers permission prompts in `-p`. Claude Code waits for its server to connect. It can't approve tools marked `requiresUserInteraction`. |
| `--permission-prompts host\|none` | `host` (default) sends prompts to the SDK host or the permission-prompt tool. `none` denies them, emits `permission_denied` system messages and fills `result.permission_denials`. Needs v2.1.259+. |
| `--settings <file\|json>` | Highest-precedence settings source (below managed settings). |
| `--json-schema '<schema>'` | Validated `structured_output` in the result. |
| `--no-session-persistence` | Nothing is written to disk; the session can't be resumed. |
| `--bare` | Skips hooks, skills, plugins, MCP servers, CLAUDE.md and auto memory. **Never reads OAuth credentials, the Keychain or `CLAUDE_CODE_OAUTH_TOKEN`**, so it is incompatible with subscription login. See 1.3. |
| `--name`, `-n` | Display name for the session. |
| `--agents <json\|file>` / `--agent` | Define or select subagents. |
| `--debug[=filter]` / `--debug-file <path>` | Debug logging. |

### 1.2 Recommended long-lived invocation

This is the pattern the TS and Python SDKs use internally.

```bash
cd "$TASK_WORKTREE" && claude -p \
  --output-format stream-json --input-format stream-json --verbose \
  --include-partial-messages \
  --session-id "$UUID" \
  --permission-mode default \
  --permission-prompt-tool stdio \
  --allowedTools "Read" "Grep" "Glob" \
  --append-system-prompt "…task context…" \
  --max-budget-usd 5
```

`--permission-prompt-tool stdio` is **UNVERIFIED and not in the public docs**. It is the value the SDKs pass so that permission prompts come back over stdout as control requests. Its documented counterpart is `--permission-prompts host`.

Minimal documented-only alternative, one process per turn:

```bash
claude -p "<prompt>" --output-format stream-json --verbose --include-partial-messages \
  [--resume <sid>] --permission-mode acceptEdits --allowedTools "Bash(npm test *)"
```

### 1.3 cwd behavior and caveats

- There is no `--cwd` flag on the main command (only `claude agents --cwd`). Claude Code uses the process working directory, so set it with `std::process::Command::current_dir(worktree)`.
- Transcripts are written to `~/.claude/projects/<cwd with every non-alphanumeric character replaced by '-'>/<session-id>.jsonl`. Names longer than 200 characters are truncated and suffixed with a hash. `CLAUDE_CONFIG_DIR` relocates this, and `CLAUDE_CODE_PROJECT_DIR_NAME` (v2.1.234+, only with `CLAUDE_CONFIG_DIR`) pins the directory name. The JSONL format is internal and may change between versions; don't parse it.
- **Security:** without `--bare`, a `-p` session runs the project's `.claude/settings.json` hooks and connects servers from its `.mcp.json`, **even in an untrusted folder, with no trust dialog**. Keep this in mind when running agents in cloned repositories.
- **`--bare` cannot be used with a Claude.ai login.** Bare mode only accepts `ANTHROPIC_API_KEY`, `apiKeyHelper`, or Bedrock/Vertex/Foundry credentials. Since your app is built on "log in with your Claude account", don't use `--bare`. Instead, isolate context with `--settings`, `--strict-mcp-config` and explicit tool lists.
- Background Bash tasks are killed about 5 s after the final result once stdin has closed. Background subagents keep `-p` alive for up to 10 minutes of idle waiting (`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`).
- If the working directory is deleted mid-session, the session keeps running. Claude Code emits a warning message and shell commands fail until the directory exists again.
- A slow stdout reader delays exit by up to 30 s while the output drains (v2.1.214+). Piped stdin is capped at 10 MB.

Sources: https://code.claude.com/docs/en/headless.md · https://code.claude.com/docs/en/sessions.md

---

## 2. stream-json output schema

Each line is one JSON object with a top-level `type`. The docs give field names but not complete JSON lines, so the examples below are illustrative. Parse with `#[serde(tag = "type")]` plus a catch-all variant and ignore unknown fields; new fields and events are added regularly.

### 2.1 `system` / `init`

It is the first event unless `hook_started`, `hook_progress`, `hook_response` or `plugin_install` events precede it. Documented fields: session ID, model, tools, MCP servers, plugins, `plugin_errors`, `mcp_server_errors`, `capabilities`. TS type field names: `apiKeySource`, `cwd`, `tools`, `mcp_servers`, `model`, `permissionMode`, `slash_commands`, `output_style`, `capabilities`.

```json
{"type":"system","subtype":"init","session_id":"5b3f2c1a-8d4e-4f6b-9a7c-2e1d0f9b8a6c","cwd":"/Users/me/wt/task-42","model":"claude-sonnet-5","permissionMode":"default","tools":["Bash","Read","Edit","Write","Glob","Grep","WebFetch","Agent","AskUserQuestion"],"mcp_servers":[{"name":"app","status":"connected"}],"slash_commands":["compact","review"],"apiKeySource":"none","output_style":"default","capabilities":["interrupt_receipt_v1","interrupt_cancel_queued_v1"],"plugins":[],"uuid":"…"}
```

- The `apiKeySource` value `"none"` for OAuth is UNVERIFIED.
- `capabilities` (v2.1.205+) is the documented feature-detection mechanism. Check it instead of comparing version strings, and ignore values you don't recognize.

### 2.2 `assistant`

Each message carries **one content block**. Blocks from the same API response share `message.id`. The payload is the raw API message under `.message`.

```json
{"type":"assistant","message":{"id":"msg_01A","role":"assistant","model":"claude-sonnet-5","content":[{"type":"text","text":"I'll run the tests first."}]},"parent_tool_use_id":null,"session_id":"5b3f…","uuid":"…"}
{"type":"assistant","message":{"id":"msg_01A","role":"assistant","content":[{"type":"tool_use","id":"toolu_01X","name":"Bash","input":{"command":"npm test","description":"Run test suite"}}]},"parent_tool_use_id":null,"session_id":"5b3f…","uuid":"…"}
{"type":"assistant","message":{"id":"msg_01B","role":"assistant","content":[{"type":"thinking","thinking":"…","signature":"…"}]},"parent_tool_use_id":null,"session_id":"5b3f…","uuid":"…"}
```

Whether thinking text is included depends on the `ThinkingConfig.display` setting. `parent_tool_use_id` is non-null for subagent messages; the value is the ID of the Agent or Skill tool call that spawned the subagent.

### 2.3 `user`

These carry tool results, echoed streamed input, and a subagent's first prompt.

```json
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01X","content":"3 failing tests…","is_error":false}]},"parent_tool_use_id":null,"session_id":"5b3f…","uuid":"…"}
```

### 2.4 `stream_event` (only with `--include-partial-messages`)

Wraps raw Messages API SSE events: `message_start`, `content_block_start`, `content_block_delta` (`text_delta`, `input_json_delta`, `thinking_delta`), `content_block_stop`, `message_delta`, `message_stop`.

- Main session only. Subagent token deltas are not forwarded, and `parent_tool_use_id` is always null here.
- `user_message_uuid` is set on the turn's first non-ping event.
- Structured output does not stream.

```json
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I'll"}},"parent_tool_use_id":null,"session_id":"5b3f…","uuid":"…"}
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"np"}},"parent_tool_use_id":null,"session_id":"5b3f…","uuid":"…"}
```

Documented order: `message_start` → `content_block_start` → deltas → **`assistant` (complete block)** → `content_block_stop` → … → `message_delta` → `message_stop` → tool runs → next turn → `result`.

### 2.5 `result`

One per turn. With stream-json input, you get one for each user message you send.

```json
{"type":"result","subtype":"success","is_error":false,"duration_ms":45872,"duration_api_ms":31210,"num_turns":4,"result":"Fixed the auth bug, all three tests pass now.","stop_reason":"end_turn","session_id":"5b3f…","total_cost_usd":0.0312,"usage":{"input_tokens":1200,"cache_creation_input_tokens":8000,"cache_read_input_tokens":42000,"output_tokens":900},"modelUsage":{"claude-sonnet-5":{}},"permission_denials":[],"uuid":"…"}
{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":3,"session_id":"5b3f…","total_cost_usd":0.02,"usage":{},"stop_reason":"tool_use","permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_…","tool_input":{"command":"rm -rf dist"}}]}
```

- Subtypes:
  - `success`
  - `error_max_turns`
  - `error_max_budget_usd`
  - `error_during_execution` (for example a cancelled request, or a crash; after a crash, costs may be zeroed and `stop_reason` is null)
  - `error_max_structured_output_retries`
- `result` is present only on `success`. Every subtype carries `total_cost_usd`, `usage`, `num_turns` and `session_id`.
- `usage` covers the main loop only; `modelUsage` covers the whole tree.
- `stop_reason` is `end_turn`, `max_tokens`, `refusal` or null.
- `total_cost_usd` is a **client-side estimate**. For subscription users it is not what they are billed. With `--resume`, it reports the whole conversation's total.
- A few trailing events, such as `prompt_suggestion`, can arrive after `result`, so read until EOF rather than stopping at the result.

### 2.6 Other documented system events

- **`system/api_retry`**: fields `attempt`, `max_retries`, `retry_delay_ms`, `error_status`, `error`, optional `no_response`. `error` is one of `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `rate_limit`, `overloaded`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error`, `unknown`. Use it for "rate-limited, retrying in N s" UI.
- `system/compact_boundary`
- `system/informational`
- `system/worker_shutting_down`
- `system/permission_denied` (with `--permission-prompts none`)
- `system/plugin_install`
- `hook_*` events
- `prompt_suggestion`
- A rate-limit event with fields like `status`, `resetsAt`, `rateLimitType`, `utilization`: the docs mention it, but the exact name and shape are UNVERIFIED.

Sources: https://code.claude.com/docs/en/headless.md · https://code.claude.com/docs/en/agent-sdk/agent-loop.md · https://code.claude.com/docs/en/agent-sdk/streaming-output.md

---

## 3. Multi-turn, interrupt and permission prompts

### 3.1 Two multi-turn strategies

**A. One long-lived process per task** (`--input-format stream-json`). Documented as the preferred mode for the SDK ("Streaming Input Mode").

- Write one NDJSON line per user message to stdin:
  ```json
  {"type":"user","message":{"role":"user","content":"Now also update the README"},"parent_tool_use_id":null}
  {"type":"user","message":{"role":"user","content":[{"type":"text","text":"See this mock"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"…"}}]},"parent_tool_use_id":null}
  ```
  The SDK wire form also adds `"session_id"` (UNVERIFIED).
- Messages queue and run in order. Each produces its own `result`.
- The process stays alive until stdin reaches EOF. An error result does not kill a streaming session; a crash does, after emitting `error_during_execution`.
- Pros: images, mid-task follow-ups, interrupts, approvals without restarting, warm MCP connections.
- Cons: one OS process and roughly 1 GiB RAM per active task, and memory grows over long sessions (docs guidance).

**B. Re-invoke per turn**: `claude -p "<follow-up>" --resume <sid> --output-format stream-json --verbose`.

- Stateless and crash-tolerant, and uses only documented flags.
- Costs: startup latency on every turn, and no mid-turn injection.
- **`--mcp-config`, `--settings`, `--plugin-dir`, `--fallback-model` and `--add-dir` are not restored on resume.** Pass them again every time.
- In `-p`, the permission mode is not restored. The exception is plan mode, when you pass `--permission-prompt-tool` and not `--permission-mode` or `--fork-session`.
- Never run two processes on the same session ID at once: "messages from both interleave into one transcript". Use `--fork-session` to branch.

Recommendation: A for the live "monitor/chat with the agent" view, and B as the recovery path after an app restart (you store the session ID in your DB).

### 3.2 Interrupt and stop

| Mechanism | Behavior | Status |
|---|---|---|
| Control request `{"subtype":"interrupt"}` on stdin | Ends the current turn and keeps the process alive. The next `result` is typically `error_during_execution`. The `interrupt_receipt_v1` and `interrupt_cancel_queued_v1` capabilities advertise the receipt and cancel-queued semantics. | Wire format UNVERIFIED (from SDK source). The capability names are documented. |
| **SIGINT** | "To end the turn instead, send SIGINT, or call the Agent SDK's `interrupt()`, before you stop the process." | Documented. Whether the process stays alive in stream-json mode after SIGINT is UNVERIFIED. |
| **SIGTERM** | Exit code **143**. The turn is left unfinished with no result. Running Bash process trees are killed, `SessionEnd` hooks run, and a pending permission prompt is left unanswered. On resume the interrupted turn stays as-is, unless `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`. | Documented. |
| Close stdin (EOF) | Graceful end. A pending permission prompt is cancelled as soon as input ends. Background shells are killed after about 5 s. | Documented. |

Rust tip: spawn with `.process_group(0)` (`CommandExt` on Unix) so the app can signal the whole tree. Send SIGINT first, wait for `result`, then close stdin, then SIGTERM, then SIGKILL on timeout.

### 3.3 Surfacing tool-permission requests to your UI

Documented evaluation order: hooks → deny rules → ask rules → permission mode → allow rules → **host callback** (`canUseTool`). `dontAsk` denies at the last step. Auto-approved calls never reach the callback. `AskUserQuestion`, `requiresUserInteraction` MCP tools and org "ask" connectors always reach it.

Three ways for a Rust host to answer:

1. **Control protocol over stdio**, used by the TS/Python SDKs and Vibe Kanban. **UNVERIFIED: undocumented and internal**, based on the open-source `claude-agent-sdk-python` `_internal/query.py`. Pin the CLI version if you rely on it.

   Your app → CLI (stdin):
   ```json
   {"type":"control_request","request_id":"req_1_a1b2","request":{"subtype":"initialize","hooks":null}}
   {"type":"control_request","request_id":"req_2_c3d4","request":{"subtype":"interrupt"}}
   {"type":"control_request","request_id":"req_3_e5f6","request":{"subtype":"set_permission_mode","mode":"acceptEdits"}}
   {"type":"control_request","request_id":"req_4_g7h8","request":{"subtype":"set_model","model":"opus"}}
   ```
   CLI → your app (stdout), when a tool needs approval:
   ```json
   {"type":"control_request","request_id":"<cli-id>","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf dist"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"rm -rf dist"}],"behavior":"allow","destination":"localSettings"}],"blocked_path":null,"tool_use_id":"toolu_01Y"}}
   ```
   Your reply (stdin), echoing the same `request_id`:
   ```json
   {"type":"control_response","response":{"subtype":"success","request_id":"<cli-id>","response":{"behavior":"allow","updatedInput":{"command":"rm -rf dist"},"updatedPermissions":[]}}}
   {"type":"control_response","response":{"subtype":"success","request_id":"<cli-id>","response":{"behavior":"deny","message":"User rejected: archive instead","interrupt":false}}}
   ```
   - The CLI acknowledges your requests with `{"type":"control_response","response":{"subtype":"success"|"error","request_id":"req_…","response":{…}|"error":"…"}}`. It may also send `control_cancel_request` for a request it abandons.
   - The allow/deny payload matches the documented `canUseTool` result: `behavior`, `updatedInput`, `updatedPermissions`, `message`. Since v2.1.207, `updatedInput` is optional.
   - `AskUserQuestion` arrives through the same request type. Answer with `updatedInput: {questions, answers: {"<question text>": "<label>"}}`.
   - The SDK enables this route by launching with `--permission-prompt-tool stdio` (UNVERIFIED).

2. **MCP permission-prompt tool** (documented flag). Host a small MCP server inside the Rust app (HTTP transport on localhost), pass it via `--mcp-config`, and add `--permission-prompt-tool mcp__app__approve`. From earlier official docs (UNVERIFIED on 2.1.283): the tool receives `{tool_name, input, tool_use_id}` and returns a text content block containing JSON: `{"behavior":"allow","updatedInput":{…}}` or `{"behavior":"deny","message":"…"}`. It can't approve `requiresUserInteraction` tools.

3. **Hooks** (documented, version-stable). A `PreToolUse` or `PermissionRequest` hook, as a command or HTTP hook, calls back into your app. A hook can allow, deny or modify a call, but a hook allow does not skip deny/ask rules. A `defer` decision lets the process exit and resume later. See https://code.claude.com/docs/en/hooks.md for the exact output JSON (not fetched here).

For unattended runs, pass `--permission-prompts none` (v2.1.259+): anything that would prompt is denied, and denials are reported in the stream.

Sources: https://code.claude.com/docs/en/agent-sdk/user-input.md · https://code.claude.com/docs/en/agent-sdk/permissions.md · https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode.md · https://code.claude.com/docs/en/agent-sdk/python.md (`interrupt()` "only works in streaming mode"; the buffer still holds the interrupted turn's result)

---

## 4. Authentication

- **Login methods:**
  - `/login` (interactive TUI only; not available in `-p`)
  - `claude auth login [--email <e>] [--sso] [--console]`. It prints the URL and reads a pasted code from stdin, which also works for SSH and containers.
  - `claude auth logout`
  - **`claude auth status`**: prints JSON by default, `--text` for human-readable output. **Exit code 0 when logged in, 1 when not.** This is the non-interactive check to use. The JSON field names are UNVERIFIED; I expect something like `loggedIn`, `authMethod`, `apiProvider`, `email`, `orgName`, `subscriptionType`.
  - `/status` inside a session shows the active method.
- **`claude setup-token`**: opens the same browser authorization as `/login` and prints a **one-year OAuth token**. It does not save it anywhere; you set it as `CLAUDE_CODE_OAUTH_TOKEN`.
  - Requires a Pro, Max, Team or Enterprise plan.
  - The token can only make model requests: no Remote Control, no claude.ai connectors. Locally configured MCP servers still work.
  - Not read in `--bare` mode.
- **Precedence**, from highest to lowest:
  1. Cloud provider, when `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX` or `CLAUDE_CODE_USE_FOUNDRY` is set
  2. `ANTHROPIC_AUTH_TOKEN` (sent as Bearer)
  3. `ANTHROPIC_API_KEY` (**in `-p` it is always used when present**, with no approval prompt)
  4. `apiKeyHelper`
  5. `CLAUDE_CODE_OAUTH_TOKEN`
  6. Anthropic profile or federation credentials (`ANTHROPIC_PROFILE`, WIF)
  7. Subscription OAuth from `/login`

  A signed-in Claude apps gateway session outranks all of these.
- **Practical point for your app:** a stray `ANTHROPIC_API_KEY` in the parent environment silently overrides the user's subscription. Decide explicitly, per user setting, whether to call `cmd.env_remove("ANTHROPIC_API_KEY")` on the child.
- **Credential storage:**
  - **macOS**: the login Keychain. If the Keychain rejects the write (for example when locked over SSH), it falls back to `~/.claude/.credentials.json` with mode 0600. The Keychain item name is UNVERIFIED (believed to be `Claude Code-credentials`).
  - **Linux**: `~/.claude/.credentials.json`, mode 0600.
  - **Windows**: `%USERPROFILE%\.claude\.credentials.json`.
  - `CLAUDE_CONFIG_DIR` relocates the file and changes which Keychain entry is used.
  - `claude doctor` reports Keychain writability.
  - Parallel sessions on one machine share the login and coordinate token refresh (fixed in v2.1.211).
- **Expiry:** a warning appears 3 days before a `/login` login expires. After that, requests fail with `Login expired · Please run /login`. Unattended sessions stall until the user logs in again, so surface this in the UI.
- **Other headless errors:** `Not logged in · Please run /login`, `Invalid API key`, and usage limits such as `You've hit your session limit` or `…weekly limit`. In `-p` these arrive as the result on stdout. Retryable failures emit `system/api_retry` with `error:"rate_limit"` and similar values.

Sources: https://code.claude.com/docs/en/authentication.md · https://code.claude.com/docs/en/cli-reference.md · https://code.claude.com/docs/en/troubleshoot-install.md · https://code.claude.com/docs/en/errors.md

---

## 5. Anthropic policy on Claude.ai login in third-party products

### 5.1 What the docs say (quoted verbatim)

- Agent SDK overview and quickstart: *"Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK. Use the API key authentication methods described in the Quickstart instead."*
- Legal and compliance, "Authentication and credential use":
  - *"OAuth authentication is intended exclusively for purchasers of Claude Free, Pro, Max, Team, and Enterprise subscription plans and is designed to support ordinary use of Claude Code and other native Anthropic applications."*
  - *"Developers building products or services that interact with Claude's capabilities, including those using the Agent SDK, should use API key authentication through Claude Console or a supported cloud provider. Anthropic does not permit third-party developers to offer Claude.ai login into their own applications, or to route requests through Free, Pro, or Max plan credentials on behalf of their users. Moreover, developers may not collect, store, or intermediate Claude.ai credentials or session tokens — sign-in to a Claude account must complete through Anthropic's own flow."*
  - *"…Nor does it prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription, including where a platform hosts Claude Code as described under 'Can customers offer Claude Code in their products?'"*
  - *"Anthropic reserves the right to take measures to enforce these restrictions and may do so without prior notice."*
- "Can customers offer Claude Code in their products?": this requires the Commercial Terms, plus:
  - The binary must not be modified, and customers may not remove, disable or restrict any built-in auth method.
  - Customers may not pay for, resell or intermediate Claude usage; each end user authenticates with their own API key, subscription or cloud credential.
  - The product can say in plain text that it "runs Claude Code", but may not use the Claude Code or Anthropic names or logos in its own product name or logo.
- Usage policy: *"Advertised usage limits for Pro and Max plans assume ordinary, individual usage of Claude Code and the Agent SDK."*
- Agent SDK branding: "Claude Agent" and "{YourAgentName} Powered by Claude" are allowed. "Claude Code", "Claude Code Agent" and Claude Code-style ASCII art are not.

### 5.2 What this means for your app (my interpretation, not legal advice)

**Personal or local tool:** a Rust app that spawns the user's own installed, unmodified `claude`, which the user logged into through Anthropic's flow (`claude auth login` or `/login`).

- This is closest to the explicitly allowed case: "an end user signing in to the unmodified Claude Code binary with their own Claude subscription."
- To stay on that side of the line:
  - Implement no OAuth of your own and no "Login with Claude" web flow.
  - Never read the Keychain or `.credentials.json`, never copy tokens, never store `CLAUDE_CODE_OAUTH_TOKEN` for the user, never proxy requests.
  - Detect login with `claude auth status` (exit code or JSON).
  - If the user isn't logged in, open a real terminal running `claude auth login`, or tell the user to run it. Anthropic's own flow then completes the sign-in.
- Remaining risk: many parallel agents in a kanban may stretch "ordinary, individual usage". Show usage-limit and `api_retry` events and cap concurrency.

**Hosted, multi-user service:**

- Don't offer Claude.ai login, and don't route through Pro/Max credentials.
- Use each user's own Console API key (billed to them), Bedrock/Vertex/Foundry, or get prior approval from Anthropic sales.
- If you host the unmodified `claude` binary in per-user sandboxes, users may sign in with their own subscription through Anthropic's flow. That requires the Commercial Terms, and you may not intermediate or store their credentials.
- Don't name the product anything like "Claude Code …".

Sources: https://code.claude.com/docs/en/agent-sdk/overview.md · https://code.claude.com/docs/en/legal-and-compliance.md · https://code.claude.com/docs/en/agent-sdk/quickstart.md

---

## 6. SDK languages vs. driving the CLI from Rust

### 6.1 Official Agent SDK

- **TypeScript** (`@anthropic-ai/claude-agent-sdk`, Node 18+) and **Python** (`claude-agent-sdk`, Python 3.10+) only.
- Both bundle a native Claude Code binary pinned to the SDK version, and spawn it as a stdio subprocess ("One agent session maps to one subprocess").
- Docs: *"To drive the same agent loop from a language other than Python or TypeScript, run the CLI as a subprocess with the `-p` flag and `--output-format json`."* So Rust driving the CLI is the officially suggested route. The bidirectional control protocol, however, is only exposed through the SDKs.
- Python `ClaudeAgentOptions` has `cli_path` (point it at the user's installed CLI) and `extra_args`. TS has `pathToClaudeCodeExecutable`.

### 6.2 Options for your Rust app

| Option | Pros | Cons |
|---|---|---|
| **A. Rust ↔ CLI stream-json and control protocol** (Vibe Kanban-style) | Single binary, full feature set (approvals, interrupt, set_mode, images) | The control protocol is undocumented. Pin a minimum CLI version, feature-detect via `system/init.capabilities`, tolerate unknown fields. |
| **B. Rust ↔ CLI, documented surfaces only** (`-p` stream-json, `--resume`, MCP permission-prompt tool or hooks served by the Rust app, SIGINT/SIGTERM) | Version-stable | More moving parts (a local MCP or HTTP endpoint). Harder to inject follow-ups mid-turn if you use per-turn processes. |
| **C. Node or Python sidecar running the Agent SDK, talking to Rust over a socket** | Official, typed API | Extra runtime. The bundled binary differs from the user's installed CLI unless you set `pathToClaudeCodeExecutable`/`cli_path`. The SDK docs steer SDK-based products toward API keys (see §5). |

### 6.3 Rust crates

All **UNVERIFIED**; crates.io and GitHub were blocked, so check maintenance status, license and CLI-version compatibility on crates.io, lib.rs or docs.rs:

- `cc-sdk` (repo ZhangHanDong/claude-code-api-rs): a Rust port of the Python SDK with interactive-client and control-protocol support.
- `claude-agent-sdk-rs` / `claude-agent-sdk`: community ports claiming Python-SDK parity.
- `claude-codes`: typed serde models for the Claude Code JSON protocol.

Recommendation: write a thin in-house layer (tokio `Child` with a `BufReader` line stream, serde enums with a `#[serde(other)]` or `Value` fallback). The protocol surface is small and changes often, so a third-party crate can lag behind the CLI.

### 6.4 Vibe Kanban reference (UNVERIFIED)

- BloopAI/vibe-kanban: Rust backend (axum, SQLite) plus a React frontend, distributed via `npx vibe-kanban`.
- An `executors` crate supports Claude Code, Codex, Gemini CLI, Amp and others, running each task attempt in its own git worktree.
- The Claude executor launches roughly `npx -y @anthropic-ai/claude-code@<pinned> -p --verbose --output-format=stream-json --input-format=stream-json --include-partial-messages --permission-prompt-tool=stdio [--permission-mode …]`, and implements the control protocol in Rust (`claude/protocol.rs`, `client.rs`).
- It relies on the user's own CLI login. Worth reading as prior art.

---

## 7. Design implications for the Rust app

1. **One `claude` process per active task**, with its cwd set to a per-task git worktree, launched in stream-json in/out mode. Persist `session_id` from `system/init` (or pre-assign it with `--session-id`) together with the NDJSON event log, which drives the "monitor what the agent is doing" view. Update the task status on `result`.
2. **Approvals:** route permission requests into a UI queue. Start in `default` mode with `--allowedTools` for safe reads, and let the user switch to `acceptEdits` or `plan` per task (via `set_permission_mode` or a restart).
3. **Auth UX:** on startup run `claude --version` and `claude auth status`. If not logged in, show an "Open terminal to log in" action that runs `claude auth login`. Never handle tokens yourself. Make the `ANTHROPIC_API_KEY` passthrough explicit.
4. **Don't use `--bare`** (it breaks subscription auth). Isolate each run with `--settings`, `--strict-mcp-config` and explicit tool lists, and warn users that untrusted repositories' `.claude/settings.json` hooks and `.mcp.json` servers run under `-p`.
5. **Resilience:** after an app restart, resume with `-p --resume <sid>`, passing `--mcp-config`, `--settings` and `--add-dir` again. Stop gracefully with SIGINT or an interrupt request, then close stdin, then SIGTERM (exit 143).
6. **Cost and limits:** show `total_cost_usd` as an estimate, `api_retry` progress, and usage-limit errors. Cap concurrency with `--max-budget-usd` and `--max-turns`.

---

### Sources

- [Headless / `-p`](https://code.claude.com/docs/en/headless.md)
- [CLI reference](https://code.claude.com/docs/en/cli-reference.md)
- [Authentication](https://code.claude.com/docs/en/authentication.md)
- [Legal and compliance](https://code.claude.com/docs/en/legal-and-compliance.md)
- [Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview.md)
- [Agent SDK quickstart](https://code.claude.com/docs/en/agent-sdk/quickstart.md)
- [Agent loop](https://code.claude.com/docs/en/agent-sdk/agent-loop.md)
- [Streaming output](https://code.claude.com/docs/en/agent-sdk/streaming-output.md)
- [Streaming input](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode.md)
- [User input / approvals](https://code.claude.com/docs/en/agent-sdk/user-input.md)
- [SDK permissions](https://code.claude.com/docs/en/agent-sdk/permissions.md)
- [SDK sessions](https://code.claude.com/docs/en/agent-sdk/sessions.md)
- [SDK hosting](https://code.claude.com/docs/en/agent-sdk/hosting.md)
- [Python SDK reference](https://code.claude.com/docs/en/agent-sdk/python.md)
- [TypeScript SDK reference](https://code.claude.com/docs/en/agent-sdk/typescript.md) (truncated when fetched; message type definitions not captured)
- [Permission modes](https://code.claude.com/docs/en/permission-modes.md)
- [CLI sessions](https://code.claude.com/docs/en/sessions.md)
- [Troubleshoot install and login](https://code.claude.com/docs/en/troubleshoot-install.md)
- [Errors](https://code.claude.com/docs/en/errors.md)
- [Hooks](https://code.claude.com/docs/en/hooks.md) (referenced, not fetched)

Not reachable in this session, so the related claims are UNVERIFIED: https://github.com/anthropics/claude-agent-sdk-python (control protocol in `_internal/query.py`, CLI arguments in `_internal/transport/subprocess_cli.py`), https://github.com/BloopAI/vibe-kanban, https://crates.io.