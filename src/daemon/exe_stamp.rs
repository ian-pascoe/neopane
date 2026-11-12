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

/// Whether the file at a path changed since `then`, now `now`: a new file,
/// one written since, or one that came or went.
pub(crate) fn changed(then: Option<ExeStamp>, now: Option<ExeStamp>) -> bool {
    then != now
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
    fn a_file_changed_only_if_its_device_inode_or_mtime_did_or_it_came_or_went() {
        assert!(!changed(stamp(1, 2, 3), stamp(1, 2, 3)));
        assert!(!changed(None, None));
        for now in [stamp(9, 2, 3), stamp(1, 9, 3), stamp(1, 2, 9), None] {
            assert!(changed(stamp(1, 2, 3), now), "{now:?}");
        }
        assert!(changed(None, stamp(1, 2, 3)));
    }

    #[test]
    fn a_file_replaced_by_rename_has_another_stamp_and_an_untouched_one_the_same() {
        let dir = std::env::temp_dir().join(format!("exe-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("exe");
        std::fs::write(&exe, "old").unwrap();
        let then = ExeStamp::of(&exe);
        assert!(then.is_some());
        assert_eq!(then, ExeStamp::of(&exe));
        // as `just install` does it: a new file renamed over the old
        let new = dir.join(".exe.new");
        std::fs::write(&new, "new").unwrap();
        std::fs::rename(&new, &exe).unwrap();
        assert!(changed(then, ExeStamp::of(&exe)));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(None, ExeStamp::of(&exe));
    }
}
