//! Which window a self-attach shows when it names no file (#83, ADR 0012):
//! an unshown window, the current one first, then by tab page and window
//! order.

/// The unshown window a self-attached pane takes, if any. `current` is
/// Neovim's current window; `wins` the windows a pane could show, in tab
/// page order and then window order; `held` says which of them a pane
/// shows or one is on its way for. The current window counts only if it is
/// among `wins`: a float or an anchor isn't.
pub(crate) fn pick_unshown_window(
    current: i64,
    wins: &[i64],
    held: impl Fn(i64) -> bool,
) -> Option<i64> {
    if wins.contains(&current) && !held(current) {
        return Some(current);
    }
    wins.iter().copied().find(|&w| !held(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_window_comes_first_when_unshown() {
        assert_eq!(
            Some(1002),
            pick_unshown_window(1002, &[1000, 1001, 1002], |_| false)
        );
    }

    #[test]
    fn then_the_first_unshown_in_tab_page_and_window_order() {
        let held = |w| w == 1000 || w == 1002;
        assert_eq!(
            Some(1001),
            pick_unshown_window(1000, &[1000, 1001, 1003], held)
        );
        assert_eq!(
            Some(1003),
            pick_unshown_window(1000, &[1003, 1001], |w| w == 1000)
        );
    }

    #[test]
    fn a_window_held_or_with_a_pane_on_its_way_is_never_taken() {
        // 1001's requested pane is on its way: as held as one shown
        let on_its_way = |w| w == 1001;
        assert_eq!(None, pick_unshown_window(1001, &[1001], on_its_way));
        assert_eq!(
            Some(1002),
            pick_unshown_window(1001, &[1001, 1002], on_its_way)
        );
    }

    #[test]
    fn a_current_window_no_pane_could_show_isnt_taken() {
        // a float, say
        assert_eq!(Some(1000), pick_unshown_window(2000, &[1000], |_| false));
        assert_eq!(None, pick_unshown_window(2000, &[], |_| false));
    }
}
