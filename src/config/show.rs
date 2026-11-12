//! `provider show [NAME]` (#94 decision 34): a built-in pane provider as the
//! Lua table a config could return in its place.

use std::fmt::Write as _;

use super::check::{Builtin, Capability, Setting, number};
use crate::name::NAME;

/// A built-in pane provider's table, as `show` writes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub name: String,
    /// In any order: written in [`Capability::ALL`]'s.
    pub capabilities: Vec<(Capability, Vec<Vec<u8>>)>,
    /// The settings it sets besides `name`, in any order: written in
    /// [`Setting::ALL`]'s.
    pub settings: Vec<(Setting, Value)>,
}

/// A value in a [`Table`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(Vec<u8>),
    Number(f64),
    List(Vec<Value>),
    /// Keys in the order written.
    Map(Vec<(String, Value)>),
}

impl Builtin {
    /// Its Lua table, if it has one. Rex is only `"rex"`.
    pub fn table(self) -> Option<Table> {
        match self {
            Builtin::Rex => None,
        }
    }
}

/// What `provider show [NAME]` prints on stdout, or why it exits 1.
pub fn show(name: Option<&str>, build: &str) -> Result<String, String> {
    let Some(name) = name else {
        return Ok(listing());
    };
    let names: Vec<&str> = Builtin::ALL.iter().map(|b| b.name()).collect();
    let builtin = Builtin::from_name(name).ok_or_else(|| {
        format!(
            "no built-in pane provider named {name} (built-ins: {})",
            names.join(", ")
        )
    })?;
    match builtin.table() {
        Some(table) => Ok(lua_file(&table, build)),
        None => Err(format!(
            "{name} is built into {NAME} and has no Lua table; return \"{name}\" from provider() \
             to use it"
        )),
    }
}

/// One line per built-in.
fn listing() -> String {
    let width = Builtin::ALL
        .iter()
        .map(|b| b.name().len())
        .max()
        .unwrap_or(0)
        + 2;
    Builtin::ALL
        .iter()
        .map(|b| {
            let what = match b.table() {
                Some(_) => format!("{NAME}.builtin.{}()", b.name()),
                None => format!(
                    "return \"{}\" from provider() (built in; no Lua table)",
                    b.name()
                ),
            };
            format!("{:width$}{what}\n", b.name())
        })
        .collect()
}

/// `table` as a Lua file: two header lines, then `return { … }`.
pub fn lua_file(table: &Table, build: &str) -> String {
    let mut out = format!(
        "-- {NAME}.builtin.{}() in {build}.\n\
         -- To change it, copy it into your init.lua and return it from provider().\n\
         return {{\n",
        table.name
    );
    let mut line = |key: &str, value: &Value| {
        let _ = writeln!(out, "  {} = {},", lua_key(key), lua_value(value));
    };
    line("name", &Value::String(table.name.clone().into_bytes()));
    for cap in Capability::ALL {
        if let Some((_, argv)) = table.capabilities.iter().find(|(c, _)| *c == cap) {
            let words = argv.iter().map(|w| Value::String(w.clone())).collect();
            line(cap.name(), &Value::List(words));
        }
    }
    for setting in Setting::ALL {
        if let Some((_, v)) = table.settings.iter().find(|(s, _)| *s == setting) {
            line(setting.name(), v);
        }
    }
    out.push_str("}\n");
    out
}

fn lua_key(key: &str) -> String {
    let ident = key
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ident {
        key.to_string()
    } else {
        format!("[{}]", lua_string(key.as_bytes()))
    }
}

fn lua_value(value: &Value) -> String {
    match value {
        Value::String(s) => lua_string(s),
        Value::Number(n) => number(*n),
        Value::List(items) if items.is_empty() => "{}".into(),
        Value::List(items) => {
            let items: Vec<String> = items.iter().map(lua_value).collect();
            format!("{{ {} }}", items.join(", "))
        }
        Value::Map(pairs) if pairs.is_empty() => "{}".into(),
        Value::Map(pairs) => {
            let pairs: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("{} = {}", lua_key(k), lua_value(v)))
                .collect();
            format!("{{ {} }}", pairs.join(", "))
        }
    }
}

/// `s` double-quoted with Lua 5.1's escapes; other control bytes as
/// three-digit `\ddd`, so a digit after one can't join it.
pub fn lua_string(s: &[u8]) -> String {
    let mut out = String::from("\"");
    for chunk in s.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                    let _ = write!(out, "\\{:03}", c as u32);
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\{b:03}");
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rex_has_no_lua_table() {
        assert_eq!(
            show(Some("rex"), "b").unwrap_err(),
            format!(
                "rex is built into {NAME} and has no Lua table; return \"rex\" from provider() \
                 to use it"
            )
        );
    }

    #[test]
    fn an_unknown_name_lists_the_builtins() {
        assert_eq!(
            show(Some("foo"), "b").unwrap_err(),
            "no built-in pane provider named foo (built-ins: rex)"
        );
    }

    #[test]
    fn no_name_lists_one_line_per_builtin() {
        assert_eq!(
            show(None, "b").unwrap(),
            "rex  return \"rex\" from provider() (built in; no Lua table)\n"
        );
    }

    #[test]
    fn a_table_is_written_name_then_capabilities_then_settings() {
        let w = |s: &str| s.as_bytes().to_vec();
        let table = Table {
            name: "mux".into(),
            capabilities: vec![
                (
                    Capability::Focus,
                    vec![w("mux"), w("focus"), w("{terminal}")],
                ),
                (Capability::Open, vec![w("mux"), w("say \"hi\"\\\n\u{1}9")]),
            ],
            settings: vec![
                (
                    Setting::ContextVars,
                    Value::List(vec![Value::String(w("MUX"))]),
                ),
                (
                    Setting::Timeouts,
                    Value::Map(vec![
                        ("open".into(), Value::Number(10.0)),
                        ("focus".into(), Value::Number(0.5)),
                    ]),
                ),
            ],
        };
        assert_eq!(
            lua_file(&table, "B 1"),
            format!(
                "-- {NAME}.builtin.mux() in B 1.\n\
                 -- To change it, copy it into your init.lua and return it from provider().\n\
                 return {{\n  \
                   name = \"mux\",\n  \
                   open = {{ \"mux\", \"say \\\"hi\\\"\\\\\\n\\0019\" }},\n  \
                   focus = {{ \"mux\", \"focus\", \"{{terminal}}\" }},\n  \
                   timeouts = {{ open = 10, focus = 0.5 }},\n  \
                   context_vars = {{ \"MUX\" }},\n\
                 }}\n"
            )
        );
    }

    #[test]
    fn strings_keep_their_bytes() {
        assert_eq!(lua_string(b"\xffa\x7f\xc3\xa9"), "\"\\255a\\127\u{e9}\"");
        assert_eq!(lua_key("odd key"), "[\"odd key\"]");
    }
}
