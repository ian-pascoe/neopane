//! What a capability's command is given (#93 decisions 13–15 and 22): its
//! `<NAME>_*` variables, the same values as whole-word placeholders, and the
//! environment it runs in.

use crate::daemon::provider::{Ask, Capability, Direction};
use crate::name;

/// Every variable the daemon sets for some capability, besides the `ENV_*`
/// ones: the one list both what an act is given and what is stripped from
/// the choosing attach's environment come from (#93 decisions 14 and 22),
/// so a variable can't be set without being stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Var {
    Capability,
    Protocol,
    Attach,
    AttachJson,
    Title,
    Pane,
    Terminal,
    Origin,
    Direction,
    Size,
    Ratio,
    Cols,
    Rows,
    X,
    Y,
    OriginCols,
    OriginRows,
    AreaBounds,
    Count,
    Pid,
    Tty,
}

impl Var {
    pub(super) const ALL: [Var; 21] = [
        Var::Capability,
        Var::Protocol,
        Var::Attach,
        Var::AttachJson,
        Var::Title,
        Var::Pane,
        Var::Terminal,
        Var::Origin,
        Var::Direction,
        Var::Size,
        Var::Ratio,
        Var::Cols,
        Var::Rows,
        Var::X,
        Var::Y,
        Var::OriginCols,
        Var::OriginRows,
        Var::AreaBounds,
        Var::Count,
        Var::Pid,
        Var::Tty,
    ];

    /// Its placeholder name: the variable's name after `<NAME>_`, in lower
    /// case.
    pub(super) fn name(self) -> &'static str {
        match self {
            Var::Capability => "capability",
            Var::Protocol => "protocol",
            Var::Attach => "attach",
            Var::AttachJson => "attach_json",
            Var::Title => "title",
            Var::Pane => "pane",
            Var::Terminal => "terminal",
            Var::Origin => "origin",
            Var::Direction => "direction",
            Var::Size => "size",
            Var::Ratio => "ratio",
            Var::Cols => "cols",
            Var::Rows => "rows",
            Var::X => "x",
            Var::Y => "y",
            Var::OriginCols => "origin_cols",
            Var::OriginRows => "origin_rows",
            Var::AreaBounds => "area_bounds",
            Var::Count => "count",
            Var::Pid => "pid",
            Var::Tty => "tty",
        }
    }

    /// The variable: `<NAME>_` and its name in upper case.
    fn var(self) -> String {
        format!("{}_{}", name::upper(), self.name().to_ascii_uppercase())
    }
}

/// One act's inputs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Inputs {
    /// `{attach}`'s words, for the opens.
    attach: Option<Vec<String>>,
    /// Each input by its placeholder name; None for one this act doesn't
    /// have, such as `area_bounds` outside a live layout.
    vars: Vec<(Var, Option<String>)>,
}

impl Inputs {
    /// The inputs of a run of `cap` with none of its own, such as
    /// `report_layout` and `events`, under protocol number `protocol`.
    pub(crate) fn bare(cap: Capability, protocol: i64) -> Inputs {
        Inputs {
            attach: None,
            vars: vec![
                (Var::Capability, Some(cap.name().to_string())),
                (Var::Protocol, Some(protocol.to_string())),
            ],
        }
    }

    /// The inputs of `ask`, run under protocol number `protocol`.
    pub(crate) fn of(ask: &Ask, protocol: i64) -> Inputs {
        let Inputs { mut vars, .. } = Inputs::bare(ask.capability(), protocol);
        let mut attach = None;
        let mut opened = |words: &[String], title: &str, pane: String| {
            attach = Some(words.to_vec());
            [
                (Var::Attach, Some(quoted(words))),
                (Var::AttachJson, Some(json(words))),
                (Var::Title, Some(title.to_string())),
                (Var::Pane, Some(pane)),
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
                vars.extend([(Var::Cols, n(*cols)), (Var::Rows, n(*rows))]);
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
                    (Var::Origin, Some(origin.to_string())),
                    (Var::Direction, Some(direction(*dir).into())),
                    (Var::Size, n(*size)),
                    (Var::Ratio, Some(fraction(*ratio))),
                    (Var::Cols, n(*cols)),
                    (Var::Rows, n(*rows)),
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
                    (Var::Origin, Some(origin.to_string())),
                    (Var::X, Some(x.to_string())),
                    (Var::Y, Some(y.to_string())),
                    (Var::Cols, n(*cols)),
                    (Var::Rows, n(*rows)),
                    (Var::OriginCols, n(*origin_cols)),
                    (Var::OriginRows, n(*origin_rows)),
                    (Var::AreaBounds, bounds),
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
                    (Var::Origin, Some(origin.to_string())),
                    (Var::Cols, n(*cols)),
                    (Var::Rows, n(*rows)),
                ]);
            }
            Ask::Close { terminal } | Ask::Focus { terminal } => {
                vars.push((Var::Terminal, Some(terminal.to_string())));
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
                (Var::Terminal, Some(terminal.to_string())),
                (Var::Origin, Some(origin.to_string())),
                (Var::Direction, Some(direction(*dir).into())),
                (Var::Size, n(*size)),
                (Var::Ratio, Some(fraction(*ratio))),
            ]),
            Ask::FocusDirection { dir, count } => vars.extend([
                (Var::Direction, Some(direction(*dir).into())),
                (Var::Count, n(*count)),
            ]),
            // `find`'s are #127's
            Ask::Find { .. } | Ask::ReportLayout => {}
        }
        Inputs { attach, vars }
    }

    /// `written`, an argv as the config wrote it, with its whole-word placeholders replaced: `{attach}` by
    /// its words, `{{x}}` by `{x}`, any other by its value, or one empty
    /// word if this act has none. A word that only holds a placeholder
    /// passes as written, and other braces are never looked at.
    pub(crate) fn argv(&self, written: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut argv = Vec::with_capacity(written.len());
        for word in written {
            if let Some(name) = crate::placeholder::whole_placeholder(word) {
                match (name, &self.attach) {
                    ("attach", Some(words)) => {
                        argv.extend(words.iter().map(|w| w.as_bytes().to_vec()))
                    }
                    _ => argv.push(self.value(name).unwrap_or("").as_bytes().to_vec()),
                }
            } else if crate::placeholder::is_escaped(word) {
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
            .find(|(v, _)| v.name() == name)
            .and_then(|(_, value)| value.as_deref())
    }

    /// The command's environment: `base`, the choosing attach's already
    /// stripped by `commands_env`, plus this act's variables.
    pub(crate) fn env(&self, base: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut env = base.to_vec();
        for (var, value) in &self.vars {
            if let Some(value) = value {
                env.push((var.var().into_bytes(), value.as_bytes().to_vec()));
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
        rest.starts_with(b"ENV_")
            || Var::ALL
                .iter()
                .any(|v| v.name().to_ascii_uppercase().as_bytes() == rest)
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
    fn every_variable_is_listed_once_and_every_placeholder_is_one() {
        // in order, the last last: a variable left out of `ALL` shifts them
        for (i, v) in Var::ALL.iter().enumerate() {
            assert_eq!(i, *v as usize, "{v:?}");
        }
        assert_eq!(Var::ALL.len(), Var::Tty as usize + 1);
        for cap in crate::config::check::Capability::ALL {
            for p in cap.placeholders() {
                assert!(Var::ALL.iter().any(|v| v.name() == *p), "{p}");
            }
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
