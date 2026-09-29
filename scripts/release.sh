#!/usr/bin/env bash
# Release build (spec §11.2 M6): `cargo tauri build` makes target/release/bundle/macos/AI Task
# Manager.app and target/release/bundle/dmg/AI Task Manager_<version>_aarch64.dmg (not signed,
# not notarized: see the README for Gatekeeper), then checks what was built:
# - the release WASM (the `ui/dist` the build has just embedded) and the release binary carry
#   none of the selftest/E2E drivers and debug commands, whose names and strings `strings`
#   would show (the UI drivers exist only with `--features testkit`, the backend's only under
#   `cfg(debug_assertions)`);
# - the bundle's identifier, version and icon, and the .dmg's checksum (`hdiutil verify`).
# stdout: the paths of the .app and the .dmg. Exits non-zero on any failure.
#
#   scripts/release.sh               # build + checks
#   scripts/release.sh --no-build    # checks of the last build only
#
# The .dmg gets Finder's default window (the app and a link to Applications): the bundler's
# AppleScript that lays it out drives Finder, which needs the Automation permission for the
# terminal and otherwise times out (-1712) and fails the build, so `CI=true` makes the bundler
# skip it. `ATM_DMG_LAYOUT=1 scripts/release.sh` runs it (macOS asks for the permission once).
set -euo pipefail
cd "$(dirname "$0")/.."

build=1
case ${1:-} in
"") ;;
--no-build) build=0 ;;
*)
    echo "usage: scripts/release.sh [--no-build]" >&2
    exit 2
    ;;
esac

if ((build)); then
    # The default beforeBuildCommand: `trunk build --release`, without `testkit`.
    if [[ ${ATM_DMG_LAYOUT:-} == 1 ]]; then
        cargo tauri build -- --locked >&2
    else
        CI=true cargo tauri build -- --locked >&2
    fi
fi

bundle=target/release/bundle
app="$bundle/macos/AI Task Manager.app"
bin="$app/Contents/MacOS/ai-task-manager"
shopt -s nullglob
dmgs=("$bundle"/dmg/*.dmg)
wasm=(ui/dist/*_bg.wasm)
js=(ui/dist/*.js)
shopt -u nullglob
fail() {
    echo "release: FAILED ($1)" >&2
    exit 1
}
[[ -x $bin ]] || fail "$bin missing"
((${#dmgs[@]} == 1)) || fail "expected one .dmg in $bundle/dmg, found ${#dmgs[@]}"
((${#wasm[@]} == 1 && ${#js[@]} == 1)) || fail "expected one WASM and one JS in ui/dist"
dmg=${dmgs[0]}
# The WASM checked must be the one this build embedded: a later `trunk build --features testkit`
# (or a testkit bundle) would have replaced it, and would be newer than the binary.
[[ ! ${wasm[0]} -nt $bin ]] || fail "ui/dist is newer than the binary: rerun without --no-build"

# Names of the debug commands and of the drivers' state, their test data, their environment.
needles=(debug_e2e debug_selftest debug_ping debug_channel_probe debug_forwarder_count
    atm-e2e-state atm-selftest-first-load 'fake:approval' 'fake:flood' ATM_E2E ATM_SELFTEST
    securitypolicyviolation 'login simulato')
found=
for file in "${wasm[0]}" "${js[0]}" "$bin"; do
    text=$(strings -a "$file")
    for needle in "${needles[@]}"; do
        if grep -qF -- "$needle" <<<"$text"; then
            found+="$file: $needle"$'\n'
        fi
    done
done
[[ -z $found ]] || fail "test drivers in the release build:"$'\n'"$found"
echo "release: no test driver in $(basename "${wasm[0]}") ($(wc -c <"${wasm[0]}" | tr -d ' ') bytes)," \
    "$(basename "${js[0]}") or the binary (${#needles[@]} strings checked)" >&2

plist="$app/Contents/Info.plist"
id=$(plutil -extract CFBundleIdentifier raw "$plist")
version=$(plutil -extract CFBundleShortVersionString raw "$plist")
icon=$(plutil -extract CFBundleIconFile raw "$plist")
[[ $id == dev.aitaskmanager.desktop ]] || fail "bundle identifier $id"
[[ -s "$app/Contents/Resources/$icon" ]] || fail "icon $icon missing"
hdiutil verify -quiet "$dmg" || fail "hdiutil verify $dmg"
echo "release: $id $version, icon $icon, $(du -sh "$app" | cut -f1) app, $(du -h "$dmg" | cut -f1) dmg" >&2
echo "$PWD/$app"
echo "$PWD/$dmg"
