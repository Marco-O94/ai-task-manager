# AI Task Manager: piano di implementazione definitivo

*Stack: Tauri 2 + Leptos 0.8 CSR + Rust/UI, con il Claude Code CLI dell'utente come agente. Redatto il 2026-09-27, in sola lettura.*

Legenda delle evidenze:
- **[V]** verificato oggi su questa macchina, oppure su crates.io o GitHub.
- **[F]** proviene dai fact sheet o dai report di ricerca.
- **[DA VERIFICARE → Mx]** non ancora confermato; la milestone indicata lo chiude.

---

## 0. Contesto

**Cosa costruiamo.** Un'app desktop macOS simile a Vibe Kanban.
- Si lavora su una kanban di task.
- Ogni task si manda al Claude Code CLI installato localmente dall'utente. Ogni tentativo gira nel proprio git worktree, su un branch dedicato.
- Si segue l'agente in tempo reale, si approvano i tool quando serve, si rivede il diff e si fa uno squash-merge nel branch target.

**Vincoli fissati dall'utente:**
- Tauri 2.
- Login solo tramite il CLI non modificato: `claude auth status` e `claude auth login`.
- Un solo agente in v1: Claude Code.
- Un worktree e un branch per ogni attempt.
- Componenti UI presi dal registry Rust/UI Leptos.
- Implementazione delegata a subagent Opus 5.5.

**Proposta di partenza.** Si parte dalla proposta **B (robust-runtime)**, vincitrice sulla media dei due giudici: B 7,73, C 7,55, A 7,33.
- Lo **scope** è stato ridotto come in A.
- **Delega, contratti congelati e mock UI** vengono da C.
- Sono stati applicati tutti gli innesti raccomandati e corretti tutti gli errori fattuali (§0.2).
- Il criterio guida è il design più semplice che funziona: 4 package più un binario di test, nessun trait per l'unico executor, nessuna astrazione speculativa.

### 0.1 Fatti verificati oggi (comandi di sola lettura)

| Fatto | Evidenza |
|---|---|
| Il CLI è `~/.local/bin/claude`, versione `2.1.283 (Claude Code)` | [V] `--version` |
| `--permission-mode default`, `--permission-mode=default`, `manual` e `acceptEdits` vengono accettati. `bogus` viene rifiutato con il messaggio *"Allowed choices are acceptEdits, auto, bypassPermissions, manual, dontAsk, plan"* | [V] |
| `--setting-sources`: *"Comma-separated list of setting sources to load (user, project, local)"*. `--strict-mcp-config`: *"Only use MCP servers from --mcp-config, ignoring all other MCP configurations"* | [V] `--help` |
| L'esempio di `--disallowedTools` e `--allowedTools` nell'help è `"Bash(git *) Edit"`: la sintassi delle regole con wildcard accetta spazi dentro le parentesi | [V] |
| `--effort` accetta: low, medium, high, xhigh, max. Gli alias di `--model` sono 'fable', 'opus', 'sonnet' | [V] |
| `claude auth status [--json\|--text]` (JSON è il default). `claude auth login` accetta `--claudeai` (default), `--console`, `--email`, `--sso` | [V] |
| `--permission-prompts host\|none` (default `host`) | [V] |
| git 2.54.0, sia Homebrew sia Apple `/usr/bin/git` | [V] |
| rust-ui/leptos-ui `style/tailwind.css` contiene 36 occorrenze di `--success/--warning/--info`. Importa anche file del sito (`index.css`, `Animate.css`, `highlight_code.css`), che **non** vanno copiati | [V] |
| `dialog.rs` importa `crate::hooks::use_random` e `crate::ui::button` e contiene un `<script>` inline. `sheet.rs` e `shimmer.rs` usano `crate::`. button, badge, card, tabs, collapsible, select_native, textarea, input, scroll_area, alert, spinner, tooltip, empty, separator, skeleton, label, kbd, callout, status, message, bubble, marker e chat non hanno né `<script>` né `crate::` | [V] |
| `hooks/use_scroll_lock.rs` espone `lock()` e `unlock()` richiamabili da Rust. `use_random` usa solo std | [V] |
| Trunk 0.21.14 ha `[watch] watch/ignore`, `features` in `[build]` e `[tools] tailwindcss/wasm_bindgen/wasm_opt` | [V] sorgente del tag |
| Versioni su crates.io: tauri 2.12.0, tauri-build 2.7.0, tauri-plugin-single-instance 2.5.0, tauri-plugin-dialog 2.8.0, rusqlite 0.40.2, tokio 1.53.1, libc 0.2.189, sha2 0.11.0, leptos 0.8.21, leptos_ui 0.3.22, tw_merge 0.1.22, icons 0.19.0, wasm-bindgen 0.2.129, web-sys/js-sys 0.3.106, wasm-bindgen-futures 0.4.79, insta 1.48.0, tempfile 3.27.0, thiserror 2.0.21 | [V] |

### 0.2 Correzioni rispetto alle proposte e ai giudici

| # | Errore | Correzione in questo piano |
|---|---|---|
| E1 | C: `default-members` nel Cargo.toml radice, che è anche il package UI. Trunk compilava così i crate nativi per wasm32 | La radice è un **workspace virtuale**. La UI sta in `ui/`, un membro non radice con i propri `Trunk.toml` e `index.html`. Trunk usa `--manifest-path ui/Cargo.toml`, quindi seleziona quel package |
| E2 | B: "`default` non è un valore valido, quindi il flag si omette". Giudice 2: "`default` viene rifiutato" | Entrambe le affermazioni sono false: `default` viene accettato [V]. `--permission-mode=<mode>` si passa **sempre**, a ogni spawn. Così si evita anche il ripristino di plan mode sul resume |
| E3 | B: i componenti Rust/UI copiati "as-is" in `components/ui/` | Vanno copiati in `ui/src/ui/*.rs` e `ui/src/hooks/*.rs`, in modo che `crate::ui::…` e `crate::hooks::…` si risolvano senza modifiche [V] |
| E4 | A e B: token da una "pagina di installazione" o da start-tauri, entrambe senza `--success/--warning/--info` | I token si prendono da **rust-ui/leptos-ui `style/tailwind.css`**, solo i blocchi `:root`, `.dark`, `@theme inline` e `@layer base` [V] |
| E5 | B: `LinesCodec` con `MaxLineLengthExceeded`, dove il loop `while let` si ferma | Un reader di righe con cap scritto a mano (§7.5), senza tokio-util |
| E6 | C: git dell'app senza `hooksPath` né `--no-textconv`, quindi codice del repo eseguito | Il runner git è irrobustito (§8.1) |
| E7 | C: riuso del `--session-id` dopo un turno morto prima di `init` | Nuovo UUID se `session_started=0` (§7.9) |
| E8 | C: `$SHELL -l -c` non carica `.zshrc` | `$SHELL -ilc` con marker di delimitazione; si importa solo `PATH` (§7.2) |
| E9 | A: rusqlite scelto dicendo che "sqlx richiede DATABASE_URL" | Il motivo reale: API sincrona, meno dipendenze, nessun pool async. Le query runtime di sqlx non richiedono `DATABASE_URL` |
| E10 | Report headless: `--strict-mcp-config` descritto come "exit on MCP failure" | Semantica corretta, verificata con `--help` (§0.1) |
| E11 | Non verificato: con `--setting-sources=user`, CLAUDE.md potrebbe non essere caricato | Mitigazione nell'append-prompt; verifica in M5 |

---

## 1. Decisioni

| # | Decisione | Motivazione |
|---|---|---|
| D1 | Tauri 2.12 con Leptos 0.8.21 CSR compilato da Trunk 0.21.14. Nessun server HTTP, WebSocket o MCP; nessun socket in ascolto | Tauri supporta Leptos solo in CSR/SSG [F]. Nessuna superficie di attacco su localhost |
| D2 | 4 package: `atm-types` (contratto serde, wasm e host), `atm-core` (tutta la logica, senza tipi Tauri), `src-tauri` (guscio sottile), `ui` (Leptos). `fake-claude` è un `[[bin]]` dentro `atm-core` | Pochi crate. Il core è testabile senza webview. Nessun trait executor |
| D3 | **Un processo `claude -p` per turno.** stdin resta aperto durante il turno (control protocol) e si chiude dopo `result`. Follow-up e ripresa dopo un riavvio usano `--resume=<uuid>` | Un attempt inattivo non costa nulla (~1 GiB per processo vivo [F]). Un solo percorso di codice per follow-up e recovery |
| D4 | Il transcript normalizzato si scrive in **SQLite prima** del `broadcast`. Il raw JSONL resta solo per diagnostica e fixture. Nessuna ri-normalizzazione in visualizzazione | Memoria limitata; nessun OOM come in Vibe Kanban [F]. Ordine snapshot + live corretto per costruzione (§6.5) |
| D5 | Sincronizzazione della board: un evento `changed{project_id, task_id}` fa fare refetch alla UI. Il transcript usa un `Channel` per vista, con snapshot iniziale e upsert idempotenti (`idx`, `rev`) | Elimina i bug di patch e riconnessione (Vibe Kanban #3343, #3227 [F]) |
| D6 | Permessi: default **Auto-edit (`acceptEdits`)**, con `--permission-prompt-tool stdio` e approvazioni in UI. **Supervisionato** = `default`. **Autonomo** = `bypassPermissions`, solo con opt-in per progetto, conferma nativa e `--allow-dangerously-skip-permissions`. Plan mode e AskUserQuestion sono rimandati | Meno approvazioni ripetitive. Un worktree non è una sandbox |
| D7 | Configurazione Claude del repo **Isolata** di default: `--setting-sources=user --strict-mcp-config`. "Trusted" richiede una conferma nativa e un fingerprint di `.claude/**` + `.mcp.json`, ricontrollato a ogni turno | Sotto `-p` i hook, `env`, `apiKeyHelper` e i server MCP del progetto partono senza trust dialog [F] |
| D8 | Auth: solo il login del CLI. Lo stato si legge con `claude auth status`; il login apre Terminal.app con `claude auth login`. `ANTHROPIC_API_KEY` e `ANTHROPIC_AUTH_TOKEN` vengono rimossi dall'env | Policy Anthropic [F]. Evita fatturazione API silenziosa |
| D9 | Git solo via CLI irrobustito. Squash merge con `merge-tree --write-tree` → `commit-tree` → `update-ref` CAS oppure `merge --ff-only`. Nessun rebase in v1 | Il checkout utente non resta mai mezzo mergiato. Un target divergente ma senza conflitti si mergia senza rebase |
| D10 | SQLite via **rusqlite 0.40.2 `bundled`**, un `Mutex<Connection>`, WAL, migrazioni con `PRAGMA user_version` | Il caso d'uso è piccolo e sincrono, e l'API è la più semplice |
| D11 | IPC con JSON testuale (`serde_json` ↔ `js_sys::JSON`), niente `serde-wasm-bindgen` | Formato identico a serde lato Rust; nessuna sorpresa su enum taggati o numeri |
| D12 | Rust/UI: si copiano a mano i componenti senza script; `dialog` viene **portato** (script rimosso, stato guidato da signal). Niente router: le viste cambiano via signal | `ui add` restituisce 404 [F]. La CSP stretta blocca gli script inline |
| D13 | UI sviluppabile in browser con la feature `mock` (`trunk serve --features mock`) | Frontend e backend procedono in parallelo |
| D14 | Target v1: solo macOS (`killpg`, `open`, `.command`) | Riduce lo scope. Linux funzionerebbe quasi tutto; Windows è rimandato |

---

## 2. Stack, versioni pinnate, setup toolchain

### 2.1 Versioni

| Area | Pin |
|---|---|
| Toolchain | rustc/cargo `1.97.1` stable (`rust-toolchain.toml`), target `wasm32-unknown-unknown`, clippy, rustfmt |
| Desktop | tauri `=2.12.0`, tauri-build `=2.7.0`, tauri-cli `2.12.0`, tauri-plugin-dialog `=2.8.0`, tauri-plugin-single-instance `=2.5.0` |
| Frontend | leptos `=0.8.21` (`csr`), leptos_ui `=0.3.22`, tw_merge `=0.1.22` (`variant`), icons `=0.19.0` (`leptos`), strum `0.27` (`derive`, come upstream [V]) |
| WASM | wasm-bindgen `=0.2.129` (uguale al CLI che scarica Trunk), wasm-bindgen-futures `0.4.79`, js-sys e web-sys `0.3.106`, console_error_panic_hook `0.1.7` |
| Build | trunk `0.21.14`; Tailwind standalone `4.3.3` via `[tools]`; `tw-animate.css` 1.4.0 **vendorizzato** (niente Node) |
| Backend | tokio `1.53.1`, rusqlite `0.40.2` (`bundled`), serde `1.0.229`, serde_json `1.0.151`, uuid `1.26.1` (`v4`), thiserror `2.0.21`, libc `0.2.189`, sha2 `0.11.0` |
| Test | insta `1.48.0` (`json`), tempfile `3.27.0` |
| Esterni | claude CLI: minimo `2.1.223`, testato `2.1.283`. git: minimo `2.38` (`merge-tree --write-tree`) |

`Cargo.lock` va nel repo.

### 2.2 Setup (eseguito in M0; nulla è stato installato durante il planning)

```bash
cd ~/Desktop/Repositories/ai-task-manager
git init -b main
rustup toolchain install 1.97.1 --profile minimal -c clippy -c rustfmt
rustup target add wasm32-unknown-unknown --toolchain 1.97.1
cargo install trunk --version 0.21.14 --locked          # [DA VERIFICARE → M0] build su 1.97.1
cargo install tauri-cli --version 2.12.0 --locked
mkdir -p ui/style/vendor
curl -fsSL https://unpkg.com/tw-animate-css@1.4.0/dist/tw-animate.css -o ui/style/vendor/tw-animate.css
cargo tauri init --help   # confermare i flag, poi generare src-tauri (icone, build.rs, capabilities) e adattarlo al §2.3
```

### 2.3 File di configurazione chiave

**`rust-toolchain.toml`**
```toml
[toolchain]
channel = "1.97.1"
components = ["clippy", "rustfmt"]
targets = ["wasm32-unknown-unknown"]
```

**`Cargo.toml` (radice, workspace virtuale)**
```toml
[workspace]
resolver = "3"
members = ["crates/atm-types", "crates/atm-core", "src-tauri", "ui"]
default-members = ["crates/atm-types", "crates/atm-core", "src-tauri"]   # ui: solo wasm32 via Trunk

[workspace.package]
edition = "2024"
rust-version = "1.90"
version = "0.1.0"

[workspace.dependencies]   # tutte dichiarate in M1; i pacchetti successivi NON toccano Cargo.toml né Cargo.lock
atm-types = { path = "crates/atm-types" }
atm-core  = { path = "crates/atm-core" }
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
thiserror = "2.0.21"
tokio = { version = "1.53.1", features = ["rt-multi-thread", "macros", "process", "io-util", "sync", "time", "fs"] }
rusqlite = { version = "0.40.2", features = ["bundled"] }
uuid = { version = "1.26.1", features = ["v4"] }
libc = "0.2.189"
sha2 = "0.11.0"
tauri = { version = "=2.12.0", features = [] }
tauri-build = "=2.7.0"
tauri-plugin-dialog = "=2.8.0"
tauri-plugin-single-instance = "=2.5.0"
leptos = { version = "=0.8.21", features = ["csr"] }
leptos_ui = "=0.3.22"
tw_merge = { version = "=0.1.22", features = ["variant"] }
icons = { version = "=0.19.0", features = ["leptos"] }
strum = { version = "0.27", features = ["derive"] }
wasm-bindgen = "=0.2.129"
wasm-bindgen-futures = "0.4.79"
js-sys = "0.3.106"
web-sys = { version = "0.3.106", features = ["DragEvent", "DataTransfer", "DomRect", "Element", "HtmlElement",
  "Window", "Document", "MediaQueryList", "KeyboardEvent", "SecurityPolicyViolationEvent"] }
console_error_panic_hook = "0.1.7"
insta = { version = "1.48.0", features = ["json"] }
tempfile = "3.27.0"

[profile.release]            # alla radice: cargo ignora i profili dei membri [F]
codegen-units = 1
lto = true
opt-level = 3
strip = true
# panic = "unwind" (default): un task in panic non abbatte l'app lasciando agenti orfani
[profile.release.package.atm-ui]
opt-level = "z"
```

**`ui/Trunk.toml`**
```toml
[build]
target = "index.html"
[watch]
watch = [".", "../crates/atm-types"]      # rebuild quando cambia il contratto
[serve]
port = 1420
open = false
[tools]
tailwindcss = "4.3.3"                     # Trunk 0.21.14 ha 3.3.5 come default [F]
wasm_bindgen = "0.2.129"
```

**`ui/index.html`.** Non deve contenere nessun `<style>`: Tauri aggiungerebbe un nonce e `'unsafe-inline'` smetterebbe di valere [F].
```html
<!DOCTYPE html>
<html lang="it"><head><meta charset="utf-8"/><title>AI Task Manager</title>
<link data-trunk rel="tailwind-css" href="style/tailwind.css"/>
<link data-trunk rel="copy-dir" href="public"/>
<link data-trunk rel="rust" data-wasm-opt="z"
      data-wasm-opt-params="--enable-bulk-memory --enable-nontrapping-float-to-int"/>
</head><body></body></html>
```
- Fallback, se wasm-opt fallisce: `data-wasm-opt="0"`. Va documentato in M0.

**`ui/style/tailwind.css`**
```css
@import "tailwindcss" source(none);
@import "./vendor/tw-animate.css";
@source "../src";
@custom-variant dark (&:is(.dark *));
/* + blocchi :root, .dark, @theme inline, @layer base da rust-ui/leptos-ui style/tailwind.css
   (commit annotato in ui/src/ui/VENDORED.toml). NON copiare i suoi @import di index.css, Animate.css, highlight_code.css */
```

**`src-tauri/tauri.conf.json`** (parti chiave)
```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "AI Task Manager",
  "identifier": "dev.aitaskmanager.desktop",
  "build": {
    "beforeDevCommand":   { "script": "trunk serve", "cwd": "../ui", "wait": false },
    "devUrl": "http://localhost:1420",
    "beforeBuildCommand": { "script": "trunk build --release", "cwd": "../ui" },
    "frontendDist": "../ui/dist"
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [{ "label": "main", "title": "AI Task Manager", "width": 1440, "height": 900,
                  "minWidth": 1024, "minHeight": 640, "dragDropEnabled": false }],
    "security": { "csp": {
      "default-src": "'self'",
      "script-src": "'self' 'wasm-unsafe-eval'",
      "style-src": "'self' 'unsafe-inline'",
      "img-src": "'self' data: blob:",
      "font-src": "'self' data:",
      "connect-src": "'self' ipc: http://ipc.localhost"
    } }
  },
  "bundle": { "active": true, "targets": ["app", "dmg"] }
}
```
- Il `cwd` si risolve relativo a `src-tauri`, perché tauri-cli fa `set_current_dir` lì (verificato dal giudice 1).
- `dragDropEnabled:false` serve a far funzionare il drag-and-drop HTML5.

**`src-tauri/capabilities/default.json`**
```json
{ "identifier": "default", "windows": ["main"], "permissions": ["core:default"] }
```
I plugin (dialog, single-instance) si chiamano solo da Rust, che non è vincolato dalle capability [F].

---

## 3. Architettura

```
┌───────────────────────────── AI Task Manager.app (1 processo, macOS) ──────────────────────────────┐
│ WKWebView ─ ui (Leptos CSR → WASM)                                                                  │
│   Onboarding │ Sidebar progetti │ Board kanban │ TaskPanel [Agente | Modifiche]                     │
│      │ invoke(cmd, {req})              ▲ event "changed" / "env_changed"   ▲ Channel<TranscriptMsg> │
│      ▼  (JSON)                         │ (refetch)                         │ (snapshot + upsert)    │
│ src-tauri (sottile): #[tauri::command] → Core · emit · Channel sink · dialog nativi · nav-guard ·   │
│                      single-instance · on_page_load(drop subscription) · ExitRequested(shutdown)    │
│      │                                                                                              │
│      ▼                                                                                              │
│ atm-core (tokio)                                                                                    │
│   lib.rs(servizi) ─ runner.rs(turni, approvazioni, stop) ─ live.rs(broadcast per attempt)           │
│   claude.rs(discovery/env/argv/auth/spawn) ─ wire.rs(NDJSON) ─ normalize.rs(puro) ─ git.rs ─ db.rs  │
└────────┬───────────────────────────────┬─────────────────────────────────┬──────────────────────────┘
         │ argv + pgid, stdin/stdout      │ git -c core.hooksPath=/dev/null │ rusqlite (WAL)
         ▼ NDJSON stream-json             ▼ …                               ▼
  claude -p (CLI dell'utente,        git CLI                          app_data_dir/
  1 processo per turno,              repo principale +                  atm.sqlite3
  cwd = worktree)                    ~/.ai-task-manager/worktrees/<id>  logs/<attempt>/<process>/*
  └ auth: login proprio del CLI (Keychain gestito SOLO dal CLI; l'app legge solo `claude auth status`)
```

**Flussi principali**
1. **Start.** `start_attempt` fa i preflight (env, login, pausa, cap), poi `git worktree add --lock` sotto il lock di repo, poi inserisce l'attempt e il process `running` (task → inprogress), poi `tokio::spawn(run_turn)`, e ritorna.
2. **Live.** Per ogni riga di stdout: `wire::parse`, poi il routing (control o normalizer), poi `db.upsert_entry`, poi `live.broadcast`. I forwarder inviano sul `Channel`, la UI applica `idx` e `rev`.
3. **Fine turno.** `result` → chiusura di stdin → EOF → `wait` → auto-commit → aggiornamento DB → `changed`.
4. **Merge.** Auto-commit → `merge-tree --write-tree` → `commit-tree` → `update-ref` CAS oppure `merge --ff-only` → DB → rimozione del worktree.

---

## 4. Layout del workspace

```
ai-task-manager/
├─ Cargo.toml  Cargo.lock  rust-toolchain.toml  .gitignore(/target /ui/dist)  README.md
├─ scripts/check.sh                       # fmt, clippy host+wasm, test, grep di sicurezza (§10.3)
├─ crates/
│  ├─ atm-types/  src/{lib.rs, model.rs, transcript.rs, review.rs, api.rs, error.rs, debug.rs}
│  │              # solo serde + serde_json; compila per wasm32 e host
│  └─ atm-core/
│     ├─ Cargo.toml                       # [lib] + [[bin]] fake-claude
│     ├─ migrations/0001_init.sql
│     ├─ src/lib.rs                       # Core, CoreConfig, startup/recovery/shutdown, 1 metodo per comando
│     ├─ src/db.rs                        # rusqlite: open, migrate, 1 fn per query, posizioni
│     ├─ src/git.rs                       # runner irrobustito + worktree/commit/diff/status/merge/fingerprint
│     ├─ src/claude.rs                    # discovery, versione, PATH login-shell, ChildEnv, argv, auth, login .command, spawn/killpg
│     ├─ src/wire.rs                      # reader di righe con cap, parse Inbound, frame outbound, approval_response()
│     ├─ src/normalize.rs                 # Normalizer puro (Value → EntryOp)
│     ├─ src/runner.rs                    # ciclo di vita del turno, approvazioni, stop, finalize
│     ├─ src/live.rs                      # broadcast per attempt, subscription e forwarder
│     ├─ src/bin/fake-claude.rs           # double del CLI (mai incluso nel bundle)
│     └─ tests/{common/mod.rs, db.rs, git.rs, claude.rs, normalize.rs, flow.rs, fixtures/**}
├─ src-tauri/
│  ├─ Cargo.toml build.rs tauri.conf.json capabilities/default.json icons/
│  └─ src/{main.rs, lib.rs, commands.rs, confirm.rs, selftest.rs}
└─ ui/                                    # package atm-ui (bin), feature `mock`
   ├─ Cargo.toml Trunk.toml index.html public/ style/{tailwind.css, vendor/tw-animate.css}
   └─ src/
      ├─ main.rs                          # mod ui; mod hooks; mod ipc; mod state; mod views; mod widgets; mount
      ├─ app.rs                           # AppCtx, gate onboarding, layout
      ├─ ipc/{mod.rs, tauri.rs, mock/{mod.rs, board.rs, attempt.rs, fixtures/*.json}}
      ├─ state/{board.rs, transcript.rs}
      ├─ views/{onboarding, sidebar, board, task_dialog, settings, task_panel, start_dialog,
      │         transcript, approval, composer, diff, merge_dialog}.rs
      ├─ widgets/{toast.rs, dnd.rs}
      ├─ ui/*.rs      + VENDORED.toml     # Rust/UI copiati (path imposto da crate::ui::…)
      └─ hooks/*.rs                       # use_random, use_scroll_lock
```

**Regole di dipendenza tra crate:**
- `atm-types` non dipende da nulla del progetto.
- `atm-core` dipende da `atm-types`.
- `src-tauri` dipende da `atm-core` e `atm-types`.
- `ui` dipende solo da `atm-types`.

**Directory a runtime:**

| Cosa | Percorso | Permessi |
|---|---|---|
| DB | `app_data_dir()/atm.sqlite3` | file 0600, dir 0700 |
| Log | `app_data_dir()/logs/<attempt>/<process>/{stdout.jsonl, stderr.log, stdin.jsonl}` | 0600 |
| Worktree | `~/.ai-task-manager/worktrees/<attempt_uuid>` | 0700; path senza spazi |
| Script di login | `app_cache_dir()/claude-login.command` | 0700, cancellato a fine polling |

---

## 5. Modello dati

### 5.1 Connessione e migrazioni

- A ogni apertura: `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA synchronous=NORMAL;`.
- Le migrazioni sono `&[include_str!(...)]`. Si applicano quelle con indice ≥ `PRAGMA user_version`, dentro una transazione.
- Gli ID sono UUID v4 minuscoli (`TEXT`); i timestamp sono `INTEGER` in ms Unix.
- `Mutex<Connection>`: tutte le chiamate sono brevi e sincrone, nessuna query dura più di pochi ms.

### 5.2 DDL (`crates/atm-core/migrations/0001_init.sql`)

```sql
CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL CHECK (json_valid(value))
) STRICT;

CREATE TABLE projects (
  id                      TEXT PRIMARY KEY,
  name                    TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
  repo_path               TEXT NOT NULL UNIQUE,          -- toplevel canonico del checkout principale
  default_target_branch   TEXT NOT NULL,
  default_permission_mode TEXT NOT NULL DEFAULT 'acceptEdits'
                          CHECK (default_permission_mode IN ('default','acceptEdits','bypassPermissions')),
  default_model           TEXT,
  config_policy           TEXT NOT NULL DEFAULT 'isolated' CHECK (config_policy IN ('isolated','trusted')),
  trusted_fingerprint     TEXT,                          -- sha256 hex (§8.9); NULL = mai approvato
  allow_bypass            INTEGER NOT NULL DEFAULT 0 CHECK (allow_bypass IN (0,1)),
  created_at              INTEGER NOT NULL,
  updated_at              INTEGER NOT NULL
) STRICT;

CREATE TABLE tasks (
  id          TEXT PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  title       TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
  description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 100000),
  status      TEXT NOT NULL DEFAULT 'todo'
              CHECK (status IN ('todo','inprogress','inreview','done','cancelled')),
  position    REAL NOT NULL,                             -- ordine nella colonna (gap 1024, midpoint)
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX tasks_board ON tasks(project_id, status, position);

CREATE TABLE attempts (
  id              TEXT PRIMARY KEY,
  task_id         TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  state           TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','merged','discarded')),
  branch          TEXT NOT NULL,                         -- atm/<8hex>-<slug>, immutabile
  target_branch   TEXT NOT NULL,
  base_commit     TEXT NOT NULL,                         -- tip del target alla creazione
  worktree_path   TEXT NOT NULL UNIQUE,                  -- canonico, immutabile
  worktree_state  TEXT NOT NULL DEFAULT 'present' CHECK (worktree_state IN ('present','removed','missing')),
  session_id      TEXT NOT NULL UNIQUE,                  -- UUID assegnato dall'app
  session_started INTEGER NOT NULL DEFAULT 0 CHECK (session_started IN (0,1)),
  permission_mode TEXT NOT NULL CHECK (permission_mode IN ('default','acceptEdits','bypassPermissions')),
  model           TEXT,
  effort          TEXT CHECK (effort IS NULL OR effort IN ('low','medium','high','xhigh','max')),
  allow_rules     TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(allow_rules)),  -- "consenti sempre" (§7.8)
  merge_commit    TEXT,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  closed_at       INTEGER
) STRICT;
CREATE UNIQUE INDEX attempts_one_active ON attempts(task_id) WHERE state = 'active';
CREATE INDEX attempts_task ON attempts(task_id, created_at);

CREATE TABLE processes (                                 -- 1 riga = 1 turno claude
  id                TEXT PRIMARY KEY,
  attempt_id        TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
  seq               INTEGER NOT NULL,
  prompt            TEXT NOT NULL,
  permission_mode   TEXT NOT NULL,
  session_id        TEXT NOT NULL,
  resumed           INTEGER NOT NULL CHECK (resumed IN (0,1)),
  status            TEXT NOT NULL DEFAULT 'running'
                    CHECK (status IN ('running','completed','failed','killed')),
  stop_reason       TEXT CHECK (stop_reason IS NULL OR stop_reason IN
                    ('user_stop','app_shutdown','app_restart','spawn_error','init_timeout',
                     'exit_timeout','crash','auth_failure','usage_limit')),
  error             TEXT,
  cli_version       TEXT,
  argv_json         TEXT NOT NULL CHECK (json_valid(argv_json)),   -- mai l'env
  pid               INTEGER,                              -- = pgid (process_group(0))
  app_instance_id   TEXT NOT NULL,                        -- UUID dell'avvio dell'app
  exit_code         INTEGER,
  result_subtype    TEXT,
  is_error          INTEGER CHECK (is_error IS NULL OR is_error IN (0,1)),
  cost_usd_estimate REAL,                                 -- stima client-side [F]
  num_turns         INTEGER,
  duration_ms       INTEGER,
  head_before       TEXT,
  head_after        TEXT,                                 -- dopo l'auto-commit
  started_at        INTEGER NOT NULL,
  finished_at       INTEGER,
  UNIQUE (attempt_id, seq)
) STRICT;
CREATE UNIQUE INDEX processes_one_running ON processes(attempt_id) WHERE status = 'running';
CREATE INDEX processes_running ON processes(status) WHERE status = 'running';

CREATE TABLE entries (                                   -- transcript normalizzato
  attempt_id TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
  idx        INTEGER NOT NULL,                            -- monotono per attempt
  rev        INTEGER NOT NULL,                            -- +1 a ogni upsert
  process_id TEXT NOT NULL,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),  -- atm_types::Entry
  ts         INTEGER NOT NULL,
  PRIMARY KEY (attempt_id, idx)
) STRICT, WITHOUT ROWID;
```

**Invarianti imposti dal DB:**
- al massimo un attempt attivo per task;
- al massimo un turno `running` per attempt, quindi mai due processi sulla stessa sessione.

**Chiavi di `settings`, con default:**

| Chiave | Default |
|---|---|
| `claude_path_override` | `null` |
| `default_model` | `null` |
| `max_running` | `2` (valori ammessi 1–6) |
| `allow_env_api_key` | `false` |
| `worktree_root` | `"~/.ai-task-manager/worktrees"` |
| `editor_app` | `"Visual Studio Code"` |
| `remove_worktree_after_merge` | `true` |

**Posizioni dei task:**
- In coda alla colonna: `max + 1024`.
- Tra due task: `(a + b) / 2`.
- Se `b - a < 1e-6`, si rinumera la colonna a passi di 1024 nella stessa transazione.
- Le posizioni le calcola solo il backend.

### 5.3 Enum (nomi in `atm-types`; stringhe identiche a quelle del DB e del CLI)

- `TaskStatus`: todo, inprogress, inreview, done, cancelled.
- `AttemptState`: active, merged, discarded.
- `WorktreeState`: present, removed, missing.
- `ProcessStatus`: running, completed, failed, killed.
- `PermissionMode`: default, acceptEdits, bypassPermissions.
- `ConfigPolicy`: isolated, trusted.
- `Effort`: low, medium, high, xhigh, max.
- `StopReason`: come nel CHECK SQL.

### 5.4 Transizioni di stato

| Evento | Task | Attempt | Process |
|---|---|---|---|
| `create_task` | → todo (in fondo, o nella colonna scelta) | – | – |
| `start_attempt` | → inprogress | insert `active`, worktree `present` | insert `running` (seq 1) |
| `send_follow_up` | → inprogress | resta `active` | insert `running` (seq n+1) |
| Fine turno, nessun altro `running` | inprogress → inreview | – | vedi tabella §7.7 |
| Spawn fallito | → inreview | – | `failed` / `spawn_error` + Notice |
| `merge_attempt` = Merged | → done | → `merged`, `merge_commit`, worktree `removed` (default), branch tenuto | – |
| `discard_attempt` | inprogress o inreview → todo | → `discarded`, commit di snapshot, worktree `removed`, branch tenuto | il turno viene fermato prima |
| Drag manuale | qualsiasi → qualsiasi | – | **rifiutato** verso done o cancelled se c'è un turno `running` ("ferma l'agente") |
| Drop in "In corso" senza attempt | la UI apre il dialog Start invece di spostare | – | – |
| Avvio app (recovery) | inprogress senza `running` → inreview | directory mancante → `missing` | `running` di istanze precedenti → `failed` / `app_restart` |

"In esecuzione" e "richiede approvazione" sono **derivati**: dal DB (process `running`) e dal registro in memoria delle approvazioni. Non vengono salvati.

---

## 6. Contratto IPC

### 6.1 Convenzioni

- Ogni comando ha la forma `#[tauri::command] async fn <name>(core: State<'_, Arc<Core>>, req: <Req>) -> Result<<Res>, AppError>`. Non ci sono comandi sincroni.
- Il comando di streaming aggiunge `on_event: tauri::ipc::Channel<TranscriptMsg>`.
- Chiavi JS: sempre `req` e, per lo streaming, `onEvent` (camelCase di default [F]).
- `AppError` è `Serialize`: la promise viene rifiutata con `{code, message}` e la UI la decodifica.
- Il contratto sta in `atm_types::api`, con un marker per comando:

```rust
pub trait Command { const NAME: &'static str; type Req: Serialize + DeserializeOwned; type Res: Serialize + DeserializeOwned; }
macro_rules! cmd { ($t:ident, $n:literal, $req:ty => $res:ty) => {
    pub struct $t; impl Command for $t { const NAME: &'static str = $n; type Req = $req; type Res = $res; } } }
```

- Il campo serde è il nome del campo Rust (nessuna `rename_all` globale). Uniche eccezioni: gli enum che corrispondono a stringhe del DB o del CLI (§5.3).

### 6.2 Tipi (`atm-types`, completi nei nomi e nelle forme)

```rust
pub type Id = String;            // UUID v4
pub type Millis = i64;

// model.rs
pub struct Project { pub id: Id, pub name: String, pub repo_path: String, pub default_target_branch: String,
    pub default_permission_mode: PermissionMode, pub default_model: Option<String>,
    pub config_policy: ConfigPolicy, pub trusted: bool /* fingerprint approvato e ancora valido */,
    pub allow_bypass: bool, pub created_at: Millis, pub updated_at: Millis }
pub struct Task { pub id: Id, pub project_id: Id, pub title: String, pub description: String,
    pub status: TaskStatus, pub position: f64, pub created_at: Millis, pub updated_at: Millis }
pub struct TaskCard { pub task: Task, pub attempt_id: Option<Id>, pub branch: Option<String>, pub running: bool,
    pub pending_approvals: u32, pub last_status: Option<ProcessStatus>, pub last_stop_reason: Option<StopReason>,
    pub worktree_state: Option<WorktreeState> }
pub struct AttemptView { pub id: Id, pub task_id: Id, pub state: AttemptState, pub branch: String,
    pub target_branch: String, pub base_commit: String, pub worktree_path: String, pub worktree_state: WorktreeState,
    pub permission_mode: PermissionMode, pub model: Option<String>, pub effort: Option<Effort>,
    pub session_started: bool, pub merge_commit: Option<String>, pub running: bool, pub pending_approvals: u32,
    pub created_at: Millis, pub closed_at: Option<Millis> }
pub struct ProcessInfo { pub id: Id, pub seq: u32, pub prompt: String, pub status: ProcessStatus,
    pub stop_reason: Option<StopReason>, pub result_subtype: Option<String>, pub is_error: Option<bool>,
    pub cost_usd_estimate: Option<f64>, pub duration_ms: Option<u64>, pub num_turns: Option<u32>,
    pub head_after: Option<String>, pub started_at: Millis, pub finished_at: Option<Millis> }
pub struct TaskDetail { pub task: Task, pub attempt: Option<AttemptView>, pub processes: Vec<ProcessInfo>,
    pub closed_attempts: Vec<AttemptView> }
pub struct BranchList { pub current: Option<String>, pub branches: Vec<String> }
pub struct Settings { pub claude_path_override: Option<String>, pub default_model: Option<String>,
    pub max_running: u32, pub allow_env_api_key: bool, pub worktree_root: String, pub editor_app: String,
    pub remove_worktree_after_merge: bool }
pub struct ClaudeInfo { pub path: Option<String>, pub version: Option<String>, pub supported: bool,
    pub min_version: String /* "2.1.223" */, pub tested_version: String /* "2.1.283" */ }
#[serde(tag = "state")]
pub enum AuthState { LoggedIn { auth_method: Option<String>, api_provider: Option<String>, email: Option<String>,
    org_name: Option<String>, subscription_type: Option<String> }, LoggedOut, Unknown { reason: String } }
pub struct EnvStatus { pub claude: ClaudeInfo, pub auth: AuthState, pub git_version: Option<String>,
    pub api_key_in_env: bool, pub cloud_provider_env: bool, pub paused: Option<String> /* usage limit */,
    pub problems: Vec<String>, pub checked_at: Millis }
pub enum LoginMethod { ClaudeAi, Console, Sso }        // → (nessun flag) | --console | --sso
pub enum OpenTarget { Finder, Terminal, Editor }

// transcript.rs
pub struct Entry { pub idx: u32, pub rev: u32, pub process_id: Id, pub ts: Millis,
    pub parent_tool_use_id: Option<String>, pub body: EntryBody }
#[serde(tag = "type")]
pub enum EntryBody {
    UserMessage   { text: String },
    SessionInit   { model: Option<String>, permission_mode: Option<String>, api_key_source: Option<String>,
                    mcp_servers: u32, warnings: Vec<String> },
    AssistantText { text: String },
    Thinking      { text: String },
    ToolCall      { tool_use_id: String, name: String, summary: String, input: String /* JSON ≤4 KiB */,
                    status: ToolStatus, output: Option<ToolOutput> },
    ApiRetry      { attempt: u32, max_retries: u32, delay_ms: u64, error: String },
    TurnEnd       { subtype: String, is_error: bool, duration_ms: Option<u64>, num_turns: Option<u32>,
                    cost_usd_estimate: Option<f64>, permission_denials: u32, text: Option<String>,
                    limit: Option<LimitKind> },
    Notice        { level: Level, text: String },
    Stderr        { text: String },
}
#[serde(tag = "state")]
pub enum ToolStatus { Running, AwaitingApproval { approval_id: Id, can_remember: bool },
    Denied { message: String }, Succeeded, Failed, Cancelled }
pub struct ToolOutput { pub text: String /* ≤8 KiB, testa+coda */, pub truncated_bytes: u64, pub is_error: bool }
pub enum Level { Info, Warn, Error }
pub enum LimitKind { UsageLimit, RateLimit, AuthFailure, Billing }
#[serde(tag = "t")]
pub enum TranscriptMsg {
    Snapshot { entries: Vec<Entry>, has_more: bool, typing: Option<String> },   // sostituisce lo store
    Upsert   { entries: Vec<Entry> },                                            // per idx, se rev > attuale
    Typing   { text: Option<String> },                                           // anteprima effimera
}
pub struct EntryPage { pub entries: Vec<Entry>, pub has_more: bool }
#[serde(tag = "kind")]
pub enum ApprovalDecision { Allow { remember: bool }, Deny { message: String, interrupt: bool } }

// review.rs
pub enum FileStatus { Added, Modified, Deleted, Renamed, Copied, TypeChanged }
pub enum LineKind { Hunk, Context, Add, Del, Meta }
pub struct DiffLine { pub kind: LineKind, pub old_no: Option<u32>, pub new_no: Option<u32>, pub text: String }
pub struct FileDiff { pub path: String, pub old_path: Option<String>, pub status: FileStatus, pub additions: u32,
    pub deletions: u32, pub binary: bool, pub too_large: bool, pub omitted: bool, pub lines: Vec<DiffLine> }
pub struct DiffResult { pub base: String, pub snapshot_tree: String, pub files: Vec<FileDiff>,
    pub additions: u32, pub deletions: u32, pub truncated: bool }
pub struct BranchStatus { pub target_branch: String, pub ahead: u32, pub behind: u32, pub dirty: bool,
    pub head_ok: bool, pub conflicts: Vec<String>, pub target_checked_out_at: Option<String>,
    pub merge_blocked: Option<String> }
pub enum MergeStrategy { UpdateRef, FfCheckedOut }
#[serde(tag = "kind")]
pub enum MergeOutcome { Merged { commit: String, strategy: MergeStrategy, cleanup_warning: Option<String> },
    NothingToMerge, Conflicts { files: Vec<String> } }

// error.rs
pub struct AppError { pub code: ErrorCode, pub message: String }
pub enum ErrorCode { NotFound, Invalid, Conflict, Busy, ConcurrencyLimit, UsageLimited, ClaudeNotFound,
    NotLoggedIn, WorktreeMissing, BranchMismatch, TargetCheckoutDirty, GitIdentityMissing, Git, Io, Db,
    NotImplemented, Internal }
```

### 6.3 Comandi (nome del comando = nome della fn Rust = `NAME` del marker)

| Comando | `req` → risposta | Note |
|---|---|---|
| `get_env` | `{force: bool}` → `EnvStatus` | Cache di 60 s. Con `force` rilegge PATH, `claude --version`, `auth status` e git |
| `open_login_terminal` | `{method: LoginMethod}` → `()` | Apre Terminal (§7.10) |
| `resume_agents` | `{}` → `EnvStatus` | Toglie la pausa impostata per usage limit |
| `get_settings` / `update_settings` | `{}` / `Settings` → `Settings` | `allow_env_api_key=true` chiede una **conferma nativa** (M6) |
| `list_projects` | `{}` → `Vec<Project>` | |
| `pick_repo_folder` | `{}` → `Option<String>` | Chiama `blocking_pick_folder` in `spawn_blocking` [F] |
| `add_project` | `{path}` → `{project, warnings: Vec<String>}` | §8.3 |
| `update_project` | `{id, name, default_target_branch, default_permission_mode, default_model}` → `Project` | |
| `set_project_security` | `{id, config_policy, allow_bypass}` → `Project` | M6. Conferma nativa quando si **eleva** il livello |
| `remove_project` | `{id}` → `()` | Rifiutato se ci sono turni attivi. Snapshot e rimozione dei worktree; branch tenuti |
| `list_branches` | `{project_id}` → `BranchList` | |
| `get_board` | `{project_id}` → `Vec<TaskCard>` | Unisce DB e registro live |
| `create_task` / `update_task` | `{project_id, title, description, status?}` / `{id, title, description}` → `TaskCard` | |
| `move_task` | `{id, status, before_id: Option<Id>}` → `()` | `Busy` se il task è in esecuzione e la destinazione è done o cancelled |
| `delete_task` | `{id}` → `()` | `Busy` se in esecuzione. Fa discard dell'attempt attivo |
| `get_task_detail` | `{id}` → `TaskDetail` | |
| `start_attempt` | `{task_id, target_branch, permission_mode, model?, effort?}` → `AttemptView` | Ritorna dopo che worktree e righe esistono; lo spawn è asincrono |
| `send_follow_up` | `{attempt_id, prompt, permission_mode?, fresh_session: bool}` → `ProcessInfo` | `Busy` se c'è un turno in corso |
| `stop_attempt` | `{attempt_id}` → `()` | Ritorna subito; l'escalation continua in background |
| `respond_approval` | `{attempt_id, approval_id, decision: ApprovalDecision}` → `()` | |
| `subscribe_transcript` | `{attempt_id}` + `onEvent: Channel<TranscriptMsg>` → `Id` (subscription) | Registra e ritorna subito [F] |
| `unsubscribe_transcript` | `{subscription_id}` → `()` | |
| `get_entries` | `{attempt_id, before_idx, limit ≤ 200}` → `EntryPage` | |
| `get_diff` | `{attempt_id}` → `DiffResult` | §8.6 |
| `get_branch_status` | `{attempt_id}` → `BranchStatus` | Include l'anteprima dei conflitti |
| `merge_attempt` | `{attempt_id, message}` → `MergeOutcome` | §8.7 |
| `discard_attempt` | `{attempt_id}` → `()` | |
| `delete_branch` | `{attempt_id}` → `()` | Solo per attempt `merged` e branch `atm/…` |
| `open_attempt` | `{attempt_id, target: OpenTarget}` → `()` | `open`, `open -a Terminal`, `open -a <editor_app>`. Il path viene dal DB |
| `open_url` | `{url}` → `()` | Solo `http(s)` |
| *(solo debug)* `debug_ping`, `debug_channel_probe`, `debug_selftest_report` | §11 M0 | `#[cfg(debug_assertions)]` |

### 6.4 Eventi globali (`app.emit`)

| Nome | Payload | Reazione della UI |
|---|---|---|
| `changed` | `Changed { project_id: Option<Id>, task_id: Option<Id> }` | Refetch con debounce di 100 ms: `get_board` se il progetto è quello selezionato; `get_task_detail` se il task è quello aperto; `list_projects` se `project_id` è None |
| `env_changed` | `EnvStatus` | Aggiorna il gate e la topbar (auth, pausa) |

`changed` si emette **dopo il commit** di ogni mutazione su task, attempt o process, compresi finalize e approvazioni.

### 6.5 Channel del transcript: ordine corretto per costruzione

```
runner:   db.upsert_entry(e)  →  live.send(Upsert e)        (prima persiste, poi trasmette)
subscribe_transcript(attempt, on_event):
  1 rx = live.sender(attempt).subscribe()        // prima il receiver
  2 snap = db.entries_tail(attempt, 200); typing = live.typing(attempt)
  3 spawn forwarder: send(Snapshot{snap, has_more, typing}); poi loop:
       rx.recv(): Upsert → accumula (≤ 50 ms o ≤ 200 entry) → send(Upsert)
                  Typing → tiene solo l'ultimo
                  Lagged → rilegge la coda dal DB → send(Snapshot)
                  Closed / errore di send → termina e si deregistra
  4 ritorna subscription_id
```
- **Lato UI:** `Snapshot` sostituisce lo store; `Upsert` si applica per `idx` solo se `rev` è maggiore di quello presente. Duplicati e riordini temporanei sono quindi innocui.
- **`broadcast`:** capacità 1024 per attempt, con perdite ammesse. La memoria resta limitata e stdout non viene mai bloccato dalla UI.
- **Pulizia:** `on_page_load(Started)` chiama `core.drop_subscriptions()`, così un reload non lascia forwarder orfani.

### 6.6 Binding WASM (`ui/src/ipc/tauri.rs`)

```rust
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn tauri_listen(event: &str, h: &Closure<dyn FnMut(JsValue)>) -> Result<JsValue, JsValue>;
    pub type Channel;
    #[wasm_bindgen(constructor, js_namespace = ["window", "__TAURI__", "core"])]
    pub fn new() -> Channel;
    #[wasm_bindgen(method, setter)]
    pub fn set_onmessage(this: &Channel, f: &Closure<dyn FnMut(JsValue)>);
}
fn to_js<T: Serialize>(v: &T) -> Result<JsValue, AppError>        // JSON.parse(serde_json::to_string(v))
fn from_js<T: DeserializeOwned>(v: &JsValue) -> Result<T, AppError> // serde_json::from_str(JSON.stringify(v))
pub async fn call<C: Command>(req: &C::Req) -> Result<C::Res, AppError>   // args = {req}; errore → AppError
pub async fn listen<T: DeserializeOwned + 'static>(ev: &str, f: impl FnMut(T) + 'static) // payload in {payload}
pub async fn subscribe_transcript(attempt_id: &Id, f: impl FnMut(TranscriptMsg) + 'static)
    -> Result<(Id, Channel, Closure<dyn FnMut(JsValue)>), AppError>   // onmessage impostato PRIMA dell'invoke
```
- **Channel:** [DA VERIFICARE → M0] se il binding `constructor` + `js_namespace` fallisce, si usa `Reflect::get(window.__TAURI__.core, "Channel")` + `Reflect::construct`.
- **Regole Leptos:**
  - `Channel`, `Closure` e la funzione di unlisten vanno in `StoredValue::new_local`.
  - Nei callback si usano `try_update`/`try_set`.
  - `on_cleanup` chiama `unsubscribe_transcript` [F].
- **Mock:** con `cfg(feature = "mock")`, `ipc/mock` sostituisce `tauri.rs` con le stesse firme.

---

## 7. Executor Claude (`claude.rs`, `wire.rs`, `normalize.rs`, `runner.rs`)

### 7.1 Discovery e versione

Si usa il primo candidato valido:
1. `settings.claude_path_override`.
2. La variabile d'ambiente `ATM_CLAUDE_PATH` (dev e test la puntano a `fake-claude`).
3. `~/.local/bin/claude`, `~/.claude/local/claude`, `/opt/homebrew/bin/claude`, `/usr/local/bin/claude`.
4. `command -v claude` dentro la login shell (§7.2), scartando i path in `$TMPDIR` o che contengono `/cmux-cli-shims/` (in terminale c'è uno shim cmux [V]).

Regole:
- Un candidato è valido se `<path> --version` termina con `(Claude Code)`.
- Si conserva il **path del symlink**, senza canonicalizzarlo: il CLI si auto-aggiorna.
- La versione è la prima parola dell'output di `--version` (semver).
  - Sotto `2.1.223`: banner con "Continua comunque" (il resume tra cwd diversi arriva da quella versione [F]).
  - Sopra `2.1.283`: solo un badge informativo.

### 7.2 Ambiente del figlio (`ChildEnv`, identico per `auth status` e per gli agenti)

| Voce | Regola |
|---|---|
| Base | L'env del processo app, ereditato |
| `PATH` | Importato una sola volta: `$SHELL -ilc 'printf "__ATM__%s__ATM__" "$PATH"'` (stdin null, timeout 5 s), estraendo il testo tra i marker. Fallback: `~/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`. **Solo PATH**: nessun'altra variabile di `.zshrc` |
| `LANG` | `en_US.UTF-8` se assente (le app lanciate dal Finder non la hanno) |
| Rimossi | `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` (salvo `allow_env_api_key`); `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`; `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE`, `GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*` |
| Impostati | `PWD=<worktree canonico>` (stessa stringa di `current_dir`), `GIT_EDITOR=true`, `GIT_SEQUENCE_EDITOR=true`, `GIT_TERMINAL_PROMPT=0`, `ATM_ATTEMPT_ID` |
| Mai impostati | `CLAUDE_CODE_OAUTH_TOKEN` (se presente passa invariato; l'app non lo legge, non lo registra e non lo salva), `CLAUDE_CODE_ENTRYPOINT` |

### 7.3 argv esatto (`claude::build_argv`: un vettore, niente shell)

```
<claude> -p
  --output-format stream-json --input-format stream-json --verbose
  --include-partial-messages
  --permission-prompt-tool stdio
  --permission-mode=<default|acceptEdits|bypassPermissions>      # SEMPRE, a ogni spawn [V]
  [--allow-dangerously-skip-permissions]                          # solo se project.allow_bypass [V]
  ( --session-id=<attempt.session_id>                             # se session_started = 0
  | --resume=<attempt.session_id> )                               # altrimenti
  --disallowedTools=AskUserQuestion                               # v1: domande rimandate
  --settings=<json>                                               # §7.8, a ogni turno
  [--setting-sources=user --strict-mcp-config]                    # policy Isolated (default)
  [--model=<m>] [--effort=<e>]
  --append-system-prompt=<ATM_APPEND>
```

- La forma `--flag=value` impedisce l'iniezione di flag. Il prompt passa **solo da stdin**.
- **Mai passati:**
  - `--bare`: disabilita l'OAuth [F];
  - `--dangerously-skip-permissions`;
  - `--system-prompt`: sostituirebbe il prompt di Claude Code;
  - `--no-session-persistence`: romperebbe il resume;
  - `--replay-user-messages`: serve solo al rewind, rimandato;
  - `--allowedTools` con un tool intero: oscurerebbe `can_use_tool`.
- **`ATM_APPEND`.** È fisso per attempt, perché `--system-prompt-snapshot` è attivo di default e il prompt si congela al primo turno [V]:
  > "You are working on a task from AI Task Manager inside a dedicated git worktree `<path>` on branch `<branch>` (created from `<target>`). Work only inside this directory. Do not push, switch or delete branches, rewrite history, or change git remotes/config. The host app commits your changes automatically after each turn. If a CLAUDE.md or AGENTS.md exists at the repository root, read it first and follow its conventions."
- La frase finale su CLAUDE.md copre il caso non verificato E11.
- **Spawn:**
  - `tokio::process::Command` con `.current_dir(wt).process_group(0).kill_on_drop(true)`, stdin, stdout e stderr in pipe;
  - `pid` = `pgid`, salvato nel DB;
  - `argv_json` salvato; l'env mai.

### 7.4 Protocollo stdin (`wire.rs`; un solo task writer)

- **Writer.** Un task scrittore riceve da un `mpsc::channel::<Value>(64)`. Scrive una riga JSON, poi `\n`, poi `flush`. Ogni riga viene copiata anche in `stdin.jsonl`.
- **ID delle richieste host.** Formato `atm_<n>_<8hex>`. Le richieste in sospeso stanno in `HashMap<String, oneshot::Sender<Result<Value,String>>>`.

**Sequenza di un turno:**
1. Il runner invia `{"type":"control_request","request_id":"atm_1_…","request":{"subtype":"initialize","hooks":null}}` e **attende** il `control_response` per al massimo 60 s, continuando intanto a instradare stdout.
   - Risposta `error`: Notice (warn) e si prosegue.
   - Timeout: stop sequence, poi `failed` / `init_timeout`.
   - Con `hooks:null` gli hook di progetto non vengono sostituiti (Vibe Kanban #3327 [F]).
2. Il runner invia `{"type":"user","message":{"role":"user","content":"<prompt>"},"parent_tool_use_id":null}`, senza `session_id` [F]. Contestualmente crea l'entry `UserMessage`.
   - Primo turno: `"# {title}\n\n{description}"`.
   - Follow-up: il testo dell'utente, invariato.
   - `fresh_session`: task + descrizione + output di `git log --oneline <base>..HEAD` + il testo.
3. All'arrivo di `result`: il runner registra il risultato, **chiude stdin** (drop del sender) e continua a leggere stdout fino a EOF, perché dopo `result` possono arrivare altri eventi [F]. Poi `wait` con timeout di 30 s; allo scadere, stop sequence con `exit_timeout`.
4. Dopo qualunque uscita: `killpg(pgid, SIGTERM)` sul gruppo residuo, ignorando `ESRCH`. Termina i processi in background avviati dall'agente, come i dev server; è un limite documentato del modello un-processo-per-turno.

**Routing dello stdout:**

| Riga | Azione |
|---|---|
| `control_response` | Risolve la richiesta host in sospeso |
| `control_request` / `can_use_tool` | Approvazioni (§7.8) |
| `control_request` di altri tipi (`hook_callback`, `mcp_message`, `elicitation`, …) | Risposta `{"subtype":"error","request_id":…,"error":"Unsupported control request subtype: X"}` [F] più una Notice (warn) |
| `control_cancel_request` | Annulla l'approvazione corrispondente, **senza risposta** [F] |
| `keep_alive` | Ignorato |
| `stream_event` | Solo anteprima di digitazione (§7.6). Non finisce nel raw log |
| Riga che non inizia con `{` | Solo raw log |
| Tutto il resto | Normalizer, più raw log |

### 7.5 Lettura dello stdout (correzione E5)

`read_line_capped(&mut BufReader, &mut Vec<u8>, max = 16 MiB) -> io::Result<Line>` usa `fill_buf`/`consume`:
- oltre il cap scarta i byte fino a `\n` e restituisce `Line::TooLong(len)`;
- il loop **continua**;
- per ogni riga troppo lunga emette una Notice (warn) "riga di output > 16 MiB ignorata".

Per stderr il cap è 64 KiB per riga. Cap dei file di log: `stdout.jsonl` 64 MiB e `stderr.log` 8 MiB. Oltre il cap non si scrive più e si registra una Notice.

### 7.6 Normalizzazione (`normalize.rs`, pura, golden test con insta)

`Normalizer::new(process_id, next_idx)`. Metodi:
- `on_user_message(&str, ts)`
- `on_line(&Value, ts) -> Vec<EntryOp>`
- `on_stderr(&str, ts)`
- `on_approval_requested(approval_id, &Value /*request*/, can_remember)`
- `on_approval_resolved(approval_id, &ApprovalDecision)`
- `on_approval_cancelled(approval_id)`
- `on_notice(level, text)`
- `finish(ts)`

Dove `EntryOp = Upsert(Entry) | Typing(Option<String>)`.

| Input | Output |
|---|---|
| Messaggio utente inviato | `UserMessage` |
| `system/init` | `SessionInit`. Warning se `apiKeySource` indica una chiave API (valori [DA VERIFICARE → M5]) o se `cwd` ≠ worktree. Effetto collaterale nel runner: `session_started=1`; se `session_id` ≠ quello atteso si salva quello osservato e si emette una Notice |
| `stream_event` `content_block_delta` (`text_delta`/`thinking_delta`) | `Typing(Some(buffer))`, al massimo ogni 100 ms. `content_block_start` azzera il buffer. Gli altri `stream_event` vengono ignorati |
| `assistant`: ogni blocco di `message.content[]` | `text` → `AssistantText`; `thinking` → `Thinking`; `tool_use` → `ToolCall{Running}`, oppure fusione con quello creato da `can_use_tool`. Più `Typing(None)`. `parent_tool_use_id` dall'envelope |
| `user` con `tool_result` | Il `ToolCall` con quel `tool_use_id` passa a `Succeeded` o `Failed(is_error)`, con `output` (stringa o array di blocchi uniti; le immagini diventano `[image]`; 8 KiB testa+coda) |
| `user` con contenuto stringa | Ignorato |
| `system/api_retry` | `ApiRetry`, un'entry per processo aggiornata in place |
| `system/compact_boundary` | Notice info "Contesto compattato" |
| `result` | `TurnEnd` con `limit` classificato dalla costante `LIMIT_PATTERNS` (subtype, `is_error`, testi "Not logged in", "/login", "Login expired", "limit", `billing_error`), più `Typing(None)` |
| Approvazione richiesta, risolta o annullata | Il `ToolCall` passa a `AwaitingApproval{…}`, poi a `Running` o `Denied{message}`. Se non esiste ancora viene creato dalla richiesta |
| stderr | `Stderr`: le righe che arrivano a meno di 2 s l'una dall'altra finiscono nella stessa entry (upsert, ≤ 64 KiB). ANSI rimosso |
| `finish()` | I `ToolCall` ancora in `Running` o `AwaitingApproval` passano a `Cancelled` |
| Altri `system/*` e tipi sconosciuti | Nessuna entry (restano nel raw log) |

**`ToolCall.summary`** (path relativi al worktree):

| Tool | Summary |
|---|---|
| Bash | `$ <cmd>` (prima riga, ≤ 200 caratteri) |
| Read | `Read <path>` |
| Edit, MultiEdit, NotebookEdit | `Edit <path>` |
| Write | `Write <path>` |
| Grep | `Grep "<pattern>" <path>` |
| Glob | `Glob <pattern>` |
| WebFetch | `<url>` |
| WebSearch | `<query>` |
| Task, Agent | `Subagent: <description>` |
| TodoWrite | `Todos: <done>/<total>` |
| `mcp__s__t` | `MCP s/t` |
| Altro | `<name>` |

**Limiti:** campi di testo 256 KiB; `input` 4 KiB; `output` 8 KiB.

### 7.7 Ciclo di vita del turno (`runner.rs`) e classificazione

```
run_turn(attempt, prompt, mode):
 1 lock per attempt (tokio::Mutex); poi l'indice processes_one_running come rete di sicurezza
 2 preflight → AppError tipizzato, nessuno spawn:
     CLI trovato · auth LoggedIn (cache ≤ 60 s; LoggedOut → NotLoggedIn; Unknown → consentito con Notice)
     · non in pausa (UsageLimited) · running < max_running (ConcurrencyLimit) · worktree present
     · HEAD == refs/heads/<branch> (BranchMismatch) · policy: se Trusted e il fingerprint del worktree
       ≠ quello approvato → turno Isolated + Notice (M6)
 3 head_before = rev-parse HEAD; INSERT process running; task → inprogress; emit changed
 4 spawn (§7.3); se fallisce → failed/spawn_error + Notice; task → inreview
 5 task: writer stdin · reader stdout · reader stderr · monitor (select! su wait, stop, timeout)
 6 initialize (attende) → messaggio utente
 7 result → chiusura stdin → EOF → wait
 8 finalize: normalizer.finish() → git::autocommit (§8.5) → head_after → verifica HEAD (Notice se diverso)
    → UPDATE process → task inprogress→inreview (se nessun altro running) → emit changed
    → auth_failure: invalida la cache auth + env_changed; usage_limit: paused = Some(testo) + env_changed
```

| Osservato | status | stop_reason |
|---|---|---|
| `result` success, uscita 0 | completed | – |
| `result` con `is_error` | failed | `usage_limit` o `auth_failure` se classificato, altrimenti – |
| Nessun `result`, uscita ≠ 0 o per segnale, senza stop richiesto | failed | crash |
| Stop richiesto | killed | user_stop / app_shutdown |
| `initialize` senza risposta entro 60 s | failed | init_timeout |
| `result` ricevuto ma nessuna uscita entro 30 s | come indicato da `result` | exit_timeout |
| Errore di `spawn()` | failed | spawn_error |

### 7.8 Approvazioni

- **All'arrivo di `can_use_tool`** (campi `tool_name`, `input` e `tool_use_id` obbligatori; `permission_suggestions` resta `Value` grezzo [F]):
  - si registra in memoria `Pending{approval_id: uuid, request_id, tool_use_id, input, rules}`;
  - si aggiorna l'entry;
  - si emette `changed` (badge "Richiede approvazione" sulla card).
- **Nessuna tabella e nessun timeout.** L'utente può sempre fermare il turno. Alla fine del processo le approvazioni pendenti diventano `Cancelled`.
- **`can_remember`** è vero solo se tutte le suggestion sono `addRules` con `behavior:"allow"` e ogni regola ha `ruleContent` non vuoto. Le regole su un tool intero (es. `Bash` nudo) non sono mai memorizzabili.

**Risposte** (funzione pura `wire::approval_response(&Pending, &ApprovalDecision) -> Value`; si fa eco del `request_id` del CLI):

| Decisione | `response` |
|---|---|
| `Allow{remember:false}` | `{"behavior":"allow","updatedInput":<input originale>}` (**sempre** presente) |
| `Allow{remember:true}` | Come sopra, più `"updatedPermissions":<suggestion filtrate, con ogni destination riscritta a "session">`. In più si aggiungono le stringhe `Tool(ruleContent)` ad `attempts.allow_rules`, ripassate nei turni successivi via `--settings` (la persistenza delle regole di sessione dopo `--resume` è [DA VERIFICARE]) |
| `Deny{message, interrupt}` | `{"behavior":"deny","message":"The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said: <msg>","interrupt":<bool>}` |
| `AskUserQuestion` (arriva comunque) | Deny con `"Ask your question in plain text in your reply instead."`, `interrupt:false` |

**JSON di `--settings`** (snapshot-testato; stringhe esatte, senza ambiguità di split):
```json
{"permissions":{
  "deny":["Bash(git push *)","Bash(git push)","Read(~/.ssh/**)","Read(~/.aws/**)",
          "Read(~/.claude/.credentials.json)","Edit(~/.claude/**)","Edit(~/.ssh/**)"],
  "allow":[ /* attempts.allow_rules */ ]}}
```
`-p` ignora in silenzio i settings non validi [V/F]: M5 deve dimostrare che il deny su `git push` funziona. È una difesa in profondità: `sh -c 'git push'` la aggira.

### 7.9 Follow-up, stop, resume

- **Follow-up.** Consentito solo se nessun turno è in corso: il composer è disabilitato durante il turno, lo Stop resta attivo. La coda dei messaggi è rimandata.
- **Sessione:**
  - `session_id` si assegna alla creazione dell'attempt;
  - `session_started=1` solo quando arriva `system/init`;
  - se un turno muore **prima** di `init`, il turno successivo genera un **nuovo UUID**, aggiorna `attempts.session_id` e usa `--session-id` (lezione di Vibe Kanban #2993; correzione E7).
- **Resume.** Stesso cwd canonico (il worktree non si sposta né si rinomina mai). I file `.jsonl` del CLI non si copiano, leggono o spostano mai. Mai due processi sulla stessa sessione (indice unico).
- **Resume fallito** (`No conversation found`): Notice con l'azione "Nuova sessione", che invia `send_follow_up{fresh_session:true}`.
- **Interrotto al riavvio.** Card e pannello mostrano **"Continua"**, che invia il follow-up "The previous run was interrupted (app restart). Continue the task." con `--resume`.
- **Stop sequence** (obiettivo ≤ 13 s; lo stato finale dipende dal nostro flag, non dal subtype di `result`, che è [DA VERIFICARE]):
  1. Se `init` è arrivato e stdin è aperto: `{"type":"control_request","request_id":"atm_n_…","request":{"subtype":"interrupt"}}`, poi fino a 5 s di attesa per `result` o uscita.
  2. Chiusura di stdin (l'EOF cancella il prompt pendente [F]), poi fino a 3 s.
  3. `killpg(pgid, SIGTERM)`, poi fino a 3 s.
  4. `killpg(pgid, SIGKILL)` e reap.
- **Shutdown dell'app.** `RunEvent::ExitRequested` → `api.prevent_exit()` una sola volta (flag atomico) → `core.shutdown(deadline 8 s)`: stop in parallelo con tempi compressi 2/2/2 s, finalize con auto-commit → `app.exit(0)`.
- **Recovery all'avvio** (prima che la UI carichi i dati):
  - per ogni process `running` con `app_instance_id` diverso da quello attuale:
    - se `kill(pid,0)` riesce **e** `ps -o command= -p <pid>` contiene il path di claude **e** il `session_id` dell'attempt (un UUID, quindi niente falsi positivi da riuso del PID) → `killpg(SIGTERM)`, 3 s, poi `SIGKILL`;
    - in ogni caso: `failed` / `app_restart`, Notice "Esecuzione interrotta dal riavvio dell'app", tool aperti → `Cancelled`, auto-commit;
  - i task in inprogress senza turni attivi passano a inreview;
  - riconciliazione dei worktree (§8.4).
- **Concorrenza.** Contatore globale con `max_running` (default 2): oltre il limite si restituisce `ConcurrencyLimit`, senza coda. Un `result` di tipo usage limit o billing **mette in pausa** i nuovi avvii finché l'utente non preme "Riprendi".

### 7.10 Auth e login UX

- **Controllo.** `claude auth status --json` con il `ChildEnv`, timeout 10 s.
  - Exit 0: parse dei campi `loggedIn, authMethod, apiProvider, email, orgId, orgName, subscriptionType` [V/F].
  - Exit 1: `LoggedOut`.
  - Altro o timeout: `Unknown`.
  - Email e org restano **solo in memoria**: mai nel DB, mai nei log.
- **Quando si controlla:** all'avvio, al focus della finestra (al più ogni 60 s), prima di ogni spawn (cache di 60 s) e dopo un `auth_failure`.
- **Gate di onboarding** (bloccante finché il CLI non è trovato e l'utente non è loggato):

| Caso | UI |
|---|---|
| CLI non trovato | Istruzioni di installazione (link ai docs Anthropic via `open_url`), campo "Percorso di claude" (override) e "Ricontrolla" |
| Versione vecchia | "Aggiorna Claude Code (`claude update` nel terminale)", "Ricontrolla" e "Continua comunque" |
| Non loggato | `select_native` con "Account Claude (abbonamento)" (default), "Anthropic Console" e "SSO", più il bottone **"Accedi con Claude Code (apre il Terminale)"** |

- **Login.** Si scrive `app_cache_dir()/claude-login.command` (0700) con questo contenuto:
  ```sh
  #!/bin/sh
  '<abs claude path, single-quote escaped>' auth login [--console|--sso]
  echo
  echo "Accesso completato? Puoi chiudere questa finestra e tornare ad AI Task Manager."
  ```
  - Si apre con `open -a Terminal <file>`: argv, niente plugin opener.
  - L'app **non** collega pipe al processo e non vede mai URL, codici o token.
  - Un dialog "Completa l'accesso nel Terminale…" interroga `auth status` ogni 3 s per al massimo 10 minuti, con "Ricontrolla ora". Poi il file viene cancellato.
  - Il comando copiabile `claude auth login` è sempre visibile come fallback. Un eventuale blocco di Gatekeeper è [DA VERIFICARE → M4].
- **Topbar:** chip `email · subscriptionType`.
  - Banner se `authMethod ≠ "claude.ai"` o `apiProvider` non è first-party ("fattura via …").
  - Banner se c'è una chiave API nell'env e `allow_env_api_key` è attivo.
- **Durante l'esecuzione:** `TurnEnd.limit == AuthFailure` → gate di nuovo visibile.

### 7.11 Timeout e limiti

| Operazione | Valore |
|---|---|
| PATH dalla login shell | 5 s |
| `--version` / `auth status` | 5 s / 10 s |
| `initialize` | 60 s |
| Dall'interrupt all'EOF, al TERM, al KILL | 5 / 3 / 3 s (shutdown: 2/2/2) |
| Da `result` all'uscita | 30 s |
| Comandi git: default / `worktree add` / diff | 60 s / 600 s / 120 s |
| Riga stdout / riga stderr | 16 MiB / 64 KiB |
| Log stdout / stderr | 64 MiB / 8 MiB |
| Broadcast / canale stdin | 1024 / 64 |
| Batch del forwarder | 50 ms o 200 entry |
| Anteprima di digitazione | 100 ms |

---

## 8. Git e worktree (`git.rs`)

### 8.1 Runner irrobustito (correzione E6)

- **Binario:** `command -v git` dalla login shell, con fallback `/usr/bin/git`. Richiede ≥ 2.38.
- **Ogni chiamata:**
  ```
  git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c core.quotePath=false -c color.ui=never -c gc.auto=0 -C <dir> …
  ```
  - I diff aggiungono `--no-ext-diff --no-textconv`; i commit `--no-verify`.
- **Env:**
  - la stessa lista di rimozione `GIT_*` del §7.2;
  - `LC_ALL=C LANGUAGE=C`, perché il git locale è in italiano [F] e il parsing richiede i messaggi C;
  - `GIT_TERMINAL_PROMPT=0 GIT_EDITOR=true GIT_PAGER=cat`;
  - `GIT_OPTIONAL_LOCKS=0` sulle letture, per non contendere `index.lock` all'agente.
- **Altro:** stdin null, `kill_on_drop`, stdout limitato a 64 MiB, `-z` ovunque compaiano path, `--` prima dei pathspec, `--end-of-options` prima delle revisioni.
- **Lock:**
  - un mutex per repo per `worktree add/remove`, merge e cancellazione di branch;
  - un mutex per attempt per turno, finalize, diff, merge e discard;
  - ordine sempre attempt → repo.

### 8.2 Nomi

| Oggetto | Regola |
|---|---|
| Worktree | `<worktree_root>/<attempt_uuid>`, dal solo ID [F]. Canonicalizzato una volta e immutabile. La root viene creata con permessi 0700, non deve stare dentro un progetto e non contiene spazi |
| Branch | `atm/<primi 8 hex dell'id>-<slug>`. Lo slug è il titolo minuscolo con `[^a-z0-9]+` → `-`, trim, ≤ 24 caratteri, `task` se vuoto. Validato con `git check-ref-format --branch`. In caso di collisione (`show-ref --verify`) si aggiunge `-2`, `-3`, … Immutabile |
| Target | Deve esistere in `for-each-ref refs/heads`. Default: `project.default_target_branch` |

### 8.3 Aggiunta del progetto

- `rev-parse --show-toplevel` (canonico).
- `--is-bare-repository` deve valere false.
- `rev-parse --verify -q HEAD` deve riuscire (almeno un commit).
- `--git-dir` deve coincidere con `--git-common-dir`: niente worktree collegati; si chiede il checkout principale.
- Il repo non deve stare dentro `worktree_root`.
- **Warning** (non bloccanti) se esistono `.claude/` o `.mcp.json`: "non verranno caricati finché il progetto è Isolato".

### 8.4 Creazione, rimozione, riconciliazione

```
sha = rev-parse --verify --end-of-options refs/heads/<target>^{commit}
worktree add --lock --reason "atm:<attempt_id>" -b <branch> <path> <sha>
canonicalize(path) → INSERT attempt (base_commit = sha)
```
- **Se la creazione fallisce:** si pulisce solo **quel** worktree (`worktree remove --force <path>`; `rm -rf` solo dopo aver verificato che il path è sotto la root canonica), poi si ritenta una volta.
- **Rimozione** (discard, merge, rimozione del progetto; nessun turno attivo):
  1. commit di snapshot `atm: wip snapshot` se il worktree è sporco;
  2. `worktree unlock <path>`, poi `worktree remove --force <path>`;
  3. se la directory manca già, si cancella solo la metadata dir corrispondente, trovata cercando `<path>/.git` in `<common>/worktrees/*/gitdir`;
  4. **mai** un `git worktree prune` su tutto il repo;
  5. `worktree_state = removed`; il branch resta.
- **Riconciliazione** (avvio e "Ricontrolla"): `worktree list --porcelain -z` confrontato con il DB → `missing` o `present`. Nessuna cancellazione automatica. Un worktree `missing` consente solo il discard (la ricreazione è rimandata).

### 8.5 Auto-commit (fine di ogni turno, prima di merge e discard)

1. `status --porcelain=v1 -z --untracked-files=all`. Se non è vuoto: `add -A`, poi `commit --no-verify -q -m "atm: turn <seq>: <prima riga del prompt, 60 caratteri>"`.
2. Se `user.name` o `user.email` mancano: `-c user.name="AI Task Manager" -c user.email=atm@localhost`, **solo** sui commit del branch `atm/*`, che poi viene squashato.
3. Notice con esito: "Commit automatico abc1234 (N file)" oppure l'errore.

### 8.6 Diff (snapshot tree; l'indice dell'agente non viene toccato)

```
base = merge-base refs/heads/<target> HEAD
tmp  = copia di $(rev-parse --git-path index) in $TMPDIR/atm-idx-<uuid>   (conserva la cache stat)
GIT_INDEX_FILE=tmp git add -A ; T = GIT_INDEX_FILE=tmp git write-tree ; rm tmp
lista:  diff --no-color --no-ext-diff --no-textconv -M -z --name-status base T
stats:  diff … -M -z --numstat base T                (binario = "-\t-")
file:   diff … -M -U3 base T -- <path> [<old_path>]  per file, finché il budget lo consente
```
- Parse dei hunk nel backend, con una funzione pura testata: `DiffLine{kind, old_no, new_no, text}`.
- **Limiti:**
  - oltre 256 KiB di patch per file: `too_large`;
  - oltre 4 MiB in totale, o oltre 300 file: `omitted` (solo statistiche) e `truncated`.
- Si copre anche il lavoro non committato durante un turno.

### 8.7 Branch status e squash merge

- **`get_branch_status`:**
  - `ahead` e `behind` da `rev-list --left-right --count refs/heads/<target>...HEAD`;
  - `dirty` da `status`;
  - `head_ok`: `symbolic-ref -q HEAD` = `refs/heads/<branch>`;
  - `conflicts`: `merge-tree --write-tree --name-only --no-messages -z <target_tip> HEAD` (exit 1 = conflitti);
  - `target_checked_out_at` da `worktree list --porcelain -z`;
  - `merge_blocked` con il motivo.
- **Merge:**
```
precondizioni: attempt active · nessun turno running · worktree present · head_ok
1 autocommit(wt)                                  → S = refs/heads/<branch>
2 lock repo; T0 = rev-parse --verify refs/heads/<target>
3 merge-tree --write-tree --name-only --no-messages -z T0 S
     exit 1 → Conflicts{files}   (nessun ref toccato)      · altro ≠ 0 → Git
4 tree == rev-parse T0^{tree} → NothingToMerge
5 C = commit-tree <tree> -p T0 -F <msgfile>      (identità mancante → GitIdentityMissing)
6 target non in checkout:   update-ref -m "atm: squash <attempt>" refs/heads/<target> C T0   (CAS; fallisce → 1 retry da 2)
  target in checkout in W:  HEAD di W == T0 e nessuna operazione in corso in W, poi git -C W merge --ff-only -q C
                            (git rifiuta se ci sono modifiche locali sovrapposte → TargetCheckoutDirty; W intatto)
7 DB (1 transazione): attempt merged + merge_commit + closed_at; task → done; emit changed
8 best effort: se remove_worktree_after_merge → rimozione (§8.4); un errore → cleanup_warning. Il branch resta
```
- **Messaggio di default** (modificabile): titolo, riga vuota, descrizione (≤ 2000 caratteri), riga vuota, `ATM-Attempt: <id>`.
- **Target divergente senza conflitti:** si mergia direttamente, senza rebase.
- **Conflitti:** "Risolvi con l'agente" invia questo follow-up:
  > "This branch conflicts with `<target>` in: <files>. Run `git merge <target>`, resolve every conflict preserving both intents, run the project's tests if available, and commit the merge. Do not push."
  
  Dopo quel turno `merge-tree` risulta pulito.

### 8.8 Regole sullo stato sporco e cose vietate

- Un turno può partire con il worktree sporco; l'auto-commit raccoglie tutto.
- Il checkout principale dell'utente viene modificato **solo** da `merge --ff-only`.
- **Mai:** `reset --hard`, `push`, `fetch`, `stash`, `checkout` nel repo principale, `branch -D` su branch non `atm/*` o su attempt non `merged`, `worktree prune` globale, `rm -rf` fuori da `worktree_root`.

### 8.9 Fingerprint della configurazione Claude (M6)

- Si calcola SHA-256 (sha2) su `path\0len\0contenuto` dei file ordinati sotto `.claude/**`, più `.mcp.json`.
  - Al massimo 2000 file; i symlink contano con la stringa del loro target; ogni path deve stare dentro repo o worktree.
- **Approvazione:** calcolata sul checkout principale.
- **Prima di ogni turno:** calcolata sul worktree.

---

## 9. UI (Leptos CSR con Rust/UI)

### 9.1 Politica sul registry

- **Copiati tali e quali** in `ui/src/ui/` (sorgente `rust-ui/leptos-ui/app_crates/registry/src/ui/*.rs`, commit annotato in `VENDORED.toml`): button, badge, card, input, textarea, label, separator, tabs, scroll_area, skeleton, alert, callout, spinner, status, tooltip, select_native, kbd, empty, collapsible, message, bubble, marker, chat. Nessuno ha `<script>` né `crate::` [V].
- **Hook** in `ui/src/hooks/`: `use_random`, `use_scroll_lock`.
- **Portato:** `dialog`, con intestazione `// ATM-PORT: script removed; signal-driven`.
  - Stessi markup e classi; `<script>` eliminato.
  - Prop `open: RwSignal<bool>` che pilota `data-state`.
  - Chiusura con Esc (`window_event_listener(ev::keydown)`) e con click sul backdrop.
  - `use_scroll_lock::lock()`/`unlock()` chiamati da Rust.
  - Focus sul primo elemento focusabile, ripristinato alla chiusura.
- **Non usati:**
  - sheet, select, dropdown_menu, popover, hover_card, command, drawer: hanno script;
  - sonner: dipende da JS del sito;
  - drag_and_drop: modifica il DOM;
  - sidenav: richiede un router;
  - shimmer: non necessario.

### 9.2 Schermate

**Layout:** sidebar (240 px) | board | pannello del task (split a destra, 55 %, visibile quando un task è selezionato). Niente router: `AppCtx { env, projects, project, board_version, open_task, detail_version, toasts }` come signal.

| Schermata | Componenti Rust/UI | Scritto a mano |
|---|---|---|
| Onboarding (§7.10) | card, callout, alert, button, spinner, select_native, input, label, kbd, dialog* (attesa del login) | polling, logica del gate |
| Sidebar e topbar | button (ghost), separator, scroll_area, badge, status, tooltip | lista progetti, "Aggiungi repository", chip account, banner di pausa con "Riprendi", contatore "in esecuzione x/y" |
| Board (5 colonne; Annullati compressa) | card, badge (stato, "in esecuzione" con spinner, "Richiede approvazione", "Fallito", "Interrotto – Continua"), scroll_area, empty, skeleton, button | **DnD** (§9.3), creazione rapida in fondo alla colonna |
| Dialog task (crea/modifica) e Impostazioni | dialog*, input, textarea, label, button, select_native | form |
| Pannello task: header | badge, button (Avvia, Stop, Scarta, Apri in Finder/Terminale/Editor), tooltip, separator | – |
| Dialog Avvia | dialog*, select_native (branch target, modello: Predefinito/opus/sonnet/fable, effort, modalità: Supervisionato/Auto-edit/Autonomo, quest'ultimo abilitato solo con `allow_bypass`), callout (avviso bypass), button | – |
| Tab **Agente** | message, bubble, chat (classi di layout), marker (notice e righe tool), collapsible (output tool, thinking), badge (esito TurnEnd, costo "≈ stima API", durata), alert (errori, limiti, auth, resume fallito), button (approvazioni, Continua, Nuova sessione), textarea e button (composer, disabilitato durante il turno), kbd (⌘↩), empty | **lista del transcript** (§9.4), **card di approvazione** (tool, input completo, motivo; "Consenti", "Consenti sempre (attempt)" solo se `can_remember`, "Nega", "Nega e ferma", campo messaggio), annidamento dei subagent tramite `parent_tool_use_id`, riga di anteprima digitazione |
| Tab **Modifiche** | collapsible (un file per blocco), badge (A/M/D/R, +/−), button (Aggiorna, Merge, Risolvi con l'agente, Elimina branch), alert (conflitti, target avanti di N commit, checkout del target sporco, HEAD non corretto), skeleton, empty | **viewer diff** (righe con numeri e colori per `LineKind`, "mostra tutto" oltre 2000 righe, segnaposto per binari, file troppo grandi e omessi); dialog* di merge con messaggio modificabile; dialog* di conferma per lo scarto |
| Toast | classi di alert | `Toaster` (Vec in un signal, al massimo 4, rimossi dopo 5 s) |

`*` = componente portato.

**Aggiornamento del tab Modifiche:**
- all'apertura del tab;
- a ogni `changed` del task mentre il tab è visibile (copre la fine del turno);
- con il bottone Aggiorna.

**Tema:** all'avvio si legge `matchMedia('(prefers-color-scheme: dark)')` e si imposta la classe `dark` su `<html>`. Il toggle manuale è rimandato.

### 9.3 Drag-and-drop della kanban (`widgets/dnd.rs`)

- Stato: `DragCtx { dragging: RwSignal<Option<Id>>, drop: RwSignal<Option<(TaskStatus, usize)>> }`.
- Card con `draggable="true"`. `dragstart` → `DataTransfer.set_data("text/plain", id)`, `effectAllowed="move"`.
- **`dragover` sulla colonna:** `prevent_default()`. L'indice di inserimento si calcola da `client_y` rispetto ai punti medi dei figli `[data-task-id]` (`get_bounding_client_rect`). Il signal si aggiorna solo se cambia; una barra segnaposto mostra la posizione.
- **`drop`:**
  - si calcola `before_id`;
  - riordino **ottimistico** locale;
  - `move_task`;
  - in caso di errore: refetch e toast;
  - l'evento `changed` che segue è la versione autorevole.
- Drop su "In corso" senza attempt: apre il dialog Avvia.
- Alternativa da tastiera o menu: `select_native` "Sposta in…" nell'header del pannello.
- Richiede `dragDropEnabled:false` [DA VERIFICARE su WKWebView → M4].

### 9.4 Prestazioni del transcript (`state/transcript.rs`)

- **Store:** `rows: RwSignal<Vec<Row>>` con `Row { idx, entry: RwSignal<Entry> }`, più `index: StoredValue<HashMap<u32, usize>>`.
  - Un upsert di un `idx` esistente con `rev` maggiore fa `row.entry.set(..)`, così si ridisegna solo quella riga.
  - Un nuovo `idx` fa `push`.
  - `<For key=|r| r.idx>`.
- **Finestra:** al massimo 300 righe nel DOM.
  - Oltre quel limite, se l'utente è in fondo, si scartano le più vecchie.
  - "Carica precedenti" chiama `get_entries(before_idx = primo, 100)` e mantiene l'ancora di scroll (`scrollTop += Δ scrollHeight`).
  - Le righe hanno `content-visibility:auto`.
- **Autoscroll:** si considera "in fondo" a meno di 48 px dal fondo. Altrimenti compare la pillola "Vai all'ultimo".
- **Rendering:** testo semplice (`whitespace-pre-wrap`) solo come **text node**; il markdown è rimandato. Output dei tool e thinking sono compressi di default.

---

## 10. Sicurezza

### 10.1 Policy Anthropic

- Si usa solo il CLI non modificato, con il suo login. Il login avviene esclusivamente in Terminal.app tramite `claude auth login`.
- L'app non legge **mai** il Keychain, `~/.claude/.credentials.json` o `~/.claude/projects/*`. Non salva né imposta token e non fa da proxy.
- Nessun `--bare`.
- Il nome del prodotto è "AI Task Manager". La frase "Usa il Claude Code installato sul tuo Mac" compare solo come testo semplice.
- Email e org non vengono salvate.
- La concorrenza di default è 2, nello spirito dell'"uso ordinario individuale" [F].
- Il costo è etichettato come stima.
- La separazione del credit pool per `-p` è **in pausa** [F]: si mostrano limiti e retry, senza ipotesi sulle quote.

### 10.2 Controlli

| Minaccia | Controlli |
|---|---|
| Configurazione del repo eseguita sotto `-p` (hook, env, `apiKeyHelper`, MCP) | Isolated di default (`--setting-sources=user --strict-mcp-config`). Trusted solo con **conferma nativa** e fingerprint ricontrollato a ogni turno: se non corrisponde, il turno gira Isolated con una Notice. Warning quando si aggiunge il progetto |
| Codice del repo eseguito dal git dell'app | Runner del §8.1 |
| Prompt injection che porta a comandi distruttivi | Auto-edit di default (Bash chiede sempre); deny rules via `--settings`; bypass solo con opt-in, conferma nativa e `--allow-dangerously-skip-permissions`; la UI dice chiaramente che il worktree **non è una sandbox** |
| XSS nella webview che abusa dell'IPC | CSP senza `unsafe-inline` negli script; solo text node; nessun `inner_html`; nav guard (plugin con `on_navigation`: consente solo `tauri://localhost`, `http://tauri.localhost` e in dev `http://localhost:1420`); `open_url` solo `http(s)`; **conferme native** (non cliccabili da un XSS) per bypass, Trusted e passthrough della chiave API |
| Fatturazione API silenziosa | Chiavi rimosse dall'env; banner su `authMethod` e `apiProvider`; warning su `apiKeySource` |
| Attacchi di rete | Nessun socket in ascolto in release |
| Fuga di segreti | Log 0600 in dir 0700; valori dell'env mai registrati; transcript solo locali, cancellati con il progetto |
| Iniezione di comandi | Sempre argv; `--flag=value`; prompt solo via stdin come JSON; branch validati; target presi dalla lista dei ref; `-z` e `--` ovunque. Nel `.command` c'è solo il path di claude, con escape |
| Perdita di dati | Commit di snapshot prima di ogni rimozione; mai rimozione automatica di lavoro non mergiato; `update-ref` CAS; `ff-only`; branch tenuti |
| Processi orfani | Process group, stop sequence, `killpg` del gruppo residuo, shutdown ordinato, recovery con kill verificato |

### 10.3 Grep di sicurezza (in `scripts/check.sh`; qualunque match fa fallire)

- **`ui/src`:** `<script`, `inner_html`, `set_inner_html`, `dangerousDisableAssetCspModification`.
- **`crates/`, `src-tauri/src`:**
  - `"--bare"`;
  - `"--dangerously-skip-permissions"`;
  - `find-generic-password`, `SecKeychain`;
  - `TcpListener`, `UdpSocket`, `0.0.0.0`;
  - `credentials.json` fuori dalla costante `DENY_RULES`;
  - `CLAUDE_CODE_OAUTH_TOKEN` fuori dal commento sul passthrough in `claude.rs`.
- **`tauri.conf.json`:** `"csp": null`, `"devtools": true`.

---

## 11. Milestone e delega

### 11.1 Regole comuni per ogni subagent

**Modello e struttura:**
- Ogni subagent è **Opus 5.5**: passare `model: "opus"` a ogni `Agent` e a ogni `agent()` di Workflow, e nelle `meta.phases`.
- Budget di agenti, sotto il tetto di 15 per run: `// MAX_AGENTS = 15; worst case = M0(1) + M1(1) + M2(5) + M3(2) + M4(1) + M5(1) + M6(1) = 12`.
- Le milestone sono **sequenziali**. Dentro M2 e M3 i pacchetti vanno in **parallelo**, ognuno in un **git worktree dedicato** (isolation worktree dell'Agent tool, oppure `git worktree add ../atm-wp-<id> -b wp/<id>`).
- L'orchestratore fa merge in `main` nell'ordine indicato e lancia `scripts/check.sh` dopo ogni merge.
- **Ownership esclusiva dei file:** i conflitti sono impossibili per costruzione.
  - `Cargo.toml` e `Cargo.lock` sono **congelati dopo M1**: tutte le dipendenze sono dichiarate lì.
  - Se ne serve una nuova, il pacchetto la segnala e la aggiunge l'orchestratore.
- **Contratti congelati in M1:** `atm-types`, le firme pubbliche dei moduli di `atm-core`, i comandi registrati, `ui/src/{main.rs, app.rs, ipc/mod.rs, ipc/tauri.rs, ipc/mock/mod.rs, widgets/toast.rs, ui/*, hooks/*}`. Si possono solo **aggiungere** cose, e solo tramite il pacchetto che possiede il file.

**Divieti:**
- mai `claude -p`, `claude auth login` o chiamate API;
- i test costruiscono sempre `Core` con `claude_path = env!("CARGO_BIN_EXE_fake-claude")`;
- mai leggere `~/.claude`;
- mai `inner_html`;
- mai toccare file fuori dalla propria ownership;
- il disco è condiviso (un `target/` per worktree, qualche GB ciascuno): accettato.

**Report finale di ogni agente:** file modificati, output dei comandi di verifica, voci [DA VERIFICARE] chiuse o ancora aperte.

**Verifica standard** (`scripts/check.sh`):
```bash
cargo fmt --all --check
cargo clippy --workspace --exclude atm-ui --all-targets -- -D warnings
cargo clippy -p atm-ui --target wasm32-unknown-unknown -- -D warnings
cargo clippy -p atm-ui --target wasm32-unknown-unknown --features mock -- -D warnings
cargo check -p atm-types --target wasm32-unknown-unknown
cargo test --workspace --exclude atm-ui
# + i grep del §10.3
```
Se M0 conferma che `atm-ui` compila per l'host, si aggiunge anche `cargo test -p atm-ui` per i reducer puri.

### 11.2 Milestone

#### M0: Scaffold e prova di toolchain e IPC (1 agente, sequenziale)

**Scope:**
- Setup del §2.2; `git init`; tutti i file del §2.3.
- `src-tauri` da `cargo tauri init`, adattato.
- Crate `ui` con Tailwind v4, token e `tw-animate.css` vendorizzato.
- Componenti del §9.1 copiati, con `dialog` già portato.
- `ipc/tauri.rs` (§6.6).
- Plugin single-instance (focus su `main`), CSP, capability, nav guard.
- Comandi di debug:
  - `debug_ping{fail}` (Ok o `AppError` tipizzato);
  - `debug_channel_probe` (50 messaggi indicizzati, tre da 20 KiB);
  - `debug_selftest_report{report}`: stampa il JSON su stdout ed esce con 0 o 1.
- In debug con `ATM_SELFTEST=1` la UI esegue da sola le prove e conta gli eventi `securitypolicyviolation`.
- Una pagina demo con i componenti.
- `scripts/check.sh` e `.gitignore`.

**File posseduti:** tutti.

**Accettazione:**
1. `scripts/check.sh` verde.
2. `cd ui && trunk build --release` passa con i flag wasm-opt, oppure con il fallback `data-wasm-opt="0"` documentato nel README.
3. `grep -q 'bg-success' ui/dist/*.css`.
4. `cargo tauri build --debug --no-bundle && out=$(ATM_SELFTEST=1 ./target/debug/ai-task-manager) && echo "$out" && grep -qF '"csp_violations":0' <<<"$out"` esce con 0, con report `{ping_ok, ping_err_typed, channel_50_in_order, csp_enforced, dialog_ok, csp_violations: 0}` (asset incorporati, quindi CSP attiva). Stessa forma (exit code + report su stdout) per i selftest di M3, M4 e M6.
5. `cargo tauri dev` mostra la demo con button, badge, tabs e dialog portato correttamente stilizzati.

**Chiude:** Trunk su 1.97.1, flag wasm-opt, download di Tailwind, compilazione dei binding, costruttore del Channel, CSP, componenti su stable, flag di `cargo tauri init`, compilazione di `atm-ui` per l'host.

#### M1: Contratti congelati (1 agente, sequenziale)

**Scope:**
- `atm-types` completo (§6.2), con test di round-trip serde e snapshot insta del JSON di ogni enum.
- `api.rs` con i marker di tutti i comandi del §6.3.
- `atm-core`:
  - migrazione `0001_init.sql` (§5.2);
  - tutti i moduli con firme pubbliche e corpi che restituiscono `AppError{NotImplemented}`;
  - `[[bin]] fake-claude` come stub;
  - `Core` con un metodo per comando più `new(CoreConfig, notify: Arc<dyn Fn(AppEvent)+Send+Sync>)`, `startup()`, `shutdown()`, `drop_subscriptions()`;
  - sink del transcript: `Box<dyn Fn(TranscriptMsg) -> bool + Send + Sync>`.
- `src-tauri`: ogni comando registrato e delegato.
- `ui`:
  - `app.rs` con `AppCtx` e layout;
  - viste stub con nomi e prop definitivi;
  - `ipc/mod.rs` con `call::<C>()`;
  - `ipc/mock/mod.rs` che smista per nome comando verso `board.rs` e `attempt.rs` (stub);
  - Toaster.
- **Tutte** le dipendenze del workspace dichiarate.

**Accettazione:**
1. check.sh verde.
2. `cargo test -p atm-types`.
3. `trunk build` con e senza `--features mock`.
4. `cargo tauri dev` si avvia; `get_env` produce un toast "NotImplemented".

#### M2: Librerie e UI in mock (5 agenti in parallelo; merge in quest'ordine: DB → GIT → CLAUDE → UI-BOARD → UI-TASK)

| Pacchetto | Possiede | Scope | Accettazione (`cargo test -p atm-core --test <x>` + check.sh) |
|---|---|---|---|
| **M2-DB** | `atm-core/src/db.rs`, `migrations/**`, `tests/db.rs` | open, migrate, query (progetti, task, board join, attempt, process, entries upsert/tail/before, settings), posizioni, `mark_orphans` | migrazione su `:memory:`; cascade delle FK; CHECK rifiutati; indici parziali (secondo attempt attivo e secondo process `running` rifiutati); 500 spostamenti casuali mantengono l'ordine stretto con rinumerazione; upsert idempotente con `rev`; paginazione |
| **M2-GIT** | `atm-core/src/git.rs`, `tests/git.rs`, `tests/common/**` | §8 tranne il fingerprint (M6): runner, validazione del repo, branch, add/remove/riconciliazione worktree, auto-commit, diff snapshot con parser dei hunk, branch status, squash merge, slug | Repo temporanei con una **gitconfig globale ostile** (hooksPath e fsmonitor che scrivono un marker): il marker non compare mai. 10 worktree concorrenti; path con spazi e unicode; `worktree prune` dell'utente non tocca i worktree bloccati; diff con modifica, rinomina, cancellazione, file non tracciato, binario, `too_large`, `omitted`; l'indice dell'agente resta invariato (mtime e hash); merge con target non in checkout (`update-ref`, fallimento CAS gestito), in checkout e pulito (`ff-only`), con sovrapposizione sporca (`TargetCheckoutDirty`, nessun byte cambiato), target divergente senza conflitti (merge ok), conflitto (nessun ref toccato), `NothingToMerge`; tutto con `LANG=it_IT.UTF-8` |
| **M2-CLAUDE** | `atm-core/src/{claude,wire,normalize}.rs`, `src/bin/fake-claude.rs`, `tests/{claude,normalize}.rs`, `tests/fixtures/**` | §7.1–7.6, §7.8 (funzioni pure), §7.10 (probe, script di login), `killpg`; fake-claude del §12.1 | snapshot insta dell'argv (primo turno, resume, isolated/trusted, bypass abilitato, model+effort): contiene `--permission-mode=` e mai `--bare`; golden del normalizer per ogni riga della tabella del §7.6; parse di `auth status` (dentro e fuori); gate di versione; `approval_response` (riscrittura della destination, regole a tool intero escluse); riga da 20 MiB saltata con lo stream che prosegue; spawn contro fake-claude: l'env registrato non contiene `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `GIT_DIR` e `PWD` = cwd; il PATH della login shell viene estratto dai marker |
| **M2-UI-BOARD** | `ui/src/views/{onboarding,sidebar,board,task_dialog,settings}.rs`, `widgets/dnd.rs`, `state/board.rs`, `ipc/mock/board.rs` | Gate, sidebar, board con DnD, dialog task, impostazioni | clippy wasm con mock; `trunk serve --features mock`: creazione di un task, drag tra colonne e dentro una colonna (l'ordine persiste nel mock), tutti gli stati dell'onboarding; con browser automatizzato se disponibile (screenshot), altrimenti una checklist per l'orchestratore |
| **M2-UI-TASK** | `ui/src/views/{task_panel,start_dialog,transcript,approval,composer,diff,merge_dialog}.rs`, `state/transcript.rs`, `ipc/mock/attempt.rs`, `ipc/mock/fixtures/**` | §9.2 (Agente, Modifiche), §9.4 | Il mock riproduce le fixture `TranscriptMsg` (simple, approval, flood da 10k); consenti, consenti sempre, nega e nega-e-ferma aggiornano le entry; con `flood`, `document.querySelectorAll('[data-entry]').length ≤ 300`; "Carica precedenti" mantiene l'ancora; il diff mostra aggiunte, rimozioni, rinomine, binari, `too_large` e omessi; il dialog di merge mostra Conflicts e TargetCheckoutDirty |

#### M3: Orchestrazione e guscio Tauri (2 agenti in parallelo; merge CORE → TAURI)

| Pacchetto | Possiede | Scope | Accettazione |
|---|---|---|---|
| **M3-CORE** | `atm-core/src/{lib,runner,live}.rs`, `tests/flow.rs` | Servizi (§6.3), §7.7–7.9, §6.5, recovery, shutdown, cap, pausa, cache env, apertura via `open` | `tests/flow.rs` con fake-claude, repo temporaneo e un sink che registra gli eventi. Sequenza ordinata: `start_attempt` → worktree presente → Snapshot → entry → approvazione pendente → `respond_approval(Allow{remember:true})` → `allow_rules` salvate e presenti nel `--settings` del turno successivo → result → auto-commit (`head_after`) → task in inreview → `get_diff` mostra `hello.txt` → il follow-up usa `--resume=` con lo stesso id → merge → task done e worktree rimosso. Casi `noinit` (nuovo `--session-id` al turno dopo), `hang` con interrupt rispettato (killed in ≤ 5 s), `hang_ignore` (SIGKILL in ≤ 13 s, `kill -0 pgid` fallisce, il nipote `sleep` è morto), `crash` (failed/crash con stderr catturato), `control` (risposta di errore registrata da fake-claude), `usage_limit` (pausa, poi `UsageLimited`), `auth_fail` (env invalidato), cap 2 (il terzo avvio dà `ConcurrencyLimit`), `auth status` con exit 1 (`NotLoggedIn` senza spawn), drop del runtime a metà turno e nuovo Core (`failed/app_restart`, gruppo verificato ucciso, task in inreview), ordine dello snapshot con upsert concorrenti, Lagged (Snapshot inviato di nuovo) |
| **M3-TAURI** | `src-tauri/**` | Corpi sottili dei comandi; sink `app.emit` e `Channel`; `on_page_load(Started)` → `drop_subscriptions`; nav guard; single-instance; `pick_repo_folder` in `spawn_blocking`; helper `confirm_native(title, msg) -> bool` (dialog plugin, `blocking_show` fuori dal main thread); `ExitRequested` (§7.9); creazione delle dir dati con permessi; selftest esteso con subscribe, unsubscribe e reload | clippy; `cargo tauri build --debug --no-bundle`; selftest con exit 0; manuale: `cargo tauri dev` arriva all'onboarding con dati veri |

#### M4: Integrazione (1 agente)

**Scope:**
- UI collegata all'IPC reale.
- Percorso completo con `ATM_CLAUDE_PATH=$PWD/target/debug/fake-claude cargo tauri dev` (§12.2).
- Verifiche nel bundle di debug: DnD su WKWebView, `Channel` con messaggi oltre 8 KiB, reload (Cmd+R) che fa ripartire la subscription (numero di forwarder = 0 dopo il reload, poi la vista si ripristina), CSP.
- Launcher `.command` su macOS 27 (Gatekeeper).
- Correzioni trasversali, minime ed elencate nel report.

**Accettazione:**
- Tutta la checklist del §12.2 passa con fake-claude.
- check.sh verde.

#### M5: Validazione con il CLI reale (utente più 1 agente; **serve l'OK esplicito dell'utente**, consuma quota dell'abbonamento)

- L'**utente** esegue la checklist del §12.3 su un repo giocattolo.
- L'app conserva i raw log.
- L'agente poi:
  - ripulisce i log (email, org, path home) e li salva in `atm-core/tests/fixtures/real/`;
  - aggiorna i golden e, se servono, `LIMIT_PATTERNS`, argv e normalizer;
  - chiude le voci [DA VERIFICARE] del §13.3, aggiornando la tabella nel README.

**Accettazione:** checklist completata e test verdi sulle fixture reali.

#### M6: Hardening, sicurezza e packaging (1 agente)

**Scope:**
- Policy Trusted con fingerprint (§8.9) e `set_project_security`.
- Opt-in del bypass (modalità Autonoma abilitata nel dialog Avvia).
- Passthrough della chiave API, tutti con conferma nativa.
- Warning di progetto.
- README: setup, "usa il Claude Code installato", login, modalità di permesso, "il worktree non è una sandbox", fallback wasm-opt.
- Icona (`cargo tauri icon`), `cargo tauri build` che produce `.app` e `.dmg`.
- Verifica prestazioni con `flood` e 3 agenti fake.
- Controllo finale dei grep.
- Probe di debug solo sotto `cfg(debug_assertions)`.

**Accettazione:**
1. Fixture di repo malevolo (hook e server `.mcp.json` che scrivono un marker, `apiKeyHelper`): in Isolated fake-claude registra i flag di isolamento; in Trusted, modificare `.claude/settings.json` nel worktree fa girare il turno successivo Isolated con Notice.
2. Uscita con 2 agenti fake attivi: `pgrep -f fake-claude` vuoto.
3. Il `.app` di release lanciato dal Finder trova `claude` e `git` (PATH).
4. 0 violazioni CSP su tutte le schermate (selftest di release: build con flag `--debug` solo per la console, poi controllo manuale nella release).
5. Dopo una reinstallazione DB, log e worktree sono ancora lì.

---

## 12. Verifica end-to-end

### 12.1 `fake-claude` (Rust, nessuna chiamata API)

- **Sottocomandi:**
  - `--version` / `-v` → `2.1.283 (Claude Code)`;
  - `auth status [--json]` → JSON, con `FAKE_CLAUDE_AUTH=in|out` che decide exit 0 o 1.
- **Modalità `-p …`:**
  - registra argv, cwd, `$PWD` e la presenza di `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT` e `GIT_DIR` in `$FAKE_CLAUDE_RECORD` (una riga JSON per chiamata);
  - risponde a `initialize` (tranne `noinit`);
  - a ogni messaggio utente esegue lo scenario scelto con `[fake:NOME]` nel testo, altrimenti `$FAKE_CLAUDE_SCENARIO`, altrimenti `simple`;
  - esce con 0 all'EOF di stdin.
- **Scenari:**

| Scenario | Comportamento |
|---|---|
| `simple` | init → delta di stream → testo → `tool_use` Write (scrive davvero `hello.txt`) → `tool_result` → `result` success |
| `approval` | Come `simple`, ma prima emette `can_use_tool` per Bash e si blocca finché non arriva la risposta |
| `slow` | Un evento al secondo |
| `hang` | init e poi attesa |
| `hang_ignore` | Ignora interrupt e SIGTERM; avvia un nipote `sleep 300` nello stesso gruppo |
| `crash` | Exit 1 senza `result` |
| `noinit` | Esce prima di `init` |
| `big` | `tool_result` da 50 KiB più una riga da 20 MiB |
| `flood` | 10.000 eventi |
| `control` | Invia un `hook_callback` e registra la risposta |
| `usage_limit` | `result` con testo di limite d'uso |
| `auth_fail` | `result` con testo "Not logged in · Please run /login" |
| `resolve_merge` | `git merge <FAKE_CLAUDE_TARGET>`, risolve concatenando le versioni, `git commit --no-edit` |

- Sull'interrupt risponde success ed emette `result` `error_during_execution` (tranne `hang_ignore`).
- Le forme JSON seguono i report di ricerca; M5 le sostituisce con catture reali.

### 12.2 Percorso utente su fake-claude (M4)

1. Avvio con `FAKE_CLAUDE_AUTH=out` → gate "Accedi". Si imposta `in` e si preme "Ricontrolla" → board.
2. "Accedi" apre Terminal con lo script, eseguendo `auth login` di fake-claude.
3. Aggiunta di un repo temporaneo con un commit. Una cartella non git, un repo bare e un repo vuoto vengono rifiutati; un repo con `.mcp.json` mostra un warning.
4. Creazione di due task, trascinamento e riordino, riavvio dell'app: l'ordine resta.
5. Avvio "Crea hello `[fake:approval]`" in Auto-edit → spinner → card di approvazione → "Consenti sempre" → streaming → TurnEnd → badge "In revisione".
6. Tab Modifiche: `hello.txt` aggiunto.
7. Follow-up `[fake:simple]` → `--resume` nel record. Stop durante `[fake:slow]` → killed entro 5 s.
8. Uscita con Cmd+Q durante `[fake:hang]` → nessun processo residuo. Al riavvio c'è "Interrotto – Continua"; "Continua" usa `--resume`.
9. Merge con il target in checkout e pulito → task Fatto, worktree rimosso, `git log` del target mostra il commit squash.
10. Secondo task che crea un conflitto → "Conflitti" → "Risolvi con l'agente" `[fake:resolve_merge]` → merge ok.
11. Scarta un attempt → worktree rimosso, branch presente, task in Da fare.
12. `[fake:usage_limit]` → banner di pausa → "Riprendi". `[fake:auth_fail]` → torna il gate.

### 12.3 Checklist con il CLI reale (M5, eseguita dall'utente)

1. Repo giocattolo con un commit. Task "Aggiungi una riga al README". Avvio in Auto-edit.
2. Annotare: entry `SessionInit` (`apiKeySource`, `permissionMode`, `mcp_servers` = 0 in Isolated), streaming, `TurnEnd` con costo.
3. Chiedere all'agente di eseguire `ls` → arriva `can_use_tool` → Consenti. Poi "Consenti sempre" su un altro comando e verificare che il turno successivo non chieda più.
4. Follow-up → il resume funziona (il contesto è ricordato).
5. Stop a metà turno → annotare il subtype del `result` dopo l'interrupt.
6. Uscire dall'app durante un turno, riavviare, "Continua".
7. Chiedere "esegui git push" → **negato** (conferma che il deny in `--settings` funziona).
8. Repo con `.claude/settings.json` il cui hook scrive un marker e con un CLAUDE.md che contiene una parola chiave: in Isolated il marker non compare. Annotare se l'agente conosce la parola chiave (voce E11).
9. Merge.
10. Consegnare i raw log all'agente per creare le fixture.

### 12.4 Controlli automatici continui

- `scripts/check.sh`: unit test, integrazione con fake-claude e repo temporanei, grep.
- `ATM_SELFTEST=1` nel bundle di debug (M0, M3, M4).
- `trunk build --release` più il grep su `bg-success` a ogni milestone che tocca la UI.

---

## 13. Rischi, cose rimandate, voci da verificare

### 13.1 Rischi

| # | Rischio | Mitigazione |
|---|---|---|
| R1 | Control protocol non documentato (`initialize`, `can_use_tool`, `--permission-prompt-tool stdio` nascosto) | Superficie minima (initialize, user, interrupt, `can_use_tool`); gate di versione; parsing tollerante (`Value`); fake-claude; golden reali in M5. Fallback rimandato: `--permission-prompts none` più `acceptEdits` (nel README se M5 lo rende necessario) |
| R2 | Toolchain WASM (Trunk 0.21.14, wasm-opt, Tailwind) | Chiusa in M0; fallback `data-wasm-opt="0"` |
| R3 | Un worktree non è una sandbox; il bypass concede molto | Auto-edit di default, deny rules, bypass opt-in con conferma nativa, testo chiaro in UI e README |
| R4 | PATH nelle app lanciate dal Finder | Import con `-ilc` e marker, fallback, override del path di claude; verifica in M6 |
| R5 | Auto-update del CLI e deriva del protocollo | Path del symlink; versione salvata per process; golden test; nessun errore sugli eventi sconosciuti |
| R6 | Il credit pool di `-p` potrebbe cambiare | Nessuna ipotesi sulle quote; limiti e retry visibili; cap di 2 |
| R7 | La chiusura di stdin dopo `result` termina il lavoro in background (Bash, dev server) | Limite documentato. Opzione futura: frame `idle` con `CLAUDE_CODE_SDK_READS_SESSION_STATE=1` |
| R8 | Isolated potrebbe non caricare CLAUDE.md (E11) | Append-prompt che fa leggere CLAUDE.md o AGENTS.md; verifica in M5 |
| R9 | Rust/UI Leptos è ormai il ramo secondario (il sito principale è passato a Dioxus) | Componenti vendorizzati e pinnati, commit annotato, intestazioni `ATM-PORT` |
| R10 | Nessun E2E automatico su WKWebView | Logica in `atm-core` testata con fake-claude; UI in mock nel browser; selftest; checklist manuali |
| R11 | I worktree non hanno `node_modules`, `.env` e cache | Accettato in v1 (i primi turni sono più lenti); script di setup e `copy_files` rimandati |
| R12 | Transcript del CLI in `~/.claude/projects/<worktree>` | Documentato; l'app non li tocca per policy |

### 13.2 Rimandati (con il trigger che li riporta in scope)

| Rimandato | Quando aggiungerlo |
|---|---|
| Plan mode ed ExitPlanMode, risposte ad AskUserQuestion | Quando servono flussi di pianificazione |
| Cambio della modalità di permesso a turno in corso | Richiesta degli utenti |
| Follow-up in coda o inseriti a metà turno | Quando gli utenti scrivono mentre l'agente lavora |
| Rendering markdown (pulldown-cmark, HTML come testo) | Lamentele sulla leggibilità |
| `--replay-user-messages`, rewind, `--fork-session` | Funzione "torna a un turno precedente" |
| Più attempt attivi per task (confronto tra modelli) | Dopo il MVP |
| Rebase, abort e continue | Se il merge-tree con squash non basta |
| Script di setup e cleanup, `copy_files` (.env) | Quando gli agenti sprecano turni a fare bootstrap |
| Ricreazione di un worktree mancante | Primo caso reale |
| PR, push e fetch (`gh`) | Quando serve un flusso con remoto |
| Notifiche OS, toggle del tema, conferma all'uscita | Molti agenti in background, preferenze degli utenti |
| Retention dei log, backup del DB prima delle migrazioni, timeout delle approvazioni, abbinamento PID via sysinfo | Crescita dei dati o bug reali |
| Report di trust completo (UI con anteprima dei file) | Dopo M6, se il fingerprint minimale non basta |
| Diff inline delle Edit nel transcript, evidenziazione della sintassi | Dopo la v1 |
| Linux e Windows, altri agenti (Codex, …), firma e notarizzazione, `freezePrototype` | v2 o distribuzione |

### 13.3 Voci da verificare, e dove si chiudono

| Voce | Dove |
|---|---|
| Build di Trunk su 1.97.1; flag wasm-opt; Tailwind 4.3.3; compilazione dei binding; costruttore del Channel; compilazione di `atm-ui` per l'host; flag di `cargo tauri init` | M0 |
| Drag-and-drop HTML5 su WKWebView con `dragDropEnabled:false`; `.command` in Terminal e Gatekeeper | M2-UI-BOARD / M4 |
| `process_group` più `killpg`; kill verificato con `ps` | M3-CORE |
| Obbligatorietà di `--verbose`; ordine e necessità di `initialize`; subtype di `result` dopo un interrupt; valori di `apiKeySource`; forma degli eventi di rate limit e usage limit | M5 |
| Deny in `--settings` efficace; `--strict-mcp-config` senza `--mcp-config` dà 0 server; `--setting-sources=user` esclude hook di progetto e CLAUDE.md | M5 |
| Le regole con destination `session` sopravvivono a `--resume` (le ripassiamo comunque) | M5 |
| Nome della directory del progetto nel CLI: realpath o `$PWD` (per noi è la stessa stringa) | M5 |

---

## 14. Domande aperte per l'utente

1. Identificatore del bundle `dev.aitaskmanager.desktop` e nome "AI Task Manager": vanno bene? Il nome non deve contenere "Claude Code".
2. Modalità di default **Auto-edit** (`acceptEdits`) e concorrenza **2**: confermi?
3. Root dei worktree `~/.ai-task-manager/worktrees`: va bene? È configurabile.
4. Dopo il merge: worktree rimosso e **branch tenuto**. Oppure vuoi la cancellazione automatica del branch?
5. Consenti la validazione con il CLI reale in M5, che consuma quota dell'abbonamento?
6. L'uso è personale o l'app verrà distribuita ad altri? In caso di distribuzione si pone la questione dei Commercial Terms per "offrire Claude Code in un prodotto". Non cambia l'architettura.
7. È confermato che in v1 sono esclusi: push, fetch e PR, immagini nei prompt, server MCP per i task, rewind?