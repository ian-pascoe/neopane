//! What a capability's command is given (#93 decisions 13–15 and 22): its
//! `<NAME>_*` variables, the same values as whole-word placeholders, and the
//! environment it runs in.

use crate::daemon::provider::{Ask, Direction};
use crate::name;

/// Every variable the daemon sets for some capability, without the
/// `<NAME>_` prefix, besides the `ENV_*` ones: none of them is passed on
/// from the choosing attach (#93 decision 22).
pub(super) const PER_ACT: &[&str] = &[
    "CAPABILITY",
    "PROTOCOL",
    "ATTACH",
    "ATTACH_JSON",
    "TITLE",
    "PANE",
    "TERMINAL",
    "ORIGIN",
    "DIRECTION",
    "SIZE",
    "RATIO",
    "COLS",
    "ROWS",
    "X",
    "Y",
    "ORIGIN_COLS",
    "ORIGIN_ROWS",
    "AREA_BOUNDS",
    "COUNT",
    "PID",
    "TTY",
];

/// One act's inputs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Inputs {
    /// `{attach}`'s words, for the opens.
    attach: Option<Vec<String>>,
    /// Each input by its placeholder name; None for one this act doesn't
    /// have, such as `area_bounds` outside a live layout.
    vars: Vec<(&'static str, Option<String>)>,
}

impl Inputs {
    /// The inputs of `ask`, run under protocol number `protocol`.
    pub(crate) fn of(ask: &Ask, protocol: i64) -> Inputs {
        let cap = ask.capability();
        let mut vars: Vec<(&'static str, Option<String>)> = vec![
            ("capability", Some(cap.name().to_string())),
            ("protocol", Some(protocol.to_string())),
        ];
        let mut attach = None;
        let mut opened = |words: &[String], title: &str, pane: String| {
            attach = Some(words.to_vec());
            [
                ("attach", Some(quoted(words))),
                ("attach_json", Some(json(words))),
                ("title", Some(title.to_string())),
                ("pane", Some(pane)),
            ]
        };
        let n = |v: u32| Some(v.to_string());
        match ask {
            Ask::Open {
                pane,
                attach,
                title,
                cols,
                rows,
            } => {
                vars.extend(opened(attach, title, pane.to_string()));
                vars.extend([("cols", n(*cols)), ("rows", n(*rows))]);
            }
            Ask::OpenBeside {
                pane,
                attach,
                title,
                origin,
                dir,
                size,
                ratio,
                cols,
                rows,
            } => {
                vars.extend(opened(attach, title, pane.to_string()));
                vars.extend([
                    ("origin", Some(origin.to_string())),
                    ("direction", Some(direction(*dir).into())),
                    ("size", n(*size)),
                    ("ratio", Some(fraction(*ratio))),
                    ("cols", n(*cols)),
                    ("rows", n(*rows)),
                ]);
            }
            Ask::OpenFloat {
                pane,
                attach,
                title,
                origin,
                x,
                y,
                cols,
                rows,
                origin_cols,
                origin_rows,
                area_bounds,
            } => {
                vars.extend(opened(attach, title, pane.to_string()));
                let bounds = area_bounds
                    .map(|b| b.iter().map(|&f| fraction(f)).collect::<Vec<_>>().join(","));
                vars.extend([
                    ("origin", Some(origin.to_string())),
                    ("x", Some(x.to_string())),
                    ("y", Some(y.to_string())),
                    ("cols", n(*cols)),
                    ("rows", n(*rows)),
                    ("origin_cols", n(*origin_cols)),
                    ("origin_rows", n(*origin_rows)),
                    ("area_bounds", bounds),
                ]);
            }
            Ask::OpenTabPage {
                pane,
                attach,
                title,
                origin,
                cols,
                rows,
            } => {
                vars.extend(opened(attach, title, pane.to_string()));
                vars.extend([
                    ("origin", Some(origin.to_string())),
                    ("cols", n(*cols)),
                    ("rows", n(*rows)),
                ]);
            }
            Ask::Close { terminal } | Ask::Focus { terminal } => {
                vars.push(("terminal", Some(terminal.to_string())));
            }
            Ask::PlaceBeside {
                terminal,
                origin,
                dir,
                size,
                ratio,
            }
            | Ask::Resize {
                terminal,
                origin,
                dir,
                size,
                ratio,
            } => vars.extend([
                ("terminal", Some(terminal.to_string())),
                ("origin", Some(origin.to_string())),
                ("direction", Some(direction(*dir).into())),
                ("size", n(*size)),
                ("ratio", Some(fraction(*ratio))),
            ]),
            Ask::FocusDirection { dir, count } => vars.extend([
                ("direction", Some(direction(*dir).into())),
                ("count", n(*count)),
            ]),
            // `find`'s and `report_layout`'s are #126's and #127's
            Ask::Find { .. } | Ask::ReportLayout => {}
        }
        Inputs { attach, vars }
    }

    /// `template` with its whole-word placeholders replaced: `{attach}` by
    /// its words, `{{x}}` by `{x}`, any other by its value, or one empty
    /// word if this act has none. A word that only holds a placeholder
    /// passes as written, and other braces are never looked at.
    pub(crate) fn argv(&self, template: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut argv = Vec::with_capacity(template.len());
        for word in template {
            if let Some(name) = crate::config::check::whole_placeholder(word) {
                match (name, &self.attach) {
                    ("attach", Some(words)) => {
                        argv.extend(words.iter().map(|w| w.as_bytes().to_vec()))
                    }
                    _ => argv.push(self.value(name).unwrap_or("").as_bytes().to_vec()),
                }
            } else if crate::config::check::is_escaped(word) {
                argv.push(word[1..word.len() - 1].to_vec());
            } else {
                argv.push(word.clone());
            }
        }
        argv
    }

    /// The value of the input placeholder `name` names, if the act has it.
    fn value(&self, name: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// The command's environment: `base`, the choosing attach's already
    /// stripped by `commands_env`, plus this act's variables.
    pub(crate) fn env(&self, base: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let prefix = name::upper();
        let mut env = base.to_vec();
        for (n, v) in &self.vars {
            if let Some(v) = v {
                let var = format!("{prefix}_{}", n.to_ascii_uppercase());
                env.push((var.into_bytes(), v.as_bytes().to_vec()));
            }
        }
        env
    }
}

/// The environment every command starts from: the choosing attach's, less
/// `<NAME>` and every variable the daemon sets for any capability, so none
/// leaks from the shell that ran the attach. The rest stays, such as
/// `<NAME>_CONFIG` (#93 decision 22).
pub(crate) fn commands_env(attach_env: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let upper = name::upper();
    let ours = |k: &[u8]| {
        if k == upper.as_bytes() {
            return true;
        }
        let Some(rest) = k
            .strip_prefix(upper.as_bytes())
            .and_then(|r| r.strip_prefix(b"_"))
        else {
            return false;
        };
        rest.starts_with(b"ENV_") || PER_ACT.iter().any(|v| v.as_bytes() == rest)
    };
    attach_env
        .iter()
        .filter(|(k, _)| !ours(k))
        .cloned()
        .collect()
}

fn direction(d: Direction) -> &'static str {
    match d {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

/// A fraction as `<NAME>_RATIO` has it: at most 4 decimals, a `.`, no
/// exponent, no trailing zeros: `0.5`, `0.3333`.
fn fraction(f: f64) -> String {
    let s = format!("{f:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.into()
    }
}

/// The attach words as one string for `sh`: each bare if it holds only
/// `[A-Za-z0-9_@%+=:,./-]`, else single-quoted (#93 decision 15).
fn quoted(words: &[String]) -> String {
    let bare = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    };
    words
        .iter()
        .map(|w| {
            if bare(w) {
                w.clone()
            } else {
                format!("'{}'", w.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The attach words as a compact JSON array.
fn json(words: &[String]) -> String {
    serde_json::Value::from(words.to_vec()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::PaneId;
    use crate::daemon::provider::TerminalId;

    fn b(words: &[&str]) -> Vec<Vec<u8>> {
        words.iter().map(|w| w.as_bytes().to_vec()).collect()
    }

    fn open_float(area_bounds: Option<[f64; 4]>) -> Ask {
        Ask::OpenFloat {
            pane: PaneId::parse("12-k3j9x2qpa8").unwrap(),
            attach: vec!["/bin/np".into(), "attach".into(), "it's here".into()],
            title: "np-k3j9x2qpa8".into(),
            origin: TerminalId::from("%1"),
            x: -2,
            y: 3,
            cols: 40,
            rows: 10,
            origin_cols: 80,
            origin_rows: 24,
            area_bounds,
        }
    }

    #[test]
    fn fractions_have_at_most_four_decimals_and_no_trailing_zeros() {
        assert_eq!("0.5", fraction(0.5));
        assert_eq!("0.3333", fraction(1.0 / 3.0));
        assert_eq!("1", fraction(1.0));
        assert_eq!("0", fraction(0.0));
        assert_eq!("0.0001", fraction(0.0001));
    }

    #[test]
    fn the_attach_string_quotes_only_words_that_need_it() {
        let words: Vec<String> = [
            "/bin/np",
            "attach",
            "a=b,c:d@e%f+g",
            "it's",
            "",
            "two words",
        ]
        .iter()
        .map(|w| w.to_string())
        .collect();
        assert_eq!(
            r"/bin/np attach a=b,c:d@e%f+g 'it'\''s' '' 'two words'",
            quoted(&words)
        );
        assert_eq!(
            r#"["a","it's \"x\""]"#,
            json(&["a".into(), "it's \"x\"".into()])
        );
    }

    #[test]
    fn placeholders_are_whole_words_and_attach_becomes_several() {
        let i = Inputs::of(&open_float(None), 1);
        let argv = i.argv(&b(&[
            "run",
            "{attach}",
            "{x}",
            "{y}",
            "{{x}}",
            "{{nope}}",
            "--at={x}",
            "#{pane_id}",
            "{area_bounds}",
            "{capability}",
            "{protocol}",
            "{origin}",
        ]));
        assert_eq!(
            b(&[
                "run",
                "/bin/np",
                "attach",
                "it's here",
                "-2",
                "3",
                "{x}",
                "{nope}",
                "--at={x}",
                "#{pane_id}",
                "",
                "open_float",
                "1",
                "%1",
            ]),
            argv
        );
    }

    #[test]
    fn the_variables_mirror_the_placeholders_and_skip_what_the_act_lacks() {
        let up = name::upper();
        let var = |env: &[(Vec<u8>, Vec<u8>)], k: &str| {
            let k = format!("{up}_{k}");
            env.iter()
                .find(|(n, _)| n == k.as_bytes())
                .map(|(_, v)| String::from_utf8(v.clone()).unwrap())
        };
        let env = Inputs::of(&open_float(None), 2).env(&[]);
        assert_eq!(Some("open_float".into()), var(&env, "CAPABILITY"));
        assert_eq!(Some("2".into()), var(&env, "PROTOCOL"));
        assert_eq!(
            Some(r"/bin/np attach 'it'\''s here'".into()),
            var(&env, "ATTACH")
        );
        assert_eq!(
            Some(r#"["/bin/np","attach","it's here"]"#.into()),
            var(&env, "ATTACH_JSON")
        );
        assert_eq!(Some("12-k3j9x2qpa8".into()), var(&env, "PANE"));
        assert_eq!(Some("-2".into()), var(&env, "X"));
        assert_eq!(Some("80".into()), var(&env, "ORIGIN_COLS"));
        assert_eq!(None, var(&env, "AREA_BOUNDS"));

        let env = Inputs::of(&open_float(Some([0.25, 0.0, 0.5, 1.0 / 3.0])), 1).env(&[]);
        assert_eq!(Some("0.25,0,0.5,0.3333".into()), var(&env, "AREA_BOUNDS"));

        let env = Inputs::of(
            &Ask::FocusDirection {
                dir: Direction::Up,
                count: 3,
            },
            1,
        )
        .env(&[]);
        assert_eq!(Some("up".into()), var(&env, "DIRECTION"));
        assert_eq!(Some("3".into()), var(&env, "COUNT"));
        assert_eq!(None, var(&env, "ATTACH"));
    }

    #[test]
    fn the_commands_environment_drops_the_name_and_every_per_act_variable() {
        let up = name::upper();
        let pair = |k: String, v: &str| (k.into_bytes(), v.as_bytes().to_vec());
        let env = vec![
            pair("HOME".into(), "/home/u"),
            pair(up.clone(), "1"),
            pair(format!("{up}_TITLE"), "stale"),
            pair(format!("{up}_ENV_TMUX_PANE"), "%3"),
            pair(format!("{up}_ATTACH_JSON"), "[]"),
            pair(format!("{up}_CONFIG"), "/cfg/init.lua"),
            pair(format!("{up}_PROVIDER"), "rex"),
            pair(format!("{up}X"), "kept"),
        ];
        let kept: Vec<String> = commands_env(&env)
            .into_iter()
            .map(|(k, _)| String::from_utf8(k).unwrap())
            .collect();
        assert_eq!(
            vec![
                "HOME".to_string(),
                format!("{up}_CONFIG"),
                format!("{up}_PROVIDER"),
                format!("{up}X"),
            ],
            kept
        );
    }
}
