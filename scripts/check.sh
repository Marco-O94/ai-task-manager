#!/usr/bin/env bash
# Standard verification (spec §11.1) plus the security greps (spec §10.3).
# Any failing command or any grep hit makes the script exit non-zero.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
# --locked: Cargo.toml and Cargo.lock are frozen after M1 (spec §11.1); a manifest edit that
# would rewrite the lock fails here instead of passing silently.
cargo clippy --locked --workspace --exclude atm-ui --all-targets -- -D warnings
# The UI as the release WASM (no test drivers), as the debug bundles' (`testkit`: selftest and
# E2E drivers, spec §11.2 M6) and as the in-browser mock.
cargo clippy --locked -p atm-ui --target wasm32-unknown-unknown -- -D warnings
cargo clippy --locked -p atm-ui --target wasm32-unknown-unknown --features testkit -- -D warnings
cargo clippy --locked -p atm-ui --target wasm32-unknown-unknown --features mock -- -D warnings
cargo check --locked -p atm-types --target wasm32-unknown-unknown
cargo test --locked --workspace --exclude atm-ui
cargo test --locked -p atm-ui --features testkit # M0 confirmed that atm-ui also builds for the host

fail=0
hits() { # hits <rule> <matches>
    if [[ -n "$2" ]]; then
        printf 'security grep hit (%s):\n%s\n' "$1" "$2" >&2
        fail=1
    fi
}
# grep where only status 1 (no match) is clean: status 2 (missing path, unreadable file) aborts the
# script instead of silently disabling a check. Results go through `m=$(...)` so that set -e sees it.
scan() {
    local rc=0
    grep "$@" || rc=$?
    if ((rc > 1)); then
        echo "check.sh: grep failed ($rc): grep $*" >&2
        exit 2
    fi
}

m=$(scan -rnF -e '<script' -e 'inner_html' -e 'set_inner_html' \
    -e 'dangerousDisableAssetCspModification' ui/src)
hits "ui/src" "$m"

# The forbidden CLI flags only in code that can pass them: tests may assert their absence.
m=$(scan -rnF -e '"--bare"' -e '"--dangerously-skip-permissions"' crates/*/src src-tauri/src)
hits "crates/*/src, src-tauri/src" "$m"

# The project's own code never listens (the single-instance plugin's Unix socket is a dependency's,
# declared in spec §10.2).
m=$(scan -rnF -e 'find-generic-password' -e 'SecKeychain' -e 'TcpListener' -e 'UdpSocket' \
    -e 'UnixListener' -e '0.0.0.0' crates src-tauri/src)
hits "crates, src-tauri/src" "$m"

# osascript only in `Inner::notify`, and its one script takes the texts as `argv` (never
# interpolated into the AppleScript, round 2026-10-01).
m=$(scan -rnF 'osascript' crates/*/src src-tauri/src | scan -vE '^crates/atm-core/src/lib\.rs:')
hits "osascript outside Inner::notify" "$m"
m=$(scan -rnF 'display notification' crates/*/src src-tauri/src |
    scan -vF '"display notification (item 2 of argv) with title (item 1 of argv)"')
hits "osascript script with interpolated text" "$m"

# credentials.json may appear only inside the `DENY_RULES` constant (up to its closing `];`).
m=$(scan -rlF 'credentials.json' crates src-tauri/src |
    while IFS= read -r f; do
        awk '/(const|static)[[:space:]]+DENY_RULES/ { inside = 1 }
             inside { if (/\];/) inside = 0; next }
             /credentials\.json/ { print FILENAME ":" FNR ": " $0 }' "$f" || exit 2
    done)
hits "credentials.json outside DENY_RULES" "$m"

# CLAUDE_CODE_OAUTH_TOKEN may appear only in a comment in atm-core's claude.rs (passthrough note)
# and as an entry of the M5 real-CLI guard's forbidden list (it refuses to run when it is set).
m=$(scan -rnF 'CLAUDE_CODE_OAUTH_TOKEN' crates src-tauri/src |
    scan -vE '^crates/atm-core/src/claude\.rs:[0-9]+:[[:space:]]*//' |
    scan -vE '^crates/atm-core/tests/real_cli\.rs:[0-9]+:[[:space:]]*"CLAUDE_CODE_OAUTH_TOKEN",$')
hits "CLAUDE_CODE_OAUTH_TOKEN" "$m"

# The M5 captures of the real CLI are committed and their sanitizer is best effort (a CLI update may
# add fields): no email, no home or temp path (plain or in the CLI's dashed form), no key, and no
# account value but `<redacted>`.
fixtures=crates/atm-core/tests/fixtures/real
m=$(scan -rnoE -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' -e '/Users/[^<]' \
    -e '-Users-[^<]' -e '/var/folders/' -e '-var-folders-' -e '/private/var/' -e '-private-var-' \
    -e 'sk-ant-' "$fixtures")
hits "$fixtures" "$m"
account_key='"(email|organization|orgId|orgName)"[[:space:]]*:[[:space:]]*'
m=$(scan -rnoE "$account_key"'("[^"]*"|[^,}[:space:]]+)' "$fixtures" |
    scan -vE "$account_key"'"<redacted>"$')
hits "$fixtures: account values" "$m"

m=$(scan -nE '"csp"[[:space:]]*:[[:space:]]*null|"devtools"[[:space:]]*:[[:space:]]*true' \
    src-tauri/tauri.conf.json)
hits "tauri.conf.json" "$m"

# The selftest and E2E drivers stay out of the release WASM (M6): the release build command does
# not enable `testkit`, and their modules are declared only under it (`scripts/release.sh` also
# checks the built WASM with `strings`).
m=$(scan -nE '"beforeBuildCommand".*testkit' src-tauri/tauri.conf.json)
hits "tauri.conf.json: release build with testkit" "$m"
m=$(awk '/^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+(e2e|selftest)[[:space:]]*[;{]/ &&
        prev !~ /^#\[cfg\(all\(feature = "testkit", not\(feature = "mock"\)\)\)\]$/ {
            print FILENAME ":" FNR ": " $0 }
        { prev = $0 }' ui/src/main.rs) || exit 2
hits "ui/src/main.rs: test driver outside testkit" "$m"
# The testkit overlay repeats the main window (a config merge replaces arrays) only to add
# `backgroundThrottling: disabled` (the E2E must not stall in a hidden window): no other drift.
# (Its `bundle.createUpdaterArtifacts: false` lets the debug bundle build without the signing key.)
m=$(/usr/bin/python3 - <<'EOF'
import json
main = json.load(open("src-tauri/tauri.conf.json"))["app"]["windows"]
kit = json.load(open("src-tauri/tauri.testkit.conf.json"))["app"]["windows"]
if len(kit) != 1 or kit[0].pop("backgroundThrottling", None) != "disabled" or kit != main:
    print("src-tauri/tauri.testkit.conf.json: app.windows differs from tauri.conf.json")
EOF
) || exit 2
hits "testkit overlay" "$m"

# The updater is driven from Rust only (round 2026-10-02): the webview gets none of its commands.
m=$(scan -rnF 'updater' src-tauri/capabilities)
hits "src-tauri/capabilities: updater permission" "$m"

if [[ $fail -ne 0 ]]; then
    echo "check.sh: security greps FAILED" >&2
    exit 1
fi
echo "check.sh: OK"
