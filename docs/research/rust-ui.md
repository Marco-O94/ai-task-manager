# rust-ui.com: what it targets as of 27 Sep 2026

## Summary

- **rust-ui.com now targets Dioxus 0.7.** The maintainer made Dioxus the main framework on **2026-09-06**, after a poll that ended 108 to 105. The Leptos version is still maintained, but it moved to **leptos.rust-ui.com** and the repo **github.com/rust-ui/leptos-ui**.
- **The "Leptos only, Dioxus is future" text is out of date.** The installation page on rust-ui.com (`public/docs/installation.md` in the repo) and its `llms.txt` were copied from the Leptos site and never updated. That is why the homepage and the installation page disagree.
- **`ui add` (installing components with the CLI) looks broken for both frameworks today.** The latest CLI is `ui-cli` 0.3.16, released 2026-06-19. I checked the URLs it builds and they return 404:
  - Leptos: it fetches `https://www.rust-ui.com/registry/styles/default/<name>.md`, which redirects to rust-ui.com and returns 404. The same file works at `leptos.rust-ui.com`.
  - Dioxus: it fetches `https://dioxus.rust-ui.com/registry/...`, which redirects to rust-ui.com, and only the 35 `use_*` hooks are published there. Every component returns 404.
  - I did not run the CLI (the task is read-only). This conclusion comes from reading the published source plus the HTTP checks.
  - For a Leptos project it is worse than a clean failure: `tree.md` at rust-ui.com is the Dioxus tree, so `ui list` or `ui add use_*` could write **Dioxus hook code into a Leptos project** (not run).
- **Workaround: copy the component source by hand.**
  - Leptos: `https://leptos.rust-ui.com/registry/styles/default/<name>.md` (copy the rust code block), or `github.com/rust-ui/leptos-ui/tree/main/app_crates/registry/src/ui/*.rs`.
  - Dioxus: `github.com/rust-ui/ui/tree/main/app_crates/registry/src/ui/*.rs`.

## How the project split

| | Dioxus (main) | Leptos (secondary, maintained) |
|---|---|---|
| Site | https://rust-ui.com (`www.` and `dioxus.` both redirect here) | https://leptos.rust-ui.com |
| Repo | github.com/rust-ui/ui (README title "Rust/UI — Dioxus", "Early stage, experimental") | github.com/rust-ui/leptos-ui (also a git submodule of rust-ui/ui) |
| Framework version | `dioxus = "=0.7.9"` (router, fullstack) in the site; the registry crate declares `dioxus = "0.7"` | `leptos 0.8` + `leptos_axum 0.8` + `axum 0.8` (the site itself is SSR on Axum, built with cargo-leptos) |
| Toolchain | stable + wasm32 | **nightly** (leptos-ui and all Leptos starters pin `channel = "nightly"`) |
| Registry | `rust-ui.com/registry/tree.md` (hooks only); components about 87 files in `app_crates/registry/src/ui` | `leptos.rust-ui.com/registry/{tree.md,styles/default/*.md}`, about 90 components |

The maintainer confirmed the split in issue #52 (2026-09-14): "`leptos-ui` repo keeps full Leptos + Tauri support, maintained going forward. The main `rust-ui` framework … is shifting to Dioxus."

## Current versions (from crates.io, 2026-09-27)

| Crate | Version | Notes |
|---|---|---|
| `ui-cli` | 0.3.16 (2026-06-19) | binary is `ui` |
| `leptos` | 0.8.21 | 0.9.0-beta2 also out |
| `leptos_axum` | 0.8.10 | |
| `cargo-leptos` | 0.3.10 | |
| `dioxus` / `dioxus-cli` | 0.7.10 | 0.8.0-alpha.1 also out |
| `leptos_ui` | 0.3.22 | depends on `leptos ^0.8` with **features=["nightly"]** and `tw_merge ^0.1` with `variant` |
| `tw_merge` | 0.1.22 | features `variant`, `debug` |
| `icons` | 0.19.0 | features `leptos`, `leptos_animated`, `dioxus`, `dioxus_animated`; starters still use 0.18 |

- rust-ui/ui pins `dioxus =0.7.9`, with a comment saying `dioxus-desktop` 0.7.10 is not on crates.io. crates.io shows `dioxus-desktop` 0.7.10 published 2026-07-30 and not yanked, so the comment seems out of date.
- About nightly: Leptos 0.8.21 only turns on its unstable features when the compiler is actually nightly (`cfg_attr(all(feature="nightly", rustc_nightly), …)`). The CLI 0.3.15 changelog also says "Leptos works on stable". Whether **all** rust-ui components compile on stable is UNVERIFIED; every official Leptos starter uses nightly.

## CLI `ui-cli` 0.3.16

Commands, from the README and changelog:
```
cargo install ui-cli --force
ui starters | ui init [-y|--yes] [-f|--force] [--reinstall] [--rtl|--no-rtl]
ui add [names…] [-n|--dry-run] [-y|--yes] [--path <dir>]   # no args = ratatui picker
ui list [--json] | ui search <q> [--json] | ui view <name> [--json]
ui diff [name] [--json] | ui update [--json] | ui info [--json] | ui docs
ui mcp | ui mcp init --client <claude|cursor|vscode|opencode>
```

**Which framework it picks:** Dioxus if `dioxus` is in `[dependencies]` or `[workspace.dependencies]`, otherwise Leptos. If neither is present, it stops with "No supported framework (leptos/dioxus) found".

**What `ui init` does:**
1. Asks for a base colour (Neutral, Stone, Zinc, Mauve, Olive, Mist, Taupe) and an accent colour.
2. Writes `ui_config.toml` with `base_color`, `color_theme`, `base_path_components` and `rtl`.
3. Merges `"type": "module"` into `package.json`.
4. Writes the Tailwind v4 CSS file.
5. Adds crates with `cargo add`. It skips the framework crate if it is already there, and uses `[workspace.dependencies]` when the project has one.
   - Leptos: `leptos` (csr), `tw_merge` (variant), `icons` (leptos), `leptos_ui`.
   - Dioxus: `dioxus` (router, fullstack), `tw_merge` (debug), `icons` (dioxus).
6. Runs `pnpm install` (or `npm`) for `@tailwindcss/cli tailwindcss tw-animate-css`.

**Where it writes the CSS file:**
- Leptos: it reads `tailwind-input-file` from `[package.metadata.leptos]` or `[[workspace.metadata.leptos]]`, which is the cargo-leptos config. If the key is missing, `ui init` fails.
- Dioxus: the file must **already exist** at `style/tailwind.css`, `assets/tailwind.css` or `tailwind.css`.

**What the CSS file contains:** `@import "tailwindcss"; @import "tw-animate-css";`, then OKLCH `:root` and `.dark` variables (`--radius: 0.625rem`), an `@theme inline {--color-background: var(--background) …}` block, and `@layer base`. The starters also add `@custom-variant dark (&:is(.dark *));`. The Dioxus starter and the rust-ui site add `@source "../src/**/*.rs";`, which you need so Tailwind scans the `.rs` files.

**Where `ui add` puts files:**
- `src/components/ui/<name>.rs`, `src/components/hooks/<name>.rs`, `src/components/demos/<name>.rs`. In a workspace this becomes `<member>/src/components/...`.
- It updates `src/components/mod.rs` (`pub mod ui;` …) and `ui/mod.rs` (`pub mod <name>;`).
- It adds `mod components;` at the top of `main.rs` or `lib.rs`.
- It adds each component's cargo dependencies and downloads any JS dependency into `public/`.

**What the component code looks like:**
- Leptos: `use leptos::prelude::*; use leptos_ui::{clx, variants};` plus `tw_merge::tw_merge!`. Some components also need `icons/leptos` or `strum`, and `sidenav` needs `leptos_router`.
- Dioxus: `use dioxus::prelude::*; use tw_merge::tw_merge;`, with `rsx!` and enums that have an `as_str()` method.

**`ui starters`:** 0.3.16 offers only `tauri` and `tauri-fullstack`, and it runs `git clone https://github.com/rust-ui/start-<name>.git`. The `start-dioxus-fullstack` README says it is available through `ui starters`, but it is **not in the CLI menu**.

## Which requested components exist

| Requested | Leptos | Dioxus | Notes |
|---|---|---|---|
| button, card, badge | Y | Y | |
| dialog | Y (+alert_dialog) | Y | Leptos deps: icons, button, use_random |
| sheet / drawer | Y / Y | Y / Y | Leptos drawer's tree entry still lists `js: /app_components/vaul_drawer.js`, even though the changelog says the drawer was ported to Rust |
| tabs | Y | Y | |
| sidebar | Y (named **`sidenav`**) | Y | Leptos deps: leptos_router, strum/derive; Ctrl/Cmd+B shortcut |
| textarea, input | Y (+input_group) | Y | |
| select | Y (+select_native, multi_select) | Y (+multi_select) | |
| dropdown menu | Y | Y | |
| toast / sonner | **sonner** (`toast` is deprecated, and the CLI redirects to sonner) | sonner (+toast_custom) | |
| scroll area, skeleton | Y | Y | |
| drag and drop / sortable | `drag_and_drop` | same | Reorders DOM elements directly inside `.dragabble__container`. There is **no on_drop or state callback**, so app state does not follow the move. In Dioxus the logic is `#[cfg(target_arch="wasm32")]`, so it does nothing on desktop. |
| data table | Y (+data_grid) | Y (+data_grid) | |
| resizable panels | **No component**, only a CSS-only `demo_resizable` | **No component**, only a `use_resizable` hook | |
| command palette | Y (`command`, `demo_command_dialog`) | Y | |
| avatar, tooltip, separator, alert, spinner | Y | Y | `callout` also exists |
| code / markdown block | **No** | **No** | The site's markdown and syntax highlighting is internal, not in the registry |

Extras useful for an agent-monitoring UI:
- In both: `message`, `bubble`, `marker` (event rows for chat transcripts, with shimmer or spinner), `input_prompt`, `attachment`, `status`, `stepper`, `kbd`, `empty`, `collapsible`, `hover_card`, `popover`, `progress`, `shimmer`.
- Leptos only: **`chat`**.
- Dioxus only: `workflow` (+ `use_workflow`) and `charts`.

## Leptos SSR + Axum through cargo-leptos

**Yes, this works.** The Leptos site itself is Leptos 0.8 SSR + `leptos_axum` built with cargo-leptos. Both Leptos starters are cargo-leptos workspaces with an `app` crate (csr/hydrate/ssr features) and a `server` crate (`app` with the `ssr` feature), using Axum 0.8, `[[workspace.metadata.leptos]]` with `tailwind-input-file = "style/tailwind.css"`, `lib-features = ["hydrate"]` and `bin-default-features = false`.

There is **no rust-ui starter built on start-axum**. Issue #55, requesting an SSR+hydration starter, is open with no reply.

To use the official leptos-rs `start-axum` template instead: its `[package.metadata.leptos]` only has `style-file = "style/main.scss"`, so you must add `tailwind-input-file = "style/tailwind.css"` before `ui init`. Whether cargo-leptos handles `style-file` and `tailwind-input-file` together cleanly is UNVERIFIED.

## Dioxus fullstack and desktop

- The rust-ui/ui site builds for web fullstack, desktop and iOS:
  - `dx serve --web --fullstack`
  - `dx serve --platform desktop`
  - `dx serve --platform ios`
- `dx serve` starts the Tailwind watcher itself when there is a root `tailwind.css`.
- Components that need the browser are gated on wasm32. Their behaviour is a no-op on desktop, for example drag-and-drop and the sidenav shortcut.
- Official alternative, not rust-ui: DioxusLabs/components (`dioxus-primitives`), installed with `dx components add button`.

## Official starters and example apps

| Repo | Stack | In `ui starters` |
|---|---|---|
| rust-ui/start-tauri | Leptos 0.8 nightly, Axum 0.8, cargo-leptos, Tauri | yes |
| rust-ui/start-tauri-fullstack | same + sqlx Postgres + migrations + `just reset_db` | yes |
| rust-ui/start-dioxus-fullstack | Dioxus 0.7 fullstack, sqlx Postgres, nightly, `pnpm css` + `dx serve` | **no** (clone by hand) |

Reference apps: rust-ui/leptos-ui (Leptos SSR, Axum, Tauri) and rust-ui/ui (Dioxus fullstack, desktop, iOS). There is also rust-ui/termui (Ratatui).

## Exact commands (not executed)

**Leptos track:**
```bash
rustup toolchain install nightly --target wasm32-unknown-unknown
cargo install --locked cargo-leptos && cargo install ui-cli --force
ui starters                      # → tauri-fullstack; or:
cargo leptos new --git https://github.com/leptos-rs/start-axum   # then add tailwind-input-file to [package.metadata.leptos]
ui init --yes
ui add button card …             # currently 404 → copy from leptos.rust-ui.com/registry/styles/default/<n>.md into src/components/ui/<n>.rs
cargo leptos watch
```

**Dioxus track:**
```bash
cargo install dioxus-cli --locked && cargo install ui-cli --force
git clone https://github.com/rust-ui/start-dioxus-fullstack.git && cd start-dioxus-fullstack && pnpm install
ui init --yes                    # needs style/ | assets/ | ./tailwind.css to already exist
ui add …                         # currently 404 for components → copy from github.com/rust-ui/ui app_crates/registry/src/ui/*.rs
pnpm css & dx serve              # or: dx serve --platform desktop
```

## Local machine (read-only check)

- `/Users/marcooliveri/Desktop/Repositories/ai-task-manager` is empty.
- Rust: rustc/cargo 1.97.1 stable, plus 1.87 and 1.94 toolchains. **No nightly.**
- Not installed: `ui`, `dx`, `cargo-leptos`, `just`, `tailwindcss`.
- Installed: pnpm 11.25.0, npm 11.19.1, node v26.8.2.

## UNVERIFIED

- That `ui add` actually fails at runtime. This is inferred from the source and HTTP checks; I did not run the CLI.
- That every rust-ui component compiles on stable Rust.
- Whether cargo-leptos accepts `style-file` and `tailwind-input-file` together.
- Which Dioxus components lose behaviour on desktop beyond the wasm32-gated ones listed above.
- Whether the drawer still needs its JS file.

## Sources

- https://rust-ui.com/ · https://rust-ui.com/docs/components/installation · https://rust-ui.com/docs/components/cli · https://rust-ui.com/docs/components/button · https://rust-ui.com/llms.txt · https://rust-ui.com/registry/tree.md
- https://leptos.rust-ui.com/ · https://leptos.rust-ui.com/registry/tree.md · https://leptos.rust-ui.com/registry/styles/default/button.md
- https://github.com/rust-ui/ui (README, Cargo.toml, Dioxus.toml, .gitmodules, package.json, public/docs/*.md, app_crates/registry, crates/ui-cli incl. PLAN_HANDLE_DIOXUS.md)
- https://github.com/rust-ui/leptos-ui (README, Cargo.toml, PLAN_MOVE_LEPTOS_SITE_PHASE_1..3.md)
- https://github.com/rust-ui/ui/discussions/49 · https://github.com/rust-ui/ui/issues/52 · /issues/41 · /issues/55
- https://crates.io/crates/ui-cli, plus the source of the published crate https://static.crates.io/crates/ui-cli/ui-cli-0.3.16.crate
- https://crates.io/crates/leptos_ui · /tw_merge · /icons · /leptos · /leptos_axum · /dioxus · /dioxus-desktop · /cargo-leptos
- https://github.com/rust-ui/start-tauri · /start-tauri-fullstack · /start-dioxus-fullstack
- https://github.com/leptos-rs/start-axum/blob/main/Cargo.toml · https://github.com/leptos-rs/cargo-leptos (README, tailwind section)
- https://github.com/DioxusLabs/components