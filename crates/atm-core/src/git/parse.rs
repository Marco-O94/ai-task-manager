//! Pure parsers of git's machine-readable output (spec §8). Every `-z` format is split on NUL.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use atm_types::{DiffLine, FileStatus, LineKind};

use super::WorktreeEntry;

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fields(out: &[u8]) -> impl Iterator<Item = &[u8]> {
    out.split(|b| *b == 0)
}

/// Parses `worktree list --porcelain -z`.
pub fn parse_worktree_list(out: &[u8]) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for field in fields(out) {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|b| *b == b' ') {
            Some(i) => (&field[..i], &field[i + 1..]),
            None => (field, &[][..]),
        };
        if key == b"worktree" {
            entries.extend(current.take());
            current = Some(WorktreeEntry {
                path: PathBuf::from(OsStr::from_bytes(value)),
                head: None,
                branch: None,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key {
            b"HEAD" => entry.head = Some(lossy(value)),
            b"branch" => {
                let full = lossy(value);
                entry.branch = Some(full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned());
            }
            b"locked" => entry.locked = true,
            b"prunable" => entry.prunable = true,
            _ => {} // bare, detached
        }
    }
    entries.extend(current);
    entries
}

/// Parses the hunks of one file's unified diff into numbered lines (spec §8.6); file headers
/// before the first `@@` are skipped.
pub fn parse_hunks(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    // Lines still expected in the current hunk (old, new) and the next line numbers.
    let mut left = (0u32, 0u32);
    let mut next = (0u32, 0u32);
    for raw in patch.lines() {
        let in_hunk = left != (0, 0);
        if !in_hunk || raw.starts_with("@@") {
            if let Some((old, new)) = hunk_header(raw) {
                next = (old.0, new.0);
                left = (old.1, new.1);
                lines.push(line(LineKind::Hunk, None, None, raw));
            } else if raw.starts_with('\\') && !lines.is_empty() {
                // `\ No newline at end of file` after the last line of a hunk.
                lines.push(line(LineKind::Meta, None, None, raw));
            }
            continue;
        }
        // An empty line is a blank context line when `diff.suppressBlankEmpty` is set.
        let (prefix, text) = match raw.chars().next() {
            Some(c) => (c, &raw[c.len_utf8()..]),
            None => (' ', ""),
        };
        match prefix {
            ' ' => {
                lines.push(line(LineKind::Context, Some(next.0), Some(next.1), text));
                next = (next.0 + 1, next.1 + 1);
                left = (left.0.saturating_sub(1), left.1.saturating_sub(1));
            }
            '-' => {
                lines.push(line(LineKind::Del, Some(next.0), None, text));
                next.0 += 1;
                left.0 = left.0.saturating_sub(1);
            }
            '+' => {
                lines.push(line(LineKind::Add, None, Some(next.1), text));
                next.1 += 1;
                left.1 = left.1.saturating_sub(1);
            }
            '\\' => lines.push(line(LineKind::Meta, None, None, raw)),
            _ => left = (0, 0), // malformed: wait for the next header
        }
    }
    lines
}

fn line(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> DiffLine {
    DiffLine {
        kind,
        old_no,
        new_no,
        text: text.to_owned(),
    }
}

/// `@@ -a[,b] +c[,d] @@…` → `((a, b), (c, d))`; counts default to 1.
fn hunk_header(raw: &str) -> Option<((u32, u32), (u32, u32))> {
    let rest = raw.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    Some((range(old)?, range(new)?))
}

fn range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// Entries of `status --porcelain=v1 -z`: path, and the source of a rename or copy.
pub(crate) fn status_entries(out: &[u8]) -> Vec<(&[u8], Option<&[u8]>)> {
    let mut it = fields(out);
    let mut entries = Vec::new();
    while let Some(entry) = it.next() {
        // `XY <path>`; the only shorter field is the empty one after the last NUL.
        if entry.len() < 4 {
            continue;
        }
        let source = if entry[..2].iter().any(|b| matches!(b, b'R' | b'C')) {
            it.next()
        } else {
            None
        };
        entries.push((&entry[3..], source));
    }
    entries
}

/// One entry of `diff --name-status -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameStatus {
    pub status: FileStatus,
    pub path: String,
    pub old_path: Option<String>,
}

pub(crate) fn parse_name_status(out: &[u8]) -> Vec<NameStatus> {
    let mut it = fields(out);
    let mut entries = Vec::new();
    while let Some(code) = it.next() {
        let Some(&letter) = code.first() else {
            continue;
        };
        let Some(first) = it.next() else { break };
        let status = match letter {
            b'A' => FileStatus::Added,
            b'D' => FileStatus::Deleted,
            b'R' => FileStatus::Renamed,
            b'C' => FileStatus::Copied,
            b'T' => FileStatus::TypeChanged,
            _ => FileStatus::Modified,
        };
        let entry = if matches!(status, FileStatus::Renamed | FileStatus::Copied) {
            let Some(second) = it.next() else { break };
            NameStatus {
                status,
                path: lossy(second),
                old_path: Some(lossy(first)),
            }
        } else {
            NameStatus {
                status,
                path: lossy(first),
                old_path: None,
            }
        };
        entries.push(entry);
    }
    entries
}

/// `diff --numstat -z`: new path → (additions, deletions, binary).
pub(crate) fn parse_numstat(out: &[u8]) -> HashMap<String, (u32, u32, bool)> {
    let mut it = fields(out);
    let mut stats = HashMap::new();
    while let Some(field) = it.next() {
        if field.is_empty() {
            continue;
        }
        let text = lossy(field);
        let mut parts = text.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // A rename has an empty path here, then the source and the destination.
        let path = if path.is_empty() {
            it.next();
            it.next().map(lossy).unwrap_or_default()
        } else {
            path.to_owned()
        };
        let binary = add == "-";
        stats.insert(
            path,
            (add.parse().unwrap_or(0), del.parse().unwrap_or(0), binary),
        );
    }
    stats
}

/// `merge-tree --write-tree --name-only --no-messages -z`: the tree, then conflicted paths.
pub(crate) fn parse_merge_tree(out: &[u8]) -> (String, Vec<String>) {
    let mut it = fields(out);
    let tree = it.next().map(lossy).unwrap_or_default();
    let files = it.take_while(|f| !f.is_empty()).map(lossy).collect();
    (tree, files)
}

/// `git version 2.54.0` or `git version 2.39.5 (Apple Git-154)` → `("2.54.0", (2, 54))`.
pub(crate) fn parse_version(out: &str) -> Option<(String, (u32, u32))> {
    let version = out
        .strip_prefix("git version ")?
        .split_whitespace()
        .next()?;
    Some((version.to_owned(), major_minor(version)?))
}

pub(crate) fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lists_rename_sources() {
        assert!(status_entries(b"").is_empty());
        let entries = status_entries(b" M a b\0R  new\0old\0?? u\0");
        let expected: [(&[u8], Option<&[u8]>); 3] =
            [(b"a b", None), (b"new", Some(b"old")), (b"u", None)];
        assert_eq!(entries, expected);
    }

    #[test]
    fn name_status_and_numstat() {
        let ns = parse_name_status(b"M\0a b\0R100\0old\0new\0A\0x\0");
        assert_eq!(ns.len(), 3);
        assert_eq!(ns[1].path, "new");
        assert_eq!(ns[1].old_path.as_deref(), Some("old"));
        let st = parse_numstat(b"1\t2\ta b\0" as &[u8]);
        assert_eq!(st["a b"], (1, 2, false));
        let st = parse_numstat(b"0\t0\t\0old\0new\0-\t-\tbin\0");
        assert_eq!(st["new"], (0, 0, false));
        assert_eq!(st["bin"], (0, 0, true));
    }

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some(("2.39.5".into(), (2, 39)))
        );
        assert_eq!(parse_version("nope"), None);
    }

    #[test]
    fn merge_tree_output() {
        assert_eq!(
            parse_merge_tree(b"abc\0f.txt\0g h\0"),
            ("abc".into(), vec!["f.txt".into(), "g h".into()])
        );
        assert_eq!(parse_merge_tree(b"abc\0"), ("abc".into(), vec![]));
    }
}
