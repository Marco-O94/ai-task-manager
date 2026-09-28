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
scripts/check.sh         # fmt, clippy host + wasm32, test, grep di sicurezza: va tenuto verde
```

## Build

```bash
(cd ui && trunk build --release)             # solo frontend → ui/dist
cargo tauri build --debug --no-bundle        # → target/debug/ai-task-manager (asset incorporati, CSP attiva)
```

## Selftest (solo build di debug)

```bash
cargo tauri build --debug --no-bundle
out=$(ATM_SELFTEST=1 ./target/debug/ai-task-manager) && echo "$out" && grep -qF '"csp_violations":0' <<<"$out"
```

La UI esegue da sola le prove IPC (`debug_ping`, errore tipizzato, 50 messaggi su `Channel` di cui tre da 20 KiB),
guida il dialog portato e conta le violazioni CSP; l'app stampa il report JSON su stdout ed esce con 0 se tutto passa,
altrimenti con 1 (anche se la UI non risponde entro 90 s). Funziona anche con `ATM_SELFTEST=1 cargo tauri dev`,
ma lì `csp_enforced` vale `null`: Tauri applica la CSP solo agli asset incorporati, non alla pagina di `trunk serve`.
In selftest il plugin single-instance non viene registrato, così la prova gira anche con un'altra istanza aperta;
il controllo su stdout esclude le uscite con 0 senza report (per esempio la finestra chiusa a mano).

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
