# AI Task Manager

App desktop per macOS: una kanban di task affidati al Claude Code CLI installato sul Mac, un git worktree per ogni tentativo.
Stack: Tauri 2 + Leptos 0.8 (CSR, WASM) + componenti Rust/UI. Piano completo: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Setup (una volta)

Servono macOS con Xcode Command Line Tools e `rustup`. Il toolchain `1.97.1` (con clippy, rustfmt e il target
`wasm32-unknown-unknown`) è fissato da `rust-toolchain.toml` e rustup lo installa da solo; in modo esplicito:

```bash
rustup toolchain install 1.97.1 --profile minimal -c clippy -c rustfmt
rustup target add wasm32-unknown-unknown --toolchain 1.97.1
cargo install trunk --version 0.21.14 --locked
cargo install tauri-cli --version 2.12.0 --locked
```

Niente Node: alla prima build Trunk scarica da solo Tailwind standalone `4.3.3`, `wasm-bindgen` `0.2.129`
(deve coincidere con la versione in `Cargo.lock`) e `wasm-opt` (sezione `[tools]` di `ui/Trunk.toml`).
`tw-animate.css` è vendorizzato in `ui/style/vendor/`.

## Sviluppo

```bash
cargo tauri dev          # avvia `trunk serve` in ui/ (porta 1420) e apre l'app
scripts/check.sh         # fmt, clippy host + wasm32 (anche --features mock), test, grep di sicurezza: va tenuto verde
(cd ui && trunk serve --features mock)   # solo UI nel browser (porta 1420), backend finto in ui/src/ipc/mock
```

Nel mock, `http://localhost:1420/?task=<id>` apre subito il pannello di quel task. Con la baseline di M1 in
`ui/src/ipc/mock/board.rs` (loggato, un progetto, un task per colonna) gli id sono `task-todo`, `task-inprogress`,
`task-inreview` e `task-done`.

## Avvio dell'app

**Con il Claude Code reale.** L'app cerca `claude` da sola (impostazione "Percorso di claude", poi `ATM_CLAUDE_PATH`,
poi `~/.local/bin/claude`, `~/.claude/local/claude`, Homebrew e il `PATH` della login shell) e legge solo
`claude --version` e `claude auth status`. L'accesso avviene nel Terminale con `claude auth login`, dal pulsante
"Accedi" del gate iniziale. Dati in `~/Library/Application Support/dev.aitaskmanager.desktop`, worktree in
`~/.ai-task-manager/worktrees`.

```bash
cargo tauri dev                                                          # con trunk serve (CSP non applicata)
cargo tauri build --debug --no-bundle && ./target/debug/ai-task-manager  # asset incorporati, CSP attiva
```

In debug l'app scrive su stderr una riga per ogni `get_env`, senza email né organizzazione, per esempio
`get_env: claude /Users/…/.local/bin/claude version 2.1.283 supported=true auth loggedIn (authMethod claude.ai, subscription max)`,
e all'uscita il ramo che ferma gli agenti (`exit: RunEvent::ExitRequested` per `app.exit` e la chiusura della finestra,
`exit: RunEvent::Exit` per Cmd+Q, Esci dal Dock e logout).

**Con fake-claude (nessuna chiamata API).** `ATM_CLAUDE_PATH` fa usare il doppio del CLI (spec §12.1). L'app usa
comunque la sua cartella dati normale: per un giro isolato c'è l'E2E qui sotto.

```bash
cargo build -p atm-core --bin fake-claude
ATM_CLAUDE_PATH=$PWD/target/debug/fake-claude cargo tauri dev
```

Nel prompt `[fake:NOME]` sceglie lo scenario (`simple`, `approval`, `slow`, `hang`, `hang_ignore`, `crash`, `noinit`,
`big`, `flood`, `control`, `usage_limit`, `auth_fail`, `resolve_merge`, `resume_fail`); il follow-up di "Risolvi con
l'agente" gioca da solo `resolve_merge`, sul target che il prompt dell'app nomina. Variabili: `FAKE_CLAUDE_AUTH=in|out`
(stato di `auth status`), `FAKE_CLAUDE_AUTH_FILE` (file con `in`/`out` che la vince sulla variabile e che `auth_fail`
porta a `out`, come il CLI vero dopo un login scaduto), `FAKE_CLAUDE_SCENARIO` (scenario di default),
`FAKE_CLAUDE_TARGET` (branch di `resolve_merge`, altrimenti quello del prompt), `FAKE_CLAUDE_DELTA_MS` (pausa tra i
delta di un testo in streaming, default 0), `FAKE_CLAUDE_RECORD` (una riga JSON per chiamata: argv, cwd, pid,
variabili presenti).

## Build

```bash
(cd ui && trunk build --release)             # solo frontend → ui/dist
cargo tauri build --debug --no-bundle        # → target/debug/ai-task-manager (asset incorporati, CSP attiva)
```

## Selftest (solo build di debug)

```bash
cargo build -p atm-core --bin fake-claude   # il selftest usa fake-claude, mai il claude reale
cargo tauri build --debug --no-bundle
out=$(ATM_SELFTEST=1 ./target/debug/ai-task-manager) && echo "$out" && grep -qF '"csp_violations":0' <<<"$out"
```

La UI esegue da sola le prove IPC (`debug_ping`, errore tipizzato, 50 messaggi su `Channel` di cui tre da 20 KiB),
guida il dialog portato, prova `subscribe_transcript`/unsubscribe e un reload della pagina (`transcript_subscribe_ok`,
`forwarder_unsub_ok`, `forwarder_reload_ok`, `reload_ok`) e conta le violazioni CSP; l'app stampa il report JSON su stdout ed esce con 0 se tutto passa,
altrimenti con 1 (anche se la UI non risponde entro 90 s). Funziona anche con `ATM_SELFTEST=1 cargo tauri dev`,
ma lì `csp_enforced` vale `null`: Tauri applica la CSP solo agli asset incorporati, non alla pagina di `trunk serve`.
In selftest il plugin single-instance non viene registrato, così la prova gira anche con un'altra istanza aperta;
per lo stesso motivo il Core usa una cartella dati privata (`$TMPDIR/atm-selftest-<pid>`, cancellata all'uscita)
e non tocca mai il DB né gli agenti dell'app aperta. Il controllo su stdout esclude le uscite con 0 senza report
(per esempio la finestra chiusa a mano).

## E2E in-app (solo build di debug, M4)

```bash
scripts/e2e.sh                  # build di fake-claude e del bundle di debug, poi le due fasi (al massimo 10 minuti)
scripts/e2e.sh --no-build       # riusa target/debug
scripts/e2e.sh --keep           # conserva log e dati anche se passa
scripts/e2e.sh --gatekeeper     # solo la verifica Gatekeeper dello script di login: apre UNA finestra del Terminale
```

Con `ATM_E2E=1` la UI guida il DOM reale del WKWebView (click, `input`/`change`, tasti, eventi HTML5 di drag con
`DataTransfer`) ed esegue in ordine tutto il percorso del §12.2 su fake-claude. Tutto sta in una cartella temporanea
(`ATM_E2E_DIR`): DB, cache, `HOME` (quindi i worktree), i repository di prova creati dal backend (uno con un commit,
una cartella non git, un repo bare, uno vuoto, uno con `.mcp.json`), il record e lo stato di login di fake-claude. Il
selettore nativo di cartelle non è automatizzabile: il backend restituisce il percorso messo in coda dalla prova
(`debug_e2e_queue_pick`), mentre il click su "Aggiungi repository", i toast e la sidebar sono quelli veri. "Accedi"
scrive davvero `claude-login.command`, ma nel giro normale l'`open -a Terminal` viene solo registrato
(`CoreConfig::open_log`, ignorato nelle build di release) e la prova esegue lo script con `/bin/sh`: il Terminale vero
lo apre solo `--gatekeeper` (sotto).

- **Fase 1:** passi 1–7 (il passo 2 si svolge dentro il gate del passo 1), con due reload della pagina: l'ordine del
  passo 4 riletto dal DB e la verifica che Cmd+R faccia ripartire la subscription del transcript (1 forwarder prima
  del reload, 0 dopo, 1 alla riapertura, vista ripristinata). Poi un task `[fake:hang_ignore]` (ignora interrupt, EOF
  e SIGTERM e ha un nipote `sleep 300`: solo il SIGKILL di gruppo dello shutdown lo ferma) e il follow-up
  `[fake:hang]` di T1, e l'uscita con il percorso di Cmd+Q: `NSApp terminate:` (quello che manda la voce Esci del menu)
  → `applicationWillTerminate:` → `RunEvent::Exit` → `Core::shutdown` sul main thread.
- **Tra le fasi:** lo script verifica dal log che l'uscita sia passata solo da `RunEvent::Exit`, che non resti
  nessun agente del giro e che nel DB entrambi i turni siano `killed/app_shutdown`.
- **Fase 2:** l'app riparte sugli stessi dati: l'ordine del passo 4 dopo il riavvio vero (DOM e DB), "Interrotto –
  Continua" su entrambe le card con la Notice dello shutdown e `--resume`, passi 9–12, i messaggi `Channel` oltre 8 KiB
  (`debug_channel_probe` e un turno `[fake:big]`) e la CSP applicata (`eval` bloccato). Infine un altro turno
  `[fake:hang_ignore]` e l'uscita del report con `app.exit` (l'altro ramo: `ExitRequested` → `Core::shutdown`);
  lo script ricontrolla ramo di uscita, processi e DB.

Gli agenti del giro sono i pid che fake-claude scrive nel proprio record (chiamate `-p` e nipoti `sleep`), ricontrollati
con `ps`: processi fake-claude di altri test o di un'altra istanza non contano e non vengono mai uccisi; con Ctrl-C lo
script chiude app e agenti del giro. Il backend conta i comandi IPC falliti: sono ammessi solo i tre `add_project`
rifiutati del passo 3 (per esempio nessun `get_diff` su un tentativo appena mergiato).

Il report JSON finale (`step_1`…`step_12`, `channel_big_ok`, `reload_resubscribe_ok`, `csp_enforced`,
`command_failures_phase1`/`_phase2`, `exit_requested_armed`, `csp_violations` sommate su tutti i caricamenti di pagina,
`details` con cosa è stato verificato o perché è fallito) va su stdout; lo script esce con 0 solo se tutto è vero,
`csp_violations` è 0 e i controlli su processi e DB passano. Se fallisce, la cartella temporanea resta con log
(`1.err`, `2.err`) e dati. L'E2E non registra il plugin single-instance, quindi gira anche con un'altra istanza aperta,
e non esegue mai il claude reale: senza `target/debug/fake-claude` si ferma subito. `ATM_E2E=1` è ignorato insieme a
`ATM_SELFTEST=1`.

Restano fuori dall'automazione: il selettore nativo di cartelle (sopra), il drag nativo col mouse (il DnD usa
`DragEvent` sintetici con `DataTransfer` sul DOM vero) e, nel giro normale, l'apertura del Terminale. Il driver della UI
(`ui/src/e2e.rs`) è compilato anche nel WASM di release, come quello del selftest: all'avvio chiede
`debug_e2e_setup`, che in release non esiste, e non fa altro.

`--gatekeeper` usa il codice del login sulla vera `app_cache_dir` (`~/Library/Caches/dev.aitaskmanager.desktop`, in una
sottocartella `e2e-gatekeeper`, così uno script di login vero non viene toccato): `write_login_script` scrive
`claude-login.command` e `open_login_terminal` lo apre con `open -a Terminal`. Il suo `claude` è un wrapper nella
cartella temporanea che scrive i propri argomenti in un marker e poi esegue fake-claude: la prova passa se il marker
dice `auth login` (Gatekeeper ha lasciato girare lo script) e lo script non ha l'attributo di quarantena; poi cancella
la sottocartella.

## Fallback di wasm-opt

`ui/index.html` passa a wasm-opt `-Oz --enable-bulk-memory --enable-nontrapping-float-to-int` (verificato in M0 con
wasm-opt `version_123`). Se wasm-opt fallisce, sostituire l'attributo con `data-wasm-opt="0"` e togliere
`data-wasm-opt-params`: il WASM resta più grande ma funziona uguale.

## Componenti Rust/UI

Copiati a mano da `rust-ui/leptos-ui` in `ui/src/ui/` e `ui/src/hooks/` (`ui add` non funziona): commit, checksum e
modifiche in [`ui/src/ui/VENDORED.toml`](ui/src/ui/VENDORED.toml). I file portati hanno un'intestazione `ATM-PORT`;
gli altri restano identici all'upstream e sono esclusi da rustfmt.

## Voci verificate (spec §13.3)

| Voce | Esito | Milestone |
|---|---|---|
| Trunk 0.21.14 compila su Rust 1.97.1 | confermato | M0 |
| Flag wasm-opt (`z` + bulk-memory, nontrapping-float-to-int) | confermato | M0 |
| Download di Tailwind 4.3.3 via `[tools]` | confermato | M0 |
| Binding WASM (`js_namespace`, `async` + `catch`) compilano | confermato | M0 |
| Costruttore `Channel` con `constructor` + `js_namespace` | confermato, fallback `Reflect` non necessario | M0 |
| CSP attiva sugli asset incorporati, 0 violazioni | confermato | M0 |
| Componenti Rust/UI su stable | confermato; `button.rs` senza `support_aria_current` (richiede leptos_router) | M0 |
| Flag di `cargo tauri init` | confermati: `--ci -d -A -W -D -P --before-dev-command --before-build-command` | M0 |
| `atm-ui` compila (e fa `cargo test`) per l'host | confermato | M0 |
| DnD HTML5 su WKWebView con `dragDropEnabled:false` | confermato con `DragEvent` sintetici e `DataTransfer` (riordino e cambio colonna, E2E passo 4); il drag nativo col mouse non è automatizzato | M4 |
| Messaggi `Channel` oltre 8 KiB | confermato: 3 da 20 KiB e un'entry di output tool da 8 KiB intatti | M4 |
| Reload (Cmd+R): forwarder a 0, subscription e vista ripristinate | confermato | M4 |
| 0 violazioni CSP durante tutto il percorso utente | confermato | M4 |
| `.command` in `app_cache_dir` aperto con `open -a Terminal` (Gatekeeper) | confermato: nessun attributo di quarantena, eseguito in meno di 1 s (macOS 27) | M4 |
| Uscita durante un turno: nessun agente residuo (neppure `hang_ignore` e il suo nipote), turni `killed/app_shutdown`, "Continua" con `--resume` | confermato sia con Cmd+Q (`NSApp terminate:` → `RunEvent::Exit`) sia con `app.exit` (`ExitRequested`) | M4 |
