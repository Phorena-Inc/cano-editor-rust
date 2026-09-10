//! The most-recently-opened file list behind Ctrl-P.
//!
//! The list is persisted beside the effective configuration file, in the same
//! place `.cyntax` palettes are looked up, as one path per line.  Paths are
//! stored as bytes and absolute, so the list still means something when cano
//! is next launched from a different directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How many paths the list keeps.  Older entries fall off the end.
pub const CAPACITY: usize = 50;

/// A most-recently-used list of opened files, and the picker's cursor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Recent {
    pub paths: Vec<PathBuf>,
    pub cursor: usize,
}

impl Recent {
    /// Reads the list, keeping only entries that still name a readable file.
    ///
    /// A missing or unreadable list is not an error: it just means there is
    /// no history yet, and refusing to start over it would be absurd.
    pub fn load(path: &Path) -> Self {
        let bytes = fs::read(path).unwrap_or_default();
        let mut recent = Self::default();
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            recent.paths.push(bytes_to_path(line));
        }
        recent.paths.truncate(CAPACITY);
        recent.prune();
        recent
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut bytes = Vec::new();
        for entry in &self.paths {
            let raw = path_bytes(entry);
            // A path containing a newline cannot survive a line-based list,
            // so it is dropped rather than split into two bogus entries.
            if raw.contains(&b'\n') {
                continue;
            }
            bytes.extend_from_slice(&raw);
            bytes.push(b'\n');
        }
        fs::write(path, bytes)
    }

    /// Moves `path` to the front of the list.
    pub fn record(&mut self, path: &Path) {
        // An absolute path keeps the list meaningful from any working
        // directory, and makes the same file recorded under two spellings
        // collapse to one entry.
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.paths.retain(|existing| *existing != path);
        self.paths.insert(0, path);
        self.paths.truncate(CAPACITY);
        self.cursor = 0;
    }

    /// Drops entries that no longer name a readable file, so the picker never
    /// offers something that cannot be opened.
    pub fn prune(&mut self) {
        self.paths.retain(|path| path.is_file());
        self.cursor = self.cursor.min(self.paths.len().saturating_sub(1));
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.paths.len() {
            self.cursor += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn selection(&self) -> Option<PathBuf> {
        self.paths.get(self.cursor).cloned()
    }

    /// Lossy display text for one entry.  The whole path is shown because two
    /// recent files often share a base name.
    pub fn display_name(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-recent-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("fixture directory");
        root
    }

    fn touch(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, b"x").expect("fixture file");
        path.canonicalize().expect("canonical fixture path")
    }

    #[test]
    fn recording_moves_to_the_front_without_duplicating_or_growing_forever() {
        let root = fixture("record");
        let first = touch(&root, "first");
        let second = touch(&root, "second");

        let mut recent = Recent::default();
        recent.record(&first);
        recent.record(&second);
        assert_eq!(recent.paths, [second.clone(), first.clone()]);

        // Re-recording promotes rather than appending a second copy.
        recent.record(&first);
        assert_eq!(recent.paths, [first, second]);
        assert_eq!(recent.cursor, 0);

        for index in 0..CAPACITY + 10 {
            recent.record(&root.join(format!("f{index}")));
        }
        assert_eq!(recent.paths.len(), CAPACITY);

        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn the_list_round_trips_through_disk_and_forgets_deleted_files() {
        let root = fixture("round-trip");
        let kept = touch(&root, "kept");
        let removed = touch(&root, "removed");
        let store = root.join("recent");

        let mut recent = Recent::default();
        recent.record(&kept);
        recent.record(&removed);
        recent.save(&store).expect("write the list");

        let reloaded = Recent::load(&store);
        assert_eq!(reloaded.paths, [removed.clone(), kept.clone()]);

        fs::remove_file(&removed).expect("remove a listed file");
        let pruned = Recent::load(&store);
        assert_eq!(pruned.paths, [kept]);

        // A list that was never written is empty rather than an error.
        assert!(Recent::load(&root.join("absent")).is_empty());

        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn pruning_keeps_the_cursor_inside_what_is_left() {
        let root = fixture("prune");
        let kept = touch(&root, "kept");
        let mut recent = Recent {
            paths: vec![root.join("gone"), kept.clone(), root.join("also-gone")],
            cursor: 2,
        };

        recent.prune();
        assert_eq!(recent.paths, [kept]);
        assert_eq!(recent.cursor, 0);
        assert_eq!(recent.selection(), recent.paths.first().cloned());

        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut recent = Recent {
            paths: vec![PathBuf::from("a"), PathBuf::from("b")],
            cursor: 0,
        };
        recent.move_up();
        assert_eq!(recent.cursor, 0);
        recent.move_down();
        recent.move_down();
        assert_eq!(recent.cursor, 1);
        assert_eq!(recent.selection(), Some(PathBuf::from("b")));

        // An empty list has nothing to select and cannot be moved off.
        let mut empty = Recent::default();
        empty.move_down();
        assert_eq!(empty.cursor, 0);
        assert_eq!(empty.selection(), None);
    }
}
