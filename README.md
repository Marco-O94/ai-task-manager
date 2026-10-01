# AI Task Manager

App desktop per macOS: una kanban di task affidati al Claude Code CLI installato sul Mac, un git worktree per ogni tentativo.
Stack: Tauri 2 + Leptos 0.8 (CSR, WASM) + componenti Rust/UI. Piano completo: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

Ogni progetto ha una pagina con tre tab: **Riepilogo** (descrizione, stato e la configurazione Claude che il repository
dà agli agenti), **Task** (kanban o lista, con il pannello del task) e **Impostazioni**. Ai task si allegano file, e per
ogni tentativo si può limitare il numero di sub-agent e sceglierne il modello: vedi
[La pagina del progetto](#la-pagina-del-progetto).

## Usa il Claude Code installato sul tuo Mac

L'app non contiene un modello né un client dell'API: guida il `claude` già installato sul Mac, non modificato, con il
suo login. Per conto suo esegue solo `claude --version` e `claude auth status`; gli agenti sono turni
`claude -p … --output-format stream-json` lanciati quando avvii un task, uno per worktree (2 in parallelo di default).

- **Login con l'abbonamento, nel Terminale.** Se `auth status` dice che non sei collegato, l'app mostra il gate
  iniziale: "Accedi" apre Terminal.app con `claude auth login` (account claude.ai; anche Console o SSO), poi
  "Ricontrolla". Il login avviene solo lì: l'app non legge mai il Portachiavi, `~/.claude/.credentials.json` o
  `~/.claude/projects`, non salva token, email od organizzazione e non fa da proxy.
- **Nessuna chiave API di default.** `ANTHROPIC_API_KEY` e `ANTHROPIC_AUTH_TOKEN` vengono tolti dall'ambiente degli
  agenti, che usano quindi il login dell'abbonamento. «Passa agli agenti la chiave API dell'ambiente», nel dialog
  Impostazioni app, li lascia passare dopo una conferma nativa; finché è attivo e una chiave è presente, un banner nella
  topbar dice che l'uso è fatturato via API. Lo stesso banner compare con un login diverso da claude.ai e con
  `CLAUDE_CODE_USE_BEDROCK/_VERTEX/_FOUNDRY` nell'ambiente. `ANTHROPIC_BASE_URL` nell'ambiente dell'app (o l'endpoint
  di un provider cloud) resta, perché è una tua scelta, ma un banner nella topbar dice che gli agenti mandano le
  richieste, con le credenziali dell'abbonamento, a quell'endpoint invece che ad Anthropic, e che lì l'uso può essere
  fatturato: `apiKeySource` resta `none`, quindi lo stop qui sotto non lo vede. Il costo mostrato nei turni è la
  stima del CLI.
- **Solo abbonamento, anche se una chiave arriva da altrove.** Con il passthrough spento, un turno il cui `system/init`
  dice che Claude Code usa una chiave API (`apiKeySource` diverso da `none`: una variabile, un `apiKeyHelper` o un
  `env` nelle impostazioni dell'utente, che l'app non legge) viene fermato subito: gruppo di processi congelato e
  ucciso, turno `failed` con una Notice che nomina la fonte. È una reazione: il CLI manda `system/init` appena prima
  della sua prima richiesta, quindi quella viene fermata prima di partire solo se il kill vince la corsa, altrimenti
  viene interrotta a metà. La configurazione del repository non può spostare la fatturazione (sotto, Attendibile).
- **Dove cerca `claude` e git.** Impostazione "Percorso di claude" (cambiarla chiede una conferma nativa: l'app esegue
  subito quel programma; deve essere un percorso assoluto fuori dai progetti e dai worktree), poi `ATM_CLAUDE_PATH`, poi `~/.local/bin/claude`,
  `~/.claude/local/claude`, Homebrew e il `PATH` della login shell, importato una volta con `$SHELL -ilc` (un'app
  aperta dal Finder ha il `PATH` minimo di launchd, non quello del Terminale); git è il primo sul `PATH` importato,
  altrimenti `/usr/bin/git`. Il dialog Impostazioni app dice quali sta usando, per esempio "In uso: Claude Code 2.1.284
  (/Users/…/.local/bin/claude); git 2.54.0.", e lo stderr dell'app (anche in release, `open --stderr <file>`) ha una
  riga per ogni `get_env`, senza email né organizzazione:
  `get_env: claude /Users/…/.local/bin/claude version 2.1.284 supported=true auth loggedIn (authMethod claude.ai, subscription max) git /opt/homebrew/bin/git version 2.54.0`.
- **Avviare l'app da dentro una sessione di Claude Code**, o da un terminale cmux o di un IDE (per esempio
  `cargo tauri dev` nel terminale della sessione): le variabili della sessione padre (`CLAUDECODE`,
  `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED`,
  `CLAUDE_CODE_EXECPATH`, `CLAUDE_PID`, `CLAUDE_EFFORT`, `CLAUDE_CODE_MESSAGING_*`) e del suo host (`CMUX_*`,
  `CLAUDE_CODE_SSE_PORT`, `ENABLE_IDE_INTEGRATION`) non arrivano agli agenti né a git, e l'app all'avvio si
  ri-esegue senza di esse, così nemmeno `ps -E` dell'app le mostra. Lo stesso vale per il `NODE_OPTIONS` con cui un
  terminale cmux fa caricare il suo modulo (`--require=…`) a ogni programma node, compreso `claude`: con
  `CMUX_ORIGINAL_NODE_OPTIONS_PRESENT=0` (l'utente non ne aveva) viene tolto, con `=1` torna al valore salvato in
  `CMUX_ORIGINAL_NODE_OPTIONS`, senza il marker resta com'è. Però la shell o la sessione che ha lanciato l'app,
  finché restano aperte, le hanno ancora nel loro ambiente, e ogni processo dello stesso utente (quindi anche un
  agente, con `ps -E`) può leggerle, compreso il socket e il token del canale di messaggi della sessione padre: per
  lavorare davvero con agenti, lancia l'app da un terminale pulito o dal Finder. Restano `CLAUDE_CONFIG_DIR`, gli
  altri `CLAUDE_CODE_*` e il token OAuth. `CLAUDE_CODE_USE_BEDROCK/_VERTEX/_FOUNDRY` restano anche loro e fanno
  comparire il banner di fatturazione. Le chiavi API, se presenti, restano nell'ambiente dell'app (servono al
  passthrough opzionale) e sono quindi leggibili allo stesso modo: non arrivano agli agenti se non le abiliti.

### Modalità di permesso

Si scelgono nel dialog Avvia (il default del progetto sta nel suo tab Impostazioni). Dati di M5, CLI 2.1.283:

| Modalità | CLI | Cosa chiede |
|---|---|---|
| Supervisionato | `--permission-mode=default` | Chiede per ogni modifica e comando; passano da sole solo le letture, per esempio `ls` |
| **Auto-edit** (default) | `acceptEdits` | Approva da solo le modifiche ai file e i comandi shell che leggono o scrivono file nel worktree (per esempio `printf … >> README.md`) e chiede per il resto |
| Autonomo | `bypassPermissions` + `--allow-dangerously-skip-permissions` | Non chiede mai. Si abilita per progetto (tab Impostazioni del progetto, Sicurezza) con una conferma nativa; toglierlo riporta a Auto-edit la modalità predefinita |

Le richieste compaiono come card nel transcript: "Approva", "Approva sempre" (per il tentativo) quando il CLI propone
una regola, "Rifiuta", "Rifiuta e ferma". In ogni modalità l'app passa via `--settings` regole deny per `git push`, `~/.ssh`,
`~/.aws` e i file di credenziali: una chiamata negata da una regola compare come Negato.

### Configurazione Claude del repository: Isolata o Attendibile

Isolata (default) passa `--setting-sources=user --strict-mcp-config`, quindi `.claude/`, `.mcp.json`, hook, `env`,
`apiKeyHelper`, server MCP e regole di permesso del repo non vengono caricati (le impostazioni e gli hook
dell'**utente** sì; `CLAUDE.md` l'agente lo legge perché il prompt dell'app glielo chiede). Attendibile si attiva dal
tab Impostazioni del progetto → Sicurezza → Applica, dopo una conferma nativa che nomina il repository per percorso,
il branch e il commit approvati, e avvisa se le sue regole `permissions.allow` consentono un tool intero (per esempio
`Bash(*)`: comandi senza chiedere in ogni modalità, di fatto un Autonomo per Bash senza l'opt-in).

**Cosa si approva: il commit, non la cartella.** L'approvazione è lo SHA-256 di `.claude/**`, `.mcp.json` e dei file
del repository che i loro comandi eseguono (lo script di un hook, il `tools/mcp.js` di un server MCP) **com'erano
committati nell'ultimo commit del branch target predefinito** del progetto, quello da cui partono i worktree, letti
dal database di git (`git cat-file` con il runner irrobustito dell'app), così com'erano quando la conferma è comparsa:
se il branch cambia configurazione mentre il dialog è aperto, l'approvazione viene rifiutata. I file locali del
checkout principale (un `.claude/settings.local.json` non committato, modifiche non committate, file ignorati) non
contano e non bloccano più i worktree. Un repository senza configurazione si può approvare (hash vuoto).

**A ogni turno** l'app ricontrolla i file del worktree, e di nuovo quando Claude Code si è avviato: se sono diversi da
quelli approvati il turno gira Isolato (o viene fermato) con una Notice che dice perché. Se è il worktree a essere
cambiato, nomina i file diversi; se è il commit da cui l'attempt è partito a non avere la configurazione approvata
(il branch target è andato avanti con una configurazione nuova dopo l'approvazione), chiede di riapprovare: finché
non lo fai il progetto non risulta più Attendibile nelle sue Impostazioni e gli attempt nuovi girano Isolati. Un commit
che non tocca la configurazione non cambia nulla.

**Mai fatturazione via API.** Una configurazione committata che farebbe fatturare gli agenti via API o da un altro
provider invece che con l'abbonamento non si può approvare: `apiKeyHelper`, `awsAuthRefresh`, `awsCredentialExport`
in `.claude/settings.json` o `.claude/settings.local.json`, oppure nel loro `env` `ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
`CLAUDE_CODE_USE_FOUNDRY` (con qualunque valore), o un file di impostazioni che non è JSON valido. "Applica" risponde
con un errore che nomina la chiave, per esempio "Configurazione Claude non approvabile (branch main, commit abc1234):
.claude/settings.json imposta apiKeyHelper. …". Conta il file che Claude Code apre davvero: sul disco del Mac (APFS,
che non distingue maiuscole, minuscole e forme Unicode) `.claude/Settings.json` è `.claude/settings.json`, quindi nel
worktree viene controllato come tale, e un commit con un nome del genere (anche `.Claude/` o `.MCP.json`) non si può
approvare. Un worktree che acquista una di queste chiavi gira Isolato con una Notice che la nomina, anche se il
progetto era stato approvato prima di questo controllo; se le acquista mentre Claude Code si avvia (un hook), il turno
viene congelato al `system/init`, ricontrollato e ucciso (`failed`, con la Notice); e in ogni caso un turno che
riporta una chiave API viene fermato (sopra).

Revocare Attendibile o l'Autonomo ferma i turni in corso che li usano; annullare la conferma di un aumento applica
comunque la parte che toglie permessi. Mai Attendibile un repo che è la home. Aggiungendo un repo con `.mcp.json` o
`.claude/` l'app lo segnala.

Limiti di Attendibile:
- l'esecuzione indiretta non è coperta: ciò che eseguono `npm run …`, `npx pkg@latest`, `bash -c "cd tools && …"` o
  un hook che lancia strumenti del repository, e i file che questi caricano;
- i link vengono seguiti solo se restano dentro il repository; nel worktree un file eseguito che un link porta fuori
  (il `python` di un virtualenv) conta per il suo percorso, non per il contenuto, e un commit che ne contiene uno non
  si può approvare;
- un file di configurazione che git cambia al checkout (conversione di fine riga, attributo `ident`, filtro smudge
  come Git LFS) è diverso in ogni worktree: i turni girano Isolati con la Notice che lo nomina;
- un attempt con un branch target diverso da quello predefinito è Attendibile solo se il suo commit di partenza ha la
  stessa configurazione approvata.

### Il worktree non è una sandbox

Il worktree NON è una sandbox: agenti, hook e server MCP girano sul Mac con i permessi dell'utente e possono toccare
file e servizi fuori dal worktree. Le regole deny di `--settings` (git push, `~/.ssh`, `~/.aws`, credenziali) sono una
difesa in profondità: `sh -c 'git push'` le aggira. Il worktree separa il lavoro dei task tra loro e dal checkout
principale, non protegge il Mac: per questo il default è Auto-edit, Autonomo è un opt-in per progetto e la
configurazione del repo è Isolata finché non la approvi.

## La pagina del progetto

**Sidebar.** Un clic sul nome apre il progetto sul Riepilogo. Il bottone "⋯" accanto al nome (compare al passaggio del
mouse o col focus) e il clic destro sul nome aprono lo stesso menu: **Impostazioni progetto** e **Rimuovi dalla
lista…**. La rimozione chiede conferma: i file del repository e i branch `atm/…` restano; i worktree dell'app vengono
salvati con un commit sul loro branch e rimossi; task, cronologia (anche i log grezzi) e allegati vengono eliminati
dall'app. Con un agente in esecuzione nel progetto la rimozione è rifiutata ("Ferma gli agenti del progetto prima di
rimuoverlo") e il dialog resta aperto. In fondo alla sidebar ci sono gli agenti in esecuzione nell'app contro il
limite, la versione di Claude Code e il bottone **Impostazioni app** (anche ⌘,): solo le impostazioni generali
(percorso di claude, agenti in parallelo, modello predefinito, chiave API, worktree, editor). Nella topbar, accanto ai
tab, "N in attesa di approvazione" apre il primo task il cui agente aspetta una tua risposta; il tab Task mostra quanti
task ha il progetto.

**Riepilogo** (la pagina su cui si atterra):
- In alto, il progetto: branch target e commit, configurazione Isolata o Attendibile, nome, **descrizione** (si scrive
  nel tab Impostazioni, al massimo 10 000 caratteri), la barra dei task per colonna con i conteggi, gli agenti attivi e,
  se un agente aspetta un'approvazione, un avviso con "Rivedi". Sotto, le card dei file e la **Configurazione agenti**
  (modello e modalità predefiniti).
- **Istruzioni agenti** (`CLAUDE.md`, `.claude/CLAUDE.md`, `AGENTS.md`), **Server MCP** (`.mcp.json`), **`.claude/`**
  (impostazioni, agenti, comandi, skill) e **README**, ciascuno con una nota su come arriva agli agenti: per esempio
  `CLAUDE.md` è "caricato all'avvio" solo in Attendibile, altrimenti "letto dall'agente su istruzione del prompt";
  `AGENTS.md` è sempre letto su istruzione del prompt, il README è "solo contesto per te".
- Tutto viene letto dall'**ultimo commit del branch target predefinito** (da lì partono i worktree ed è quello che
  Attendibile approva), mai dai file del checkout principale: una modifica non committata non compare. Un repository
  che è la tua home non si legge.
- **Segreti mascherati:** in `.claude/settings.json`, a ogni profondità, i valori di `env` e `headers` e degli helper
  di credenziali (ogni chiave che finisce in `Helper`, più `awsAuthRefresh` e `awsCredentialExport`) diventano `•••`,
  e da ogni valore di una chiave `url` si tolgono userinfo, query e fragment; dei server MCP si vedono nome, trasporto,
  comando o URL (senza userinfo, query e fragment) e solo i **nomi** delle variabili e degli header, e il testo di
  `.mcp.json` non compare mai. Un JSON non valido non si mostra. Il mascheramento è euristico: un segreto scritto dentro
  un comando, i suoi argomenti, il path di un URL o un URL sotto una chiave diversa da `url` (o dentro un altro testo)
  si vede.
- Testo semplice, senza markdown; i caratteri invisibili o bidirezionali (quelli che possono nascondere istruzioni)
  compaiono come `⟨U+202E⟩` con un badge. I file oltre 64 KiB, i binari e i symlink (mostrati come "→ target", mai
  seguiti) non si leggono. I server aggiunti con `claude mcp add` stanno in `~/.claude.json`, che l'app non legge mai:
  qui non compaiono. Dei file lunghi si vedono le prime righe, "Mostra tutto" apre il resto.
- "Aggiorna" rilegge (non c'è cache); **Apri task (N)** passa al tab Task (**Crea il primo task** se il progetto non
  ne ha).

**Task: Kanban o Lista.** La toolbar del tab Task passa dalla kanban (trascinamento, creazione rapida in fondo alle
colonne) a una **lista**: una tabella con Titolo, Stato, Agente (gli stessi badge delle card), Branch e Aggiornato, in
ordine di colonna e posizione; il titolo apre il task, la matita lo modifica, "Nuovo task" lo crea in Da fare. Con il
pannello aperto Branch e Aggiornato si nascondono. Niente ricerca, filtri né ordinamento, e la scelta non viene
ricordata (a ogni visita si riparte dalla kanban).

**Sotto task.** Un sotto task è un task completo (stato, agente, worktree e allegati propri) legato a un padre; c'è un
solo livello, quindi un sotto task non ha sotto task. Si crea dal pannello del padre, nella sezione **Sotto task**, con
"Aggiungi sotto task" (il dialog diventa "Nuovo sotto task"), oppure lo crea l'agente del padre (sotto). I sotto task
stanno nelle colonne della kanban come gli altri task: la card del padre mostra "↳ fatti/totali" con una barra
sottile, quella del figlio "↳ titolo del padre". Nella Lista i figli stanno subito sotto il padre, rientrati, e il
toggle "↳ n/m" accanto al titolo del padre li nasconde o li mostra (non ricordato). Il pannello del padre elenca i
figli con stato, titolo (apre il figlio) e badge dell'agente; il pannello di un figlio ha in alto il link al padre. Il
primo prompt di un sotto task (e quello di una "Nuova sessione") riporta titolo e descrizione del padre in una sezione
`## Parent task`, solo come contesto. Lo stato del padre non si aggiorna da solo. **Eliminare un padre elimina anche i
suoi sotto task** (il dialog lo avvisa: "Elimina anche N sotto task."), ciascuno con worktree, allegati e log; se
l'agente del padre o di un sotto task è in esecuzione l'eliminazione è rifiutata e non tocca nulla.

**Tool board per gli agenti.** Ogni agente dei task ha i tool `mcp__atm__*` per gestire la board del suo progetto,
serviti dall'app stessa sulla stdio del CLI (un server MCP "in-process": nessun processo, socket o porta in più). Sono
limitati al progetto del tentativo (un task di un altro progetto è "non trovato"), e `"self"` indica il task
dell'agente. L'autonomia è **"crea libero, il resto chiede"**, in ogni modalità, Autonomo compreso:
- senza chiedere: `list_tasks`, `get_task` e `create_task` (con `parent_id: "self"` crea un sotto task del proprio
  task);
- con la tua approvazione: `update_task` (titolo e descrizione), `move_task` (cambio di colonna; un task con l'agente
  in esecuzione non va in Fatto o Annullati) e `start_task` (avvia l'agente di un task con i default del progetto; un
  sotto task parte nella modalità dell'agente che lo avvia). La card di approvazione mostra una frase leggibile, per
  esempio «Sposta «X» in Fatto», e non offre "Approva sempre": ogni chiamata chiede di nuovo. L'app esegue solo la
  chiamata che hai approvato, anche se il CLI non chiedesse.

`start_task` rispetta "agenti in parallelo" (oltre il limite l'agente riceve un errore e il task non parte; con
l'Autopilota acceso il task va invece in coda, sotto) e le opzioni sub-agent del chiamante; un agente avviato da un
altro agente non può a sua volta avviare agenti.

**Autopilota** (round del 2026-10-01). Un interruttore per progetto, nella topbar («Autopilota», con un puntino indaco
quando è acceso) e nella pagina Impostazioni. Acceso, l'app guida da sola i task che le hai **affidato**: li avvia
quando c'è un agente libero, a fine turno esegue il **comando di verifica**, rimanda all'agente gli errori e, se vuoi,
fa il merge. Ti avvisa solo quando serve.
- **Impostazioni del progetto**, sezione «Autopilota»: «Comando di verifica» (per esempio `cargo test`; vuoto =
  nessuna verifica, il turno completato conta come verificato), «Timeout della verifica» (10–3600 s, default 600),
  «Tentativi di correzione» (0–5, default 2), «Merge automatico se verificato» (spento di default).
- **Il comando di verifica esegue codice del repository** sul tuo Mac, con i tuoi permessi e **senza chiedere**, anche
  in Supervisionato e Auto-edit, dopo ogni turno completato: l'agente può aver modificato proprio i test o gli script
  che il comando lancia. Gira con `/bin/sh -c` nel worktree del task, con lo stesso ambiente ripulito degli agenti, in
  un process group suo che l'app chiude al timeout, alla fine, se scarti o elimini il task e quando esci. Si legge solo
  dalle impostazioni dell'app, mai da file del repository. L'output va nei log del tentativo (0600) e la sua coda nel
  pannello.
- **Affidare un task**: nel dialog del task o nel pannello, «Affida all'autopilota»; «Parte dopo…» lo fa partire solo
  quando un altro task del progetto è Fatto (una sola dipendenza; niente cicli né task annullati). I task affidati
  partono in ordine di board, rispettando "agenti in parallelo" e la pausa per limite d'uso.
- **Dopo un turno completato**: verifica. Se fallisce, l'app manda all'agente comando, codice d'uscita e coda
  dell'output (fino ai tentativi impostati), anche aspettando che si liberi uno slot: una correzione passa prima dei
  task in coda. Se passa: con «Merge automatico» fa lo squash merge del solo commit verificato (se ci sono conflitti li
  rimanda all'agente, contandoli come correzione; se il worktree ha file non committati, per esempio scritti dalla
  verifica, si ferma), altrimenti il task resta In revisione con il badge «Verificato».
- **Non insiste mai**: un turno fallito, uno Stop, un limite d'uso, una verifica che non parte o i tentativi esauriti
  fanno lasciare il task all'autopilota (resta In revisione, con una notifica). Scartare il tentativo riprende il task
  (non riparte da solo).
- **Sulla board**: un'icona robot sui task affidati, «In coda» (il tooltip dice perché aspetta), «Verifica…»,
  «Verificato», «Verifica fallita (1/2)»; nel pannello la riga «Verifica: `comando` · passata · correzioni n/max» con
  l'output collassabile.
- **Agenti**: il sotto task che un agente affidato crea con `parent_id: "self"` è affidato anche lui e parte da solo
  (con `after` in sequenza); uno `start_task` approvato senza slot liberi mette il task in coda invece di dare errore.
  In entrambi i casi parte come se l'avesse avviato quell'agente, quindi non può avviare altri agenti.
- **Notifiche macOS** (Impostazioni app, «Notifiche macOS», accese di default): un'approvazione in attesa su un task
  affidato, pronto per il merge, mergiato, verifica fallita dopo i tentativi, avvio non riuscito, pausa per limite
  d'uso. Al massimo una ogni 30 s per task e tipo.

**Allegati.** Nel dialog del task, "Aggiungi file…" apre il selettore nativo del Mac: file qualsiasi (immagini
comprese), al massimo 20 per task e 25 MB l'uno. L'app ne fa una **copia** in
`~/Library/Application Support/dev.aitaskmanager.desktop/attachments/<progetto>/<task>/…`, fuori dal repository e mai
committata; gli originali non vengono toccati. In creazione i file restano in attesa e si aggiungono subito dopo il
task (se l'aggiunta fallisce il task resta, con l'avviso "Task creato, allegati non aggiunti"); in modifica si
aggiungono e si tolgono subito. Il primo prompt (e quello di una "Nuova sessione") elenca i path delle copie in una
sezione `## Attachments` e chiede di trattarle in sola lettura, e a ogni turno l'agente riceve la cartella del task con
`--add-dir`: le immagini le legge con il suo tool Read. Un file aggiunto a tentativo avviato va citato nel messaggio
successivo. Il pannello del task mostra gli allegati come chip (col path nel tooltip). Per sicurezza la webview non
passa mai un percorso: il selettore consegna i file al backend, che dà alla pagina solo gettoni monouso validi 10
minuti, e rifiuta i file dentro `~/.claude*`, `$CLAUDE_CONFIG_DIR`, `~/.ssh`, `~/.aws`, il Portachiavi e le cartelle
dell'app, i link che portano lì e tutto ciò che non è un file normale; alla copia ricontrolla che il file sia ancora
quello scelto (sostituito o spostato nel frattempo, va scelto di nuovo). "Sola lettura" è solo un'istruzione: l'agente
con Bash può modificare o cancellare le copie. Niente trascinamento di file nella finestra.

**Sub-agent** (dialog Avvia). **Sub-agent (max)**: "Predefinito del CLI" (nessun limite: niente regole sui sub-agent,
solo quelle dei tool board che ogni turno ha),
"Nessun sub-agent" (0) o da 1 a 10, per tutto il tentativo. Con un limite l'app vieta il tool `Workflow` (che avvia
agenti senza passare da Agent) e fa arrivare a sé ogni avvio di un sub-agent: lo concede e lo conta finché resta
spazio, poi risponde al modello "Sub-agent limit for this task reached (N). Complete the work directly without
starting sub-agents."; a limite esaurito (o con 0) anche `Agent` e `Task` sono vietati. Queste richieste non compaiono
mai come approvazioni da dare. **Modello dei sub-agent**: "Predefinito del CLI" o un alias (`opus`, `sonnet`, `haiku`,
`fable`), passato come `CLAUDE_CODE_SUBAGENT_MODEL`; vale solo per i sub-agent che non chiedono un modello proprio
(Explore, o una chiamata con un modello esplicito, possono usarne un altro). Il pannello del task riassume: "Modello:
opus · Sub-agent: sonnet, max 3 (usati 1)". Il modello del tentativo "Predefinito (x)" è quello del progetto, poi
quello delle Impostazioni app, altrimenti quello del CLI.

## Installazione

Servono macOS su Apple Silicon, Claude Code installato (`claude`, per esempio in `~/.local/bin`) con un login
dell'abbonamento, e git ≥ 2.44 (Xcode Command Line Tools o Homebrew): con un git più vecchio l'app lo segnala e non
esegue nessun comando git, perché non potrebbe impedire il fetch pigro di un partial clone. Per compilare vedi Setup
qui sotto.

```bash
scripts/release.sh     # cargo tauri build + controlli; stampa i percorsi di .app e .dmg
```

- `target/release/bundle/macos/AI Task Manager.app` e `target/release/bundle/dmg/AI Task Manager_0.1.0_aarch64.dmg`.
  Si installa trascinando l'app in Applicazioni (dal .dmg o dalla cartella). Il .dmg ha la finestra standard del
  Finder: impaginarla (icone e link ad Applicazioni in posizione) richiede che il terminale possa controllare il Finder
  (permesso Automazione), senza il quale l'AppleScript del bundler va in timeout e la build fallisce; lo script la
  salta (`CI=true`), `ATM_DMG_LAYOUT=1 scripts/release.sh` la chiede.
- **Non firmata né notarizzata.** Un'app compilata sul tuo Mac non ha l'attributo di quarantena e si apre con un doppio
  clic. Se il .app o il .dmg arrivano da un altro Mac (download, AirDrop), Gatekeeper la blocca la prima volta:
  fino a macOS 14 clic destro (o Ctrl-clic) sull'app → Apri → Apri; da macOS 15 il clic destro non basta più: dopo il
  primo tentativo, Impostazioni di Sistema → Privacy e sicurezza → «Apri comunque». In alternativa, per un'app di cui
  ti fidi: `xattr -dr com.apple.quarantine "/Applications/AI Task Manager.app"`.
- **Dati**: DB, log e copie degli allegati in `~/Library/Application Support/dev.aitaskmanager.desktop` (`atm.sqlite3`,
  `logs/`, `attachments/`; log e allegati di un task se ne vanno con il task o con il progetto), cache
  (script di login) in `~/Library/Caches/dev.aitaskmanager.desktop`, worktree in `~/.ai-task-manager/worktrees`
  (configurabile). Niente sta dentro il bundle: reinstallare (un .app nuovo copiato sopra il vecchio) li lascia
  intatti. Per disinstallare si tolgono l'app e queste tre cartelle (i branch `atm/…` restano nei repository).
- **Nessun socket di rete.** L'unico socket in ascolto è quello Unix del plugin single-instance,
  `/tmp/dev_aitaskmanager_desktop_si.sock`: un secondo avvio gli passa cartella e argomenti e porta avanti la finestra
  già aperta. `/tmp` è condiviso tra gli account del Mac: se a quel percorso c'è un socket di un altro utente l'app non
  lo usa (niente controllo di istanza singola, una riga sullo stderr) invece di passargli l'avvio e uscire.
- `scripts/release.sh` verifica che il WASM di release e il binario non contengano i driver di selftest ed E2E (vedi
  Build), l'identificatore del bundle, l'icona e il checksum del .dmg (`hdiutil verify`). `--no-build` ripete solo i
  controlli sull'ultima build.

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
cargo tauri dev          # avvia `trunk serve --features testkit` in ui/ (porta 1420) e apre l'app
scripts/check.sh         # fmt, clippy host + wasm32 (release, --features testkit, --features mock), test, grep di sicurezza: va tenuto verde
(cd ui && trunk serve --features mock)   # solo UI nel browser (porta 1420), backend finto in ui/src/ipc/mock
```

Nel mock, `http://localhost:1420/?task=<id>` apre subito il pannello di quel task (nel tab Task). Con la baseline di M1 in
`ui/src/ipc/mock/board.rs` (loggato, un progetto, un task per colonna) gli id sono `task-todo`, `task-inprogress`,
`task-inreview` (che ha già due allegati) e `task-done`. Il progetto `demo` ha un Riepilogo completo (CLAUDE.md,
AGENTS.md con un carattere bidi, README troppo grande, impostazioni e server MCP mascherati), `?overview_error` lo fa
fallire; "Aggiungi file…" risponde a turno con un file, due file, nessuno (selettore annullato) e un file troppo grande.
Le task di Zed (`.zed/tasks.json`) lanciano l'app, l'app con fake-claude e il mock.

## Avvio dell'app in sviluppo

**Con il Claude Code reale.** `cargo tauri dev` usa il `claude` installato (e il tuo abbonamento quando lanci un
agente), la cartella dati normale e il login descritto sopra.

```bash
cargo tauri dev                                                  # con trunk serve (CSP non applicata)
cargo tauri build --debug --no-bundle && ./target/debug/ai-task-manager   # asset incorporati, CSP attiva, senza driver di test
```

All'uscita le build di debug scrivono anche il ramo che ferma gli agenti (`exit: RunEvent::ExitRequested` per
`app.exit` e la chiusura della finestra, `exit: RunEvent::Exit` per Cmd+Q, Esci dal Dock e logout).

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
`noinit`, `big`, `flood`, `control`, `usage_limit`, `auth_fail`, `resolve_merge`, `resume_fail`, `background`,
`subagents`; `append` aggiunge a `hello.txt` la prima riga del messaggio, così due task sullo stesso file vanno in
conflitto; `background` lascia un `sleep 300` in un process group suo, che l'app chiude a fine turno; `subagents` avvia
`FAKE_CLAUDE_SUBAGENTS` sub-agent, default 3, e registra per ciascuno se l'app l'ha concesso o negato; `board_tools` usa i
tool board come un agente: crea un sotto task di sé, elenca e legge, poi chiede di modificare, spostare (a
`[status:S]` del messaggio, default `inreview`) e avviare (`[target:ID]`, default il sotto task creato), e con
`[ask:skip]` chiama questi tre senza chiedere, per provare che l'app li rifiuta; `board_chain` crea due sotto task di
sé, il secondo con `after` sul primo; `fix_on_resume` è `simple`, ma in una sessione ripresa scrive anche `fixed.txt`,
e lo gioca da solo il prompt di correzione dell'autopilota; `mcp_other` interroga un server MCP che l'app non ha). Scenario e tag si leggono fuori dalla sezione `## Parent task` del prompt di un sotto task. Come il
CLI reale, fake-claude fa l'handshake MCP con il server `atm` dell'app prima di `system/init`, a ogni processo. Il
follow-up di "Risolvi con l'agente" gioca da solo `resolve_merge`, sul target che il prompt dell'app nomina. Variabili:
`FAKE_CLAUDE_AUTH=in|out` (stato di `auth status`), `FAKE_CLAUDE_AUTH_FILE` (file con `in`/`out` che la vince sulla
variabile e che `auth_fail` porta a `out` con una scrittura atomica, come il CLI vero dopo un login scaduto),
`FAKE_CLAUDE_SCENARIO` (scenario di default),
`FAKE_CLAUDE_TARGET` (branch di `resolve_merge`, altrimenti quello del prompt), `FAKE_CLAUDE_DELTA_MS` (pausa tra i
delta di un testo in streaming, default 0), `FAKE_CLAUDE_FLOOD_EVENTS` (testi di `flood`, default 10000),
`FAKE_CLAUDE_FLOOD_PAUSE_MS` (pausa ogni 100 testi di `flood`, default 0), `FAKE_CLAUDE_RECORD` (una riga JSON per
chiamata: argv, cwd, pid, variabili presenti; poi una per ogni messaggio utente con lo scenario giocato e il testo
intero del messaggio, e le risposte dell'host, comprese quelle agli avvii di sub-agent). Con `FAKE_CLAUDE_PROJECT_CONFIG=1` fake-claude esegue la configurazione del repo come il CLI reale (hook
`SessionStart`, `apiKeyHelper`, server di `.mcp.json`) salvo `--setting-sources=user` o `--strict-mcp-config`. Lo usa
il test di accettazione M6 `tests/flow.rs::malicious_repo_config_runs_only_when_trusted_and_unchanged`. Il suo
`system/init` dice `apiKeySource: "ANTHROPIC_API_KEY"` quando quella variabile gli arriva, `"apiKeyHelper"` dopo un
helper del repo, altrimenti `"none"`; `FAKE_CLAUDE_API_KEY_SOURCE` impone un valore (i test dello stop del turno).

## Build

```bash
scripts/release.sh                     # release: .app + .dmg, poi i controlli (sopra)
(cd ui && trunk build --release)       # solo frontend di release → ui/dist
cargo tauri build --debug --no-bundle --config src-tauri/tauri.testkit.conf.json   # debug con i driver di test
```

I driver di selftest ed E2E della UI (`ui/src/selftest.rs`, `ui/src/e2e.rs`) esistono solo con la feature `testkit` di
`atm-ui`; i comandi di debug che guidano, solo nelle build di debug (`cfg(debug_assertions)`). `cargo tauri build` usa
il `beforeBuildCommand` di `tauri.conf.json` (`trunk build --release`, senza `testkit`): nel WASM di release non ci
sono (M6: 1,73 MB contro 2,02 MB, e `strings` non trova nessuno dei loro nomi). Le build di debug che devono
guidarsi da sole li aggiungono con `--config src-tauri/tauri.testkit.conf.json`, che sostituisce il comando con
`trunk build --release --features testkit` (e toglie la sospensione della pagina quando la finestra è nascosta,
`backgroundThrottling: disabled`); `cargo tauri dev` li ha sempre (`trunk serve --features testkit`).
`scripts/check.sh` passa clippy su tutte e tre le varianti e fallisce se `tauri.conf.json` compila la release con
`testkit` o se i due moduli non sono dichiarati sotto la feature. Passare da una variante all'altra ricompila una
parte di Tauri (circa 10 s).

## Icona

`src-tauri/icons/icon.svg` è il sorgente (kanban su griglia delle icone macOS: 824 px, raggio 185). Per rigenerare
il set di `src-tauri/icons/`:

```bash
swift scripts/render-icon.swift src-tauri/icons/icon.svg target/icon-1024.png   # PNG 1024×1024 con trasparenza (AppKit)
cargo tauri icon target/icon-1024.png && rm -rf src-tauri/icons/android src-tauri/icons/ios
```

## Selftest (solo build di debug)

```bash
cargo build -p atm-core --bin fake-claude   # il selftest usa fake-claude, mai il claude reale
cargo tauri build --debug --no-bundle --config src-tauri/tauri.testkit.conf.json
out=$(ATM_SELFTEST=1 ./target/debug/ai-task-manager) && echo "$out" && grep -qF '"csp_violations":0' <<<"$out"
```

La UI esegue da sola le prove IPC (`debug_ping`, errore tipizzato, 50 messaggi su `Channel` di cui tre da 20 KiB),
guida il dialog portato, prova `subscribe_transcript`/unsubscribe e un reload della pagina (`transcript_subscribe_ok`,
`forwarder_unsub_ok`, `forwarder_reload_ok`, `reload_ok`) e conta le violazioni CSP; l'app stampa il report JSON su stdout ed esce con 0 se tutto passa,
altrimenti con 1 (anche se la UI non risponde entro 90 s: succede anche con una UI compilata senza `testkit`, e il
messaggio lo dice). Funziona anche con `ATM_SELFTEST=1 cargo tauri dev`,
ma lì `csp_enforced` vale `null`: Tauri applica la CSP solo agli asset incorporati, non alla pagina di `trunk serve`.
In selftest il plugin single-instance non viene registrato, così la prova gira anche con un'altra istanza aperta;
per lo stesso motivo il Core usa una cartella dati privata (`$TMPDIR/atm-selftest-<pid>`, cancellata all'uscita)
e non tocca mai il DB né gli agenti dell'app aperta. Il controllo su stdout esclude le uscite con 0 senza report
(per esempio la finestra chiusa a mano).

## E2E in-app (solo build di debug, M4 e M6)

```bash
scripts/e2e.sh                  # build (--locked) di fake-claude e del bundle di debug con testkit, poi le tre fasi (max 15 min)
scripts/e2e.sh --no-build       # riusa target/debug (bundle e fake-claude)
scripts/e2e.sh --keep           # conserva log e dati anche se passa
scripts/e2e.sh --perf           # solo la fase 3 (prestazioni), su dati nuovi
scripts/e2e.sh --gatekeeper     # solo la verifica Gatekeeper dello script di login: apre UNA finestra del Terminale
```

Con `ATM_E2E=1` la UI guida il DOM reale del WKWebView (click, `input`/`change`, tasti, eventi HTML5 di drag con
`DataTransfer`) ed esegue in ordine tutto il percorso del §12.2 su fake-claude. Lo script compila il bundle
`target/debug/bundle/macos/AI Task Manager.app` (con `--config src-tauri/tauri.testkit.conf.json`), ne installa una
copia nella cartella temporanea del giro (`Applications/`, come in `/Applications`) e lancia quella, con una copia di
fake-claude tutta sua (`bin/fake-claude`), così `pgrep -f` vede solo gli agenti del giro. Tutto sta nella cartella
temporanea (`ATM_E2E_DIR`): DB, cache, `HOME` (quindi i worktree), i repository di prova creati dal backend (`main`
con un commit, una cartella non git, un repo bare, uno vuoto, `mcp` con un CLAUDE.md e un server in `.mcp.json`
committati, `da-rimuovere` per allegati, sub-agent e rimozione), il file da allegare `attach/specifiche-e2e.txt`, il
record e lo stato di login di fake-claude. I selettori nativi di cartelle e di file non sono automatizzabili: il
backend restituisce il percorso messo in coda dalla prova (`debug_e2e_queue_pick`, un posto solo: lo prende il primo
selettore che si apre), mentre i click su "Aggiungi repository" e "Aggiungi file…", i toast e la sidebar sono quelli
veri.
Allo stesso modo le conferme native ricevono la risposta che il giro mette in coda con `debug_e2e_queue_confirm`, e il
giro legge i testi chiesti con `debug_e2e_confirms` (solo build di debug con `ATM_E2E=1`). "Accedi" scrive davvero
`claude-login.command`, ma nel giro normale l'`open -a Terminal` viene solo registrato (`CoreConfig::open_log`,
ignorato nelle build di release) e la prova esegue lo script con `/bin/sh`: il Terminale vero lo apre solo
`--gatekeeper` (sotto).

- **Fase 1:** passi 1–7 (il passo 2 si svolge dentro il gate del passo 1), con due reload nativi della pagina
  (`-[WKWebView reload]`, come "Ricarica" nel menu contestuale del WebView di debug: l'app non ha una scorciatoia
  Cmd+R): l'ordine del passo 4 riletto dal DB e la verifica che il reload faccia ripartire la subscription del
  transcript (1 forwarder prima del reload, 0 dopo, 1 alla riapertura, vista ripristinata). Il passo 4 trascina quattro
  card fino a `[T3, T6, T2, T1]` (create T1, T2, T3, T6); ogni `dragover` deve essere annullato (senza, WebKit non
  darebbe il `drop`) e vede un `DataTransfer` senza dati, come nella modalità protetta di un drag vero. Il passo 7
  ferma anche un `[fake:hang_ignore]`: lo Stop percorre tutta la sequenza del §7.9 (interrupt 5 s, EOF 3 s, SIGTERM
  3 s, SIGKILL del gruppo) e deve chiudersi tra 10 e 13 s senza lasciare processi; il `TurnEnd` del turno fermato
  dice "Interrotto dall'utente" senza `[ede_diagnostic]`. Poi un task `[fake:hang_ignore]`
  (ignora interrupt, EOF e SIGTERM e ha un nipote `sleep 300`: solo il SIGKILL di gruppo dello shutdown lo ferma), il
  follow-up `[fake:hang]` di T1 e l'uscita con Cmd+Q con questi due agenti attivi: un ⌘Q (key-down e key-up, sorgente
  HID) mandato al pid dell'app attraverso il window server con `CGEventPostToPid` → finestra, poi voce Esci del menu
  di Tauri → `NSApp terminate:` → `applicationWillTerminate:` → `RunEvent::Exit` → `Core::shutdown` sul main thread.
  Serve che il terminale che lancia lo script abbia l'accesso Accessibilità (per inviare eventi); senza, il ⌘Q è un
  `NSEvent` dato a `-[NSApplication sendEvent:]` dentro l'app, e `details.cmd_q` del report dice quale via è stata
  usata. Se l'app non esce entro 30 s la fase fallisce (con ⌘X al posto di ⌘Q fallisce davvero: il test non passa
  per caso).
- **Tra le fasi 1 e 2:** lo script verifica dal log che l'uscita sia passata solo da `RunEvent::Exit`, che non resti
  nessun processo del giro (`pgrep -f` della copia di fake-claude vuoto, M6 #2) e che nel DB lo Stop del passo 7 sia
  `killed/user_stop` e i due turni dell'uscita `killed/app_shutdown`. Poi **reinstalla** l'app (M6 #5): una copia nuova
  del bundle al posto di quella installata, con DB, log dei processi e worktree ancora tutti lì; la fase 2 gira sulla
  copia nuova.
- **Fase 2:** l'app riparte sugli stessi dati: l'ordine `[T3, T6, T2]` del passo 4 dopo il riavvio vero (DOM e DB,
  posizioni invariate), "Interrotto – Continua" su entrambe le card con la Notice dello shutdown, e Continua su tutte e
  due (`--resume` della sessione di ciascuna, turno completato), passi 9–12, i messaggi `Channel` oltre 8 KiB
  (`debug_channel_probe` e un turno `[fake:big]`) e la CSP applicata (`eval` bloccato). Al passo 10 T6 e T3 aggiungono
  righe diverse a `hello.txt` (`[fake:append]`); T6 viene mergiato, così T3 va in conflitto con `main` e "Risolvi con
  l'agente" manda il prompt dell'app (che non ha tag): fake-claude registra di aver giocato `resolve_merge`. Il
  controllo `security_confirmations` (M6) prova le conferme native: Attendibile + Annulla resta Isolato, + OK è Trusted
  e `trusted`; bypass + OK consentito; abbassare entrambi non chiede nulla; il passthrough della chiave API + OK mostra
  il banner nella topbar e disattivarlo non chiede nulla. Infine un altro turno `[fake:hang_ignore]` e l'uscita del
  report con `app.exit` (l'altro ramo: `ExitRequested` → `Core::shutdown`); lo script ricontrolla ramo di uscita,
  processi e DB.
- **Fase 3 (prestazioni, M6):** di nuovo sugli stessi dati, Agenti in parallelo = 3 dalle Impostazioni app e tre task
  `[fake:flood]` avviati dalla UI (10 000 testi dell'assistente per turno, una pausa di 100 ms ogni 100: i tre turni
  corrono insieme per una decina di secondi), l'ultimo aperto nel pannello. Mentre scorrono, un timer da 20 ms non
  deve mai arrivare più di 1 s in ritardo (il main thread della pagina resta libero), passare a Modifiche e tornare ad
  Agente deve mostrarsi entro 1,5 s e il transcript non deve mai avere più di 300 righe nel DOM; alla fine i tre turni
  si sono sovrapposti, sono completati con tutti i testi salvati e il pannello termina sul `TurnEnd`. Lo script
  controlla che non resti nessun processo e che i tre turni siano `completed/success` nel DB. I due tempi valgono solo
  con la pagina visibile: WebKit fa scattare i timer di una pagina nascosta (schermo bloccato o spento, finestra
  coperta, app nascosta) una volta al secondo. Per questo durante la fase la finestra resta sopra le altre, il
  campione del timer conta solo a pagina visibile e il report dice `"perf_responsiveness":"measured"`; con la pagina
  nascosta dice `"not measured (page hidden)"`: il giro completo stampa un avviso e passa se il resto della fase
  passa, `--perf` da solo fallisce. La finestra dei bundle `testkit` ha `backgroundThrottling: disabled`
  (`src-tauri/tauri.testkit.conf.json`, che ripete la finestra di `tauri.conf.json`: `scripts/check.sh` controlla che
  non divergano), così una pagina nascosta non viene sospesa e il giro non si blocca.

**Pagina del progetto, allegati, lista e sub-agent** (round feature del 2026-09-29, spec §12.2 punto 15). Selezionare
un progetto porta sul Riepilogo, quindi il giro passa esplicitamente al tab Task prima di usare la kanban, e apre le
impostazioni generali dal dialog Impostazioni app e quelle del progetto dal suo tab. Al passo 3 (fase 1) si aggiunge
prima `mcp`, il cui repository committa un `CLAUDE.md` e il server `e2e-tools` in `.mcp.json` con una variabile `TOKEN`:
il suo Riepilogo deve mostrare il file e il nome `TOKEN`, mai il suo valore (né nella pagina né nella risposta di
`get_project_overview`), la pagina deve essere un `tabpanel` dentro il `main` e i blocchi dei file devono dire se sono
aperti (`aria-expanded`, testo chiuso `inert`). Nella fase 2, dopo le conferme di sicurezza: `task_list_view`, il
passaggio alla Lista su `main` (righe nell'ordine della kanban) e il ritorno alla kanban; `attachment_to_the_agent`,
sul progetto `da-rimuovere`, un allegato scelto con il selettore in coda che arriva all'agente (fake-claude registra
la sezione `## Attachments` del prompt e `--add-dir=` nell'argv) in un tentativo con "Nessun sub-agent", che dà
`--disallowedTools` con `Agent`, `Task` e `Workflow`; `subagent_limit`, sullo stesso progetto con max 2 e modello
haiku, dove tre avvii di sub-agent ricevono allow, allow, deny e DB e pannello dicono «usati 2»; poi, dal round del
2026-09-30, `subtasks_from_the_panel` ("Aggiungi sotto task" nel pannello crea un figlio con `parent_id` nel DB, che il
pannello del padre elenca, la card del padre conta "0/1" e la Lista mostra annidato e comprimibile),
`board_tools_agent` (un task `[fake:board_tools]` con agenti in parallelo = 1: il sotto task nasce subito, modifica,
spostamento e avvio chiedono con la frase leggibile e senza "Approva sempre", e l'avvio torna all'agente come errore
`ConcurrencyLimit`) e `subtask_cascade` (eliminare il padre avvisa "Elimina anche 1 sotto task." e toglie padre e
figlio); dal round del 2026-10-01, `autopilot_fix_and_merge` (l'interruttore della topbar e le Impostazioni del
progetto con il comando `test -f fixed.txt` e il merge automatico; un task affidato dal dialog parte da solo, la
prima verifica fallisce, la correzione dell'autopilota scrive `fixed.txt` e il task è mergiato da solo, con le
notifiche registrate in un file invece che mostrate) e `autopilot_after` (con agenti in parallelo = 1, il secondo di
due task affidati resta «In coda» finché il primo non è Fatto, poi parte ed è mergiato anche lui); infine
`project_removal`, il menu di `da-rimuovere` aperto con un clic destro sintetico, chiuso con un clic fuori, e la sua
rimozione dalla lista, mentre `main` resta selezionato.

L'app gira con `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`,
`CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_EFFORT`, `GIT_DIR`, le variabili di cmux e il suo
`NODE_OPTIONS=--require=…` con `CMUX_ORIGINAL_NODE_OPTIONS_PRESENT=0` impostati (valori finti):
`child_env_scrubbed` verifica nel record di fake-claude che nessun agente dei giri li abbia avuti, e lo script che
`ps -E` dell'app non mostri né il socket di cmux né il suo `NODE_OPTIONS`.

I processi del giro sono i pid che fake-claude scrive nel proprio record (chiamate `-p` e nipoti `sleep`),
ricontrollati con `ps`, più qualunque processo della macchina, registrato o no, che lavora dentro la cartella del giro
(trovato con `lsof`: gli agenti girano nei suoi worktree). Dentro l'app contano solo fake-claude `-p` e `sleep 300`; tra le fasi e
alla fine lo script conta qualunque programma, più `pgrep -f` della copia di fake-claude del giro (e riporta, senza
farli contare, gli altri fake-claude della macchina). Processi fake-claude di altri test o di un'altra istanza non
contano e non vengono mai uccisi; con Ctrl-C lo script chiude app e processi del giro. Il backend conta i comandi IPC
falliti e il giro li confronta, nell'ordine, con quelli che provoca: nella fase 1 i tre `add_project` rifiutati del
passo 3; nella fase 2, tutti dopo i merge del passo 10 (che non devono farne fallire nessuno, per esempio nessun
`get_diff` su un tentativo appena mergiato), il `set_project_security` annullato (Attendibile + Annulla) e poi il
`get_task_detail` (NotFound) sul task del progetto `da-rimuovere` appena rimosso; nella fase 3 nessuno.

I report JSON delle fasi 2 e 3 (`step_1`…`step_12`, `channel_big_ok`, `reload_resubscribe_ok`, `csp_enforced`,
`security_confirmations`, `task_list_view`, `attachment_to_the_agent`, `subagent_limit`, `subtasks_from_the_panel`,
`board_tools_agent`, `subtask_cascade`, `autopilot_fix_and_merge`, `autopilot_after`, `project_removal`,
`command_failures_phase1`/`_phase2`/`_phase3`, `exit_requested_armed`, `child_env_scrubbed`,
`perf_flood`, `csp_violations` sommate su tutti i caricamenti di pagina, `details` con cosa è stato verificato, con le
misure della fase 3, o perché è fallito) sono l'unica cosa su stdout (i log di build vanno su stderr); lo script esce
con 0 solo se tutto è vero, `csp_violations` è 0 e i controlli su processi e DB passano. Se fallisce, la cartella
temporanea resta con log (`1.err`, `2.err`, `3.err`) e dati. L'E2E non registra il plugin single-instance, quindi gira
anche con un'altra istanza aperta, e non esegue mai il claude reale: il binario indicato (`ATM_CLAUDE_PATH`, risolto nei
symlink) deve chiamarsi `fake-claude` e contenere il messaggio di login di fake-claude (letto, mai eseguito),
altrimenti si ferma subito. `debug_e2e_git` accetta solo i controlli di sola lettura della UI (`show`, `log`, `status`,
`branch --list`, `worktree list`, nessuna opzione prima del sottocomando) e i comandi sui file rifiutano i symlink
pendenti. `ATM_E2E=1` è ignorato insieme a `ATM_SELFTEST=1`.

Restano fuori dall'automazione: i selettori nativi di cartelle e di file (sopra), il drag nativo col mouse (il DnD usa
`DragEvent` sintetici con `DataTransfer` sul DOM vero: una sessione di drag nativa segue il cursore reale e
spostarlo disturberebbe chi usa il Mac; `effectAllowed` non si può verificare perché WebKit lo ignora su un
`DataTransfer` costruito), il Cmd+R (non esiste) e, nel giro normale, l'apertura del Terminale (coperta da
`--gatekeeper`, da lanciare a parte perché apre una finestra).

`--gatekeeper` usa il codice del login sulla vera `app_cache_dir` (`~/Library/Caches/dev.aitaskmanager.desktop`, in una
sottocartella `e2e-gatekeeper`, così uno script di login vero non viene toccato): `write_login_script` scrive
`claude-login.command` e `open_login_terminal` lo apre con `open -a Terminal`. Il suo `claude` è un wrapper nella
cartella temporanea che scrive i propri argomenti in un marker e poi esegue fake-claude: la prova passa se il marker
dice `auth login` (Gatekeeper ha lasciato girare lo script) e lo script non ha l'attributo di quarantena; poi cancella
la sottocartella.

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
  isolamento 1), circa 0,15–0,25 USD l'uno nella stima del CLI. Un giro completo con `--ignored` esegue anche i test
  dei round successivi (sotto): 7 turni dei sub-agent e 3 dei tool board, questi ultimi su `haiku`.
- **Sub-agent e allegati (round feature del 2026-09-29): eseguiti il 2026-09-29 con il CLI 2.1.284, 4 su 4
  passati.** Quattro test verificano ciò che il round aveva preso dalla documentazione e dal bundle del CLI:
  - la regola `ask` su `Agent` arriva all'app in Supervisionato, Auto-edit e Autonomo (3 turni): ogni spawn è un
    `can_use_tool` con motivo `rule`, risposto subito dall'app e mai in attesa;
  - il rifiuto oltre il limite viene rispettato (2 turni): con limite 1 il secondo spawn è `Denied` con il testo
    esatto, e al turno dopo `--disallowedTools=AskUserQuestion,Agent,Task,Workflow` toglie il tool Agent (il CLI
    risponde "No such tool available: Agent");
  - `CLAUDE_CODE_SUBAGENT_MODEL` vale per un sub-agent senza modello proprio (1 turno): agente principale su `sonnet`,
    sub-agent su `opus`, entrambi nel `modelUsage` del turno;
  - `--add-dir` con uno spazio nel percorso (`Application Support`) è leggibile (1 turno): l'agente legge
    l'allegato con Read, senza approvazioni.

  Sono 7 turni reali, circa 1,9 USD nella stima del CLI, sull'abbonamento. Per ripeterli:

  ```bash
  ATM_REAL_CLAUDE=1 cargo test -p atm-core --test real_cli -- --ignored --test-threads=1 subagent add_dir
  ```
- **Tool board (round del 2026-09-30): verificati il 2026-10-01 con il CLI 2.1.286, test passato.** Lo spike del 2026-09-30
  (CLI 2.1.285, haiku, 2 turni, circa 0,11 USD) ha verificato il server MCP `sdk` sulla stdio del CLI: dichiarazione con
  `--mcp-config`, richieste `mcp_message`, risposte `{"mcp_response": …}`, nomi `mcp__atm__<tool>` e la regola `ask`
  che fa arrivare `can_use_tool` all'app in Supervisionato e in Autonomo. Il test dell'app,
  `real_cli_board_tools_ask_in_every_mode`, ripete la prova con i tool veri in Supervisionato, Auto-edit (mai provato
  finora) e Autonomo: l'agente crea un sotto task di sé senza chiedere e sposta un altro task in Fatto dopo
  l'approvazione. Sono 3 turni reali su `haiku` (circa 0,19 USD stimati); il 2026-10-01 è passato in tutte e tre le
  modalità. Per ripeterlo (consuma quota):

  ```bash
  ATM_REAL_CLAUDE=1 cargo test -p atm-core --test real_cli -- --ignored --test-threads=1 board_tools
  ```
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

## Limiti noti

- Il worktree non è una sandbox (sopra); le regole deny sono aggirabili; in Attendibile hook e server MCP del repo
  girano con i permessi dell'utente.
- Attendibile approva la configurazione committata sul branch target, non i file locali del checkout principale; un
  branch che cambia configurazione va riapprovato (sopra).
- Alla fine di ogni turno l'app chiude lo stdin del CLI: il lavoro lasciato in background da un agente (dev server,
  `run_in_background`) viene chiuso con il turno (spec R7).
- I worktree non hanno `node_modules`, `.env` e cache del checkout principale: i primi turni possono spendere tempo a
  fare bootstrap (R11). Script di setup e `copy_files` sono rimandati.
- I discendenti di un agente morto da solo senza `result`, o insieme all'app, non sono coperti dalla chiusura dei
  processi (§7.9).
- Il CLI salva i suoi transcript in `~/.claude/projects/<worktree>`: l'app non li legge e non li cancella (R12).
- Il modello può rifiutare un follow-up che ritiene estraneo a un task già finito (M5); il prompt dell'app lo mitiga
  per gli attempt nuovi.
- Solo macOS su Apple Silicon; app non firmata né notarizzata (Gatekeeper sopra); nessun aggiornamento automatico.
- Niente push, fetch, PR, plan mode, rewind (spec §13.2). Le immagini si passano solo come allegati (niente incolla nel
  composer né trascinamento di file).
- Il modello dei sub-agent scelto nel dialog Avvia è la fonte con la priorità più bassa per Claude Code: il modello
  che una chiamata Agent chiede, o quello della definizione dell'agente (Explore), vince. Il limite conta i sub-agent
  concessi, non quelli finiti, e non copre modi di avviarli diversi da `Agent`, `Task` e `Workflow`. La regola `ask`
  su cui si basa è comportamento osservato del CLI 2.1.284 (verificato il 2026-09-29, sopra), non documentato.
- Gli allegati sono copie che l'agente può modificare o cancellare con Bash; gli originali non vengono mai toccati. Un
  gettone del selettore scade dopo 10 minuti: un dialog di creazione lasciato aperto più a lungo crea il task senza
  allegati (con l'avviso).
- Il Riepilogo non mostra i server MCP di `~/.claude.json` (`claude mcp add`), `.claude/settings.local.json` né i file
  dietro un symlink; non ha cache (ogni visita rilegge il commit) e non rende il markdown.
- La Lista non ha ricerca, filtri né ordinamento; pagina e vista non vengono ricordate.
- Sotto task: un solo livello; lo stato del padre non segue quello dei figli; eliminare un padre elimina sempre i suoi
  sotto task; la Lista non ricorda quali padri sono compressi.
- Tool board: "Approva sempre" non esiste per loro, quindi ogni modifica, spostamento o avvio chiede di nuovo; un
  agente avviato da un agente non può avviarne altri; l'approvazione di `start_task` non mostra la modalità che avrà il
  nuovo agente; il loro funzionamento con il CLI reale dipende da un protocollo non documentato di Claude Code,
  verificato solo dallo spike (sopra).
- Autopilota: non riprova mai da solo un turno fallito, fermato o al limite d'uso, e non riprende un turno interrotto
  dalla chiusura dell'app; una verifica interrotta dalla chiusura diventa "non eseguita" e il task resta a te. Il
  comando di verifica gira senza chiedere in ogni modalità (sopra). Un file non ignorato scritto dalla verifica blocca
  il merge automatico. Spegnere l'autopilota non ferma una verifica in corso. Un task annullato dopo essere stato
  scelto in «Parte dopo…» blocca chi dipende da lui. Notifiche solo macOS, al massimo una ogni 30 s per task e tipo,
  senza azioni. Le card mostrano un cambio dell'autopilota del progetto o dei tentativi alla loro prossima modifica, e
  la riga «Verifica:» del pannello mostra il comando attuale, non la durata (spec §13.7).
- L'E2E non automatizza i selettori nativi di cartelle e di file né il drag nativo col mouse; i tempi della sua fase 3
  si misurano solo con lo schermo sbloccato e la finestra visibile (altrimenti il report li dà come non misurati).

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
| Fingerprint della configurazione Claude (accettazione #1) | confermato con fake-claude che esegue la configurazione del repo (`FAKE_CLAUDE_PROJECT_CONFIG=1`): in Isolated nessun marker; in Trusted invariato sì; dopo una modifica nel worktree turno Isolated con Notice; dal 2026-09-29 senza `apiKeyHelper` (non più approvabile) e con il branch target che cambia configurazione (riapprovare) | M6 |
| Conferme native per Trusted, bypass e chiave API | confermate dall'E2E (`security_confirmations`) con risposte in coda | M6 |
| `TurnEnd` dopo uno Stop | mostra "Interrotto dall'utente" (non l'`[ede_diagnostic]` interno) | M6 |
| Deny di una regola dei permessi | mostrato come Negato, non Fallito | M6 |
| Variabili di una sessione Claude Code padre | tolte dall'ambiente degli agenti (`tests/claude.rs`, `tests/flow.rs`, E2E `child_env_scrubbed`) | M6 |
| Solo abbonamento: Trusted rifiutato per una configurazione che fattura via API (2026-09-29) | confermato: `apiKeyHelper`, `awsAuthRefresh`, `env.ANTHROPIC_BASE_URL` (in `settings.local.json` committato), `env.ANTHROPIC_API_KEY`, `env.CLAUDE_CODE_USE_VERTEX`, un JSON non valido → `Invalid` che nomina la chiave, nulla salvato; un'approvazione precedente che la conteneva lascia il progetto non attendibile e il turno Isolato, helper mai eseguito (`tests/flow.rs::trusted_is_refused_when_the_config_bills_outside_the_subscription`); un worktree che aggiunge `env.ANTHROPIC_API_KEY` gira Isolato con la Notice e torna Attendibile quando la toglie (`a_worktree_config_that_bills_outside_the_subscription_runs_isolated`); ogni nome delle due liste è controllato (`tests/git.rs::billing_keys_of_the_settings`) | M6+ |
| Solo abbonamento: turno fermato su `apiKeySource` (2026-09-29) | confermato con fake-claude (`FAKE_CLAUDE_API_KEY_SOURCE`): `[fake:slow]` fermato al `system/init` in meno di 5 s, `failed` senza `stop_reason`, Notice ed `error` "Turno fermato: … (apiKeySource: ANTHROPIC_API_KEY)", nessun testo del modello né `TurnEnd`, processo sparito; col passthrough attivo lo stesso turno completa (`tests/flow.rs::an_api_key_source_stops_the_turn_unless_the_passthrough_is_on`) | M6+ |
| Approvazione del commit di punta del branch target (2026-09-29) | confermato: la configurazione di un commit letta con `git cat-file --batch` è identica a quella di un suo worktree (directory, script eseguibili, link a file e a directory, link pendente, submodule, file eseguiti e marketplace) e non cambia con i file locali del checkout principale (`tests/git.rs::commit_config_is_the_config_of_a_checkout_of_it`); link fuori dal repo, file eseguiti portati fuori da un link, cicli, 2000 record, 64 MiB, commit sconosciuto → errore (`commit_config_limits_and_links`); `settings.local.json` non tracciato (anche con `apiKeyHelper`) e modifiche non committate non bloccano i turni Trusted, un commit che non tocca la configurazione mantiene l'approvazione, repo senza configurazione approvato con l'hash vuoto (`tests/flow.rs::trusted_approves_the_target_tip_not_the_working_tree`); branch andato avanti → progetto non attendibile, l'attempt dal tip nuovo gira Isolato con la Notice da riapprovare (`malicious_repo_config_runs_only_when_trusted_and_unchanged`) | M6+ |
| `NODE_OPTIONS` di cmux (2026-09-29) | confermato: marker `0` → tolto, `1` con il valore salvato → ripristinato, senza marker → intatto, in ogni figlio (`tests/claude.rs::child_env_restores_the_users_node_options_under_cmux`, `tests/flow.rs::api_key_passthrough_is_opt_in_and_a_parent_session_never_leaks`) e nella ri-esecuzione dell'app (E2E: 16 chiamate di agenti senza `NODE_OPTIONS`, `ps -E` dell'app senza il `--require` di cmux) | M6+ |
| Revisione del 2026-09-29: file di impostazioni sotto un altro nome (APFS) | confermato su questo Mac (volume che ignora maiuscole e forme Unicode): `.claude/Settings.json` e `.claude/ſettings.local.json` (U+017F) si aprono come i file di impostazioni; nel worktree sono analizzati come tali (chiavi di fatturazione, regola `Bash`, hook e `apiKeyHelper` eseguiti tracciati), nel commit rendono la configurazione non verificabile, come `.Claude/` e `.MCP.json` nella radice (`tests/git.rs::settings_under_another_name_are_what_the_cli_opens`); Trusted rifiutato per un commit con `.claude/Settings.json`, e con un'approvazione precedente il turno gira Isolato con la Notice di fatturazione, `apiKeyHelper` mai eseguito (`tests/flow.rs::a_settings_file_under_another_case_is_checked_like_the_settings`) | M6+ |
| Revisione del 2026-09-29: fetch pigro di un partial clone | confermato con git 2.54: con `extensions.partialClone`, `remote.origin.promisor` e un `remote.origin.uploadpack` che crea un file, `git diff` semplice verso un commit con oggetti mancanti lo esegue (controllo); dal runner dell'app (`GIT_NO_LAZY_FETCH=1` su ogni chiamata) diff, `merge-tree`, `worktree add` e configurazione del commit falliscono senza eseguire nulla (`tests/git.rs::a_partial_clone_never_fetches_from_the_app`); un git 2.30 o 2.43 non esegue nient'altro che `--version` (`git_discovery_and_version_gate`) | M6+ |
| Revisione del 2026-09-29: ricontrollo al `system/init` | confermato con fake-claude: un hook `SessionStart` che scrive `env.ANTHROPIC_BASE_URL` in `.claude/settings.local.json` → turno `[fake:slow]` congelato, ricontrollato e ucciso in meno di 5 s, `failed` senza `stop_reason`, Notice ed `error` "Turno fermato: … imposta env.ANTHROPIC_BASE_URL.", nessun testo del modello, processo sparito (`tests/flow.rs::a_billing_key_gained_during_the_cli_start_kills_the_turn_at_once`); un cambio qualsiasi all'avvio dà lo stesso esito e il turno dopo gira Isolato (`a_config_change_during_the_cli_start_stops_the_turn`) | M6+ |
| Revisione del 2026-09-29: `ANTHROPIC_BASE_URL` nell'ambiente dell'app | confermato: resta nell'ambiente degli agenti come `CLAUDE_CODE_USE_*` (`tests/claude.rs::child_env_drops_a_parent_sessions_variables_only`), `EnvStatus.base_url_env` lo segnala (`tests/flow.rs::api_key_passthrough_is_opt_in_and_a_parent_session_never_leaks`, anche per gli endpoint dei provider: `tests/claude.rs::api_key_and_cloud_provider_detection`) e la topbar mostra il banner `base-url` (test di `ui/src/views/sidebar.rs`) | M6+ |
| Revisione del 2026-09-29: link e limiti della visita | confermato: nel worktree un link fuori dal repository non viene seguito né toccato (niente `realpath`: un target in una cartella senza permessi dà il record `e` col percorso testuale, `tests/git.rs::a_link_out_of_the_root_is_never_looked_at`); una parola più lunga di `PATH_MAX` si salta, più di 4096 parole cercate come path, alberi letti oltre 16 MiB in tutto (due cartelle da 9 MiB; una sola passa), un albero con un nome ripetuto → errore (`commit_and_checkout_walks_are_bounded`); passi e tempo limitati (test unitari di `git/fingerprint.rs`); gli errori `Invalid` della configurazione di un commit restano in cache come i risultati | M6+ |
| Revisione di sicurezza di M6 | fingerprint esteso ai file eseguiti dalla configurazione (`tests/flow.rs::trusted_config_covers_the_script_an_mcp_server_runs`), compare-and-set di approvazione e impostazioni, revoche che fermano i turni, ricontrollo al `system/init`, variabili cmux/IDE tolte e app ri-eseguita senza quelle della sessione padre (E2E: `ps -E` dell'app), filter driver del repo spenti nel git dell'app | M6 |
| `claude auth status` e `--version` con cwd `/` | confermato (CLI 2.1.284, nessuna quota): da `/` `authMethod: claude.ai`; in una cartella con `apiKeyHelper` nel `.claude/settings.json` `auth status` riporta `api_key_helper` (legge le impostazioni del cwd senza eseguirle), per questo i probe girano sempre con cwd `/` | M6 |
| Driver di selftest ed E2E fuori dal WASM di release | confermato: solo con la feature `testkit`; `strings` sul WASM e sul binario di release non trova nessuno dei loro nomi (`scripts/release.sh`), mentre il WASM `testkit` li contiene; selftest ed E2E passano con `--config src-tauri/tauri.testkit.conf.json` | M6 |
| `cargo tauri build`: `.app` e `.dmg` | confermato (non firmati): `AI Task Manager.app` (9,9 MiB) e `AI Task Manager_0.1.0_aarch64.dmg` (5,5 MiB), icona nuova (`cargo tauri icon` da `icon.svg`); il .dmg con l'impaginazione del Finder richiede il permesso Automazione (senza, l'AppleScript del bundler va in timeout: `scripts/release.sh` la salta con `CI=true`) | M6 |
| `.app` di release aperto con LaunchServices con l'ambiente del Finder trova `claude` e `git` | confermato (2026-09-28): `open` sotto `env -i` con il `PATH` di launchd (`/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`; `open` da solo passa all'app l'ambiente del Terminale); con il `claude` reale, solo `--version` e `auth status`: `claude ~/.local/bin/claude version 2.1.284 … auth loggedIn (authMethod claude.ai, subscription max) git /opt/homebrew/bin/git version 2.54.0`, board senza gate e "In uso" letti via Accessibilità | M6 |
| Uscita con 2 agenti fake attivi (accettazione #2) | confermato dall'E2E: Cmd+Q con `[fake:hang]` e `[fake:hang_ignore]` in corso, poi `pgrep -f` di fake-claude vuoto | M6 |
| Reinstallazione (accettazione #5) | confermato dall'E2E: copia nuova del bundle sopra quella installata (inode nuovo), la fase 2 la esegue sui dati della fase 1 e dopo ogni riga `processes` e ogni log della fase 1 sono ancora lì, invariati; e con il `.app` di release (dati di un giro `--perf`, `HOME` temporanea): stessi 3 task, 30 009 entry, 9 log e 3 worktree prima e dopo, board uguale | M6 |
| Prestazioni con `flood` e 3 agenti | confermato dall'E2E (fase 3, schermo sbloccato): 3 × 10 000 testi in ~11 s con 10 s di sovrapposizione, timer da 20 ms mai più di 27–29 ms in ritardo, cambio tab visibile al primo controllo (51–54 ms, il polling è ogni 50 ms), al massimo 300 righe nel DOM | M6 |
| Limite di sub-agent (round feature del 2026-09-29) | confermato con fake-claude (`[fake:subagents]`): con max 2 e 3 avvii, due concessi e contati (`subagents_used` salvato), il terzo negato con il testo per il modello, nessuna approvazione pendente (`tests/flow.rs::subagent_limit_allows_up_to_the_max_then_denies`); con 0 `Agent`, `Task` e `Workflow` vietati dal primo turno (`a_zero_limit_disallows_subagents_from_the_first_turn`); senza limite uno spawn è un'approvazione normale (`without_a_limit_a_subagent_spawn_is_an_ordinary_approval`); limite e modello controllati prima di creare il worktree (`subagent_options_are_checked_before_the_worktree`); argv senza opzioni identico (snapshot `claude__*`) | round 2026-09-29 |
| Allegati (round feature del 2026-09-29) | confermato con fake-claude: sezione `## Attachments` nel primo prompt e `--add-dir` a ogni turno, file e log cancellati con il task e con il progetto (`tests/flow.rs::attachments_reach_the_agent_and_go_with_the_task`, `removing_the_project_removes_its_attachments_and_logs`); percorsi rifiutati, gettoni monouso e scaduti, limite di 20 senza copie orfane (`picks_outside_the_rules_are_refused`, `tokens_are_single_use_and_the_copy_checks_again`, `the_attachment_limit_removes_the_copies_it_refuses`, `staged_picks_expire_and_the_oldest_go_first`) | round 2026-09-29 |
| Riepilogo del progetto (round feature del 2026-09-29) | confermato: file nell'ordine atteso dal tip del branch target, mai dal checkout né dalla home, segreti mascherati (nessun "secret" nel riepilogo serializzato), note per file grandi, binari, link e JSON non valido (`tests/git.rs::overview_shows_the_committed_context_in_order`, `overview_notes_what_it_does_not_show`, `overview_masks_every_secret`, `core_overview_reads_the_target_tip_and_never_home`) | round 2026-09-29 |
| Regola `ask` su `Agent` con il CLI reale in ogni modalità, deny del limite, `CLAUDE_CODE_SUBAGENT_MODEL`, `--add-dir` con spazi | **verificato** il 2026-09-29 con il CLI 2.1.284: 4 test `#[ignore]` di `tests/real_cli.rs` passati, 7 turni reali (sopra) | round 2026-09-29 |
| Server MCP `sdk` in-process sul control protocol (spike del 2026-09-30) | **verificato** con il CLI 2.1.285 (2 turni haiku): `--mcp-config={"mcpServers":{"atm":{"type":"sdk","name":"atm"}}}` compatibile con `--strict-mcp-config`; handshake `initialize`, `notifications/initialized`, `tools/list` come `mcp_message` prima di `system/init`; `tools/call` con `_meta."claudecode/toolUseId"`; risposte `{"mcp_response": …}`; tool `mcp__atm__<nome>`; la regola `ask` manda `can_use_tool` (con `mcp_server.source = "sdk"`) in `default` e in `bypassPermissions` | round 2026-09-30 |
| Sotto task e tool board con fake-claude | confermato: sotto task validati (un livello, stesso progetto), eliminazione a cascata con worktree, allegati e log, `Busy` senza toccare nulla se gira il padre o un figlio (`tests/flow.rs::deleting_a_parent_deletes_its_subtasks_and_their_files`, `deleting_a_running_parent_removes_nothing`), sezione `## Parent task` nel prompt (`a_subtask_prompt_carries_its_parent`); tool board: creazione immediata, modifica, spostamento e avvio solo dopo l'approvazione, rifiuto rispettato, chiamata non approvata rifiutata dall'app, `ConcurrencyLimit`, profondità 2, modalità del progetto per un task non figlio, nessun accesso ad altri progetti (`board_tools_*`, `board_start_*`, `a_denied_board_tool_changes_nothing`, `an_unapproved_board_call_is_refused`); E2E `subtasks_from_the_panel`, `board_tools_agent`, `subtask_cascade` | round 2026-09-30 |
| Autopilota con fake-claude | confermato: coda che rispetta "agenti in parallelo" e riempie gli slot liberi, `after`, verifica fallita → correzione → merge automatico o «pronto», tentativi esauriti, conflitti rimandati all'agente, nulla in pausa, Stop rispettato, coda ricostruita al riavvio, timeout e chiusura dell'app che uccidono il gruppo della verifica, nessuna corsa con un merge manuale, correzione che aspetta uno slot e passa prima della coda, follow-up dell'utente durante una verifica, discard che riprende il task, cicli di dipendenze rifiutati, task in coda da un agente avviati con `started_by_attempt` (`tests/flow.rs::autopilot_*`, `board_start_without_a_slot_is_queued_in_an_autopilot_project`, `board_subtasks_inherit_auto_and_take_after`); E2E `autopilot_fix_and_merge`, `autopilot_after` | round 2026-10-01 |
| Tool board con il CLI reale nell'app, in Supervisionato, Auto-edit e Autonomo | **verificato** il 2026-10-01 con il CLI 2.1.286: `real_cli_board_tools_ask_in_every_mode`, 3 turni haiku, circa 0,19 USD | round 2026-09-30 |
