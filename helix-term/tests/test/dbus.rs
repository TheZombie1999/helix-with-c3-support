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
