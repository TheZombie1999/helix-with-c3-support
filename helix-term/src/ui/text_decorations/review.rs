use helix_core::diagnostic::DiagnosticProvider;
use helix_core::Position;
use helix_view::{theme::Style, Document, Theme};

use crate::ui::document::{LinePos, TextRenderer};
use crate::ui::text_decorations::Decoration;

/// Renders `:review` comments (diagnostics with no backing language server,
/// see [`DiagnosticProvider::Helix`]) as a short end-of-line note styled like
/// a code comment, instead of the underline-plus-box treatment real LSP
/// diagnostics get. Always on, independent of the user's
/// `inline-diagnostics`/`end-of-line-diagnostics` config — those exist to
/// tame noisy LSP output, not this.
pub struct ReviewComments<'a> {
    doc: &'a Document,
    style: Style,
}

impl<'a> ReviewComments<'a> {
    pub fn new(doc: &'a Document, theme: &Theme) -> Self {
        ReviewComments {
            doc,
            style: theme.get("comment"),
        }
    }
}

impl Decoration for ReviewComments<'_> {
    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        if !pos.first_visual_line {
            return Position::default();
        }

        let mut messages = self
            .doc
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.line == pos.doc_line
                    && matches!(diagnostic.provider, DiagnosticProvider::Helix)
            })
            .map(|diagnostic| diagnostic.message.lines().next().unwrap_or_default())
            .peekable();
        if messages.peek().is_none() {
            return Position::default();
        }
        let message = messages.collect::<Vec<_>>().join(" · ");

        let col = virt_off.col;
        if !renderer.column_in_bounds(col, 1) {
            return Position::default();
        }

        let draw_col = col as u16 + 1;
        let width = renderer.viewport.width;
        let (end_col, _) = renderer.set_string_truncated(
            renderer.viewport.x + draw_col,
            pos.visual_line,
            &message,
            width.saturating_sub(draw_col) as usize,
            |_| self.style,
            true,
            false,
        );

        Position::new(0, (end_col - renderer.viewport.x) as usize - col)
    }
}
