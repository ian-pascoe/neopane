//! Gives the crate its commit, for the hello's `build` string
//! (`src/attach/mod.rs`): `BUILD_GIT_COMMIT` (short hash, `-dirty` when
//! tracked files differ from it) and `BUILD_GIT_DATE` (its commit date).
//! Built from outside git, neither is set.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn main() {
    // the sources decide `-dirty`; git's own files, where commits land
    for path in ["build.rs", "Cargo.toml", "Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let Some(git_dir) = git(&["rev-parse", "--git-dir"]) else {
        return;
    };
    let common = git(&["rev-parse", "--git-common-dir"]).unwrap_or(git_dir.clone());
    let mut watched = vec![
        Path::new(&git_dir).join("HEAD"),
        Path::new(&git_dir).join("index"),
        Path::new(&common).join("packed-refs"),
    ];
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(Path::new(&common).join(head_ref));
    }
    for path in watched.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let (Some(commit), Some(date)) = (
        git(&["rev-parse", "--short", "HEAD"]),
        git(&["log", "-1", "--format=%cs"]),
    ) else {
        return;
    };
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=BUILD_GIT_COMMIT={commit}{suffix}");
    println!("cargo:rustc-env=BUILD_GIT_DATE={date}");
}
