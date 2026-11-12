//! What the daemon knows of the file at its exe path: enough to tell that
//! it changed, as an install in place changes it (#91 decision 24).

use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The file at a path, as `stat` has it: its device, inode and mtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExeStamp {
    dev: u64,
    ino: u64,
    mtime: (i64, i64),
}

impl ExeStamp {
    /// The file at `path`, symlinks followed, or None if there is none.
    pub(crate) fn of(path: impl AsRef<Path>) -> Option<ExeStamp> {
        let m = std::fs::metadata(path).ok()?;
        Some(ExeStamp {
            dev: m.dev(),
            ino: m.ino(),
            mtime: (m.mtime(), m.mtime_nsec()),
        })
    }
}

/// A requested pane's terminal was refused for its version: the binary at
/// the daemon's exe path is another than it runs. It holds until that file
/// changes again: replaced, written, or come or gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VersionRefused {
    /// The file at the exe path as it was at the refusal.
    then: Option<ExeStamp>,
}

impl VersionRefused {
    /// A refusal now, of the binary at `exe`.
    pub(crate) fn of(exe: impl AsRef<Path>) -> VersionRefused {
        VersionRefused {
            then: ExeStamp::of(exe),
        }
    }

    /// Whether the file at `exe` is still the one refused.
    pub(crate) fn holds(self, exe: impl AsRef<Path>) -> bool {
        ExeStamp::of(exe) == self.then
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(dev: u64, ino: u64, mtime: i64) -> Option<ExeStamp> {
        Some(ExeStamp {
            dev,
            ino,
            mtime: (mtime, 0),
        })
    }

    #[test]
    fn a_stamp_differs_if_the_device_inode_or_mtime_does() {
        assert_eq!(stamp(1, 2, 3), stamp(1, 2, 3));
        for other in [stamp(9, 2, 3), stamp(1, 9, 3), stamp(1, 2, 9), None] {
            assert_ne!(stamp(1, 2, 3), other);
        }
    }

    #[test]
    fn a_refusal_holds_until_the_file_is_replaced_or_goes() {
        let dir = std::env::temp_dir().join(format!("exe-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("exe");
        std::fs::write(&exe, "old").unwrap();
        let refused = VersionRefused::of(&exe);
        assert!(refused.then.is_some());
        assert!(refused.holds(&exe));
        // as `just install` does it: a new file renamed over the old
        let new = dir.join(".exe.new");
        std::fs::write(&new, "new").unwrap();
        std::fs::rename(&new, &exe).unwrap();
        assert!(!refused.holds(&exe));
        let refused = VersionRefused::of(&exe);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!refused.holds(&exe));
    }
}
