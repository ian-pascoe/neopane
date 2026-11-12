use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mlua::{Lua, Table, Value};

use super::*;
use crate::config::check::{ATTACH_TIMEOUT, Builtin, Capability, PROVIDER_KEY, PairKey, Setting};
use crate::config::wire::{Attach, Request};
use crate::config::{config_var, default_file};

/// A fresh directory for one test, removed when dropped, even on failure.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(test: &str) -> Temp {
    let dir = std::env::temp_dir().join(format!("{NAME}-types-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Temp(dir)
}

#[test]
fn the_default_file_follows_the_daemons_order() {
    let config = config_var();
    let cwd = Path::new("/work");
    let with = |vars: &[(&str, &str)]| {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        default_file(
            |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            cwd,
        )
    };
    let home = ("HOME", "/home/u");
    let in_home = PathBuf::from(format!("/home/u/.config/{NAME}/init.lua"));
    assert_eq!(with(&[home]), Some(in_home.clone()));
    assert_eq!(
        with(&[home, ("XDG_CONFIG_HOME", "/xdg")]),
        Some(PathBuf::from(format!("/xdg/{NAME}/init.lua")))
    );
    // empty or relative: unset
    assert_eq!(
        with(&[home, ("XDG_CONFIG_HOME", "")]),
        Some(in_home.clone())
    );
    assert_eq!(
        with(&[home, ("XDG_CONFIG_HOME", "rel")]),
        Some(in_home.clone())
    );
    assert_eq!(
        with(&[home, (&config, "/c/my.lua")]),
        Some(PathBuf::from("/c/my.lua"))
    );
    assert_eq!(
        with(&[home, (&config, "c/my.lua")]),
        Some(PathBuf::from("/work/c/my.lua"))
    );
    assert_eq!(with(&[home, (&config, "")]), Some(in_home));
    assert_eq!(with(&[]), None);
}

#[test]
fn types_writes_both_files_then_keeps_luarc() {
    let tmp = temp("write");
    let home = &tmp.0;
    let file = home.join("cfg/init.lua");
    let report = write(&file, "B 1", Some(home));
    let url = format!("{}/blob/main/docs/config.md", repo_url());
    assert_eq!(
        report,
        Report {
            out: format!(
                "wrote ~/cfg/types/{NAME}.lua\nwrote ~/cfg/.luarc.json\nno init.lua yet: see \
                 {url}\n"
            ),
            error: None,
        }
    );
    let meta_file = home.join(format!("cfg/types/{NAME}.lua"));
    let written = std::fs::read_to_string(&meta_file).unwrap();
    assert!(written.starts_with("---@meta\n-- B 1\n"), "{written}");
    let luarc = home.join("cfg/.luarc.json");
    assert_eq!(std::fs::read_to_string(&luarc).unwrap(), LUARC);

    // the user's own .luarc.json stays; the types are rewritten
    std::fs::write(&luarc, "{}").unwrap();
    std::fs::write(&file, "return {}").unwrap();
    let report = write(&file, "B 2", None);
    let shown = |p: &Path| p.display().to_string();
    assert_eq!(
        report.out,
        format!(
            "wrote {}\nkept {} (already there)\n",
            shown(&meta_file),
            shown(&luarc)
        )
    );
    assert_eq!(std::fs::read_to_string(&luarc).unwrap(), "{}");
    assert!(
        std::fs::read_to_string(&meta_file)
            .unwrap()
            .starts_with("---@meta\n-- B 2\n")
    );
}

#[test]
fn a_write_error_stops_it() {
    let tmp = temp("error");
    let dir = &tmp.0;
    // types can't be a directory: a file is in the way
    std::fs::write(dir.join("types"), "").unwrap();
    let report = write(&dir.join("init.lua"), "B", None);
    assert_eq!(report.out, "");
    let error = report.error.unwrap();
    assert!(
        error.starts_with(&format!("can't write {}", dir.join("types").display())),
        "{error}"
    );
}

#[test]
fn luarc_sets_the_schema_and_lua_51_only() {
    assert!(LUARC.contains("\"$schema\": \"https://"));
    assert!(LUARC.contains("\"runtime.version\": \"Lua 5.1\""));
    assert!(!LUARC.contains("workspace"));
}

/// Each `---@class` or `---@alias` in the meta file, with its fields; the
/// global's fields include its functions.
fn classes(meta: &str) -> Vec<(String, BTreeSet<String>)> {
    let mut out: Vec<(String, BTreeSet<String>)> = Vec::new();
    for line in meta.lines() {
        if let Some(rest) = line
            .strip_prefix("---@class ")
            .or_else(|| line.strip_prefix("---@alias "))
        {
            let name = rest.split_whitespace().next().unwrap();
            out.push((name.to_string(), BTreeSet::new()));
        } else if let Some(rest) = line.strip_prefix("---@field ") {
            let field = rest
                .split_whitespace()
                .next()
                .unwrap()
                .trim_end_matches('?');
            out.last_mut().unwrap().1.insert(field.to_string());
        } else if let Some(rest) = line.strip_prefix(&format!("function {NAME}.")) {
            let f = rest.split('(').next().unwrap();
            let global = out.iter_mut().find(|(c, _)| c == NAME).unwrap();
            global.1.insert(f.to_string());
        }
    }
    out
}

fn fields(meta: &str, class: &str) -> BTreeSet<String> {
    let class = if class.is_empty() {
        NAME.to_string()
    } else {
        format!("{NAME}.{class}")
    };
    classes(meta)
        .into_iter()
        .find(|(c, _)| *c == class)
        .unwrap_or_else(|| panic!("no class {class}"))
        .1
}

fn set<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    names.into_iter().map(String::from).collect()
}

fn keys(t: &Table) -> BTreeSet<String> {
    t.pairs::<String, Value>().map(|p| p.unwrap().0).collect()
}

#[test]
fn the_meta_file_has_the_decided_classes() {
    let meta = meta("B");
    let names: Vec<String> = classes(&meta).into_iter().map(|(c, _)| c).collect();
    let mut want = vec![NAME.to_string()];
    for class in [
        "Attach",
        "Provider",
        "Timeouts",
        "Pair",
        "Choice",
        "Config",
        "RunOptions",
        "RunResult",
    ] {
        want.push(format!("{NAME}.{class}"));
    }
    want.sort();
    let mut names = names;
    names.sort();
    assert_eq!(names, want);
    assert!(meta.starts_with("---@meta\n-- B\n"));
    assert!(!meta.contains("__"), "a token left in:\n{meta}");
}

/// The meta file lists every key the checker reads, and no other.
#[test]
fn the_meta_file_keeps_in_step_with_the_checker() {
    let meta = meta("B");
    let mut provider = set(Capability::ALL.map(Capability::name));
    provider.extend(set(Setting::ALL.map(Setting::name)));
    assert_eq!(fields(&meta, "Provider"), provider);
    let timed = Capability::ALL
        .into_iter()
        .filter(|c| c.default_timeout().is_some())
        .map(Capability::name);
    assert_eq!(fields(&meta, "Timeouts"), set(timed));
    assert_eq!(fields(&meta, "Pair"), set(PairKey::ALL.map(PairKey::name)));
    assert_eq!(fields(&meta, "Config"), set([PROVIDER_KEY]));
    // the built-in names a choice can be
    let alias = meta
        .lines()
        .find_map(|l| l.strip_prefix(&format!("---@alias {NAME}.Choice ")))
        .unwrap();
    let names = alias
        .split('|')
        .filter_map(|t| t.strip_prefix('"')?.strip_suffix('"'));
    assert_eq!(set(names), set(Builtin::ALL.map(Builtin::name)));
}

/// The text after `---@field <field>`'s type in `class`.
fn field_doc<'m>(meta: &'m str, class: &str, field: &str) -> &'m str {
    let header = format!("---@class {NAME}.{class}");
    let at = meta.find(&header).unwrap();
    meta[at..]
        .lines()
        .find_map(|l| {
            let rest = l.strip_prefix("---@field ")?;
            let (name, rest) = rest.split_once(' ')?;
            (name.trim_end_matches('?') == field).then(|| rest.split_once(' ').unwrap().1)
        })
        .unwrap_or_else(|| panic!("no {class}.{field}"))
}

/// The defaults the meta file states are the checker's.
#[test]
fn the_meta_files_defaults_are_the_checkers() {
    let meta = meta("B");
    for cap in Capability::ALL {
        let Some(default) = cap.default_timeout() else {
            continue;
        };
        let doc = field_doc(&meta, "Timeouts", cap.name());
        let want = format!("Default {}", default.as_secs());
        assert!(doc.starts_with(&want), "{}: {doc}", cap.name());
    }
    let doc = field_doc(&meta, "Provider", Setting::AttachTimeout.name());
    let want = format!("default {},", ATTACH_TIMEOUT.as_secs());
    assert!(doc.contains(&want), "{doc}");
}

/// The meta file lists what the child gives the config: the global's
/// fields and `attach`'s.
#[test]
fn the_meta_file_keeps_in_step_with_the_api() {
    let meta = meta("B");
    let lua = Lua::new();
    let request = Request {
        file: b"/c/init.lua".to_vec(),
        version: "v".into(),
        attach: Attach::default(),
        time: Duration::from_secs(3),
    };
    crate::config::api::install(&lua, &request, Instant::now()).unwrap();
    let api: Table = lua.globals().get(NAME).unwrap();
    assert_eq!(fields(&meta, ""), keys(&api));
    let mut attach = keys(&crate::config::child::attach_table(&lua, &request.attach).unwrap());
    // nil without a hint
    attach.insert("provider".into());
    assert_eq!(fields(&meta, "Attach"), attach);
}
