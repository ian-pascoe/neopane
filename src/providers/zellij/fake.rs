//! A fake zellij for tests: a shell script in a directory of its own that
//! logs each action it is asked and answers it as the test says.

use std::path::PathBuf;

use super::session::Session;

/// A fake zellij: it logs each action's name, one a line, and the whole
/// action, `ZELLIJ_PANE_ID=N` before it if set, and answers it as
/// `answers` say, a shell snippet per action. Removed as it drops.
pub(super) struct Fake {
    dir: PathBuf,
}

impl Fake {
    pub(super) fn new(test: &str, answers: &[(&str, &str)]) -> Fake {
        let dir = std::env::temp_dir().join(format!("zj-fake-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (log, calls) = (dir.join("log"), dir.join("calls"));
        let mut script = format!(
            "#!/bin/sh\nshift 3\necho \"$1\" >> '{}'\n\
             echo \"${{ZELLIJ_PANE_ID:+ZELLIJ_PANE_ID=$ZELLIJ_PANE_ID }}$*\" >> '{}'\n\
             case \"$1\" in\n",
            log.display(),
            calls.display()
        );
        for (action, answer) in answers {
            script.push_str(&format!("{action}) {answer} ;;\n"));
        }
        script.push_str("esac\n");
        let program = dir.join("zellij");
        std::fs::write(&program, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir }
    }

    pub(super) fn session(&self) -> Session {
        Session::faked(&self.dir.join("zellij"), "work")
    }

    /// The actions it was asked, in order.
    pub(super) fn asked(&self) -> Vec<String> {
        self.read("log")
    }

    /// The whole of each action it was asked, its words joined by spaces,
    /// in order.
    pub(super) fn calls(&self) -> Vec<String> {
        self.read("calls")
    }

    fn read(&self, file: &str) -> Vec<String> {
        let log = std::fs::read_to_string(self.dir.join(file)).unwrap_or_default();
        log.lines().map(String::from).collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
