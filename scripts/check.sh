#!/usr/bin/env bash
# Standard verification (spec §11.1) plus the security greps (spec §10.3).
# Any failing command or any grep hit makes the script exit non-zero.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --exclude atm-ui --all-targets -- -D warnings
cargo clippy -p atm-ui --target wasm32-unknown-unknown -- -D warnings
# TODO(M1): enable once the `mock` feature exists:
# cargo clippy -p atm-ui --target wasm32-unknown-unknown --features mock -- -D warnings
cargo check -p atm-types --target wasm32-unknown-unknown
cargo test --workspace --exclude atm-ui
cargo test -p atm-ui # M0 confirmed that atm-ui also builds for the host

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

m=$(scan -rnF -e '"--bare"' -e '"--dangerously-skip-permissions"' \
    -e 'find-generic-password' -e 'SecKeychain' -e 'TcpListener' -e 'UdpSocket' -e '0.0.0.0' \
    crates src-tauri/src)
hits "crates, src-tauri/src" "$m"

# credentials.json may appear only inside the `DENY_RULES` constant (up to its closing `];`).
m=$(scan -rlF 'credentials.json' crates src-tauri/src |
    while IFS= read -r f; do
        awk '/(const|static)[[:space:]]+DENY_RULES/ { inside = 1 }
             inside { if (/\];/) inside = 0; next }
             /credentials\.json/ { print FILENAME ":" FNR ": " $0 }' "$f" || exit 2
    done)
hits "credentials.json outside DENY_RULES" "$m"

# CLAUDE_CODE_OAUTH_TOKEN may appear only in a comment in atm-core's claude.rs (passthrough note).
m=$(scan -rnF 'CLAUDE_CODE_OAUTH_TOKEN' crates src-tauri/src |
    scan -vE '^crates/atm-core/src/claude\.rs:[0-9]+:[[:space:]]*//')
hits "CLAUDE_CODE_OAUTH_TOKEN" "$m"

m=$(scan -nE '"csp"[[:space:]]*:[[:space:]]*null|"devtools"[[:space:]]*:[[:space:]]*true' \
    src-tauri/tauri.conf.json)
hits "tauri.conf.json" "$m"

if [[ $fail -ne 0 ]]; then
    echo "check.sh: security greps FAILED" >&2
    exit 1
fi
echo "check.sh: OK"
