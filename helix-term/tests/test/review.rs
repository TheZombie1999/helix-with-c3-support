use helix_core::diagnostic::Severity;
use helix_view::doc;

use super::helpers::*;

fn workspace_with_file(content: &str) -> anyhow::Result<(tempfile::TempDir, std::path::PathBuf)> {
    let workspace = tempfile::tempdir()?;
    std::fs::create_dir(workspace.path().join(".git"))?;
    let file_path = workspace.path().join("lib.rs");
    std::fs::write(&file_path, content)?;
    Ok((workspace, file_path))
}

/// With nothing highlighted beyond the default single-char cursor, `:review`
/// attaches the comment to the cursor's line as a point entry (`path:N`,
/// no quoted snippet).
#[tokio::test(flavor = "multi_thread")]
async fn review_on_cursor_writes_point_entry() -> anyhow::Result<()> {
    let (workspace, file_path) = workspace_with_file("fn main() {\n    let x = 1;\n}\n")?;
    let mut app = AppBuilder::new().with_file(file_path, None).build()?;

    test_key_sequence(
        &mut app,
        Some(":review fix this<ret>"),
        Some(&|app| {
            let doc = doc!(app.editor);
            let diagnostics = doc.diagnostics();
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].message, "fix this");
            assert_eq!(diagnostics[0].severity, Some(Severity::Hint));
        }),
        false,
    )
    .await?;

    let review_md = std::fs::read_to_string(workspace.path().join("REVIEW.md"))?;
    assert!(review_md.contains("## lib.rs:1"));
    assert!(review_md.contains("fix this"));
    assert!(!review_md.contains("```"));

    Ok(())
}

/// With a real multi-character selection, `:review` attaches the comment to
/// the selected range (`path:N-M`) and quotes the selected text in a fenced
/// snippet.
#[tokio::test(flavor = "multi_thread")]
async fn review_on_selection_writes_range_entry_with_snippet() -> anyhow::Result<()> {
    let (workspace, file_path) =
        workspace_with_file("fn main() {\n    let x = 1;\n    let y = 2;\n}\n")?;
    let mut app = AppBuilder::new().with_file(file_path, None).build()?;

    test_key_sequence(
        &mut app,
        // select from the start of the file, extending down two lines, then comment.
        Some("vjj:review needs error handling<ret>"),
        Some(&|app| {
            let doc = doc!(app.editor);
            let diagnostics = doc.diagnostics();
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].message, "needs error handling");
        }),
        false,
    )
    .await?;

    let review_md = std::fs::read_to_string(workspace.path().join("REVIEW.md"))?;
    assert!(review_md.contains("## lib.rs:1-3"));
    assert!(review_md.contains("```rust"));
    assert!(review_md.contains("fn main() {"));
    assert!(review_md.contains("let x = 1;"));
    assert!(review_md.contains("needs error handling"));

    Ok(())
}

/// Re-opening a file with existing `REVIEW.md` entries surfaces them as
/// diagnostics immediately, without needing to run `:review` again.
#[tokio::test(flavor = "multi_thread")]
async fn existing_review_entries_load_as_diagnostics_on_open() -> anyhow::Result<()> {
    let (workspace, file_path) = workspace_with_file("fn main() {\n    let x = 1;\n}\n")?;
    helix_term::review::append_entry(
        workspace.path(),
        "lib.rs",
        0,
        0,
        None,
        "pre-existing comment",
    )?;

    let app = AppBuilder::new().with_file(file_path, None).build()?;

    let doc = doc!(app.editor);
    let diagnostics = doc.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "pre-existing comment");

    Ok(())
}
