use std::{
    collections::HashMap,
    env,
    process::ExitCode,
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context};
use helix_dbus::{
    pid_bus_name, proxy::Helix1Proxy, BUS_NAME_PID_PREFIX, OBJECT_PATH, SHARED_WELL_KNOWN_NAME,
};
use zbus::Connection;

pub fn main() -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start a tokio runtime");

    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        return Ok(());
    };

    match command.as_str() {
        "open" => open_file(false, args).await,
        "edit" => open_file(true, args).await,
        "changed" => changed_files(false, args).await,
        "close" => close_files(args).await,
        "reload" => reload_files(args).await,
        "watch" => watch(args).await,
        "cursor" => get_cursor_position(args).await,
        "selection" => get_selection(args).await,
        "diagnostics" => get_diagnostics(args).await,
        "workspace-root" => get_workspace_root(args).await,
        "content" => get_buffer_content(args).await,
        "modified" => is_modified(args).await,
        "set-content" => set_buffer_content(args).await,
        "list-files" | "ls" => list_files(args).await,
        "list-instances" | "ps" => list_instances().await,
        "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command {other:?}; see `hx-remote help`"),
    }
}

fn print_usage() {
    println!(
        "\
hx-remote: talk to a running Helix instance over D-Bus

USAGE:
    hx-remote open <path>[:LINE[:COL]]... [--no-focus] [--wait] [--instance NAME | --pid PID | --all]
    hx-remote edit <path>[:LINE[:COL]]... [--instance NAME | --pid PID | --all]
    hx-remote changed [--no-focus] [--wait] [--instance NAME | --pid PID | --all]
    hx-remote close <path>... [--force] [--instance NAME | --pid PID | --all]
    hx-remote reload [<path>...] [--instance NAME | --pid PID | --all]
    hx-remote watch [--interval SECS] [--instance NAME | --pid PID | --all]
    hx-remote cursor <path> [--instance NAME | --pid PID]
    hx-remote selection <path> [--instance NAME | --pid PID]
    hx-remote diagnostics <path> [--instance NAME | --pid PID]
    hx-remote workspace-root [<path>] [--instance NAME | --pid PID]
    hx-remote content <path> [--instance NAME | --pid PID]
    hx-remote modified <path> [--instance NAME | --pid PID]
    hx-remote set-content <path> [--instance NAME | --pid PID]   (reads new content from stdin)
    hx-remote list-files [--instance NAME | --pid PID]
    hx-remote list-instances

COMMANDS:
    open               Open one or more files. Unless --no-focus is given,
                       the last one switches the current view to show it.
                       With --wait, doesn't return until every file's buffer
                       is closed.
    edit               Like `open --wait`; usable as $EDITOR, e.g.:
                           EDITOR=\"hx-remote edit\" git commit
    changed            Open every file `git status` reports as changed in
                       the current directory's repository (same flags as
                       `open`; deleted files are skipped).
    close              Close one or more open files' buffers. Fails on a
                       file with unsaved changes unless --force is given.
    reload             Reload one or more open files' buffers from disk,
                       discarding in-memory edits (recoverable with undo).
                       With no <path>, reloads every currently open file
                       (e.g. `hx-remote reload --all` reloads everything
                       open in every running instance).
    watch              Runs in the foreground, polling every --interval
                       seconds (default 1) for on-disk changes to whatever
                       is currently open, reloading changed files as they're
                       found. Files opened or closed after `watch` starts
                       are picked up automatically; stop with Ctrl-C.
    cursor             Print an open file's cursor position, as LINE:COLUMN.
    selection          Print an open file's selection range and text.
    diagnostics        Print an open file's current diagnostics (e.g. LSP
                       errors/warnings), one per line.
    workspace-root     Print the workspace root for a path (the nearest
                       ancestor with a .git/.svn/.jj/.helix), or this
                       instance's own workspace if no path is given. The
                       path doesn't need to be open, or even exist.
    content            Print an open file's current buffer content,
                       including unsaved changes (unlike reading the file
                       itself from disk).
    modified           Print \"true\" or \"false\": whether an open file's
                       buffer has unsaved changes.
    set-content        Replace an open file's buffer with stdin's content,
                       applied as a diff against the current content (so
                       it's undo-recoverable). Never touches disk; leaves
                       the buffer modified. E.g.:
                           git show HEAD:path/to/file | hx-remote set-content path/to/file
    list-files, ls     List the open, on-disk buffers of one running instance.
    list-instances, ps List the bus names of all running Helix instances.

Without --instance/--pid/--all, commands target whichever instance claimed
the shared \"org.helix_editor.Helix\" bus name (normally the first one
started). --all applies to every running instance instead of just one; for
`open`/`edit` with --wait, it returns once the files are closed in all of
them."
    );
}

/// Which running instance(s) a command applies to.
enum Target {
    Named(String),
    All,
}

#[derive(Clone)]
struct FileSpec {
    path: String,
    line: u32,
    column: u32,
}

struct OpenArgs {
    files: Vec<FileSpec>,
    focus: bool,
    wait: bool,
    target: Target,
}

fn parse_open_args(
    wait_by_default: bool,
    args: impl Iterator<Item = String>,
) -> anyhow::Result<OpenArgs> {
    let mut paths = Vec::new();
    let mut focus = true;
    let mut wait = wait_by_default;
    let mut instance = None;
    let mut all = false;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-focus" => focus = false,
            "--wait" => wait = true,
            "--no-wait" => wait = false,
            "--all" => all = true,
            "--instance" => instance = Some(args.next().context("--instance requires a value")?),
            "--pid" => {
                let pid: u32 = args
                    .next()
                    .context("--pid requires a value")?
                    .parse()
                    .context("--pid must be a number")?;
                instance = Some(pid_bus_name(pid));
            }
            other if other.starts_with("--") => bail!("unexpected argument {other:?}"),
            path => paths.push(path.to_string()),
        }
    }

    let target = match (all, instance) {
        (true, Some(_)) => bail!("--all cannot be combined with --instance/--pid"),
        (true, None) => Target::All,
        (false, instance) => {
            Target::Named(instance.unwrap_or_else(|| SHARED_WELL_KNOWN_NAME.to_string()))
        }
    };

    if paths.is_empty() {
        bail!("missing <path> argument");
    }
    let files = paths
        .into_iter()
        .map(|p| {
            let (path, line, column) = parse_path_spec(&p);
            let path = absolute_path(&path)?;
            Ok(FileSpec { path, line, column })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(OpenArgs {
        files,
        focus,
        wait,
        target,
    })
}

/// Splits `path`, `path:LINE`, or `path:LINE:COL` (1-indexed), matching the
/// same convention `hx` itself accepts on its own command line.
fn parse_path_spec(s: &str) -> (String, u32, u32) {
    if let Some((path, line, column)) = split_path_line_col(s) {
        return (path, line, column);
    }
    if let Some((path, line)) = split_path_line(s) {
        return (path, line, 0);
    }
    (s.to_string(), 0, 0)
}

fn split_path_line_col(s: &str) -> Option<(String, u32, u32)> {
    let (rest, column) = s.rsplit_once(':')?;
    let column: u32 = column.parse().ok()?;
    let (path, line) = rest.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    Some((path.to_string(), line, column))
}

fn split_path_line(s: &str) -> Option<(String, u32)> {
    let (path, line) = s.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    Some((path.to_string(), line))
}

/// Resolves `path` against our own current directory. The path is sent to a
/// Helix instance that may have a different working directory, so a
/// relative path must be made absolute locally rather than left for the
/// server to resolve.
fn absolute_path(path: &str) -> anyhow::Result<String> {
    let path = std::path::Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .context("determining the current directory")?
            .join(path)
    };
    Ok(path.to_string_lossy().into_owned())
}

/// Resolves a [`Target`] to the bus name(s) a command should actually talk
/// to, erroring out for `--all` if nothing is currently running.
async fn resolve_instances(conn: &Connection, target: Target) -> anyhow::Result<Vec<String>> {
    match target {
        Target::Named(instance) => Ok(vec![instance]),
        Target::All => {
            let instances = list_instance_names(conn).await?;
            if instances.is_empty() {
                bail!("no running Helix instances found");
            }
            Ok(instances)
        }
    }
}

async fn connect(conn: &Connection, instance: &str) -> anyhow::Result<Helix1Proxy<'static>> {
    Helix1Proxy::builder(conn)
        .destination(instance.to_string())?
        .path(OBJECT_PATH)?
        .build()
        .await
        .with_context(|| format!("connecting to Helix instance {instance:?} (is it running?)"))
}

/// Opens `files` on one instance: sequentially, so that only the last one
/// ends up focused (deterministically, regardless of `--all`'s concurrency
/// across instances) instead of racing. If `wait`, every file is then
/// waited on concurrently, so one slow-to-close buffer doesn't hold up
/// noticing that the others have already closed.
async fn open_files_on_instance(
    conn: Connection,
    instance: String,
    files: Vec<FileSpec>,
    focus: bool,
    wait: bool,
) -> anyhow::Result<()> {
    let proxy = connect(&conn, &instance).await?;

    let mut errors = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let focus_this = focus && i == files.len() - 1;
        let result = proxy
            .open_file(&file.path, file.line, file.column, focus_this, false)
            .await
            .with_context(|| format!("calling OpenFile on {instance:?}"))
            .and_then(|error| {
                if error.is_empty() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(error))
                }
            });
        if let Err(err) = result {
            errors.push(format!("{}: {err:#}", file.path));
        }
    }

    if wait {
        let mut waits = tokio::task::JoinSet::new();
        for file in files {
            let proxy = proxy.clone();
            waits.spawn(async move {
                let result = proxy
                    .open_file(&file.path, 0, 0, false, true)
                    .await
                    .context("waiting for the buffer to close")
                    .and_then(|error| {
                        if error.is_empty() {
                            Ok(())
                        } else {
                            Err(anyhow::anyhow!(error))
                        }
                    });
                (file.path, result)
            });
        }
        while let Some(joined) = waits.join_next().await {
            let (path, result) = joined.expect("hx-remote wait task panicked");
            if let Err(err) = result {
                errors.push(format!("{path}: {err:#}"));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

async fn open_file(
    wait_by_default: bool,
    args: impl Iterator<Item = String>,
) -> anyhow::Result<()> {
    let OpenArgs {
        files,
        focus,
        wait,
        target,
    } = parse_open_args(wait_by_default, args)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let instances = resolve_instances(&conn, target).await?;

    open_files_everywhere(&conn, instances, files, focus, wait).await
}

/// Opens `files` on every instance in `instances` in parallel, so one slow
/// or `--wait`-blocked instance doesn't hold up the others. Shared by
/// `open`/`edit` (explicit file list) and `changed` (file list from `git
/// status`).
async fn open_files_everywhere(
    conn: &Connection,
    instances: Vec<String>,
    files: Vec<FileSpec>,
    focus: bool,
    wait: bool,
) -> anyhow::Result<()> {
    let report_per_instance = instances.len() > 1;

    let mut tasks = tokio::task::JoinSet::new();
    for instance in instances {
        let conn = conn.clone();
        let files = files.clone();
        tasks.spawn(async move {
            let result = open_files_on_instance(conn, instance.clone(), files, focus, wait).await;
            (instance, result)
        });
    }

    let mut results = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        results.push(joined.expect("hx-remote open task panicked"));
    }
    results.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut had_error = false;
    for (instance, result) in results {
        match result {
            Ok(()) if report_per_instance => println!("{instance}: ok"),
            Ok(()) => {}
            Err(err) => {
                had_error = true;
                eprintln!("{instance}: error: {err:#}");
            }
        }
    }

    if had_error {
        bail!("failed to open the file(s) in one or more instances");
    }
    Ok(())
}

/// Parses the flags `open`/`edit`/`changed` share, minus the explicit
/// `<path>` arguments (which only `open`/`edit` take; `changed` derives its
/// file list from `git status` instead).
fn parse_focus_wait_target(
    wait_by_default: bool,
    args: impl Iterator<Item = String>,
) -> anyhow::Result<(bool, bool, Target)> {
    let mut focus = true;
    let mut wait = wait_by_default;
    let mut instance = None;
    let mut all = false;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-focus" => focus = false,
            "--wait" => wait = true,
            "--no-wait" => wait = false,
            "--all" => all = true,
            "--instance" => instance = Some(args.next().context("--instance requires a value")?),
            "--pid" => {
                let pid: u32 = args
                    .next()
                    .context("--pid requires a value")?
                    .parse()
                    .context("--pid must be a number")?;
                instance = Some(pid_bus_name(pid));
            }
            other => bail!("unexpected argument {other:?}"),
        }
    }

    let target = match (all, instance) {
        (true, Some(_)) => bail!("--all cannot be combined with --instance/--pid"),
        (true, None) => Target::All,
        (false, instance) => {
            Target::Named(instance.unwrap_or_else(|| SHARED_WELL_KNOWN_NAME.to_string()))
        }
    };

    Ok((focus, wait, target))
}

/// Opens every file `git status` reports as changed in the current
/// directory's repository (same semantics as `open`, just with the file
/// list computed from git instead of given explicitly).
async fn changed_files(
    wait_by_default: bool,
    args: impl Iterator<Item = String>,
) -> anyhow::Result<()> {
    let (focus, wait, target) = parse_focus_wait_target(wait_by_default, args)?;

    let paths = git_changed_files()?;
    if paths.is_empty() {
        println!("no changed files according to git");
        return Ok(());
    }
    let files: Vec<FileSpec> = paths
        .into_iter()
        .map(|path| FileSpec {
            path,
            line: 0,
            column: 0,
        })
        .collect();

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let instances = resolve_instances(&conn, target).await?;

    open_files_everywhere(&conn, instances, files, focus, wait).await
}

/// Absolute paths `git status` reports as changed in the current directory
/// (relative to it, same as everywhere else `hx-remote` resolves paths),
/// excluding entries with nothing left on disk to open (deletions).
fn git_changed_files() -> anyhow::Result<Vec<String>> {
    let output = std::process::Command::new("git")
        // `--untracked-files=all` makes git list every individual file
        // inside a new, untracked directory instead of collapsing it to a
        // single `?? dir/` entry.
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .context("running `git status` (is `git` installed and on PATH?)")?;

    if !output.status.success() {
        bail!(
            "`git status` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_git_status_z(&output.stdout)
        .into_iter()
        .map(|path| absolute_path(&path))
        .collect()
}

/// Parses `git status --porcelain=v1 -z` output (NUL-separated, so paths
/// with spaces or other odd characters don't need escaping) into the paths
/// it reports, skipping deletions.
fn parse_git_status_z(output: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = output.split(|&b| b == 0);

    while let Some(record) = fields.next() {
        // "XY path", at minimum a 2-byte status, a space, and a 1-byte path.
        if record.len() < 4 {
            continue;
        }
        let status = [record[0], record[1]];
        let path = String::from_utf8_lossy(&record[3..]).into_owned();

        // Renamed/copied entries carry an extra NUL-terminated field for
        // the original path, which we don't need (we only want to open the
        // file where it lives now).
        if status[0] == b'R' || status[0] == b'C' {
            fields.next();
        }

        // Deleted (in the index, the working tree, or both): nothing left
        // on disk to open.
        if status[0] == b'D' || status[1] == b'D' {
            continue;
        }

        paths.push(path);
    }

    paths
}

/// Parses `<path>... [--all | --instance NAME | --pid PID] [flag]`, where
/// `flag` (if any) is recognized by `handle_flag`. Shared by `close` and
/// `reload`, which otherwise only differ in which D-Bus method they call.
///
/// `require_paths` controls whether at least one `<path>` is mandatory:
/// `close` has no sensible default target, but `reload` with no paths
/// means "every currently open file", so it passes `false`.
fn parse_paths_and_target(
    args: impl Iterator<Item = String>,
    mut handle_flag: impl FnMut(&str) -> bool,
    require_paths: bool,
) -> anyhow::Result<(Vec<String>, Target)> {
    let mut paths = Vec::new();
    let mut instance = None;
    let mut all = false;

    let mut args = args;
    while let Some(arg) = args.next() {
        if handle_flag(&arg) {
            continue;
        }
        match arg.as_str() {
            "--all" => all = true,
            "--instance" => instance = Some(args.next().context("--instance requires a value")?),
            "--pid" => {
                let pid: u32 = args
                    .next()
                    .context("--pid requires a value")?
                    .parse()
                    .context("--pid must be a number")?;
                instance = Some(pid_bus_name(pid));
            }
            other if other.starts_with("--") => bail!("unexpected argument {other:?}"),
            path => paths.push(absolute_path(path)?),
        }
    }

    let target = match (all, instance) {
        (true, Some(_)) => bail!("--all cannot be combined with --instance/--pid"),
        (true, None) => Target::All,
        (false, instance) => {
            Target::Named(instance.unwrap_or_else(|| SHARED_WELL_KNOWN_NAME.to_string()))
        }
    };

    if require_paths && paths.is_empty() {
        bail!("missing <path> argument");
    }
    Ok((paths, target))
}

async fn close_files(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let mut force = false;
    let (paths, target) = parse_paths_and_target(
        args,
        |arg| {
            if arg == "--force" {
                force = true;
                true
            } else {
                false
            }
        },
        true,
    )?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let instances = resolve_instances(&conn, target).await?;

    let mut tasks = tokio::task::JoinSet::new();
    for instance in &instances {
        for path in &paths {
            let conn = conn.clone();
            let instance = instance.clone();
            let path = path.clone();
            tasks.spawn(async move {
                let result: anyhow::Result<()> = async {
                    let proxy = connect(&conn, &instance).await?;
                    let error = proxy
                        .close_file(&path, force)
                        .await
                        .with_context(|| format!("calling CloseFile on {instance:?}"))?;
                    if !error.is_empty() {
                        bail!("{error}");
                    }
                    Ok(())
                }
                .await;
                (instance, path, result)
            });
        }
    }

    let mut results = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        results.push(joined.expect("hx-remote close task panicked"));
    }
    report_per_file_results(results, "close")
}

/// Reloads `explicit_paths` on one instance, or every file `ListOpenFiles`
/// reports for it if `explicit_paths` is empty (each instance may have a
/// different set of open files, so this has to be resolved per instance
/// rather than once up front).
async fn reload_files_on_instance(
    conn: Connection,
    instance: String,
    explicit_paths: Vec<String>,
) -> Vec<(String, String, anyhow::Result<()>)> {
    let proxy = match connect(&conn, &instance).await {
        Ok(proxy) => proxy,
        Err(err) => return vec![(instance, "<connect>".to_string(), Err(err))],
    };

    let paths = if explicit_paths.is_empty() {
        match proxy
            .list_open_files()
            .await
            .with_context(|| format!("calling ListOpenFiles on {instance:?}"))
        {
            Ok(paths) => paths,
            Err(err) => return vec![(instance, "<list-open-files>".to_string(), Err(err))],
        }
    } else {
        explicit_paths
    };

    let mut results = Vec::new();
    for path in paths {
        let result = proxy
            .reload_file(&path)
            .await
            .with_context(|| format!("calling ReloadFile on {instance:?}"))
            .and_then(|error| {
                if error.is_empty() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(error))
                }
            });
        results.push((instance.clone(), path, result));
    }
    results
}

async fn reload_files(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (paths, target) = parse_paths_and_target(args, |_| false, false)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let instances = resolve_instances(&conn, target).await?;

    let mut tasks = tokio::task::JoinSet::new();
    for instance in instances {
        let conn = conn.clone();
        let paths = paths.clone();
        tasks.spawn(reload_files_on_instance(conn, instance, paths));
    }

    let mut results = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        results.extend(joined.expect("hx-remote reload task panicked"));
    }

    if results.is_empty() {
        println!("nothing to reload");
        return Ok(());
    }
    report_per_file_results(results, "reload")
}

/// Prints an `ok`/error line per `(instance, path)` result when there's
/// more than one, and returns an error overall if any entry failed.
fn report_per_file_results(
    mut results: Vec<(String, String, anyhow::Result<()>)>,
    verb: &str,
) -> anyhow::Result<()> {
    results.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let report_detail = results.len() > 1;

    let mut had_error = false;
    for (instance, path, result) in results {
        match result {
            Ok(()) if report_detail => println!("{instance}: {path}: ok"),
            Ok(()) => {}
            Err(err) => {
                had_error = true;
                eprintln!("{instance}: {path}: error: {err:#}");
            }
        }
    }

    if had_error {
        bail!("failed to {verb} the file(s) in one or more instances");
    }
    Ok(())
}

fn parse_watch_args(args: impl Iterator<Item = String>) -> anyhow::Result<(Target, Duration)> {
    let mut instance = None;
    let mut all = false;
    let mut interval = Duration::from_secs(1);

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all" => all = true,
            "--instance" => instance = Some(args.next().context("--instance requires a value")?),
            "--pid" => {
                let pid: u32 = args
                    .next()
                    .context("--pid requires a value")?
                    .parse()
                    .context("--pid must be a number")?;
                instance = Some(pid_bus_name(pid));
            }
            "--interval" => {
                let secs: f64 = args
                    .next()
                    .context("--interval requires a value")?
                    .parse()
                    .context("--interval must be a number of seconds")?;
                interval = Duration::try_from_secs_f64(secs).context("--interval is invalid")?;
            }
            other => bail!("unexpected argument {other:?}"),
        }
    }

    let target = match (all, instance) {
        (true, Some(_)) => bail!("--all cannot be combined with --instance/--pid"),
        (true, None) => Target::All,
        (false, instance) => {
            Target::Named(instance.unwrap_or_else(|| SHARED_WELL_KNOWN_NAME.to_string()))
        }
    };

    Ok((target, interval))
}

/// The instance(s) `watch` should poll on this tick. Unlike [`resolve_instances`],
/// an empty `--all` isn't an error — `watch` just keeps polling for an
/// instance to show up, rather than giving up because none happened to be
/// running yet.
async fn watch_targets(conn: &Connection, target: &Target) -> anyhow::Result<Vec<String>> {
    match target {
        Target::Named(instance) => Ok(vec![instance.clone()]),
        Target::All => list_instance_names(conn).await,
    }
}

/// Polls every `interval` for on-disk changes to whatever is currently open
/// in the target instance(s), reloading changed files as they're found.
/// Re-fetches both the instance list (for `--all`) and each instance's open
/// files on every tick, so files (or instances) that come and go after
/// `watch` starts are picked up without needing a restart. Runs forever;
/// the user is expected to stop it with Ctrl-C.
async fn watch(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (target, interval) = parse_watch_args(args)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;

    // instance -> path -> on-disk mtime as of the last tick we saw it on.
    let mut known: HashMap<String, HashMap<String, SystemTime>> = HashMap::new();
    let mut proxies: HashMap<String, Helix1Proxy<'static>> = HashMap::new();

    println!(
        "Watching for file changes every {:.1}s. Press Ctrl-C to stop.",
        interval.as_secs_f64()
    );

    loop {
        let instances = match watch_targets(&conn, &target).await {
            Ok(instances) => instances,
            Err(err) => {
                eprintln!("error listing running instances: {err:#}");
                tokio::time::sleep(interval).await;
                continue;
            }
        };

        // Drop state for instances that have gone away.
        known.retain(|instance, _| instances.contains(instance));
        proxies.retain(|instance, _| instances.contains(instance));

        for instance in &instances {
            let proxy = match proxies.get(instance) {
                Some(proxy) => proxy.clone(),
                None => match connect(&conn, instance).await {
                    Ok(proxy) => {
                        proxies.insert(instance.clone(), proxy.clone());
                        proxy
                    }
                    Err(err) => {
                        eprintln!("{instance}: {err:#}");
                        continue;
                    }
                },
            };

            let open_files = match proxy.list_open_files().await {
                Ok(files) => files,
                Err(err) => {
                    eprintln!("{instance}: error listing open files: {err:#}");
                    proxies.remove(instance);
                    continue;
                }
            };

            let instance_known = known.entry(instance.clone()).or_default();
            instance_known.retain(|path, _| open_files.contains(path));

            for path in &open_files {
                let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
                    // Can't stat it (e.g. deleted); leave it be, ReloadFile
                    // will surface the problem if/when it actually changes.
                    continue;
                };

                match instance_known.get(path) {
                    // First time we've seen this file open: just record its
                    // current mtime. It's already in sync (Helix just read
                    // it), so there's nothing to reload yet.
                    None => {
                        instance_known.insert(path.clone(), modified);
                    }
                    Some(&last) if modified > last => {
                        instance_known.insert(path.clone(), modified);
                        match proxy.reload_file(path).await {
                            Ok(error) if error.is_empty() => {
                                println!("{instance}: reloaded {path}");
                            }
                            Ok(error) => eprintln!("{instance}: {path}: {error}"),
                            Err(err) => eprintln!("{instance}: {path}: {err:#}"),
                        }
                    }
                    Some(_) => {}
                }
            }
        }

        tokio::time::sleep(interval).await;
    }
}

fn parse_instance_flag(args: impl Iterator<Item = String>) -> anyhow::Result<String> {
    let mut instance = SHARED_WELL_KNOWN_NAME.to_string();
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instance" => instance = args.next().context("--instance requires a value")?,
            "--pid" => {
                let pid: u32 = args
                    .next()
                    .context("--pid requires a value")?
                    .parse()
                    .context("--pid must be a number")?;
                instance = pid_bus_name(pid);
            }
            other => bail!("unexpected argument {other:?}"),
        }
    }
    Ok(instance)
}

async fn list_files(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let instance = parse_instance_flag(args)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let paths = proxy
        .list_open_files()
        .await
        .with_context(|| format!("calling ListOpenFiles on {instance:?}"))?;
    for path in paths {
        println!("{path}");
    }
    Ok(())
}

/// Parses `[<path>] [--instance NAME | --pid PID]` for the single-instance
/// query commands (`cursor`, `selection`, `diagnostics`, `workspace-root`).
/// `path_required` is false only for `workspace-root`, which treats a
/// missing path as "this instance's own workspace".
fn parse_path_and_instance(
    args: impl Iterator<Item = String>,
    path_required: bool,
) -> anyhow::Result<(String, String)> {
    let mut path = None;
    let mut instance = SHARED_WELL_KNOWN_NAME.to_string();

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instance" => instance = args.next().context("--instance requires a value")?,
            "--pid" => {
                let pid: u32 = args
                    .next()
                    .context("--pid requires a value")?
                    .parse()
                    .context("--pid must be a number")?;
                instance = pid_bus_name(pid);
            }
            other if other.starts_with("--") => bail!("unexpected argument {other:?}"),
            p if path.is_none() => path = Some(absolute_path(p)?),
            other => bail!("unexpected argument {other:?}"),
        }
    }

    let path = match path {
        Some(path) => path,
        None if path_required => bail!("missing <path> argument"),
        None => String::new(),
    };
    Ok((path, instance))
}

async fn get_cursor_position(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, true)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let (line, column, error) = proxy
        .get_cursor_position(&path)
        .await
        .with_context(|| format!("calling GetCursorPosition on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    println!("{line}:{column}");
    Ok(())
}

async fn get_selection(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, true)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let (start_line, start_column, end_line, end_column, text, error) = proxy
        .get_selection(&path)
        .await
        .with_context(|| format!("calling GetSelection on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    println!("{start_line}:{start_column}-{end_line}:{end_column}");
    println!("{text}");
    Ok(())
}

async fn get_diagnostics(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, true)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let (diagnostics, error) = proxy
        .get_diagnostics(&path)
        .await
        .with_context(|| format!("calling GetDiagnostics on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    for (start_line, start_column, end_line, end_column, severity, message) in diagnostics {
        println!("{start_line}:{start_column}-{end_line}:{end_column} {severity}: {message}");
    }
    Ok(())
}

async fn get_workspace_root(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, false)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let (root, error) = proxy
        .get_workspace_root(&path)
        .await
        .with_context(|| format!("calling GetWorkspaceRoot on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    println!("{root}");
    Ok(())
}

async fn get_buffer_content(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, true)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let (content, error) = proxy
        .get_buffer_content(&path)
        .await
        .with_context(|| format!("calling GetBufferContent on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    print!("{content}");
    Ok(())
}

async fn is_modified(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, true)?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let (modified, error) = proxy
        .is_modified(&path)
        .await
        .with_context(|| format!("calling IsModified on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    println!("{modified}");
    Ok(())
}

async fn set_buffer_content(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let (path, instance) = parse_path_and_instance(args, true)?;

    let mut content = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut content)
        .context("reading new content from stdin")?;

    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let proxy = connect(&conn, &instance).await?;

    let error = proxy
        .set_buffer_content(&path, &content)
        .await
        .with_context(|| format!("calling SetBufferContent on {instance:?}"))?;
    if !error.is_empty() {
        bail!("{error}");
    }
    Ok(())
}

/// The per-PID bus name of every currently running Helix instance.
async fn list_instance_names(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .context("talking to org.freedesktop.DBus")?;
    let names = dbus.list_names().await?;
    Ok(names
        .iter()
        .map(|name| name.as_str().to_string())
        .filter(|name| name.starts_with(BUS_NAME_PID_PREFIX))
        .collect())
}

async fn list_instances() -> anyhow::Result<()> {
    let conn = Connection::session()
        .await
        .context("connecting to the D-Bus session bus")?;
    let names = list_instance_names(&conn).await?;

    if names.is_empty() {
        eprintln!("No running Helix instances found.");
    } else {
        for name in names {
            println!("{name}");
        }
    }
    Ok(())
}
