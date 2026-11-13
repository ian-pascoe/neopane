//! Terminal modes for running an editor, and how to undo them.

use super::input::{kitty_pop_literal, kitty_push_literal, kitty_query_literal};

/// Alternate screen, bracketed paste, focus reports, mouse (1002 drags, 1006
/// SGR encoding), then the kitty keyboard flags, then a query for them: the
/// answer tells the input parser they are on.
pub(crate) const EDITOR_ON: &str = concat!(
    "\x1b[?1049h\x1b[?2004h\x1b[?1004h\x1b[?1002h\x1b[?1006h",
    kitty_push_literal!(),
    kitty_query_literal!()
);

/// Undoes `EDITOR_ON` in reverse, and resets colors and the cursor's shape
/// and visibility, which Neovim may have changed.
pub(crate) const EDITOR_OFF: &str = concat!(
    kitty_pop_literal!(),
    "\x1b[?1006l\x1b[?1002l\x1b[?1004l\x1b[?2004l\x1b[0m\x1b[0 q\x1b[?25h\x1b[?1049l"
);

/// The bytes that undo what the daemon writes to a pane's terminal, for its
/// helper to write as the attach ends (ADR 0013): the end of a synchronized
/// update a frame cut short left open, then `EDITOR_OFF`.
pub(crate) fn undo() -> Vec<u8> {
    [b"\x1b[?2026l", EDITOR_OFF.as_bytes()].concat()
}

/// Saves the terminal's title on its title stack (xterm's `CSI 22;2 t`),
/// before the daemon writes a pane title (ADR 0006).
pub(crate) const TITLE_PUSH: &str = "\x1b[22;2t";

/// Clears the title, then puts back the one saved (`CSI 23;2 t`): a
/// terminal with no title stack is left with none (#91 decision 6).
pub(crate) const TITLE_POP: &str = "\x1b]2;\x1b\\\x1b[23;2t";

/// The bytes that save the terminal's title and set it to `title`.
pub(crate) fn set_title(title: &str) -> Vec<u8> {
    format!("{TITLE_PUSH}\x1b]2;{title}\x1b\\").into_bytes()
}

/// `undo`, once a pane title is written: it puts the title back too.
pub(crate) fn undo_with_title() -> Vec<u8> {
    [undo(), TITLE_POP.as_bytes().to_vec()].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::input;

    // Checks the literals match input's consts.
    #[test]
    fn editor_modes_wrap_the_kitty_flags() {
        assert!(EDITOR_ON.ends_with(&format!("{}{}", input::KITTY_PUSH, input::KITTY_QUERY)));
        assert!(EDITOR_OFF.starts_with(input::KITTY_POP));
    }
}
