//! Where Rex's floating panes go and what size its new blocks start at: the
//! fractions of a Rex window `new_layer` takes, with room for the frame Rex
//! draws around a floating pane.

/// A new Rex block's size, cols x rows, until the app lays it out.
pub(crate) const UNLAID_SIZE: [u32; 2] = [80, 24];

/// Where a tab page's Rex floating pane sits, as fractions of its Rex window:
/// x, y, w, h.
pub(crate) const TAB_PAGE_BOUNDS: [f64; 4] = [0.05, 0.05, 0.9, 0.9];

/// About how many cells a Rex floating pane's frame takes, across and down.
/// Only a first guess: once made, its floats fit whatever size it is.
pub(crate) const LAYER_FRAME: [i32; 2] = [3, 2];

/// The size of the block of a tab page's Rex floating pane, cols x rows, in
/// a Rex window of `cols` x `rows`.
pub(crate) fn tab_page_size(cols: u32, rows: u32) -> [u32; 2] {
    let w = (f64::from(cols) * TAB_PAGE_BOUNDS[2]) as i64;
    let h = (f64::from(rows) * TAB_PAGE_BOUNDS[3]) as i64;
    [
        (w - i64::from(LAYER_FRAME[0])).max(1) as u32,
        (h - i64::from(LAYER_FRAME[1])).max(2) as u32,
    ]
}

/// The bounds of the Rex floating pane of a float group `cols` x `rows`
/// cells, at `x`, `y` cells from the top left of its origin's content,
/// inside its inset, as `open_float` asks for it (Rex reports no inset, so
/// that is its origin's top left): fractions of the Rex window, frame
/// included. `area_bounds`, the group as fractions of its origin's layout
/// area, the Rex window, give that window's cells; without them the
/// origin, `origin_cols` x `origin_rows`, is taken for the window. None
/// with no room to measure by.
pub(crate) fn float_bounds(
    [x, y]: [i32; 2],
    [cols, rows]: [u32; 2],
    [origin_cols, origin_rows]: [u32; 2],
    area_bounds: Option<[f64; 4]>,
) -> Option<[f64; 4]> {
    let Some(a) = area_bounds else {
        return bounds_around(x, y, cols, rows, origin_cols, origin_rows);
    };
    if !(a[2] > 0.0 && a[3] > 0.0) {
        return None;
    }
    let area_cols = (f64::from(cols) / a[2]).round();
    let area_rows = (f64::from(rows) / a[3]).round();
    bounds_around(
        (a[0] * area_cols).round() as i32,
        (a[1] * area_rows).round() as i32,
        cols,
        rows,
        area_cols as u32,
        area_rows as u32,
    )
}

/// Bounds for a Rex floating pane around `cols` x `rows` cells at `x`, `y` on a
/// window of `screen_cols` x `screen_rows`, as fractions of it, leaving room
/// for the frame Rex draws around it. None on an empty window.
fn bounds_around(
    x: i32,
    y: i32,
    cols: u32,
    rows: u32,
    screen_cols: u32,
    screen_rows: u32,
) -> Option<[f64; 4]> {
    if screen_cols == 0 || screen_rows == 0 {
        return None;
    }
    let sw = f64::from(screen_cols);
    let sh = f64::from(screen_rows);
    let fw = LAYER_FRAME[0];
    let fh = LAYER_FRAME[1];
    let w = ((i64::from(cols) + i64::from(fw)) as f64 / sw).min(1.0);
    let h = ((i64::from(rows) + i64::from(fh)) as f64 / sh).min(1.0);
    let x = (f64::from(x - fw.div_euclid(2)) / sw).clamp(0.0, 1.0 - w);
    let y = (f64::from(y - fh.div_euclid(2)) / sh).clamp(0.0, 1.0 - h);
    Some([x, y, w, h])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: [f64; 4], b: [f64; 4]) {
        for i in 0..4 {
            assert!((a[i] - b[i]).abs() < 1e-9, "{a:?} != {b:?}");
        }
    }

    /// A picker's group, 60 x 20 cells at column 10, row 4, of a 200 x 50
    /// Rex window, as `open_float` asks from a pane that is the whole window.
    fn picker(at: [i32; 2], size: [u32; 2]) -> Option<[f64; 4]> {
        float_bounds(at, size, [200, 50], None)
    }

    #[test]
    fn a_floating_panes_bounds_are_fractions_of_the_window_around_its_group_and_its_frame() {
        // 3 x 2 cells of frame: half of each around the group
        approx(
            [0.045, 0.06, 0.315, 0.44],
            picker([10, 4], [60, 20]).unwrap(),
        );

        // too big for the window: it fills it
        approx([0.0, 0.0, 1.0, 1.0], picker([10, 4], [300, 80]).unwrap());

        // at the right and bottom edges: pulled back in, so it's whole
        let b = picker([190, 45], [60, 20]).unwrap();
        approx([1.0 - b[2], 1.0 - b[3], 63.0 / 200.0, 22.0 / 50.0], b);

        // off the top left: pulled in to it
        let b = picker([0, 0], [60, 20]).unwrap();
        assert_eq!([0.0, 0.0], [b[0], b[1]]);

        // nothing to measure by
        assert_eq!(None, float_bounds([10, 4], [60, 20], [0, 0], None));
        assert_eq!(
            None,
            float_bounds([10, 4], [60, 20], [100, 50], Some([0.5, 0.1, 0.0, 0.4]))
        );
    }

    #[test]
    fn a_floats_area_bounds_place_it_in_the_window_not_its_origin() {
        // the origin is the right half of the 200 x 50 window: the group is
        // 10 cells into it, 110 into the window
        let b = float_bounds(
            [10, 4],
            [60, 20],
            [100, 50],
            Some([110.0 / 200.0, 4.0 / 50.0, 60.0 / 200.0, 20.0 / 50.0]),
        );
        approx([0.545, 0.06, 0.315, 0.44], b.unwrap());
    }

    #[test]
    fn a_tab_pages_block_is_its_bounds_of_the_window_less_the_frame() {
        assert_eq!([177, 43], tab_page_size(200, 50));
        assert_eq!([1, 2], tab_page_size(2, 2));
    }
}
