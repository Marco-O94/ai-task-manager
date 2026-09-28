# Leptos vs Dioxus fullstack for a local-first vibe-kanban clone with Rust/UI (as of 2026-09-27)

## 0. TL;DR

- **Rust/UI changed direction.** On 2026-09-06 its maintainer made **Dioxus the main framework**. `www.rust-ui.com` now serves the Dioxus site. The Leptos registry moved to **`leptos.rust-ui.com`** and its code lives in `rust-ui/leptos-ui`, which is still maintained (last commit 2026-09-22).
- **Current registry state:**
  - The Leptos registry is complete: about 89 UI components and 441 entries, including `drag_and_drop`.
  - The Dioxus port calls itself "Early stage, experimental". Its CLI registry (`tree.md`) lists **only hooks, no UI components** right now. The component source is only shown on the website, for copy-paste.
- **`ui-cli` 0.3.16 (latest, 2026-06-19) is broken for Leptos in practice.** It sends Leptos requests to `https://www.rust-ui.com/registry`, which now hosts the Dioxus site. `.../styles/default/button.md` returns **404** there and returns 200 on `leptos.rust-ui.com`. So `ui add button` in a Leptos project will most likely fail (inferred from probing the endpoints; I did not run the CLI end-to-end — UNVERIFIED). Workaround: copy components from `leptos.rust-ui.com`, or fetch `https://leptos.rust-ui.com/registry/styles/default/<name>.md`.
- **Recommendation: Leptos 0.8.21 + cargo-leptos 0.3.10 + the Rust/UI Leptos registry.** The call is close. Leptos wins today on three hard requirements:
  1. A single binary with embedded assets. There is an official rust-embed example, and `--frontend-only` / `--server-only` build flags. The SSR shell is generated in Rust, so no `index.html` has to sit on disk.
  2. A complete component registry, including drag-and-drop.
  3. A Leptos + Tauri fullstack starter from Rust/UI itself.
- **Choose Dioxus 0.7.10 instead** if you would rather follow Rust/UI's future direction and much better hot reload (RSX hot reload plus Subsecond hot-patching, native SSE). The costs: the Rust/UI port is experimental, and you would have to engineer single-binary asset embedding yourself.

## 1. What is installed locally (read-only checks)

| Check | Result |
|---|---|
| `rustup target list --installed` | `aarch64-apple-darwin` only. **`wasm32-unknown-unknown` is missing.** |
| `cargo install --list` | empty (no cargo-installed binaries) |
| `which cargo-leptos dx tailwindcss` | all **not found** |
| Also not found | `wasm-bindgen`, `wasm-opt`, `trunk`, `cargo-generate`, `cargo-binstall`, `sqlx` (cli), `just`, `sass`, `lld` |
| rustc / cargo | 1.97.1 stable (2026-07-14). Other toolchains: 1.87, 1.94 |
| Present | node v26.8.2, npm, npx, pnpm (`~/.npm-global/bin`), Homebrew, Xcode (`/Applications/Xcode.app`, needed for Tauri on macOS) |
| Project dir | empty, not a git repo |

Nothing was installed or changed.

## 2. Latest versions (crates.io and npm, checked 2026-09-27)

| Crate / tool | Latest stable | Pre-release | Notes |
|---|---|---|---|
| leptos | **0.8.21** (2026-09-26) | 0.9.0-beta2 (2026-09-26) | 0.9: edition 2024, `SignalOrFn`, `lazy` feature, serde_qs 1.0 |
| leptos_axum | **0.8.10** (2026-06-25) | 0.9.0-beta2 | depends on axum ^0.8, **tower-http ^0.6**, leptos ^0.8.20 |
| leptos_router / leptos_meta | 0.8.16 / 0.8.7 | | |
| server_fn | 0.8.13 | 0.9.0-beta2 | |
| cargo-leptos | **0.3.10** (2026-09-26) | | MSRV 1.82 |
| leptos-use | 0.19.2 | | has `use_event_source`, `use_websocket`, `use_draggable`, `use_drop_zone` |
| dioxus / dioxus-fullstack / dioxus-cli | **0.7.10** (2026-07-30) | 0.8.0-alpha.1 (2026-07-31) | 0.8 alpha moves to edition 2024; props become `#[non_exhaustive]` |
| dioxus-desktop | 0.7.10 (published 2026-07-30) | | Rust/UI pins `=0.7.9`, claiming 0.7.10 was unavailable. That comment looks stale. |
| axum | 0.8.9 | | |
| tower-http | 0.7.1 | | **Pin 0.6.x.** Both leptos_axum and dioxus-fullstack require ^0.6. |
| tokio | 1.53.1 | | |
| sqlx | **0.9.0** (2026-05-21) | | MSRV 1.94. Feature `sqlite` = `sqlite-bundled` + deserialize, load-extension and unlock-notify. Defaults: any, macros, migrate, json |
| wasm-bindgen | 0.2.129 (2026-09-25) | | CLI and crate versions must match exactly |
| rust-embed | 8.12.0 | | memory-serve 2.4.0 is an alternative (compatibility UNVERIFIED) |
| tauri | **2.12.0** (2026-09-26) | 3.0.0-alpha.3 | |
| tailwindcss (npm) | 4.3.3 | | cargo-leptos defaults to **v4.2.1**; dx 0.7.10 hard-codes **v4.1.5** |
| ui-cli (Rust/UI) | 0.3.16 (2026-06-19) | | has a `Framework { Leptos, Dioxus }` enum; see the URL bug above |
| leptos_ui / tw_merge / icons | 0.3.22 / 0.1.22 / 0.19.0 | | Rust/UI starters still use icons 0.18 |

## 3. Rust/UI status in detail

- **Decision thread:** discussion #49 ended 108 to 105. The maintainer then wrote: "Going with Dioxus as the main framework going forward". The reasons given were hot reload, fewer hydration problems, cross-platform support, and traction.
- **`rust-ui/ui`** (main repo, 667 stars) is now "Rust/UI — Dioxus". It uses Dioxus 0.7 and Tailwind v4, has 95 `.rs` files under `registry/src/ui` including `drag_and_drop.rs`, and has `leptos-ui` as a git submodule.
- **Endpoints:**
  - `www.rust-ui.com` and `dioxus.rust-ui.com` both serve the Dioxus build (the pages contain `dx_hydrate`). Their `/registry/tree.md` is 1.9 KB and lists hooks only.
  - `leptos.rust-ui.com/registry/tree.md` is 49 KB and complete.
  - The Button docs page on `www` shows `rsx!` code and no `ui add` instruction. The Leptos page shows `view!` and `ui add button`.
- **CLI commands:** `ui starters | init | add | list | search | view | diff | update | mcp`. `ui mcp init --client claude` wires the registry into Claude Code over MCP. Framework detection reads `Cargo.toml`.
- **Drag-and-drop component (both ports):** it attaches document-level `dragstart`/`dragover` listeners and calls `container.insert_before` directly. That changes the DOM behind the framework's reactive system. For a kanban board whose order is stored in the database, write signal-driven DnD yourself instead (see §5).
- **Starters:**
  - `start-tauri-fullstack` (Leptos + Tauri, workspace `app/` + `server/`, uses `[[workspace.metadata.leptos]]` and `tailwind-input-file`).
  - `start-dioxus-fullstack` (Dioxus 0.7, `server`/`web`/`mobile` features).
  - Both assume Postgres and nightly Rust.

## 4. Comparison by requirement

| Requirement | Leptos 0.8 + cargo-leptos | Dioxus 0.7 fullstack |
|---|---|---|
| Axum backend | `leptos_axum` `.leptos_routes(...)` on your own `axum::Router`; state via `provide_context` / `with_state` | `dioxus::serve(\|\| async { Ok(dioxus::server::router(app).route(...)) })`. Your own routes take priority because SSR is the fallback handler. `Lazy<T>` for globals such as a `SqlitePool` |
| Server functions | `#[server]` with codecs (Json, Cbor, Postcard, Bitcode, Rkyv, GetUrl, MultipartFormData…). Since 0.8.19 they respect Axum body limits (2 MB default), so raise `DefaultBodyLimit` if needed | `#[server]` or REST-style `#[get("/api/x/{id}?q")]`, `#[post]`…. Any Axum handler signature works; extractors can be hoisted |
| SQLite via sqlx | sqlx 0.9 as an optional dep behind the `ssr` feature; derive `FromRow` only under `ssr` | same pattern behind the `server` feature |
| **Native streaming** | (a) WebSocket server fn: `#[server(protocol = Websocket<JsonEncoding, JsonEncoding>)]` with `BoxedStream<T, ServerFnError>` in and out (bidirectional). (b) `#[server(output = StreamingText)] -> Result<TextStream, ServerFnError>` (chunked HTTP). (c) SSE is **not** a server_fn codec: use a plain `axum::response::Sse` route plus `leptos_use::use_event_source`. I found no type called `ServerFnWebSocket` (UNVERIFIED); the real type is `server_fn::Websocket` | `ServerEvents<T>`: native SSE, a `Stream` on the client with `.recv()`. `Websocket<In, Out, Enc>` with `WebSocketOptions::on_upgrade` and the `use_websocket` hook (JSON, CBOR or MsgPack). `Streaming<T>`, `TextStream`, `ByteStream`, `JsonStream`, `CborStream` |
| HTML5 DnD in WASM | raw `web_sys::DragEvent` through `on:dragstart` / `on:dragover` / `on:drop`, with `ev.data_transfer()` (needs web-sys features `DragEvent`, `DataTransfer`) | `ondragstart` etc. give `DragData` with `.data_transfer()` (dioxus-html 0.7.10). Crate `dioxus-dnd` 2.1+ offers typed payloads. Desktop issue #3961: DnD events do not fire on Linux (desktop only) |
| Tailwind v4 | `tailwind-input-file = "style/tailwind.css"`. cargo-leptos downloads the Tailwind binary itself (default v4.2.1, override with `LEPTOS_TAILWIND_VERSION=v4.3.3`), plus Lightning CSS | automatic when `tailwind.css` is at the crate root; outputs `assets/tailwind.css`. dx downloads **v4.1.5** (hard-coded; I did not find an override) |
| Hot reload | `cargo leptos watch`: parallel rebuild of server and wasm, browser live reload, CSS hot reload. `--hot-reload` is "partial… Requires rust nightly [beta]". Subsecond through `dx` is an experimental proof of concept, CSR-only, and a "low priority" | `dx serve`: instant RSX hot reload, plus Subsecond Rust hot-patching (`--hot-patch`, default in 0.8 alpha; needs `subsecond::call` checkpoints for server code). Clearly better |
| Release single binary | `cargo leptos build --release` produces the binary plus `target/site`. Embedding: the official `hackernews_islands_axum/src/fallback.rs` pattern with `#[derive(Embed)] #[folder = "target/site/"]` serves `.br`/`.gz` variants. Build the frontend first: `cargo leptos build --release --frontend-only && cargo leptos build --release --server-only`. First-class embed API is PR #4715, **open**, targeting 0.9 | `dx bundle --web --release` produces `target/dx/<app>/release/web/{server, public/}`. `serve_static_assets` reads `public/` next to the exe. **No built-in embedding.** You would need a two-pass build plus a custom static handler (UNVERIFIED feasibility) |
| Tauri 2 | Tauri's Leptos guide covers CSR/SSG with Trunk only: "Tauri doesn't officially support server based solutions." In practice, run Axum inside the Tauri process and point a `WebviewWindow` at `http://127.0.0.1:<port>`. vibe-kanban does this in `crates/tauri-app`, and Rust/UI's `start-tauri-fullstack` provides it (exact wiring UNVERIFIED) | Usually unnecessary: Dioxus has its own desktop renderer (wry). An embedded server needs its own tokio runtime on a separate thread plus `cfg` gating of server fns (maintainer, discussion #5031). Tauri wrapping would follow the same localhost pattern |
| Rust/UI fit | complete registry today; CLI URL bug | main framework going forward; registry and CLI incomplete |

## 5. Suggested architecture (Leptos path)

```
ai-task-manager/
  Cargo.toml        # [workspace] + [[workspace.metadata.leptos]] bin-package="server" lib-package="app" (or frontend)
  crates/domain     # shared types: serde only; #[cfg_attr(feature="ssr", derive(sqlx::FromRow))]; must compile to wasm
  crates/app        # components, pages, server fns; features: hydrate / ssr; src/components/ui/* (Rust/UI copies)
  crates/frontend   # cdylib hydrate entry (start-axum-workspace layout)
  crates/server     # axum main, SqlitePool, migrations, rust-embed of target/site, SSE routes
  crates/agent      # tokio::process runner (claude CLI), BufReader::lines -> broadcast::Sender<LogEvent>, persists to SQLite
  crates/desktop    # optional tauri 2.12 shell -> spawns server, opens webview on 127.0.0.1
  style/tailwind.css  migrations/
```

**Streaming.** Each agent run gets a `broadcast` channel, and every line is also written to a SQLite `run_logs` table so a page reload can replay history.
- Use SSE for the log tail: it reconnects automatically and can resume from `Last-Event-ID`.
- Use the WebSocket server fn when the UI must send data back to the agent (stdin, cancel, approve).

**Kanban DnD.**
- `on:dragstart` puts the task id into the DataTransfer.
- `on:dragover` calls `prevent_default`.
- `on:drop` calls a `move_task(id, column, pos)` server fn and updates a `Store` or signal. Render columns with keyed `<For>`.
- Do not use the Rust/UI `drag_and_drop` component, because it mutates the DOM directly.

## 6. Setup commands (none were run)

**Common**
- `rustup target add wasm32-unknown-unknown`
- Pin stable in `rust-toolchain.toml` and avoid nightly. Rust/UI's workspaces use `leptos` `features=["nightly"]`, but the components work on stable (ui-cli 0.3.15 changelog).

**Leptos**
- `cargo install --locked cargo-leptos` (0.3.10). It bundles cargo-generate 0.23 as a library, so no separate install is needed.
- Scaffold with `cargo leptos new --git https://github.com/leptos-rs/start-axum` (single crate; the template defaults to SCSS, so switch to `tailwind-input-file`) or `--git https://github.com/leptos-rs/start-axum-workspace`.
- Run with `cargo leptos watch`; release with `cargo leptos build --release [-P precompress] [--split]`.
- Tools download to `~/Library/Caches/cargo-leptos`. Defaults: wasm-opt `version_123`, sass 1.86.0.
- **wasm-bindgen:** cargo-leptos installs the CLI version that matches `Cargo.lock` only if no global `wasm-bindgen-cli` exists. A global install must be managed by hand. The template pins `wasm-bindgen = "0.2.106"`; switch to the version your lockfile resolves (0.2.129 today).

**Dioxus**
- Install dx: `curl -sSL https://dioxus.dev/install.sh | bash`, or `cargo binstall dioxus-cli --version 0.7.10 --force`, or `cargo install dioxus-cli --version 0.7.10 --locked`. The dx version must match the `dioxus` crate version.
- `dx new <name>` prompts for template (Bare-Bones / Jumpstart / Workspace), fullstack, router, Tailwind, LLMs.txt and platform.
- Run with `dx serve --web`; release with `dx bundle --web --release`.
- `dx doctor` checks the setup.
- dx manages wasm-bindgen-cli per project version: it downloads from GitHub, falls back to binstall, then to `cargo install`.

**Rust/UI:** `cargo install ui-cli --force`, then `ui init` and `ui add …` (subject to the URL bug above).

**Optional:** `cargo install sqlx-cli --no-default-features --features sqlite,rustls`, and `cargo install tauri-cli --version "^2" --locked`.

## 7. Gotchas

- **Hydration mismatches (Leptos):**
  - invalid HTML nesting (`<p><div>`, `<a><a>`, `<table>` without `<tbody>`);
  - anything non-deterministic during render (time, random, `localStorage`), which belongs in `Effect`;
  - rendering that differs under `cfg!(feature = "ssr")`;
  - DOM injected by browser extensions;
  - `<For>` ordering, fixed in 0.8.21.
- **Hydration (Dioxus):** use `use_server_future` for data needed during SSR. The maintainer says there are fewer mismatch problems (maintainer opinion, not independently verified).
- **Toolchain drift:** Dioxus issue #5766 reports that nightly ≥ 2026-08-06 (LLVM 23) breaks wasm-bindgen ("doesn't have an adapter listed"), cross-referenced to wasm-bindgen #5268. It is open and nightly-only; whether it reaches a future stable is UNVERIFIED. Stay on stable 1.97.x.
- **Version skew:** pin `tower-http = "0.6"`. Adding 0.7.1 creates duplicate, type-incompatible layers.
- **sqlx:** keep it out of the wasm build (optional dep behind `ssr` / `server`). `query!` needs `DATABASE_URL` or offline `.sqlx` data (`cargo sqlx prepare`). sqlx 0.9 needs Rust ≥ 1.94 (local 1.97.1 is fine).
- **Leptos runtime config when shipped as one binary:**
  - `get_configuration(None)` relies on env vars or `Cargo.toml` metadata. Build through cargo-leptos so `LEPTOS_OUTPUT_NAME` is set at compile time, and set `site_addr` in code (details UNVERIFIED).
  - Keep `hash-files = false`, or embed `hash.txt`. Otherwise the server reads `hash.txt` from beside the executable.
- **rust-embed:** in debug builds it reads from disk; only release embeds. The site folder must exist before the server compiles, so build the frontend first.
- **Tailwind v4 source detection:** the project is not a git repo. To make sure `target/` is not scanned, prefer `@import "tailwindcss" source(none); @source "../crates";`. Whether Tailwind honours `.gitignore` without git is UNVERIFIED.
- **Compile times:** Leptos typed `view!` trees produce deep generic types, and every save rebuilds both server and wasm. In dev use `RUSTFLAGS="--cfg erase_components"` (UNVERIFIED flag name) and `opt-level = 3` for dependencies. Dioxus avoids most rebuilds thanks to RSX hot reload.
- **Wasm size:** release profile `opt-level = 'z'`, `lto = true`, `codegen-units = 1`. cargo-leptos runs `wasm-opt -Oz` automatically; Leptos `--split` with `#[lazy]` and Dioxus wasm-split can split code. Typical sizes are UNVERIFIED.
- **Upcoming majors:** Leptos 0.9 (beta2) and Dioxus 0.8 (alpha.1) are both near. Pin exact versions (`=0.8.21` / `=0.7.10`).
- **Reference implementation:** vibe-kanban itself is an Axum 0.8 + sqlx SQLite + rust-embed + Tauri 2 workspace with a React frontend. Its stack pattern maps directly onto the Leptos path.

## Sources

- Crates: https://crates.io/crates/leptos , https://crates.io/crates/leptos_axum , https://crates.io/crates/cargo-leptos , https://crates.io/crates/dioxus , https://crates.io/crates/dioxus-cli , https://crates.io/crates/dioxus-desktop , https://crates.io/crates/sqlx , https://crates.io/crates/tauri , https://crates.io/crates/wasm-bindgen , https://crates.io/crates/rust-embed , https://crates.io/crates/ui-cli , https://crates.io/crates/leptos-use , https://crates.io/crates/server_fn
- Rust/UI: https://rust-ui.com/ , https://rust-ui.com/docs/components/installation , https://github.com/rust-ui/ui , https://github.com/rust-ui/ui/discussions/49 , https://github.com/rust-ui/leptos-ui , https://github.com/rust-ui/ui/tree/main/crates/ui-cli , https://leptos.rust-ui.com/registry/tree.md , https://www.rust-ui.com/registry/tree.md , https://github.com/rust-ui/start-tauri-fullstack , https://github.com/rust-ui/start-dioxus-fullstack
- Leptos: https://github.com/leptos-rs/leptos/releases/tag/v0.8.0 , https://github.com/leptos-rs/leptos/releases/tag/v0.8.21 , https://github.com/leptos-rs/leptos/releases/tag/v0.9.0-beta , https://github.com/leptos-rs/leptos/tree/main/examples/websocket , https://github.com/leptos-rs/leptos/blob/main/examples/server_fns_axum/src/app.rs , https://github.com/leptos-rs/leptos/blob/main/examples/hackernews_islands_axum/src/fallback.rs , https://github.com/leptos-rs/leptos/tree/main/examples/subsecond_hot_patch , https://github.com/leptos-rs/leptos/pull/4715 , https://github.com/leptos-rs/leptos/discussions/3028 , https://github.com/leptos-rs/cargo-leptos (README, `src/config/version.rs`, `src/config/cli.rs`) , https://github.com/leptos-rs/start-axum , https://docs.rs/server_fn/latest/server_fn/codec/index.html
- Dioxus: https://dioxuslabs.com/blog/release-070/ , https://github.com/DioxusLabs/dioxus/releases , https://dioxuslabs.com/learn/0.7/essentials/fullstack/websockets/ , https://dioxuslabs.com/learn/0.7/essentials/fullstack/axum/ , https://dioxuslabs.com/learn/0.7/essentials/fullstack/server_functions/ , https://dioxuslabs.com/learn/0.7/tutorial/deploy/ , https://dioxuslabs.com/learn/0.7/getting_started/ , https://docs.rs/dioxus-fullstack/latest/dioxus_fullstack/payloads/index.html , https://github.com/DioxusLabs/dioxus/blob/v0.7.10/examples/07-fullstack/server_sent_events.rs , https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/cli/src/tailwind.rs , https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/cli/src/wasm_bindgen.rs , https://docs.rs/dioxus-html/latest/dioxus_html/events/struct.DragData.html , https://github.com/DioxusLabs/dioxus/issues/5766 , https://github.com/DioxusLabs/dioxus/issues/3961 , https://github.com/DioxusLabs/dioxus/discussions/5031 , https://github.com/DioxusLabs/components
- Tauri / reference: https://tauri.app/start/frontend/leptos/ , https://github.com/BloopAI/vibe-kanban