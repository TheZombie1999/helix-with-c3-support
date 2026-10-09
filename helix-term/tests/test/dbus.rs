use std::time::Duration;

use anyhow::Context;
use helix_dbus::{proxy::Helix1Proxy, OBJECT_PATH};
use helix_term::application::Application;
use tokio::{task::JoinHandle, time::Instant};
use zbus::Connection;

use super::helpers::*;

/// Drives `app`'s event loop until `task` completes. This is needed because
/// the D-Bus server answers a request by round-tripping a callback through
/// `Application`'s job queue, which only gets drained while the event loop is
/// running.
async fn pump_while<T>(app: &mut Application, task: JoinHandle<T>) -> T {
    tokio::pin!(task);
    loop {
        tokio::select! {
            res = &mut task => return res.expect("dbus client task panicked"),
            _ = run_event_loop_until_idle(app) => {},
        }
    }
}

/// Connects to `app`'s D-Bus server and returns a proxy once it is actually
/// answering calls, retrying for a while since the server claims its bus
/// name from a task spawned inside `Application::new`, which races with
/// this connecting. `ListOpenFiles` is used as a side-effect-free probe to
/// confirm the service is up (merely building the proxy succeeds even
/// before anyone owns the bus name).
async fn connect(
    app: &mut Application,
    client: &Connection,
) -> anyhow::Result<Helix1Proxy<'static>> {
    let bus_name = app.dbus_bus_name.clone();
    let client = client.clone();

    let task: JoinHandle<anyhow::Result<Helix1Proxy<'static>>> = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let attempt = async {
                let proxy = Helix1Proxy::builder(&client)
                    .destination(bus_name.clone())?
                    .path(OBJECT_PATH)?
                    .build()
                    .await?;
                proxy.list_open_files().await?;
                Ok::<_, zbus::Error>(proxy)
            };

            match attempt.await {
                Ok(proxy) => return Ok(proxy),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(err) => {
                    return Err(err).context("Helix D-Bus service never came up");
                }
            }
        }
    });

    pump_while(app, task).await
}

/// Skips the calling test (rather than failing it) when no D-Bus session bus
/// is reachable at all, since that reflects the test environment rather than
/// a bug in Helix.
macro_rules! require_session_bus {
    () => {
        if Connection::session().await.is_err() {
            eprintln!("skipping: no D-Bus session bus available");
            return Ok(());
        }
    };
}

/// A request with `focus: true` switches the current view to show the
/// opened file, in addition to loading it.
#[tokio::test(flavor = "multi_thread")]
async fn open_file_over_dbus_with_focus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\nsecond line\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let task = tokio::spawn(async move { proxy.open_file(&path, 2, 3, true, false).await });
    let result = pump_while(&mut app, task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file opened over D-Bus should be loaded in the editor");

    let view_id = app.editor.tree.focus;
    assert_eq!(
        app.editor.tree.get(view_id).doc,
        doc_id,
        "focus: true should switch the current view to the opened file"
    );

    let doc = helix_view::doc!(app.editor, &doc_id);
    let selection = doc.selection(view_id);
    let cursor = selection.primary().cursor(doc.text().slice(..));
    let pos = helix_core::coords_at_pos(doc.text().slice(..), cursor);
    assert_eq!(pos, helix_core::Position::new(1, 2));

    Ok(())
}

/// A request with `focus: false` loads the file into the background without
/// switching the currently displayed view away from whatever it was showing.
#[tokio::test(flavor = "multi_thread")]
async fn open_file_over_dbus_without_focus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let view_id = app.editor.tree.focus;
    let doc_id_before = app.editor.tree.get(view_id).doc;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let task = tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await });
    let result = pump_while(&mut app, task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    app.editor
        .document_id_by_path(file.path())
        .expect("file opened over D-Bus should still be loaded in the editor");

    assert_eq!(
        app.editor.tree.get(view_id).doc,
        doc_id_before,
        "focus: false should not change which document the current view displays"
    );

    Ok(())
}

/// `ListOpenFiles` reports the paths of currently open, on-disk buffers.
#[tokio::test(flavor = "multi_thread")]
async fn list_open_files_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = tokio::spawn({
        let proxy = proxy.clone();
        let path = path.clone();
        async move { proxy.open_file(&path, 0, 0, false, false).await }
    });
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let canonical_path = helix_stdx::path::canonicalize(file.path());

    let list_task = tokio::spawn(async move { proxy.list_open_files().await });
    let open_files = pump_while(&mut app, list_task).await?;

    assert!(
        open_files
            .iter()
            .any(|p| std::path::Path::new(p) == canonical_path),
        "expected {canonical_path:?} in ListOpenFiles output: {open_files:?}"
    );

    Ok(())
}

/// `OpenFile` with `wait: true` doesn't reply until the opened buffer is
/// closed, letting Helix act as a blocking `$EDITOR`.
#[tokio::test(flavor = "multi_thread")]
async fn open_file_over_dbus_wait_blocks_until_closed() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let mut wait_task = tokio::spawn(async move { proxy.open_file(&path, 0, 0, true, true).await });

    // Pump until the file shows up as open, without letting the still-
    // blocked `wait: true` call complete the test on its own.
    let doc_id = loop {
        tokio::select! {
            res = &mut wait_task => {
                panic!("OpenFile(wait: true) returned before the buffer was closed: {res:?}");
            }
            _ = run_event_loop_until_idle(&mut app) => {
                if let Some(id) = app.editor.document_id_by_path(file.path()) {
                    break id;
                }
            }
        }
    };

    assert!(
        app.editor.close_document(doc_id, true).is_ok(),
        "closing the just-opened buffer should succeed"
    );

    let result = pump_while(&mut app, wait_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    Ok(())
}

/// `CloseFile` closes an unmodified buffer outright.
#[tokio::test(flavor = "multi_thread")]
async fn close_file_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");
    assert!(app.editor.document_id_by_path(file.path()).is_some());

    let close_task = tokio::spawn(async move { proxy.close_file(&path, false).await });
    let result = pump_while(&mut app, close_task).await?;
    assert!(result.is_empty(), "CloseFile returned an error: {result}");

    assert!(
        app.editor.document_id_by_path(file.path()).is_none(),
        "file should no longer be open after CloseFile"
    );

    Ok(())
}

/// `CloseFile` refuses to close a buffer with unsaved changes unless
/// `force` is set, matching `:bc`/`:bc!`.
#[tokio::test(flavor = "multi_thread")]
async fn close_file_over_dbus_requires_force_when_modified() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file should be open");
    let view_id = app.editor.tree.focus;
    let doc = helix_view::doc_mut!(app.editor, &doc_id);
    let transaction =
        helix_core::Transaction::change(doc.text(), std::iter::once((0, 0, Some("x".into()))));
    doc.apply(&transaction, view_id);
    assert!(doc.is_modified());

    let close_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.close_file(&path, false).await })
    };
    let result = pump_while(&mut app, close_task).await?;
    assert!(
        !result.is_empty(),
        "CloseFile without force should fail on a modified buffer"
    );
    assert!(app.editor.document_id_by_path(file.path()).is_some());

    let close_task = tokio::spawn(async move { proxy.close_file(&path, true).await });
    let result = pump_while(&mut app, close_task).await?;
    assert!(
        result.is_empty(),
        "CloseFile with force should succeed: {result}"
    );
    assert!(app.editor.document_id_by_path(file.path()).is_none());

    Ok(())
}

/// `ReloadFile` replaces a buffer's content with whatever is now on disk.
#[tokio::test(flavor = "multi_thread")]
async fn reload_file_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "original contents\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    // Change the file on disk, out from under the open buffer.
    std::fs::write(file.path(), "updated contents\n")?;

    let reload_task = tokio::spawn(async move { proxy.reload_file(&path).await });
    let result = pump_while(&mut app, reload_task).await?;
    assert!(result.is_empty(), "ReloadFile returned an error: {result}");

    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file should still be open");
    let doc = helix_view::doc!(app.editor, &doc_id);
    assert_eq!(doc.text().to_string(), "updated contents\n");
    assert!(
        !doc.is_modified(),
        "reloading should not leave the buffer marked as modified"
    );

    Ok(())
}

/// `GetCursorPosition` reports where `OpenFile` just placed the cursor.
#[tokio::test(flavor = "multi_thread")]
async fn get_cursor_position_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\nsecond line\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 2, 3, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let get_task = tokio::spawn(async move { proxy.get_cursor_position(&path).await });
    let (line, column, error) = pump_while(&mut app, get_task).await?;
    assert!(error.is_empty(), "GetCursorPosition returned: {error}");
    assert_eq!((line, column), (2, 3));

    Ok(())
}

/// `GetSelection` reports the primary selection's range and text.
#[tokio::test(flavor = "multi_thread")]
async fn get_selection_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    // Select "hello" (chars 0..5) in the view that `open_file` set up.
    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file should be open");
    let view_id = app.editor.tree.focus;
    let doc = helix_view::doc_mut!(app.editor, &doc_id);
    doc.set_selection(view_id, helix_core::Selection::single(0, 5));

    let get_task = tokio::spawn(async move { proxy.get_selection(&path).await });
    let (start_line, start_column, end_line, end_column, text, error) =
        pump_while(&mut app, get_task).await?;
    assert!(error.is_empty(), "GetSelection returned an error: {error}");
    assert_eq!((start_line, start_column), (1, 1));
    assert_eq!((end_line, end_column), (1, 6));
    assert_eq!(text, "hello");

    Ok(())
}

/// `GetDiagnostics` reports a document's currently cached diagnostics.
#[tokio::test(flavor = "multi_thread")]
async fn get_diagnostics_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file should be open");
    let doc = helix_view::doc_mut!(app.editor, &doc_id);
    let diagnostic = helix_core::Diagnostic {
        range: helix_stdx::range::Range { start: 6, end: 10 },
        ends_at_word: false,
        starts_at_word: false,
        zero_width: false,
        line: 0,
        message: "something's off".to_string(),
        severity: Some(helix_core::diagnostic::Severity::Warning),
        code: None,
        provider: helix_core::diagnostic::DiagnosticProvider::Lsp {
            server_id: helix_lsp::LanguageServerId::default(),
            identifier: None,
        },
        tags: Vec::new(),
        source: None,
        data: None,
    };
    doc.replace_diagnostics(vec![diagnostic], &[], None);

    let get_task = tokio::spawn(async move { proxy.get_diagnostics(&path).await });
    let (diagnostics, error) = pump_while(&mut app, get_task).await?;
    assert!(
        error.is_empty(),
        "GetDiagnostics returned an error: {error}"
    );
    assert_eq!(
        diagnostics,
        vec![(
            1,
            7,
            1,
            11,
            "warning".to_string(),
            "something's off".to_string()
        )]
    );

    Ok(())
}

/// `GetWorkspaceRoot` resolves the nearest ancestor containing a `.git`,
/// independent of whether the path is open in the editor at all.
#[tokio::test(flavor = "multi_thread")]
async fn get_workspace_root_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let workspace = tempfile::tempdir()?;
    std::fs::create_dir(workspace.path().join(".git"))?;
    let nested = workspace.path().join("src/nested");
    std::fs::create_dir_all(&nested)?;
    let file_path = nested.join("file.rs");
    std::fs::write(&file_path, "fn main() {}\n")?;
    let path = file_path.to_str().unwrap().to_owned();

    let get_task = tokio::spawn(async move { proxy.get_workspace_root(&path).await });
    let (root, error) = pump_while(&mut app, get_task).await?;
    assert!(
        error.is_empty(),
        "GetWorkspaceRoot returned an error: {error}"
    );
    assert_eq!(
        helix_stdx::path::canonicalize(root),
        helix_stdx::path::canonicalize(workspace.path())
    );

    Ok(())
}

/// `GetBufferContent` and `IsModified` both see an unsaved edit that the
/// on-disk file doesn't have — the whole point of reading the buffer
/// instead of the file.
#[tokio::test(flavor = "multi_thread")]
async fn get_buffer_content_and_is_modified_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "hello from dbus\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    // Unmodified right after opening.
    let modified_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.is_modified(&path).await })
    };
    let (modified, error) = pump_while(&mut app, modified_task).await?;
    assert!(error.is_empty(), "IsModified returned an error: {error}");
    assert!(!modified, "freshly opened buffer should not be modified");

    // Edit the buffer without saving: the on-disk file is now stale.
    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file should be open");
    let view_id = app.editor.tree.focus;
    let doc = helix_view::doc_mut!(app.editor, &doc_id);
    let transaction = helix_core::Transaction::change(
        doc.text(),
        std::iter::once((0, 0, Some("EDITED: ".into()))),
    );
    doc.apply(&transaction, view_id);

    let content_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.get_buffer_content(&path).await })
    };
    let (content, error) = pump_while(&mut app, content_task).await?;
    assert!(
        error.is_empty(),
        "GetBufferContent returned an error: {error}"
    );
    assert_eq!(content, "EDITED: hello from dbus\n");
    assert_eq!(
        std::fs::read_to_string(file.path())?,
        "hello from dbus\n",
        "the on-disk file should be untouched by the in-memory edit"
    );

    let modified_task = tokio::spawn(async move { proxy.is_modified(&path).await });
    let (modified, error) = pump_while(&mut app, modified_task).await?;
    assert!(error.is_empty(), "IsModified returned an error: {error}");
    assert!(modified, "buffer with an unsaved edit should be modified");

    Ok(())
}

/// `SetBufferContent` replaces an open file's buffer without touching disk,
/// and leaves the buffer modified.
#[tokio::test(flavor = "multi_thread")]
async fn set_buffer_content_over_dbus() -> anyhow::Result<()> {
    require_session_bus!();

    let mut app = AppBuilder::new().build()?;
    let client = Connection::session().await?;
    let proxy = connect(&mut app, &client).await?;

    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), "original contents\n")?;
    let path = file.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.open_file(&path, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let set_task = {
        let proxy = proxy.clone();
        let path = path.clone();
        tokio::spawn(async move { proxy.set_buffer_content(&path, "replaced contents\n").await })
    };
    let error = pump_while(&mut app, set_task).await?;
    assert!(
        error.is_empty(),
        "SetBufferContent returned an error: {error}"
    );

    let doc_id = app
        .editor
        .document_id_by_path(file.path())
        .expect("file should still be open");
    let doc = helix_view::doc!(app.editor, &doc_id);
    assert_eq!(doc.text().to_string(), "replaced contents\n");
    assert!(
        doc.is_modified(),
        "SetBufferContent should leave the buffer modified"
    );
    assert_eq!(
        std::fs::read_to_string(file.path())?,
        "original contents\n",
        "SetBufferContent must not touch the file on disk"
    );

    // Also works for a file that was never displayed in any view.
    let file2 = tempfile::NamedTempFile::new()?;
    std::fs::write(file2.path(), "second file\n")?;
    let path2 = file2.path().to_str().unwrap().to_owned();

    let open_task = {
        let proxy = proxy.clone();
        let path2 = path2.clone();
        tokio::spawn(async move { proxy.open_file(&path2, 0, 0, false, false).await })
    };
    let result = pump_while(&mut app, open_task).await?;
    assert!(result.is_empty(), "OpenFile returned an error: {result}");

    let set_task = tokio::spawn(async move { proxy.set_buffer_content(&path2, "new\n").await });
    let error = pump_while(&mut app, set_task).await?;
    assert!(
        error.is_empty(),
        "SetBufferContent returned an error: {error}"
    );

    let doc_id2 = app
        .editor
        .document_id_by_path(file2.path())
        .expect("second file should still be open");
    let doc2 = helix_view::doc!(app.editor, &doc_id2);
    assert_eq!(doc2.text().to_string(), "new\n");

    Ok(())
}
