//! Reads Rex's session view, the per-window JSON list, into a
//! `LayoutSnapshot`: plain values the daemon applies to its panes. It knows
//! nothing of our panes, so the snapshot holds every block Rex placed.

use serde_json::Value;

use crate::rex::client;

/// What a view says, in the order it said it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LayoutSnapshot {
    /// Each block's rect in its window, as fractions: x, y, w, h. A block
    /// named twice is in the list twice, the later one is current.
    pub rects: Vec<(String, [f64; 4])>,
    /// Floating layers with their first block: where the user may have moved them.
    pub layer_bounds: Vec<LayerBounds>,
    /// The windows with tiled blocks that have rects.
    pub windows: Vec<WindowTiles>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayerBounds {
    pub block: String,
    /// x, y, w, h as fractions of the window.
    pub bounds: [f64; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowTiles {
    /// None unless the view's id is the length of Rex's.
    pub id: Option<[u8; 43]>,
    /// Its tiled blocks that have a rect.
    pub blocks: Vec<String>,
}

pub(crate) fn read(wins: &[Value]) -> LayoutSnapshot {
    let mut s = LayoutSnapshot::default();
    for w in wins {
        let Some(Value::Array(layers)) = client::get(w, "layers") else {
            continue;
        };
        let mut blocks = Vec::new();
        for layer in layers {
            let tiled = client::get_str(layer, "kind").unwrap_or("") == "tiled";
            if !tiled {
                read_layer_bounds(layer, &mut s.layer_bounds);
            }
            // a tab page's windows float over the tiled ones
            if let Some(l) = client::get(layer, "layout") {
                let mut found = Vec::new();
                read_rects(l, &mut found);
                if tiled {
                    blocks.extend(found.iter().map(|(b, _)| b.clone()));
                }
                s.rects.extend(found);
            }
        }
        if blocks.is_empty() {
            continue;
        }
        let id = client::get_str(w, "window_id")
            .filter(|id| id.len() == 43)
            .map(|id| id.as_bytes().try_into().unwrap());
        s.windows.push(WindowTiles { id, blocks });
    }
    s
}

fn read_layer_bounds(layer: &Value, out: &mut Vec<LayerBounds>) {
    let Some(bounds) = rect4(client::get(layer, "bounds")) else {
        return;
    };
    let Some(Value::Array(blocks)) = client::get(layer, "blocks") else {
        return;
    };
    let Some(block) = blocks
        .first()
        .and_then(|first| client::get_str(first, "block_id"))
    else {
        return;
    };
    out.push(LayerBounds {
        block: block.to_string(),
        bounds,
    });
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
        let id = format!("block:00000000-0000-0000-0000-{n:0>12}");
        id
    }

    fn window_id(n: u32) -> String {
        let id = format!("window:00000000-0000-0000-0000-{n:0>12}");
        assert_eq!(43, id.len());
        id
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
    fn tiled_layouts_give_each_block_its_rect_and_the_window_its_blocks() {
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
        let s = read(&wins);
        assert_eq!(
            vec![
                (block_id(99), [0.0, 0.0, 1.0, 1.0]),
                (block_id(1), [0.0, 0.0, 0.5, 1.0]),
                (block_id(2), [0.5, 0.0, 0.5, 0.25]),
            ],
            s.rects
        );
        assert_eq!(2, s.windows.len());
        assert_eq!(
            Some(window_id(9).as_bytes().try_into().unwrap()),
            s.windows[1].id
        );
        assert_eq!(vec![block_id(1), block_id(2)], s.windows[1].blocks);
        assert!(s.layer_bounds.is_empty());
    }

    #[test]
    fn a_window_id_of_the_wrong_length_or_missing_is_none_and_floating_blocks_arent_tiled() {
        let layers = json!([ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 1.0, 1.0] } } ]);
        let s = read(&[json!({ "window_id": "window:short", "layers": layers })]);
        assert_eq!(None, s.windows[0].id);
        assert_eq!(1, s.rects.len());
        let s = read(&[json!({ "layers": layers })]);
        assert_eq!(None, s.windows[0].id);
        // only in a floating layer: a rect, but the tiled ones hold the window
        let s = read(&[
            json!({ "window_id": window_id(4), "layers": [ { "kind": "floating",
            "layout": { "block_id": block_id(1), "rect": [0.1, 0.1, 0.2, 0.2] } } ] }),
        ]);
        assert!(s.windows.is_empty());
        assert_eq!(vec![(block_id(1), [0.1, 0.1, 0.2, 0.2])], s.rects);
        // no layers, or not a list
        assert_eq!(
            LayoutSnapshot::default(),
            read(&[json!({ "window_id": window_id(4) })])
        );
        assert_eq!(LayoutSnapshot::default(), read(&[json!({ "layers": 3 })]));
    }

    #[test]
    fn a_floating_layers_bounds_are_read_for_its_first_block_only_if_complete() {
        let moved = [0.1, 0.2, 0.3, 0.4];
        let view = |layer: Value| read(&[json!({ "window_id": window_id(1), "layers": [layer] })]);
        let s = view(json!({ "kind": "floating", "bounds": moved,
            "blocks": [ { "block_id": block_id(7) }, { "block_id": block_id(8) } ] }));
        assert_eq!(
            vec![LayerBounds {
                block: block_id(7),
                bounds: moved
            }],
            s.layer_bounds
        );
        for layer in [
            json!({ "kind": "floating", "blocks": [ { "block_id": block_id(7) } ] }),
            json!({ "kind": "floating", "bounds": [0.1, 0.2],
                "blocks": [ { "block_id": block_id(7) } ] }),
            json!({ "kind": "floating", "bounds": moved }),
            json!({ "kind": "floating", "bounds": moved, "blocks": [] }),
            json!({ "kind": "floating", "bounds": moved, "blocks": [ {} ] }),
        ] {
            assert!(view(layer).layer_bounds.is_empty());
        }
    }

    #[test]
    fn the_same_view_reads_to_equal_snapshots() {
        let v = json!({ "window_id": window_id(1), "layers": [ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 1.0, 1.0] } } ] });
        assert_eq!(
            read(std::slice::from_ref(&v)),
            read(std::slice::from_ref(&v))
        );
        let w = json!({ "window_id": window_id(1), "layers": [ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 0.5, 1.0] } } ] });
        assert_ne!(read(&[v]), read(&[w]));
    }
}
