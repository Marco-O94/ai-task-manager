#!/usr/bin/env bash
# In-app E2E of spec §12.2 with fake-claude (M4). Builds fake-claude and the debug bundle
# (assets embedded, CSP active), then runs the app twice on one temporary directory:
# - phase 1: steps 1-7, then Cmd+Q (a ⌘Q key event posted to the app through the window
#   server -> menu Quit -> `NSApp terminate:` -> RunEvent::Exit) during a [fake:hang] and a
#   [fake:hang_ignore] turn;
# - phase 2: relaunch; step 4's order after the restart, the rest of step 8, steps 9-12, then
#   `app.exit` -> RunEvent::ExitRequested during another [fake:hang_ignore] turn.
# The UI drives its own DOM. stdout carries only phase 2's JSON report (build logs go to
# stderr). Between and after the phases the script checks that no process of the run survived
# (recorded by fake-claude or working inside the run's directory) and, in the DB, that every
# hang turn ended as expected. Exits 0 only if all of this holds, every step passed and there
# were 0 CSP violations.
#
#   scripts/e2e.sh                 # build + run (the run is bounded at 10 minutes)
#   scripts/e2e.sh --no-build      # reuse target/debug
#   scripts/e2e.sh --keep          # keep logs and data of a passing run too
#   scripts/e2e.sh --gatekeeper    # only the Gatekeeper check of the login .command: opens
#                                  # ONE Terminal window running the real login script, whose
#                                  # claude is a wrapper of fake-claude (`auth login`)
#
# Never runs the real claude: ATM_CLAUDE_PATH points at target/debug/fake-claude and the app
# refuses to run the E2E without it. Nothing outside the temporary directory is touched,
# except the --gatekeeper login script in a folder of the app's cache dir (deleted afterwards),
# and only processes of this run are ever killed.
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
    *)
        echo "usage: scripts/e2e.sh [--no-build] [--keep] [--gatekeeper]" >&2
        exit 2
        ;;
    esac
done

app=$PWD/target/debug/ai-task-manager
fake=$PWD/target/debug/fake-claude
if ((build)); then
    # Their output (trunk's included) goes to stderr: stdout is the report alone.
    cargo build --locked -p atm-core --bin fake-claude >&2
    cargo tauri build --debug --no-bundle -- --locked >&2
fi
for bin in "$app" "$fake"; do
    [[ -x $bin ]] || {
        echo "e2e: $bin missing (run without --no-build)" >&2
        exit 2
    }
done

dir=$(mktemp -d "${TMPDIR:-/tmp}/atm-e2e.XXXXXX")
dir_real=$(cd "$dir" && pwd -P)
unset ATM_SELFTEST
export ATM_E2E=1 ATM_E2E_DIR="$dir" ATM_CLAUDE_PATH="$fake"
deadline=$((SECONDS + 600))
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
# never sent anywhere): the UI checks in fake-claude's record that no agent got them.
run_phase() {
    local phase=$1 end=$((SECONDS + $2)) rc=0
    echo "e2e: phase ${phase}..." >&2
    ATM_E2E_PHASE=$phase ANTHROPIC_API_KEY=e2e-not-a-key ANTHROPIC_AUTH_TOKEN=e2e-not-a-token \
        CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=e2e GIT_DIR="$dir/not-a-git-dir" \
        "$app" >"$dir/$phase.out" 2>"$dir/$phase.err" &
    app_pid=$!
    while kill -0 "$app_pid" 2>/dev/null; do
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

if [[ $mode == gatekeeper ]]; then
    run_phase gatekeeper 90 || fail "gatekeeper phase exited $?"
    cat "$dir/gatekeeper.out"
    rm -rf "$dir"
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
turns=$(hang_turns) || fail "DB not readable"
[[ $turns == $'killed/user_stop\nkilled/app_shutdown\nkilled/app_shutdown' ]] ||
    fail "hang turns after the Cmd+Q quit: $turns"
run_phase 2 300 || fail "phase 2 exited $?"
report=$(cat "$dir/2.out")
echo "$report"
grep -qF '"csp_violations":0' <<<"$report" || fail "CSP violations"
grep -qx 'exit: RunEvent::ExitRequested' "$dir/2.err" ||
    fail "phase 2 did not exit through RunEvent::ExitRequested"
left=$(agents)
[[ -z $left ]] || fail "agents left after the app.exit of phase 2: $left"
turns=$(hang_turns) || fail "DB not readable"
[[ $turns == $'killed/user_stop\nkilled/app_shutdown\nkilled/app_shutdown\nkilled/app_shutdown' ]] ||
    fail "hang turns after the app.exit of phase 2: $turns"
echo "e2e: no process of the run left after either quit; hang turns: user Stop killed/user_stop," \
    "the others killed/app_shutdown" >&2
if ((keep)); then
    echo "e2e: logs and data kept in $dir" >&2
else
    rm -rf "$dir"
fi
echo "e2e: OK" >&2
