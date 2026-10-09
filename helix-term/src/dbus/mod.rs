//! A small D-Bus interface that lets other processes ask a running Helix
//! instance to open a file, e.g. from a terminal, a file manager "Open
//! with" action, or a `git commit` editor hook. Callers choose whether
//! opening the file should also switch the current view to show it, and
//! whether to block until the file's buffer is closed again (so Helix can
//! act as a blocking `$EDITOR`). `CloseFile` and `ReloadFile` round out the
//! other direction (closing a buffer, and picking up out-of-band changes to
//! an already-open file), and `ListOpenFiles` lets a remote picker or
//! status tool see what's currently open.
//!
//! Each instance claims a PID-suffixed bus name (`org.helix_editor.Helix.pid<PID>`)
//! so that multiple running instances never collide, and additionally makes a
//! best-effort attempt to claim the bare `org.helix_editor.Helix` name so that
//! simple clients which don't care which instance they talk to can reach
//! whichever instance claimed it first (typically the first one started).
//!
//! Connecting to the session bus is not available everywhere (e.g. minimal
//! containers, or non-Linux systems without a D-Bus daemon), so failures here
//! are reported as a one-off status message rather than treated as fatal.
//!
//! `helix-dbus` holds the wire contract: `helix1.xml` (the source of truth
//! for the interface) and a client proxy generated from it via `cargo xtask
//! dbus-codegen` (used by this crate's own tests, and by the `helix-remote`
//! CLI). The server implementation below is hand-written and must be kept
//! in sync with the XML file manually.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use helix_core::{pos_at_coords, Position, Selection};
use helix_dbus::{OBJECT_PATH, SHARED_WELL_KNOWN_NAME};
use helix_event::register_hook;
use helix_loader::workspace_trust::TrustQuery;
use helix_view::{
    doc_mut, editor::Action, editor::CloseError, events::DocumentDidClose, view_mut, DocumentId,
    Editor,
};
use parking_lot::Mutex;
use tokio::sync::oneshot;
use zbus::{connection, fdo::RequestNameFlags, interface};

use crate::job;

static INSTANCE_SEQ: AtomicU32 = AtomicU32::new(0);

/// Claims a bus name for a new Helix instance in this process; call exactly
/// once per `Application` and hand the result to [`run`].
///
/// The first call in a process (the common, real-world case: one Helix
/// instance per process) gets the plain PID-based name
/// ([`helix_dbus::pid_bus_name`]), which is what other processes should
/// expect to find a given `hx` at. Later calls — only possible when several
/// `Application`s run in a single process, as in this crate's own test
/// suite — get a disambiguating suffix so they can't collide with each
/// other or be routed to the wrong instance.
pub fn claim_instance_name() -> String {
    let seq = INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed);
    let base = helix_dbus::pid_bus_name(std::process::id());
    if seq == 0 {
        base
    } else {
        format!("{base}-{seq}")
    }
}

/// Identifies one `Editor` across `Application`s that share a process (only
/// possible in this crate's own test suite — a real `hx` has exactly one),
/// so that [`CLOSE_WAITERS`] can't mix up same-numbered [`DocumentId`]s from
/// different editors. Never dereferenced; only ever compared.
type EditorKey = usize;

fn editor_key(editor: &Editor) -> EditorKey {
    editor as *const Editor as EditorKey
}

type CloseWaiters = HashMap<(EditorKey, DocumentId), Vec<oneshot::Sender<()>>>;

/// Callers waiting on `OpenFile`'s `wait: true`, keyed by the document
/// they're waiting on. Populated by `open_file` while handling a request,
/// drained by the `DocumentDidClose` hook registered in [`run`].
static CLOSE_WAITERS: Mutex<Option<CloseWaiters>> = Mutex::new(None);

fn register_close_waiter(editor: &Editor, doc: DocumentId, tx: oneshot::Sender<()>) {
    CLOSE_WAITERS
        .lock()
        .get_or_insert_with(HashMap::new)
        .entry((editor_key(editor), doc))
        .or_default()
        .push(tx);
}

/// Registers the hook that wakes up [`register_close_waiter`]'s callers.
///
/// Must be called exactly once per `Application`, i.e. every time [`run`]
/// runs — *not* guarded by a process-wide `Once`. Helix's event registry is
/// scoped per-`tokio::runtime` in test builds specifically so that multiple
/// `Application`s running in parallel (as in this crate's own test suite)
/// each get their own independent hooks; a `Once` would make every
/// `Application` after the first in a process silently run with no close
/// hook at all, hanging any `wait: true` request it ever got. In a real
/// `hx`, where there's one `Application` per process, this still only runs
/// once, same as every other hook registration in `handlers::register_hooks`.
fn register_close_hook() {
    register_hook!(move |event: &mut DocumentDidClose<'_>| {
        let key = (editor_key(event.editor), event.doc.id());
        if let Some(waiters) = CLOSE_WAITERS
            .lock()
            .get_or_insert_with(HashMap::new)
            .remove(&key)
        {
            for tx in waiters {
                let _ = tx.send(());
            }
        }
        Ok(())
    });
}

struct Interface;

// NOTE: must match `helix_dbus::INTERFACE_NAME` (a macro attribute needs a
// literal, so it can't just reference the constant).
#[interface(name = "org.helix_editor.Helix1")]
impl Interface {
    /// Open `path` in this Helix instance.
    ///
    /// `line` and `column` are 1-indexed; pass `0` for either to leave the
    /// cursor untouched on that axis. If `focus` is true, the current view
    /// switches to show the file immediately; otherwise it is only loaded in
    /// the background (e.g. for a `git commit` editor hook that shouldn't
    /// steal focus from whatever the user is already looking at). If `wait`
    /// is true, the reply is held back until the file's buffer is closed
    /// (or this instance exits), so Helix can be used as a blocking
    /// `$EDITOR`. Returns an empty string on success, or a human readable
    /// error message on failure.
    async fn open_file(
        &self,
        path: String,
        line: u32,
        column: u32,
        focus: bool,
        wait: bool,
    ) -> String {
        let (tx, rx) = oneshot::channel();
        let (close_tx, close_rx) = oneshot::channel();
        let path = PathBuf::from(path);

        job::dispatch(move |editor, _compositor| {
            let result = open_file(editor, &path, line, column, focus);
            if wait {
                if let Ok(doc_id) = &result {
                    register_close_waiter(editor, *doc_id, close_tx);
                }
            }
            let _ = tx.send(result.map(|_| ()));
        })
        .await;

        match rx.await {
            Ok(Ok(())) => {
                if wait {
                    // No timeout: this waits for as long as the buffer stays
                    // open. The sender side is dropped (closing this
                    // `recv`) if Helix exits first, so this cannot hang past
                    // the process's own lifetime.
                    let _ = close_rx.await;
                }
                String::new()
            }
            Ok(Err(err)) => err,
            Err(_) => "Helix shut down before the file could be opened".to_string(),
        }
    }

    /// Lists the absolute paths of this instance's currently open buffers
    /// that are backed by a file on disk (unsaved scratch buffers with no
    /// path are omitted).
    async fn list_open_files(&self) -> Vec<String> {
        let (tx, rx) = oneshot::channel();

        job::dispatch(move |editor, _compositor| {
            let paths = editor
                .documents()
                .filter_map(|doc| doc.path())
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            let _ = tx.send(paths);
        })
        .await;

        rx.await.unwrap_or_default()
    }

    /// Closes `path`'s buffer. Fails if it has unsaved changes unless
    /// `force` is true, in which case they are discarded. Returns an empty
    /// string on success, or a human readable error message on failure.
    async fn close_file(&self, path: String, force: bool) -> String {
        let (tx, rx) = oneshot::channel();
        let path = PathBuf::from(path);

        job::dispatch(move |editor, _compositor| {
            let _ = tx.send(close_file(editor, &path, force));
        })
        .await;

        match rx.await {
            Ok(Ok(())) => String::new(),
            Ok(Err(err)) => err,
            Err(_) => "Helix shut down before the file could be closed".to_string(),
        }
    }

    /// Reloads `path`'s buffer from disk: its content is replaced with a
    /// diff against the file's current on-disk content and applied as a
    /// single edit, so unsaved changes are overwritten but still
    /// recoverable with undo. Returns an empty string on success, or a
    /// human readable error message on failure (e.g. the file isn't open,
    /// or no longer exists on disk).
    async fn reload_file(&self, path: String) -> String {
        let (tx, rx) = oneshot::channel();
        let path = PathBuf::from(path);

        job::dispatch(move |editor, _compositor| {
            let _ = tx.send(reload_file(editor, &path));
        })
        .await;

        match rx.await {
            Ok(Ok(())) => String::new(),
            Ok(Err(err)) => err,
            Err(_) => "Helix shut down before the file could be reloaded".to_string(),
        }
    }
}

fn open_file(
    editor: &mut Editor,
    path: &Path,
    line: u32,
    column: u32,
    focus: bool,
) -> Result<DocumentId, String> {
    let action = if focus { Action::Replace } else { Action::Load };
    let doc_id = editor.open(path, action).map_err(|err| err.to_string())?;

    if line > 0 || column > 0 {
        let view_id = editor.tree.focus;
        let doc = doc_mut!(editor, &doc_id);
        let pos = Position::new(
            line.saturating_sub(1) as usize,
            column.saturating_sub(1) as usize,
        );
        let idx = pos_at_coords(doc.text().slice(..), pos, true);
        doc.set_selection(view_id, Selection::point(idx));
    }

    Ok(doc_id)
}

fn close_file(editor: &mut Editor, path: &Path, force: bool) -> Result<(), String> {
    let path = helix_stdx::path::canonicalize(path);
    let doc_id = editor
        .document_id_by_path(&path)
        .ok_or_else(|| "file is not open".to_string())?;

    editor
        .close_document(doc_id, force)
        .map_err(|err| match err {
            CloseError::DoesNotExist => "file is not open".to_string(),
            CloseError::BufferModified(name) => {
                format!("{name} has unsaved changes (pass force to discard them)")
            }
            CloseError::SaveError(err) => format!("failed to save before closing: {err}"),
        })
}

fn reload_file(editor: &mut Editor, path: &Path) -> Result<(), String> {
    let path = helix_stdx::path::canonicalize(path);
    let doc_id = editor
        .document_id_by_path(&path)
        .ok_or_else(|| "file is not open".to_string())?;

    let fallback_view_id = editor.tree.focus;
    let doc = doc_mut!(editor, &doc_id);
    let mut view_ids: Vec<_> = doc.selections().keys().copied().collect();
    if view_ids.is_empty() {
        doc.ensure_view_init(fallback_view_id);
        view_ids.push(fallback_view_id);
    }
    let view_id = view_ids[0];

    let trust_full = editor
        .workspace_trust
        .query(doc.workspace_root(), TrustQuery::Git)
        .is_trusted();

    let view = view_mut!(editor, view_id);
    view.sync_changes(doc);
    doc.reload(view, &editor.diff_providers, trust_full)
        .map_err(|err| err.to_string())?;

    if let Some(path) = doc.path().map(ToOwned::to_owned) {
        editor
            .language_servers
            .file_event_handler
            .file_changed(path);
    }

    Ok(())
}

/// Connect to the session bus and serve the Helix D-Bus interface at `name`
/// (from [`claim_instance_name`]) until the process exits. Intended to be
/// run as a detached background job; errors are reported through the status
/// line and are never fatal to the editor.
pub async fn run(name: String) -> anyhow::Result<()> {
    register_close_hook();

    let conn = connection::Builder::session()?
        .name(name)?
        .serve_at(OBJECT_PATH, Interface)?
        .build()
        .await?;

    // Best-effort: whichever instance gets here first "owns" the bare name.
    // Losing this race is normal and not an error.
    let _ = conn
        .request_name_with_flags(SHARED_WELL_KNOWN_NAME, RequestNameFlags::DoNotQueue.into())
        .await;

    // `conn`'s background dispatch tasks keep the connection alive as long
    // as this future is still being polled, i.e. for the life of the process.
    std::future::pending::<()>().await;
    unreachable!("Helix process exited through the D-Bus task instead of normally")
}
