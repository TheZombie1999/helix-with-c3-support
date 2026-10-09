//! A lightweight, file-based review-comment system backing the `:review`
//! typable command.
//!
//! `:review <comment>` appends a structured entry to `REVIEW.md` at the
//! current file's workspace root — referencing the cursor position, or, if
//! the user actually has a region selected, the selected range and its
//! text. Helix reads that same file back and surfaces its entries as
//! diagnostics (gutter signs, `]d`/`[d` navigation, the status line count)
//! wherever a reviewed file is open.
//!
//! There's no in-memory queue and no D-Bus: `REVIEW.md` itself is the
//! interchange format, so an external tool (e.g. an AI coding agent) can
//! read the same file directly.
//!
//! Known limitation: the dedicated `:dp` diagnostics picker reads from a
//! separate, LSP-only store (`Editor::diagnostics`) that injected
//! diagnostics never reach, so review comments won't show up there — every
//! other diagnostic surface (gutter, navigation, status line) does show
//! them.
//!
//! Review comments are displayed as a short end-of-line note (styled like a
//! code comment) rather than the usual underline-plus-box treatment LSP
//! diagnostics get: they're tagged with [`DiagnosticProvider::Helix`] (no
//! backing language server), which both the underline highlighter and the
//! generic inline-diagnostics renderer know to skip, letting a dedicated
//! `ReviewComments` decoration (`helix-term/src/ui/text_decorations/review.rs`)
//! render them instead.

use std::{fs::OpenOptions, io::Write as _, path::Path};

use helix_core::diagnostic::{Diagnostic, DiagnosticProvider, Range as DiagnosticRange, Severity};
use helix_view::{Document, DocumentId, Editor};

const REVIEW_FILE: &str = "REVIEW.md";

fn provider() -> DiagnosticProvider {
    DiagnosticProvider::Helix
}

struct Entry {
    /// As written in `REVIEW.md`, relative to the workspace root.
    path: String,
    /// 0-indexed, inclusive.
    start_line: usize,
    /// 0-indexed, inclusive.
    end_line: usize,
    comment: String,
}

/// Appends one entry to `<workspace_root>/REVIEW.md`, creating the file
/// (with a header) if it doesn't exist yet. `start_line`/`end_line` are
/// 0-indexed and inclusive; pass them equal for a plain cursor position.
/// `snippet`, if given, is `(language, selected text)` and is rendered as a
/// quoted fenced code block.
pub fn append_entry(
    workspace_root: &Path,
    rel_path: &str,
    start_line: usize,
    end_line: usize,
    snippet: Option<(&str, &str)>,
    comment: &str,
) -> std::io::Result<()> {
    let review_path = workspace_root.join(REVIEW_FILE);
    let is_new = !review_path.exists();

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&review_path)?;

    if is_new {
        file.write_all(b"# Review\n\n")?;
    }

    if start_line == end_line {
        writeln!(file, "## {rel_path}:{}", start_line + 1)?;
    } else {
        writeln!(file, "## {rel_path}:{}-{}", start_line + 1, end_line + 1)?;
    }
    file.write_all(b"\n")?;

    if let Some((language, text)) = snippet {
        writeln!(file, "```{language}")?;
        file.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        writeln!(file, "```")?;
        file.write_all(b"\n")?;
    }

    writeln!(file, "{}", comment.trim())?;
    file.write_all(b"\n---\n\n")?;

    Ok(())
}

/// Parses `<workspace_root>/REVIEW.md`. Returns an empty list if the file
/// doesn't exist; malformed headings/entries are skipped rather than
/// treated as an error — this feeds diagnostics, not something that should
/// ever block opening a file.
fn parse_entries(workspace_root: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(workspace_root.join(REVIEW_FILE)) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let Some(header) = line.strip_prefix("## ") else {
            continue;
        };
        let Some((path, range)) = header.rsplit_once(':') else {
            continue;
        };
        let (start, end) = range.split_once('-').unwrap_or((range, range));
        let (Ok(start_line), Ok(end_line)) = (start.parse::<usize>(), end.parse::<usize>()) else {
            continue;
        };
        if start_line == 0 || end_line < start_line {
            continue;
        }

        // The body runs up to the next `## ` heading, a lone `---`
        // separator, or EOF. A fenced snippet block (if this entry has
        // one) is skipped rather than folded into the comment text.
        let mut body = Vec::new();
        let mut in_fence = false;
        loop {
            match lines.peek() {
                None => break,
                Some(next) if next.starts_with("## ") => break,
                Some(next) if *next == "---" && !in_fence => {
                    lines.next();
                    break;
                }
                Some(next) => {
                    if next.starts_with("```") {
                        in_fence = !in_fence;
                    } else if !in_fence {
                        body.push(*next);
                    }
                    lines.next();
                }
            }
        }
        let comment = body.join("\n").trim().to_string();
        if comment.is_empty() {
            continue;
        }

        entries.push(Entry {
            path: path.to_string(),
            start_line: start_line - 1,
            end_line: end_line - 1,
            comment,
        });
    }

    entries
}

/// Builds the diagnostics `doc` should show from the `REVIEW.md` entries
/// that reference its path.
fn diagnostics_for(doc: &Document, workspace_root: &Path, entries: &[Entry]) -> Vec<Diagnostic> {
    let Some(doc_path) = doc.path() else {
        return Vec::new();
    };
    let doc_path = helix_stdx::path::canonicalize(doc_path);

    let text = doc.text().slice(..);
    let max_line = text.len_lines().saturating_sub(1);

    entries
        .iter()
        .filter(|entry| {
            helix_stdx::path::canonicalize(workspace_root.join(&entry.path)) == doc_path
        })
        .filter_map(|entry| {
            let start_line = entry.start_line.min(max_line);
            let end_line = entry.end_line.min(max_line);
            let start = text.line_to_char(start_line);
            let end = text.line_to_char((end_line + 1).min(text.len_lines()));
            if start >= end {
                return None;
            }

            Some(Diagnostic {
                range: DiagnosticRange { start, end },
                ends_at_word: false,
                starts_at_word: false,
                zero_width: false,
                line: start_line,
                message: entry.comment.clone(),
                severity: Some(Severity::Hint),
                code: None,
                provider: provider(),
                tags: Vec::new(),
                source: Some("review".to_string()),
                data: None,
            })
        })
        .collect()
}

/// Re-reads `REVIEW.md` and refreshes `doc_id`'s review diagnostics from
/// it, leaving any real LSP diagnostics untouched. Safe to call any time —
/// on open, and right after `:review` appends a new entry.
pub fn refresh_diagnostics(editor: &mut Editor, doc_id: DocumentId) {
    let Some(doc) = editor.documents.get(&doc_id) else {
        return;
    };
    let workspace_root = doc.workspace_root().to_path_buf();
    let entries = parse_entries(&workspace_root);

    let doc = editor.documents.get_mut(&doc_id).unwrap();
    let diagnostics = diagnostics_for(doc, &workspace_root, &entries);
    doc.replace_diagnostics(diagnostics, &[], Some(&provider()));
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_point_and_range_entries() {
        let dir = tempfile::tempdir().unwrap();
        append_entry(dir.path(), "src/lib.rs", 4, 4, None, "fix this typo").unwrap();
        append_entry(
            dir.path(),
            "src/lib.rs",
            9,
            11,
            Some(("rust", "fn foo() {\n    todo!()\n}\n")),
            "this needs error handling",
        )
        .unwrap();
        append_entry(dir.path(), "src/other.rs", 0, 0, None, "unrelated file").unwrap();

        let entries = parse_entries(dir.path());
        assert_eq!(entries.len(), 3);

        assert_eq!(entries[0].path, "src/lib.rs");
        assert_eq!(entries[0].start_line, 4);
        assert_eq!(entries[0].end_line, 4);
        assert_eq!(entries[0].comment, "fix this typo");

        assert_eq!(entries[1].path, "src/lib.rs");
        assert_eq!(entries[1].start_line, 9);
        assert_eq!(entries[1].end_line, 11);
        assert_eq!(entries[1].comment, "this needs error handling");

        assert_eq!(entries[2].path, "src/other.rs");
    }

    #[test]
    fn missing_file_parses_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_entries(dir.path()).is_empty());
    }
}
