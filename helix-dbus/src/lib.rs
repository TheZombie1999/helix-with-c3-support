//! The D-Bus wire contract for Helix's remote-control interface: shared
//! naming constants plus a generated client proxy ([`proxy::Helix1Proxy`]).
//!
//! `helix1.xml` (next to this file) is the source of truth for the
//! interface; `src/proxy.rs` is generated from it via `cargo xtask
//! dbus-codegen` (or `just dbus-codegen`). This crate intentionally depends
//! on nothing but `zbus`, so that lightweight clients (like `helix-remote`)
//! don't have to pull in the editor itself (and its tree-sitter grammar
//! build step) just to talk to one.
//!
//! The server implementation lives in `helix-term::dbus`, since it needs
//! the actual `Editor`; it must be kept in sync with `helix1.xml` by hand.

#[cfg(unix)]
pub mod proxy;

/// D-Bus interface implemented by a running Helix instance.
pub const INTERFACE_NAME: &str = "org.helix_editor.Helix1";
/// Object path at which the interface is served.
pub const OBJECT_PATH: &str = "/org/helix_editor/Helix";
/// Bare well-known bus name a single, arbitrary instance may additionally
/// claim (in practice, whichever instance started first), for clients that
/// don't care which instance they talk to.
pub const SHARED_WELL_KNOWN_NAME: &str = "org.helix_editor.Helix";
/// Prefix of the per-PID bus name every instance claims for itself; see
/// [`pid_bus_name`].
pub const BUS_NAME_PID_PREFIX: &str = "org.helix_editor.Helix.pid";

/// The bus name a Helix instance with process ID `pid` claims for itself.
pub fn pid_bus_name(pid: u32) -> String {
    format!("{BUS_NAME_PID_PREFIX}{pid}")
}
