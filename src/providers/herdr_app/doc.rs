//! The core's `doc.get herdr-app`, as the built-in reads it: spaces, each
//! of tabs, each a `root` split (`row` or `column`) whose children are
//! splits or `block`s; a block shows `content.terminal`, an hc terminal,
//! or a mod. The doc is the only map from a terminal to its Herdr app
//! pane: a block's id, such as `p4c22…`.

use serde_json::Value;

/// The ids of every block of `doc` showing terminal `terminal`, in the
/// doc's order: its Herdr app panes.
pub(super) fn panes_of(doc: &Value, terminal: &str) -> Vec<String> {
    let mut found = Vec::new();
    for space in doc["spaces"].as_array().into_iter().flatten() {
        for tab in space["tabs"].as_array().into_iter().flatten() {
            walk(&tab["root"], &mut |node| {
                if node["content"]["terminal"] == terminal
                    && let Some(pane) = node["id"].as_str()
                {
                    found.push(pane.to_string());
                }
            });
        }
    }
    found
}

/// The one block of `doc` showing terminal `terminal`: its Herdr app pane;
/// or why there's none, as when it shows in none or several.
pub(super) fn pane_of(doc: &Value, terminal: &str) -> Result<String, String> {
    match panes_of(doc, terminal).as_slice() {
        [] => Err(format!("{terminal} has no Herdr app pane")),
        [one] => Ok(one.clone()),
        several => Err(format!(
            "{terminal} shows in {} Herdr app panes",
            several.len()
        )),
    }
}

/// The ids of the spaces holding a block of `doc` showing terminal
/// `terminal`, one for each block, in the doc's order.
pub(super) fn spaces_of(doc: &Value, terminal: &str) -> Vec<String> {
    let mut found = Vec::new();
    for space in doc["spaces"].as_array().into_iter().flatten() {
        for tab in space["tabs"].as_array().into_iter().flatten() {
            walk(&tab["root"], &mut |node| {
                if node["content"]["terminal"] == terminal
                    && let Some(id) = space["id"].as_str()
                {
                    found.push(id.to_string());
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
        assert_eq!(
            panes_of(&nested, "t3"),
            ["p4c229a9b57964c201d7ab577ccf4a2ea"]
        );
        assert_eq!(
            panes_of(&nested, "t1"),
            ["pf7e3d563947903ed7a8c93ee0ac58181"]
        );
        assert!(panes_of(&nested, "t9").is_empty());
        assert!(panes_of(&Value::Null, "t1").is_empty());
    }

    #[test]
    fn a_terminal_has_one_pane_else_says_why_not() {
        let nested = doc("nested-doc.get");
        assert_eq!(
            pane_of(&nested, "t2"),
            Ok("p7b3398d57b6dd529d0f978c04b35c687".into())
        );
        assert_eq!(
            pane_of(&nested, "t9"),
            Err("t9 has no Herdr app pane".into())
        );
        let twice: Value =
            serde_json::from_str(&nested.to_string().replace("\"t2\"", "\"t3\"")).unwrap();
        assert_eq!(
            pane_of(&twice, "t3"),
            Err("t3 shows in 2 Herdr app panes".into())
        );
    }
}
