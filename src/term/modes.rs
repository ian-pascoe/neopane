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
