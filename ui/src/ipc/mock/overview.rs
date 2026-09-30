//! Mock of `get_project_overview` (spec F3). Owner: UI-SHELL.
//!
//! The demo project gets a full repository: a long CLAUDE.md with a hidden character, no
//! `.claude/CLAUDE.md`, an AGENTS.md with a bidirectional character, a README,
//! `.claude/settings.json` with its secrets masked, `.mcp.json` servers and `.claude/` agents,
//! commands and skills. Any other project gets an empty one, for the empty states.
//! `?overview_error` answers with the error of a default target branch that cannot be read.

use atm_types::*;
use serde_json::Value;

use super::board;

const COMMIT: &str = "3f2a9c1e5b7d4f608a1c2e3d4b5a69788c9d0e1f";

pub fn handle(req: Value) -> Result<Value, AppError> {
    let req: ProjectIdReq = serde_json::from_value(req)?;
    let project = board::find_project(&req.project_id)
        .ok_or_else(|| AppError::not_found(format!("progetto {}", req.project_id)))?;
    if flag("overview_error") {
        return Err(AppError::git(format!(
            "Il branch target predefinito {} non si può leggere (fatal: Not a valid object name)",
            project.default_target_branch
        )));
    }
    Ok(serde_json::to_value(overview(&project))?)
}

fn overview(project: &Project) -> ProjectOverview {
    let loaded = project.config_policy == ConfigPolicy::Trusted && project.trusted;
    let mut overview = ProjectOverview {
        project_id: project.id.clone(),
        branch: project.default_target_branch.clone(),
        commit: COMMIT.into(),
        agents_load_config: loaded,
        files: Vec::new(),
        mcp_servers: Vec::new(),
        claude_agents: Vec::new(),
        claude_commands: Vec::new(),
        claude_skills: Vec::new(),
    };
    if project.name != "demo" {
        return overview;
    }

    // The usage notes are the backend's (`git/overview.rs`, `usage`).
    let (memory, config) = if loaded {
        ("caricato all'avvio", "caricato all'avvio")
    } else {
        (
            "letto dall'agente su istruzione del prompt",
            "non caricato in modalità Isolata",
        )
    };
    overview.files = vec![
        ContextFile {
            used_by_agents: loaded,
            hidden_chars: true,
            ..file(
                "CLAUDE.md",
                ContextFileKind::Memory,
                "# demo\n\n\
                 API REST in Rust (axum) con parser e lista utenti.\n\n\
                 ## Regole\n\
                 - Esegui `cargo test` e `cargo clippy -- -D warnings` prima di chiudere un task.\n\
                 - Niente dipendenze nuove senza chiedere.\n\
                 - Messaggi di errore per l'utente in italiano.\n\
                 - Un commit per task, con un messaggio che dice il perché.\n\
                 - Non toccare `migrations/` già rilasciate: aggiungine una nuova.\n\n\
                 ## Struttura\n\
                 - `src/api/`: handler HTTP\n\
                 - `src/parser/`: parser dei filtri\n\
                 - `src/db/`: accesso al database (sqlx)\n\
                 - `tests/`: test di integrazione (database in memoria)\n\n\
                 ## Stile\n\
                 - Errori con `thiserror`, mai `unwrap()` fuori dai test.\n\
                 - Nomi dei moduli al singolare.\n\
                 - Log con `tracing`, livello `debug` per i dettagli.\n\n\
                 ## Comandi\n\
                 - `cargo run -- --port 8080`: avvia il server\n\
                 - `cargo test parser`: solo i test del parser\n\
                 - `cargo sqlx prepare`: aggiorna le query verificate\n\n\
                 ## Note\n\
                 - La paginazione usa un cursore, non l'offset.\n\
                 - Il campo `email`⟨U+200B⟩ degli utenti è unico.\n",
                memory,
            )
        },
        ContextFile {
            hidden_chars: true,
            ..file(
                "AGENTS.md",
                ContextFileKind::Agents,
                "# Istruzioni per gli agenti\n\n\
                 - Lavora solo nel worktree del task.\n\
                 - Aggiorna CHANGELOG.md a ogni modifica visibile.\n\
                 - Commento lasciato da un contributore: ⟨U+202E⟩txt.exe⟨U+202C⟩\n",
                "letto su istruzione del prompt",
            )
        },
        file(
            "README.md",
            ContextFileKind::Readme,
            "# demo\n\n\
             API REST di esempio: utenti, ricerca con filtri e paginazione.\n\n\
             ## Avvio\n\n\
             ```sh\n\
             cp .env.example .env\n\
             cargo run -- --port 8080\n\
             ```\n\n\
             ## Endpoint\n\n\
             - `GET /users?filter=…&cursor=…`\n\
             - `GET /users/{id}`\n\
             - `POST /users`\n\n\
             ## Licenza\n\n\
             MIT\n",
            "solo contesto per te",
        ),
        ContextFile {
            used_by_agents: loaded,
            note: Some("valori segreti mascherati".into()),
            ..file(
                ".claude/settings.json",
                ContextFileKind::Settings,
                "{\n  \"permissions\": {\n    \"allow\": [\"Bash(cargo test:*)\", \"Bash(cargo clippy:*)\"],\n    \"deny\": [\"Read(./.env)\"]\n  },\n  \"env\": {\n    \"RUST_LOG\": \"•••\",\n    \"DATABASE_URL\": \"•••\"\n  },\n  \"hooks\": {\n    \"PostToolUse\": [\n      { \"matcher\": \"Edit|Write\", \"hooks\": [{ \"type\": \"command\", \"command\": \"cargo fmt\" }] }\n    ]\n  }\n}\n",
                config,
            )
        },
        ContextFile {
            used_by_agents: loaded,
            note: Some("valori segreti mascherati".into()),
            ..file(
                ".mcp.json",
                ContextFileKind::Mcp,
                "{\n  \"mcpServers\": {\n    \"database\": {\n      \"command\": \"node\",\n      \"args\": [\"tools/mcp-db.js\"],\n      \"env\": { \"DATABASE_URL\": \"•••\", \"PGPASSWORD\": \"•••\" }\n    },\n    \"docs\": {\n      \"type\": \"http\",\n      \"url\": \"https://docs.example.com/mcp\",\n      \"headers\": { \"Authorization\": \"•••\" }\n    }\n  }\n}\n",
                config,
            )
        },
    ];
    overview.mcp_servers = vec![
        McpServer {
            name: "database".into(),
            transport: "stdio".into(),
            target: "node tools/mcp-db.js".into(),
            env_keys: vec!["DATABASE_URL".into(), "PGPASSWORD".into()],
            header_keys: Vec::new(),
        },
        McpServer {
            name: "docs".into(),
            transport: "http".into(),
            target: "https://docs.example.com/mcp".into(),
            env_keys: Vec::new(),
            header_keys: vec!["Authorization".into()],
        },
    ];
    overview.claude_agents = vec!["code-reviewer".into(), "test-writer".into()];
    overview.claude_commands = vec!["release".into()];
    overview.claude_skills = vec!["api-conventions".into()];
    overview
}

/// A file read in full, not loaded by the CLI on its own.
fn file(path: &str, kind: ContextFileKind, content: &str, usage_note: &str) -> ContextFile {
    ContextFile {
        path: path.into(),
        kind,
        size: content.len() as u64,
        content: Some(content.into()),
        note: None,
        hidden_chars: false,
        used_by_agents: false,
        usage_note: usage_note.into(),
    }
}

/// `?<name>` in the page URL.
fn flag(name: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .is_some_and(|s| s.trim_start_matches('?').split('&').any(|p| p == name))
}
