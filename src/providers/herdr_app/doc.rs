//! The core's `doc.get herdr-app`, as the built-in reads it: spaces, each
//! of tabs, each a `root` split (`row` or `column`) whose children are
//! splits or `block`s; a block shows `content.terminal`, an hc terminal,
//! or a mod. The doc is the only map from a terminal to its Herdr app
//! pane: a block's id, such as `p4c22…`.

use serde_json::Value;

/// A block of the doc showing a terminal: its Herdr app pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Block {
    /// The pane's id, the block's.
    pub pane: String,
    pub tab: String,
    pub space: String,
}

/// Every block of `doc` showing terminal `terminal`, in the doc's order.
pub(super) fn blocks_of(doc: &Value, terminal: &str) -> Vec<Block> {
    let mut found = Vec::new();
    for space in doc["spaces"].as_array().into_iter().flatten() {
        let space_id = space["id"].as_str().unwrap_or_default();
        for tab in space["tabs"].as_array().into_iter().flatten() {
            let tab_id = tab["id"].as_str().unwrap_or_default();
            walk(&tab["root"], &mut |node| {
                if node["content"]["terminal"] == terminal
                    && let Some(pane) = node["id"].as_str()
                {
                    found.push(Block {
                        pane: pane.into(),
                        tab: tab_id.into(),
                        space: space_id.into(),
                    });
                }
            });
        }
    }
    found
}

/// Calls `f` on every block under `node`, `node` too.
fn walk(node: &Value, f: &mut impl FnMut(&Value)) {
    if node["type"] == "block" {
        f(node);
    }
    for child in node["children"].as_array().into_iter().flatten() {
        walk(child, f);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::recorded_one;
    use super::*;

    fn doc(name: &str) -> Value {
        let got = recorded_one(name, "doc.get");
        got.replies[0]["result"]["value"].clone()
    }

    #[test]
    fn a_terminal_is_found_in_its_block_however_deep() {
        let nested = doc("nested-doc.get");
        let tab = "tb14c28a7769ba1044103fd9584cc818f0";
        let space = "sp22637f42bbefeecf910ee187a961334c";
        assert_eq!(
            blocks_of(&nested, "t3"),
            [Block {
                pane: "p4c229a9b57964c201d7ab577ccf4a2ea".into(),
                tab: tab.into(),
                space: space.into(),
            }]
        );
        assert_eq!(
            blocks_of(&nested, "t1")[0].pane,
            "pf7e3d563947903ed7a8c93ee0ac58181"
        );
        assert_eq!(blocks_of(&nested, "t9"), []);
        assert_eq!(blocks_of(&Value::Null, "t1"), []);
    }
}
