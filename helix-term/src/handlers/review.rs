use helix_event::register_hook;
use helix_view::{events::DocumentDidOpen, handlers::Handlers};

use crate::review;

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        review::refresh_diagnostics(event.editor, event.doc);
        Ok(())
    });
}
