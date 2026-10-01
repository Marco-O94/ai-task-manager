#!/usr/bin/env bash
# In-app E2E of spec §12.2 with fake-claude (M4, M6). Builds fake-claude and the debug .app
# bundle with the UI's test drivers (`--features testkit`, src-tauri/tauri.testkit.conf.json;
# assets embedded, CSP active), installs a copy of the bundle in the run's temporary directory
# and runs it three times on that directory's data:
# - phase 1: steps 1-7 (step 3 also checks the `mcp` project's Riepilogo: its committed
#   CLAUDE.md, the `.mcp.json` server with its env key and never the key's value, in neither
#   the page nor `get_project_overview`), then Cmd+Q (a ⌘Q key event posted to the app through
#   the window server -> menu Quit -> `NSApp terminate:` -> RunEvent::Exit) during a
#   [fake:hang] and a [fake:hang_ignore] turn: two agents running, none left afterwards
#   (`pgrep`);
# - reinstall: a fresh copy of the bundle over the installed one; phase 2 runs it on phase 1's
#   data, and every process row and log of phase 1 is still there, unchanged, afterwards;
# - phase 2: relaunch; step 4's order after the restart, the rest of step 8, steps 9-12, the
#   security confirmations, then the feature round's checks: `task_list_view` (Lista on main),
#   `attachment_to_the_agent`, `subagent_limit`, `subtasks_from_the_panel`,
#   `board_tools_agent`, `subtask_cascade`, `autopilot_fix_and_merge` and `autopilot_after`
#   (on the scratch project `da-rimuovere`),
#   `project_removal` (that project removed from the sidebar menu, main stays selected); then
#   `app.exit` -> RunEvent::ExitRequested during another [fake:hang_ignore] turn;
# - phase 3 (perf): [fake:flood] on three concurrent attempts, one of them open; the page stays
#   responsive and the transcript keeps at most 300 rows in the DOM.
# The UI drives its own DOM. stdout carries only the JSON reports of phases 2 and 3 (build logs
# go to stderr). Between and after the phases the script checks that no process of the run
# survived (recorded by fake-claude or working inside the run's directory) and, in the DB, that
# every hang turn ended as expected and the flood turns completed. Exits 0 only if all of this
# holds, every step passed and there were 0 CSP violations.
#
#   scripts/e2e.sh                 # build + run (the run is bounded at 15 minutes)
#   scripts/e2e.sh --no-build      # reuse target/debug (the bundle and fake-claude)
#   scripts/e2e.sh --keep          # keep logs and data of a passing run too
#   scripts/e2e.sh --perf          # only phase 3, on fresh data
#   scripts/e2e.sh --gatekeeper    # only the Gatekeeper check of the login .command: opens
#                                  # ONE Terminal window running the real login script, whose
#                                  # claude is a wrapper of fake-claude (`auth login`)
#
# Never runs the real claude: ATM_CLAUDE_PATH points at the run's own copy of
# target/debug/fake-claude and the app refuses to run the E2E without it. Nothing outside the
# temporary directory is touched, except the --gatekeeper login script in a folder of the app's
# cache dir (deleted afterwards), and only processes of this run are ever killed.
set -euo pipefail
cd "$(dirname "$0")/.."

build=1
keep=0
mode=e2e
for arg in "$@"; do
    case $arg in
    --no-build) build=0 ;;
    --keep) keep=1 ;;
    --gatekeeper) mode=gatekeeper ;;
    --perf) mode=perf ;;
    *)
        echo "usage: scripts/e2e.sh [--no-build] [--keep] [--perf | --gatekeeper]" >&2
        exit 2
        ;;
    esac
done

bundle="$PWD/target/debug/bundle/macos/AI Task Manager.app"
fake_built=$PWD/target/debug/fake-claude
if ((build)); then
    # Their output (trunk's included) goes to stderr: stdout is the reports alone.
    cargo build --locked -p atm-core --bin fake-claude >&2
    cargo tauri build --debug --bundles app --config src-tauri/tauri.testkit.conf.json \
        -- --locked >&2
fi
for bin in "$bundle/Contents/MacOS/ai-task-manager" "$fake_built"; do
    [[ -x $bin ]] || {
        echo "e2e: $bin missing (run without --no-build)" >&2
        exit 2
    }
done

dir=$(mktemp -d "${TMPDIR:-/tmp}/atm-e2e.XXXXXX")
dir_real=$(cd "$dir" && pwd -P)
# The installed app, as in /Applications, and the run's own fake-claude: `pgrep -f "$fake"`
# sees this run's agents only, never those of a concurrent `cargo test` or of another run.
installed="$dir_real/Applications/AI Task Manager.app"
app="$installed/Contents/MacOS/ai-task-manager"
fake=$dir_real/bin/fake-claude
mkdir -p "$dir_real/Applications" "$dir_real/bin"
cp -p "$fake_built" "$fake"
install_app() {
    rm -rf "$installed"
    ditto "$bundle" "$installed"
}
install_app
unset ATM_SELFTEST
export ATM_E2E=1 ATM_E2E_DIR="$dir" ATM_CLAUDE_PATH="$fake"
deadline=$((SECONDS + 900))
app_pid=

# The processes of this run still alive (none may survive a quit): the pids fake-claude
# recorded in this run's FAKE_CLAUDE_RECORD (its -p calls and the `sleep 300` of hang_ignore)
# that ps still shows running that program, and any process on the machine, recorded or not,
# whose working directory is inside the run's directory (the agents run in its worktrees).
# Never another run's, test's or app instance's.
agents() {
    local pids
    {
        pids=$(grep -oE '"pid":[0-9]+' "$dir/record.jsonl" 2>/dev/null | cut -d: -f2 | sort -u |
            paste -sd, -) || true
        if [[ -n $pids ]]; then
            ps -o pid=,command= -p "$pids" 2>/dev/null |
                awk -v fake="$fake" '
                    index($0, fake) || ($2 == "sleep" && $3 == "300") { print $1 }' || true
        fi
        lsof -w -d cwd -F pn 2>/dev/null |
            awk -v d="$dir_real/" '
                /^p/ { pid = substr($0, 2) }
                /^n/ && index(substr($0, 2) "/", d) == 1 { print pid }' ||
            true
    } | sort -un | paste -sd' ' -
}

# Ctrl-C (a background job ignores SIGINT here) or any exit: nothing of the run keeps running.
cleanup() {
    if [[ -n $app_pid ]]; then
        kill -9 "$app_pid" 2>/dev/null || true
    fi
    local left
    left=$(agents)
    if [[ -n $left ]]; then
        # shellcheck disable=SC2086 # one pid per word
        kill -9 $left 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# run_phase <phase> <max seconds>: the app's stdout/stderr go to $dir/<phase>.{out,err}.
# The app runs with every variable its children must not inherit (spec §7.2; dummy values,
# never sent anywhere): the UI checks in fake-claude's record that no agent got them. Those of
# a parent Claude Code session and of its host must also be gone from the app's own process
# (it re-executes itself without them, M6): `ps -E`, which shows any process of the same user
# the environment it was started with, must not show the messaging token, cmux's socket, nor
# the `NODE_OPTIONS` preload a cmux terminal sets when the user has none (marker `0`).
run_phase() {
    local phase=$1 end=$((SECONDS + $2)) rc=0 checked=0
    echo "e2e: phase ${phase}..." >&2
    ATM_E2E_PHASE=$phase ANTHROPIC_API_KEY=e2e-not-a-key ANTHROPIC_AUTH_TOKEN=e2e-not-a-token \
        CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=e2e GIT_DIR="$dir/not-a-git-dir" \
        CLAUDE_CODE_SESSION_ID=e2e-parent-session CLAUDE_CODE_CHILD_SESSION=1 \
        CLAUDE_CODE_SESSION_ATTENDED=1 CLAUDE_CODE_EXECPATH=/nonexistent/claude CLAUDE_PID=1 \
        CLAUDE_CODE_MESSAGING_TOKEN=e2e-parent-messaging-token CLAUDE_EFFORT=max \
        CLAUDE_CODE_SSE_PORT=1 ENABLE_IDE_INTEGRATION=true \
        CMUX_SOCKET_PATH="$dir/not-a-cmux.sock" CMUX_CUA_AUTH_TOKEN_FILE="$dir/not-a-token" \
        CMUX_ORIGINAL_NODE_OPTIONS_PRESENT=0 NODE_OPTIONS="--require=$dir/not-a-cmux-preload.js" \
        "$app" >"$dir/$phase.out" 2>"$dir/$phase.err" &
    app_pid=$!
    while kill -0 "$app_pid" 2>/dev/null; do
        if ((!checked && SECONDS > end - $2 + 3)); then
            checked=1
            local started_env
            started_env=$(ps -wwE -o command= -p "$app_pid" 2>/dev/null) || true
            if [[ -n $started_env ]]; then
                [[ $started_env == *"ATM_E2E_PHASE=$phase"* ]] ||
                    fail "phase $phase: ps -E does not show the app's environment"
                [[ $started_env != *CLAUDE_CODE_MESSAGING_TOKEN=* &&
                    $started_env != *CMUX_SOCKET_PATH=* &&
                    $started_env != *not-a-cmux-preload.js* ]] ||
                    fail "phase $phase: the parent session's variables are still in the app's environment (ps -E)"
                echo "e2e: phase $phase: ps -E of the app shows none of the parent session's variables" >&2
            fi
        fi
        if ((SECONDS > end || SECONDS > deadline)); then
            echo "e2e: phase $phase timed out" >&2
            kill -9 "$app_pid" 2>/dev/null || true
            wait "$app_pid" 2>/dev/null || true
            app_pid=
            return 124
        fi
        sleep 1
    done
    wait "$app_pid" || rc=$?
    app_pid=
    return "$rc"
}

# `status/stop_reason` of every [fake:hang…] turn, in order: step 7's user Stop of a
# [fake:hang_ignore] turn, then the turns the quits must have finalized.
hang_turns() {
    sqlite3 -readonly "$dir/data/atm.sqlite3" \
        "SELECT status || '/' || ifnull(stop_reason, '-') FROM processes
         WHERE instr(prompt, '[fake:hang') > 0 ORDER BY started_at"
}

fail() {
    cleanup
    echo "e2e: FAILED ($1); logs and data kept in $dir" >&2
    for f in "$dir"/*.out; do
        [[ -s $f ]] && cat "$f"
    done
    tail -n 40 "$dir"/*.err >&2 || true
    exit 1
}

# No fake-claude of this run is running (M6 acceptance #2 is `pgrep -f fake-claude` empty: the
# run's own copy makes the check exact); any other fake-claude on the machine is only reported.
no_fake_left() {
    local mine others
    mine=$(pgrep -fl -- "$fake") || true
    [[ -z $mine ]] || fail "fake-claude still running after $1: $mine"
    others=$(pgrep -fl fake-claude) || true
    echo "e2e: pgrep -f fake-claude after $1: none of this run${others:+ (others: $others)}" >&2
}

# DB, process logs and worktrees, listed. Compared before and after the reinstall only as a guard
# on this script: the data dir, the logs and the worktrees live outside the bundle, which
# install_app alone replaces.
kept_data() {
    (cd "$dir_real" && {
        ls data/atm.sqlite3
        find data/logs -type f
        find home/.ai-task-manager/worktrees -mindepth 1 -maxdepth 1
    } | sort)
}

# What phase 1 left, which the reinstalled app must find and keep (spec §11.2 M6 #5): every
# process row with its status, and the hash of every log of those (finished) processes. Checked
# again after phase 2 has run the new copy of the bundle on these data.
phase1_state() {
    sqlite3 -readonly "$dir/data/atm.sqlite3" \
        "SELECT id || ' ' || status FROM processes ORDER BY id" || return 1
    (cd "$dir_real/data/logs" && find . -type f -print0 | sort -z | xargs -0 shasum)
}

# Phase 3: the perf report (on stdout), then no agent left and the three flood turns completed.
# Its two timings need the page visible (WebKit fires a hidden page's timers once a second):
# with the screen locked, the window covered or the app hidden they are not measured, which
# fails `--perf` alone and is only a warning in the full run (every other check still holds).
perf_phase() {
    run_phase 3 300 || fail "phase 3 exited $?"
    cat "$dir/3.out"
    grep -qF '"csp_violations":0' "$dir/3.out" || fail "CSP violations in phase 3"
    if ! grep -qF '"perf_responsiveness":"measured"' "$dir/3.out"; then
        [[ $mode == perf ]] && fail "phase 3: responsiveness not measured (page hidden)"
        echo "e2e: WARNING: phase 3 could not measure the responsiveness (page hidden: screen" \
            "locked, window covered or app hidden); rerun scripts/e2e.sh --perf with the screen" \
            "unlocked" >&2
    fi
    left=$(agents)
    [[ -z $left ]] || fail "agents left after phase 3: $left"
    no_fake_left "phase 3"
    # Every text of every flood turn stored: 10000 AssistantText rows per process (the UI
    # checks only the newest page of each).
    local floods
    floods=$(sqlite3 -readonly "$dir/data/atm.sqlite3" \
        "SELECT p.status || '/' || ifnull(p.result_subtype, '-') || '/' ||
                (SELECT count(*) FROM entries e
                 WHERE e.process_id = p.id AND e.kind = 'AssistantText')
         FROM processes p
         WHERE instr(p.prompt, '[fake:flood]') > 0 ORDER BY p.started_at") ||
        fail "DB not readable"
    [[ $floods == $'completed/success/10000\ncompleted/success/10000\ncompleted/success/10000' ]] ||
        fail "flood turns (status/subtype/texts stored): $floods"
    echo "e2e: the 3 flood turns completed with their 10000 texts each in the DB" >&2
}

if [[ $mode == gatekeeper ]]; then
    run_phase gatekeeper 90 || fail "gatekeeper phase exited $?"
    cat "$dir/gatekeeper.out"
    rm -rf "$dir"
    exit 0
fi
if [[ $mode == perf ]]; then
    perf_phase
    if ((keep)); then
        echo "e2e: logs and data kept in $dir" >&2
    else
        rm -rf "$dir"
    fi
    echo "e2e: OK (perf)" >&2
    exit 0
fi

run_phase 1 300 || fail "phase 1 exited $?"
[[ -s $dir/phase1.json ]] || fail "phase 1 quit without handing over"
# Cmd+Q's path: the menu's `terminate:` ends the event loop without ExitRequested (the shell logs
# the branch).
grep -qx 'exit: RunEvent::Exit' "$dir/1.err" && ! grep -q 'exit: RunEvent::ExitRequested' "$dir/1.err" ||
    fail "phase 1 did not quit through RunEvent::Exit alone"
left=$(agents)
[[ -z $left ]] || fail "agents left after the Cmd+Q quit: $left"
no_fake_left "the Cmd+Q quit with 2 agents running"
turns=$(hang_turns) || fail "DB not readable"
[[ $turns == $'killed/user_stop\nkilled/app_shutdown\nkilled/app_shutdown' ]] ||
    fail "hang turns after the Cmd+Q quit: $turns"
# Reinstall (M6 #5): a fresh copy of the bundle replaces the installed one; phase 2 runs the new
# copy on the data phase 1 left, which must still be there, unchanged, after it.
before=$(kept_data) || fail "phase 1 left no DB, logs or worktrees"
state1=$(phase1_state) || fail "phase 1 left no readable DB"
old_inode=$(stat -f %i "$app")
install_app
[[ $(stat -f %i "$app") != "$old_inode" ]] || fail "the reinstall did not replace the app"
after=$(kept_data) || fail "data gone after the reinstall"
[[ $after == "$before" ]] || fail "the reinstall changed the data: $(diff <(echo "$before") <(echo "$after"))"
run_phase 2 300 || fail "phase 2 exited $?"
state2=$(phase1_state) || fail "DB not readable after phase 2"
missing=$(comm -23 <(sort <<<"$state1") <(sort <<<"$state2"))
[[ -z $missing ]] || fail "after the reinstall, phase 2 lost or changed data of phase 1: $missing"
echo "e2e: reinstall: phase 2 ran the new copy on phase 1's data and kept its" \
    "$(grep -vc '^[0-9a-f]\{40\}  ' <<<"$state1") process rows and" \
    "$(grep -c '^[0-9a-f]\{40\}  ' <<<"$state1") logs unchanged, $(grep -c '^home/' <<<"$after") worktrees" >&2
report=$(cat "$dir/2.out")
echo "$report"
grep -qF '"csp_violations":0' <<<"$report" || fail "CSP violations"
grep -qx 'exit: RunEvent::ExitRequested' "$dir/2.err" ||
    fail "phase 2 did not exit through RunEvent::ExitRequested"
left=$(agents)
[[ -z $left ]] || fail "agents left after the app.exit of phase 2: $left"
no_fake_left "the app.exit of phase 2"
turns=$(hang_turns) || fail "DB not readable"
[[ $turns == $'killed/user_stop\nkilled/app_shutdown\nkilled/app_shutdown\nkilled/app_shutdown' ]] ||
    fail "hang turns after the app.exit of phase 2: $turns"
echo "e2e: no process of the run left after either quit; hang turns: user Stop killed/user_stop," \
    "the others killed/app_shutdown" >&2
perf_phase
if ((keep)); then
    echo "e2e: logs and data kept in $dir" >&2
else
    rm -rf "$dir"
fi
echo "e2e: OK" >&2
