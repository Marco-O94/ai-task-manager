# Vibe Kanban (BloopAI/vibe-kanban): reference architecture report

Sources were read with `curl` from raw.githubusercontent.com and api.github.com, plus WebFetch. `gh` is not installed. The GitHub API rate limit was hit partway through, so the issue research used github.com pages instead. Nothing was written to the project folder. The scratchpad holds only `tree.json` and `paths.txt`.

URL prefixes used below:
- `M:` means `https://github.com/BloopAI/vibe-kanban/blob/main/`
- `T:` means `https://github.com/BloopAI/vibe-kanban/blob/v0.0.116-20251106133609/`

## 0. Which version to study

- **The product is shut down.** The README says "Vibe Kanban is sunsetting". The blog post https://www.vibekanban.com/blog/shutdown gives the date as 10 Apr 2026 and says it moves to community maintenance and back to a fully local setup. Latest tag is `v0.1.45-20260919085201`.
- **There are two architectures in the history:**
  - **Classic (≤ v0.0.13x, up to about Dec 2025):** fully local. The model is project → task → task_attempt, with one git repo per project. **This is the right reference for a minimal clone.** I used tag `v0.0.116-20251106133609`.
  - **Current main:**
    - task_attempts became `workspaces` + `sessions` (migration `M:crates/db/migrations/20251216142123_refactor_task_attempts_to_workspaces_sessions.sql`).
    - Workspaces can hold several repos (`20251209000000_add_project_repositories.sql`).
    - Kanban issues moved to a remote Postgres/ElectricSQL server (`crates/remote`; `20251202000000_migrate_to_electric.sql`, `20260113144821_remove_shared_tasks.sql`). The local `tasks` table is now legacy and mostly read-only.
    - Added relay, WebRTC, Tauri, SSH and preview-proxy crates.
- The Claude executor, MsgStore, streaming, approvals and git internals are much the same in both versions. Main is more polished, so I cite main for those parts.

## 1. Workspace crate layout

Classic (T:, 8 crates):

| crate | responsibility |
|---|---|
| `server` | axum 0.8 HTTP/WS/SSE API under `/api`. Serves the React build embedded with `rust_embed` (`#[folder="../../frontend/dist"]`, `T:crates/server/src/routes/frontend.rs`). Binaries: main server, `bin/mcp_task_server.rs` (MCP), `bin/generate_types.rs` (ts-rs → `shared/types.ts`). Routes live in `routes/{projects,tasks,task_attempts,execution_processes,events,approvals,auth,github,images,config,drafts,tags,filesystem,containers}.rs`. |
| `db` | sqlx 0.8 on SQLite, with feature `sqlite-preupdate-hook`. Migrations in `crates/db/migrations/*.sql`. Models in `src/models/*.rs` use `query_as!` macros with offline `.sqlx` data. |
| `executors` | Coding-agent adapters (`executors/{claude,codex,gemini,amp,cursor,opencode,qwen,copilot,acp}`). Also `actions/` (ExecutorAction chain: CodingAgentInitialRequest, CodingAgentFollowUpRequest, ScriptRequest), `logs/` (NormalizedEntry, JSON-Patch helpers, plain-text/stderr processors), `profile.rs` + `default_profiles.json` (variants), `mcp_config.rs` + `default_mcp.json`, `stdout_dup.rs`. |
| `services` | Business services: `container.rs` (the `ContainerService` trait: start/stop/chain executions, finalize), `git.rs`/`git_cli.rs`, `worktree_manager.rs`, `events/` (SQLite hooks → JSON Patch), `diff_stream.rs`, `approvals/`, `github_service.rs` (octocrab), `pr_monitor.rs`, `config/versions/v1..v7`, `notification.rs`, `filesystem_watcher.rs`, `image.rs`, `drafts.rs`, `analytics.rs`. |
| `deployment` | The `Deployment` trait: an accessor bundle for config, db, container, git, events, approvals and so on. Lets the server stay generic over local vs cloud (`M:crates/deployment/src/lib.rs`). |
| `local-deployment` | The concrete `LocalDeployment` + `LocalContainerService`. "Containers" are git worktrees. Holds the child-process store, exit monitor, auto-commit and periodic worktree cleanup (`src/container.rs`, `src/command.rs`). |
| `utils` | `msg_store.rs` (broadcast + bounded history), `log_msg.rs` (LogMsg ↔ SSE/WS), `diff.rs`, `text.rs` (branch slug, short uuid), `path.rs` (temp dirs), `port_file.rs`, `shell.rs`, `approvals.rs` (shared types), `process.rs` (process-group kill), `assets.rs`. |

Main adds these crates (27+):
- **Split out of `services`:** `git` (git2 + CLI, `M:crates/git/src/lib.rs`, 1722 LOC), `git-host` (GitHub/Azure through the `gh`/`az` CLIs), `worktree-manager`, `workspace-manager` (multi-repo layout), `mcp` (its own crate).
- **Cloud / remote:** `api-types`, `remote` (cloud server, excluded from the workspace), `relay-*`, `trusted-key-auth`, `embedded-ssh`, `desktop-bridge`, `tauri-app`, `preview-proxy`.
- **Other:** `review` (a standalone PR-review CLI).

Root `M:Cargo.toml` pins: `axum 0.8.4` (features macros/multipart/ws), `tokio full`, `git2 0.20`, `ts-rs` (fork), `schemars`.

## 2. Database schema (SQLite; ids are UUID BLOBs; timestamps are `TEXT datetime('now','subsec')`)

### Classic, v0.0.116
Sources: `T:crates/db/src/models/*.rs` and migrations.

- **projects**: `id, name, git_repo_path UNIQUE, setup_script, dev_script, cleanup_script, copy_files` (a list of files such as `.env` to copy into worktrees), plus timestamps.
- **tasks**: `id, project_id FK CASCADE, title, description, status CHECK IN ('todo','inprogress','inreview','done','cancelled'), parent_task_attempt` (subtasks spawned from an attempt), timestamps.
  - The API returns `TaskWithAttemptStatus`: task + `has_in_progress_attempt`, `has_merged_attempt`, `last_attempt_failed`, `executor`, all computed by a join.
- **task_attempts**: `id, task_id FK, container_ref` (worktree path), `branch, target_branch, executor` (e.g. `CLAUDE_CODE`), `worktree_deleted, setup_completed_at`, timestamps.
- **execution_processes**:
  - `id, task_attempt_id FK`
  - `run_reason CHECK IN ('setupscript','codingagent','devserver','cleanupscript')`
  - `executor_action` (JSON of the ExecutorAction chain)
  - `before_head_commit, after_head_commit`
  - `status CHECK IN ('running','completed','failed','killed')`, `exit_code`
  - `dropped` (set when a user "restores" to an earlier point)
  - `started_at, completed_at`
  - Indexes on `(task_attempt_id, run_reason, created_at)`.
- **executor_sessions**: `id, task_attempt_id, execution_process_id FK, session_id` (the external Claude/Amp session id), `prompt, summary` (last assistant message).
- **execution_process_logs**: `execution_id FK, logs TEXT` (JSONL, one `LogMsg` per line), `byte_size, inserted_at`. The PK was dropped so rows can be appended (`20251101090000_drop_execution_process_logs_pk.sql`).
- **merges**: `id, task_attempt_id, merge_type CHECK('direct','pr'), merge_commit` (direct), `pr_number, pr_url, pr_status CHECK('open','merged','closed'), pr_merged_at, pr_merge_commit_sha, target_branch_name`. A CHECK constraint enforces the direct-vs-PR column sets (`M:crates/db/migrations/20250819000000_move_merge_commit_to_merges_table.sql`).
- **images**: `id, file_path` (relative to cache/images), `original_name, mime_type, size_bytes, hash UNIQUE` (SHA-256 dedup).
- **task_images**: junction table (`20250818150000_refactor_images_to_junction_tables.sql`).
- Also: `tags` (reusable prompt snippets, formerly task_templates) and `drafts` (unsent follow-ups).

### Current main
- `repos`: `path UNIQUE, name, display_name, setup_script, cleanup_script, archive_script, copy_files, parallel_setup_script, dev_server_script, default_target_branch`.
- `project_repos`.
- `workspaces`: `task_id` is no longer a foreign key; columns `container_ref, branch, agent_working_dir, setup_completed_at, archived, pinned, name, worktree_deleted`.
- `workspace_repos(workspace_id, repo_id, target_branch)`.
- `sessions(workspace_id, executor, name, agent_working_dir)`.
- `execution_processes`: now keyed by `session_id`.
- `execution_process_repo_states(execution_process_id, repo_id, before_head_commit, after_head_commit, merge_commit)`.
- `coding_agent_turns(execution_process_id, agent_session_id, agent_message_id, prompt, summary, seen)`.
- `merges` (direct merges only).
- `pull_requests(pr_url UNIQUE, pr_number, pr_status, target_branch_name, merged_at, merge_commit_sha, synced_at)` (`20260311000000_add_tracked_prs.sql`).
- `scratch(id, scratch_type, payload JSON)`: drafts and queued messages.
- `workspace_images`, `tags`.
- **Logs have moved out of SQLite.** They are now JSONL files at `{asset_dir}/sessions/{session_id}/processes/{process_id}.jsonl`. There is a one-time migration "to move logs from SQLite to flat file to improve performance" (`M:crates/utils/src/execution_logs.rs`, `M:crates/services/src/services/execution_process.rs`).
- Migration style: SQLite table rebuilds with the `COMMIT; PRAGMA foreign_keys=OFF; BEGIN … PRAGMA foreign_key_check; COMMIT` workaround, because sqlx-sqlite has no `-- no-transaction`.

## 3. Task status enum and transitions

- **Enum:** `TaskStatus {Todo (default), InProgress, InReview, Done, Cancelled}`, serialized in lowercase (`T:crates/db/src/models/task.rs`).
- **Transitions in classic:**
  - **Todo → InProgress:** in `ContainerService::start_execution` for any run_reason except DevServer (`T:crates/services/src/services/container.rs` ~L587).
  - **→ InReview:**
    - when the process fails to start (same file ~L660);
    - in `finalize_task` once the last action in the chain completes, fails or is killed (`T:crates/local-deployment/src/container.rs` ~L132);
    - on a user stop (`stop_execution` ~L885).
  - **→ Done:**
    - after a direct merge (`T:crates/server/src/routes/task_attempts.rs` ~L691);
    - when an attached PR is already merged (~L1606);
    - when `pr_monitor` finds the PR merged. It polls every 60 s (`T:crates/services/src/services/pr_monitor.rs` ~L142).
  - **Cancelled:** manual only (`PUT /api/tasks/{id}`, or the MCP `update_task` tool).
- **Per-attempt / process state:** `ExecutionProcessStatus {Running, Completed, Failed, Killed}`. `finalize` is skipped for DevServer processes and for parallel setup scripts. Killed processes do not send notifications.

## 4. Claude Code executor

Main file: `M:crates/executors/src/executors/claude.rs` (3284 LOC). Helpers in `claude/{protocol,client,types}.rs`.

### Command line (main)
```
npx -y @anthropic-ai/claude-code@2.1.119 -p
  [plan|approvals: --permission-prompt-tool=stdio --permission-mode=bypassPermissions]
  [auto:           --disallowedTools=AskUserQuestion]
  [--dangerously-skip-permissions] [--model M] [--effort low|medium|high|xhigh|max] [--agent A]
  --verbose --output-format=stream-json --input-format=stream-json
  --include-partial-messages --replay-user-messages
follow-up adds: --resume <agent_session_id> [--resume-session-at <message_uuid>]
```
- Classic v0.0.116 used `@anthropic-ai/claude-code@2.0.31` and follow-ups ran `--fork-session --resume <id>`.
- There is an optional `claude_code_router` path: `npx -y @musistudio/claude-code-router@1.0.66 code`.
- `CmdOverrides` allows `base_command_override`, `additional_params` and `env` (`M:crates/executors/src/command.rs`). The command is split with shlex and the executable is looked up with `resolve_executable_path`.
- Default profile: `CLAUDE_CODE.DEFAULT = {dangerously_skip_permissions: true}` (`M:crates/executors/default_profiles.json`).

### Process setup and environment
- Spawned with `tokio::process::Command` via `group_spawn_no_window` (crate `command_group`, so it gets its own process group), with `kill_on_drop(true)`.
- stdin, stdout and stderr are all piped; cwd is the worktree (or its `working_dir` subdirectory).
- Environment:
  - `NPM_CONFIG_LOGLEVEL=error`
  - `VK_WORKSPACE_ID`, `VK_WORKSPACE_BRANCH` (`M:crates/local-deployment/src/container.rs` ~L1366)
  - the profile's env
  - `ANTHROPIC_API_KEY` is removed if `disable_api_key` is set.
- The spawn has a 30 s timeout.

### stdin: bidirectional control protocol
`ProtocolPeer` (`protocol.rs`) writes newline-delimited JSON to the CLI's stdin. Types are in `types.rs`.
1. `{"type":"control_request","request_id":uuid,"request":{"subtype":"initialize","hooks":{…}}}`
2. `{"type":"control_request",…,"request":{"subtype":"set_permission_mode","mode":"plan|default|bypassPermissions"}}`
3. `{"type":"user","message":{"role":"user","content":"<prompt + append_prompt>"}}`
4. It answers the CLI's `control_request` messages with `{"type":"control_response","response":{"subtype":"success|error","request_id",…}}`. Two request kinds arrive: `can_use_tool` (response `{"behavior":"allow","updatedInput",…}` or `{"behavior":"deny","message","interrupt"}`) and `hook_callback`.
5. To interrupt: `{"subtype":"interrupt"}`.

The read loop exits when it sees `{"type":"result"}`. After that the peer is dropped. UNVERIFIED: I assume this closes stdin and the CLI then exits.

### stdout handling
- The protocol reader consumes the child's real stdout.
- Every line is re-written through `LogWriter` into a fresh pipe that replaces `child.stdout` (`M:crates/executors/src/stdout_dup.rs`). The generic container pipeline therefore sees both the CLI's JSON and synthetic lines (`ApprovalRequested`, `ApprovalResponse`, `QuestionResponse`).
- `track_child_msgs_in_store` wraps stdout and stderr in `ReaderStream`, maps them to `LogMsg::Stdout` / `LogMsg::Stderr`, and forwards them into a per-process `MsgStore` (`M:crates/local-deployment/src/container.rs` ~L857).

### Log normalization
`ClaudeLogProcessor::process_logs` (claude.rs ~L762):
- Subscribes to `msg_store.history_plus_stream()` and buffers stdout into lines.
- Parses each line as `serde_json::from_str::<ClaudeJson>`. Variants: `System, Assistant, User, ToolUse, ToolResult, StreamEvent{MessageStart, ContentBlockStart/Delta/Stop, MessageDelta, MessageStop}, Result, RateLimitEvent, Approval*, Control*, Unknown`.
- **Session and message ids:**
  - The first non-null `session_id` → `msg_store.push_session_id`.
  - A `User` message's `uuid` → `push_message_id`.
  - An `Assistant` message's `uuid` is held as pending and only committed when a `Result` arrives. These uuids are what `--resume-session-at` accepts.
- **Output:** a JSON Patch per entry, `[{op:"add"|"replace", path:"/entries/{idx}", value:{type:"NORMALIZED_ENTRY", content: NormalizedEntry}}]` (`M:crates/executors/src/logs/utils/patch.rs`).
- **Entry types:** `NormalizedEntryType {UserMessage, UserFeedback, AssistantMessage, ToolUse{tool_name, action_type, status}, SystemMessage, ErrorMessage, Thinking, Loading, NextAction, TokenUsageInfo, UserAnsweredQuestions}` (`M:crates/executors/src/logs/mod.rs`).
- **Tool mapping (`extract_action_type`):** each Claude tool maps to an `ActionType`: Read → FileRead; Edit/Write/MultiEdit → FileEdit with a unified diff; Bash → CommandRun; Grep/Glob → Search; WebFetch/WebSearch; Task → TaskCreate; ExitPlanMode → PlanPresentation; TodoWrite → TodoManagement; AskUserQuestion; MCP tools → Tool.
- **Other rules:**
  - A `tool_result` replaces the original tool entry, looked up in `tool_map` by tool_use id.
  - Partial text/thinking deltas update one streaming entry with `replace`.
  - Lines that are not JSON become `SystemMessage`.
  - stderr goes through `PlainTextLogProcessor` (2 s grouping gap) and becomes `ErrorMessage`.
  - If the `System` init message reports `apiKeySource == "ANTHROPIC_API_KEY"`, a warning entry is added.
  - `EntryIndexProvider` keeps entry indexes consistent across the stdout and stderr processors.

### Persistence
- `spawn_stream_raw_logs_to_storage` appends **only** Stdout/Stderr `LogMsg`s as JSONL.
- `LogMsg::SessionId` → `coding_agent_turns.agent_session_id` (`M:crates/services/src/services/execution_process.rs` ~L286–310).
- Normalized patches are **not** stored. To replay a finished process, it loads the raw log into a temporary MsgStore, re-runs the normalizer, and removes consecutive patches to the same path (`M:crates/services/src/services/container.rs` `stream_normalized_logs` ~L830).

### Follow-ups
- `POST /api/sessions/{id}/follow-up` (classic: `/api/task-attempts/{id}/follow-up`).
- It builds `CodingAgentFollowUpRequest{prompt, session_id: latest agent_session_id, reset_to_message_id}`, which runs `spawn_follow_up` → `--resume`.
- "Reset to process" (`reset_session_to_process`, services/container.rs ~L633): git-reset each repo to that process's `before_head_commit`, mark later processes `dropped`, then resume with `--resume-session-at`.
- Messages sent while an agent is running are queued (`queued_message_service`) and started from the exit monitor.

### Stop / kill (`stop_execution`, local-deployment/container.rs ~L1405)
1. Set the DB status to Killed.
2. Cancel the `CancellationToken`, which makes the ProtocolPeer send an `interrupt` control_request and keep reading.
3. Wait up to 5 s for the exit monitor.
4. `kill_process_group`: SIGINT, wait 2 s, SIGTERM, wait 2 s, SIGKILL, all via `killpg` on the pgid captured at spawn (`M:crates/utils/src/process.rs`).
5. `push_finished`.
6. Wait up to 5 s for the log writer.
7. Record `after_head_commit`.

### Exit monitor (`spawn_exit_monitor` ~L480)
- Waits on either the OS process exit or the executor's optional `exit_signal`.
- Updates the DB.
- On success: `try_commit_changes` auto-commits leftover changes, then either starts `next_action` (the cleanup script, which runs only if the agent changed something) or calls `finalize_task` (notification; in classic, also task → InReview).
- Then drains queued follow-ups.
- On startup, `cleanup_orphan_executions` marks rows still `running` as Failed.

## 5. Git worktree lifecycle

- **Base directory** (`M:crates/worktree-manager/src/worktree_manager.rs` ~L510, `M:crates/utils/src/path.rs` ~L108):
  - macOS: `std::env::temp_dir()/vibe-kanban/worktrees` (under `/var/folders/...`)
  - Linux: `/var/tmp/vibe-kanban/worktrees`
  - debug builds use `vibe-kanban-dev`
  - a user override maps to `<dir>/.vibe-kanban-workspaces`.
- **Names:**
  - Directory: `{short_uuid(first 4 hex)}-{slug}`. The slug is lowercase, non-alphanumerics become `-`, and it is capped at 16 chars (`M:crates/utils/src/text.rs`).
  - Branch: `{git_branch_prefix}/{short_uuid}-{slug}`. The default prefix is `vk` (`M:crates/services/src/services/config/versions/v8.rs`).
  - In classic both are derived from the attempt id and the task title.
  - In main, a workspace directory contains one worktree subdirectory per repo (`{workspace_dir}/{repo.name}`).
- **Create:**
  1. git2 `create_branch(base)`.
  2. Take a per-path tokio Mutex.
  3. If the worktree is not properly set up, run a cleanup: `git worktree remove --force`, delete `.git/worktrees/<name>`, `rm -rf` the directory, `git worktree prune`.
  4. Run **CLI** `git worktree add` (chosen for sparse-checkout semantics), and retry once after a metadata cleanup.
  5. Copy the `copy_files` globs and task images into the worktree.
  6. Store `container_ref`.
- **Scripts:**
  - setup: sequential chain via `next_action`, or parallel per repo;
  - coding agent;
  - cleanup: only after changes;
  - dev server: `run_reason=devserver`, never finalizes;
  - archive.
  - All run via `sh -c`/`bash -c` (`get_shell_command`) with stdin set to null, in their own process group (`M:crates/executors/src/actions/script.rs`).
- **Cleanup:**
  - Every 30 min, delete the worktrees of workspaces idle for more than 72 h (1 h if archived). The branch is kept and `worktree_deleted=1` is set.
  - `ensure_container_exists` recreates the worktree lazily.
  - The env var `DISABLE_WORKTREE_CLEANUP` turns this off (`M:crates/local-deployment/src/container.rs` ~L276, `M:crates/db/src/models/workspace.rs` ~L285).
- **Diff:**
  - Base is the merge-base of the workspace branch and the target branch (`get_base_commit`).
  - `get_diffs` lists paths with `git diff` CLI name-status against the base, including the working tree, and loads contents with git2 (`M:crates/git/src/lib.rs` ~L327).
  - Live diff (`M:crates/services/src/services/diff_stream.rs`): a `notify-debouncer-full` watcher on the worktree plus a watcher on the git dir (HEAD/refs) re-diffs the changed paths and emits patches at `/entries/{repo}/{path}`.
  - Past 200 MB of cumulative diff bytes, file contents are omitted (`content_omitted`). There is a `stats_only` mode.
- **Merge** (`merge_changes` ~L575): always a **squash**.
  - Refused if the base branch has moved ahead (`BranchesDiverged`); the user must rebase.
  - If the base branch is checked out somewhere, it runs CLI `merge --squash` there and refuses if staged changes exist.
  - Otherwise it builds the squash commit in memory with libgit2 and updates the ref.
  - Afterwards the task branch ref is reset to the squash commit so follow-up work continues from the merged state.
- **Rebase** (~L1129): `git rebase --onto new_base old_base task_branch` via CLI in the worktree.
  - Refuses a dirty tree or a rebase already in progress.
  - Conflicts come back as `MergeConflicts{conflicted_files}`, with abort and continue endpoints.
- **PR:**
  1. `push_to_remote(branch)`.
  2. `gh pr create --repo owner/name --head B --base T --title … --body-file tmp [--draft]` (`M:crates/git-host/src/github/cli.rs` ~L225).
  3. An AI-written description is optional: it runs a follow-up agent turn (`M:crates/server/src/routes/workspaces/pr.rs`).
  4. Status is polled with `gh pr view --json …` every 60 s.
  - Classic used octocrab plus a GitHub OAuth device-flow token instead (`T:crates/server/src/routes/auth.rs`, `T:crates/services/src/services/github_service.rs`).

## 6. Streaming to the frontend

### MsgStore
`M:crates/utils/src/msg_store.rs`:
- A `tokio::broadcast` channel (capacity 100k) plus a `VecDeque` history capped at about 100 MB.
- `history_plus_stream()` gives a replay of history followed by live messages. On `Lagged` it drops messages and logs an error.
- There is one store per execution process and one global store for DB events.
- `LogMsg {Stdout, Stderr, JsonPatch, SessionId, MessageId, Ready, Finished}` (`M:crates/utils/src/log_msg.rs`).
  - As SSE: `event: json_patch|stdout|…`.
  - As WS text frames: `{"JsonPatch":[ops]}`, `{"Ready":true}`, `{"finished":true}`.

### DB → UI updates
`M:crates/services/src/services/events.rs`:
- sqlx `set_update_hook` and `set_preupdate_hook` are installed on every pooled connection.
  - On insert/update: fetch the row by rowid and build a JSON Patch (`add`/`replace` at `/tasks/{id}` or `/execution_processes/{id}`, and in main `/workspaces/{id}` and `/scratch/...`).
  - On delete: build the `remove` patch from the old column value in the pre-update hook.
- Patches are pushed into the global MsgStore.
- Each WS endpoint filters the global broadcast per client (classic `stream_tasks_raw` filters on `project_id`, `T:crates/services/src/services/events/streams.rs`). It first sends a snapshot `{"op":"replace","path":"/tasks","value":{id: task…}}` and then live patches.

### Transport
- **Mostly WebSocket.** Classic endpoints:
  - `GET /api/tasks/stream/ws?project_id=`
  - `GET /api/execution-processes/stream/ws` (UNVERIFIED query param name)
  - `GET /api/execution-processes/{id}/raw-logs/ws`
  - `GET /api/execution-processes/{id}/normalized-logs/ws`
  - `GET /api/task-attempts/{id}/diff/ws`
- **SSE** exists only at `GET /api/events` (global store history + live, `KeepAlive`, `T:crates/server/src/routes/events.rs`).
- The WS handlers drain and ignore client messages and just forward frames.
- Main uses signed WS (`middleware/signed_ws.rs`) for the relay.

### Frontend
- React hook `useJsonPatchWsStream` applies patches with `rfc6902` + `immer` and reconnects with exponential backoff from 1 s to 8 s (`M:packages/web-core/src/shared/hooks/useJsonPatchWsStream.ts`).

### Other classic REST routes
- Tasks: `/api/tasks` (CRUD) and `/api/tasks/create-and-start`.
- Attempt actions under `/api/task-attempts/{id}/`: `follow-up`, `stop`, `merge`, `push`, `rebase`, `conflicts/abort`, `pr`, `pr/attach`, `branch-status`, `start-dev-server`, `replace-process`, `open-editor`, `children`, `change-target-branch`, `rename-branch`.
- `/api/projects/{id}/branches`, `/api/config`, `/api/profiles`, `/api/mcp-config`, `/api/approvals/{id}/respond`.

## 7. Approvals and permissions

- **Modes:**
  - `PermissionPolicy::Auto`: the CLI runs with bypassPermissions (the default profile even adds `--dangerously-skip-permissions`).
  - `Supervised`: `approvals=true`.
  - `Plan`: `plan=true`.
- **Hooks sent in `initialize`** (`get_hooks`, claude.rs ~L205):
  - Supervised: `PreToolUse` with matcher `^(?!(Glob|Grep|NotebookRead|Read|Task|TodoWrite)$).*` → callback `tool_approval`.
  - Plan: `^(ExitPlanMode|AskUserQuestion)$` → approval; every other tool → `AUTO_APPROVE_CALLBACK_ID`.
  - Optional `Stop` hook (commit reminder): blocks the stop with a reason if there are uncommitted changes.
- **Flow** (`claude/client.rs`):
  1. The hook callback answers `permissionDecision:"ask"`. This makes the CLI send a `can_use_tool` control_request. This is how `--permission-prompt-tool=stdio` gets used.
  2. `ExecutorApprovalBridge` (`M:crates/services/src/services/approvals/executor_approvals.rs`) creates an `ApprovalRequest` with a 10 h timeout (`APPROVAL_TIMEOUT_SECONDS=36000`, `M:crates/utils/src/approvals.rs`) and sends an OS notification.
  3. A synthetic `ApprovalRequested` line goes into the log, so the normalizer sets `ToolStatus::PendingApproval{approval_id}` on the tool entry.
  4. It then waits on a oneshot receiver shared as a future, with a timeout watcher.
  5. The UI answers with `POST /api/approvals/{id}/respond {execution_process_id, status: approved | denied{reason} | answered{answers} | timed_out}`. Pending approvals stream on `GET /api/approvals/stream/ws`.
  6. The answer becomes a `PermissionResult`:
     - Allow `{updatedInput}`. Approving ExitPlanMode also sends `updatedPermissions:[{type:setMode, mode:bypassPermissions, destination:session}]`.
     - Deny `{message: "The user doesn't want to proceed with this tool use… " + reason, interrupt:false}`.
     - Timeout → deny with `interrupt:true`.
  - For AskUserQuestion, the answers are injected into `updatedInput.answers`.
- The in-memory service is `Approvals` (DashMap pending/completed + broadcast of patches, `M:crates/services/src/services/approvals.rs`).
- The approvals bridge is only wired for Codex, Claude, Gemini, Qwen and Opencode; the rest get a Noop service (`M:crates/local-deployment/src/container.rs` ~L1330).

## 8. MCP server

- **Binary:**
  - Classic: `T:crates/server/src/bin/mcp_task_server.rs` + `T:crates/server/src/mcp/task_server.rs`.
  - Main: `M:crates/mcp/src/bin/vibe_kanban_mcp.rs`.
- Built on `rmcp` with the **stdio** transport. It is a thin HTTP client to the running backend.
- Backend URL resolution: `VIBE_BACKEND_URL`, else `HOST` + `BACKEND_PORT`/`PORT`, else the port file the server writes at startup (`write_port_file_with_proxy`).
- Agents launch it as `npx -y vibe-kanban@latest --mcp` (`M:crates/executors/default_mcp.json`). The UI writes that entry into the agent's MCP config (Claude: `~/.claude.json`) through `/api/mcp-config`.
- **Classic tools:** `list_projects`, `list_tasks(project_id,status,limit)`, `create_task`, `get_task`, `update_task(status…)`, `delete_task`, `start_task_attempt(task_id, executor, variant, base_branch)`. This lets an agent plan and dispatch subtasks.
- **Main tools:** workspaces / sessions (`run_session_prompt`, execution status), remote issues / projects / tags / assignees / relationships, repos.

## 9. Claude authentication

- **Vibe Kanban has no Claude authentication of its own.** It relies entirely on the user's local `claude` CLI login.
  - `get_availability_info` only checks whether `~/.claude.json` exists and reads its mtime → `LoginDetected{last_auth_timestamp}` (claude.rs ~L600).
  - The README says: "Make sure you have authenticated with your favourite coding agent."
- If `ANTHROPIC_API_KEY` is set it is inherited, which means API billing. A warning entry is shown, and the `disable_api_key` option strips the variable.
- The `/api/auth/handoff/*`, `/auth/local/login` and `/auth/token` routes (`M:crates/server/src/routes/oauth.rs`) and `oauth_credentials.rs` are for the **Vibe Kanban cloud account** (GitHub/Google OAuth handoff to the `remote` server, with a JWT refresh token stored locally). They are not an Anthropic login.
- Classic's `/api/auth/github/device/*` produced a GitHub token for PRs.
- **Important for a "log in with Claude" feature:**
  - Nothing in the repo implements an OAuth flow against Anthropic.
  - Practical options are to spawn `claude /login` / `claude setup-token` in a PTY, or to accept `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`. These are UNVERIFIED — general knowledge of the Claude CLI, not in this repo.
  - Check Anthropic's terms before offering claude.ai subscription login inside a third-party app (UNVERIFIED policy risk).
  - Issue #3417 reports that from 15 Jun 2026, `claude -p` and Agent SDK usage draw from a separate monthly credit pool (Pro $20 / Max5x $100 / Max20x $200). This is UNVERIFIED and user-reported: https://github.com/BloopAI/vibe-kanban/issues/3417

## 10. Complexity hotspots

- **`local-deployment/src/container.rs` (1646 LOC) and `services/src/services/container.rs` (1392 LOC):** the exit-monitor state machine. It handles action chaining (setup → agent → cleanup), parallel vs sequential setup, auto-commit rules, queued follow-ups, finalize/notify, orphan recovery and before/after HEAD bookkeeping.
- **`executors/claude.rs` (3284 LOC):** normalization of streaming deltas, tool_use/tool_result pairing, subagent handling, context-window tracking, plus the control protocol in `client.rs`/`protocol.rs`.
- **`git/src/lib.rs` (1722 LOC) and `worktree_manager.rs`:** a mix of libgit2 and git CLI, worktree metadata repair, squash merge with and without a checked-out base, rebase conflict handling.
- **`diff_stream.rs`:** combining two watchers, debounce/reset logic, byte caps.
- **Events:** SQLite hooks → row re-fetch → per-client filtering. Remove patches cannot be filtered by project.
- **Schema churn:** 80+ migrations with SQLite table rebuilds. Config went through versions v1–v8 with upgrade code. There are 10 executors plus the ACP harness, and profiles/variants.

## 11. Known pain points (issues)

- **Session resume breaks when the cwd changes.** Claude keys sessions under `~/.claude/projects/<cwd>`, so renaming a worktree or repo gives "No conversation found". Also, retries created poisoned session ids. #2993 was fixed by PR #2996; #3255 is still open. **Lesson: name worktree directories from a stable id only, never from a mutable title.**
- **`initialize` hooks replace the project's `.claude/settings.json` hooks** instead of merging with them (#3327, open).
- **Memory.**
  - Replaying historical logs with re-normalization is quadratic and can OOM (#3218, #3455).
  - Runaway memory while loading the board (#3373) and in long sessions (#3352).
  - Normalization tasks were not cancelled when the WS disconnected.
  - Logs were moved from SQLite to JSONL files for performance.
  - **Lesson: store normalized entries once a process has finished.**
- **WebSocket issues.**
  - No reconnect after the tab is suspended → session stuck "running" (#3227).
  - Double rendering from a race between streaming and the status reload (#3343).
  - "Invalid WS" (#1731); periodic refresh (#1361).
- **Pinned `npx` Claude version pointed at a release that does not exist** (#3347).
- **Lifecycle.** Requests for configurable worktree cleanup (#765); workspaces disappearing after cleanup (#3329); zombie processes after sleep/wake (#3205); the MCP server spawning processes in a loop (#3157); MCP JSON-RPC id truncated to i32 (#3232).
- **Product drift.** Users asked for the old UI and local-only projects back (#2687, #3354); requests to turn off auto-commit (#3313).
- Issue lists: https://github.com/BloopAI/vibe-kanban/issues?q=is%3Aissue%20sort%3Acomments-desc

## 12. Minimal Rust clone: what to keep and what to drop

**Must keep:**
1. **SQLite + sqlx + migrations** with this schema:
   - `projects(repo_path, setup/dev/cleanup scripts, copy_files)`
   - `tasks(status enum)`
   - `attempts(task_id, branch, target_branch, worktree_path, executor, worktree_deleted)`
   - `execution_processes(attempt_id, run_reason, status, exit_code, action_json, before/after_head)`
   - `agent_turns(process_id, agent_session_id, last_message_uuid, prompt, summary)`
   - `merges` / `pull_requests`
2. **Worktree per attempt.** Directory named from the id only; `vk/{short}-{slug}` branch; `git worktree add` through the CLI; the metadata repair and prune routine; copy_files; lazy recreate.
3. **Claude executor.**
   - Command: `claude -p --output-format=stream-json --input-format=stream-json --verbose --include-partial-messages` (+ `--replay-user-messages`). Prefer the `claude` binary on PATH, with an override, over a pinned `npx` version.
   - Prompt sent via stdin JSON.
   - Capture `session_id` and message uuids; follow-ups use `--resume` (+ `--resume-session-at`).
   - Process group spawn; stop with the `interrupt` control_request first, then SIGINT → SIGTERM → SIGKILL.
4. **Per-process MsgStore** (broadcast + bounded history), raw JSONL log file, a Claude normalizer producing `NormalizedEntry`s as JSON Patch at `/entries/{i}`. Also persist the normalized result when the process finishes, which fixes the OOM problem.
5. **Exit monitor.** Update status, auto-commit, move task Todo→InProgress→InReview, run the next action (setup/cleanup), recover orphans at startup.
6. **One streaming transport with JSON Patch.** Pick WS or SSE. Needed for the task board (snapshot + patches), process logs and the live diff. A simpler option than SQLite hooks: have the service layer publish patches after each write.
7. **Git operations:** diff against the merge-base, squash merge with the "base moved ahead → rebase" guard, rebase via CLI, push + `gh pr create`, and a PR status poll → Done.

**Can defer (phase 2):**
- Supervised/plan approvals via the `initialize` hooks + `can_use_tool` flow (start with bypassPermissions). If you add hooks later, merge them with project hooks.
- The MCP task server (rmcp stdio → local HTTP).
- The dev-server script.
- Queued follow-ups and drafts.

**Can drop:**
- The other 9 executors and ACP, the profiles/variants system, config version migrations.
- All cloud and packaging extras: remote/ElectricSQL/relay/WebRTC/Tauri/SSH/preview-proxy/review crates, multi-repo workspaces, analytics/Sentry/PostHog, images, tags/templates, editor integration, claude-code-router, slash-command/model discovery.
- ts-rs type generation. With a Rust frontend, share a `serde` types crate between backend and WASM frontend instead; I did not check whether the `json-patch` crate builds for WASM (UNVERIFIED).
- UNVERIFIED: I did not fetch rust-ui.com. It is assumed to be a Leptos + Tailwind component library; if so, the frontend is a Leptos CSR/SSR app talking to this axum API over WS.

## Primary source URLs

- https://github.com/BloopAI/vibe-kanban (README, AGENTS.md, Cargo.toml)
- M:crates/executors/src/executors/claude.rs, claude/protocol.rs, claude/client.rs, claude/types.rs
- M:crates/executors/src/executors/mod.rs, actions/mod.rs, actions/script.rs, actions/coding_agent_follow_up.rs, env.rs, command.rs, stdout_dup.rs, logs/mod.rs, logs/utils/patch.rs, default_profiles.json, default_mcp.json
- M:crates/utils/src/msg_store.rs, log_msg.rs, process.rs, text.rs, path.rs, execution_logs.rs, approvals.rs
- M:crates/local-deployment/src/container.rs; M:crates/services/src/services/{container.rs, events.rs, diff_stream.rs, approvals.rs, approvals/executor_approvals.rs, execution_process.rs}
- M:crates/worktree-manager/src/worktree_manager.rs; M:crates/git/src/lib.rs; M:crates/git-host/src/github/cli.rs; M:crates/server/src/routes/{events.rs, approvals.rs, oauth.rs, workspaces/pr.rs, workspaces/streams.rs, sessions/mod.rs}
- M:crates/db/migrations/ (init, execution_processes, executor_sessions, merges, images, project_repositories, refactor_task_attempts_to_workspaces_sessions, add_tracked_prs, refactor_to_scratch)
- T:crates/db/src/models/*.rs, T:crates/services/src/services/{container.rs, events/streams.rs, events/patches.rs, pr_monitor.rs}, T:crates/local-deployment/src/container.rs, T:crates/server/src/routes/{mod.rs, tasks.rs, task_attempts.rs, execution_processes.rs, events.rs}, T:crates/server/src/mcp/task_server.rs, T:crates/server/src/bin/mcp_task_server.rs
- M:packages/web-core/src/shared/hooks/useJsonPatchWsStream.ts
- https://www.vibekanban.com/blog/shutdown; issues #2993, #3218, #3327, #3417, plus the issue search pages above.