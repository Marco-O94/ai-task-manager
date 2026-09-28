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

**Con fake-claude (nessuna chiamata API).** `ATM_CLAUDE_PATH` fa usare il doppio del CLI (spec §12.1). Quando è
impostata è vincolante: se il binario indicato non risponde come Claude Code (manca, o `--version` supera i 5 s),
l'app dice "Claude Code non trovato" invece di passare al candidato successivo, che sarebbe il CLI reale. Solo
l'impostazione "Percorso di claude" le passa davanti. L'app usa comunque la sua cartella dati normale: per un giro
isolato c'è l'E2E qui sotto.

```bash
cargo build -p atm-core --bin fake-claude
ATM_CLAUDE_PATH=$PWD/target/debug/fake-claude cargo tauri dev
```

Nel prompt `[fake:NOME]` sceglie lo scenario (`simple`, `append`, `approval`, `slow`, `hang`, `hang_ignore`, `crash`,
`noinit`, `big`, `flood`, `control`, `usage_limit`, `auth_fail`, `resolve_merge`, `resume_fail`, `background`; `append`
aggiunge a `hello.txt` la prima riga del messaggio, così due task sullo stesso file vanno in conflitto; `background`
lascia un `sleep 300` in un process group suo, che l'app chiude a fine turno); il follow-up di "Risolvi
con l'agente" gioca da solo `resolve_merge`, sul target che il prompt dell'app nomina. Variabili:
`FAKE_CLAUDE_AUTH=in|out` (stato di `auth status`), `FAKE_CLAUDE_AUTH_FILE` (file con `in`/`out` che la vince sulla
variabile e che `auth_fail` porta a `out` con una scrittura atomica, come il CLI vero dopo un login scaduto),
`FAKE_CLAUDE_SCENARIO` (scenario di default),
`FAKE_CLAUDE_TARGET` (branch di `resolve_merge`, altrimenti quello del prompt), `FAKE_CLAUDE_DELTA_MS` (pausa tra i
delta di un testo in streaming, default 0), `FAKE_CLAUDE_RECORD` (una riga JSON per chiamata: argv, cwd, pid,
variabili presenti; poi una per ogni messaggio utente con lo scenario giocato, e le risposte dell'host).

## Validazione con il CLI reale (M5, opt-in)

L'harness `crates/atm-core/tests/real_cli.rs` guida il Core contro il `claude` installato (`~/.local/bin/claude`,
oppure `ATM_REAL_CLAUDE_PATH`) e consuma quota dell'abbonamento: non fa parte di `scripts/check.sh` e senza
`ATM_REAL_CLAUDE=1` i suoi test tornano subito.

```bash
ATM_REAL_CLAUDE=1 cargo test -p atm-core --test real_cli -- --ignored --test-threads=1
```

- **Solo abbonamento.** Tre condizioni.
  1. Prima di ogni `claude -p`: nessuna tra `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`,
     `CLAUDE_CODE_USE_BEDROCK/VERTEX/FOUNDRY`, `ANTHROPIC_BASE_URL` nell'ambiente, e `claude auth status` deve dire
     `loggedIn`, `authMethod: "claude.ai"`, `apiProvider: "firstParty"`.
  2. Prima di ogni spawn che porta un messaggio utente, un preflight gratuito con lo stesso argv (sessione nuova) e
     lo stesso ambiente: solo `initialize`, stdin chiuso alla risposta, quindi nessuna richiesta all'API. L'`account`
     della risposta deve avere `apiProvider: "firstParty"`, un `subscriptionType` non vuoto e nessun `apiKeySource` o
     `tokenSource` diverso da `none`. Così conta anche la configurazione dei settings dell'utente (`env`,
     `apiKeyHelper`), che il controllo dell'ambiente non vede. Resta da verificare (serve un giro del CLI con una
     chiave fittizia) quanto una chiave nei settings cambi davvero questi campi.
  3. Durante il turno: il primo `system/init` deve avere `apiKeySource: "none"` e nessun output del modello può
     arrivare prima; altrimenti il turno viene ucciso. Questa condizione reagisce soltanto: il CLI manda
     `system/init` insieme alla sua prima richiesta, che quindi non può impedire.

  Ogni scatto della guardia è permanente: gli altri test del giro si rifiutano di partire, e così ogni giro successivo
  finché non si cancella a mano il marker `target/tmp/real_cli.TRIPPED` (dopo aver sistemato la configurazione). I
  test si serializzano su un lock anche senza `--test-threads=1`. Le variabili `CLAUDE*` del processo padre (per
  esempio una sessione di Claude Code) vengono tolte, tranne `CLAUDE_CONFIG_DIR`.
- **Approvazioni.** Il modello reale gira su questa macchina: ogni passo approva solo i comandi che il suo prompt
  nomina (`ls`, `touch approved.txt`, l'attesa `python3 -c 'import time; time.sleep(4x)'`, la lettura di
  `CLAUDE.md`); qualunque altra richiesta riceve "Nega e ferma" e fa fallire il run.
- **Quota.** `sonnet`, effort `low`, prompt minimi, nessun retry: 8 turni reali (probe 1, checklist 5, push 1,
  isolamento 1), circa 0,15–0,25 USD l'uno nella stima del CLI.
- **Repo.** Ogni test lavora in una cartella temporanea, cancellata alla fine: un clone locale del repo giocattolo
  (`ATM_REAL_CLAUDE_REPO`, default `~/Desktop/Repositories/test-rust`, mai modificato: anche i suoi `git status`
  girano con `--no-optional-locks`, che non riscrive l'indice; l'`origin` del clone diventa un repo bare usa e getta)
  o un repo creato sul momento. Il test verifica che il repo giocattolo non cambi e che nessun processo del giro resti
  vivo.
- **Catture.** Log grezzi ripuliti (email, organizzazione, home, utente, host e percorsi temporanei sostituiti; output
  degli hook dell'utente oscurati) e osservazioni in `ATM_REAL_CLAUDE_CAPTURE` (default `target/tmp/real_cli`). Quelle
  di M5 sono in `crates/atm-core/tests/fixtures/real/` e `tests/real_fixtures.rs` le ripete senza CLI (golden
  insta). Per rigenerarle dopo un aggiornamento del CLI: `ATM_REAL_CLAUDE_CAPTURE=$PWD/crates/atm-core/tests/fixtures/real`,
  poi `cargo insta review`. `scripts/check.sh` fallisce se nelle fixture compaiono un'email, un percorso home o
  temporaneo (anche nella forma con i trattini del CLI), una chiave `sk-ant-` o un valore di `email`/`organization`
  diverso da `<redacted>`.

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
scripts/e2e.sh                  # build (--locked) di fake-claude e del bundle di debug, poi le due fasi (max 10 min)
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

- **Fase 1:** passi 1–7 (il passo 2 si svolge dentro il gate del passo 1), con due reload nativi della pagina
  (`-[WKWebView reload]`, come "Ricarica" nel menu contestuale del WebView di debug: l'app non ha una scorciatoia
  Cmd+R): l'ordine del passo 4 riletto dal DB e la verifica che il reload faccia ripartire la subscription del
  transcript (1 forwarder prima del reload, 0 dopo, 1 alla riapertura, vista ripristinata). Il passo 4 trascina quattro
  card fino a `[T3, T6, T2, T1]` (create T1, T2, T3, T6); ogni `dragover` deve essere annullato (senza, WebKit non
  darebbe il `drop`) e vede un `DataTransfer` senza dati, come nella modalità protetta di un drag vero. Il passo 7
  ferma anche un `[fake:hang_ignore]`: lo Stop percorre tutta la sequenza del §7.9 (interrupt 5 s, EOF 3 s, SIGTERM
  3 s, SIGKILL del gruppo) e deve chiudersi tra 10 e 13 s senza lasciare processi. Poi un task `[fake:hang_ignore]`
  (ignora interrupt, EOF e SIGTERM e ha un nipote `sleep 300`: solo il SIGKILL di gruppo dello shutdown lo ferma), il
  follow-up `[fake:hang]` di T1 e l'uscita con Cmd+Q: un ⌘Q (key-down e key-up, sorgente HID) mandato al pid dell'app
  attraverso il window server con `CGEventPostToPid` → finestra, poi voce Esci del menu di Tauri → `NSApp terminate:`
  → `applicationWillTerminate:` → `RunEvent::Exit` → `Core::shutdown` sul main thread. Serve che il terminale che
  lancia lo script abbia l'accesso Accessibilità (per inviare eventi); senza, il ⌘Q è un `NSEvent` dato a
  `-[NSApplication sendEvent:]` dentro l'app, e `details.cmd_q` del report dice quale via è stata usata. Se l'app non
  esce entro 30 s la fase fallisce (con ⌘X al posto di ⌘Q fallisce davvero: il test non passa per caso).
- **Tra le fasi:** lo script verifica dal log che l'uscita sia passata solo da `RunEvent::Exit`, che non resti
  nessun processo del giro e che nel DB lo Stop del passo 7 sia `killed/user_stop` e i due turni dell'uscita
  `killed/app_shutdown`.
- **Fase 2:** l'app riparte sugli stessi dati: l'ordine `[T3, T6, T2]` del passo 4 dopo il riavvio vero (DOM e DB,
  posizioni invariate), "Interrotto – Continua" su entrambe le card con la Notice dello shutdown, e Continua su tutte e
  due (`--resume` della sessione di ciascuna, turno completato), passi 9–12, i messaggi `Channel` oltre 8 KiB
  (`debug_channel_probe` e un turno `[fake:big]`) e la CSP applicata (`eval` bloccato). Al passo 10 T6 e T3 aggiungono
  righe diverse a `hello.txt` (`[fake:append]`); T6 viene mergiato, così T3 va in conflitto con `main` e "Risolvi con
  l'agente" manda il prompt dell'app (che non ha tag): fake-claude registra di aver giocato `resolve_merge`. Infine un
  altro turno `[fake:hang_ignore]` e l'uscita del report con `app.exit` (l'altro ramo: `ExitRequested` →
  `Core::shutdown`); lo script ricontrolla ramo di uscita, processi e DB.

L'app gira con `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT` e `GIT_DIR` impostati
(valori finti): `child_env_scrubbed` verifica nel record di fake-claude che nessun agente dei due giri li abbia avuti.

I processi del giro sono i pid che fake-claude scrive nel proprio record (chiamate `-p` e nipoti `sleep`),
ricontrollati con `ps`, più qualunque processo della macchina, registrato o no, che lavora dentro la cartella del giro
(trovato con `lsof`: gli agenti girano nei suoi worktree). Dentro l'app contano solo fake-claude `-p` e `sleep 300`; tra le fasi e
alla fine lo script conta qualunque programma. Processi fake-claude di altri test o di un'altra istanza non contano e
non vengono mai uccisi; con Ctrl-C lo script chiude app e processi del giro. Il backend conta i comandi IPC falliti: sono
ammessi solo i tre `add_project` rifiutati del passo 3 (per esempio nessun `get_diff` su un tentativo appena mergiato).

Il report JSON finale (`step_1`…`step_12`, `channel_big_ok`, `reload_resubscribe_ok`, `csp_enforced`,
`command_failures_phase1`/`_phase2`, `exit_requested_armed`, `csp_violations` sommate su tutti i caricamenti di pagina,
`child_env_scrubbed`, `details` con cosa è stato verificato o perché è fallito) è l'unica cosa su stdout (i log di build
vanno su stderr); lo script esce con 0 solo se tutto è vero,
`csp_violations` è 0 e i controlli su processi e DB passano. Se fallisce, la cartella temporanea resta con log
(`1.err`, `2.err`) e dati. L'E2E non registra il plugin single-instance, quindi gira anche con un'altra istanza aperta,
e non esegue mai il claude reale: il binario indicato (`ATM_CLAUDE_PATH`, risolto nei symlink) deve chiamarsi
`fake-claude` e contenere il messaggio di login di fake-claude (letto, mai eseguito), altrimenti si ferma subito.
`debug_e2e_git` accetta solo i controlli di sola lettura della UI (`show`, `log`, `status`, `branch --list`,
`worktree list`, nessuna opzione prima del sottocomando) e i comandi sui file rifiutano i symlink pendenti.
`ATM_E2E=1` è ignorato insieme a `ATM_SELFTEST=1`.

Restano fuori dall'automazione: il selettore nativo di cartelle (sopra), il drag nativo col mouse (il DnD usa
`DragEvent` sintetici con `DataTransfer` sul DOM vero: una sessione di drag nativa segue il cursore reale e
spostarlo disturberebbe chi usa il Mac; `effectAllowed` non si può verificare perché WebKit lo ignora su un
`DataTransfer` costruito), il Cmd+R (non esiste) e, nel giro normale, l'apertura del Terminale (coperta da
`--gatekeeper`, da lanciare a parte perché apre una finestra). Il driver della UI (`ui/src/e2e.rs`) è compilato anche
nel WASM di release, come quello del selftest: all'avvio chiede `debug_e2e_setup`, che in release non esiste, e non fa
altro (i comandi di debug esistono solo nelle build di debug); toglierli dal WASM di release è rimandato a M6.

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
| DnD HTML5 su WKWebView con `dragDropEnabled:false` | confermato con `DragEvent` sintetici e `DataTransfer` (riordino di 4 card e cambio colonna, E2E passo 4; `dragover` annullato con `DataTransfer` senza dati); il drag nativo col mouse non è automatizzato | M4 |
| Messaggi `Channel` oltre 8 KiB | confermato: 3 da 20 KiB e un'entry di output tool da 8 KiB intatti | M4 |
| Reload della pagina: forwarder a 0, subscription e vista ripristinate | confermato con `-[WKWebView reload]` (il reload del menu contestuale in debug; il menu di Tauri non ha Cmd+R) | M4 |
| 0 violazioni CSP durante tutto il percorso utente | confermato | M4 |
| `.command` in `app_cache_dir` aperto con `open -a Terminal` (Gatekeeper) | confermato con `scripts/e2e.sh --gatekeeper` (2026-09-28): nessun attributo di quarantena, `claude auth login` eseguito dal Terminale dopo 684 ms (macOS 27) | M4 |
| Uscita durante un turno: nessun agente residuo (neppure `hang_ignore` e il suo nipote), turni `killed/app_shutdown`, "Continua" con `--resume` | confermato sia con Cmd+Q (⌘Q inviato al pid con `CGEventPostToPid` → voce Esci → `NSApp terminate:` → `RunEvent::Exit`; con ⌘X la fase fallisce) sia con `app.exit` (`ExitRequested`) | M4 |
| Stop di un turno che ignora interrupt, EOF e SIGTERM | confermato: `killed/user_stop` dopo ~11 s (5 + 3 + 3 s, poi SIGKILL del gruppo), nipote compreso | M4 |
| `--verbose` obbligatorio con `-p` e `stream-json` | confermato: senza, exit 1 ("requires --verbose") | M5 |
| Ordine e necessità di `initialize` | non necessario (mantenuto); risposta dopo l'avvio degli hook `SessionStart` dell'utente e prima di `system/init`, che arriva solo col primo messaggio; porta `account` con email e organizzazione, oscurate nel raw log (fix) | M5 |
| Subtype di `result` dopo un interrupt | `error_during_execution`, `is_error`, `errors: ["[ede_diagnostic] …"]`, exit 1; risposta `{"still_queued":[]}`; stop in 0,4–0,7 s | M5 |
| Valori di `apiKeySource` | `none` con il login claude.ai (abbonamento Max); nessun altro valore osservato | M5 |
| Eventi di rate limit / usage limit | `rate_limit_event` (`status`, `rateLimitType`, `resetsAt`, `unifiedWindows` a 5 h e 7 giorni) a ogni richiesta, senza entry; nessun limite raggiunto, `LIMIT_PATTERNS` invariati | M5 |
| Deny di `git push` via `--settings` | confermato: nessuna richiesta, `permission_denials` nel `result`, remote intatto | M5 |
| `--strict-mcp-config` senza `--mcp-config` | confermato: 0 server (anche con server MCP e plugin dell'utente e un `.mcp.json` di progetto) | M5 |
| `--setting-sources=user` esclude hook di progetto e CLAUDE.md | confermato (controllo positivo con `user,project`); E11: CLAUDE.md non è nel contesto, l'append prompt lo fa leggere e l'agente conosce la parola chiave; gli hook dell'utente girano | M5 |
| Regole `session` dopo `--resume` | non sopravvivono: il re-pass via `--settings` serve e funziona anche in una sessione nuova | M5 |
| Directory del progetto nel CLI | realpath (non `$PWD`), caratteri non alfanumerici → `-` | M5 |
| Forma di `decision_reason` | stringa (`"This command requires approval"`) con `decision_reason_type`; assente se c'è `blocked_path` | M5 |
| `RESUME_FAILED_PATTERN` | confermato: "No conversation found with session ID: …" su stderr e negli `errors` del `result` | M5 |
| "Consenti sempre" con le suggestion reali | fix: il CLI aggiunge sempre `addDirectories` e `setMode`; `can_remember` ora considera (e inoltra) solo le `addRules`/`allow` | M5 |
| Processi dei comandi Bash | fix: il CLI li mette in process group propri; il Core registra i discendenti del leader mentre vive (al `result` e a ogni gradino dello stop) e a fine turno chiude quelli rimasti, anche quando il CLI esce da solo; sui percorsi con SIGKILL li uccide con il leader | M5 |
| `acceptEdits` e Bash | il CLI approva da solo i comandi che leggono o scrivono file nel cwd (non "Bash chiede sempre"); in Supervisionato `ls` passa, `touch` e `python3` chiedono, un `sleep N` isolato è bloccato dal CLI | M5 |
