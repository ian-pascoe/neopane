//! Reads Rex's session view, the per-window JSON list, into a `LiveLayout`:
//! one layout area per Rex window, holding every block Rex placed in it
//! with its rect. It knows nothing of our panes.

use serde_json::Value;

use super::bounds::LAYER_FRAME;
use crate::daemon::live_layout::{LayoutArea, LiveLayout, PlacedTerminal};
use crate::daemon::provider::TerminalId;
use crate::rex::client;

/// The cells of a Rex floating pane's frame at each edge: left, top, right,
/// bottom. `LAYER_FRAME`'s cells across and down, the lesser half before.
pub(crate) const FRAME_INSET: [u16; 4] = {
    let [across, down] = LAYER_FRAME;
    let (left, top) = (across.div_euclid(2), down.div_euclid(2));
    [
        left as u16,
        top as u16,
        (across - left) as u16,
        (down - top) as u16,
    ]
};

/// The layout `wins` says, its focused block being `current`. A block in
/// `float_blocks`, opened for a float group, is placed at its floating
/// layer's bounds, frame and all, with the frame as its inset.
pub(crate) fn read(
    wins: &[Value],
    current: Option<&str>,
    float_blocks: &[TerminalId],
) -> LiveLayout {
    let mut l = LiveLayout {
        areas: Vec::new(),
        current: current.map(TerminalId::from),
    };
    for (i, w) in wins.iter().enumerate() {
        let Some(Value::Array(layers)) = client::get(w, "layers") else {
            continue;
        };
        let mut terminals = Vec::new();
        for layer in layers {
            let floating = client::get_str(layer, "kind").unwrap_or("") != "tiled";
            let mut found = Vec::new();
            if let Some(node) = client::get(layer, "layout") {
                read_rects(node, &mut found);
            }
            let framed = floating
                .then(|| framed_block(layer, float_blocks))
                .flatten();
            if let Some((block, _)) = &framed {
                found.retain(|(b, _)| b != block);
            }
            terminals.extend(found.into_iter().map(|(b, rect)| PlacedTerminal {
                terminal: TerminalId::from(b.as_str()),
                rect,
                floating,
                inset: [0; 4],
            }));
            if let Some((block, bounds)) = framed {
                terminals.push(PlacedTerminal {
                    terminal: TerminalId::from(block.as_str()),
                    rect: bounds,
                    floating,
                    inset: FRAME_INSET,
                });
            }
        }
        if terminals.is_empty() {
            continue;
        }
        let id = match client::get_str(w, "window_id") {
            Some(id) => id.to_string(),
            // never one of Rex's ids
            None => format!("#{i}"),
        };
        l.areas.push(LayoutArea { id, terminals });
    }
    l
}

/// The first block of floating layer `layer`, if it's one of `float_blocks`,
/// and the layer's bounds, if it has them whole.
fn framed_block(layer: &Value, float_blocks: &[TerminalId]) -> Option<(String, [f64; 4])> {
    let bounds = rect4(client::get(layer, "bounds"))?;
    let Some(Value::Array(blocks)) = client::get(layer, "blocks") else {
        return None;
    };
    let block = client::get_str(blocks.first()?, "block_id")?;
    float_blocks
        .iter()
        .any(|t| t.as_str() == block)
        .then(|| (block.to_string(), bounds))
}

/// Finds the blocks under layout node `node` that have rects.
fn read_rects(node: &Value, out: &mut Vec<(String, [f64; 4])>) {
    match node {
        Value::Object(o) => {
            if let Some(b) = client::str(o.get("block_id"))
                && let Some(r) = rect4(o.get("rect"))
            {
                out.push((b.to_string(), r));
            }
            for v in o.values() {
                read_rects(v, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                read_rects(x, out);
            }
        }
        _ => {}
    }
}

/// `[x, y, w, h]` as fractions.
fn rect4(v: Option<&Value>) -> Option<[f64; 4]> {
    let Some(Value::Array(a)) = v else {
        return None;
    };
    if a.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (x, o) in a.iter().zip(out.iter_mut()) {
        // a number std.json read as a float or an integer; it kept others as strings
        let Value::Number(n) = x else {
            return None;
        };
        *o = if n.is_f64() {
            n.as_f64()?
        } else {
            n.as_i64()? as f64
        };
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block_id(n: u32) -> String {
        format!("block:00000000-0000-0000-0000-{n:0>12}")
    }

    fn window_id(n: u32) -> String {
        format!("window:00000000-0000-0000-0000-{n:0>12}")
    }

    fn placed(n: u32, rect: [f64; 4], floating: bool) -> PlacedTerminal {
        PlacedTerminal {
            terminal: TerminalId::from(block_id(n).as_str()),
            rect,
            floating,
            inset: [0; 4],
        }
    }

    fn area(id: &str, terminals: Vec<PlacedTerminal>) -> LayoutArea {
        LayoutArea {
            id: id.to_string(),
            terminals,
        }
    }

    #[test]
    fn the_frame_inset_is_the_frame_split_with_the_lesser_half_before() {
        assert_eq!([1, 1, 2, 1], FRAME_INSET);
    }

    #[test]
    fn rect4_reads_four_numbers_and_nothing_else() {
        let v = json!([0.25, 0, 1, 0.5]);
        assert_eq!(Some([0.25, 0.0, 1.0, 0.5]), rect4(Some(&v)));
        assert_eq!(None, rect4(None));
        assert_eq!(None, rect4(Some(&json!([0.1, 0.2, 0.3]))));
        assert_eq!(None, rect4(Some(&json!([0.1, 0.2, 0.3, 0.4, 0.5]))));
        // std.json keeps some numbers as strings: not ones to read
        assert_eq!(None, rect4(Some(&json!([0.1, 0.2, 0.3, "0.4"]))));
        assert_eq!(None, rect4(Some(&json!({ "x": 1 }))));
    }

    #[test]
    fn each_rex_window_is_a_layout_area_holding_its_blocks_with_their_rects() {
        let wins = vec![
            json!({ "window_id": window_id(8), "layers": [ { "kind": "tiled", "layout": {
                "kind": "leaf", "block_id": block_id(99), "rect": [0, 0, 1, 1] } } ] }),
            json!({ "window_id": window_id(9), "layers": [ { "kind": "tiled", "layout": {
                "kind": "split", "children": [
                    { "kind": "leaf", "block_id": block_id(1), "rect": [0.0, 0.0, 0.5, 1.0] },
                    { "kind": "split", "children": [
                        { "kind": "leaf", "block_id": block_id(2), "rect": [0.5, 0.0, 0.5, 0.25] },
                        // a bad rect: not read
                        { "kind": "leaf", "block_id": block_id(3), "rect": [0.5, 0.25, 0.5] },
                    ] },
                ] } } ] }),
        ];
        let l = read(&wins, Some(&block_id(2)), &[]);
        assert_eq!(
            LiveLayout {
                areas: vec![
                    area(&window_id(8), vec![placed(99, [0.0, 0.0, 1.0, 1.0], false)]),
                    area(
                        &window_id(9),
                        vec![
                            placed(1, [0.0, 0.0, 0.5, 1.0], false),
                            placed(2, [0.5, 0.0, 0.5, 0.25], false),
                        ]
                    ),
                ],
                current: Some(TerminalId::from(block_id(2).as_str())),
            },
            l
        );
    }

    #[test]
    fn blocks_of_floating_layers_are_floating_and_a_window_with_none_is_no_area() {
        let l = read(
            &[json!({ "window_id": window_id(4), "layers": [
                { "kind": "tiled", "layout": {
                    "block_id": block_id(1), "rect": [0.0, 0.0, 1.0, 1.0] } },
                { "kind": "floating", "bounds": [0.05, 0.05, 0.9, 0.9],
                  "blocks": [ { "block_id": block_id(2) } ],
                  "layout": { "block_id": block_id(2), "rect": [0.1, 0.1, 0.8, 0.8] } } ] })],
            None,
            &[],
        );
        assert_eq!(
            vec![area(
                &window_id(4),
                vec![
                    placed(1, [0.0, 0.0, 1.0, 1.0], false),
                    // not a float group's: its layout's rect, no frame
                    placed(2, [0.1, 0.1, 0.8, 0.8], true),
                ]
            )],
            l.areas
        );
        assert_eq!(None, l.current);
        // no layers, or not a list, or none with a rect
        for w in [
            json!({ "window_id": window_id(4) }),
            json!({ "layers": 3 }),
            json!({ "window_id": window_id(4), "layers": [ { "kind": "tiled" } ] }),
        ] {
            assert_eq!(LiveLayout::default(), read(&[w], None, &[]));
        }
    }

    #[test]
    fn a_window_without_an_id_is_still_an_area_of_its_own() {
        let layers = json!([ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 1.0, 1.0] } } ]);
        let l = read(
            &[
                json!({ "layers": layers }),
                json!({ "window_id": "window:short", "layers": layers }),
                json!({ "layers": layers }),
            ],
            None,
            &[],
        );
        let ids: Vec<&str> = l.areas.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(vec!["#0", "window:short", "#2"], ids);
    }

    #[test]
    fn a_float_group_block_is_at_its_layers_bounds_inset_by_the_frame_only_if_complete() {
        let moved = [0.1, 0.2, 0.3, 0.4];
        let float_blocks = [TerminalId::from(block_id(7).as_str())];
        let view = |layer: Value| {
            read(
                &[json!({ "window_id": window_id(1), "layers": [layer] })],
                None,
                &float_blocks,
            )
        };
        let l = view(json!({ "kind": "floating", "bounds": moved,
            "blocks": [ { "block_id": block_id(7) }, { "block_id": block_id(8) } ],
            "layout": { "block_id": block_id(7), "rect": [0.0, 0.0, 1.0, 1.0] } }));
        assert_eq!(
            vec![PlacedTerminal {
                inset: FRAME_INSET,
                ..placed(7, moved, true)
            }],
            l.areas[0].terminals
        );
        // without whole bounds, or not its layer's first: no frame
        for layer in [
            json!({ "kind": "floating", "blocks": [ { "block_id": block_id(7) } ] }),
            json!({ "kind": "floating", "bounds": [0.1, 0.2],
                "blocks": [ { "block_id": block_id(7) } ] }),
            json!({ "kind": "floating", "bounds": moved }),
            json!({ "kind": "floating", "bounds": moved, "blocks": [] }),
            json!({ "kind": "floating", "bounds": moved, "blocks": [ {} ] }),
            json!({ "kind": "floating", "bounds": moved,
                "blocks": [ { "block_id": block_id(8) }, { "block_id": block_id(7) } ] }),
        ] {
            assert_eq!(LiveLayout::default(), view(layer));
        }
        // a block that isn't a float group's: its layout rect alone
        let l = read(
            &[json!({ "window_id": window_id(1), "layers": [
                { "kind": "floating", "bounds": moved, "blocks": [ { "block_id": block_id(9) } ],
                  "layout": { "block_id": block_id(9), "rect": [0.0, 0.0, 1.0, 1.0] } } ] })],
            None,
            &float_blocks,
        );
        assert_eq!(
            vec![placed(9, [0.0, 0.0, 1.0, 1.0], true)],
            l.areas[0].terminals
        );
    }

    #[test]
    fn the_same_view_reads_to_equal_layouts() {
        let v = json!({ "window_id": window_id(1), "layers": [ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 1.0, 1.0] } } ] });
        let w = json!({ "window_id": window_id(1), "layers": [ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 0.5, 1.0] } } ] });
        let read1 = |v: &Value| read(std::slice::from_ref(v), None, &[]);
        assert_eq!(read1(&v), read1(&v));
        assert_ne!(read1(&v), read1(&w));
    }
}
