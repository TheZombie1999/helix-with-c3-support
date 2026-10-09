//! Regenerates `helix-dbus/src/proxy.rs`, the client proxy for Helix's
//! D-Bus interface, from the interface specification in
//! `helix-dbus/helix1.xml`.
//!
//! The XML file is the source of truth for the interface; the server
//! implementation in `helix-term/src/dbus/mod.rs` is hand-written and must
//! be kept in sync with it manually.

use std::fs;

use zbus::zvariant::ObjectPath;
use zbus_xml::Node;
use zbus_xmlgen::CodeGenerator;

use crate::{path, DynError};

pub fn generate() -> Result<(), DynError> {
    let dir = path::project_root().join("helix-dbus");
    let xml_path = dir.join("helix1.xml");
    let out_path = dir.join("src/proxy.rs");

    let xml = fs::read(&xml_path)?;
    let node = Node::from_reader(&*xml)?;
    let interface = node
        .interfaces()
        .iter()
        .find(|iface| iface.name() == helix_dbus::INTERFACE_NAME)
        .ok_or_else(|| {
            format!(
                "{} not found in {}",
                helix_dbus::INTERFACE_NAME,
                xml_path.display()
            )
        })?;

    let object_path = ObjectPath::try_from(helix_dbus::OBJECT_PATH)?;
    let code = CodeGenerator::new()
        .with_node_types(node.telepathy_types())
        .with_path(Some(&object_path))
        .with_format(true)
        .file_code(
            std::slice::from_ref(interface),
            &[],
            "helix1.xml",
            "cargo xtask dbus-codegen",
            env!("CARGO_PKG_VERSION"),
        )?;

    fs::write(&out_path, code)?;
    println!("Wrote {}", out_path.display());

    Ok(())
}
