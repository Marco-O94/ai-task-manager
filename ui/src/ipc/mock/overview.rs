//! Mock of `get_project_overview` (spec F3). Owner: UI-SHELL.
//!
//! The demo project gets a full repository: CLAUDE.md, `.claude/CLAUDE.md` as a link, an
//! AGENTS.md with a bidirectional character, a README too large to show, `.claude/settings.json`
//! with its secrets masked, `.mcp.json` servers and `.claude/` agents, commands and skills.
//! Any other project gets a bare one (README only). `?overview_error` answers with the error
//! of a default target branch that cannot be read.

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
        overview.files.push(file(
            "README.md",
            ContextFileKind::Readme,
            "# sito-web\n\nSito statico: `npm run build` genera `dist/`.\n",
            "solo contesto per te",
        ));
        return overview;
    }

    // The usage notes are the backend's (`git/overview.rs`, `usage`).
    let (memory, nested_memory, config) = if loaded {
        (
            "caricato all'avvio",
            "caricato all'avvio",
            "caricato all'avvio",
        )
    } else {
        (
            "letto dall'agente su istruzione del prompt",
            "non caricato in modalità Isolata: il prompt fa leggere solo il CLAUDE.md della radice",
            "non caricato in modalità Isolata",
        )
    };
    overview.files = vec![
        ContextFile {
            used_by_agents: loaded,
            ..file(
                "CLAUDE.md",
                ContextFileKind::Memory,
                "# demo\n\n\
                 API REST in Rust (axum) con parser e lista utenti.\n\n\
                 ## Regole\n\
                 - Esegui `cargo test` e `cargo clippy -- -D warnings` prima di chiudere un task.\n\
                 - Niente dipendenze nuove senza chiedere.\n\
                 - Messaggi di errore per l'utente in italiano.\n\n\
                 ## Struttura\n\
                 - `src/api/`: handler HTTP\n\
                 - `src/parser/`: parser dei filtri\n\
                 - `tests/`: test di integrazione (database in memoria)\n",
                memory,
            )
        },
        ContextFile {
            size: "../CLAUDE.md".len() as u64,
            content: None,
            note: Some("→ ../CLAUDE.md".into()),
            used_by_agents: loaded,
            ..file(
                ".claude/CLAUDE.md",
                ContextFileKind::Memory,
                "",
                nested_memory,
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
        ContextFile {
            size: 91_136,
            content: None,
            note: Some("troppo grande (89 KiB)".into()),
            ..file(
                "README.md",
                ContextFileKind::Readme,
                "",
                "solo contesto per te",
            )
        },
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
