//! `provider types [--config FILE]` (#93 decision 32, #94 decision 35):
//! writes the config's `---@meta` file, and a `.luarc.json` if there is none,
//! so lua-language-server knows the API when the config's directory is the
//! editor's workspace folder.

use std::path::Path;

use crate::name::{NAME, repo_url, upper};

/// The `---@meta` file, with its name, build and `<NAME>` filled in.
pub fn meta(build: &str) -> String {
    TEMPLATE
        .replace("__NAME__", NAME)
        .replace("__UPPER__", &upper())
        .replace("__BUILD__", build)
}

const TEMPLATE: &str = include_str!("types.lua");

/// `.luarc.json`: the schema and the Lua version, never `workspace.library`,
/// since the types file is inside the workspace.
pub const LUARC: &str = "{\n  \
    \"$schema\": \"https://raw.githubusercontent.com/LuaLS/vscode-lua/master/setting/schema.json\",\n  \
    \"runtime.version\": \"Lua 5.1\"\n}\n";

/// What `types` did: its stdout lines, then the error that stopped it, if
/// any (exit 1).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub out: String,
    pub error: Option<String>,
}

/// Writes the types for the config `file` (`--config`, else
/// [`super::default_file`]), into its directory, which it creates. `home` shortens
/// paths as `~`.
pub fn write(file: &Path, build: &str, home: Option<&Path>) -> Report {
    let mut report = Report::default();
    let home = home.filter(|h| h.is_absolute());
    let shown = |p: &Path| match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => p.display().to_string(),
    };
    let dir = file.parent().unwrap_or(Path::new("/"));
    let types = dir.join("types");
    let meta_file = types.join(format!("{NAME}.lua"));
    if let Err(e) =
        std::fs::create_dir_all(&types).and_then(|()| std::fs::write(&meta_file, meta(build)))
    {
        report.error = Some(format!("can't write {}: {e}", shown(&meta_file)));
        return report;
    }
    report.out += &format!("wrote {}\n", shown(&meta_file));
    let luarc = dir.join(".luarc.json");
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&luarc)
        .and_then(|mut f| std::io::Write::write_all(&mut f, LUARC.as_bytes()));
    match created {
        Ok(()) => report.out += &format!("wrote {}\n", shown(&luarc)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            report.out += &format!("kept {} (already there)\n", shown(&luarc))
        }
        Err(e) => {
            report.error = Some(format!("can't write {}: {e}", shown(&luarc)));
            return report;
        }
    }
    if file.symlink_metadata().is_err() {
        // `no init.lua yet` (#94 decision 35), unless --config or
        // $<NAME>_CONFIG names a file called otherwise
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| super::FILE_NAME.into());
        report.out += &format!(
            "no {name} yet: see {}/blob/main/docs/config.md\n",
            repo_url()
        );
    }
    report
}

#[cfg(test)]
mod tests;
