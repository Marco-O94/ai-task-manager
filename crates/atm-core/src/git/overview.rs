//! Project overview (spec F3): the files that give a project's agents their context and
//! configuration (`CLAUDE.md`, `.claude/CLAUDE.md`, `AGENTS.md`, `README.md`,
//! `.claude/settings.json`, `.mcp.json`) and the `.claude/agents|commands|skills` listings,
//! read from one commit through the hardened runner (never the main checkout, never `$HOME`).
//! Owner: CORE-OVERVIEW. [`Source`] and [`read`]'s signature are frozen: `Core` calls it as is.
//!
//! One `git ls-tree -z -l` of the fixed paths gives each file's mode, type, id and size; the
//! blobs of at most [`MAX_FILE_BYTES`] are then read by id through one `cat-file --batch`,
//! each asked for with exactly its size (an object larger than asked would be left unread and
//! the reader out of step for every file after it). Nothing is followed: a symlink shows its
//! target, a tree at one of the paths is skipped, and so is `.claude` when it is a link.
//!
//! Text is shown as committed (CRLF line ends as LF), with every hidden or bidirectional
//! character ([`crate::is_hidden_char`]) but line feeds and tabs written `⟨U+XXXX⟩`. The two
//! JSON files are shown parsed and pretty-printed (keys sorted) with their secrets masked
//! ([`mask`]); the servers of `.mcp.json` are summarized by name, never by value. The listings
//! come from the configuration records the core already walked ([`Source::records`]).

use std::collections::BTreeSet;
use std::path::Path;

use atm_types::{AppError, ContextFile, ContextFileKind, McpServer, ProjectOverview};
use serde_json::Value;

use super::{
    BILLING_SETTINGS_KEYS, CatFile, CatObject, Git, RecordDigest, RunOpts, check_rev, failure,
};

/// What [`read`] reads, as `Core::get_project_overview` resolved it.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    /// Canonical toplevel of the main checkout; never the home directory (refused before).
    pub repo: &'a Path,
    /// `ProjectOverview::project_id`.
    pub project_id: &'a str,
    /// The project's default target branch and its tip (full hex id): the commit read.
    pub branch: &'a str,
    pub commit: &'a str,
    /// Policy Trusted and `Project::trusted`: the agents load the repository's configuration
    /// (`ProjectOverview::agents_load_config`, and each file's `used_by_agents`).
    pub agents_load_config: bool,
    /// Records of the configuration committed in `commit` (`ConfigSnapshot::records`, cached
    /// by the core; kinds and paths as in `git/fingerprint.rs`): `.claude/**` and `.mcp.json`
    /// as a checkout has them, plus the files that configuration runs. The `.claude/agents`,
    /// `.claude/commands` and `.claude/skills` listings come from here. Empty when the
    /// configuration could not be walked (a limit, a link out of the tree).
    pub records: &'a [RecordDigest],
}

/// A blob larger than this is not read: `troppo grande (N KiB)`.
pub const MAX_FILE_BYTES: u64 = 64 << 10;
/// Characters of [`McpServer::target`] at most, the last one `…` when cut.
pub const MAX_TARGET_CHARS: usize = 200;

/// The files of [`ProjectOverview::files`], in its order, and what each is to the CLI.
const FILES: [(&str, ContextFileKind); 6] = [
    ("CLAUDE.md", ContextFileKind::Memory),
    (".claude/CLAUDE.md", ContextFileKind::Memory),
    ("AGENTS.md", ContextFileKind::Agents),
    ("README.md", ContextFileKind::Readme),
    (".claude/settings.json", ContextFileKind::Settings),
    (".mcp.json", ContextFileKind::Mcp),
];
/// What a masked secret shows.
const MASK: &str = "•••";
/// The mode of a symlink blob.
const LINK_MODE: &str = "120000";
/// The schemes a WHATWG URL parser calls special ([`strip_url`]).
const SPECIAL_SCHEMES: [&str; 6] = ["ftp", "file", "http", "https", "ws", "wss"];
/// A longer symlink target is not read (`PATH_MAX` of macOS: no checkout has one).
const MAX_LINK_TARGET: u64 = 1024;
/// stdout of the `ls-tree`: six entries, more only in a crafted tree that repeats a name.
const MAX_LISTING: usize = 64 << 10;

/// The overview of `src.commit` in `src.repo`: one `ls-tree` of the fixed paths, then the
/// blobs small enough to show through `cat-file`; secrets masked, hidden characters replaced.
/// Errors: `Git` (the runner failed), `Invalid` (a bad revision).
pub async fn read(git: &Git, src: &Source<'_>) -> Result<ProjectOverview, AppError> {
    check_rev(src.commit)?;
    let mut args = vec!["ls-tree", "-z", "-l", "--end-of-options", src.commit, "--"];
    args.extend(FILES.map(|(path, _)| path));
    let (out, overflow) = git
        .exec(src.repo, &args, &RunOpts::read(), MAX_LISTING)
        .await?;
    if overflow {
        return Err(AppError::git(format!(
            "git ls-tree: output oltre {} KiB",
            MAX_LISTING >> 10
        )));
    }
    if out.code != 0 {
        return Err(failure(&args, &out));
    }
    let blobs = blobs(&out.stdout);
    let mut overview = ProjectOverview {
        project_id: src.project_id.to_owned(),
        branch: src.branch.to_owned(),
        commit: src.commit.to_owned(),
        agents_load_config: src.agents_load_config,
        files: Vec::new(),
        mcp_servers: Vec::new(),
        claude_agents: markdown_names(src.records, ".claude/agents/"),
        claude_commands: markdown_names(src.records, ".claude/commands/"),
        claude_skills: skill_names(src.records),
    };
    // Started for the first blob read, then kept for the others.
    let mut reader = None;
    for (path, kind) in FILES {
        let Some(blob) = blobs.iter().find(|b| b.path == path.as_bytes()) else {
            continue;
        };
        let shown = show(git, src.repo, &mut reader, blob, kind).await?;
        if kind == ContextFileKind::Mcp {
            overview.mcp_servers = shown.servers;
        }
        let (used_by_agents, usage_note) = usage(path, kind, src.agents_load_config);
        overview.files.push(ContextFile {
            path: path.to_owned(),
            kind,
            size: blob.size,
            content: shown.content,
            note: shown.note,
            hidden_chars: shown.hidden,
            used_by_agents,
            usage_note: usage_note.to_owned(),
        });
    }
    Ok(overview)
}

/// One blob entry of `ls-tree -z -l`.
struct Blob<'a> {
    mode: &'a str,
    oid: &'a str,
    size: u64,
    path: &'a [u8],
}

/// The blob entries of `ls-tree -z -l` output (`<mode> blob <oid> <size>\t<path>\0`), in its
/// order: trees and submodules at the paths asked for are left out.
fn blobs(out: &[u8]) -> Vec<Blob<'_>> {
    out.split(|&b| b == 0)
        .filter_map(|record| {
            let tab = record.iter().position(|&b| b == b'\t')?;
            let head = std::str::from_utf8(&record[..tab]).ok()?;
            let mut fields = head.split_ascii_whitespace();
            let (mode, kind, oid, size) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (kind == "blob").then_some(Blob {
                mode,
                oid,
                size: size.parse().ok()?,
                path: &record[tab + 1..],
            })
        })
        .collect()
}

/// What one file shows.
#[derive(Default)]
struct Shown {
    content: Option<String>,
    /// Italian: why `content` is missing or altered.
    note: Option<String>,
    hidden: bool,
    /// `.mcp.json` only.
    servers: Vec<McpServer>,
}

impl Shown {
    fn note(note: impl Into<String>) -> Shown {
        Shown {
            note: Some(note.into()),
            ..Shown::default()
        }
    }
}

/// `blob`, the file `kind` of the overview: a link by its target, a file too large or binary
/// by a note, a JSON file parsed and masked, any text with its hidden characters visible.
async fn show(
    git: &Git,
    repo: &Path,
    reader: &mut Option<CatFile>,
    blob: &Blob<'_>,
    kind: ContextFileKind,
) -> Result<Shown, AppError> {
    if blob.mode == LINK_MODE {
        if blob.size > MAX_LINK_TARGET {
            return Ok(Shown::note("collegamento simbolico troppo lungo"));
        }
        let data = read_blob(git, repo, reader, blob).await?;
        let (target, hidden) = visible(&String::from_utf8_lossy(&data), false);
        return Ok(Shown {
            hidden,
            ..Shown::note(format!("→ {target}"))
        });
    }
    if blob.size > MAX_FILE_BYTES {
        return Ok(Shown::note(format!(
            "troppo grande ({} KiB)",
            blob.size.div_ceil(1024)
        )));
    }
    let data = read_blob(git, repo, reader, blob).await?;
    let Some(text) = String::from_utf8(data).ok().filter(|t| !t.contains('\0')) else {
        return Ok(Shown::note("binario"));
    };
    let mut servers = Vec::new();
    let (text, masked) = match kind {
        ContextFileKind::Settings | ContextFileKind::Mcp => {
            let Ok(mut json) = serde_json::from_str::<Value>(&text) else {
                return Ok(Shown::note("JSON non valido"));
            };
            if kind == ContextFileKind::Mcp {
                servers = mcp_servers(&json);
            }
            let masked = mask(&mut json);
            (serde_json::to_string_pretty(&json)?, masked)
        }
        ContextFileKind::Memory | ContextFileKind::Agents | ContextFileKind::Readme => {
            (text, false)
        }
    };
    let (content, hidden) = visible(&text, true);
    Ok(Shown {
        content: Some(content),
        note: masked.then(|| "valori segreti mascherati".to_owned()),
        hidden,
        servers,
    })
}

/// The bytes of `blob`, asked for with exactly its listed size; the reader is started on the
/// first call. Errors: `Git` (the reader failed, or the object is not the blob listed).
async fn read_blob(
    git: &Git,
    repo: &Path,
    reader: &mut Option<CatFile>,
    blob: &Blob<'_>,
) -> Result<Vec<u8>, AppError> {
    let cat = match reader {
        Some(cat) => cat,
        None => reader.insert(git.cat_file(repo).await?),
    };
    match cat.get(blob.oid, blob.size).await? {
        CatObject::Found { kind, data, .. } if kind == "blob" => Ok(data),
        _ => Err(AppError::git(format!(
            "git cat-file: {} non è il blob di {} byte elencato",
            blob.oid, blob.size
        ))),
    }
}

/// Whether the CLI loads the file on its own in this project's agents, and how they get it
/// (Italian). Isolated agents run with `--setting-sources=user --strict-mcp-config`: no
/// project memory, settings or servers; the prompt asks them to read the root CLAUDE.md or
/// AGENTS.md (spec §7.3), never `.claude/CLAUDE.md`.
fn usage(path: &str, kind: ContextFileKind, loaded: bool) -> (bool, &'static str) {
    match kind {
        ContextFileKind::Readme => (false, "solo contesto per te"),
        ContextFileKind::Agents => (false, "letto su istruzione del prompt"),
        ContextFileKind::Memory | ContextFileKind::Settings | ContextFileKind::Mcp if loaded => {
            (true, "caricato all'avvio")
        }
        ContextFileKind::Memory if path == "CLAUDE.md" => {
            (false, "letto dall'agente su istruzione del prompt")
        }
        ContextFileKind::Memory => (
            false,
            "non caricato in modalità Isolata: il prompt fa leggere solo il CLAUDE.md della \
             radice",
        ),
        ContextFileKind::Settings | ContextFileKind::Mcp => {
            (false, "non caricato in modalità Isolata")
        }
    }
}

/// `text` with every hidden or bidirectional character ([`crate::is_hidden_char`]) written
/// `⟨U+XXXX⟩`, except line feeds and tabs when `lines` (a CRLF line end then becomes LF),
/// and whether it had one.
fn visible(text: &str, lines: bool) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut hidden = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if lines && c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        if crate::is_hidden_char(c) && !(lines && matches!(c, '\n' | '\t')) {
            hidden = true;
            out.push_str(&format!("⟨U+{:04X}⟩", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    (out, hidden)
}

/// Masks the secrets of a parsed settings file or `.mcp.json` in place, at any depth: every
/// value of `env` and `headers`, of a `*Helper` key and of the other credential helpers
/// ([`BILLING_SETTINGS_KEYS`]), and the userinfo, query and fragment of a `url`
/// ([`strip_url`]). Whether it changed anything.
fn mask(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                changed |= match (key.as_str(), value) {
                    ("env" | "headers", Value::Object(secrets)) => secrets
                        .values_mut()
                        .fold(false, |c, secret| hide(secret) | c),
                    ("env" | "headers", value) => hide(value),
                    ("url", Value::String(url)) => {
                        let stripped = strip_url(url);
                        let differs = stripped != *url;
                        *url = stripped;
                        differs
                    }
                    (key, value)
                        if key.ends_with("Helper") || BILLING_SETTINGS_KEYS.contains(&key) =>
                    {
                        hide(value)
                    }
                    (_, value) => mask(value),
                };
            }
        }
        Value::Array(items) => {
            for item in items {
                changed |= mask(item);
            }
        }
        _ => {}
    }
    changed
}

/// Replaces a secret with [`MASK`] (`null`, no secret, stays). Whether it changed.
fn hide(value: &mut Value) -> bool {
    if value.is_null() || *value == MASK {
        return false;
    }
    *value = Value::from(MASK);
    true
}

/// `url` without its userinfo, query and fragment, split as a WHATWG URL parser (the CLI's)
/// splits it. The text is first normalized as that parser does: leading and trailing C0
/// controls and spaces trimmed, tabs and line breaks removed anywhere. After a special scheme
/// ([`SPECIAL_SCHEMES`], any case) any run of `/` and `\` leads to the authority, which runs
/// to the first `/`, `\`, `?` or `#`; after another scheme only `//` does, and the authority
/// ends at the first `/`, `?` or `#`. Its userinfo runs to its last `@`. Any other text (no
/// scheme, or another scheme without `//`) is split as after a special scheme, from its start.
fn strip_url(url: &str) -> String {
    let url: String = url
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let (prefix, rest, special) = match url.split_once(':') {
        Some((scheme, rest)) if is_scheme(scheme) => {
            if SPECIAL_SCHEMES
                .iter()
                .any(|s| s.eq_ignore_ascii_case(scheme))
            {
                let body = rest.trim_start_matches(['/', '\\']);
                (&url[..url.len() - body.len()], body, true)
            } else if let Some(body) = rest.strip_prefix("//") {
                (&url[..url.len() - body.len()], body, false)
            } else {
                ("", url.as_str(), true)
            }
        }
        _ => ("", url.as_str(), true),
    };
    let end = if special {
        rest.find(['/', '\\', '?', '#'])
    } else {
        rest.find(['/', '?', '#'])
    };
    let (authority, path) = rest.split_at(end.unwrap_or(rest.len()));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let path = path.split(['?', '#']).next().unwrap_or_default();
    format!("{prefix}{host}{path}")
}

/// RFC 3986 `scheme`: a letter, then letters, digits, `+`, `-`, `.`.
fn is_scheme(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The servers of a parsed `.mcp.json` (`mcpServers`), sorted by name: transport (`type`,
/// else `stdio` with a `command`, `http` with a `url`, else `sconosciuto`), target (the
/// command and its arguments, or the URL stripped by [`strip_url`], at most
/// [`MAX_TARGET_CHARS`]) and the names of `env` and `headers`, never a value.
fn mcp_servers(json: &Value) -> Vec<McpServer> {
    let Some(servers) = json.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut servers: Vec<McpServer> = servers
        .iter()
        .map(|(name, server)| {
            let text = |key| server.get(key).and_then(Value::as_str);
            let (command, url) = (text("command"), text("url"));
            let transport = match text("type") {
                Some(kind) => kind,
                None if command.is_some() => "stdio",
                None if url.is_some() => "http",
                None => "sconosciuto",
            };
            let target = match (command, url) {
                (Some(command), _) if url.is_none() || transport == "stdio" => {
                    let args = server.get("args").and_then(Value::as_array);
                    std::iter::once(command)
                        .chain(args.into_iter().flatten().filter_map(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" ")
                }
                (_, Some(url)) => strip_url(url),
                _ => String::new(),
            };
            let keys = |key| {
                let mut keys: Vec<String> = server
                    .get(key)
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|map| map.keys().map(|k| visible(k, false).0))
                    .collect();
                keys.sort();
                keys
            };
            McpServer {
                name: visible(name, false).0,
                transport: visible(transport, false).0,
                target: visible(&capped(&target), false).0,
                env_keys: keys("env"),
                header_keys: keys("headers"),
            }
        })
        .collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    servers
}

/// `text` cut to [`MAX_TARGET_CHARS`] characters, the last one `…`.
fn capped(text: &str) -> String {
    if text.chars().count() <= MAX_TARGET_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_TARGET_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The agents or commands under `dir` (`.claude/agents/`, `.claude/commands/`): each `.md`
/// file by its path below `dir` without the extension (`reviewer`, `frontend/test`).
fn markdown_names(records: &[RecordDigest], dir: &str) -> Vec<String> {
    names(records, |path| path.strip_prefix(dir)?.strip_suffix(".md"))
}

/// The skills: the folder of each `.claude/skills/<name>/SKILL.md`.
fn skill_names(records: &[RecordDigest]) -> Vec<String> {
    names(records, |path| {
        let name = path
            .strip_prefix(".claude/skills/")?
            .strip_suffix("/SKILL.md")?;
        (!name.contains('/')).then_some(name)
    })
}

/// The non-empty `name`s of the regular files in `records` (a link counts by what it resolves
/// to, which the walk records under the link's own path), with their hidden characters
/// visible; sorted, without repeats.
fn names<'a>(
    records: &'a [RecordDigest],
    name: impl Fn(&'a str) -> Option<&'a str>,
) -> Vec<String> {
    records
        .iter()
        .filter(|r| matches!(r.kind, 'f' | 'x'))
        .filter_map(|r| name(&r.path))
        .filter(|name| !name.is_empty())
        .map(|name| visible(name, false).0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
