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
| E4 | A e B: token da una "pagina di installazione" o da start-tauri, entrambe senza `--success/--warning/--info` | I blocchi `:root`, `.dark`, `@theme inline` e `@layer base` hanno le chiavi di **rust-ui/leptos-ui `style/tailwind.css`** [V]; dal 2026-09-30 i valori sono i token del design (Claude Design "Direzione visiva", §9.2), più `--status-*` e `--destructive-foreground` |
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
| D1 | Tauri 2.12 con Leptos 0.8.21 CSR compilato da Trunk 0.21.14. Nessun server HTTP o WebSocket; nessun socket in ascolto. L'unico server MCP (`atm`, i tool board degli agenti, round 2026-09-30) è **in-process**: il CLI lo raggiunge sulla stessa stdio del control protocol (`control_request` `mcp_message`, §7.4), senza processi, socket o porte | Tauri supporta Leptos solo in CSR/SSG [F]. Nessuna superficie di attacco su localhost |
| D2 | 4 package: `atm-types` (contratto serde, wasm e host), `atm-core` (tutta la logica, senza tipi Tauri), `src-tauri` (guscio sottile), `ui` (Leptos). `fake-claude` è un `[[bin]]` dentro `atm-core` | Pochi crate. Il core è testabile senza webview. Nessun trait executor |
| D3 | **Un processo `claude -p` per turno.** stdin resta aperto durante il turno (control protocol) e si chiude dopo `result`. Follow-up e ripresa dopo un riavvio usano `--resume=<uuid>` | Un attempt inattivo non costa nulla (~1 GiB per processo vivo [F]). Un solo percorso di codice per follow-up e recovery |
| D4 | Il transcript normalizzato si scrive in **SQLite prima** del `broadcast`. Il raw JSONL resta solo per diagnostica e fixture. Nessuna ri-normalizzazione in visualizzazione | Memoria limitata; nessun OOM come in Vibe Kanban [F]. Ordine snapshot + live corretto per costruzione (§6.5) |
| D5 | Sincronizzazione della board: un evento `changed{project_id, task_id}` fa fare refetch alla UI. Il transcript usa un `Channel` per vista, con snapshot iniziale e upsert idempotenti (`idx`, `rev`) | Elimina i bug di patch e riconnessione (Vibe Kanban #3343, #3227 [F]) |
| D6 | Permessi: default **Auto-edit (`acceptEdits`)**, con `--permission-prompt-tool stdio` e approvazioni in UI. **Supervisionato** = `default`. **Autonomo** = `bypassPermissions`, solo con opt-in per progetto, conferma nativa e `--allow-dangerously-skip-permissions`. Plan mode e AskUserQuestion sono rimandati | Meno approvazioni ripetitive. Un worktree non è una sandbox |
| D7 | Configurazione Claude del repo **Isolata** di default: `--setting-sources=user --strict-mcp-config`. "Trusted" richiede una conferma nativa e un fingerprint di `.claude/**` + `.mcp.json` del commit da cui partono i worktree (tip del branch target), ricontrollato a ogni turno; mai approvata una configurazione che fattura fuori dall'abbonamento (§8.9) | Sotto `-p` i hook, `env`, `apiKeyHelper` e i server MCP del progetto partono senza trust dialog [F] |
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
| Esterni | claude CLI: minimo `2.1.223`, testato `2.1.283`. git: minimo `2.44` (`merge-tree --write-tree` da 2.38; `GIT_NO_LAZY_FETCH` da 2.44, §8.1) |

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
@custom-variant dark (&:is(.dark, .dark *));
/* + blocchi :root, .dark, @theme inline, @layer base con le chiavi di rust-ui/leptos-ui style/tailwind.css
   (commit annotato in ui/src/ui/VENDORED.toml) e i valori di Claude Design "Direzione visiva" (commento in testa
   al file; la nota [css] di VENDORED.toml precede il redesign). NON copiare i suoi @import di index.css,
   Animate.css, highlight_code.css */
```

**`src-tauri/tauri.conf.json`** (parti chiave)
```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "AI Task Manager",
  "identifier": "dev.aitaskmanager.desktop",
  "build": {
    "beforeDevCommand":   { "script": "trunk serve --features testkit", "cwd": "../ui", "wait": false },
    "devUrl": "http://localhost:1420",
    "beforeBuildCommand": { "script": "trunk build --release", "cwd": "../ui" },
    "frontendDist": "../ui/dist"
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [{ "label": "main", "title": "AI Task Manager", "width": 1440, "height": 900,
                  "minWidth": 1024, "minHeight": 640, "visible": false, "dragDropEnabled": false }],
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
- `visible:false`: il guscio mostra la finestra solo dopo `Core::startup` (recovery), quindi niente finestra bianca bloccata; se `Core::new` fallisce compare solo un dialog nativo d'errore e l'app esce con 1.

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
│   Onboarding │ Sidebar │ Riepilogo │ Kanban/Lista │ TaskPanel [Agente | Modifiche] │ Impostazioni   │
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
                                                                        attachments/<project>/<task>/…
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
├─ scripts/{e2e.sh, release.sh, bump-version.sh}  # E2E (§12.2), bundle locale, nuova versione (§9.5)
├─ .github/workflows/release.yml          # release su un tag v*: build macOS, firma updater, latest.json (§9.5)
├─ crates/
│  ├─ atm-types/  src/{lib.rs, model.rs, transcript.rs, review.rs, api.rs, error.rs, debug.rs,
│  │              #   update.rs (versione e aggiornamenti, §9.5)}
│  │              # solo serde + serde_json; compila per wasm32 e host
│  └─ atm-core/
│     ├─ Cargo.toml                       # [lib] + [[bin]] fake-claude
│     ├─ migrations/{0001_init.sql, 0002_overview_attachments_subagents.sql, 0003_subtasks.sql,
│     │                                   #   0004_autopilot.sql, 0005_planner.sql}
│     ├─ src/lib.rs                       # Core, CoreConfig, startup/recovery/shutdown, 1 metodo per comando
│     ├─ src/attachments.rs               # allegati dei task: layout su disco, staging, copia (round 2026-09-29)
│     ├─ src/db.rs                        # rusqlite: open, migrate, 1 fn per query, posizioni
│     ├─ src/git.rs                       # runner irrobustito + worktree/commit/diff/status/merge/fingerprint
│     │                                   # + git/overview.rs: riepilogo del progetto dal tip del target
│     ├─ src/claude.rs                    # discovery, versione, PATH login-shell, ChildEnv, argv, auth, login .command, spawn/killpg
│     ├─ src/wire.rs                      # reader di righe con cap, parse Inbound, frame outbound, approval_response()
│     ├─ src/normalize.rs                 # Normalizer puro (Value → EntryOp)
│     ├─ src/runner.rs                    # ciclo di vita del turno, approvazioni, stop, finalize
│     ├─ src/autopilot.rs                 # scheduler dell'autopilota: coda, fine turno, correzioni, merge (§7.12)
│     ├─ src/autopilot/verify.rs          # verifica: /bin/sh -c <verify_command> nel worktree (§7.12)
│     ├─ src/plan.rs                      # pianificatore: avvio, fine turno, conferma, recovery (§7.13)
│     ├─ src/live.rs                      # broadcast per attempt, subscription e forwarder
│     ├─ src/bin/fake-claude.rs           # double del CLI (mai incluso nel bundle)
│     └─ tests/{common/mod.rs, db.rs, git.rs, claude.rs, normalize.rs, flow.rs, fixtures/**}
├─ src-tauri/
│  ├─ Cargo.toml build.rs tauri.conf.json capabilities/default.json icons/
│  └─ src/{main.rs, lib.rs, commands.rs, confirm.rs, selftest.rs, e2e.rs, updater.rs (§9.5)}
└─ ui/                                    # package atm-ui (bin), feature `mock`
   ├─ Cargo.toml Trunk.toml index.html public/ style/{tailwind.css, vendor/tw-animate.css}
   └─ src/
      ├─ main.rs                          # mod ui; mod hooks; mod ipc; mod state; mod views; mod widgets; mount
      ├─ app.rs                           # AppCtx, gate onboarding, layout
      ├─ ipc/{mod.rs, tauri.rs, mock/{mod.rs, board.rs, attempt.rs, overview.rs, attachments.rs,
      │       plan.rs, update.rs, fixtures/*.json (anche plan.json)}}
      ├─ state/{board.rs, transcript.rs}
      ├─ views/{onboarding, sidebar, board, task_dialog, settings, task_panel, start_dialog,
      │         transcript, approval, composer, diff, merge_dialog, overview, planner, update}.rs
      │         + views/board/list.rs (vista Lista), views/settings/project.rs (pagina Impostazioni)
      ├─ widgets/{toast.rs, dnd.rs, context_menu.rs}
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
| Log | `app_data_dir()/logs/<attempt>/<process>/{stdout.jsonl, stderr.log, stdin.jsonl}` | 0600; cancellati con il task o il progetto |
| Allegati dei task | `app_data_dir()/attachments/<project_id>/<task_id>/<attachment_id>/<nome>` (`atm_core::attachments`) | file 0600 (`create_new`), dir 0700; fuori da ogni worktree e da git; cancellati con il task o il progetto |
| Worktree | `~/.ai-task-manager/worktrees/<attempt_uuid>` | 0700; path senza spazi |
| Script di login | `app_cache_dir()/claude-login.command` | 0700, cancellato a fine polling |

**Pulizia di allegati e log** (round feature del 2026-09-29): nessuna cascade del DB arriva ai file. `delete_task` e `remove_project` leggono gli id degli attempt (`task_attempt_ids`, `project_attempt_ids`, chiusi compresi) prima di cancellare le righe; dopo il commit rimuovono la cartella degli allegati (`attachments/<project_id>/<task_id>`, o `attachments/<project_id>` per il progetto) e `logs/<attempt_id>` di ogni attempt. È best effort, in `spawn_blocking`: un errore finisce sullo stderr dell'app (`eprintln!`) e non fa fallire il comando. Prima di questo round i log grezzi sopravvivevano, benché il dialog di rimozione prometta di cancellare la cronologia.

---

## 5. Modello dati

### 5.1 Connessione e migrazioni

- A ogni apertura: `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA synchronous=NORMAL;`.
- Le migrazioni sono `&[include_str!(...)]`: `0001_init.sql`, `0002_overview_attachments_subagents.sql` (round feature del 2026-09-29), `0003_subtasks.sql` (round del 2026-09-30), `0004_autopilot.sql` (round del 2026-10-01) e `0005_planner.sql` (round del 2026-10-02). Si applicano quelle con indice ≥ `PRAGMA user_version`, ciascuna dentro una transazione; un DB con uno `user_version` più alto di quelle note (app più vecchia) viene rifiutato.
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

**Migrazione 0002** (`crates/atm-core/migrations/0002_overview_attachments_subagents.sql`, round feature del 2026-09-29): descrizione del progetto, modello e limite dei sub-agent, allegati dei task. `ADD COLUMN` controlla i CHECK sulle righe esistenti, e i default li soddisfano; il test `migrates_a_v1_database_with_rows_to_v2` parte da un DB con la sola 0001 e le righe dentro.

```sql
ALTER TABLE projects ADD COLUMN description TEXT NOT NULL DEFAULT ''
  CHECK (length(description) <= 10000);

ALTER TABLE attempts ADD COLUMN subagent_model TEXT;       -- alias di MODEL_ALIASES; NULL = default del CLI
ALTER TABLE attempts ADD COLUMN max_subagents INTEGER
  CHECK (max_subagents IS NULL OR max_subagents BETWEEN 0 AND 10);   -- NULL = nessun limite
ALTER TABLE attempts ADD COLUMN subagents_used INTEGER NOT NULL DEFAULT 0
  CHECK (subagents_used >= 0);

CREATE TABLE task_attachments (                          -- file in data_dir/attachments/, mai nel worktree
  id         TEXT PRIMARY KEY,
  task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
  size       INTEGER NOT NULL CHECK (size >= 0),
  created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX task_attachments_task ON task_attachments(task_id, created_at);
```

- Il path della copia non si salva: lo calcola il Core dagli id e dal nome (`atm_core::attachments::attachment_path`).
- Al massimo `MAX_ATTACHMENTS_PER_TASK` (20) allegati per task: `Db::insert_attachments` inserisce e riconta nella stessa transazione, `Invalid` oltre.
- `Db::count_subagent` conta uno spawn solo finché `subagents_used < max_subagents` (in una sola istruzione).
- Le righe degli allegati spariscono con il task o il progetto (cascade). File e log restano del chiamante, che legge prima gli id (`project_task_ids`, `project_attempt_ids`, `task_attempt_ids`).

**Migrazione 0003** (`crates/atm-core/migrations/0003_subtasks.sql`, round del 2026-09-30): sotto task e avvio da parte degli agenti (strumenti board). Le righe esistenti restano task di primo livello e attempt avviati dall'utente (`NULL`); il test `migrates_a_v2_database_with_rows_to_v3` parte da un DB con 0001 e 0002 e le righe dentro.

```sql
ALTER TABLE tasks ADD COLUMN parent_id TEXT REFERENCES tasks(id) ON DELETE CASCADE;  -- NULL = task di primo livello
CREATE INDEX tasks_parent ON tasks(parent_id);

ALTER TABLE attempts ADD COLUMN started_by_attempt TEXT;  -- attempt che l'ha avviato con start_task; NULL = l'utente
```

- **Un solo livello**: il padre sta nello stesso progetto e non è a sua volta un sotto task. Il DB salva `parent_id` così com'è (solo la FK); la regola è del Core (`Invalid`).
- Eliminare il padre elimina i figli per cascade, con i loro attempt, processi, voci e allegati. Worktree, file e log dei figli restano del chiamante, che legge prima gli id (`task_children`, poi `task_attempt_ids` di ciascuno).
- `started_by_attempt` è testo semplice, senza FK: eliminare l'attempt che l'ha avviato non tocca la riga. Si scrive solo all'inserimento (`AttemptRow::started_by_attempt` in `begin_attempt`).
- Le card contano i figli con due sottoquery correlate (`subtasks_done` = figli in `done`, `subtasks_total` = tutti); `Db::subtask_cards` dà le card dei figli nell'ordine della board, per `TaskDetail::subtasks`.

**Migrazione 0004** (`crates/atm-core/migrations/0004_autopilot.sql`, round del 2026-10-01): l'autopilota per progetto. Le righe esistenti hanno l'autopilota spento, i default qui sotto e nessuna verifica; il test `migrates_a_v3_database_with_rows_to_v4` parte da un DB con 0001–0003 e le righe dentro.

```sql
ALTER TABLE projects ADD COLUMN autopilot INTEGER NOT NULL DEFAULT 0 CHECK (autopilot IN (0, 1));
ALTER TABLE projects ADD COLUMN autopilot_merge INTEGER NOT NULL DEFAULT 0 CHECK (autopilot_merge IN (0, 1));
ALTER TABLE projects ADD COLUMN verify_command TEXT
    CHECK (verify_command IS NULL OR length(verify_command) BETWEEN 1 AND 1000);  -- NULL = nessuna verifica
ALTER TABLE projects ADD COLUMN verify_timeout_secs INTEGER NOT NULL DEFAULT 600
    CHECK (verify_timeout_secs BETWEEN 10 AND 3600);
ALTER TABLE projects ADD COLUMN autopilot_max_fixes INTEGER NOT NULL DEFAULT 2
    CHECK (autopilot_max_fixes BETWEEN 0 AND 5);

ALTER TABLE tasks ADD COLUMN auto INTEGER NOT NULL DEFAULT 0 CHECK (auto IN (0, 1));  -- affidato all'autopilota
ALTER TABLE tasks ADD COLUMN after_id TEXT REFERENCES tasks(id) ON DELETE SET NULL;   -- parte dopo che è Fatto
CREATE INDEX tasks_after ON tasks(after_id);
ALTER TABLE tasks ADD COLUMN auto_by TEXT;  -- attempt il cui agente l'ha affidato all'autopilota; NULL = l'utente

ALTER TABLE attempts ADD COLUMN verify_state TEXT
    CHECK (verify_state IN ('running', 'passed', 'failed', 'error'));  -- NULL = mai verificato
ALTER TABLE attempts ADD COLUMN verify_head TEXT;                       -- commit verificato
ALTER TABLE attempts ADD COLUMN verify_fixes INTEGER NOT NULL DEFAULT 0 CHECK (verify_fixes >= 0);
ALTER TABLE attempts ADD COLUMN verify_summary TEXT;                    -- coda dell'output, ≤ 8 KiB
ALTER TABLE attempts ADD COLUMN verify_pending TEXT;                    -- prompt di una correzione in attesa di uno slot
```

- **Una sola dipendenza** per task (`after_id`): un altro task dello stesso progetto, mai il task stesso, mai un task annullato (non sarebbe mai Fatto) e mai un ciclo (A dopo B, B dopo A: nessuno dei due partirebbe). Il DB salva solo la FK; le regole sono del Core (`check_after`, `Invalid` in italiano: segue la catena degli `after_id`). Eliminare la dipendenza la toglie (`SET NULL`). Annullare la dipendenza **dopo** averla scelta non è rifiutato (§13.7).
- **`auto_by`** è testo semplice, senza FK, come `started_by_attempt`: l'attempt il cui agente ha affidato il task all'autopilota (`start_task` in coda, sotto task ereditato, §7.4). Lo scrive `Db::set_task_auto_by`; `Db::set_task_auto` e un `auto` dato da `update_task` (l'utente) lo azzerano. Non è nel contratto IPC.
- **`verify_pending`**: il prompt di una correzione che non ha trovato uno slot (`ConcurrencyLimit`), o che ha trovato la pausa per limite d'uso, o l'app in chiusura. `Db::set_verify_pending`, `Db::pending_fixes` (attempt attivi, i più vecchi prima); lo azzerano l'invio, `begin_verify` e la fine di ogni turno dell'attempt. Sopravvive al riavvio.
- **Coda dell'autopilota** (`Db::autopilot_candidates(project_id)`): i task del progetto con `auto = 1`, in `todo`, senza attempt attivo, con `after_id` nullo o in `done`, e con il padre (se sotto task) non annullato, in ordine di posizione. Il flag `projects.autopilot` lo controlla il chiamante. `Db::set_task_auto` toglie (o rimette) `auto` da solo.
- **Verifica**: `Db::begin_verify` (stato `running`, `verify_head` = HEAD alla partenza, riassunto azzerato), `Db::finish_verify` (`passed`, `failed` o `error`, con il riassunto e il commit), `Db::add_verify_fix` (conta un rimando, restituisce il totale) e `Db::undo_verify_fix` (lo restituisce se il follow-up non è partito). Allo startup `Db::mark_stale_verifies` porta a `error` ogni verifica rimasta `running` (l'app si è chiusa durante la verifica) e restituisce gli attempt toccati.
- Le card riportano `verify_state` e `verify_fixes` solo dell'attempt **attivo** (`None` e 0 altrimenti); `TaskCard::verifying` viene dal registro live.

**Migrazione 0005** (`crates/atm-core/migrations/0005_planner.sql`, round del 2026-10-02): il pianificatore («Pianifica con un agente»). Le righe esistenti restano task visibili (`kind = 'task'`), senza piano né `launch`; il test `migrates_a_v4_database_with_rows_to_v5` parte da un DB con 0001–0004 e le righe dentro.

```sql
ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'task' CHECK (kind IN ('task', 'plan'));
ALTER TABLE tasks ADD COLUMN planned_by TEXT REFERENCES tasks(id) ON DELETE SET NULL;  -- il piano che l'ha creato
CREATE INDEX tasks_planned_by ON tasks(planned_by);
ALTER TABLE tasks ADD COLUMN plan_state TEXT
    CHECK (plan_state IN ('running', 'awaiting', 'started', 'dismissed', 'failed'));   -- solo i task 'plan'
CREATE UNIQUE INDEX tasks_one_active_plan ON tasks(project_id)
    WHERE kind = 'plan' AND plan_state IN ('running', 'awaiting');
ALTER TABLE tasks ADD COLUMN launch INTEGER NOT NULL DEFAULT 0 CHECK (launch IN (0, 1)); -- da avviare anche senza autopilota
```

- **Il piano è un task nascosto** (`kind = 'plan'`): titolo = prima riga non vuota del prompt (al massimo `MAX_PLAN_TITLE` = 80 caratteri, tagliata con `…`, `db::plan_title`), descrizione = il prompt, in `todo` (poi `inprogress`/`inreview` come ogni attempt: nessuno lo vede). Lo escludono `Db::board` e `Db::subtask_cards` (quindi `get_board`, vista lista, statistiche della panoramica, contatori della sidebar, `list_tasks` degli strumenti board), `Db::autopilot_candidates`, `Db::projects_with_launch`, `own_task` degli strumenti board (un piano, anche il proprio, «non esiste»: niente `self` come padre o `after`) e `check_parent`/`check_after`. Il recupero all'avvio (`mark_orphans`) lo tratta come un task qualunque. Rimuovere il progetto lo elimina; eliminarlo azzera `planned_by` dei task creati.
- **Un solo piano attivo** (`running` o `awaiting`) per progetto: `Db::insert_plan` lo controlla nella sua transazione (`Conflict` in italiano) e l'indice parziale lo impone.
- **`Db::launch_plan(plan_id, autopilot)`**, una transazione: il piano `running` o `awaiting` passa a `started` e ogni task **di primo livello** che ha creato ancora in `todo` va allo scheduler (un sotto task lo avvia l'agente del padre, con `start_task`: mai due agenti sullo stesso lavoro), con `auto = 1` (e `auto_by` nullo: come se l'avesse affidato l'utente) se il progetto ha l'autopilota, altrimenti con `launch = 1`. Restituisce gli id, dal più vecchio; `Conflict` se il piano è in un altro stato.
- **`launch`**: lo scheduler avvia anche i task con `launch = 1` dei progetti **senza** autopilota (`Db::projects_with_launch`); `Db::autopilot_candidates(project_id, include_auto)` restituisce i task con `launch`, e quelli con `auto` solo se `include_auto` (= `projects.autopilot`). `begin_attempt` lo azzera nella sua transazione, e così ogni spostamento fuori da `todo` (`place`, quindi `move_task` e le transizioni): un task tolto da Da fare e poi rimesso non parte più da solo; `Db::set_task_launch` lo toglie a mano (avvio fallito). `Task::launch` lo porta alla UI: la card mostra «In coda» con il motivo. Verifica, correzioni e merge dopo il turno restano solo dell'autopilota.
- **Startup**: `Db::fail_stale_plans` porta a `failed` ogni piano ancora `running` senza un turno `running` (dopo `mark_orphans`) e restituisce gli id (il Core scarta i loro attempt).
- `PlannedTask::parent_id` distingue i sotto task: «Avvia N task?» conta solo quelli di primo livello.
- Altri accessi: `Db::latest_plan(project_id)`, `Db::plan_state`, `Db::set_plan_state(plan_id, from, to)` (compare-and-set), `Db::set_planned_by(task_id, plan_id)`, `Db::plan_tasks(plan_id)`, `Db::plan_view(plan_id)` → `PlanView` (attempt più recente del piano, task creati).

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
| `notifications` | `true` (notifiche macOS dell'autopilota, round 2026-10-01) |

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
- `VerifyState`: running, passed, failed, error (round 2026-10-01; `error` = non è stato possibile eseguirla: spawn, timeout, app chiusa durante la verifica).
- `TaskKind`: task, plan (round 2026-10-02; default `task`).
- `PlanState`: running, awaiting, started, dismissed, failed (round 2026-10-02; `PlanState::is_active` = running o awaiting).

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
pub const MODEL_ALIASES: &[&str] = &["opus", "sonnet", "haiku", "fable"];   // unica lista di alias (CLI 2.1.284)
pub const MAX_SUBAGENTS: u8 = 10;
pub const MAX_ATTACHMENTS_PER_TASK: usize = 20;
pub const MAX_ATTACHMENT_BYTES: u64 = 25 << 20;          // 25 MiB
pub const MAX_PROJECT_DESCRIPTION: usize = 10_000;       // caratteri
pub const VERIFY_TIMEOUT_SECS: RangeInclusive<u32> = 10..=3600;   // autopilota (round 2026-10-01)
pub const DEFAULT_VERIFY_TIMEOUT_SECS: u32 = 600;
pub const MAX_AUTOPILOT_FIXES: u32 = 5;
pub const DEFAULT_AUTOPILOT_MAX_FIXES: u32 = 2;
pub const MAX_VERIFY_COMMAND: usize = 1000;             // caratteri
pub const MAX_PLAN_TITLE: usize = 80;                   // titolo del task nascosto di un piano (round 2026-10-02)
pub struct Project { pub id: Id, pub name: String, pub description: String /* ≤ 10 000 caratteri, "" di default */,
    pub repo_path: String, pub default_target_branch: String,
    pub default_permission_mode: PermissionMode, pub default_model: Option<String>,
    pub config_policy: ConfigPolicy, pub trusted: bool /* Trusted e fingerprint approvato = quello del tip del branch target */,
    pub allow_bypass: bool, pub created_at: Millis, pub updated_at: Millis,
    #[serde(default)] pub autopilot: bool, #[serde(default)] pub autopilot_merge: bool /* merge dopo una verifica verde */,
    #[serde(default)] pub verify_command: Option<String> /* None = nessuna verifica */,
    #[serde(default = 600)] pub verify_timeout_secs: u32 /* 10..=3600 */,
    #[serde(default = 2)] pub autopilot_max_fixes: u32 /* 0..=5 */ }
pub struct Task { pub id: Id, pub project_id: Id, pub title: String, pub description: String,
    pub status: TaskStatus, pub position: f64, pub created_at: Millis, pub updated_at: Millis,
    #[serde(default)] pub parent_id: Option<Id> /* padre di un sotto task (un solo livello); None = primo livello */,
    #[serde(default)] pub auto: bool /* affidato all'autopilota */,
    #[serde(default)] pub after_id: Option<Id> /* parte dopo che questo task è Fatto */,
    #[serde(default)] pub kind: TaskKind /* Plan solo per il task nascosto di un piano; assente = Task */,
    #[serde(default)] pub launch: bool /* consegnato allo scheduler da un piano (§5.2, §7.13) */ }
pub struct TaskCard { pub task: Task, pub attempt_id: Option<Id>,
    pub attempt_state: Option<AttemptState> /* Some(Active) = ha un attempt attivo */, pub branch: Option<String>, pub running: bool,
    pub pending_approvals: u32, pub last_status: Option<ProcessStatus>, pub last_stop_reason: Option<StopReason>,
    pub worktree_state: Option<WorktreeState>,
    #[serde(default)] pub subtasks_done: u32 /* sotto task in done */, #[serde(default)] pub subtasks_total: u32 /* tutti i sotto task */,
    #[serde(default)] pub verifying: bool /* verifica in corso (registro live) */,
    #[serde(default)] pub verify_state: Option<VerifyState> /* dell'attempt attivo */,
    #[serde(default)] pub verify_fixes: u32 /* dell'attempt attivo */ }
pub struct AttemptView { pub id: Id, pub task_id: Id, pub state: AttemptState, pub branch: String,
    pub target_branch: String, pub base_commit: String, pub worktree_path: String, pub worktree_state: WorktreeState,
    pub permission_mode: PermissionMode, pub model: Option<String>, pub effort: Option<Effort>,
    pub subagent_model: Option<String> /* CLAUDE_CODE_SUBAGENT_MODEL; None = default del CLI */,
    pub max_subagents: Option<u8> /* 0..=10; None = nessun limite */, pub subagents_used: u32,
    pub session_started: bool, pub merge_commit: Option<String>, pub running: bool, pub pending_approvals: u32,
    pub created_at: Millis, pub closed_at: Option<Millis>,
    #[serde(default)] pub started_by_attempt: Option<Id> /* attempt che l'ha avviato con start_task; None = l'utente */,
    #[serde(default)] pub verify_state: Option<VerifyState> /* None = mai verificato */,
    #[serde(default)] pub verify_head: Option<String> /* commit dell'ultima verifica */,
    #[serde(default)] pub verify_fixes: u32 /* rimandi dell'autopilota */,
    #[serde(default)] pub verify_summary: Option<String> /* coda dell'output, ≤ 8 KiB */ }
pub struct ProcessInfo { pub id: Id, pub seq: u32, pub prompt: String, pub status: ProcessStatus,
    pub stop_reason: Option<StopReason>, pub result_subtype: Option<String>, pub is_error: Option<bool>,
    pub cost_usd_estimate: Option<f64>, pub duration_ms: Option<u64>, pub num_turns: Option<u32>,
    pub head_after: Option<String>, pub started_at: Millis, pub finished_at: Option<Millis> }
pub struct TaskDetail { pub task: Task, pub attempt: Option<AttemptView>, pub processes: Vec<ProcessInfo>,
    pub closed_attempts: Vec<AttemptView>, pub attachments: Vec<Attachment> /* dal più vecchio */,
    #[serde(default)] pub subtasks: Vec<TaskCard> /* card dei sotto task, nell'ordine della board */ }
pub struct PlanView { pub id: Id /* il task nascosto del piano */, pub prompt: String,
    pub model: Option<String>, pub effort: Option<Effort> /* dell'attempt del piano */, pub state: PlanState,
    pub attempt_id: Option<Id> /* scartato a fine piano, transcript ancora leggibile; None solo prima dell'attempt */,
    pub created: Vec<PlannedTask> /* task creati (planned_by), dal più vecchio */, pub created_at: Millis }
pub struct PlannedTask { pub id: Id, pub title: String, pub status: TaskStatus,
    #[serde(default)] pub parent_id: Option<Id> /* un sotto task: mai avviato dal piano */ }
pub struct Attachment { pub id: Id, pub task_id: Id, pub name: String, pub size: u64,
    pub path: String /* assoluto, calcolato dal Core */, pub created_at: Millis }
pub struct PickedFile { pub token: Id /* monouso, scade dopo 10 min */, pub name: String, pub size: u64 }
pub struct ProjectOverview { pub project_id: Id, pub branch: String, pub commit: String /* sha completo del tip */,
    pub agents_load_config: bool /* policy Trusted e trusted */, pub files: Vec<ContextFile>,
    pub mcp_servers: Vec<McpServer>, pub claude_agents: Vec<String>, pub claude_commands: Vec<String>,
    pub claude_skills: Vec<String> }
pub struct ContextFile { pub path: String, pub kind: ContextFileKind, pub size: u64,
    pub content: Option<String> /* segreti mascherati; None se troppo grande, binario, symlink o JSON non valido */,
    pub note: Option<String> /* "troppo grande (N KiB)", "→ AGENTS.md", "binario", "JSON non valido",
                                "valori segreti mascherati" */,
    pub hidden_chars: bool /* anche nel target di un symlink */, pub used_by_agents: bool, pub usage_note: String }
pub enum ContextFileKind { Memory, Agents, Readme, Settings, Mcp }
pub struct McpServer { pub name: String, pub transport: String /* type, altrimenti stdio | http | sconosciuto */,
    pub target: String /* comando e argomenti, o URL senza userinfo, query e fragment; ≤ 200 caratteri */,
    pub env_keys: Vec<String>, pub header_keys: Vec<String> /* solo i nomi, mai i valori */ }
pub struct BranchList { pub current: Option<String>, pub branches: Vec<String> }
pub struct Settings { pub claude_path_override: Option<String>, pub default_model: Option<String>,
    pub max_running: u32, pub allow_env_api_key: bool, pub worktree_root: String, pub editor_app: String,
    pub remove_worktree_after_merge: bool, #[serde(default = true)] pub notifications: bool }
pub struct ClaudeInfo { pub path: Option<String>, pub version: Option<String>, pub supported: bool,
    pub min_version: String /* "2.1.223" */, pub tested_version: String /* "2.1.283" */ }
#[serde(tag = "state")]
pub enum AuthState { LoggedIn { auth_method: Option<String>, api_provider: Option<String>, email: Option<String>,
    org_name: Option<String>, subscription_type: Option<String> }, LoggedOut, Unknown { reason: String } }
pub struct EnvStatus { pub claude: ClaudeInfo, pub auth: AuthState, pub git_version: Option<String>,
    pub api_key_in_env: bool, pub cloud_provider_env: bool, pub base_url_env: bool /* §7.2 */,
    pub paused: Option<String> /* usage limit */,
    pub running: u32, pub max_running: u32 /* topbar "in esecuzione x/y" */,
    pub problems: Vec<String>, pub checked_at: Millis /* la UI tiene il più recente */ }
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
    ToolCall      { tool_use_id: String, name: String, summary: String,
                    input: String /* JSON ≤4 KiB; ≤256 KiB se ha chiesto approvazione */,
                    status: ToolStatus, output: Option<ToolOutput> },
    ApiRetry      { attempt: u32, max_retries: u32, delay_ms: u64, error: String },
    TurnEnd       { subtype: String, is_error: bool, duration_ms: Option<u64>, num_turns: Option<u32>,
                    cost_usd_estimate: Option<f64>, permission_denials: u32, text: Option<String>,
                    limit: Option<LimitKind>,
                    stopped: Option<StopReason> /* M6: fermato dall'app, §7.6; assente nel JSON se None */ },
    Notice        { level: Level, text: String, action: Option<NoticeAction> },
    Stderr        { text: String },
}
#[serde(tag = "state")]
pub enum ToolStatus { Running, AwaitingApproval { approval_id: Id, can_remember: bool, reason: Option<String> },
    Denied { message: String }, Succeeded, Failed, Cancelled }
pub struct ToolOutput { pub text: String /* ≤8 KiB, testa+coda */, pub truncated_bytes: u64, pub is_error: bool }
pub enum Level { Info, Warn, Error }
pub enum NoticeAction { NewSession }                  // bottone "Nuova sessione" (§7.9)
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
pub fn merge_message(title: &str, description: &str, attempt_id: &str) -> String   // default del §8.7

// api.rs (oltre a marker, Req e Changed)
pub const CONTINUE_PROMPT: &str = "The previous run was interrupted when the app closed. Continue the task.";
pub struct UpdateProjectReq { pub id: Id, pub name: String, pub default_target_branch: String,
    pub default_permission_mode: PermissionMode, pub default_model: Option<String>,
    #[serde(default)] pub description: String,
    #[serde(default)] pub autopilot: bool, #[serde(default)] pub autopilot_merge: bool,
    #[serde(default)] pub verify_command: Option<String>,
    #[serde(default = 600)] pub verify_timeout_secs: u32, #[serde(default = 2)] pub autopilot_max_fixes: u32 }
pub struct CreateTaskReq { pub project_id: Id, pub title: String, pub description: String,
    pub status: Option<TaskStatus> /* None = todo */,
    #[serde(default)] pub parent_id: Option<Id> /* crea un sotto task: stesso progetto, padre di primo livello */,
    #[serde(default)] pub auto: bool, #[serde(default)] pub after_id: Option<Id> }
pub struct UpdateTaskReq { pub id: Id, pub title: String, pub description: String,
    #[serde(default)] pub auto: Option<bool> /* assente = invariato */,
    #[serde(default)] pub after_id: Option<Option<Id>> /* assente = invariato, null = nessuna dipendenza */ }
pub struct StartAttemptReq { pub task_id: Id, pub target_branch: String, pub permission_mode: PermissionMode,
    pub model: Option<String>, pub effort: Option<Effort>,
    pub subagent_model: Option<String> /* un alias di MODEL_ALIASES */, pub max_subagents: Option<u8> /* 0..=10 */ }
pub struct AddTaskAttachmentsReq { pub task_id: Id, pub tokens: Vec<Id> }
pub struct StartPlanReq { pub project_id: Id, pub prompt: String, pub model: Option<String>, pub effort: Option<Effort> }
pub struct GetPlanReq { pub project_id: Id }
pub struct ResolvePlanReq { pub plan_id: Id, pub proceed: bool }

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
| `get_settings` / `update_settings` | `{}` / `Settings` → `Settings` | `allow_env_api_key=true` e un `claude_path_override` nuovo chiedono una **conferma nativa** (M6); il Core applica solo se quei due valori sono ancora quelli letti prima del dialog (`Core::update_settings_checked`, altrimenti `Conflict`) |
| `list_projects` | `{}` → `Vec<Project>` | |
| `pick_repo_folder` | `{}` → `Option<String>` | Chiama `blocking_pick_folder` in `spawn_blocking` [F] |
| `add_project` | `{path}` → `{project, warnings: Vec<String>}` | §8.3 |
| `update_project` | `{id, name, default_target_branch, default_permission_mode, default_model, description?, autopilot?, autopilot_merge?, verify_command?, verify_timeout_secs?, autopilot_max_fixes?}` → `Project` | `description` senza spazi in testa e in coda, al massimo 10 000 caratteri (`Invalid`); assente = vuota. Autopilota (round 2026-10-01): `verify_command` senza spazi in testa e in coda, vuoto = nessuno, al massimo 1000 caratteri; `verify_timeout_secs` da 10 a 3600; `autopilot_max_fixes` da 0 a 5 (`Invalid` in italiano). Assenti: spento, nessun comando, 600 e 2 |
| `set_project_security` | `{id, config_policy, allow_bypass}` → `Project` | M6. Conferma nativa quando si **eleva** il livello **effettivo**: Trusted se il progetto non è `trusted` ora (anche Trusted con fingerprint scaduto), oppure bypass. Il guscio legge una volta, prima del dialog, progetto, stato salvato e configurazione committata sul tip del branch target predefinito (`Core::security_snapshot`, con branch e commit); il testo della conferma nomina il repository per path (su una riga), il branch e il commit approvati, dice che cosa si concede (configurazione del repo, regole che consentono un tool intero, modalità Autonoma), che una configurazione che fattura fuori dall'abbonamento non si approva e che un turno con una chiave API viene fermato, e che il worktree non è una sandbox. `Core::apply_project_security` salva il fingerprint che il dialog ha descritto, solo se è ancora quello e se lo stato salvato è ancora quello letto (`Conflict` altrimenti, §8.9); `Invalid` se il repo è `$HOME`, se il branch non si legge, se la configurazione non è verificabile o se farebbe fatturare gli agenti fuori dall'abbonamento (nomina la chiave, §8.9). Annullato l'aumento, la parte che abbassa si applica comunque (l'errore lo dice). Togliere `allow_bypass` riporta a Auto-edit una modalità predefinita Autonoma; una revoca ferma i turni che la usano |
| `remove_project` | `{id}` → `()` | Rifiutato se ci sono turni attivi (`Busy`). Snapshot e rimozione dei worktree; branch tenuti; task, cronologia, allegati e log grezzi cancellati (§4) |
| `list_branches` | `{project_id}` → `BranchList` | |
| `get_project_overview` | `{project_id}` → `ProjectOverview` | Letto dal commit in cima a `default_target_branch` (la base dei worktree, quello che il trust approva), mai dal checkout principale; `Invalid` se il repo è `$HOME` o la revisione non è valida, `Git` se il runner fallisce. `Core` risolve tip, record della configurazione (in cache) e `agents_load_config`, poi chiama `git::overview::read`: un solo `git ls-tree -z -l --end-of-options <tip> -- CLAUDE.md .claude/CLAUDE.md AGENTS.md README.md .claude/settings.json .mcp.json` sul runner irrobustito (sola lettura, stdout ≤ 64 KiB), poi un solo `cat-file --batch` che legge per id, con la dimensione esatta, i blob ≤ 64 KiB. Non in cache: ogni chiamata costa un `ls-tree` e al più un `cat-file` (§10.2, §13) |
| `get_board` | `{project_id}` → `Vec<TaskCard>` | Unisce DB e registro live |
| `create_task` / `update_task` | `{project_id, title, description, status?, parent_id?, auto?, after_id?}` / `{id, title, description, auto?, after_id?}` → `TaskCard` | Con `parent_id` crea un sotto task (§5.2 migrazione 0003): `Invalid` se il padre non esiste, è di un altro progetto o è a sua volta un sotto task. La creazione prende il lock del padre, così non si incrocia con la sua eliminazione. `after_id` (§5.2 migrazione 0004): `Invalid` se il task non esiste, è di un altro progetto, è il task stesso, è annullato o chiude un ciclo di dipendenze. In `update_task` `auto` e `after_id` assenti restano invariati; `after_id: null` toglie la dipendenza |
| `move_task` | `{id, status, before_id: Option<Id>}` → `()` | `Busy` se il task è in esecuzione e la destinazione è done o cancelled |
| `delete_task` | `{id}` → `()` | `Busy` se in esecuzione. Fa discard dell'attempt attivo; allegati e log grezzi cancellati (§4). Round 2026-09-30, a cascata: prende il lock del task e poi quello di ogni sotto task per tutta l'operazione; `Busy` ("Il task è in esecuzione: …") se gira l'agente del task, `Busy` ("Un sotto task è in esecuzione") se gira quello di un sotto task, e in entrambi i casi nulla viene toccato. Altrimenti elimina prima i sotto task, ciascuno con la stessa pulizia (worktree, allegati, log grezzi), poi il task. Eliminare un sotto task emette `changed` anche per il padre (§6.4) |
| `get_task_detail` | `{id}` → `TaskDetail` | Con gli allegati e le card dei sotto task |
| `pick_attachment_files` | `{}` → `Vec<PickedFile>` | Picker nativo multiplo (`blocking_pick_files` in `spawn_blocking`); i path vanno solo a `Core::stage_picks`, la webview riceve token monouso. Annullato: lista vuota. In E2E usa il path in coda di `debug_e2e_queue_pick` |
| `add_task_attachments` | `{task_id, tokens}` → `Vec<Attachment>` | Sotto il lock del task: copia i file nella cartella del task, poi una sola transazione che riconta e inserisce; `Invalid` oltre 20 allegati o con un token sconosciuto o scaduto ("File non più disponibile: sceglilo di nuovo"); a ogni errore le copie appena fatte si cancellano (§10.2) |
| `remove_task_attachment` | `{id}` → `()` | Sotto il lock del task: cancella riga e copia |
| `start_attempt` | `{task_id, target_branch, permission_mode, model?, effort?, subagent_model?, max_subagents?}` → `AttemptView` | Ritorna dopo che worktree e righe esistono; lo spawn è asincrono. `bypassPermissions` senza `allow_bypass` → `Invalid`; così `max_subagents` oltre 10 e un `subagent_model` fuori da `MODEL_ALIASES` |
| `send_follow_up` | `{attempt_id, prompt, permission_mode?, fresh_session: bool}` → `ProcessInfo` | `Busy` se c'è un turno in corso. `permission_mode` vale **solo per quel turno** (`processes.permission_mode`); quella dell'attempt non cambia |
| `stop_attempt` | `{attempt_id}` → `()` | Ritorna subito; l'escalation continua in background |
| `respond_approval` | `{attempt_id, approval_id, decision: ApprovalDecision}` → `()` | |
| `subscribe_transcript` | `{attempt_id}` + `onEvent: Channel<TranscriptMsg>` → `Id` (subscription) | Registra e ritorna subito [F]. Attempt sconosciuto: nessun errore, `Snapshot` vuoto (`has_more:false`) |
| `unsubscribe_transcript` | `{subscription_id}` → `()` | |
| `get_entries` | `{attempt_id, before_idx, limit ≤ 200}` → `EntryPage` | |
| `get_diff` | `{attempt_id}` → `DiffResult` | §8.6 |
| `get_branch_status` | `{attempt_id}` → `BranchStatus` | Include l'anteprima dei conflitti |
| `merge_attempt` | `{attempt_id, message}` → `MergeOutcome` | §8.7 |
| `discard_attempt` | `{attempt_id}` → `()` | |
| `delete_branch` | `{attempt_id}` → `()` | Solo per attempt `merged` e branch `atm/…` |
| `open_attempt` | `{attempt_id, target: OpenTarget}` → `()` | `open`, `open -a Terminal`, `open -a <editor_app>`. Il path viene dal DB |
| `open_url` | `{url}` → `()` | Solo `http(s)` |
| `start_plan` | `{project_id, prompt, model?, effort?}` → `PlanView` | Round 2026-10-02 (§5.2 migrazione 0005). Crea il task nascosto del piano e avvia il suo attempt di sola lettura (modalità Default). `Conflict` se un piano del progetto è in corso o in attesa; gli errori di `start_attempt` non lasciano nulla |
| `get_plan` | `{project_id}` → `Option<PlanView>` | L'ultimo piano del progetto, in qualunque stato |
| `resolve_plan` | `{plan_id, proceed}` → `PlanView` | Risponde a «Avvia N task?» di un piano `awaiting` (`Invalid` altrimenti): Sì → come l'avvio automatico (`Db::launch_plan`), No → `dismissed`, i task restano in Da fare |
| `app_info` | `{}` → `AppInfo {version}` | Round 2026-10-02 (§9.5): la versione del bundle (`package_info`, cioè la versione del workspace) |
| `check_update` | `{}` → `Option<UpdateInfo {current, latest, required, notes, install_error}>` | Il risultato dell'ultimo controllo in background; non va mai in rete. `None` anche nelle build di debug e con `ATM_NO_UPDATE_CHECK` |
| `install_update` | `{}` → `()` | Scarica e verifica la firma, ferma gli agenti con lo shutdown dell'uscita, installa sopra il bundle e riavvia: se riesce non ritorna. `Invalid` senza aggiornamento in sospeso, `Busy` se ne è già in corso uno, `Io` se il download o la firma falliscono (l'app resta com'è) |
| *(solo debug)* `debug_ping`, `debug_channel_probe`, `debug_selftest_enabled`, `debug_forwarder_count`, `debug_selftest_report` | §11 M0 | `#[cfg(debug_assertions)]` |

### 6.4 Eventi globali (`app.emit`)

| Nome | Payload | Reazione della UI |
|---|---|---|
| `changed` | `Changed { project_id: Option<Id>, task_id: Option<Id> }` | Refetch con debounce di 100 ms: `get_board` se il progetto è quello selezionato; `get_task_detail` se il task è quello aperto; `list_projects` se `project_id` è None |
| `env_changed` | `EnvStatus` | Aggiorna il gate e la topbar (auth, pausa) |
| `update_available` | `Option<UpdateInfo>` | Round 2026-10-02 (§9.5): un controllo in background ha trovato una release più nuova (banner o modal bloccante), o `None`: non la trova più dopo averla annunciata (la UI li toglie) |

`changed` si emette **dopo il commit** di ogni mutazione su task, attempt o process, compresi finalize e approvazioni. Round 2026-09-30: la modifica di un sotto task emette un secondo `changed` con `task_id` = il padre, perché il pannello del padre elenca i sotto task; vale anche quando il sotto task viene eliminato (`delete_task` lo emette esplicitamente, la riga non c'è più).

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
1. `settings.claude_path_override`. Cambiarlo chiede una **conferma nativa** con il percorso (l'app lo esegue subito e per ogni agente); il Core accetta solo un percorso assoluto di un file esistente fuori da ogni progetto e dalla cartella dei worktree (M6-REVIEW: un repository non sceglie il programma che l'app esegue).
2. La variabile d'ambiente `ATM_CLAUDE_PATH` (dev e test la puntano a `fake-claude`).
3. `~/.local/bin/claude`, `~/.claude/local/claude`, `/opt/homebrew/bin/claude`, `/usr/local/bin/claude`.
4. `command -v claude` dentro la login shell (§7.2), scartando i path in `$TMPDIR` o che contengono `/cmux-cli-shims/` (in terminale c'è uno shim cmux [V]).

Regole:
- Un candidato è valido se `<path> --version` termina con `(Claude Code)`.
- `ATM_CLAUDE_PATH`, se impostata, è vincolante: se non è valida la discovery si ferma lì ("Claude Code non trovato") e non passa ai candidati 3–4. In dev punta a `fake-claude`, e un `--version` lento durante la compilazione farebbe altrimenti partire il CLI reale sull'abbonamento senza avviso.
- Si conserva il **path del symlink**, senza canonicalizzarlo: il CLI si auto-aggiorna.
- `--version`, `auth status` e la login shell girano con cwd `/` (`claude::PROBE_CWD`, M6-REVIEW): mai nella cartella da cui è partita l'app, che può essere un repository non fidato (il suo `.claude` o `.envrc` non devono valere fuori da un turno).
- La versione è la prima parola dell'output di `--version` (semver).
  - Sotto `2.1.223`: banner con "Continua comunque" (il resume tra cwd diversi arriva da quella versione [F]).
  - Sopra `2.1.283`: solo un badge informativo.

### 7.2 Ambiente del figlio (`ChildEnv`, identico per `auth status` e per gli agenti)

| Voce | Regola |
|---|---|
| Base | L'env del processo app, ereditato |
| `PATH` | Importato una sola volta: `$SHELL -ilc 'printf "__ATM__%s__ATM__" "$PATH"'` (stdin null, timeout 5 s), estraendo il testo tra i marker. Fallback: `~/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`. **Solo PATH**: nessun'altra variabile di `.zshrc` |
| `LANG` | `en_US.UTF-8` se assente (le app lanciate dal Finder non la hanno) |
| Rimossi | `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` (salvo `allow_env_api_key`: opt-in con conferma nativa, poi banner nella topbar quando una chiave è presente, M6); le variabili di una **sessione Claude Code padre**, che l'app eredita quando viene lanciata da dentro una sessione (tool Bash, suo terminale), elencate in `claude::CLAUDE_NESTING_VARS` con la motivazione di ognuna (M6, viste nell'ambiente del tool Bash di Claude Code 2.1.x): `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_EXECPATH`, `CLAUDE_PID`, `CLAUDE_EFFORT` (diventerebbe l'effort di un attempt senza `--effort`) e il prefisso `CLAUDE_CODE_MESSAGING_*` (socket e token del canale di messaggi della sessione padre); e quelle dell'**host** di quella sessione (M6-REVIEW, `claude::HOST_SESSION_VARS`/`HOST_SESSION_PREFIXES`): il prefisso `CMUX_*` (socket di automazione del terminale cmux, compreso quello di computer use e il file del suo token, che può cliccare anche le conferme native) e `CLAUDE_CODE_SSE_PORT`, `ENABLE_IDE_INTEGRATION` (collegamento di un terminale IDE al suo editor). **`NODE_OPTIONS` di cmux** (2026-09-29, `claude::scrub_host_env`/`cmux_node_options`): un terminale cmux fa caricare a ogni programma node il suo modulo (`NODE_OPTIONS=--require=<file di cmux> …`) e annota quello dell'utente; `claude` è un programma node. Regola, decisa prima di togliere i `CMUX_*`: `CMUX_ORIGINAL_NODE_OPTIONS_PRESENT=0` → `NODE_OPTIONS` tolta; `=1` con `CMUX_ORIGINAL_NODE_OPTIONS` impostata → `NODE_OPTIONS` torna a quel valore; marker assente (o altro valore, o `=1` senza il valore salvato) → `NODE_OPTIONS` intatta. Vale per ogni figlio (agenti, `auth status`, git, `open`). Queste variabili (`claude::is_nesting_var`, e `NODE_OPTIONS` con la stessa regola) lasciano anche il **processo dell'app**: all'avvio, se ce n'è una, l'app si ri-esegue (`execve` dello stesso eseguibile, stessi argomenti e pid) senza di esse, perché `ps -E` mostra a ogni processo dello stesso utente l'ambiente con cui un processo è partito e toglierle in memoria non basta. **Restano** invece la configurazione dell'utente (`CLAUDE_CONFIG_DIR`, gli altri `CLAUDE_CODE_*` come `CLAUDE_CODE_MAX_OUTPUT_TOKENS`), il token OAuth (riga sotto), `CLAUDE_CODE_USE_BEDROCK`/`_VERTEX`/`_FOUNDRY` e gli endpoint `claude::BASE_URL_VARS` (`ANTHROPIC_BASE_URL`: un proxy o un gateway che riceve ogni richiesta con le credenziali dell'abbonamento e può fatturare a modo suo, mentre `apiKeySource` resta `none` e lo stop a runtime non lo vede; `ANTHROPIC_BEDROCK_BASE_URL`, `ANTHROPIC_VERTEX_BASE_URL`, `ANTHROPIC_FOUNDRY_BASE_URL`, usati con quel provider), che non si tolgono in silenzio ma si mostrano (`EnvStatus.cloud_provider_env`, `EnvStatus.base_url_env` con un valore non vuoto → banner di fatturazione nella topbar, 2026-09-29; i valori non si mostrano mai); nelle impostazioni di un repository gli stessi nomi non si approvano mai (§8.9); `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE`, `GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*` |
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
  --disallowedTools=AskUserQuestion[,Agent,Task][,Workflow]       # v1: domande rimandate; sub-agent sotto
  --settings=<json>                                               # §7.8, a ogni turno
  [--setting-sources=user --strict-mcp-config]                    # policy Isolated (default)
  [--model=<m>] [--effort=<e>]
  [--add-dir=<cartella canonica degli allegati del task>]         # solo se il task ha allegati
  --mcp-config={"mcpServers":{"atm":{"type":"sdk","name":"atm"}}} # sempre: tool board (round 2026-09-30)
  --append-system-prompt=<ATM_APPEND>
```

- La forma `--flag=value` impedisce l'iniezione di flag. Il prompt passa **solo da stdin**.
- **Limite di sub-agent** (round feature del 2026-09-29; `attempts.max_subagents`, con `remaining = max_subagents − subagents_used` ricalcolato a ogni turno):
  - `None` (nessun limite, il default): `--disallowedTools=AskUserQuestion`, come prima del round;
  - `Some(n)` con `remaining == 0` (anche `n = 0`): `--disallowedTools=AskUserQuestion,Agent,Task,Workflow`;
  - `Some(n)` con `remaining > 0`: `--disallowedTools=AskUserQuestion,Workflow` e `"ask":["Agent","Task"]` nelle `--settings` (§7.8), così ogni spawn arriva all'host come `can_use_tool` e l'host lo conta.
  - `Workflow` (alias `RunWorkflow`) è un secondo modo di avviare agenti, con script che non passano dal tool Agent: con un limite è sempre vietato. Il CLI mappa `Task` su `Agent`; elencarli entrambi non costa nulla. Una regola `ask` sul tool intero il CLI la valuta prima della modalità, bypass compreso (letto nel bundle di 2.1.284; conferma con il CLI reale ancora da fare, §13).
- **Modello dei sub-agent** (`attempts.subagent_model`, un alias di `MODEL_ALIASES`): `"env":{"CLAUDE_CODE_SUBAGENT_MODEL":"<alias>"}` dentro le `--settings`, non nell'ambiente del figlio. Le impostazioni passate con il flag battono l'`env` delle impostazioni di utente e progetto, e il valore compare nell'argv salvato e nel record di fake-claude. Per il CLI è la fonte con la **priorità più bassa** (§13).
- **Server MCP `atm`** (round 2026-09-30, spike [V] con il CLI 2.1.285): `--mcp-config=` in un solo elemento argv, sempre, subito prima di `--append-system-prompt`. Il server è di tipo `sdk`: lo serve l'host stesso sul control protocol (§7.4), nessun processo né socket. È compatibile con `--strict-mcp-config` (Isolated), che carica solo i server di `--mcp-config`, e non cambia la fiducia: Trusted resta deciso dall'assenza di `--strict-mcp-config` (`TurnCaps`, `recheck_trust`). La richiesta `initialize` dell'host non cambia (niente `sdkMcpServers`). Il modello vede i tool come `mcp__atm__<tool>`.
- **Allegati** (`--add-dir`): un solo elemento argv, dopo `--effort` e prima di `--mcp-config`, a **ogni** turno finché il task ha almeno un allegato. La cartella è quella canonica del task (`/var/folders` diventa `/private/var/folders`), la stessa stringa dei path nel prompt (§7.4). Nessuna regola deny sulla cartella: proteggerebbe solo le copie, e Bash la aggira. "Sola lettura" è un'istruzione del prompt (§13); gli originali non vengono mai toccati.
- **Senza opzioni** (nessun allegato, nessun limite e nessun modello dei sub-agent) l'argv è quello di prima del round del 2026-09-29 più `--mcp-config` e le regole board nelle `--settings` (round 2026-09-30). Per questo il round del 2026-09-30 ha cambiato **tutti** gli snapshot `claude__argv_*`: ogni argv ora ha `--mcp-config`, e ogni `--settings` ha l'`allow` dei tool board di lettura e creazione e l'`ask` di `update_task`, `move_task` e `start_task`, anche senza limite di sub-agent (§7.8).
- **M5 [V]:** `--verbose` è obbligatorio (senza, `-p` con `--output-format stream-json` esce con 1: "requires --verbose"). `--permission-mode=default` è accettato anche se l'help di 2.1.283 elenca `manual` al suo posto (alias: la risposta a `initialize` dice `current_permission_mode: "default"` per entrambi).
- **Mai passati:**
  - `--bare`: disabilita l'OAuth [F];
  - `--dangerously-skip-permissions`;
  - `--system-prompt`: sostituirebbe il prompt di Claude Code;
  - `--no-session-persistence`: romperebbe il resume;
  - `--replay-user-messages`: serve solo al rewind, rimandato;
  - `--allowedTools` con un tool intero: oscurerebbe `can_use_tool`.
- **`ATM_APPEND`.** È fisso per attempt, perché `--system-prompt-snapshot` è attivo di default e il prompt si congela al primo turno [V]:
  > "You are working on a task from AI Task Manager inside a dedicated git worktree `<path>` on branch `<branch>` (created from `<target>`). Work only inside this directory. Do not push, switch or delete branches, rewrite history, or change git remotes/config. The host app commits your changes automatically after each turn. The user's later messages continue this task, even once it looks done: carry out what they ask. If a CLAUDE.md or AGENTS.md exists at the repository root, read it first and follow its conventions. The mcp__atm__* tools manage this project's task board. To split your work into subtasks, use mcp__atm__create_task with parent_id \"self\". When your task is driven by the autopilot, its subtasks start on their own: give a subtask `after` (another task's id) to start it only once that task is done."
- La frase finale su CLAUDE.md copre il caso non verificato E11. Il Riepilogo del progetto la dà per scontata (`git/overview.rs`, `usage`: CLAUDE.md e AGENTS.md "letti su istruzione del prompt"): se questa frase cambia, cambiano anche quelle note.
- La frase sui messaggi successivi (M6) risponde a M5, che ha visto il modello rifiutare un follow-up ritenuto estraneo a un task già finito (§13.4).
- **Spawn:**
  - `tokio::process::Command` con `.current_dir(wt).process_group(0).kill_on_drop(true)`, stdin, stdout e stderr in pipe;
  - `pid` = `pgid`, salvato nel DB;
  - `argv_json` salvato; l'env mai.

### 7.4 Protocollo stdin (`wire.rs`; un solo task writer)

- **Writer.** Un task scrittore riceve da un `mpsc::channel::<Value>(64)`. Scrive una riga JSON, poi `\n`, poi `flush`. Ogni riga viene copiata anche in `stdin.jsonl`.
- **ID delle richieste host.** Formato `atm_<n>_<8hex>`. Le richieste in sospeso stanno in `HashMap<String, oneshot::Sender<Result<Value,String>>>`.

**Sequenza di un turno:**
1. Il runner invia `{"type":"control_request","request_id":"atm_1_…","request":{"subtype":"initialize","hooks":null}}` e **attende** il `control_response` per al massimo 60 s, continuando intanto a instradare stdout.
   - M5 [V]: non è necessario (un messaggio utente senza `initialize` ottiene `system/init`, streaming e `result` success), ma resta: rende esplicito l'avvio. La risposta arriva dopo l'avvio degli hook `SessionStart` dell'utente e prima di `system/init`, che il CLI emette solo al primo messaggio utente; contiene `commands`, `agents`, `models`, `current_permission_mode`, `pid` e `account: {email, organization, subscriptionType, apiProvider}`. Nel raw log `email` e `organization` diventano `<redacted>` (`wire::redact_for_log`, §7.10).
   - Risposta `error`: Notice (warn) e si prosegue.
   - Timeout: stop sequence, poi `failed` / `init_timeout`.
   - Con `hooks:null` gli hook di progetto non vengono sostituiti (Vibe Kanban #3327 [F]).
2. Il runner invia `{"type":"user","message":{"role":"user","content":"<prompt>"},"parent_tool_use_id":null}`, senza `session_id` [F]. Contestualmente crea l'entry `UserMessage`.
   - Primo turno: `"# {title}\n\n{description}"`, senza spazi in coda, più la sezione del task padre se è un sotto task, più la sezione degli allegati se il task ne ha.
   - Follow-up: il testo dell'utente, invariato (niente sezione del padre né degli allegati: la cartella resta visibile con `--add-dir`, §7.3).
   - `fresh_session`: task + descrizione + sezione del task padre + sezione degli allegati + output di `git log --oneline <base>..HEAD` + il testo.
   - **Sezione del task padre** (round 2026-09-30, solo per un sotto task; `first_prompt`/`fresh_prompt`, in inglese): `\n\n## Parent task\n\nThis task is a sub-task of the one below, given for context: work only on this task.\n\n### {titolo del padre}\n\n{descrizione del padre}`, subito dopo la descrizione del task e **prima** di `## Attachments`. Con la descrizione del padre vuota la sezione finisce col titolo. Test: `tests/flow.rs::a_subtask_prompt_carries_its_parent`.
   - **Sezione degli allegati** (round feature del 2026-09-29, `attachments::prompt_section`; in inglese come gli altri testi per il modello): `\n\n## Attachments\n\nThe user attached these files to the task. They are read-only copies kept outside the repository: read them from these paths, do not modify, move or delete them.\n` e poi una riga `- \`<path assoluto canonico>\`` per file, dal più vecchio. Le immagini l'agente le legge con il tool Read (§14 q.7). Il nome di un allegato non contiene apici inversi (§10.2), quindi il code span non si rompe.
3. All'arrivo di `result`: il runner registra il risultato, **chiude stdin** (drop del sender) e continua a leggere stdout fino a EOF, perché dopo `result` possono arrivare altri eventi [F]. Poi `wait` con timeout di 30 s; allo scadere, stop sequence con `exit_timeout`.
4. Dopo qualunque uscita: `killpg(pgid, SIGTERM)` sul gruppo residuo, ignorando `ESRCH`. Termina i processi in background avviati dall'agente, come i dev server; è un limite documentato del modello un-processo-per-turno.
   - M5 [V]: il tool Bash del CLI 2.1.283 esegue ogni comando in un **process group suo** (pgid ≠ quello di claude), che il `killpg` del gruppo non raggiunge. Il CLI li chiude da sé su interrupt (osservato, anche per i task in background). Quando il leader è morto i suoi figli passano a launchd e nulla li lega più al turno, quindi il Core **registra i discendenti del leader mentre vive** (`ps -A -o pid,ppid,pgid`, `claude::tree_of`): all'arrivo di `result`, prima di chiudere stdin, e ai gradini EOF e SIGTERM della stop sequence. All'uscita del leader manda SIGTERM a quelli ancora vivi e poi, dopo 3 s, SIGKILL (`claude::survivors`: stesso pgid e genitore launchd o un altro discendente registrato, contro il riuso dei pid); `run_in_background` compresi. Dove il CLI riceve SIGKILL (ultimo gradino, runtime abbandonato con `KillGroupOnDrop`, recovery di un orfano vivo) il Core uccide anche i discendenti, raccolti prima (`claude::kill_tree`). Limite: se il leader muore da solo senza `result` (crash) o insieme all'app, i discendenti non registrati restano (§7.9).

**Routing dello stdout:**

| Riga | Azione |
|---|---|
| `control_response` | Risolve la richiesta host in sospeso; nel raw log l'`account` della risposta a `initialize` è oscurato (M5). L'oscuramento (`wire::redact_for_log`) vale per ogni riga registrata, di qualunque tipo, con `account` in `response.response`, in `response` o al primo livello; un `account` che non è un oggetto diventa `<redacted>` per intero |
| `control_request` / `can_use_tool` | Approvazioni (§7.8) |
| `control_request` / `mcp_message` con `server_name:"atm"` (round 2026-09-30) | Risposta sincrona, mai pendente: `{"subtype":"success","request_id":…,"response":{"mcp_response":<risposta JSON-RPC>}}` (sotto) |
| `control_request` / `mcp_message` per un altro server | Risposta `{"subtype":"error",…,"error":"Unknown MCP server: X"}` più una Notice (warn) |
| `control_request` di altri tipi (`hook_callback`, `elicitation`, …, e un `mcp_message` senza `server_name` o senza oggetto `message`) | Risposta `{"subtype":"error","request_id":…,"error":"Unsupported control request subtype: X"}` [F] più una Notice (warn) |
| `control_cancel_request` | Annulla l'approvazione corrispondente, **senza risposta** [F] |
| `keep_alive` | Ignorato |
| `stream_event` | Solo anteprima di digitazione (§7.6). Non finisce nel raw log |
| Riga che non inizia con `{` | Solo raw log |
| Tutto il resto | Normalizer, più raw log |

**Server MCP `atm`** (round 2026-09-30; `runner/turn.rs` `mcp_reply`, tool in `runner/board_tools.rs`). Forme verificate dallo spike del 2026-09-30 [V]:
- il CLI apre il server prima di `system/init`, dopo `initialize` dell'host e il primo messaggio utente: `initialize` (id 0), la notifica `notifications/initialized` (senza id), `tools/list` (id 1); poi un `tools/call` per ogni uso di un tool, con `params {name, arguments, _meta}`;
- risposte dell'host: a `initialize` eco di `protocolVersion`, `capabilities: {"tools":{}}`, `serverInfo: {"name":"atm",…}`; a una notifica `{"jsonrpc":"2.0","result":{}}`; a `tools/list` i sei tool con `inputSchema`; a `tools/call` `result.content[{type:"text",text:<JSON del risultato>}]`, oppure lo stesso con `isError: true` e un testo inglese (cosa è successo, poi codice e messaggio dell'`AppError`); a un metodo sconosciuto l'errore JSON-RPC `-32601`;
- tutto è sincrono nel ciclo del turno, come `count_subagent` (§7.8): un tool che chiede approvazione è già stato approvato quando arriva il suo `tools/call`. L'host non si fida solo del CLI: quando l'utente consente un `can_use_tool` di `update_task`, `move_task` o `start_task` ne registra il `tool_use_id`, e il `tools/call` di quei tre tool gira solo se il suo `_meta."claudecode/toolUseId"` è tra quelli consentiti (ciascuno vale una volta). Altrimenti (un hook `PreToolUse` dell'utente che consente, un CLI che salta la regola `ask`) la risposta è l'errore del tool "Not approved: …" e nulla cambia (`tests/flow.rs::an_unapproved_board_call_is_refused`).

| Tool (`mcp__atm__*`) | Effetto | Regola (§7.8) |
|---|---|---|
| `list_tasks {status?, parent_id?}` | Card del progetto: id, titolo, stato, parent, `auto`, `after_id`, stato dell'agente e `verify` (`{state, fixes}` dell'attempt attivo, `running` durante una verifica; `null` senza verifiche) | allow |
| `get_task {id}` | Descrizione, stato, parent, `auto`, `after` (`{id, title, status}`), attempt (con `verify`) e sotto task con il loro stato | allow |
| `create_task {title, description?, status?, parent_id?, after?}` | `Core::create_task`; `parent_id:"self"` = il task dell'attempt chiamante; `after` (id o `"self"`) = `after_id`. Un sotto task con `parent_id:"self"` di un task con `auto` ha `auto` anche lui, con `auto_by` = l'attempt chiamante (round 2026-10-01): l'autopilota lo avvia da solo, dopo il suo `after`, come l'avrebbe avviato il chiamante con `start_task` (§7.12). Non se il chiamante è stato avviato a sua volta da un agente: non può avviare agenti, né direttamente né così | allow |
| `update_task {id, title?, description?}` | `Core::update_task` (i campi assenti restano) | ask |
| `move_task {id, status}` | `Core::move_task` in coda alla colonna; `status` è il nome DB di `TaskStatus` (`todo`, `inprogress`, `inreview`, `done`, `cancelled`) | ask |
| `start_task {id, model?, effort?}` | `Core::start_attempt` con il target predefinito del progetto e le opzioni sub-agent dell'attempt chiamante; la modalità è quella dell'attempt chiamante per un sotto task, quella predefinita del progetto per ogni altro task (così un agente Autonomo non avvia in bypass un task qualsiasi); `started_by_attempt` = l'attempt chiamante. Rifiutato se l'attempt chiamante ha a sua volta `started_by_attempt` (profondità 2, letto dal DB alla chiamata); `ConcurrencyLimit`, `Conflict` e gli altri errori tornano come errore del tool. Eccezione (round 2026-10-01): un `ConcurrencyLimit` su un task in Da fare di un progetto con l'autopilota acceso imposta `auto` e `auto_by` = l'attempt chiamante, e risponde `{task_id, queued: true, message}`; il task parte quando lo sceglie lo scheduler, con la stessa modalità, gli stessi limiti sub-agent e lo stesso `started_by_attempt` di un avvio immediato (§7.12) | ask |

- **Ambito:** ogni id è cercato nel progetto dell'attempt chiamante; un id di un altro progetto è "non trovato", come uno inesistente. `"self"` vale il task chiamante ovunque si passi un id.
- Ogni modifica passa dai metodi del Core usati dalla UI: validazioni, lock ed `emit_changed` sono gli stessi.

### 7.5 Lettura dello stdout (correzione E5)

`read_line_capped(&mut BufReader, &mut Vec<u8>, max = 16 MiB) -> io::Result<Line>` usa `fill_buf`/`consume`:
- oltre il cap scarta i byte fino a `\n` e restituisce `Line::TooLong(len)`;
- il loop **continua**;
- per ogni riga troppo lunga emette una Notice (warn) "riga di output > 16 MiB ignorata".

Per stderr il cap è 64 KiB per riga. Cap dei file di log: `stdout.jsonl` 64 MiB e `stderr.log` 8 MiB. Oltre il cap non si scrive più e si registra una Notice.

### 7.6 Normalizzazione (`normalize.rs`, pura, golden test con insta)

`Normalizer::new(process_id, next_idx, worktree)` (il worktree canonico rende relativi i path dei summary e si confronta con il `cwd` di `system/init`). Metodi:
- `on_user_message(&str, ts)`
- `on_line(&Value, ts) -> Vec<EntryOp>`
- `on_stderr(&str, ts)`
- `on_approval_requested(approval_id, &Value /*request*/, can_remember, ts)`
- `on_approval_resolved(approval_id, &ApprovalDecision)`
- `on_approval_cancelled(approval_id)`
- `on_notice(level, text, action: Option<NoticeAction>, ts)`
- `on_stop_requested(reason: StopReason)` (M6): chiamato dal runner quando l'app avvia la stop sequence (Stop, "Rifiuta e ferma", shutdown); vince il primo motivo
- `finish(ts)`

Dove `EntryOp = Upsert(Entry) | Typing(Option<String>)`.

| Input | Output |
|---|---|
| Messaggio utente inviato | `UserMessage` |
| `system/init` | `SessionInit`. Warning se `apiKeySource` indica una chiave API (M5 [V]: con il login claude.ai dell'abbonamento vale `none`, l'unico valore di `NO_API_KEY_SOURCES`; ogni altro avvisa), se manca (fonte della fatturazione non verificabile) o se `cwd` ≠ worktree. **Solo abbonamento** (2026-09-29, in ogni turno, Isolated o Trusted): se `apiKeySource` c'è e non è in `NO_API_KEY_SOURCES` (`normalize::api_key_billing`) e `allow_env_api_key` è spento, il runner ferma subito il turno: `SIGSTOP` del gruppo, discendenti registrati, `SIGKILL` (§7.4 passo 4), Notice error `API_KEY_STOP_NOTICE` "(apiKeySource: X)", `processes.error` con lo stesso testo, stato `failed` senza `stop_reason`. Copre anche ciò che l'app non legge (le impostazioni dell'utente, un `apiKeyHelper` in `~/.claude`). È una reazione: il CLI emette `system/init` subito prima della sua prima richiesta (M5, §13.4), quindi la ferma prima che parta solo se il kill vince la corsa; altrimenti la richiesta viene interrotta a metà. Un `apiKeySource` assente resta solo un warning. Effetto collaterale nel runner: `session_started=1`; se `session_id` ≠ quello atteso si salva quello osservato e si emette una Notice |
| `stream_event` `content_block_delta` (`text_delta`/`thinking_delta`) | `Typing(Some(buffer))`, al massimo ogni 100 ms. `content_block_start` azzera il buffer. Gli altri `stream_event` vengono ignorati |
| `assistant`: ogni blocco di `message.content[]` | `text` → `AssistantText`; `thinking` → `Thinking`; `tool_use` → `ToolCall{Running}`, oppure fusione con quello creato da `can_use_tool`. Più `Typing(None)`. `parent_tool_use_id` dall'envelope |
| `user` con `tool_result` | Il `ToolCall` con quel `tool_use_id` passa a `Succeeded` o `Failed(is_error)`, con `output` (stringa o array di blocchi uniti; le immagini diventano `[image]`; 8 KiB testa+coda). M6: un risultato d'errore di una chiamata negata da una regola dei permessi (`tool_result_meta[].non_execution_kind: "permission-rule"`, o il testo "Permission to use … has been denied", M5) passa a `Denied{testo}`; dopo `on_stop_requested` la chiamata che il CLI rifiuta per l'interrupt (`non_execution_kind: "user-rejected"`) passa a `Cancelled`, senza quel testo |
| `system/permission_denied` (M6; M5 [V]: `tool_use_id`, `decision_reason_type: "rule"`, `message`) | Il `ToolCall` passa a `Denied{message}` |
| `user` con contenuto stringa | Ignorato |
| `system/api_retry` | `ApiRetry`, un'entry per processo aggiornata in place |
| `system/compact_boundary` | Notice info "Contesto compattato" |
| `result` | `TurnEnd` con `limit` classificato dalla costante `LIMIT_PATTERNS` (subtype, `is_error`, testi "Not logged in", "/login", "Login expired", "limit", `billing_error`), più `Typing(None)`. M6: prima i `ToolCall` di `permission_denials[].tool_use_id` ancora `Failed` o `Running` passano a `Denied` (messaggio: il loro output); dopo `on_stop_requested` un `result` d'errore dà `TurnEnd{stopped: Some(motivo), text: None}`: la UI mostra "Interrotto dall'utente" (o "alla chiusura dell'app") invece dell'errore interno `[ede_diagnostic] …` |
| Approvazione richiesta, risolta o annullata | Il `ToolCall` passa a `AwaitingApproval{…}`, poi a `Running` o `Denied{message}`. Se non esiste ancora viene creato dalla richiesta |
| stderr | `Stderr`: le righe che arrivano a meno di 2 s l'una dall'altra finiscono nella stessa entry (upsert, ≤ 64 KiB). ANSI rimosso |
| `finish()` | I `ToolCall` ancora in `Running` o `AwaitingApproval` passano a `Cancelled` |
| Altri `system/*` e tipi sconosciuti | Nessuna entry (restano nel raw log). M5 ha visto `system/status` (`requesting`, prima di ogni richiesta API), `system/hook_started`/`hook_progress`/`hook_response` (hook dell'utente), `system/thinking_tokens`, `system/task_started`/`task_updated`/`task_notification`/`background_tasks_changed` (comandi in background) e `rate_limit_event` (sotto) |

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

**Limiti:** campi di testo 256 KiB; `input` 4 KiB (256 KiB per un tool che ha chiesto approvazione: la card lo mostra intero); `output` 8 KiB.

### 7.7 Ciclo di vita del turno (`runner.rs`) e classificazione

```
run_turn(attempt, prompt, mode):
 1 lock per attempt (tokio::Mutex); poi l'indice processes_one_running come rete di sicurezza
 2 preflight → AppError tipizzato, nessuno spawn:
     CLI trovato · auth LoggedIn (cache ≤ 60 s; LoggedOut → NotLoggedIn; Unknown → consentito con Notice)
     · non in pausa (UsageLimited) · running < max_running (ConcurrencyLimit) · worktree present
     · HEAD == refs/heads/<branch> (BranchMismatch) · policy: se Trusted e il fingerprint del worktree
       ≠ quello approvato, o il worktree fattura fuori dall'abbonamento → turno Isolated + Notice (M6)
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
| `system/init` con una chiave API (`apiKeySource` ≠ `none`) e passthrough spento (2026-09-29) | failed | – (`error` = la Notice) |
| `result` ricevuto ma nessuna uscita entro 30 s | come indicato da `result` | exit_timeout |
| Errore di `spawn()` | failed | spawn_error |

### 7.8 Approvazioni

- **All'arrivo di `can_use_tool`** (campi `tool_name`, `input` e `tool_use_id` obbligatori; `permission_suggestions` resta `Value` grezzo [F]; `decision_reason` facoltativo diventa `AwaitingApproval.reason`. M5 [V]: è una stringa, per esempio `"This command requires approval"`, con accanto `decision_reason_type: "other"`, e manca quando la richiesta porta `blocked_path`; altri campi di 2.1.283: `display_name`, `description`, `blocked_path`):
  - si registra in memoria `Pending{approval_id: uuid, request_id, tool_use_id, input, rules}`;
  - si aggiorna l'entry;
  - si emette `changed` (badge "Attende approvazione" sulla card).
- **Nessuna tabella e nessun timeout.** L'utente può sempre fermare il turno. Alla fine del processo le approvazioni pendenti diventano `Cancelled`.
- **`can_remember`** è vero se c'è almeno una suggestion `addRules` con `behavior:"allow"` e ognuna di queste ha regole tutte con `ruleContent` specifico: non vuoto e non fatto solo di `*`, `:` e spazi (`*`, `:*` valgono il tool intero). Le regole su un tool intero (es. `Bash` nudo) non sono mai memorizzabili. Gli altri tipi di suggestion si ignorano e non vengono mai inoltrati: M5 ha visto che il CLI 2.1.283 aggiunge a ogni richiesta Bash `addDirectories` (il worktree) e `setMode` (`acceptEdits`, una modalità che l'utente non ha scelto); con la regola precedente ("tutte le suggestion sono `addRules`") "Consenti sempre" non era mai disponibile.

**Risposte** (funzione pura `wire::approval_response(&Pending, &ApprovalDecision) -> Value`; si fa eco del `request_id` del CLI):

| Decisione | `response` |
|---|---|
| `Allow{remember:false}` | `{"behavior":"allow","updatedInput":<input originale>}` (**sempre** presente) |
| `Allow{remember:true}` | Come sopra, più `"updatedPermissions":<solo le suggestion addRules/allow, con ogni destination riscritta a "session">`. In più si aggiungono le stringhe `Tool(ruleContent)` ad `attempts.allow_rules`, ripassate nei turni successivi via `--settings`. M5 [V]: le regole di sessione **non** sopravvivono a `--resume` (senza il re-pass il comando richiede di nuovo l'approvazione), mentre la regola in `--settings` evita la richiesta anche in una sessione nuova: il re-pass è necessario |
| `Deny{message, interrupt}` | `{"behavior":"deny","message":"The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said: <msg>","interrupt":<bool>}` |
| `AskUserQuestion` (arriva comunque) | Deny con `"Ask your question in plain text in your reply instead."`, `interrupt:false` |
| Sub-agent (`Agent`/`Task`) di un attempt **con un limite** (round feature del 2026-09-29) | Risposta immediata, mai pendente, sullo stesso percorso di `AskUserQuestion` (`runner/turn.rs`, `count_subagent`). Se `Db::count_subagent` lo concede (`subagents_used < max_subagents`, incremento e controllo in un solo `UPDATE … RETURNING`): `{"behavior":"allow","updatedInput":<input originale>}`, `subagents_used` salvato e `changed` emesso (il pannello del task mostra il contatore). Altrimenti, e anche quando il conteggio non si può scrivere: `{"behavior":"deny","message":"Sub-agent limit for this task reached (N). Complete the work directly without starting sub-agents.","interrupt":false}` con N = `max_subagents`, **senza** il prefisso "The user doesn't want to proceed…" (costruito con `wire::control_success`); il `ToolCall` passa a `Denied` con quel testo. Si conta lo spawn concesso, non quello completato |

- Senza limite (`max_subagents` `None`) un `Agent`/`Task` che chiede comunque (per una regola dell'utente) è un'approvazione normale, pendente e non contata.
- Le richieste arrivano una alla volta nel ciclo del turno (`select!` unico, `on_can_use_tool` sincrono): anche una raffica di spawn in parallelo viene contata esattamente, senza `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS` (non documentata, esclusa).

**JSON di `--settings`** (snapshot-testato; stringhe esatte, senza ambiguità di split; `claude::settings_json(&SettingsParts{allow, ask, env})`, con `DENY_RULES` sempre in testa):
```json
{"permissions":{
  "deny":["Bash(git push *)","Bash(git push)","Read(~/.ssh/**)","Read(~/.aws/**)",
          "Read(~/.claude/.credentials.json)","Edit(~/.claude/**)","Edit(~/.ssh/**)"],
  "allow":["mcp__atm__list_tasks","mcp__atm__get_task","mcp__atm__create_task", /* attempts.allow_rules */ ],
  "ask":["mcp__atm__update_task","mcp__atm__move_task","mcp__atm__start_task"
         /* ,"Agent","Task"   solo con un limite di sub-agent non esaurito (§7.3) */ ] }
  /* ,"env":{"CLAUDE_CODE_SUBAGENT_MODEL":"<alias>"}   solo con un modello dei sub-agent */ }
```
- **Tool board** (round 2026-09-30): lettura e creazione sono `allow`; modifica, spostamento e avvio sono `ask`. Con la regola `ask` il CLI manda `can_use_tool` prima del `tools/call`, con `mcp_server: {"name":"atm","source":"sdk"}` e senza `permission_suggestions`: lo spike del 2026-09-30 lo ha verificato in `default` e in `bypassPermissions` [V]; il 2026-10-01 `real_cli_board_tools_ask_in_every_mode` lo ha verificato con l'app intera in `default`, `acceptEdits` e `bypassPermissions` (CLI 2.1.286) [V] (§13.3). L'host lo tratta come ogni tool che chiede: `Pending`, card di approvazione, nessuna risposta automatica, e poi lascia passare solo il `tools/call` con quel `tool_use_id` (§7.4). Senza suggestion `can_remember` è falso: la card non offre "Consenti sempre", e anche un `Allow{remember:true}` approva solo quella chiamata e non salva regole (mai una regola sul tool intero).
- Senza `env` e senza sub-agent il JSON è quello di prima del round del 2026-09-29 più le regole board (i test piegano `DENY_RULES` nello snapshot e `check.sh` ammette `credentials.json` solo in quella costante).
`-p` ignora in silenzio i settings non validi [V/F]: M5 ha dimostrato che il deny su `git push` funziona [V]: nessun `can_use_tool`, `tool_result` con `is_error` e testo "Permission to use Bash with command git push has been denied.", la voce in `result.permission_denials` (`{tool_name, tool_use_id, tool_input}`) e il remote intatto; il `result` resta `success`. È una difesa in profondità: `sh -c 'git push'` la aggira.

### 7.9 Follow-up, stop, resume

- **Follow-up.** Consentito solo se nessun turno è in corso: il composer è disabilitato durante il turno, lo Stop resta attivo. La coda dei messaggi è rimandata.
- **Sessione:**
  - `session_id` si assegna alla creazione dell'attempt;
  - `session_started=1` solo quando arriva `system/init`;
  - se un turno muore **prima** di `init`, il turno successivo genera un **nuovo UUID**, aggiorna `attempts.session_id` e usa `--session-id` (lezione di Vibe Kanban #2993; correzione E7).
- **Resume.** Stesso cwd canonico (il worktree non si sposta né si rinomina mai). I file `.jsonl` del CLI non si copiano, leggono o spostano mai. Mai due processi sulla stessa sessione (indice unico).
- **Resume fallito** (`No conversation found`, costante `normalize::RESUME_FAILED_PATTERN`): Notice con `action: Some(NoticeAction::NewSession)`; la UI mostra "Nuova sessione", che invia `send_follow_up{fresh_session:true}`.
- **Interrotto dalla chiusura dell'app.** Card e pannello mostrano **"Continua"** (card: `attempt_state == Active` e `last_stop_reason` = `app_shutdown` — Cmd+Q ordinato — oppure `app_restart` — crash, recuperato all'avvio), che invia il follow-up `atm_types::CONTINUE_PROMPT` ("The previous run was interrupted when the app closed. Continue the task.") con `--resume`.
- **Stop sequence** (obiettivo ≤ 13 s; lo stato finale dipende dal nostro flag, non dal subtype di `result`; M5 [V]: il CLI risponde all'interrupt con `{"still_queued":[]}`, aggiunge la riga utente `[Request interrupted by user…]` e chiude con `result` `error_during_execution`, `is_error`, `errors: ["[ede_diagnostic] …"]`, `terminal_reason: "aborted_streaming"`, poi esce con 1: lo stop reale è durato 0,4–0,7 s):
  1. Se `init` è arrivato e stdin è aperto: `{"type":"control_request","request_id":"atm_n_…","request":{"subtype":"interrupt"}}`, poi fino a 5 s di attesa per `result` o uscita.
  2. Chiusura di stdin (l'EOF cancella il prompt pendente [F]), poi fino a 3 s.
  3. `killpg(pgid, SIGTERM)`, poi fino a 3 s.
  4. `killpg(pgid, SIGKILL)` e reap.
- **Shutdown dell'app.** `RunEvent::ExitRequested` → `api.prevent_exit()` una sola volta (flag atomico) → `core.shutdown(deadline 8 s)`: stop in parallelo con tempi compressi 2/2/2 s, finalize con auto-commit → `app.exit(0)`.
- **Recovery all'avvio** (prima che la UI carichi i dati):
  - per ogni process `running` con `app_instance_id` diverso da quello attuale:
    - se `kill(pid,0)` riesce **e** `ps -o command= -p <pid>` contiene il path di claude **e** il `session_id` dell'attempt (un UUID, quindi niente falsi positivi da riuso del PID) → `killpg(SIGTERM)`, 3 s, poi `SIGKILL`, discendenti del leader compresi (§7.4 passo 4);
    - limite: se il leader è già morto (per esempio uscito all'EOF quando l'app è crollata), i comandi Bash che aveva in process group propri sono ormai figli di launchd e la recovery non li riconosce; restano finché non finiscono;
    - in ogni caso: `failed` / `app_restart`, Notice "Esecuzione interrotta dal riavvio dell'app", tool aperti → `Cancelled`, auto-commit;
  - i task in inprogress senza turni attivi passano a inreview;
  - riconciliazione dei worktree (§8.4).
- **Concorrenza.** Contatore globale con `max_running` (default 2): oltre il limite si restituisce `ConcurrencyLimit`, senza coda. Fa eccezione l'autopilota (round 2026-10-01, §7.12): i task con `auto` di un progetto con l'autopilota acceso aspettano in coda, e lo scheduler li avvia quando si libera uno slot; il comando `start_attempt` dell'utente risponde sempre `ConcurrencyLimit`. Un `result` di tipo usage limit o billing **mette in pausa** i nuovi avvii finché l'utente non preme "Riprendi".
- **Notifiche macOS** (round 2026-10-01, `Inner::notify(task, title, body)`): `/usr/bin/osascript -e 'on run argv' -e 'display notification (item 2 of argv) with title (item 1 of argv)' -e 'end run' -- <titolo> <testo>`. I testi sono argomenti dello script, mai parte del suo codice; `--` impedisce che un testo che inizia con `-` diventi un'opzione. `ChildEnv` ripulito, process group proprio, output scartato, `killpg` più `kill_tree` dopo 10 s; gira in un task a parte e non blocca mai chi la chiama. Al massimo una ogni 30 s per task e titolo (per l'app se il task manca): un'approvazione chiesta non nasconde l'esito dell'autopilota che arriva subito dopo; niente se `Settings.notifications` è spento. Con `CoreConfig::notify_log` (test, E2E) la notifica si aggiunge al file come `[task_id, titolo, testo]` (JSON, una per riga) invece di lanciare `osascript`. Partono: un'approvazione in attesa su un task con `auto` («Autopilota: approvazione richiesta»), la pausa per limite d'uso mentre un progetto ha l'autopilota acceso («Autopilota in pausa», solo al passaggio in pausa), e gli eventi dello scheduler (§7.12).

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
- **Topbar:** chip con il solo `subscriptionType` (dal 2026-09-30 niente email né organizzazione, §10.1; nel tooltip metodo di accesso e provider).
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
| Verifica dell'autopilota (`verify_timeout_secs`) | 600 s di default, da 10 a 3600 s; poi `killpg` più l'albero |
| Log di una verifica / riassunto (`verify_summary`) | 8 MiB / ultimi 8 KiB |
| Attesa dell'EOF dell'output dopo la fine della verifica | 2 s |
| Tick di sicurezza dello scheduler | 60 s |
| Verifiche di fila se HEAD cambia tra verifica e merge | 3 |
| Notifica `osascript` | 10 s; al massimo una ogni 30 s per task e titolo |

### 7.12 Autopilota (round 2026-10-01; `autopilot.rs`, `autopilot/verify.rs`)

Un interruttore per progetto (`projects.autopilot`). Quando è acceso il Core guida da solo coda → avvio → verifica → correzione → merge dei task con `auto`. Tutto lo stato sta nel DB (`tasks.auto`, `after_id`, `auto_by`, `attempts.verify_*`): un riavvio ricostruisce la coda. In memoria ci sono solo le verifiche in corso (`Autopilot::verifying`, con il process group del comando).

- **Scheduler.** Un solo task tokio per Core, avviato da `Core::startup` dopo il recupero. Lo svegliano (`tokio::sync::Notify`, un permesso resta se è occupato) la fine di ogni turno (dal supervisore di `runner.rs`, su entrambi i percorsi, a slot già libero), `update_project`, `create_task`, `update_task`, `move_task`, `delete_task`, `merge_attempt`, `discard_attempt`, `update_settings`, `resume_agents` e lo `start_task` in coda; più un tick ogni 60 s. Non fa niente se l'app si chiude (`closing`: termina) o è in pausa per limite d'uso.
- **Un passaggio.**
  1. Prima le **correzioni in attesa** (`Db::pending_fixes`): un attempt già avviato ha la precedenza sulla coda. Una che non trova ancora lo slot ferma il passaggio.
  2. Poi, per ogni progetto con l'autopilota, i candidati di `Db::autopilot_candidates` (§5.2) in ordine di posizione, con `start_attempt_by`: branch target e modalità predefiniti del progetto, modello dei default. Un task affidato da un agente (`auto_by`) parte come l'avrebbe avviato quell'agente con `start_task`: `started_by_attempt` = quell'attempt (così la catena resta a profondità 2, §10.2), la sua modalità per un sotto task, i suoi limiti sub-agent. Se l'attempt non c'è più, i default del progetto ma sempre con `started_by_attempt`.
  3. `ConcurrencyLimit`, `UsageLimited` e `Busy` fermano il passaggio, il task resta in coda; `Conflict` (avviato nel frattempo) lo salta; ogni altro errore toglie `auto` con la notifica «Impossibile avviare «X»: …».
- **Fine di un turno** di un attempt attivo il cui task ha `auto` e il cui progetto ha l'autopilota (`after_turn`, in un task a parte; la fine del turno azzera prima `verify_pending`):
  - `Completed` → verifica;
  - ogni altro esito (`Failed`, `Killed`, Stop dell'utente, `AuthFailure`, `UsageLimit`) → l'autopilota lascia il task (`auto = 0`) con una notifica che dice perché. Non riprova mai da solo e non scavalca mai uno Stop.
- **Verifica** (`Inner::verify`). Prende il lock dell'attempt (merge, follow-up e discard aspettano); niente se il tentativo è chiuso, se gira un turno o se l'app si chiude. Senza `verify_command` conta come passata. Altrimenti:
  - `/bin/sh -c <verify_command>` con cwd nel worktree: il comando è dell'utente e si legge **solo dal DB**, mai da file del repository;
  - `ChildEnv` ripulito (§7.2) come l'agente, stdin null, stdout e stderr sulla stessa pipe;
  - `process_group(0)`, `kill_on_drop`; il pgid si registra in `Autopilot::verifying`;
  - allo scadere di `verify_timeout_secs`, e anche dopo un'uscita normale, `verify::kill_group`: `killpg` del gruppo più i discendenti dei suoi membri trovati con `ps` (un server avviato dai test con `setsid`), così la pipe arriva all'EOF;
  - output in `logs/<attempt>/verify/<ms>.log` (cartella 0700, file 0600, al massimo 8 MiB), coda di 8 KiB in `verify_summary`;
  - `begin_verify` registra `running` e `verify_head` (HEAD prima di partire), `finish_verify` l'esito: `passed` (exit 0), `failed` (exit ≠ 0, segnale, timeout) o `error` (non è partita);
  - `TaskCard::verifying` è vero intanto, e `get_branch_status` mette `merge_blocked` «La verifica dell'autopilota è in corso».
- **Verifica passata.**
  - Senza «Merge automatico»: notifica «pronto per il merge»; il task resta In revisione con `auto`.
  - Con «Merge automatico»: `get_branch_status`. Se ci sono conflitti, il follow-up `conflict_prompt` (lo stesso testo di «Risolvi con l'agente», §8.7), contato come correzione. Altrimenti `Core::merge_attempt_at` con `merge_message`: sotto il lock dell'attempt controlla che HEAD sia ancora `verify_head` (se no, una nuova verifica, al massimo 3 di fila, poi l'autopilota lascia il task) e che il worktree sia pulito (`status --porcelain` vuoto: niente file scritti dalla verifica o dopo, altrimenti `Invalid` e l'autopilota lascia il task). Poi lo squash merge di §8.7 e la notifica «mergiato». Un merge sveglia lo scheduler, che avvia i task con `after_id` sul task mergiato.
- **Verifica fallita.** Se `verify_fixes < autopilot_max_fixes`, il follow-up di correzione (in inglese: il comando, come è finito, la coda dell'output; fake-claude lo gioca come `fix_on_resume`), sempre **dopo** che il supervisore ha liberato lo slot, mai dentro `finalize`. Esaurite le correzioni: notifica «Verifica fallita dopo N tentativi», il task resta In revisione e l'autopilota lo lascia. Un errore della verifica (`error`) lo lascia subito.
- **Invio di una correzione** (`deliver_fix`). Si conta in `verify_fixes` prima del follow-up e si restituisce se non parte.
  - Se gira un turno (un follow-up dell'utente durante la verifica), niente: la sua fine torna in `after_turn`.
  - `ConcurrencyLimit`, `UsageLimited` o l'app in chiusura: il prompt va in `verify_pending` e lo manda lo scheduler, prima della coda, quando c'è uno slot.
  - `Busy`: niente.
  - Ogni altro errore lascia il task, a meno che l'utente non l'abbia ripreso nel frattempo (discard, eliminazione, `auto` tolto).
- **L'utente riprende il task.** Un discard toglie `auto` (altrimenti lo scheduler lo riavvierebbe subito); discard ed eliminazione uccidono prima la verifica in corso dell'attempt, che altrimenti terrebbe il lock fino al timeout. Togliere `auto` o spegnere l'autopilota del progetto ferma la catena alla prossima decisione (una verifica in corso finisce).
- **Chiusura e riavvio.** `Core::shutdown` imposta `closing`, sveglia lo scheduler (che termina) e uccide il gruppo di ogni verifica registrata; una verifica che parte dopo vede `closing` e si uccide da sola. Una verifica interrotta così resta `running`: allo startup `mark_stale_verifies` la porta a `error` con la notifica «… interrotta dalla chiusura dell'app», e il task resta In revisione con `auto` finché l'utente non lo riprende. Un turno interrotto dall'app non riparte da solo (lo «Continua» resta all'utente); una correzione in `verify_pending` sì.

### 7.13 Pianificatore (round 2026-10-02; `plan.rs`)

- **Avvio** (`Core::start_plan`): prompt con trim (vuoto → `Invalid`), modello in `MODEL_ALIASES` o nessuno (poi `default_model` del progetto e delle impostazioni, come `start_attempt`). `Db::insert_plan` (`Conflict` con un piano `running`/`awaiting` nel progetto), poi l'attempt del piano con `start_attempt_by`: modalità Default qualunque sia quella del progetto, `default_target_branch`, nessun limite né modello dei sotto agenti. Se l'avvio fallisce (`ConcurrencyLimit`, `NotLoggedIn`, …) il piano viene eliminato e l'errore restituito: non resta nulla. Il turno del piano conta in `max_running`.
- **argv** (`TurnArgs::plan`, da `task.kind == Plan` in `plan_turn`): `--permission-mode=default` anche nei follow-up (anche `processes.permission_mode`), `claude::PLAN_DENY` (`Edit`, `Write`, `NotebookEdit`, `Bash`, `mcp__atm__start_task`, `mcp__atm__move_task`, `mcp__atm__update_task`) dopo `DENY_RULES` nei `deny` di `--settings` (fuori dalla costante `DENY_RULES`), quei tre strumenti board tolti da `ask`, sempre **Isolated** anche in un progetto Trusted (`--setting-sources=user`, `--strict-mcp-config`: nessun server MCP né regola allow del repository o dell'utente gli dà un modo di scrivere), e `--append-system-prompt=` `claude::plan_append_prompt` (inglese: esplora, crea task di primo livello ben descritti, niente `self`, sotto task dei task creati, `after` per le dipendenze, non implementa nulla, chiude con un riassunto). Snapshot `flow__plan_argv`.
- **Strumenti board**: con un piano come chiamante `update_task`, `move_task` e `start_task` sono rifiutati anche dall'host (`Invalid`); `create_task` scrive `planned_by` = id del piano ed emette `changed` (mai `auto`); un `parent_id` deve essere un task creato dallo stesso piano (`Invalid` per i task dell'utente).
- **Approvazioni**: un piano in attesa di un'approvazione notifica «Pianificazione: approvazione richiesta» (`notify_pending`, come i task dell'autopilota): il piano non è sulla board, quindi né la pillola della topbar né il Riepilogo la contano; la sua card la mostra nel transcript.
- **Fine del turno** (`Inner::plan_ended`, dal seguito del turno di `autopilot.rs`, al posto di verifica e merge; niente con l'app in chiusura), sotto il lock del task del piano, solo da `running` (un errore lo porta a `failed`, `settle_or_fail`: un piano `running` senza turno rifiuterebbe ogni altro piano fino al riavvio):
  - `Completed` senza task creati, o progetto in AcceptEdits/BypassPermissions → `Db::launch_plan(plan, project.autopilot)` e sveglia dello scheduler (`started`);
  - `Completed` in Default → `awaiting` e notifica «Pianificazione» ««titolo»: avviare N task?» (`Inner::notify`, stessa impostazione e throttle);
  - ogni altro esito (Stop dell'utente, crash, limite, …) → `failed`, i task creati restano come sono.
  
  Poi, in ogni caso, il discard esistente dell'attempt (snapshot, worktree rimosso) e la cancellazione del suo branch `atm/…`; il transcript resta (`PlanView::attempt_id`).
- **Conferma** (`Core::resolve_plan`): solo un piano `awaiting` (`Invalid` altrimenti, anche se `launch_plan` risponde `Conflict`); Sì → `launch_plan` + sveglia, No → `dismissed`.
- **Scheduler**: serve i progetti con l'autopilota **o** in `Db::projects_with_launch`; candidati `autopilot_candidates(id, project.autopilot)`, avviati con i default del progetto (`after_id` e `max_running` valgono); un errore non ritentabile toglie `auto` e `launch`. Verifica, correzioni e merge restano dell'autopilota.
- **Startup** (`Inner::recover_plans`, dopo `recover_orphans`): ogni piano `running` senza processo in corso (`Db::stale_plans`) si chiude dall'esito del suo ultimo processo come alla fine del turno (un turno completato appena prima della chiusura dell'app avvia i task o chiede conferma; uno interrotto → `failed`; nessun processo → `failed`); poi il discard (con il branch) di ogni attempt attivo di un piano non `running` (`Db::plan_attempts_left`), e la cancellazione dei branch `atm/…` rimasti degli attempt chiusi dei piani (`Db::closed_plan_branches`: una cancellazione fallita o interrotta).

---

## 8. Git e worktree (`git.rs`)

### 8.1 Runner irrobustito (correzione E6)

- **Binario:** `command -v git` dalla login shell, con fallback `/usr/bin/git`. Richiede ≥ 2.44 (`merge-tree --write-tree` da 2.38, `GIT_NO_LAZY_FETCH` da 2.44): un git più vecchio compare tra i problemi dell'ambiente e il runner del Core non esegue nessun'altra chiamata (`Git::gated`: errore `Git` "git X è troppo vecchio: serve almeno la versione 2.44"), perché ignorerebbe `GIT_NO_LAZY_FETCH` (2026-09-29).
- **Ogni chiamata:**
  ```
  git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c core.quotePath=false -c color.ui=never -c gc.auto=0 -C <dir> …
  ```
  - I diff aggiungono `--no-ext-diff --no-textconv`; i commit `--no-verify`.
  - Sulle scritture, su `merge-tree` e su `status` (che esegue il clean filter dei file con i dati di stat cambiati) si legge prima `git config --show-scope --null --get-regexp '^(hook|merge|filter|gpg)\.'` e si passano via `GIT_CONFIG_*` gli override di `git::config_overrides_from`: ogni hook `hook.<n>.enabled=false`; ogni merge driver `/usr/bin/false`; un filter driver con `clean`/`smudge`/`process` definito nella configurazione del repository (scope `local`/`worktree`, quella che un agente può scrivere con `git config` dal suo worktree), salvo i comandi di Git LFS, viene neutralizzato (`clean`/`smudge` = `cat`, `process` vuoto, `required=false`), altrimenti ogni auto-commit e `merge --ff-only` lo eseguirebbe fuori da ogni turno; un programma `gpg.*` nella stessa configurazione spegne la firma (`commit.gpgSign=false`, `tag.gpgSign=false`). I filtri globali dell'utente (Git LFS) restano (M6-REVIEW).
- **Env:**
  - la stessa lista di rimozione `GIT_*` del §7.2, più le chiavi API e le variabili di una sessione padre (M6-REVIEW: git esegue filtri e helper della configurazione del repository, che non devono ricevere né la chiave né il canale della sessione);
  - `LC_ALL=C LANGUAGE=C`, perché il git locale è in italiano [F] e il parsing richiede i messaggi C;
  - `GIT_TERMINAL_PROMPT=0 GIT_EDITOR=true GIT_PAGER=cat`;
  - `GIT_NO_LAZY_FETCH=1` su **ogni** chiamata (2026-09-29): in un partial clone la lettura di un oggetto mancante farebbe un fetch dal remote promisor, cioè eseguirebbe ciò che dice la configurazione del repository (`remote.<n>.uploadpack`, il trasporto, `core.sshCommand`), che un agente può scrivere con `git config` dal suo worktree, fuori da ogni turno (diff, `merge-tree`, `worktree add` del tip). L'app non fa mai fetch: un oggetto mancante è un errore. Test: `tests/git.rs::a_partial_clone_never_fetches_from_the_app` (il controllo con git semplice esegue l'`uploadpack`);
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
- **Messaggio di default** (modificabile, costruito dalla UI con `atm_types::merge_message`): titolo, riga vuota, descrizione (≤ 2000 caratteri, omessa se vuota), riga vuota, `ATM-Attempt: <id>`.
- **Target divergente senza conflitti:** si mergia direttamente, senza rebase.
- **Conflitti:** "Risolvi con l'agente" invia questo follow-up:
  > "This branch conflicts with `<target>` in: <files>. Run `git merge <target>`, resolve every conflict preserving both intents, run the project's tests if available, and commit the merge. Do not push."
  
  Dopo quel turno `merge-tree` risulta pulito.

### 8.8 Regole sullo stato sporco e cose vietate

- Un turno può partire con il worktree sporco; l'auto-commit raccoglie tutto.
- Il checkout principale dell'utente viene modificato **solo** da `merge --ff-only`.
- **Mai:** `reset --hard`, `push`, `fetch`, `stash`, `checkout` nel repo principale, `branch -D` su branch non `atm/*` o su attempt non `merged`, `worktree prune` globale, `rm -rf` fuori da `worktree_root`.

### 8.9 Fingerprint della configurazione Claude (M6; approvazione del commit e fatturazione: 2026-09-29)

- Si calcola SHA-256 (sha2, hex minuscolo) sui record di `.claude/**`, di `.mcp.json` e dei file del repository che la loro configurazione esegue, ordinati per `(path, tipo)` (byte), ognuno come `path\0tipo\0len\0contenuto` (`len` decimale, `path` relativo alla radice come visto attraverso i link). Due sorgenti con lo stesso codice di visita (trait `Source` di `git/fingerprint.rs`): un **checkout su disco** (il worktree prima di ogni turno) e un **commit** letto dal database degli oggetti (quello approvato). Un checkout pulito di un commit ha il fingerprint di quel commit (`tests/git.rs::commit_config_is_the_config_of_a_checkout_of_it`). Implementazione in `git/fingerprint.rs` (M6, rivista in M6-REVIEW e il 2026-09-29):
  - tipo `d` directory, contenuto vuoto: ogni directory visitata conta nei limiti (un albero di directory vuote non si visita gratis) e una directory vuota che compare o sparisce cambia il fingerprint. Un submodule del commit (`160000`) è la directory vuota che ha un worktree nuovo;
  - tipo `f` file regolare, `x` file regolare con un bit di esecuzione (uno script di hook reso eseguibile cambia il fingerprint; nel commit il blob `100755`): contenuto = i byte;
  - tipo `l` symlink: contenuto = la stringa del target (nel commit il contenuto del blob `120000`). Il link viene **anche seguito**, perché il CLI legge attraverso di esso: il target deve risolversi dentro la radice (repo o worktree; altrimenti errore, quindi mai Trusted) e ciò a cui punta viene registrato sotto il path del link (il suo `f`/`x`, o il contenuto della directory). Senza questo, `.claude/settings.json → ../x.json` permetterebbe di cambiare la configurazione senza cambiare il fingerprint. Un link pendente è solo il suo `l`: quando il target compare, il fingerprint cambia. Nel commit i link si risolvono dentro il suo albero come farebbe il kernel in un suo checkout (componenti divise a ogni `/`, `file/` e `file/.` non attraversano un file, al più `MAX_LINK_HOPS` = 32 link); un target assoluto o un `..` sopra la radice esce dal repository;
  - tipo `o` ogni altra voce (FIFO, socket, device), contenuto vuoto: mai aperta (una FIFO bloccherebbe il controllo);
  - tipo `e` (solo checkout) un file che la configurazione esegue il cui path è dentro la radice ma che un link porta fuori (il `python` di un virtualenv): contenuto = il path assoluto dove esce dalla radice, con il resto del path applicato come testo (2026-09-29: niente `realpath`, che toccherebbe ciò che il link nomina, come una share di rete montata al volo o un volume bloccato). Ciò che sta fuori dalla radice non viene né letto né guardato, come `node` o `uv` nel `PATH`. Un commit non ha una posizione su disco: lì lo stesso caso è un errore (non approvabile);
  - **file eseguiti dalla configurazione** (M6-REVIEW): i comandi di `.claude/settings.json` e `.claude/settings.local.json` (ogni `command`, quindi `hooks[*].hooks[*].command` e `statusLine.command`; `apiKeyHelper`, `awsAuthRefresh`, `awsCredentialExport`, `otelHeadersHelper`; i valori di `env`; il `path` di un `extraKnownMarketplaces` con sorgente `directory` o `file`) e ogni stringa di `.mcp.json` (`command`, `args`, `env`, …) vengono divisi in parole come farebbe una shell; ogni parola che nomina un file regolare dentro la radice (relativa, assoluta sotto la radice di un checkout, o dopo `$CLAUDE_PROJECT_DIR`/`$PWD`) aggiunge il record di quel file sotto il suo path canonico, e la directory di un marketplace viene visitata tutta. Così `node tools/mcp.js` o un hook `./scripts/setup.sh` non si possono modificare in un worktree senza che il turno giri Isolated;
  - **nomi** (2026-09-29): il CLI apre i suoi file per nome e un filesystem che ignora maiuscole e forme Unicode (APFS, HFS+) apre `.claude/Settings.json` o `.claude/ſettings.json` (U+017F) come `.claude/settings.json`. Quale voce è un file di configurazione (`.claude` e `.mcp.json` nella radice, `settings.json` e `settings.local.json` in `.claude`) lo decide ciò che si aprirebbe con quel nome, mai il nome elencato (`Source::opened_as`): nel **checkout** il nome elencato uguale byte per byte, altrimenti la voce con lo stesso device e inode del nome cercato con `fstatat` nella directory (lo decide il filesystem), registrata e analizzata sotto il nome del file di configurazione (quindi le sue chiavi di fatturazione, le regole ampie e i file che esegue contano; due voci con lo stesso inode sono un errore); nel **commit**, che non ha ancora un filesystem, un'altra voce che un filesystem potrebbe aprire con quel nome (lo stesso nome con altre maiuscole ASCII, o con byte non ASCII le cui lettere e i cui punti ASCII ci stanno: `fingerprint::may_fold_to`, prudente) rende la configurazione non verificabile (`Invalid` che la nomina). Test: `tests/git.rs::settings_under_another_name_are_what_the_cli_opens`, `tests/flow.rs::a_settings_file_under_another_case_is_checked_like_the_settings`;
  - **accesso al filesystem** (checkout): la radice si apre una volta; ogni apertura parte dal suo descrittore, un componente alla volta con `O_NOFOLLOW` (e `O_NONBLOCK` per i file): il kernel non segue mai un link da solo, nemmeno in una directory intermedia, quindi una directory sostituita con un link a `~/.claude` durante la visita dà errore e non viene mai letta. I link li risolve la visita stessa (solo metadati), un componente alla volta dal descrittore della radice come nel commit (al più 32 link), e devono restare nella radice: un target assoluto o un `..` sopra la radice esce, senza guardare oltre (`tests/git.rs::a_link_out_of_the_root_is_never_looked_at`). Prima si visita l'albero, poi si ordinano i record e si legge e si calcola l'hash di un file alla volta (in memoria al massimo un file, più le impostazioni già lette per cercarvi i comandi, hashate come sono state lette);
  - **accesso agli oggetti** (commit): `Git::commit_config_snapshot` avvia un solo `git cat-file --batch` nel repository con il runner irrobustito (§8.1: flag, env ripulito, sola lettura, `GIT_NO_LAZY_FETCH=1`, niente filtri né textconv), entro 60 s in tutto e ucciso alla fine; legge alberi (in cache per la visita, ordinati per nome e cercati per bisezione; ≤ 16 MiB **in tutto**, `git::MAX_TREE_BYTES`; un albero con un nome ripetuto, vuoto, `.`, `..` o con `/` non è verificabile) e blob uno alla volta, un blob oltre il budget residuo non viene letto. Il contenuto è il blob com'è salvato: una conversione di fine riga, un attributo `ident` o un filtro smudge (Git LFS) che cambia un file di configurazione al checkout rende ogni worktree diverso, quindi Isolated (limite noto). La visita è codice bloccante in `spawn_blocking`, le letture girano sul runtime (`Handle::block_on`); l'attesa è limitata anche dall'esterno (`git::COMMIT_CONFIG_TIMEOUT`, 70 s). Il risultato si tiene in cache per `(repo, commit)` (un commit non cambia; al più 64 voci), **anche quando è un errore `Invalid`** (un limite, un link, un nome: si ripeterebbe uguale), così un tip costruito apposta non viene rivisitato a ogni lettura di un progetto Trusted; gli altri errori (tempo scaduto, lettore fallito) no;
  - limiti, che danno errore e mai un hash parziale: 2000 record (directory comprese), 64 MiB di contenuto, 32 livelli di directory (un ciclo di link ne raggiunge uno), 16 MiB di alberi letti (commit), 4096 parole dei comandi cercate come path (`git::MAX_COMMAND_WORDS`), 2^20 componenti di path risolti (`git::MAX_WALK_STEPS`; nel checkout ogni componente conta quante aperture dalla radice costa), 60 s per visita (`git::MAX_WALK_TIME`, controllati tra un passo e l'altro; errore `Io`). Una parola più lunga di `PATH_MAX` non nomina nessun file che il kernel aprirebbe e si salta. Test: `tests/git.rs::commit_and_checkout_walks_are_bounded`;
  - `ConfigSnapshot` ha anche il digest di ogni record (per dire quali path differiscono), ciò che le impostazioni consentono senza chiedere (regole `permissions.allow` di un tool intero o con `*`, `additionalDirectories`, `enableAllProjectMcpServers`), mostrato nella conferma, e `billing` (sotto).
- **Fatturazione solo con l'abbonamento** (requisito dell'utente, 2026-09-29): `ConfigSnapshot::billing` elenca, con una frase italiana ciascuna (`.claude/settings.json imposta apiKeyHelper`, `… imposta env.ANTHROPIC_BASE_URL`), ciò che in `.claude/settings.json` e `.claude/settings.local.json` farebbe fatturare gli agenti fuori dall'abbonamento: le chiavi di primo livello `git::BILLING_SETTINGS_KEYS` (`apiKeyHelper`: la sua uscita diventa la chiave API; `awsAuthRefresh`, `awsCredentialExport`: credenziali per Amazon Bedrock) con un valore non nullo, e i nomi di `env` in `git::BILLING_ENV_VARS` (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`: chiave o token dell'API; `ANTHROPIC_BASE_URL`: un altro endpoint, che fattura a modo suo e riceverebbe le credenziali dell'abbonamento; `CLAUDE_CODE_USE_BEDROCK`/`_VERTEX`/`_FOUNDRY`: un cloud provider) con qualunque valore. Anche un file di impostazioni non vuoto che non è JSON valido finisce nella lista: non si può controllare. `otelHeadersHelper`, `statusLine` e l'`env` di `.mcp.json` (arriva a un server MCP, non alle richieste del CLI) non contano. Le liste sono costanti documentate e crescono con gli helper di credenziali del CLI.
  - **Approvazione rifiutata:** se il commit da approvare ha voci in `billing`, `set_project_security(Trusted)` fallisce con `Invalid`, nominandole ("Configurazione Claude non approvabile (branch main, commit abc1234): .claude/settings.json imposta apiKeyHelper. …"); nulla viene salvato. `Project.trust_error` lo dice anche per un'approvazione precedente al controllo.
  - **Per turno:** un worktree con voci in `billing` gira Isolated con `BILLING_WORKTREE_NOTICE` più le voci, **qualunque sia il fingerprint** (anche un'approvazione di M6 che le conteneva); lo stesso al `system/init` (turno fermato).
  - **A runtime:** il `system/init` di ogni turno con una chiave API ferma il turno (§7.6), per ciò che l'app non vede (impostazioni dell'utente).
- **Approvazione:** calcolata **sul commit di punta del branch target predefinito del progetto** (`default_target_branch`, da cui `start_attempt` crea i worktree), non sui file del checkout principale: `Core::security_snapshot` la calcola **una volta, prima della conferma nativa** e restituisce anche branch e commit (`ConfigBase`), che la conferma nomina; la salva solo se un nuovo calcolo sul tip, dopo la conferma e sotto il lock del progetto, dà lo stesso valore (`Core::apply_project_security`; altrimenti `Conflict`: la configurazione del branch è cambiata mentre il dialog era aperto; un tip nuovo con la stessa configurazione è la stessa approvazione). Anche lo stato salvato (policy, bypass, fingerprint) si scrive solo se è ancora quello letto prima del dialog (compare-and-set in un solo `UPDATE`, che azzera anche un default Autonomo quando il bypass viene tolto). Restando Trusted senza riapprovare, si tiene il fingerprint approvato. Un errore del calcolo (branch che non si legge, configurazione non verificabile, voci in `billing`) rifiuta l'approvazione. Un repository senza configurazione è approvabile: il fingerprint vuoto (`e3b0c442…`) è valido. Rifiutata se il toplevel del repo è `$HOME`: il suo `.claude` è quello dell'utente, che l'app non legge mai (§10.1); per lo stesso motivo nessun fingerprint di `$HOME`, né del checkout né dei suoi commit, viene mai calcolato né considerato valido (la home, anche canonica, si legge una volta all'avvio).
- **I file locali del checkout principale non contano:** `.claude/settings.local.json` non tracciato, modifiche non committate, file ignorati non arrivano nei worktree e non entrano nell'approvazione; non bloccano più Trusted (prima di questa modifica i turni giravano Isolated finché esistevano).
- **`Project.trusted`** è calcolato a ogni lettura: policy Trusted e fingerprint approvato uguale a quello del commit di punta attuale del branch target predefinito (senza voci in `billing`); **`Project.trust_error`** dice perché non si può calcolare o approvare (M6-REVIEW), e la pagina Impostazioni del progetto lo mostra, rileggendo il progetto a ogni visita (un commit nuovo non manda eventi). Se il branch va avanti con una configurazione diversa, il progetto non è più `trusted` e gli attempt nuovi, che partono dal tip nuovo, girano Isolated finché l'utente non riapprova; un commit che non tocca la configurazione non cambia nulla.
- **Prima di ogni turno** (`Inner::untrusted_reason`): calcolata sul worktree, com'è su disco. Voci in `billing` → Isolated con `BILLING_WORKTREE_NOTICE`. Uguale all'approvata → Trusted. Diversa (o non calcolabile) → il turno gira Isolated con una Notice che dice perché, confrontando con la configurazione del **commit da cui è partito l'attempt** (`attempts.base_commit`, in cache): `UNTRUSTED_BASE_NOTICE` più `Commit di partenza: <sha> (<target>)` se quel commit non ha la configurazione approvata (il branch target è andato avanti dopo l'approvazione, o l'attempt ha un altro target: riapprovare), `UNVERIFIABLE_BASE_NOTICE` più `Motivo: <errore>` se non si può verificare; altrimenti è il worktree a essere cambiato: `UNTRUSTED_WORKTREE_NOTICE` più `File diversi: <path>` (i record diversi dal commit di partenza), `UNVERIFIABLE_WORKTREE_NOTICE` più `Motivo: <errore>` se il worktree non si può verificare. La stessa riga va sullo stderr dell'app.
- **All'avvio del CLI:** un turno Trusted ripete lo stesso controllo al primo `system/init`; se è cambiato tra il controllo e l'avvio (un processo lasciato da un turno precedente, un hook della configurazione stessa) il turno viene fermato con `CHANGED_AT_START_NOTICE` e il motivo, e il successivo gira Isolated. A quel punto il CLI ha già caricato la configurazione (anche un `ANTHROPIC_BASE_URL` o un provider appena comparsi) e sta per mandare la prima richiesta: il suo gruppo viene **congelato** (`SIGSTOP`) mentre il controllo gira, poi ripreso (`SIGCONT`) se nulla è cambiato, altrimenti ucciso subito come nello stop per `apiKeySource` (albero registrato, `SIGKILL`); il turno è `failed` senza `stop_reason` (non uno stop dell'utente), con la Notice (livello errore) e `processes.error` (2026-09-29). Test: `tests/flow.rs::a_config_change_during_the_cli_start_stops_the_turn`, `a_billing_key_gained_during_the_cli_start_kills_the_turn_at_once`. È un rilevamento: gli hook `SessionStart` e i server MCP sono già partiti a quel punto.
- **Revoca:** togliere il bypass o passare a Isolated ferma i turni in corso del progetto che li usano (argv con `--allow-dangerously-skip-permissions`, o senza i flag di isolamento; anche quelli non ancora lanciati), con `REVOKED_NOTICE`: una revoca non aspetta la fine del turno. Se la parte che alza il livello viene annullata nel dialog (o non si può approvare), la parte che lo abbassa si applica comunque.
- **Limite noto (M6-REVIEW):** l'esecuzione indiretta non è coperta: `npm run x`, `npx pkg@latest`, `bash -c "cd tools && node server.js"`, un hook che lancia strumenti del repository, i file che questi caricano. La conferma nativa e il README lo dicono.
- **Limite noto (2026-09-29):** conversioni al checkout (fine riga, `ident`, smudge/LFS) dei file di configurazione rendono ogni worktree diverso dal commit approvato: Isolated con la Notice che nomina i file. Un attempt con un target diverso da quello predefinito è Trusted solo se il suo commit di partenza ha la stessa configurazione approvata.
- `git::config_fingerprint`, `git::config_snapshot` e `Git::commit_config_snapshot` sono `async`: leggono e calcolano l'hash in `spawn_blocking`, fuori dai worker tokio.

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
- **Scritto a mano** (round feature del 2026-09-29): il menu del progetto, `widgets/context_menu.rs` (`MenuState`, `ContextMenu`, `ContextMenuItem`), guidato da signal e senza script (`dropdown_menu` upstream ha un `<script>`). `role=menu` con `role=menuitem`, posizione fissa da `style:left/top` reattivi e limitata alla finestra; il primo elemento prende il focus, ↑/↓/Home/End lo spostano. Si chiude con Esc, un `pointerdown` fuori da menu e trigger, un click su una voce, `blur` o `resize` della finestra; Esc, Tab e una voce riportano il focus sul trigger. Niente chiusura sullo scroll né Shift+F10 (rimandati). Nessuna nuova feature di web-sys, nessuna modifica alla CSP.
- **Non usati:**
  - sheet, select, dropdown_menu, popover, hover_card, command, drawer: hanno script;
  - sonner: dipende da JS del sito;
  - drag_and_drop: modifica il DOM;
  - sidenav: richiede un router;
  - shimmer: non necessario.

### 9.2 Schermate

**Layout:** sidebar (240 px) | topbar con nome e path del progetto e i tab **Riepilogo | Task | Impostazioni** | pagina del progetto: Riepilogo; Task = board (Kanban o Lista) più il pannello del task (split a destra, 55 %, visibile quando un task è selezionato); Impostazioni del progetto. Niente router: `AppCtx { env, projects, project, project_view, remove_target, board_version, cards, open_task, task_dialog, detail_version, toasts }` come signal. `cards` (le card della board, di `views/board.rs`) e `task_dialog` (la modalità dell'unico `TaskDialog`, montato dalla board) stanno in `AppCtx` perché anche il pannello del task li usa (round 2026-09-30): il pannello apre lo stesso dialog per "Aggiungi sotto task" invece di montarne un secondo, che duplicherebbe `#task-title` e gli altri id.
- **Pagina del progetto** (round feature del 2026-09-29): `ProjectView = Overview | Tasks | Settings`, non persistita. `AppCtx::select_project(id, view)` imposta `open_task = None`, il progetto e la pagina; la chiamano il click nella sidebar, `add_repository`, l'auto-selezione di `refresh_projects` e le voci del menu. Ogni selezione atterra sul **Riepilogo**, tranne la voce di menu "Impostazioni progetto" (Impostazioni) e il `?task=` del mock (Task). **Nessun Effect su `project` scrive `project_view`**: la voce di menu imposta progetto e pagina insieme. Un click sul progetto già selezionato non fa nulla (la pagina resta). Un click su un tab non chiude il pannello del task.
- `Layout` mette ogni pagina dietro un proprio `<Show>` su un `Memo<ProjectView>`, mai un `match` ricostruito sul posto (Leptos 0.8 terrebbe i vecchi handler): ogni visita è un montaggio nuovo. Riepilogo e Impostazioni scorrono (`overflow-y-auto`). Senza progetto, una volta caricata la lista, `Layout` mostra `board::NoProject` ("Nessun progetto", "Aggiungi repository").
- Tab: `role=tablist` "Pagine del progetto" con `role=tab`, `aria-selected`, tabindex 0 solo sul tab scelto, ←/→/Home/End (circolari); le pagine sono `#project-page-<v>[role=tabpanel]` (un `div` con `aria-labelledby` sul suo tab) dentro l'unico `main` del layout, che resta il landmark: un `main` con un altro ruolo non lo sarebbe più. I tab stanno fuori dalla closure che mostra nome e path del progetto.
- Il dialog globale è diventato **"Impostazioni app"**: solo le impostazioni generali (§5.2), senza tab; il bottone nella sidebar si chiama così. Le impostazioni del progetto sono la pagina Impostazioni.
- **Direzione visiva** (redesign del 2026-09-30, sorgente: progetto Claude Design "AI Task Manager - Direzione visiva"): accento indaco (hue 268) su neutri freddi, font di sistema (SF Pro, SF Mono), raggi 4–12 px, ombre solo sugli elementi sollevati. Una **palette di stato** (`--status-todo\|running\|waiting\|review\|done\|failed\|cancelled`) è condivisa da colonne, badge dell'agente, barre e pallini; il giallo (`waiting`) vuol dire solo "attende una tua risposta". Le classi stanno in `widgets/status.rs` (`Status`, `StatusDot`, `AgentBadge`, `agent_state`, `pill_tab`, `FOCUS_RING`). I componenti vendored non cambiano: si restilizzano con `class` (tw_merge) e con i token.

| Schermata | Componenti Rust/UI | Scritto a mano |
|---|---|---|
| Onboarding (§7.10) | card, callout, alert, button, spinner, select_native, input, label, kbd, dialog* (attesa del login) | polling, logica del gate |
| Sidebar e topbar | button, scroll_area, badge, callout, tooltip, kbd | lista progetti, "Aggiungi repository"; nel piede della sidebar il misuratore "In esecuzione x/y" (`EnvStatus.running`/`max_running`, un segmento per slot, `role=meter`), "Impostazioni app" (anche con ⌘,, non sopra un altro dialog) e la versione di Claude Code ("· non testata" se più recente di quella testata); nella topbar nome e path, la pillola "N in attesa di approvazione" (somma delle `pending_approvals` delle card; apre il primo task in attesa sulla pagina Task), i tab Riepilogo/Task/Impostazioni (il tab Task mostra il numero di task, "Task (N)" per i lettori di schermo) e il chip account (solo il piano, mai email né organizzazione); banner di pausa con "Riprendi". Topbar e Riepilogo leggono le card con lo stesso `project_cards` (un `get_board` per `board_version` ciascuno; solo il Riepilogo mostra l'errore in un toast); **menu del progetto** (§9.1): bottone "⋯" accanto al nome (visibile su hover e focus, `aria-haspopup=menu`, `aria-expanded`, `aria-label` "Azioni per «nome»") e clic destro sulla voce (`contextmenu` con `preventDefault`), stesso popover con "Impostazioni progetto" e "Rimuovi dalla lista…" |
| Menu progetto e `RemoveProjectDialog` | dialog*, callout, button, spinner | **un'istanza sola di ciascuno a livello di Sidebar**, con il bersaglio (id e nome del progetto) in un signal, per il dialog `AppCtx::remove_target: Option<(Id, String)>`, letto al click, mai catturato (Leptos 0.8 ricostruisce sul posto e terrebbe il vecchio handler: si rimuoverebbe il progetto sbagliato). Il dialog chiede "Rimuovere «nome» dalla lista?", dice "I file del repository e i branch atm/… restano; i worktree dell'app vengono salvati con un commit sul loro branch e rimossi; task, cronologia e allegati vengono eliminati dall'app." e rimuove sempre il **bersaglio**, non il progetto selezionato. Errore (per esempio `Busy`, "Ferma gli agenti del progetto prima di rimuoverlo"): il dialog resta aperto con una callout (un toast se nel frattempo è stato chiuso o riaperto per un altro progetto). Ok: toast "«nome» rimosso dalla lista; i branch restano", `open_task` azzerato se il bersaglio era selezionato, il dialog chiuso solo se mostra ancora quel bersaglio (non se nel frattempo è stato riaperto per un altro progetto), poi `refresh_projects` seleziona il primo progetto rimasto sul Riepilogo (o `NoProject`). "Rimuovi" è disabilitato, con lo spinner, solo mentre è in corso la rimozione del bersaglio mostrato. Il menu si chiude e rende il focus al trigger prima che il dialog si apra. Lo apre anche il bottone "Rimuovi dalla lista…" della pagina Impostazioni |
| **Riepilogo** (pagina di atterraggio, `views/overview.rs`) | collapsible, callout, skeleton, spinner | tra hero e bento la **card del pianificatore** («Pianifica con un agente», `views/planner.rs`, §7.13; non dipende dal fetch del riepilogo). **hero**: chip del branch "main @ 3f2a9c1" e della configurazione (Isolata/Attendibile), nome, descrizione (se vuota, un riquadro tratteggiato con "Scrivi descrizione", che porta alla pagina Impostazioni), "Aggiorna" e la CTA **"Apri task (N)"** (N = tutte le card di `get_board`, in un chip; "Apri task (N)" per i lettori di schermo) che passa al tab Task, o **"Crea il primo task"** se il progetto non ne ha; poi la barra dei task per colonna, i conteggi per colonna (rifatti a ogni `board_version`) e gli agenti attivi "n qui · x/y nell'app" (senza task, solo la barra vuota e "Nessun task ancora"); se un agente attende un'approvazione, una riga `role=status` "«titolo» attende la tua approvazione" (più "· altri N task in attesa") con **"Rivedi"**, che apre quel task. Sotto, una griglia **bento a 12 colonne**: **Istruzioni agenti** (CLAUDE.md, .claude/CLAUDE.md, AGENTS.md; 7 colonne su due righe), **Configurazione agenti** (modello predefinito "<m> (predefinito dell'app)" o "Predefinito di Claude Code", da un `get_settings` per montaggio; modalità; configurazione con la sua nota), **Server MCP** (una card per server: nome, trasporto, target, chip con i **soli nomi** delle chiavi di env e header), con la nota che i server aggiunti con `claude mcp add` stanno in `~/.claude.json`, che l'app non legge; **README**; **.claude/** (settings.json già mascherato dal backend, liste di agenti, comandi e skill). Testo `whitespace-pre-wrap` monospace come text node dentro collapsible (sempre nel DOM): il trigger ha `aria-expanded` e `aria-controls` sul testo, che da chiuso è `inert` (fuori dall'albero di accessibilità e dal focus); da aperto mostra le prime righe e "Mostra tutto"/"Mostra meno" (`aria-expanded`) apre il resto; niente markdown. Badge neutro "caricato dagli agenti"/"non caricato" e badge per i file con caratteri nascosti o bidi (`⟨U+XXXX⟩`). Il **testo di `.mcp.json` non si mostra mai**: solo intestazione, note e card dei server. Fetch al montaggio e al cambio di progetto con guardia di generazione, più "Aggiorna". Un errore del backend è una callout |
| **Impostazioni progetto** (pagina, `views/settings/project.rs`) | input, textarea, label, select_native, callout, button | montata da capo a ogni visita del tab (niente più prop `open`); l'Effect che la riempie traccia solo `project` (la voce di menu può cambiare progetto con la pagina aperta; le modifiche non salvate si perdono) e scarta le risposte arrivate dopo un cambio di progetto. Campi: nome, **Descrizione** (textarea, contatore "n/10000" sul testo senza spazi in testa e in coda, "Salva il progetto" disabilitato oltre `MAX_PROJECT_DESCRIPTION`), branch target, modello e modalità predefiniti; sicurezza (policy, bypass, "Applica", come in M6); "Rimuovi dalla lista…" apre il `RemoveProjectDialog` condiviso (prima era un bottone a due click) |
| Board (5 colonne; Annullati compressa) | scroll_area, empty, skeleton, button, input | card e badge scritti a mano (`widgets/status.rs`): un badge colorato con pallino per lo stato dell'agente, "Attende approvazione" (sostituisce "In esecuzione", senza conteggio), "In esecuzione" (pallino pulsante, nascosto con reduced motion), "Interrotto" (neutro, fermo) più il link "Continua", "Fallito", "Fermato", "Pronto al merge", "Mergiato"/"Scartato"; "Worktree mancante" a parte. **DnD** (§9.3), creazione rapida in fondo alla colonna; **sotto task** (round 2026-09-30): la card del padre mostra il chip "↳ n/m" (sotto task fatti su totali) con una barra sottile, la card del figlio una riga mono e muted "↳ titolo del padre"; i sotto task stanno nelle colonne come gli altri task e i contatori per colonna li includono; **toolbar Kanban \| Lista** (toggle con `aria-pressed`, non persistito, Kanban a ogni montaggio; le due viste dietro `Show` su un memo, una sola nel DOM) con "Nuovo task" a destra in entrambe le viste |
| **Lista** (`views/board/list.rs`) | skeleton, empty | `<table>` semantica sulle stesse card e sullo stesso `get_board` della kanban, colonne a larghezza fissa: **Titolo** (un `<button>` troncato che apre il task, `title` = titolo intero; `aria-current` sul task aperto) più una matita "Modifica il task", **Stato** (pallino e titolo della colonna), **Agente** (gli stessi badge della card, o "—"), **Branch**, **Aggiornato** ("oggi, 14:05", "ieri, 09:12", "12 set", "12 set 2025"; data completa nel `title`). Ordine fisso: colonna, poi posizione; i sotto task (round 2026-09-30) stanno subito sotto il padre, rientrati e con "↳", in ordine di colonna e posizione. Il padre ha dopo la matita un toggle "↳ n/m" con chevron che comprime o espande i suoi figli (`aria-expanded`, `aria-label` "Nascondi i sotto task (n su m fatti)"/"Mostra …"; non persistito, tutto espanso a ogni montaggio). Con il pannello aperto Branch e Aggiornato si nascondono. "Nuovo task" della toolbar apre il dialog in Da fare. Niente ricerca, filtro o ordinamento (filtrare la kanban romperebbe l'indice del DnD, `widgets/dnd.rs`); nessun cambio di stato in riga (si usa "Sposta in" del pannello) |
| Dialog task (crea/modifica) | dialog*, input, textarea, label, button | form; sezione **Allegati**: "Aggiungi file…" (`type=button`, picker nativo multiplo, token) e righe con "×". In **creazione** i file scelti restano in attesa (solo nome e dimensione): "Crea task" fa `create_task`, poi `add_task_attachments` con i token, e chiude solo dopo entrambi; se l'aggiunta fallisce il task resta e compare il toast "Task creato, allegati non aggiunti: …". In **modifica** aggiunta e rimozione sono immediate, con `get_task_detail` all'apertura e una guardia contro le risposte vecchie. Un pick oltre i 20 allegati tiene i primi file (nell'ordine del picker) che ci stanno e dice quanti ne ha lasciati fuori in un avviso (`p[role=alert]`: "Al massimo 20 allegati per task: N file non aggiunti."); la dimensione la controlla il Core. Mentre "Crea task" o "Salva" sono in corso la sezione Allegati non cambia ("Aggiungi file…" e "×" disabilitati); una risposta arrivata dopo che il dialog è stato chiuso o riaperto non lo chiude e non ne cambia lo stato (solo il toast). Con un tentativo attivo: "La cartella allegati è visibile all'agente a ogni turno: cita i nuovi file nel messaggio." Round 2026-09-30: la creazione accetta un padre (`TaskDialogMode::Create { status, parent }`); allora il titolo è "Nuovo sotto task" e sotto l'intestazione c'è "↳ Sotto task di «titolo»". In modifica, il primo click su "Elimina" di un padre avvisa "Elimina anche N sotto task." (`role=alert`); a eliminazione riuscita il pannello si chiude se mostrava il task o uno dei suoi sotto task |
| Dialog "Impostazioni app" | dialog*, input, label, button, select_native | form delle impostazioni generali (§5.2), senza tab; round 2026-10-01: la casella «Notifiche macOS (autopilota: approvazioni, verifiche, merge)» (`Settings.notifications`) |
| **Aggiornamenti** (round 2026-10-02, §9.5, `views/update.rs`) | button, spinner | modal bloccante «Aggiornamento richiesto» (`[data-testid=update-required]`, layout sotto `inert`; con un errore anche «Pagina delle release» e «Continua per ora»), banner `[data-banner=update]` sotto la topbar per un aggiornamento facoltativo o rimandato, versione nel piede della sidebar (`[data-testid=app-version]`) e in «Impostazioni app» (`[data-testid=settings-app-version]`) |
| **Autopilota** (round 2026-10-01, §7.12) | button, input, label, checkbox nativo, callout | **topbar**: chip-interruttore «Autopilota» del progetto selezionato (`aria-pressed`; acceso, un puntino indaco pulsante al posto dell'icona robot), salva subito con `update_project`. **Impostazioni progetto**, sezione «Autopilota» («Avvia da soli i task affidati, li verifica, rimanda gli errori all'agente e, se vuoi, fa il merge.»): «Autopilota attivo», «Comando di verifica» (input mono, con l'avviso «Gira con /bin/sh nel worktree del task dopo ogni turno completato. Esegue codice del repository sul tuo Mac con i tuoi permessi. …»), «Timeout della verifica (secondi)», «Tentativi di correzione» (0–5), «Merge automatico se verificato», e «Salva l'autopilota» (toast «Autopilota aggiornato»). **Dialog task e pannello**: «Affida all'autopilota» e il select «Parte dopo…» («Nessun task», poi i task del progetto tranne il task stesso e gli annullati); nel pannello si salvano subito. **Card e Lista**: icona robot sui task con `auto` («Affidato all'autopilota»), badge «In coda» (`auto` o `launch` di un piano, Da fare, nessun attempt; tooltip «Parte dopo «X»», «Autopilota del progetto spento» o «Attende un agente libero», per `launch` preceduti da «Avviato dalla pianificazione» e con il modo di annullarlo: spostarlo fuori da Da fare), badge di verifica «Verifica…» (spinner), «Verificato», «Verifica fallita (n/max)», «Verifica non eseguita». **Pannello**: riga «Verifica: `cmd` · passata · correzioni n/max» dell'attempt attivo, con «Output della verifica» (la coda, `<details>`). Scostamenti dal piano: niente durata della verifica (non si registra), e `cmd` è il comando **attuale** del progetto (tooltip «Comando attuale del progetto: …»), non necessariamente quello verificato. Le card leggono autopilota e tentativi del progetto senza tracciarli (§13.7) |
| Pannello task: header | button (Avvia, Merge, Continua, Stop, Scarta, Apri in Finder/Terminale/Editor), tooltip, select_native ("Sposta in…") | tre righe: contesto (pallino e titolo della colonna, "creato <data>" nel formato della Lista) con le azioni e la chiusura; il titolo; badge dell'agente, chip del branch e chip degli allegati (nome e dimensione, tooltip con il path assoluto). Un'azione primaria per stato: Avvia senza attempt, Continua se interrotto, altrimenti **Merge** in revisione con l'agente fermo, che apre il tab Modifiche (dove si fa il merge) e ne porta il focus sul tab. Sotto, un `<dl>` dell'attempt attivo con le coppie **Modello**, **Sub-agent** ("sonnet, max 3 (usati 1)", "nessuno" con max 0; le due omesse se l'attempt non le imposta), **Modalità** e **Da** ("main @ 3f2a9c1"), con un ": " sr-only dopo ogni etichetta; presente per ogni attempt attivo. Un pallino giallo sul tab Agente (con `aria-describedby` "In attesa di approvazione") segnala un'approvazione in attesa. Round 2026-09-30: nel pannello di un sotto task, sopra il titolo, il link "↳ titolo del padre" apre il padre; nel pannello di un task di primo livello, sotto l'header, la sezione **Sotto task** (`section[aria-label="Sotto task"]`): intestazione con "↳ n/m" e il bottone "Aggiungi sotto task" (apre il `TaskDialog` della board in creazione con questo task come padre), poi una riga per figlio con il pallino della colonna, il titolo (un bottone che apre il figlio; il nome della colonna è nel testo sr-only e nel `title`), barrato se annullato, e il badge dell'agente |
| Dialog Avvia | dialog*, select_native (branch target; modello: "Predefinito (x)" con x il default del progetto, poi quello delle Impostazioni app, altrimenti "CLI", e `MODEL_ALIASES` (opus/sonnet/haiku/fable); effort; **Sub-agent (max)**: "Predefinito del CLI" (nessun limite), "Nessun sub-agent" (0), 1–10; **Modello dei sub-agent**: "Predefinito del CLI" o un alias, nascosto con max 0 (e allora non inviato); modalità: Supervisionato/Auto-edit/Autonomo, quest'ultimo abilitato solo con `allow_bypass`, con sotto la descrizione della modalità scelta: Auto-edit approva da solo modifiche e comandi shell sui file del worktree e chiede per il resto, Supervisionato chiede per tutto tranne le letture, M5), callout (avviso bypass: il worktree non è una sandbox), button | nota sui sub-agent: "Il massimo vale per tutto il tentativo. Il modello vale per i sub-agent che non ne chiedono uno proprio: Explore o una chiamata con un modello esplicito possono usarne un altro."; con "Predefinito" il modello inviato è `None` e il Core risolve lo stesso default (§6.3) |
| Tab **Agente** | marker (notice e righe tool), collapsible (output tool, thinking), badge (esito TurnEnd, "Attende approvazione", "Negato", costo "≈ stima API", durata), alert (errori, limiti, auth, resume fallito), button (approvazioni, Continua, Nuova sessione), input (messaggio dell'approvazione), textarea e button (composer, disabilitato durante il turno), kbd (⌘↩), empty, skeleton, spinner | **lista del transcript** (§9.4, messaggi e righe scritti a mano), **card di approvazione** (tool, input completo, motivo; "Approva", "Approva sempre" (per il tentativo) solo se `can_remember`, "Rifiuta", "Rifiuta e ferma", campo messaggio). Per i tool `mcp__atm__*` (round 2026-09-30, `approval::board_tool_label`) la card mostra un'etichetta leggibile, per esempio «Sposta «X» in Fatto», «Modifica «X»», «Avvia l'agente su «X»» (il titolo dalle card della board, altrimenti l'id), con l'input JSON in un `<details>` "Input"; niente "Approva sempre", perché le richieste dei tool board non hanno suggestion (§7.8), annidamento dei subagent tramite `parent_tool_use_id`, riga di anteprima digitazione |
| Tab **Modifiche** (`DiffView{attempt_id, task}`) | collapsible (un file per blocco), badge (A/M/D/R, +/−), button (Aggiorna, Merge, Risolvi con l'agente, Elimina branch), alert (conflitti, target avanti di N commit, checkout del target sporco, HEAD non corretto), skeleton, empty | **viewer diff** (righe con numeri e colori per `LineKind`, "mostra tutto" oltre 2000 righe, segnaposto per binari, file troppo grandi e omessi); dialog* di merge con messaggio modificabile; dialog* di conferma per lo scarto |
| Toast | — | scritto a mano (icona del tipo in un cerchio, testo, "Chiudi"); `Toaster` (Vec in un signal, al massimo 4, rimossi dopo 5 s) |

`*` = componente portato.

**Contratto selettori DOM** (round feature del 2026-09-29, fissato in Fase A): l'E2E (`ui/src/e2e.rs`) li usa, le viste li rispettano.

| Elemento | Selettore |
|---|---|
| Tab progetto | `[data-project-view=overview\|tasks\|settings]` |
| Radici delle pagine | `[data-view=overview]`, `[data-view=task-list]`, `[data-testid=project-settings]` |
| Voce progetto in sidebar | `[data-testid=projects] [data-project=<nome>]` |
| Menu progetto | trigger `[data-action=project-menu]`, contenuto `[data-name=ContextMenuContent]`, voci `[data-action=menu-settings\|menu-remove]` |
| Dialog di rimozione | `data_name_prefix="RemoveProjectDialog"`, conferma `[data-action=confirm-remove]` |
| Overview | `[data-overview-file="CLAUDE.md"]`, `[data-mcp-server=<nome>]` |
| Vista task | toggle `[data-task-view=kanban\|list]`, righe `[data-row-task-id]` |
| Allegati | `[data-action=add-attachment]`, `[data-attachment]` |
| StartDialog | `#start-max-subagents`, `#start-subagent-model` |

Round 2026-09-30 (sotto task e tool board), stessa regola:

| Elemento | Selettore |
|---|---|
| Card e righe | chip del padre `[data-subtask-progress="<fatti>/<totali>"]` (card, riga della Lista, sezione del pannello); toggle della Lista `button[data-action=toggle-subtasks][aria-expanded]` |
| Pannello task | sezione `[data-subtasks]` (solo su un task di primo livello), righe `li[data-subtask-id=<id>]` (primo `button` = apre il figlio), `[data-action=add-subtask]`; nel pannello di un figlio `button[data-parent-link=<id del padre>]` |
| Dialog task | `[data-parent-title]` ("↳ Sotto task di «…»", solo in creazione di un sotto task), `[data-delete-warning]` ("Elimina anche N sotto task.") |
| Card di approvazione | `[data-board-tool]` (l'etichetta leggibile di un tool `mcp__atm__*`) |

Hook aggiunti dai pacchetti UI oltre al contratto (stessa regola: l'E2E li usa, le viste li rispettano):

| Elemento | Selettore |
|---|---|
| Tab e pagine | tab `button[role=tab][data-project-view=<v>]#project-tab-<v>` dentro `[data-view=topbar] [role=tablist]`; pagine `main > #project-page-<v>[role=tabpanel]` (`main` senza ruolo esplicito); la pagina Task contiene `section[data-view=board]` e, con un task aperto, `aside > [data-view=task-panel]` |
| Sidebar | misuratore `[data-testid=running]` (con dentro `[role=meter]`); ogni voce è `li.group` con `button[data-project=<nome>]` (`aria-current="true"` se selezionata, etichetta nel primo `<span>`) e il trigger `button[data-action=project-menu]` (opacità 0 fuori da hover e focus, ma cliccabile); il `contextmenu` si ascolta sul `<li>`. Il bottone del piede dice "Impostazioni app" e apre il dialog con `data_name_prefix="Settings"`, che contiene `[data-testid=app-settings]` |
| Menu progetto | `[role=menu][data-name=ContextMenuContent][aria-label="Azioni per «nome»"]`, montato sotto il `<nav>` della sidebar solo quando è aperto; voci `button[role=menuitem][data-action=menu-settings\|menu-remove]` |
| Rimozione | `[data-name=RemoveProjectDialogContent]`, errore `[data-remove-error]`; nella pagina Impostazioni `[data-testid=project-settings] [data-action=remove-project]` |
| Topbar | `[data-action=open-waiting]` ("N in attesa di approvazione", solo con approvazioni in attesa) |
| Riepilogo | `[data-action=refresh-overview]`, `[data-action=open-tasks]` ("Apri task (N)", "Crea il primo task" senza task), `[data-testid=project-description]` o `[data-action=edit-description]`, `[data-testid=overview-counts] [data-count=todo\|inprogress\|inreview\|done\|cancelled]`, `[data-testid=overview-running\|overview-branch\|overview-config]`, `[data-hidden-chars]`, `[data-mcp-server=<nome>]` (una card per server), `[data-claude-dir=agents\|commands\|skills]`, `[data-overview-error]` |
| Impostazioni progetto | `#project-name`, `#project-description`, `[data-testid=project-description-count]`, `#project-branch`, `#project-model`, `#project-mode`; sicurezza invariata (`#project-policy`, `#project-bypass`, `[data-action=apply-security]`, `[data-trust=…]`) |
| Board e Lista | `[data-testid=board-toolbar]`, `button[data-task-view=kanban\|list][aria-pressed]`; `[data-testid=columns]` solo in Kanban, `[data-view=task-list]` solo in Lista (un check sulla Lista torna a Kanban prima di usare `[data-column]`); righe `tr[data-row-task-id=<id>]` (titolo = primo `button` della riga, matita `button[aria-label="Modifica il task"]`), `[data-action=new-task]`; nessun `data-task-id` nella Lista |
| Dialog task | `[data-name=TaskDialogContent] [data-testid=attachments]`, righe `li[data-attachment=<nome>]` con `button[data-action=remove-attachment]`, avviso del limite `p[role=alert]`, `[data-testid=attachments-hint]` |
| Pannello task | chip `[data-view=task-panel] header li[data-attachment=<nome>]` (`title` = path), `[data-testid=attempt-meta]` (un `<dl>`), `[data-action=open-merge]` (apre il tab Modifiche) |
| StartDialog | `#start-model` (prima opzione `""` = "Predefinito (x)"), `#start-max-subagents` (valori `""`, `0`…`10`), `#start-subagent-model` (`""` e gli alias; assente dal DOM con max `0`), `[data-testid=subagent-help]` |

Round 2026-10-01 (Autopilota), stessa regola:

| Elemento | Selettore |
|---|---|
| Topbar | `button[data-action=toggle-autopilot][aria-pressed=true\|false]` (solo con un progetto selezionato) |
| Impostazioni progetto | `#project-autopilot`, `#project-verify-command`, `#project-verify-timeout`, `#project-max-fixes`, `#project-autopilot-merge` (caselle e input), `[data-action=save-autopilot]` |
| Dialog task | `#task-auto[data-auto-toggle]` (casella), `[data-after] select#task-after` (valore `""` = nessuna dipendenza, altrimenti l'id) |
| Pannello task | `[data-testid=task-autopilot]` con `#panel-auto[data-auto-toggle]` e `#panel-after`; riga della verifica `[data-view=task-panel] [data-verify=running\|passed\|failed\|error]` |
| Card e righe della Lista | dentro `[data-testid=badges]`: `[data-auto]` (icona robot), `[data-queued]` («In coda», `title` = il motivo), `[data-verify=running\|passed\|failed\|error]` (il badge di verifica) |
| Impostazioni app | `#settings-notifications` |

Round 2026-10-02 (Pianificatore, aggiornamenti), stessa regola:

| Elemento | Selettore |
|---|---|
| Card del pianificatore (Riepilogo) | `[data-planner]` con `[data-planner-state=none\|running\|awaiting\|started\|dismissed\|failed]`; `#planner-prompt`, `#planner-model`, `#planner-effort`, `[data-action=start-plan]`, `[data-testid=planner-help]`; in corso `[data-planner-transcript]` e `[data-action=stop-plan]`; `[data-testid=plan-question]` con `[data-action=plan-proceed]` e `[data-action=plan-dismiss]`; `[data-testid=plan-outcome]`, `[data-action=toggle-plan-transcript]`; `[data-testid=planned-tasks]` con `[data-planned-task=<id>]` (apre il pannello del task) |
| Aggiornamenti | modale `[data-testid=update-required]` con `[data-action=install-update]` e `[data-testid=update-error]`; banner `[data-banner=update]` con `[data-action=dismiss-update]`; `[data-testid=settings-app-version]` |

**Aggiornamento del tab Modifiche:**
- all'apertura del tab;
- a ogni `changed` del task mentre il tab è visibile (copre la fine del turno);
- con il bottone Aggiorna.

**Tema:** all'avvio si legge `matchMedia('(prefers-color-scheme: dark)')` e si imposta la classe `dark` su `<html>`. Il toggle manuale è rimandato.

### 9.3 Drag-and-drop della kanban (`widgets/dnd.rs`)

- Stato: `DragCtx { dragging: RwSignal<Option<Id>>, drop: RwSignal<Option<(TaskStatus, usize)>> }`.
- Card con `draggable="true"`. `dragstart` → `DataTransfer.set_data("text/plain", id)`, `effectAllowed="move"`.
- **`dragover` sulla colonna:** `prevent_default()`. L'indice di inserimento si calcola da `client_y` rispetto ai punti medi dei figli `[data-task-id]` (`get_bounding_client_rect`). Il signal si aggiorna solo se cambia; un segnaposto tratteggiato della misura di una card (`DropPlaceholder`) mostra la posizione.
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

### 9.5 Versione e aggiornamenti (round 2026-10-02; `src-tauri/src/updater.rs`, `ui/src/views/update.rs`)

- **Versione:** una sola, `[workspace.package] version` di `Cargo.toml` (`tauri.conf.json` non ne ha: vale quella del crate). La UI la chiede con `app_info` e la mostra come «AI Task Manager v0.1.0» in fondo alla sidebar e in «Impostazioni app» (con «disponibile la vX» se c'è un aggiornamento).
- **Canale:** le release GitHub del repository pubblico `Marco-O94/ai-task-manager`. `tauri-plugin-updater` `=2.13.1` (senza `zip`: solo macOS per ora, Windows in un round successivo) legge `https://github.com/Marco-O94/ai-task-manager/releases/latest/download/latest.json` e scarica il `.app.tar.gz` della piattaforma; la firma minisign (`.sig`) è verificata contro `plugins.updater.pubkey` di `tauri.conf.json` prima di installare. La chiave privata sta fuori dal repository (`~/.tauri/ai-task-manager.key`, generata con `cargo tauri signer generate`, senza password) e nel secret `TAURI_SIGNING_PRIVATE_KEY` del repository GitHub.
- **Controllo:** nel guscio, all'avvio (dopo la recovery del core) e poi ogni 6 h; **mai** nelle build di debug (selftest, E2E, `cargo tauri dev`) né con `ATM_NO_UPDATE_CHECK` impostata. Il plugin confronta le versioni (solo una più nuova conta); un errore (offline, nessuna release, firma o JSON non validi) è una riga sullo stderr e l'app va avanti. Il risultato resta nello stato del guscio (`updater::Pending`) per `check_update` e parte l'evento `update_available` (payload `Option<UpdateInfo>`: `None` quando un controllo successivo non trova più l'aggiornamento annunciato, per esempio una release ritirata, e la UI toglie banner e modal). La webview non ha nessun permesso del plugin (le capability restano `core:default`, un grep del §10.3 lo tiene così): tutto passa dai tre comandi del §6.3.
- **Gravità** (`updater::is_required`, pura, con test): **richiesto** se la major remota è maggiore, oppure se entrambe le major sono 0 e la minor remota è maggiore (convenzione semver 0.x: `0.1.x → 0.2.0` rompe); **facoltativo** ogni altro aggiornamento più nuovo. Una versione non leggibile non è mai richiesta.
- **UI:** richiesto → modal bloccante «Aggiornamento richiesto» sopra tutto (anche l'onboarding), senza chiusura: solo «Aggiorna e riavvia», con «Download e installazione…» mentre gira e l'errore con «Riprova» se fallisce. Con un errore (di questo tentativo o, via `UpdateInfo::install_error`, di quello prima del riavvio) offre anche «Pagina delle release» (`open_url` su `RELEASES_URL`, per installare a mano) e «Continua per ora» (il modal lascia il posto al banner fino al prossimo avvio). Mentre il modal è su, il layout sotto è `inert` (niente focus né Tab dietro lo sfondo). Facoltativo → banner sotto la topbar «Disponibile la vX, aggiorna» con «Aggiorna e riavvia» e la chiusura per versione (in memoria: torna al prossimo avvio o per una versione più nuova). Mock: `?update=optional`, `?update=required`, `?update=required,fail` (il primo install fallisce), `?update=required,installfail` (l'app riavviata dopo un'installazione fallita).
- **Installazione** (`install_update`): prima il download e la verifica (un errore lascia l'app com'è), poi `shutdown_core` (lo stesso shutdown ordinato dell'uscita: turni `killed/app_shutdown`, riprendibili con «Continua»), poi l'installazione sopra il bundle e `request_restart`, che passa da `RunEvent::ExitRequested` (lo shutdown, già fatto, non si ripete). Prima di fermare gli agenti `installable()` rifiuta un bundle che macOS esegue da una posizione temporanea (App Translocation) o da un volume di sola lettura (`EROFS`, un'immagine disco): l'errore torna al modal e gli agenti restano. Una cartella non scrivibile va bene (il plugin chiede la password di amministratore). Dopo lo stop degli agenti non si torna indietro: un'installazione fallita scrive il perché in `update-install-error.txt` nella cartella dati e riavvia la versione vecchia, che lo rilegge (e cancella) all'avvio e lo mostra nel modal.
- **Release:** `scripts/bump-version.sh X.Y.Z` (versione del workspace, `Cargo.lock`, commit «Release vX.Y.Z», tag `vX.Y.Z`), poi `git push origin main vX.Y.Z`. Il tag avvia `.github/workflows/release.yml`: `macos-latest`, `aarch64-apple-darwin` e `x86_64-apple-darwin` uno dopo l'altro (ognuno aggiunge la sua piattaforma a `latest.json`), toolchain da `rust-toolchain.toml`, Trunk `0.21.14` e tauri-cli `2.12.0` come nel Setup, `tauri-apps/tauri-action` con `includeUpdaterJson` in una release **draft**; il job `publish`, solo dopo entrambe le piattaforme, la pubblica come latest (`gh release edit --draft=false --latest`): un'app installata non vede mai un `latest.json` con una piattaforma sola. Ogni action è fissata a un commit (il tag in commento): la chiave di firma è nell'env del job, e un tag spostato non deve poterla leggere. Il job fallisce se il tag non è la versione del workspace. `bundle.createUpdaterArtifacts: true` vale per le build di release; l'overlay `tauri.testkit.conf.json` lo spegne (la build di debug dell'E2E non ha la chiave) e `scripts/release.sh` usa `~/.tauri/ai-task-manager.key` se c'è, altrimenti costruisce senza gli archivi dell'updater.

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
| Configurazione del repo eseguita sotto `-p` (hook, env, `apiKeyHelper`, MCP, regole `permissions.allow`) | Isolated di default (`--setting-sources=user --strict-mcp-config`). Trusted solo con **conferma nativa** (che nomina il repository per path, non per nome modificabile, il branch e il commit approvati e le regole che consentono un tool intero) e fingerprint della configurazione **committata sul tip del branch target** (da cui partono i worktree; i file locali del checkout principale non contano), esteso ai file del repo che la configurazione esegue, ricontrollato sul worktree a ogni turno e al suo `system/init`: se non corrisponde, il turno gira Isolated (o viene fermato) con una Notice che dice cosa è diverso (il worktree, o il commit di partenza da riapprovare). Ciò che si conferma è ciò che si salva (compare-and-set). Riapprovare un fingerprint scaduto chiede di nuovo la conferma. Mai Trusted un repo che è `$HOME`. Link seguiti solo dentro il repo, aperture relative al descrittore della radice con `O_NOFOLLOW` (§8.9). Warning quando si aggiunge il progetto. Non coperta l'esecuzione indiretta (§8.9). Accettazione di M6: `tests/flow.rs::malicious_repo_config_runs_only_when_trusted_and_unchanged`, `trusted_config_covers_the_script_an_mcp_server_runs`; del commit (2026-09-29): `trusted_approves_the_target_tip_not_the_working_tree`, `tests/git.rs::commit_config_is_the_config_of_a_checkout_of_it`, `commit_config_limits_and_links` |
| Codice del repo eseguito dal git dell'app | Runner del §8.1: hook, merge driver, filter driver e programmi `gpg` della configurazione del repository spenti; mai il fetch pigro di un partial clone (`GIT_NO_LAZY_FETCH=1` su ogni chiamata, git ≥ 2.44 o nessuna chiamata); l'env di git senza chiavi API né variabili di una sessione padre |
| Prompt injection che porta a comandi distruttivi | Auto-edit di default (M5 [V]: in `acceptEdits` il CLI 2.1.283 approva da solo i comandi Bash che leggono o scrivono file nel cwd, per esempio `printf … >> README.md`; gli altri chiedono. In Supervisionato `ls` passa da solo, `touch` e `python3 -c` chiedono; un `sleep N` isolato il CLI lo blocca e suggerisce `run_in_background`); deny rules via `--settings`; bypass solo con opt-in, conferma nativa e `--allow-dangerously-skip-permissions`; la UI dice chiaramente che il worktree **non è una sandbox** |
| XSS nella webview che abusa dell'IPC | CSP senza `unsafe-inline` negli script; solo text node; nessun `inner_html`; gli allegati passano solo per token del picker nativo (riga sotto); nav guard (plugin con `on_navigation`: consente solo `tauri://localhost`, `http://tauri.localhost` e in dev `http://localhost:1420`); `open_url` solo `http(s)`; **conferme native** (non cliccabili da un XSS) per bypass, Trusted, passthrough della chiave API e percorso di Claude Code, con il testo su una riga senza caratteri di controllo o di direzione; il nome del progetto non li può contenere |
| Fatturazione API silenziosa (requisito: agenti solo con l'abbonamento) | Chiavi rimosse dall'env (passthrough solo con `allow_env_api_key`, conferma nativa e banner); banner su `authMethod`, `apiProvider`, sui `CLAUDE_CODE_USE_*` e su `ANTHROPIC_BASE_URL` (e gli endpoint dei provider) dell'ambiente (`EnvStatus.base_url_env`, §7.2); warning su `apiKeySource` diverso da `none` o assente; **Trusted mai approvato** per una configurazione del repo con `apiKeyHelper`, `awsAuthRefresh`, `awsCredentialExport` o con `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_BEDROCK`/`_VERTEX`/`_FOUNDRY` in `env` (`git::BILLING_SETTINGS_KEYS`/`BILLING_ENV_VARS`, `Invalid` che nomina la chiave), anche in un file che il filesystem apre come uno dei due file di impostazioni sotto un altro nome (`.claude/Settings.json` su APFS: nel worktree conta, nel commit non è verificabile), e un worktree che le acquista gira Isolated, o viene ucciso al `system/init` se le acquista mentre il CLI parte (§8.9); **in ogni turno** un `system/init` con `apiKeySource` fuori da `NO_API_KEY_SOURCES` e il passthrough spento ferma subito il turno (`failed`, Notice; §7.6); `ATM_CLAUDE_PATH` vincolante (§7.1); `claude_path_override` solo con conferma nativa e fuori dai repository. Test: `tests/flow.rs::trusted_is_refused_when_the_config_bills_outside_the_subscription`, `a_worktree_config_that_bills_outside_the_subscription_runs_isolated`, `an_api_key_source_stops_the_turn_unless_the_passthrough_is_on`, `a_settings_file_under_another_case_is_checked_like_the_settings`, `a_billing_key_gained_during_the_cli_start_kills_the_turn_at_once`, `tests/claude.rs::api_key_and_cloud_provider_detection` |
| App lanciata da dentro una sessione Claude Code (o da un terminale cmux o IDE) | Le variabili della sessione padre (id, messaggi, effort, PID, …) e del suo host (`CMUX_*`, IDE, e il `NODE_OPTIONS` con cui cmux fa caricare il suo modulo a ogni programma node, riportato a quello dell'utente) non arrivano agli agenti né a git e lasciano anche il processo dell'app, che all'avvio si ri-esegue senza (§7.2); verificato da `tests/claude.rs`, `tests/flow.rs` e dall'E2E (anche con `ps -E` dell'app). Resta vero che i processi antenati ancora vivi (la shell o la sessione che ha lanciato l'app) hanno quelle variabili, leggibili con `ps -E` da ogni processo dello stesso utente: per non esporle agli agenti si lancia l'app da un terminale pulito o dal Finder |
| Allegati dei task: una webview compromessa che fa copiare all'host un file segreto (Portachiavi, `~/.ssh`, `~/.aws`, il DB e i log dell'app) in una cartella che l'agente legge senza chiedere (round feature del 2026-09-29) | **Il picker è l'autorità**: la webview non passa mai un path. `pick_attachment_files` apre il picker nativo nel guscio e consegna i path solo a `Core::stage_picks`; la webview riceve `PickedFile{token, name, size}` e `add_task_attachments` accetta solo token, **monouso**, validi **10 minuti**, al più **100** in memoria (i più vecchi scartati), al più 20 file per pick; un pick è tutto o niente. Difesa in profondità sul path, **canonicalizzato** (link risolti): rifiutato se sta sotto `~/.claude*` (ogni voce della home che inizia per `.claude`, in qualunque maiuscola, e il target di una tale voce se è un link), `$CLAUDE_CONFIG_DIR`, `~/.ssh`, `~/.aws`, `~/Library/Keychains`, la cartella dati o quella della cache dell'app (confronto ASCII senza maiuscole, come APFS; anche le forme canoniche delle cartelle); poi `symlink_metadata().is_file()`; poi apertura con `O_NOFOLLOW\|O_NONBLOCK` (un file sostituito da un link dopo lo staging dà `ELOOP`, una FIFO non blocca), `fstat` regolare e ≤ 25 MiB (`MAX_ATTACHMENT_BYTES`), copia con `take(MAX+1)`. Alla copia il file riaperto deve essere **lo stesso** dello staging (device e inode dell'`fstat`, salvati con il token): una cartella del percorso sostituita da un link, un hard link o un rename sopra il file danno "«nome» è cambiato dopo la scelta (sostituito o spostato): sceglilo di nuovo"; una modifica sul posto dello stesso file no. Il nome viene da `file_name()` senza caratteri nascosti o bidi; vuoto, `.`, `..` o con un apice inverso è rifiutato, oltre 120 caratteri è accorciato (estensione fino a 16 caratteri tenuta). Gli errori nominano solo il file, mai il path intero. Le copie stanno in `data_dir/attachments/…` (dir 0700, file 0600 `create_new`), fuori da ogni worktree e da git (`git add -A` non le vede); aggiunta e rimozione girano sotto il lock del task e una transazione riconta e inserisce, e a ogni errore (anche la FK di un task appena cancellato) le cartelle appena create si rimuovono. All'agente: `--add-dir` sulla cartella del task e i path nel prompt (§7.3, §7.4), nessuna regola deny: Bash può modificare le copie, mai gli originali (§13). Test: `tests/flow.rs::attachments_reach_the_agent_and_go_with_the_task`, `removing_the_project_removes_its_attachments_and_logs`, `picks_outside_the_rules_are_refused`, `tokens_are_single_use_and_the_copy_checks_again`, `the_attachment_limit_removes_the_copies_it_refuses`, `staged_picks_expire_and_the_oldest_go_first` |
| Riepilogo del progetto: il testo del repository mostrato all'utente (segreti, caratteri che ingannano, file fuori dal repo) | Letto dal **commit in cima al branch target predefinito** (`ls-tree` e `cat-file --batch` sul runner irrobustito del §8.1, sola lettura), mai dal checkout principale, mai da `$HOME` (repo che è la home: `Invalid`), mai da `~/.claude.json` (i server aggiunti con `claude mcp add` non compaiono). Nessun link seguito: un symlink mostra solo "→ target". Segreti mascherati (`•••`) a ogni profondità in `.claude/settings.json` e `.mcp.json`: i valori di `env` e `headers`, ogni chiave che finisce in `Helper` più `awsAuthRefresh`/`awsCredentialExport`, e dal valore di ogni chiave `url` (e dall'URL di un server MCP) userinfo, query e fragment, anche nelle forme che il parser WHATWG del CLI accetta senza `scheme://` (`https:/`, `https:///`, `https:\\`, spazi in testa e in coda, tab e a capo interni); un JSON non valido non si mostra mai grezzo ("JSON non valido"). I server MCP si riducono a nome, trasporto, target e **nomi** delle chiavi di env e header; la UI non rende mai il testo di `.mcp.json`. Caratteri nascosti e bidi resi visibili lato server come `⟨U+XXXX⟩` (con un badge), `\r\n` → `\n`; solo testo semplice, mai HTML né markdown. Limite: il mascheramento è euristico, un segreto dentro `command`/`args`, nel comando di un hook, nel path di un URL o in un URL sotto una chiave diversa da `url` (o dentro un altro testo) si vede. Test: `tests/git.rs::overview_masks_every_secret` (nessuna sottostringa "secret" nell'overview serializzato), `overview_strips_lax_urls_too`, `overview_shows_the_committed_context_in_order`, `overview_notes_what_it_does_not_show`, `core_overview_reads_the_target_tip_and_never_home` |
| Un agente che modifica, sposta o avvia task, anche di altri (tool board, round 2026-09-30) | Solo il progetto dell'attempt: un id di un altro progetto è "non trovato". Creare e leggere sono liberi; modificare, spostare e avviare passano da `ask` → `can_use_tool` → approvazione dell'utente, in ogni modalità, e "Consenti sempre" non salva regole (§7.8); l'host poi esegue solo il `tools/call` con il `tool_use_id` approvato, così un CLI che salta la domanda (un hook dell'utente, un cambio di precedenza) non cambia nulla (§7.4). Le modifiche usano i servizi della UI (validazioni, `Busy` su un task in esecuzione, lock). Gli avvii rispettano `max_running` (`ConcurrencyLimit` come errore del tool), usano la modalità dell'agente chiamante solo per un sotto task (per gli altri task quella predefinita del progetto) e si fermano a profondità 2 (`started_by_attempt`): un agente avviato da un agente non avvia agenti. Il server è in-process sulla stdio del CLI: nessun socket (D1). **Autopilota** (round 2026-10-01): due avvii non passano subito da `start_attempt`. (1) Uno `start_task` approvato che trova gli slot pieni in un progetto con l'autopilota mette il task in coda (`auto`, `auto_by` = l'attempt chiamante). (2) Un sotto task creato con `parent_id:"self"` (tool `allow`, senza domanda) da un task con `auto` eredita `auto` e parte da solo, senza approvazione: è la scelta dell'utente di affidare il padre all'autopilota. In entrambi i casi lo scheduler avvia come avrebbe avviato il chiamante: `started_by_attempt` = il chiamante, la sua modalità solo per un sotto task, i suoi limiti sub-agent (§7.12); e un chiamante avviato a sua volta da un agente non fa ereditare `auto`. La profondità resta 2. Accettazione: `tests/flow.rs::board_tools_*`, `board_start_*` (anche `board_start_without_a_slot_is_queued_in_an_autopilot_project`: lo avvia lo scheduler con `started_by_attempt`), `board_subtasks_inherit_auto_and_take_after`, `a_denied_board_tool_changes_nothing`, `an_unapproved_board_call_is_refused` |
| Comando di verifica dell'autopilota: codice del repository eseguito senza approvazione (round 2026-10-01) | Lo scrive l'utente nelle Impostazioni del progetto e si legge **solo dal DB** (`projects.verify_command`), mai da file del repository; la UI avvisa che «esegue codice del repository sul tuo Mac con i tuoi permessi». **Limite da sapere**: in Supervisionato e Auto-edit ogni comando Bash dell'agente che non tocca solo file chiede l'approvazione, mentre la verifica gira `/bin/sh -c <verify_command>` dopo ogni turno completato **senza chiedere**, e l'agente può aver modificato proprio i file che il comando esegue (test, `build.rs`, script di `package.json`, `Makefile`). È esecuzione di codice scritto dall'agente, come quella dei test che l'utente lancerebbe a mano: il worktree non è una sandbox. Controlli: `ChildEnv` ripulito come per gli agenti (niente chiavi API né variabili di una sessione padre), stdin null, process group proprio, timeout (`verify_timeout_secs`, 10–3600 s) con `killpg` più i discendenti, il gruppo ucciso anche dopo un'uscita normale, da discard ed eliminazione del task e da `shutdown`; log 0600 in dir 0700, al massimo 8 MiB. Un merge automatico squasha solo il commit verificato con il worktree pulito (§7.12). Test: `tests/flow.rs::autopilot_verification_timeout_kills_the_group`, `autopilot_shutdown_kills_the_verification` |
| Notifiche macOS (`osascript`, round 2026-10-01) | Un solo punto (`Inner::notify`), un solo script fisso: titolo e testo (che contengono titoli di task, scritti anche dagli agenti) passano come `argv` dopo `--` e lo script li legge con `on run argv`, mai interpolati nell'AppleScript; argv senza shell, `ChildEnv` ripulito con PATH `/usr/bin:/bin`, cwd `/`, process group proprio, `killpg` più l'albero dopo 10 s, output scartato; al massimo una ogni 30 s per task e titolo, spegnibili. Due grep del §10.3 lo tengono così |
| Aggiornamenti dell'app (round 2026-10-02) | Solo HTTPS verso GitHub (rustls), solo dal guscio e solo nelle build di release; ogni pacchetto è verificato con la firma minisign contro la chiave pubblica incorporata in `tauri.conf.json` prima dell'installazione (un `latest.json` o un archivio manomessi vengono rifiutati); la chiave privata non è nel repository; la webview non ha permessi dell'updater (§9.5). Non coperto: chi ha la chiave privata (il secret di GitHub) può pubblicare un aggiornamento che l'app installa |
| Attacchi di rete | Nessun socket TCP/UDP in ascolto. L'unico socket in ascolto è quello Unix del plugin single-instance (`/tmp/dev_aitaskmanager_desktop_si.sock`, release e debug senza selftest/E2E): riceve cwd e argv di un secondo avvio e porta avanti la finestra. `/tmp` è condiviso tra gli account: se a quel path c'è un socket di un altro utente, che altrimenti riceverebbe il lancio e lo farebbe uscire con 0, l'app spegne il controllo di istanza singola e lo dice sullo stderr (M6-REVIEW) |
| Fuga di segreti | Log 0600 in dir 0700; valori dell'env mai registrati; transcript solo locali; righe, log grezzi (`logs/<attempt>`) e allegati cancellati con il task o il progetto (§4) |
| Iniezione di comandi | Sempre argv; `--flag=value`; prompt solo via stdin come JSON; branch validati; target presi dalla lista dei ref; `-z` e `--` ovunque. Nel `.command` c'è solo il path di claude, con escape |
| Perdita di dati | Commit di snapshot prima di ogni rimozione; mai rimozione automatica di lavoro non mergiato; `update-ref` CAS; `ff-only`; branch tenuti |
| Processi orfani | Process group, stop sequence, `killpg` del gruppo residuo più i discendenti del leader registrati mentre viveva (i comandi Bash del CLI hanno gruppi propri, §7.4 passo 4), shutdown ordinato (anche le verifiche dell'autopilota, §7.12), recovery con kill verificato. Non coperti: i discendenti di un leader morto da solo senza `result` o insieme all'app (§7.9), e quelli di una verifica il cui genitore è già uscito (riassegnati a launchd, fuori dal gruppo) |

### 10.3 Grep di sicurezza (in `scripts/check.sh`; qualunque match fa fallire)

- **`ui/src`:** `<script`, `inner_html`, `set_inner_html`, `dangerousDisableAssetCspModification`.
- **`crates/*/src`, `src-tauri/src`** (i test possono asserire che mancano):
  - `"--bare"`;
  - `"--dangerously-skip-permissions"`.
- **`crates/`, `src-tauri/src`:**
  - `find-generic-password`, `SecKeychain`;
  - `TcpListener`, `UdpSocket`, `UnixListener`, `0.0.0.0` (il codice del progetto non apre socket in ascolto; quello del plugin single-instance è nelle sue dipendenze, §10.2);
  - `credentials.json` fuori dalla costante `DENY_RULES`;
  - `CLAUDE_CODE_OAUTH_TOKEN` fuori dal commento sul passthrough in `claude.rs`.
- **`crates/*/src`, `src-tauri/src`** (round 2026-10-01, notifiche):
  - `osascript` fuori da `crates/atm-core/src/lib.rs` (`Inner::notify`);
  - `display notification` in una riga diversa dallo script fisso `"display notification (item 2 of argv) with title (item 1 of argv)"` (nessun testo interpolato nell'AppleScript).
- **`tauri.conf.json`:** `"csp": null`, `"devtools": true`.
- **`src-tauri/capabilities`** (round 2026-10-02): `updater` (la webview non riceve permessi dell'updater, §9.5).

---

## 11. Milestone e delega

### 11.1 Regole comuni per ogni subagent

**Modello e struttura:**
- Ogni subagent è **Opus 5.5**: passare `model: "opus"` a ogni `Agent` e a ogni `agent()` di Workflow, e nelle `meta.phases`.
- Budget di agenti, sotto il tetto di 15 per run: `// MAX_AGENTS = 15; worst case = M0(1) + M1(1) + M2(5) + M3(2) + M4(1) + M5(1) + M6(1) = 12`.
- Le milestone sono **sequenziali**. Dentro M2 e M3 i pacchetti vanno in **parallelo**, ognuno in un **git worktree dedicato** (isolation worktree dell'Agent tool, oppure `git worktree add ../atm-wp-<id> -b wp/<id>`).
- L'orchestratore fa merge in `main` nell'ordine indicato e lancia `scripts/check.sh` dopo ogni merge.
- **Ownership esclusiva dei file:** i conflitti sono impossibili per costruzione.
  - `Cargo.toml` e `Cargo.lock` sono **congelati dopo M1**: tutte le dipendenze sono dichiarate lì (check.sh usa `--locked`).
  - Se ne serve una nuova, il pacchetto la segnala e la aggiunge l'orchestratore.
  - Un file nuovo si aggiunge come sottomodulo di un file posseduto (es. `mod parse;` in `git.rs` → `src/git/parse.rs`), perché le liste dei moduli (`lib.rs`, `main.rs`) sono congelate.
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
cargo clippy --locked --workspace --exclude atm-ui --all-targets -- -D warnings
cargo clippy --locked -p atm-ui --target wasm32-unknown-unknown -- -D warnings
cargo clippy --locked -p atm-ui --target wasm32-unknown-unknown --features mock -- -D warnings
cargo check --locked -p atm-types --target wasm32-unknown-unknown
cargo test --locked --workspace --exclude atm-ui
cargo test --locked -p atm-ui
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
| **M2-DB** | `atm-core/src/db.rs`, `migrations/**`, `tests/db.rs`, `tests/snapshots/db__*` | open, migrate, query (progetti, task, board join, attempt, process, entries upsert/tail/before, settings), posizioni, `mark_orphans` | migrazione su `:memory:`; cascade delle FK; CHECK rifiutati; indici parziali (secondo attempt attivo e secondo process `running` rifiutati); 500 spostamenti casuali (xorshift con seme fisso scritto nel test: nessuna dipendenza RNG) mantengono l'ordine stretto con rinumerazione; upsert idempotente con `rev`; paginazione |
| **M2-GIT** | `atm-core/src/git.rs`, `tests/git.rs`, `tests/common/**`, `tests/snapshots/git__*` | §8 tranne il fingerprint (M6), con l'env di test via `Git::with_env` (mai `std::env::set_var`): runner, validazione del repo, branch, add/remove/riconciliazione worktree, auto-commit, diff snapshot con parser dei hunk, branch status, squash merge, slug | Repo temporanei con una **gitconfig globale ostile** (hooksPath e fsmonitor che scrivono un marker): il marker non compare mai. 10 worktree concorrenti; path con spazi e unicode; `worktree prune` dell'utente non tocca i worktree bloccati; diff con modifica, rinomina, cancellazione, file non tracciato, binario, `too_large`, `omitted`; l'indice dell'agente resta invariato (mtime e hash); merge con target non in checkout (`update-ref`, fallimento CAS gestito), in checkout e pulito (`ff-only`), con sovrapposizione sporca (`TargetCheckoutDirty`, nessun byte cambiato), target divergente senza conflitti (merge ok), conflitto (nessun ref toccato), `NothingToMerge`; tutto con `LANG=it_IT.UTF-8` |
| **M2-CLAUDE** | `atm-core/src/{claude,wire,normalize}.rs`, `src/bin/fake-claude.rs`, `tests/{claude,normalize}.rs`, `tests/fixtures/**`, `tests/snapshots/{claude,normalize}__*` | §7.1–7.6, §7.8 (funzioni pure), §7.10 (probe, script di login), `killpg`; fake-claude del §12.1 | snapshot insta dell'argv (primo turno, resume, isolated/trusted, bypass abilitato, model+effort): contiene `--permission-mode=` e mai `--bare`; golden del normalizer per ogni riga della tabella del §7.6; parse di `auth status` (dentro e fuori); gate di versione; `approval_response` (riscrittura della destination, regole a tool intero escluse); riga da 20 MiB saltata con lo stream che prosegue; spawn contro fake-claude: l'env registrato non contiene `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `GIT_DIR` e `PWD` = cwd; il PATH della login shell viene estratto dai marker |
| **M2-UI-BOARD** | `ui/src/views/{onboarding,sidebar,board,task_dialog,settings}.rs`, `widgets/dnd.rs`, `state/board.rs`, `ipc/mock/board.rs` | Gate, sidebar, board con DnD, dialog task, impostazioni | clippy wasm con mock; `trunk serve --features mock`: creazione di un task, drag tra colonne e dentro una colonna (l'ordine persiste nel mock), tutti gli stati dell'onboarding; con browser automatizzato se disponibile (screenshot), altrimenti una checklist per l'orchestratore |
| **M2-UI-TASK** | `ui/src/views/{task_panel,start_dialog,transcript,approval,composer,diff,merge_dialog}.rs`, `state/transcript.rs`, `ipc/mock/attempt.rs`, `ipc/mock/fixtures/**` | §9.2 (Agente, Modifiche), §9.4. Nel suo worktree `ipc/mock/board.rs` è la baseline di M1 (loggato, un progetto, un task per colonna, sola lettura): con `trunk serve --features mock` l'URL `/?task=task-inreview` apre il pannello senza la board | Il mock riproduce le fixture `TranscriptMsg` (simple, approval, flood da 10k); consenti, consenti sempre, nega e nega-e-ferma aggiornano le entry; con `flood`, `document.querySelectorAll('[data-entry]').length ≤ 300`; "Carica precedenti" mantiene l'ancora; il diff mostra aggiunte, rimozioni, rinomine, binari, `too_large` e omessi; il dialog di merge mostra Conflicts e TargetCheckoutDirty |

#### M3: Orchestrazione e guscio Tauri (2 agenti in parallelo; merge CORE → TAURI)

| Pacchetto | Possiede | Scope | Accettazione |
|---|---|---|---|
| **M3-CORE** | `atm-core/src/{lib,runner,live}.rs`, `tests/flow.rs`, `tests/snapshots/flow__*` | Servizi (§6.3), §7.7–7.9, §6.5, recovery, shutdown, cap, pausa, cache env, apertura via `open` | `tests/flow.rs` con fake-claude (variabili `FAKE_CLAUDE_*` per Core via `CoreConfig.extra_env`, mai `std::env::set_var`), repo temporaneo e un sink che registra gli eventi. Sequenza ordinata: `start_attempt` → worktree presente → Snapshot → entry → approvazione pendente → `respond_approval(Allow{remember:true})` → `allow_rules` salvate e presenti nel `--settings` del turno successivo → result → auto-commit (`head_after`) → task in inreview → `get_diff` mostra `hello.txt` → il follow-up usa `--resume=` con lo stesso id → merge → task done e worktree rimosso. Casi `noinit` (nuovo `--session-id` al turno dopo), `hang` con interrupt rispettato (killed in ≤ 5 s), `hang_ignore` (SIGKILL in ≤ 13 s, `kill -0 pgid` fallisce, il nipote `sleep` è morto), `crash` (failed/crash con stderr catturato), `control` (risposta di errore registrata da fake-claude), `usage_limit` (pausa, poi `UsageLimited`), `auth_fail` (env invalidato), cap 2 (il terzo avvio dà `ConcurrencyLimit`), `auth status` con exit 1 (`NotLoggedIn` senza spawn), drop del runtime a metà turno e nuovo Core (`failed/app_restart`, gruppo verificato ucciso, task in inreview), ordine dello snapshot con upsert concorrenti, Lagged (Snapshot inviato di nuovo) |
| **M3-TAURI** | `src-tauri/**`, `ui/src/selftest.rs` | Corpi sottili dei comandi; sink `app.emit` e `Channel`; `on_page_load(Started)` → `drop_subscriptions`; nav guard; single-instance; `pick_repo_folder` in `spawn_blocking`; helper `confirm_native(title, msg) -> bool` (dialog plugin, `blocking_show` fuori dal main thread); `ExitRequested` (§7.9); creazione delle dir dati con permessi; selftest esteso con subscribe, unsubscribe e reload (`debug_forwarder_count`; un attempt sconosciuto dà uno Snapshot vuoto, quindi non serve una fixture) | Nel proprio worktree (Core ancora stub): clippy, `cargo tauri build --debug --no-bundle`, selftest di M0 con exit 0. Dopo il rebase sul merge di M3-CORE (ordine CORE → TAURI): selftest esteso con exit 0; manuale: `cargo tauri dev` arriva all'onboarding con dati veri |

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
- Probe di debug solo sotto `cfg(debug_assertions)`. **Fatto (M6-PKG):** anche i driver della UI (`ui/src/selftest.rs`, `ui/src/e2e.rs`) sono fuori dalla release: esistono solo con la feature `testkit` di `atm-ui`, che `cargo tauri dev` passa sempre (`trunk serve --features testkit`) e le build di debug che si guidano da sole aggiungono con `--config src-tauri/tauri.testkit.conf.json` (`trunk build --release --features testkit`); `cargo tauri build` usa il `beforeBuildCommand` senza. `scripts/check.sh` fa clippy su release, `testkit` e `mock` e fallisce se `tauri.conf.json` o `ui/src/main.rs` li rimettono nella release; `scripts/release.sh` controlla con `strings` il WASM e il binario di release (1,73 MB contro 2,02 MB del WASM `testkit`, che i nomi li contiene).
- **Fatto (M6-PKG):** icona kanban (`src-tauri/icons/icon.svg`, PNG 1024 con `scripts/render-icon.swift`, poi `cargo tauri icon`); `scripts/release.sh` → `AI Task Manager.app` (9,9 MiB) e `AI Task Manager_0.1.0_aarch64.dmg` (5,5 MiB), non firmati (Gatekeeper nel README), con `CI=true` perché l'AppleScript del bundler che impagina il .dmg nel Finder va in timeout (-1712) senza il permesso Automazione; la riga `get_env` su stderr (percorso e versione di claude e di git, login senza email) c'è anche in release e Impostazioni → Generali (dal round feature del 2026-09-29 il dialog "Impostazioni app") dice "In uso: Claude Code … ; git …".

- **Fatto (2026-09-29, tre scelte dell'utente: gli agenti solo con l'abbonamento):** (1) Trusted mai approvato per una configurazione del repo che fattura fuori dall'abbonamento, un worktree che la acquista gira Isolated, e ogni turno il cui `system/init` riporta una chiave API viene fermato (§7.6, §8.9); (2) approvato il commit di punta del branch target, letto con `git cat-file` dal runner irrobustito, non il checkout principale: i file locali non bloccano più Trusted, un branch andato avanti chiede di riapprovare (§8.9); (3) `NODE_OPTIONS` di cmux tolto o riportato a quello dell'utente in ogni figlio e nella ri-esecuzione dell'app (§7.2). Dopo la revisione: file di impostazioni riconosciuti per ciò che il filesystem apre (APFS), link risolti senza `realpath`, visita del commit limitata e i suoi errori `Invalid` in cache (§8.9); `GIT_NO_LAZY_FETCH` su ogni chiamata git e git ≥ 2.44 (§8.1); `ANTHROPIC_BASE_URL` dell'ambiente mostrato nel banner (§7.2); al `system/init` il gruppo congelato durante il ricontrollo e ucciso se la configurazione è cambiata (§8.9). Test: `tests/flow.rs` (approvazione rifiutata, fallback per turno, stop su `apiKeySource`, approvazione del tip), `tests/git.rs` (commit = checkout, limiti e link del commit, chiavi di fatturazione), `tests/claude.rs` (regola di `NODE_OPTIONS`), `tests/normalize.rs` (`api_key_billing`), l'E2E (`NODE_OPTIONS` di cmux nell'ambiente dell'app, assente da `ps -E` dell'app e dagli agenti).
- **Fatto (M6-REVIEW, revisione di sicurezza):** fingerprint esteso ai file che la configurazione esegue, con directory contate e aperture relative al descrittore della radice (§8.9); approvazione e stato di sicurezza compare-and-set, riduzioni applicate anche se l'aumento viene annullato, revoche che fermano i turni che le usano, ricontrollo al `system/init` (§8.9); motivo di un fingerprint non calcolabile nella Notice e nelle Impostazioni; variabili dell'host (cmux, IDE) tolte e l'app che si ri-esegue senza le variabili della sessione padre (§7.2); probe con cwd `/` (§7.1); `claude_path_override` con conferma nativa e validato; filter driver e `gpg` del repository spenti nel git dell'app, che non riceve più chiavi API (§8.1); conferme con il path del repository su una riga e nomi di progetto senza caratteri nascosti; socket del single-instance dichiarato e ignorato se di un altro utente (§10.2); un `result` di errore arrivato prima dell'interrupt resta un errore col suo testo.

**Accettazione:**
1. Fixture di repo malevolo (hook e server `.mcp.json` che scrivono un marker, `apiKeyHelper`): in Isolated fake-claude registra i flag di isolamento; in Trusted, modificare `.claude/settings.json` nel worktree fa girare il turno successivo Isolated con Notice. **Fatto (M6-SEC):** `tests/flow.rs::malicious_repo_config_runs_only_when_trusted_and_unchanged`, con fake-claude che esegue la configurazione del repo come il CLI reale (`FAKE_CLAUDE_PROJECT_CONFIG=1`, §12.1): in Isolated nessun marker; in Trusted invariato i marker compaiono (controllo positivo) e `SessionInit` dice `mcp_servers: 1`; dopo la modifica nel worktree turno Isolated con Notice e nessun marker. **Rivisto il 2026-09-29:** l'`apiKeyHelper` della fixture non è più approvabile (`trusted_is_refused_when_the_config_bills_outside_the_subscription`: `Invalid`, e un'approvazione vecchia che lo conteneva gira Isolated senza eseguirlo), quindi il controllo positivo usa hook e server MCP; un commit nuovo della configurazione sul branch target toglie `trusted` al progetto e fa girare Isolated, con la Notice da riapprovare, l'attempt che parte dal tip nuovo; riapprovare rende Trusted i worktree che hanno quella configurazione.
2. Uscita con 2 agenti fake attivi: `pgrep -f fake-claude` vuoto. **Fatto (M6-PKG):** `scripts/e2e.sh`, fase 1: Cmd+Q con il `[fake:hang]` di T1 e il `[fake:hang_ignore]` di T5 in corso, poi `pgrep -f` della copia di fake-claude del giro vuoto (e nessun altro fake-claude sulla macchina); lo stesso dopo le fasi 2 e 3.
3. Il `.app` di release lanciato dal Finder trova `claude` e `git` (PATH). **Fatto (M6-PKG, 2026-09-28):** `open` con l'ambiente di launchd (`env -i` con `PATH=$(launchctl getenv PATH)` = `/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`, `HOME`, `USER`, `SHELL`, `TMPDIR`: `open` passa all'app l'ambiente di chi lo lancia, quindi senza `env -i` l'app avrebbe il `PATH` del Terminale), `ps eww` conferma quel `PATH` nell'app, e con il `claude` reale presente (solo `--version` e `auth status`, nessun turno) lo stderr dice `claude ~/.local/bin/claude version 2.1.284 supported=true auth loggedIn (authMethod claude.ai, subscription max) git /opt/homebrew/bin/git version 2.54.0`; via Accessibilità la UI mostra la board (niente gate) e Impostazioni "In uso: Claude Code 2.1.284 (…/.local/bin/claude); git 2.54.0.". Nessun processo figlio, DB dell'app invariato.
4. 0 violazioni CSP su tutte le schermate (selftest di release: build con flag `--debug` solo per la console, poi controllo manuale nella release). **Fatto (M6-PKG):** 0 violazioni sulle tre fasi dell'E2E (bundle di debug con asset incorporati e la stessa CSP, `eval` bloccato) e nel selftest; la release (senza contatore) si apre e disegna board e Impostazioni.
5. Dopo una reinstallazione DB, log e worktree sono ancora lì. **Fatto (M6-PKG):** `scripts/e2e.sh` installa il bundle di debug nella cartella del giro e tra le fasi 1 e 2 ci copia sopra un bundle nuovo: DB, 18 log e 2 worktree intatti, la fase 2 riparte sui dati. Con il `.app` di release (installato in una cartella temporanea, `HOME` sui dati di un giro `--perf`): reinstallato con `ditto` (inode nuovo), 1 progetto, 3 task, 3 attempt, 30 009 entry, 9 log (stesso hash) e 3 worktree prima e dopo, e la board mostra i tre task.
6. (Verifica prestazioni) **Fatto (M6-PKG):** fase 3 dell'E2E (`scripts/e2e.sh --perf` da sola): Agenti in parallelo = 3, tre `[fake:flood]` da 10 000 testi (pausa di 100 ms ogni 100, `FAKE_CLAUDE_FLOOD_PAUSE_MS`) avviati dalla UI, l'ultimo aperto: 3 turni in ~11 s con ~10 s di sovrapposizione, un timer da 20 ms mai più di 27 ms in ritardo, cambio tab Modifiche/Agente visibile al primo controllo (51–55 ms, polling a 50 ms), al massimo 300 righe nel DOM, turni `completed/success` con tutti i testi. I due tempi valgono solo a pagina visibile (una pagina nascosta, con lo schermo bloccato o la finestra coperta, ha i timer a 1 Hz): la finestra resta sopra le altre durante la fase, i campioni contano solo a pagina visibile e altrimenti il report dice `perf_responsiveness: "not measured (page hidden)"` (avviso nel giro completo, errore con `--perf`); la finestra dei bundle `testkit` ha `backgroundThrottling: disabled`, così una pagina nascosta non viene sospesa (prima il giro restava fermo fino al watchdog).

#### Round feature 2026-09-29: pagina progetto, allegati, vista lista, sub-agent

Stesse regole del §11.1 (Opus, al massimo 15 agent per workflow, un worktree per pacchetto su `wp/f-<pkg>`, merge `--no-ff` su `wp/f-int`, `check.sh` dopo ogni merge; i commit su `main` li fa l'utente).

| Fase | Pacchetto | Ownership |
|---|---|---|
| A | CONTRATTI | tipi (`atm-types`), migrazione 0002 e `db.rs`, wrapper finale `Core::get_project_overview` e stub dei metodi degli allegati, `git/overview.rs` stub, `attachments.rs` (layout su disco), comandi Tauri (`pick_attachment_files` completo), dichiarazioni e stub UI, mock, spec §5, §6, §9.2 |
| B1 | CORE-RUNNER | `lib.rs`, `claude.rs`, `runner.rs`, `runner/turn.rs`, `bin/fake-claude.rs`, `attachments.rs`; `tests/{flow,claude,real_cli}.rs` |
| B2 | CORE-OVERVIEW | `git/overview.rs`, `tests/git.rs` |
| B3 | UI-SHELL | `app.rs`, `views/{sidebar,settings,settings/project,overview}.rs`, `widgets/context_menu.rs`, `ipc/mock/overview.rs` |
| B4 | UI-TASKS | `views/{board,board/list,task_dialog,task_panel,start_dialog}.rs`, `state/board.rs`, `ipc/mock/attachments.rs` |
| C1 | MERGE | merge in ordine RUNNER → OVERVIEW → SHELL → TASKS su `wp/f-int`; spec §7.3, §7.8, §9.2, §10, §12.2, §13, §14, README |
| C2 | E2E | `ui/src/e2e.rs`, `src-tauri/src/e2e.rs`; `scripts/e2e.sh` verde |
| D | REVIEW | backend e sicurezza, UI/Leptos/a11y, test e spec; verifica avversariale dei finding HIGH, poi un agent di fix |

Item `pub` congelati durante B: `sidebar::{add_repository, projects_loaded}`, `start_dialog::mode_help`, `board::{column_title, NoProject}`, `git::overview::{Source, read}`.

#### Round 2026-10-01: Autopilota

Stesse regole, worktree `wp/a-<pkg>`, integrazione su `wp/a-int`.

| Fase | Pacchetto | Ownership |
|---|---|---|
| A | CONTRATTI | migrazione 0004, tipi, `db.rs` (mapping, candidati, setter della verifica), mock minimi, spec §5–§6 |
| B1 | SCHEDULER | `autopilot.rs`, `autopilot/verify.rs`, hook nel supervisore di `runner.rs`, wake in `lib.rs`, `merge_blocked`, `tests/flow.rs` |
| B2 | NOTIFICHE E TOOL | `Inner::notify`, `Settings.notifications`, `runner/board_tools.rs` (`after`, `auto` ereditato, coda), `ATM_APPEND`, scenari `fix_on_resume` e `board_chain` |
| B3 | UI | topbar, impostazioni progetto e app, dialog e pannello del task, badge di card e Lista, mock |
| C | E2E | merge su `wp/a-int`, `autopilot_fix_and_merge`, `autopilot_after`; `check.sh` ed `e2e.sh` verdi |
| D | REVIEW | core e concorrenza, sicurezza e processi, UI/spec/test; poi un agent di fix (correzioni in attesa di uno slot, profondità 2 via `auto_by`, kill delle verifiche, merge del solo commit verificato, cicli di dipendenze, §7.12, §9.2, §10, §13.7, README) |

#### Round 2026-10-02: Pianificatore e aggiornamenti

Stesse regole, worktree `wp/p-<pkg>` e `wp/u-updater`, integrazione su `wp/p-int`.

| Fase | Pacchetto | Ownership |
|---|---|---|
| A | CONTRATTI | migrazione 0005, tipi (`TaskKind`, `PlanState`, `PlanView`, i tre comandi), `db.rs` (piani, `launch`, filtri dei task nascosti), mock minimi, spec §5–§6 |
| B1 | CORE | `plan.rs`, argv di sola lettura (`PLAN_DENY`, append prompt), fine turno del piano, `launch` nello scheduler, recovery all'avvio, `create_task` → `planned_by`, scenario `plan`, `tests/flow.rs`, §7.13 |
| B2 | UI | card del pianificatore (`views/planner.rs`), mock con trascrizione, check E2E `planner_card` |
| B3 | UPDATER | `tauri-plugin-updater`, `updater.rs`, `app_info`/`check_update`/`install_update`, modale e banner, workflow di release, `bump-version.sh`, §9.5 (solo macOS; Windows in un round successivo) |
| C | INTEGRAZIONE | merge su `wp/p-int`, conteggio dei comandi (42), README e spec; `check.sh` ed `e2e.sh` verdi |

---

## 12. Verifica end-to-end

### 12.1 `fake-claude` (Rust, nessuna chiamata API)

- **Sottocomandi:**
  - `--version` / `-v` → `2.1.283 (Claude Code)`;
  - `auth status [--json]` → JSON, con `FAKE_CLAUDE_AUTH=in|out` che decide exit 0 o 1.
- **Modalità `-p …`:**
  - registra argv, cwd, `$PWD` e la presenza di `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `GIT_DIR`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_EFFORT`, delle altre variabili di una sessione padre e del suo host (`RECORDED_VARS`), di `NODE_OPTIONS` e `CMUX_ORIGINAL_NODE_OPTIONS_PRESENT` e di `CLAUDE_CONFIG_DIR` in `$FAKE_CLAUDE_RECORD` (una riga JSON per chiamata);
  - `system/init.apiKeySource` (2026-09-29) vale `none`, oppure `ANTHROPIC_API_KEY` se quella variabile gli arriva non vuota, oppure `apiKeyHelper` dopo un helper del repo (sotto), oppure il valore di `FAKE_CLAUDE_API_KEY_SOURCE`, che vince su tutti: così i test vedono l'app fermare un turno che fatturerebbe via API;
  - con `FAKE_CLAUDE_PROJECT_CONFIG=1` (M6) carica la configurazione del repo come il CLI reale: salvo `--setting-sources` senza `project` (o `local`), esegue con `sh -c` gli hook `SessionStart` di tipo command e l'`apiKeyHelper` di `.claude/settings.json` (o `settings.local.json`); salvo `--strict-mcp-config`, avvia i server di `.mcp.json` con `command`; registra ogni esecuzione (`kind: "project_config"`) e riporta server e `apiKeySource: "apiKeyHelper"` in `system/init`;
  - registra anche, per ogni messaggio utente, `{"kind":"turn",…,"prompt":<testo intero>}` (round feature del 2026-09-29: così un test vede la sezione `## Attachments` del prompt, §7.4) e, nello scenario `subagents`, ogni risposta dell'host a uno spawn come `{"kind":"subagent","n":i,"behavior":"allow"|"deny","message":…}`;
  - risponde a `initialize` (tranne `noinit`);
  - quando `--mcp-config` dichiara il server sdk `atm` (cioè sempre con l'argv dell'app, round 2026-09-30), prima di `system/init` fa l'handshake MCP come il CLI reale (`initialize`, `notifications/initialized`, `tools/list` come `mcp_message`) in ogni scenario che arriva a `init` (non `noinit` e `resume_fail`), e registra ogni messaggio con la risposta e il `pid` del processo (`kind: "mcp"`);
  - a ogni messaggio utente esegue lo scenario scelto con `[fake:NOME]` nel testo (mai nella sezione `## Parent task` del prompt di un sotto task, e lo stesso vale per i tag `[target:…]` e `[status:…]`), altrimenti `$FAKE_CLAUDE_SCENARIO`, altrimenti `simple`;
  - esce con 0 all'EOF di stdin.
- **Scenari:**

| Scenario | Comportamento |
|---|---|
| `simple` | init → delta di stream → testo → `tool_use` Write (scrive davvero `hello.txt`) → `tool_result` → `result` success |
| `approval` | Come `simple`, ma prima emette `can_use_tool` per Bash e si blocca finché non arriva la risposta |
| `slow` | Un evento al secondo |
| `hang` | init e poi attesa |
| `hang_ignore` | Ignora interrupt e SIGTERM; avvia un nipote `sleep 300` in un process group suo |
| `background` | Avvia un `sleep 300` in un process group suo (come un `run_in_background`), poi `result` success; esce all'EOF: il Core deve chiuderlo a fine turno |
| `crash` | Exit 1 senza `result` |
| `noinit` | Esce prima di `init` |
| `big` | `tool_result` da 50 KiB più una riga da 20 MiB |
| `flood` | 10.000 eventi |
| `control` | Invia un `hook_callback` e registra la risposta |
| `usage_limit` | `result` con testo di limite d'uso |
| `auth_fail` | `result` con testo "Not logged in · Please run /login" |
| `resolve_merge` | `git merge <FAKE_CLAUDE_TARGET>`, risolve concatenando le versioni, `git commit --no-edit` |
| `board_tools` | Solo se `--mcp-config` dichiara il server sdk `atm` (round 2026-09-30): `create_task` con parent `self`, `list_tasks`, `get_task` (subito), e `update_task`, `move_task` (a `[status:S]` del messaggio, default `inreview`) e `start_task` (di `[target:ID]`, default il sotto task creato), ciascuno dopo il suo `can_use_tool` (con `[ask:skip]` senza chiedere, come un CLI che salta la regola `ask`: l'host deve rifiutarli); ogni `tools/call` porta `_meta."claudecode/toolUseId"`; registra gli esiti (`kind: "board"`, `denied` per un rifiuto) |
| `fix_on_resume` | Come `simple`; in una sessione ripresa (`--resume`) scrive anche `fixed.txt`. Lo gioca anche il prompt di correzione dell'autopilota dopo una verifica fallita (round 2026-10-01) |
| `board_chain` | Come `board_tools` per il server `atm` (round 2026-10-01): due `create_task` con parent `self`, il secondo con `after` = il primo; poi `result` success |
| `plan` | Come `board_tools` per il server `atm` (round 2026-10-02): due `create_task` di primo livello («Primo task pianificato», «Secondo task pianificato» con `after` = il primo; descrizioni con `[fake:simple]`), poi un testo e `result` success; con `[then:hang]` resta in attesa dopo averli creati |
| `mcp_other` | Un `tools/list` al server `other` (non dichiarato); registra la risposta di errore |
| `subagents` | `FAKE_CLAUDE_SUBAGENTS` spawn (default 3) uno dopo l'altro, ciascuno un `tool_use` `Agent` più un `can_use_tool`; registra ogni risposta (`kind: "subagent"`) e chiude con `result` success (round feature del 2026-09-29) |

- Sull'interrupt risponde success ed emette `result` `error_during_execution` (tranne `hang_ignore`).
- Le forme JSON seguono le catture reali di M5 (`tests/fixtures/real/`): risposta a `initialize` con `account`, `system/status` prima di ogni richiesta, `rate_limit_event` dopo il primo messaggio assistant, `can_use_tool` con `display_name`, `description`, `decision_reason` stringa e le tre suggestion (`addRules`, `addDirectories`, `setMode`), risposta all'interrupt `{"still_queued":[]}` seguita da `[Request interrupted by user]`. Il `sleep` di `hang_ignore` guida un process group suo, come i comandi del tool Bash reale.

### 12.2 Percorso utente su fake-claude (M4)

1. Avvio con `FAKE_CLAUDE_AUTH=out` → gate "Accedi". Si imposta `in` e si preme "Ricontrolla" → board.
2. "Accedi" apre Terminal con lo script, eseguendo `auth login` di fake-claude.
3. Aggiunta di un repo temporaneo con un commit. Una cartella non git, un repo bare e un repo vuoto vengono rifiutati; un repo con `.mcp.json` mostra un warning.
4. Creazione di due task, trascinamento e riordino, riavvio dell'app: l'ordine resta.
5. Avvio "Crea hello `[fake:approval]`" in Auto-edit → badge live dell'agente (in esecuzione o approvazione, `role=status`) → card di approvazione → "Approva sempre" → streaming → TurnEnd → badge "In revisione".
6. Tab Modifiche: `hello.txt` aggiunto.
7. Follow-up `[fake:simple]` → `--resume` nel record. Stop durante `[fake:slow]` → killed entro 5 s.
8. Uscita con Cmd+Q durante `[fake:hang]` → nessun processo residuo. Al riavvio c'è "Interrotto – Continua"; "Continua" usa `--resume`.
9. Merge con il target in checkout e pulito → task Fatto, worktree rimosso, `git log` del target mostra il commit squash.
10. Secondo task che crea un conflitto → "Conflitti" → "Risolvi con l'agente" `[fake:resolve_merge]` → merge ok.
11. Scarta un attempt → worktree rimosso, branch presente, task in Da fare.
12. `[fake:usage_limit]` → banner di pausa → "Riprendi". `[fake:auth_fail]` → torna il gate.
13. (M6) Sicurezza, `security_confirmations` nella fase 2: le conferme native ricevono la risposta che il run mette in coda (`debug_e2e_queue_confirm`, solo build di debug con `ATM_E2E=1`, come il folder picker); il run vede ogni conferma chiesta e il suo testo. Attendibile + Annulla → resta Isolato; + OK → Trusted e `trusted`, annotato nelle impostazioni del progetto; bypass + OK → consentito; abbassare entrambi non chiede nulla. Passthrough della chiave API + OK → banner nella topbar; disattivarlo non chiede nulla. Al passo 7 il `TurnEnd` del turno fermato dice "Interrotto dall'utente" senza `[ede_diagnostic]`; nessun agente riceve le variabili di una sessione Claude Code padre (`scripts/e2e.sh` le imposta all'app).
14. (M6) Il giro gira un bundle `.app` di debug (con `testkit`) installato nella sua cartella: dopo l'uscita del passo 8 lo reinstalla (copia nuova sopra) e controlla che DB, log e worktree restino; `pgrep -f` della copia di fake-claude del giro è vuoto dopo ogni uscita. Fase 3: `[fake:flood]` su 3 attempt concorrenti con reattività e finestra del DOM misurate (§11.2 M6).
15. (Round feature del 2026-09-29; i dettagli stanno in `ui/src/e2e.rs` e, per le fixture, in `src-tauri/src/e2e.rs`) La pagina del progetto cambia il percorso: selezionare un progetto atterra sul **Riepilogo**, quindi
    - `select_main`/`select_tasks` cliccano sempre il tab `[data-project-view=tasks]` e aspettano le colonne (`[data-column=todo] [data-card-list]`); `exit_during_turn` passa prima ai Task;
    - le Impostazioni si dividono: `open_app_settings()` apre il dialog "Impostazioni app" senza tab (usata per gli agenti in parallelo e il passthrough della chiave API), `open_project_settings()` apre il tab Impostazioni di `main` e restituisce `[data-testid=project-settings]` (usata dalle conferme di sicurezza);
    - le voci della sidebar si leggono da `[data-testid=projects] [data-project]`, non da ogni `button` (c'è anche il "⋯").
    - **Fixture** (`create_repos` del backend): il repo `mcp` **committa** sul branch target un `CLAUDE.md` ("# Istruzioni E2E") e un `.mcp.json` con il server `e2e-tools` (`command`, `args` e `env: {TOKEN: "secret-value"}`; il Riepilogo legge solo il commit); il repo `da-rimuovere` (solo un README) e il file da allegare `attach/specifiche-e2e.txt`, fuori da ogni cartella che il Core rifiuta; `main` resta com'è.
    - **Passo 3** (fase 1): dopo i tre rifiuti si aggiunge prima `mcp` (toast e avviso su `.mcp.json` e Isolato), che atterra sul suo Riepilogo (`overview_of_mcp`): card CLAUDE.md con "non caricato" e "letto dall'agente su istruzione del prompt", riga `[data-mcp-server=e2e-tools]` con `stdio` e la chiave `TOKEN`, blocchi di README.md e `.mcp.json`, configurazione "Isolata"; `secret-value` non compare né nel markup della pagina (testo e attributi) né nella risposta di `get_project_overview`; la pagina è `main > #project-page-overview[role=tabpanel]`, con `main` senza ruolo; il blocco di CLAUDE.md è aperto, quello di README.md chiuso con il testo `inert` finché il suo trigger (`aria-expanded`, `aria-controls`) non lo apre; "Crea il primo task" (`[data-action=open-tasks]`) porta alla sua board vuota. Poi si aggiunge `main`, che atterra sul proprio Riepilogo (il README, nessun CLAUDE.md), e si passa ai suoi Task.
    - **Fase 2**, dopo `security_confirmations` e `csp_enforced`, nell'ordine:
      - `task_list_view` su `main`: la Lista mostra una riga `tr[data-row-task-id]` per card, nell'ordine di `get_board` (colonna, poi posizione) e con il titolo della colonna, nessuna card; il titolo di T4 apre il suo pannello e nasconde Branch e Aggiornato; Kanban riporta le colonne;
      - `attachment_to_the_agent`: aggiunge il progetto `da-rimuovere` (atterra sul Riepilogo) e dal dialog crea un task con "Aggiungi file…", che prende il file messo in coda con `debug_e2e_queue_pick`, copiato in `<data_dir>/attachments/<progetto>/<task>/<id>/specifiche-e2e.txt`; lo avvia con "Nessun sub-agent": il primo turno ha nel prompt `## Attachments` con il path della copia e "read-only", e nell'argv `--add-dir=…/attachments/<progetto>/<task>` e `--disallowedTools=AskUserQuestion,Agent,Task,Workflow`; il pannello mostra il chip dell'allegato e "Sub-agent: nessuno";
      - `subagent_limit`: sullo stesso progetto un task `[fake:subagents]` (tre spawn) avviato con "Sub-agent (max)" 2 e modello haiku ha `--disallowedTools=AskUserQuestion,Workflow`, nessun `--add-dir`, `"ask"` che finisce con `"Agent","Task"` (dopo le regole dei tool board) e `CLAUDE_CODE_SUBAGENT_MODEL=haiku` nelle `--settings`; l'host risponde allow, allow, deny, quest'ultimo con "Sub-agent limit for this task reached (2)."; DB e pannello dicono "Sub-agent: haiku, max 2 (usati 2)";
      - `subtasks_from_the_panel` (giro 2026-09-30), su `da-rimuovere`: "Aggiungi sotto task" (`[data-action=add-subtask]`) nel pannello di un task apre `TaskDialog` come "Nuovo sotto task" con `[data-parent-title]` "↳ Sotto task di «…»"; il figlio ha `parent_id` nel DB, il pannello del padre lo elenca in `[data-subtasks] [data-subtask-id]`, la card del padre ha `[data-subtask-progress]` "0/1" e quella del figlio la riga "↳ <padre>"; nella Lista le righe seguono `state::board::nested` (il figlio subito sotto il padre, con "↳") e `[data-action=toggle-subtasks]` del padre lo nasconde e lo rimostra;
      - `board_tools_agent`: con Agenti in parallelo = 1 (poi di nuovo 2), un task `[fake:board_tools]` avviato dal dialog Avvia crea subito il suo sotto task (sulla board, sotto il padre, mentre l'agente aspetta la prima approvazione); `update_task`, `move_task` e `start_task` chiedono con la card di approvazione, che ha la frase (`[data-board-tool]`, es. «Sposta «Sotto task dal fake (rivisto)» in In revisione») e nessun "Approva sempre"; approvati, il sotto task è rinominato, passa a In revisione, e il suo avvio torna all'agente come errore del tool `ConcurrencyLimit`; il record di fake-claude ha le risposte ai suoi `mcp_message` (handshake, `tools/list` con 6 tool, 6 `tools/call`);
      - `subtask_cascade`: "Modifica" sulla card del padre di `subtasks_from_the_panel` → "Elimina" avvisa "Elimina anche 1 sotto task." (`[data-delete-warning]`) → "Conferma eliminazione": padre e figlio spariscono dalla board e da `get_board`;
      - `autopilot_fix_and_merge` (round 2026-10-01), su `da-rimuovere`: l'interruttore «Autopilota» della topbar (`[data-action=toggle-autopilot]`) passa ad `aria-pressed=true` e la casella `#project-autopilot` delle Impostazioni del progetto lo segue; lì si salvano (`[data-action=save-autopilot]`, toast «Autopilota aggiornato») il comando di verifica `test -f fixed.txt` e «Merge automatico se verificato», con 2 tentativi di correzione. Un task creato dal dialog con «Affida all'autopilota» (`#task-auto[data-auto-toggle]`) e `[fake:fix_on_resume]` parte da solo, senza il dialog Avvia; la prima verifica fallisce (badge `[data-verify=failed]` «Verifica fallita (1/2)» sulla card), il follow-up dell'autopilota (`--resume`, prompt «The project's verification command `test -f fixed.txt` exited with code 1…») scrive `fixed.txt`, la seconda passa e il task è mergiato da solo: Fatto, attempt `merged` con `verify_state` passed e 1 correzione, commit squash in cima al target con il titolo del task. Le notifiche finiscono in `notify.jsonl` della cartella del giro (`CoreConfig::notify_log`), mai in `osascript`;
      - `autopilot_after`: con Agenti in parallelo = 1 (poi di nuovo 2), due task `[fake:append]` affidati dal dialog, il secondo con «Parte dopo…» (`[data-after] select`) sul primo; il secondo resta in Da fare con il badge `[data-queued]` «In coda» (tooltip «Parte dopo «…»») e parte solo quando il primo è Fatto; poi è mergiato anche lui, sopra il primo (`fixed.txt` è già sul target: le verifiche passano subito);
      - `app_version` (round 2026-10-02): il piede della sidebar dice «AI Task Manager v<versione del workspace>» (`app_info`), e una build di debug non annuncia aggiornamenti (niente banner né modal);
      - `planner_card` (round 2026-10-02), su `da-rimuovere` messo in Supervisionato dalle sue Impostazioni: la card `[data-planner]` del Riepilogo parte vuota e «Pianifica» resta spenta finché non c'è un prompt; un primo piano `[fake:hang]` deve mostrare la trascrizione dal vivo con «Ferma», che lo porta a `failed`; un secondo piano `[fake:plan]` arriva a «Avvia 2 task?» (`[data-testid=plan-question]`), con «Pianifica» spenta nel frattempo; i due task sono in Da fare senza attempt, il secondo «dopo» il primo; il task del piano non è né in `get_board` né sulla pagina; «No» lo lascia `dismissed` (`get_plan`) e i task intatti; la trascrizione finita si apre con il toggle e un task creato apre il suo pannello;
      - `project_removal`: con `main` selezionato, il menu di `da-rimuovere` si apre con un `contextmenu` sintetico che la pagina deve annullare (voci Impostazioni progetto e Rimuovi dalla lista…, trigger "⋯" `aria-expanded`) e si chiude con un `pointerdown` fuori; riaperto, "Rimuovi dalla lista…" → `RemoveProjectDialog` ("Rimuovere «da-rimuovere» dalla lista?") → "Rimuovi": il progetto sparisce dalla sidebar e da `list_projects`, `main` resta selezionato sulla sua board, il task del progetto dà `get_task_detail: NotFound`, gli allegati e i log dei tre attempt spariscono dalla cartella dati e i worktree dal disco e da git; branch e checkout restano.
    - **Errori IPC attesi**: il giro confronta, nell'ordine, i comandi falliti con quelli che provoca: nella fase 1 i tre `add_project: Invalid` del passo 3 (`EXPECTED_FAILURES`); nella fase 2, tutti dopo il passo 10, `["set_project_security: Invalid", "get_task_detail: NotFound"]` (`EXPECTED_FAILURES_PHASE2`: Attendibile + Annulla, poi il task del progetto rimosso); nella fase 3 nessuno.

### 12.3 Checklist con il CLI reale (M5, eseguita dall'utente)

M5 (2026-09-28) l'ha eseguita l'agente, con l'OK esplicito dell'utente e **solo con l'abbonamento**, attraverso le API del Core: harness opt-in `crates/atm-core/tests/real_cli.rs` (comando e guardia nel README), cloni temporanei del repo giocattolo, `sonnet` con effort `low`. Esiti nel §13.4.

1. Repo giocattolo con un commit. Task "Aggiungi una riga al README". Avvio in Auto-edit.
2. Annotare: entry `SessionInit` (`apiKeySource`, `permissionMode`, `mcp_servers` = 0 in Isolated), streaming, `TurnEnd` con costo.
3. Chiedere all'agente di eseguire `ls` → arriva `can_use_tool` → Approva. Poi "Approva sempre" su un altro comando e verificare che il turno successivo non chieda più.
4. Follow-up → il resume funziona (il contesto è ricordato).
5. Stop a metà turno → annotare il subtype del `result` dopo l'interrupt.
6. Uscire dall'app durante un turno, riavviare, "Continua".
7. Chiedere "esegui git push" → **negato** (conferma che il deny in `--settings` funziona).
8. Repo con `.claude/settings.json` il cui hook scrive un marker e con un CLAUDE.md che contiene una parola chiave: in Isolated il marker non compare. Annotare se l'agente conosce la parola chiave (voce E11).
9. Merge.
10. Consegnare i raw log all'agente per creare le fixture.

### 12.4 Controlli automatici continui

- `scripts/check.sh`: unit test, integrazione con fake-claude e repo temporanei, grep.
- `ATM_SELFTEST=1` nel bundle di debug con `testkit` (M0, M3, M4, M6).
- `scripts/e2e.sh` (M4, M6: reinstallazione e prestazioni) e `scripts/release.sh` (M6: `.app`, `.dmg`, niente driver di test nella release).
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
| R13 | Il modello dei sub-agent non è garantito: per il CLI `CLAUDE_CODE_SUBAGENT_MODEL` è la fonte con la priorità più bassa. Vincono il `model` della chiamata Agent e quello del frontmatter dell'agente (Explore ha `inheritCap: "opus"`); `CLAUDE_CODE_SUBAGENT_MODEL_FORCE` li scavalcherebbe, ma si è visto solo nel bundle e non si usa (round feature del 2026-09-29) | Detto nel dialog Avvia ("vale per i sub-agent che non ne chiedono uno proprio") e nel README; valore nelle `--settings`, che battono l'`env` delle impostazioni di utente e repo (§7.3) |
| R14 | Il limite di sub-agent dipende da una regola `ask` su `Agent`/`Task` che arriva all'host in ogni modalità, bypass compreso: verificato con il CLI reale 2.1.284 il 2026-09-29 (§13.3). Non copre percorsi di spawn diversi da Agent, Task e Workflow; conta gli spawn concessi, non quelli completati | `Workflow` vietato con un limite; spawn oltre il limite negati dall'host; test `#[ignore]` in `tests/real_cli.rs`, passati il 2026-09-29 (§13.3) |
| R15 | Gli allegati sono copie che l'agente può modificare o cancellare con Bash (in Auto-edit e Autonomo senza chiedere): "sola lettura" è solo un'istruzione del prompt | Gli originali non vengono mai toccati; nessuna regola deny, che proteggerebbe solo le copie e Bash aggirerebbe (§7.3, §10.2) |
| R16 | I tool board (round 2026-09-30) dipendono dal protocollo dei server MCP `sdk` del CLI 2.1.285 (`--mcp-config` con `type:"sdk"`, `control_request` `mcp_message`, risposta `{"mcp_response": …}`, `_meta."claudecode/toolUseId"`): non documentato, verificato dallo spike del 2026-09-30 e dal test con l'app del 2026-10-01 (CLI 2.1.286), e può cambiare come il control protocol (R1). Anche la regola `ask` sui tool `mcp__atm__*` in ogni modalità è comportamento osservato, come in R14 | Parsing tollerante e risposta di errore per ciò che non si riconosce (§7.4); l'host esegue un tool che chiede solo con il `tool_use_id` approvato, quindi un cambio di precedenza del CLI fa fallire la chiamata invece di saltare l'approvazione; fake-claude riproduce le forme dello spike; test `#[ignore]` `real_cli_board_tools_ask_in_every_mode` da ripetere a ogni versione testata del CLI (§13.3) |

### 13.2 Rimandati (con il trigger che li riporta in scope)

| Rimandato | Quando aggiungerlo |
|---|---|
| Plan mode ed ExitPlanMode, risposte ad AskUserQuestion | Quando servono flussi di pianificazione |
| Cambio della modalità di permesso a turno in corso | Richiesta degli utenti |
| Follow-up in coda o inseriti a metà turno | Quando gli utenti scrivono mentre l'agente lavora |
| Rendering markdown (pulldown-cmark, HTML come testo), anche di CLAUDE.md e README nel Riepilogo | Lamentele sulla leggibilità |
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
| Ricerca, filtro e ordinamento nella vista Lista (solo lì: filtrare la kanban romperebbe l'indice del DnD) | Progetti con molti task |
| Drag&drop di file negli allegati (`dragDropEnabled:false` serve al DnD HTML5 della kanban) | Se il picker non basta |
| Cache del Riepilogo per `(repo, tip)`, come quella della configurazione del commit | Se `ls-tree` più `cat-file` a ogni visita diventano lenti |
| Default di progetto per limite e modello dei sub-agent; cambio del modello a ogni turno | Richiesta degli utenti |
| Pagina e vista (Kanban/Lista) ricordate per progetto; chiusura del menu del progetto sullo scroll e Shift+F10 | Richiesta degli utenti |

### 13.3 Voci da verificare, e dove si chiudono

| Voce | Dove |
|---|---|
| Build di Trunk su 1.97.1; flag wasm-opt; Tailwind 4.3.3; compilazione dei binding; costruttore del Channel; compilazione di `atm-ui` per l'host; flag di `cargo tauri init` | M0 |
| Drag-and-drop HTML5 su WKWebView con `dragDropEnabled:false`; `.command` in Terminal e Gatekeeper | M2-UI-BOARD / M4 |
| `process_group` più `killpg`; kill verificato con `ps` | M3-CORE |
| Obbligatorietà di `--verbose`; ordine e necessità di `initialize`; subtype di `result` dopo un interrupt; valori di `apiKeySource`; forma degli eventi di rate limit e usage limit | M5, chiuse (§13.4) |
| Deny in `--settings` efficace; `--strict-mcp-config` senza `--mcp-config` dà 0 server (M5; dal round 2026-09-30 l'app passa sempre `--mcp-config` col solo server `atm`, quindi con Isolated il CLI carica solo quello); `--setting-sources=user` esclude hook di progetto e CLAUDE.md | M5, chiuse (§13.4) |
| Le regole con destination `session` sopravvivono a `--resume` (le ripassiamo comunque) | M5, chiusa (§13.4) |
| Nome della directory del progetto nel CLI: realpath o `$PWD` (per noi è la stessa stringa) | M5, chiusa (§13.4) |
| ~~`claude auth status` e `--version` con cwd `/` (M6-REVIEW, §7.1) rispondono come dalla cartella dell'app; il CLI legge o no le impostazioni di progetto per `auth status`~~ **Chiuso 2026-09-29 (CLI 2.1.284, nessuna quota):** da `/` rispondono `2.1.284 (Claude Code)` e `authMethod: claude.ai`. In una cartella con `.claude/settings.json` che dichiara `apiKeyHelper`, `auth status` riporta `authMethod: api_key_helper` (legge le impostazioni di progetto del cwd, senza eseguire né hook né helper): `PROBE_CWD = "/"` è quindi necessario | M6 (chiuso) |
| ~~Round feature 2026-09-29: la regola `ask` su `Agent`/`Task` arriva all'host in Supervisionato, Auto-edit e Autonomo; il deny del limite viene rispettato; `CLAUDE_CODE_SUBAGENT_MODEL` si applica a un sub-agent senza modello proprio; `--add-dir` con uno spazio nel path (`Application Support`) è leggibile~~ **Chiuso 2026-09-29 (CLI 2.1.284, 7 turni reali, ≈ 1,9 USD stimati, solo abbonamento):** in tutte e tre le modalità, bypass compreso, ogni spawn di `Agent` arriva come `can_use_tool` con `decision_reason.type = "rule"` e l'host risponde senza approvazione pendente; con limite 1 il secondo spawn è `Denied` con il testo esatto (nei `permission_denials` del `result` il CLI lo riporta col nome storico `Task`) e al turno successivo `--disallowedTools=AskUserQuestion,Agent,Task,Workflow` rimuove il tool ("No such tool available: Agent"); con `env.CLAUDE_CODE_SUBAGENT_MODEL = "opus"` nelle `--settings` e agente principale `sonnet`, il sub-agent gira su `claude-opus-5-5` (entrambi i modelli in `modelUsage`); un allegato sotto `…/Application Support/attachments/…` passato con `--add-dir` si legge con Read senza approvazioni. Test: `real_cli_subagent_ask_rule_reaches_the_host_in_every_mode`, `real_cli_subagent_deny_is_respected`, `real_cli_subagent_model_applies_without_an_explicit_model`, `real_cli_add_dir_with_spaces_is_readable` | Round 2026-09-29 (chiuso) |
| Round 2026-09-30, **fase E**: i tool board con il CLI reale e l'app intera (non lo script dello spike). In Supervisionato, Auto-edit e Autonomo, `create_task` con parent `self` crea il sotto task senza chiedere; `move_task` arriva all'host come `can_use_tool` con `mcp_server.source = "sdk"` e, approvato, sposta il task; `system/init` conta il server `atm`. Lo spike aveva verificato default e bypass con un tool `ping`. Test: `real_cli_board_tools_ask_in_every_mode` (3 turni haiku, `ATM_REAL_CLAUDE=1 cargo test -p atm-core --test real_cli -- --ignored board_tools`), **passato il 2026-10-01 con il CLI 2.1.286 in tutte e tre le modalità** (circa 0,19 USD) | ~~Round 2026-09-30~~ **Chiuso 2026-10-01** |

### 13.4 Esiti di M5 (CLI 2.1.283, 2026-09-28)

Osservati con l'harness `tests/real_cli.rs` (18 turni reali in tutto, stima del CLI ≈ 2,9 USD a prezzo di listino, addebitati all'abbonamento), traffico ripulito in `crates/atm-core/tests/fixtures/real/` e ripetuto senza CLI da `tests/real_fixtures.rs`.

| Voce | Esito |
|---|---|
| `--verbose` | Obbligatorio: senza, exit 1 con "When using --print, --output-format=stream-json requires --verbose" |
| `initialize` | Non necessario, mantenuto. Risposta dopo l'avvio degli hook `SessionStart` dell'utente, prima di `system/init` (che arriva solo col primo messaggio utente); porta `account` con email e organizzazione → oscurate nel raw log (fix). Con `--resume` di una sessione inesistente il CLI esce prima di rispondere |
| `result` dopo un interrupt | `error_during_execution`, `is_error`, `errors: ["[ede_diagnostic] …"]`, `terminal_reason: "aborted_streaming"`, exit 1; risposta all'interrupt `{"still_queued":[]}`; stop reale in 0,4–0,7 s. Il testo di `TurnEnd` era quel `[ede_diagnostic]` interno; M6: `TurnEnd.stopped` e "Interrotto dall'utente", e la chiamata approvata che il CLI rifiuta sull'interrupt (`non_execution_kind: "user-rejected"`) è `Cancelled` invece di `Failed` |
| `apiKeySource` | `none` con il login claude.ai (Max); nessun altro valore osservato (la guardia dell'harness avrebbe fermato il turno). L'app avvisa anche quando manca (fix) e dal 2026-09-29 ferma il turno quando indica una chiave API col passthrough spento (§7.6), con lo stesso limite della guardia. La guardia non può impedire la prima richiesta di un turno (`system/init` arriva con essa): per questo l'harness fa prima un preflight gratuito con solo `initialize` e controlla l'`account` della risposta (`apiProvider`, `subscriptionType`) |
| Eventi di limite | `rate_limit_event` con `rate_limit_info: {status: "allowed", resetsAt, rateLimitType: "five_hour", overageStatus, overageDisabledReason, isUsingOverage, unifiedWindows: {five_hour, seven_day: {utilization, resetsAt}}}`, uno per richiesta; nessuna entry. Nessun limite raggiunto: `LIMIT_PATTERNS` invariati |
| Deny in `--settings` | Efficace (§7.8): `git push` negato senza `can_use_tool`, voce in `permission_denials`, remote vuoto. Il CLI emette anche `system/permission_denied` e marca il `tool_result` con `non_execution_kind: "permission-rule"`; M6: la chiamata è `Denied`, non `Failed` |
| `--strict-mcp-config` senza `--mcp-config` | `mcp_servers: []` benché l'utente abbia server MCP e plugin; il server di `.mcp.json` non parte (con `--setting-sources=user,project` e senza strict sì: controllo positivo) |
| `--setting-sources=user` | Esclude gli hook di progetto (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`: nessun marker; con `user,project` il `SessionStart` scrive) e CLAUDE.md, che non è nel contesto (l'agente risponde `NOT_IN_CONTEXT`). E11: l'istruzione dell'append prompt basta, l'agente legge CLAUDE.md (con `cat`) e conosce la parola chiave. Gli hook dell'**utente** girano comunque |
| Regole `session` dopo `--resume` | Non sopravvivono; il re-pass via `--settings` è necessario e funziona anche in una sessione nuova (§7.8) |
| Directory del progetto nel CLI | realpath (cwd `/private/var/…` anche con `PWD=/var/…`), ogni carattere non alfanumerico → `-`; si vede in `system/init.memory_paths.auto` e nel `transcript_path` degli hook |
| `decision_reason` | Stringa (`"This command requires approval"`) più `decision_reason_type: "other"`; assente quando c'è `blocked_path` |
| `permission_suggestions` | `addRules` (`localSettings`) + `addDirectories` (`session`, il worktree) + `setMode` (`session`, `acceptEdits`): `can_remember` filtra solo le `addRules`/`allow` (fix, §7.8) |
| `RESUME_FAILED_PATTERN` | Confermato: stderr "No conversation found with session ID: <uuid>" e lo stesso testo in `errors` di un `result` `error_during_execution`, exit 1 |
| Permission mode | `--permission-mode=default` accettato (alias di `manual`); `acceptEdits` approva da solo i comandi Bash di lettura/scrittura file nel cwd (§10.2) |
| Process group dei comandi Bash | Ognuno nel suo gruppo: il CLI li chiude su interrupt; il Core registra i discendenti del leader mentre vive e a fine turno chiude quelli rimasti, anche su un'uscita normale, e li uccide con il leader sui percorsi con SIGKILL (fix, §7.4). `ps -o command=` mostra il path di claude e il `--session-id`/`--resume`: la verifica degli orfani (§7.9) funziona |
| Follow-up "fuori tema" | Il modello può rifiutare un follow-up che ritiene estraneo al task ("Task done already…"). M6: `ATM_APPEND` dice che i messaggi successivi dell'utente continuano il task anche quando sembra finito (§7.3); vale per gli attempt nuovi (il prompt si congela al primo turno) |

### 13.5 Limiti noti del round feature del 2026-09-29

- **Riepilogo** (`git/overview.rs`):
  - i symlink non si seguono: un `CLAUDE.md → AGENTS.md` mostra solo "→ AGENTS.md", benché il CLI legga attraverso il link, e un `.claude` che è un link nasconde `.claude/CLAUDE.md` e `.claude/settings.json`;
  - maiuscole e forme Unicode di APFS non si considerano: un `claude.md` non compare, benché il CLI su APFS lo apra;
  - `.claude/settings.local.json` non si mostra mai; un `.mcp.json` oltre 64 KiB non dà l'elenco dei server;
  - il JSON mostrato ha le chiavi riordinate alfabeticamente e perde la formattazione originale;
  - un URL con una password che contiene `/`, `?`, `#` o `\` non codificati (quindi non un URL valido) può comparire in parte: la divisione segue WHATWG;
  - U+200D nelle sequenze emoji conta come carattere nascosto;
  - in un partial clone senza i blob del tip, `ls-tree -l` fallisce e il Riepilogo dà un errore `Git` (lì l'app non può nemmeno creare worktree);
  - il mascheramento è euristico (§10.2) e il Riepilogo non ha cache (ogni visita costa un `ls-tree` e al più un `cat-file`);
  - i server MCP aggiunti con `claude mcp add` (in `~/.claude.json`, scope local o user) non compaiono: l'app non legge mai quel file.
- **Allegati:** Bash può modificare le copie (R15); un token scade dopo 10 minuti, quindi un dialog di creazione lasciato aperto più a lungo finisce con il toast "Task creato, allegati non aggiunti"; lo stesso file scelto due volte dà due allegati; la UI controlla solo il numero, la dimensione la controlla il Core allo staging; un'aggiunta in corsa con `remove_project` può lasciare cartelle vuote; niente drag&drop di file.
- **Sub-agent:** R13 e R14; il comportamento del CLI su cui si basano è verificato con il CLI reale 2.1.284 (§13.3, 2026-09-29).
- **UI:** pagina e vista non ricordate; nessun cambio di stato dalla Lista ("Sposta in" del pannello); le date "oggi"/"ieri" della Lista non si aggiornano da sole dopo mezzanotte; il trigger di `Collapsible` (vendorizzato) non ha `aria-expanded` fuori dal Riepilogo (che glielo passa, con `aria-controls` e il testo chiuso `inert`), cioè negli output dei tool e nei file del diff; rimosso il progetto selezionato, la sua pagina resta per un giro di `list_projects` finché `refresh_projects` non ne seleziona un altro; se la voce rimossa aveva il focus, alla chiusura del dialog il focus va sul `body`.

### 13.6 Limiti noti del round del 2026-09-30 (sotto task e tool board)

- **Sotto task:** un solo livello (un sotto task non ha sotto task); nessun roll-up automatico dello stato del padre, che mostra solo "↳ n/m"; la Lista non ricorda quali padri sono compressi; eliminare un padre elimina i sotto task senza modo di staccarli prima.
- **Eliminazione a cascata:** `delete_task` tiene i lock del task e dei sotto task, quindi nessun sotto task parte o nasce durante l'eliminazione; un follow-up su un tentativo già esistente di un sotto task prende però solo il lock del suo attempt, e se parte nella finestra tra il controllo e la rimozione di quel sotto task l'eliminazione si ferma con `Busy` a metà, dopo i sotto task già tolti.
- **Tool board:** "Consenti sempre" non ricorda mai un tool board (nessuna suggestion, §7.8): ogni modifica, spostamento e avvio chiede di nuovo; un agente avviato da un agente non può usare `start_task` (profondità 2); `start_task` su un sotto task usa la modalità dell'agente chiamante, e la card di approvazione mostra solo l'etichetta e l'input, non la modalità che avrà il nuovo agente; il dialog Avvia non c'è, quindi branch target e opzioni sono quelli predefiniti del progetto e del chiamante.
- **CLI reale:** R16; la verifica completa con l'app (fase E) è passata il 2026-10-01 con il CLI 2.1.286 (§13.3).

### 13.7 Limiti noti del round del 2026-10-01 (Autopilota)

- **Nessun nuovo tentativo da solo:** dopo un turno `Failed`, `Killed`, uno Stop dell'utente, un `AuthFailure` o un `UsageLimit`, o una verifica in `error`, l'autopilota lascia il task (`auto` spento, notifica). Un turno interrotto dalla chiusura dell'app non riparte da solo («Continua» resta all'utente), e una verifica interrotta dalla chiusura diventa `error` allo startup, con il task che resta In revisione con `auto` finché l'utente non lo riprende.
- **Verifica:** gira codice del repository senza approvazione, in ogni modalità (§10.2); non è una sandbox. Un processo che la verifica stacca dal suo albero (un genitore già uscito) può sopravviverle; allora l'output oltre i 2 s di attesa dell'EOF si perde dal riassunto. La durata non si registra. Spegnere l'autopilota o togliere `auto` non ferma una verifica in corso (la catena si ferma dopo); discard ed eliminazione sì.
- **Merge automatico:** se la verifica lascia file non ignorati nel worktree (snapshot, report di coverage, lockfile riscritti), il merge non parte e l'autopilota lascia il task: vanno ignorati in `.gitignore` o committati dall'agente. Se HEAD cambia tra verifica e merge per 3 volte di fila, l'autopilota lascia il task.
- **Dipendenze:** una sola per task; cicli e task annullati sono rifiutati quando si sceglie la dipendenza, ma un task annullato **dopo** blocca chi dipende da lui («In coda» per sempre, finché l'utente non cambia «Parte dopo…»).
- **Correzioni in attesa:** una correzione che non trova lo slot aspetta in `verify_pending` e passa prima della coda; se nel frattempo l'utente manda un follow-up e il suo turno finisce, la correzione decade e si rifà la verifica. In una finestra stretta (fine del turno dell'utente mentre lo scheduler sta già mandando la correzione) può partire una correzione superflua, contata.
- **Notifiche:** al massimo una ogni 30 s per task e titolo: due esiti dell'autopilota sullo stesso task a meno di 30 s (raro) mostrano solo il primo. Solo macOS (`osascript`), nessuna azione cliccabile.
- **UI:** le card leggono l'autopilota del progetto, i «Tentativi di correzione» e il titolo della dipendenza senza tracciarli: un tooltip «In coda» o un «(n/max)» si aggiorna al prossimo cambiamento della card. La riga «Verifica:» del pannello mostra il comando attuale del progetto, non per forza quello verificato.
- **Coda:** lo scheduler sceglie per posizione nella board, progetto per progetto, senza equità tra progetti.

---

## 14. Domande aperte per l'utente

1. Identificatore del bundle `dev.aitaskmanager.desktop` e nome "AI Task Manager": vanno bene? Il nome non deve contenere "Claude Code".
2. Modalità di default **Auto-edit** (`acceptEdits`) e concorrenza **2**: confermi?
3. Root dei worktree `~/.ai-task-manager/worktrees`: va bene? È configurabile.
4. Dopo il merge: worktree rimosso e **branch tenuto**. Oppure vuoi la cancellazione automatica del branch?
5. Consenti la validazione con il CLI reale in M5, che consuma quota dell'abbonamento?
6. L'uso è personale o l'app verrà distribuita ad altri? In caso di distribuzione si pone la questione dei Commercial Terms per "offrire Claude Code in un prodotto". Non cambia l'architettura.
7. È confermato che in v1 sono esclusi: push, fetch e PR, server MCP per i task, rewind? Le immagini non sono più escluse (round feature del 2026-09-29): si allegano al task come file qualsiasi e l'agente le legge con il tool Read dal path che il prompt gli dà (§7.4); restano esclusi l'incolla di immagini nel composer e il drag&drop di file.
