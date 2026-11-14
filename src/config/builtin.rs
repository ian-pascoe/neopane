//! The built-in pane providers that are tables (ADR 0016): each a static
//! pane provider table, built without Lua, whose every capability runs the
//! daemon's own binary as `{exe} provider builtin NAME CAPABILITY`, which
//! speaks the capability contract for that multiplexer
//! (`crate::providers::builtin`). A config gets one from
//! `<name>.builtin.NAME()`, to return as it is or copy and change; with no
//! config, the built-in default may choose one. Rex's is no table: it is
//! [`Builtin`](super::Builtin).

use super::check::{Capability, Choice, Problems, Provider, Setting, check};
use super::show::{Table, Value};
use super::tree::{Marker, Tree};
use super::wire::{Answer, Called, Ran};

/// A built-in pane provider whose table runs the daemon's own binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTable {
    Tmux,
    Herdr,
    Zellij,
}

impl BuiltinTable {
    /// In proving order: the built-in default proves them in this order.
    pub const ALL: [BuiltinTable; 3] = [
        BuiltinTable::Tmux,
        BuiltinTable::Herdr,
        BuiltinTable::Zellij,
    ];

    pub fn from_name(name: &[u8]) -> Option<BuiltinTable> {
        BuiltinTable::ALL
            .into_iter()
            .find(|b| b.name().as_bytes() == name)
    }

    /// Its name: the word after `builtin` in its commands, and its table's
    /// `name`.
    pub fn name(self) -> &'static str {
        match self {
            BuiltinTable::Tmux => "tmux",
            BuiltinTable::Herdr => "herdr",
            BuiltinTable::Zellij => "zellij",
        }
    }

    /// The capabilities its command speaks, so the ones its table gives.
    pub fn capabilities(self) -> &'static [Capability] {
        match self {
            BuiltinTable::Tmux => &[
                Capability::Find,
                Capability::Close,
                Capability::ReportLayout,
                Capability::OpenBeside,
                Capability::OpenFloat,
                Capability::Events,
                Capability::PlaceBeside,
                Capability::Resize,
                Capability::Focus,
                Capability::FocusDirection,
                Capability::OpenTabPage,
            ],
            BuiltinTable::Herdr => &[
                Capability::Find,
                Capability::Close,
                Capability::ReportLayout,
                Capability::OpenBeside,
                Capability::Events,
                Capability::Focus,
                Capability::FocusDirection,
                Capability::OpenTabPage,
                Capability::PlaceBeside,
                Capability::Resize,
            ],
            BuiltinTable::Zellij => &[
                Capability::Find,
                Capability::Close,
                Capability::ReportLayout,
                Capability::OpenBeside,
                Capability::OpenFloat,
                Capability::Focus,
                Capability::FocusDirection,
                Capability::OpenTabPage,
            ],
        }
    }

    /// Its context variables and find variables: none of tmux's, as `TMUX`
    /// leaks into terminals tmux doesn't run, so its `find` proves a pane
    /// by its tty alone; herdr's server, by its socket, and zellij's
    /// session; and the pane each one's `find` checks.
    pub fn vars(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            BuiltinTable::Tmux => (&[], &[]),
            BuiltinTable::Herdr => (&["HERDR_SOCKET_PATH"], &["HERDR_PANE_ID"]),
            BuiltinTable::Zellij => (
                &["ZELLIJ_SESSION_NAME", "ZELLIJ_SOCKET_DIR"],
                &["ZELLIJ_PANE_ID"],
            ),
        }
    }

    /// The argv its table gives `cap`: `{exe} provider builtin NAME CAP`.
    /// Its words are frozen: a daemon upgraded in place runs a newer
    /// binary at `{exe}`.
    pub fn argv(self, cap: Capability) -> Vec<Vec<u8>> {
        ["{exe}", "provider", "builtin", self.name(), cap.name()]
            .iter()
            .map(|w| w.as_bytes().to_vec())
            .collect()
    }

    /// Its table, as `<name>.builtin.NAME()` returns it and `provider
    /// show NAME` prints it.
    pub fn table(self) -> Table {
        Table {
            name: self.name().into(),
            capabilities: self
                .capabilities()
                .iter()
                .map(|&cap| (cap, self.argv(cap)))
                .collect(),
            settings: self.settings(),
        }
    }

    /// The settings its table sets besides `name`: its variables, if any.
    fn settings(self) -> Vec<(Setting, Value)> {
        let list = |vars: &[&str]| {
            Value::List(
                vars.iter()
                    .map(|v| Value::String(v.as_bytes().to_vec()))
                    .collect(),
            )
        };
        let (context, find) = self.vars();
        let mut settings = Vec::new();
        if !context.is_empty() {
            settings.push((Setting::ContextVars, list(context)));
        }
        if !find.is_empty() {
            settings.push((Setting::FindVars, list(find)));
        }
        settings
    }

    /// Its table, read by the checker as a config that returned it would
    /// be, and the problems the checker found.
    pub fn checked(self) -> (Provider, Problems) {
        let answer = Answer {
            build: String::new(),
            ran: Ran::Returned {
                file: Tree::Table(vec![(
                    Tree::String(super::check::PROVIDER_KEY.into()),
                    Tree::Marker(Marker::Function),
                )]),
                provider: Called::Returned(tree_of_table(&self.table())),
            },
        };
        match check(&answer, super::FILE_NAME) {
            Ok(chosen) => match chosen.choice {
                Choice::Whole(provider) => (provider, chosen.warnings),
                other => unreachable!("a built-in table is chosen whole, not as {other:?}"),
            },
            Err(problems) => panic!("a built-in table is malformed: {problems:?}"),
        }
    }

    /// Its table as the pane provider a config returning it chooses.
    pub fn provider(self) -> Provider {
        self.checked().0
    }
}

/// `table` as the config child would send it.
pub fn tree_of_table(table: &Table) -> Tree {
    let mut pairs = vec![(
        Tree::String(b"name".to_vec()),
        Tree::String(table.name.clone().into_bytes()),
    )];
    for (cap, argv) in &table.capabilities {
        let words = argv.iter().map(|w| Value::String(w.clone())).collect();
        pairs.push((
            Tree::String(cap.name().into()),
            tree_of(&Value::List(words)),
        ));
    }
    for (setting, value) in &table.settings {
        pairs.push((Tree::String(setting.name().into()), tree_of(value)));
    }
    Tree::Table(pairs)
}

fn tree_of(value: &Value) -> Tree {
    match value {
        Value::String(s) => Tree::String(s.clone()),
        Value::Number(n) => Tree::Number(*n),
        Value::List(items) => Tree::Table(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (Tree::Number((i + 1) as f64), tree_of(v)))
                .collect(),
        ),
        Value::Map(pairs) => Tree::Table(
            pairs
                .iter()
                .map(|(k, v)| (Tree::String(k.clone().into_bytes()), tree_of(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_table_passes_the_checker_with_no_problem() {
        for b in BuiltinTable::ALL {
            let (provider, problems) = b.checked();
            assert_eq!(problems, Problems::default(), "{b:?}");
            assert_eq!(provider.name, b.name());
            let given: Vec<Capability> = provider.capabilities.iter().map(|(c, _)| *c).collect();
            let mut want = b.capabilities().to_vec();
            want.sort();
            assert_eq!(given, want, "{b:?}");
            let (context, find) = b.vars();
            assert_eq!(provider.context_vars, context, "{b:?}");
            assert_eq!(provider.find_vars, find, "{b:?}");
        }
    }

    #[test]
    fn herdr_runs_the_binary_for_its_capabilities_and_reaches_by_its_socket_and_pane() {
        let t = BuiltinTable::Herdr.table();
        let shown: Vec<(&str, String)> = t
            .capabilities
            .iter()
            .map(|(c, argv)| {
                let words: Vec<_> = argv.iter().map(|w| String::from_utf8_lossy(w)).collect();
                (c.name(), words.join(" "))
            })
            .collect();
        let want: Vec<(&str, String)> = [
            "find",
            "close",
            "report_layout",
            "open_beside",
            "events",
            "focus",
            "focus_direction",
            "open_tab_page",
            "place_beside",
            "resize",
        ]
        .into_iter()
        .map(|cap| (cap, format!("{{exe}} provider builtin herdr {cap}")))
        .collect();
        assert_eq!(shown, want);
        let list = |v: &str| Value::List(vec![Value::String(v.as_bytes().to_vec())]);
        assert_eq!(
            t.settings,
            [
                (Setting::ContextVars, list("HERDR_SOCKET_PATH")),
                (Setting::FindVars, list("HERDR_PANE_ID")),
            ]
        );
    }

    #[test]
    fn zellij_runs_the_binary_for_every_capability_but_open_place_beside_resize_and_events() {
        let t = BuiltinTable::Zellij.table();
        let shown: Vec<(&str, String)> = t
            .capabilities
            .iter()
            .map(|(c, argv)| {
                let words: Vec<_> = argv.iter().map(|w| String::from_utf8_lossy(w)).collect();
                (c.name(), words.join(" "))
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("find", "{exe} provider builtin zellij find".to_string()),
                ("close", "{exe} provider builtin zellij close".to_string()),
                (
                    "report_layout",
                    "{exe} provider builtin zellij report_layout".to_string()
                ),
                (
                    "open_beside",
                    "{exe} provider builtin zellij open_beside".to_string()
                ),
                (
                    "open_float",
                    "{exe} provider builtin zellij open_float".to_string()
                ),
                ("focus", "{exe} provider builtin zellij focus".to_string()),
                (
                    "focus_direction",
                    "{exe} provider builtin zellij focus_direction".to_string()
                ),
                (
                    "open_tab_page",
                    "{exe} provider builtin zellij open_tab_page".to_string()
                ),
            ]
        );
        let string = |s: &str| Value::String(s.as_bytes().to_vec());
        assert_eq!(
            t.settings,
            [
                (
                    Setting::ContextVars,
                    Value::List(vec![
                        string("ZELLIJ_SESSION_NAME"),
                        string("ZELLIJ_SOCKET_DIR")
                    ])
                ),
                (
                    Setting::FindVars,
                    Value::List(vec![string("ZELLIJ_PANE_ID")])
                ),
            ]
        );
    }

    #[test]
    fn tmux_runs_the_binary_for_every_capability_but_open_and_events() {
        let t = BuiltinTable::Tmux.table();
        let shown: Vec<(&str, String)> = t
            .capabilities
            .iter()
            .map(|(c, argv)| {
                let words: Vec<_> = argv.iter().map(|w| String::from_utf8_lossy(w)).collect();
                (c.name(), words.join(" "))
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("find", "{exe} provider builtin tmux find".to_string()),
                ("close", "{exe} provider builtin tmux close".to_string()),
                (
                    "report_layout",
                    "{exe} provider builtin tmux report_layout".to_string()
                ),
                (
                    "open_beside",
                    "{exe} provider builtin tmux open_beside".to_string()
                ),
                (
                    "open_float",
                    "{exe} provider builtin tmux open_float".to_string()
                ),
                ("events", "{exe} provider builtin tmux events".to_string()),
                (
                    "place_beside",
                    "{exe} provider builtin tmux place_beside".to_string()
                ),
                ("resize", "{exe} provider builtin tmux resize".to_string()),
                ("focus", "{exe} provider builtin tmux focus".to_string()),
                (
                    "focus_direction",
                    "{exe} provider builtin tmux focus_direction".to_string()
                ),
                (
                    "open_tab_page",
                    "{exe} provider builtin tmux open_tab_page".to_string()
                ),
            ]
        );
        assert!(t.settings.is_empty());
    }
}
