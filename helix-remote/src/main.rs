//! `hx-remote`: a small CLI client for Helix's D-Bus remote-control
//! interface (see `helix-dbus` for the wire contract and `helix-term::dbus`
//! for the server side). Lets you open a file in a running `hx`, optionally
//! blocking until it's closed again so Helix can act as `$EDITOR`.

#[cfg(unix)]
mod cli;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    cli::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("hx-remote talks to Helix over D-Bus, which is only available on Unix.");
    std::process::ExitCode::FAILURE
}
