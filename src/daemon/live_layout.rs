//! The live layout (ADR 0008, #92 decision 6): where a pane provider's
//! terminals sit and which share a layout area, as it reports them. Plain
//! values; the daemon gives its panes their `Area`s from them.

use super::provider::TerminalId;

/// What a pane provider reports of its terminals' places.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LiveLayout {
    pub areas: Vec<LayoutArea>,
    /// The terminal that has focus, if the pane provider knows. Unread: focus
    /// moves come as `ProviderEvent::Focused`.
    #[allow(dead_code)]
    pub current: Option<TerminalId>,
}

/// One layout area: the space a group of terminals share, such as a Rex
/// window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayoutArea {
    /// The pane provider's id for it, opaque.
    pub id: String,
    pub terminals: Vec<PlacedTerminal>,
}

/// A terminal in a layout area.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlacedTerminal {
    pub terminal: TerminalId,
    /// x, y, w, h as fractions of its area.
    pub rect: [f64; 4],
    /// Floating over the area's tiled terminals, such as in a Rex floating
    /// pane.
    pub floating: bool,
    /// The cells at each edge of `rect` that aren't the terminal's, such as a
    /// floating pane's frame: left, top, right, bottom.
    pub inset: [u16; 4],
}

/// Where a pane is, as the live layout last said (#92 decision 17).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Area {
    /// In layout area `id`, at `rect`, as fractions of it: x, y, w, h.
    Live {
        id: String,
        rect: [f64; 4],
        floating: bool,
    },
    /// A layout area of its own.
    Own,
    /// A requested pane the live layout hasn't reported yet: in no layout
    /// area.
    Unreported,
}

impl LiveLayout {
    /// Where terminal `t` is, if the layout names it; the last place it is
    /// named wins.
    pub(crate) fn area_of(&self, t: &TerminalId) -> Option<Area> {
        let (a, p) = self.placed(t)?;
        Some(Area::Live {
            id: a.id.clone(),
            rect: p.rect,
            floating: p.floating,
        })
    }

    /// Terminal `t` as the layout places it, and its area; the last place it
    /// is named wins.
    pub(crate) fn placed(&self, t: &TerminalId) -> Option<(&LayoutArea, &PlacedTerminal)> {
        let mut found = None;
        for a in &self.areas {
            for p in &a.terminals {
                if &p.terminal == t {
                    found = Some((a, p));
                }
            }
        }
        found
    }
}

impl Area {
    /// Its rect in its layout area, if it's in a live one.
    pub(crate) fn rect(&self) -> Option<[f64; 4]> {
        match self {
            Area::Live { rect, .. } => Some(*rect),
            _ => None,
        }
    }
}
