use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: OsString,
    pub path: PathBuf,
    pub directory: bool,
}

impl Entry {
    /// Lossy display text, with the legacy directory marker appended.
    pub fn display_name(&self) -> String {
        let mut name = self.name.to_string_lossy().into_owned();
        if self.directory {
            name.push('/');
        }
        name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Explorer {
    pub directory: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selection {
    Directory(PathBuf),
    File(PathBuf),
}

impl Explorer {
    /// Scan regular files and directories, including hidden names, and add a
    /// synthetic parent-directory entry. Symlinks and other special file kinds
    /// are not regular explorer entries.
    pub fn scan(directory: &Path) -> io::Result<Self> {
        // Resolving the parent up front keeps repeated ascents from
        // accumulating literal `..` components in every opened file's name.
        let parent = directory
            .canonicalize()
            .ok()
            .and_then(|canonical| canonical.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| directory.join(".."));
        let mut entries = vec![Entry {
            name: OsString::from(".."),
            path: parent,
            directory: true,
        }];

        for result in std::fs::read_dir(directory)? {
            // One unreadable entry should not make the whole directory
            // unbrowsable; skip what cannot be inspected.
            let Ok(entry) = result else {
                continue;
            };
            let Ok(file_type) = entry
                .file_type()
                .or_else(|_| entry.metadata().map(|metadata| metadata.file_type()))
            else {
                continue;
            };
            if !file_type.is_dir() && !file_type.is_file() {
                continue;
            }
            entries.push(Entry {
                name: entry.file_name(),
                path: entry.path(),
                directory: file_type.is_dir(),
            });
        }

        // Deterministic lossy display ordering is the specified Rust
        // compatibility choice; locale-dependent collation is intentionally not
        // introduced into this adapter.
        entries.sort_by_cached_key(Entry::display_name);
        Ok(Self {
            directory: directory.to_path_buf(),
            entries,
            cursor: 0,
        })
    }

    pub fn move_down(&mut self) {
        if self.cursor < self.entries.len().saturating_sub(1) {
            self.cursor += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn selection(&self) -> Option<Selection> {
        self.entries.get(self.cursor).map(|entry| {
            if entry.directory {
                Selection::Directory(entry.path.clone())
            } else {
                Selection::File(entry.path.clone())
            }
        })
    }

    /// Re-scan a selected directory and reset navigation to the first sorted
    /// entry. If scanning fails, the current explorer state is left unchanged.
    pub fn enter_directory(&mut self, path: &Path) -> io::Result<()> {
        let next = Self::scan(path)?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos();
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "cano-fresh-explorer-{label}-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&root).expect("create explorer fixture");
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn display_names(explorer: &Explorer) -> Vec<String> {
        explorer.entries.iter().map(Entry::display_name).collect()
    }

    #[test]
    fn display_name_marks_only_directories() {
        let file = Entry {
            name: OsString::from("name"),
            path: PathBuf::from("name"),
            directory: false,
        };
        let directory = Entry {
            directory: true,
            ..file.clone()
        };
        assert_eq!(file.display_name(), "name");
        assert_eq!(directory.display_name(), "name/");
    }

    #[test]
    fn scan_includes_parent_hidden_files_and_directories_in_display_order() {
        let fixture = Fixture::new("scan");
        std::fs::create_dir(fixture.root.join("a-dir")).unwrap();
        std::fs::create_dir(fixture.root.join("z-dir")).unwrap();
        std::fs::write(fixture.root.join("middle"), b"m").unwrap();
        std::fs::write(fixture.root.join(".hidden"), b"h").unwrap();

        let explorer = Explorer::scan(&fixture.root).unwrap();

        assert_eq!(
            display_names(&explorer),
            ["../", ".hidden", "a-dir/", "middle", "z-dir/"]
        );
        assert_eq!(explorer.directory, fixture.root);
        assert_eq!(explorer.cursor, 0);
        let canonical_parent = fixture
            .root
            .canonicalize()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(
            explorer.selection(),
            Some(Selection::Directory(canonical_parent))
        );
    }

    #[test]
    fn parent_is_sorted_with_names_instead_of_forced_to_index_zero() {
        let fixture = Fixture::new("parent-sort");
        std::fs::write(fixture.root.join("!before-parent"), []).unwrap();

        let explorer = Explorer::scan(&fixture.root).unwrap();

        assert_eq!(display_names(&explorer), ["!before-parent", "../"]);
        assert_eq!(
            explorer.selection(),
            Some(Selection::File(fixture.root.join("!before-parent")))
        );
    }

    #[test]
    fn navigation_clamps_at_both_ends_and_selection_tracks_the_cursor() {
        let fixture = Fixture::new("navigation");
        std::fs::write(fixture.root.join("a"), []).unwrap();
        let mut explorer = Explorer::scan(&fixture.root).unwrap();

        explorer.move_up();
        assert_eq!(explorer.cursor, 0);
        explorer.move_down();
        explorer.move_down();
        explorer.move_down();
        assert_eq!(explorer.cursor, 1);
        assert_eq!(
            explorer.selection(),
            Some(Selection::File(fixture.root.join("a")))
        );
        explorer.move_up();
        assert_eq!(explorer.cursor, 0);
    }

    #[test]
    fn out_of_bounds_and_empty_navigation_are_safe() {
        let mut explorer = Explorer {
            directory: PathBuf::from("unused"),
            entries: Vec::new(),
            cursor: usize::MAX,
        };
        assert_eq!(explorer.selection(), None);
        explorer.move_down();
        explorer.move_up();
        assert_eq!(explorer.selection(), None);
    }

    #[test]
    fn entering_a_directory_rescans_it_and_resets_the_cursor() {
        let fixture = Fixture::new("enter-directory");
        let child = fixture.root.join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("inside"), b"data").unwrap();
        let mut explorer = Explorer::scan(&fixture.root).unwrap();
        let child_index = explorer
            .entries
            .iter()
            .position(|entry| entry.path == child)
            .unwrap();
        explorer.cursor = child_index;

        assert_eq!(
            explorer.selection(),
            Some(Selection::Directory(child.clone()))
        );
        explorer.enter_directory(&child).unwrap();
        assert_eq!(explorer.directory, child);
        assert_eq!(explorer.cursor, 0);
        assert_eq!(display_names(&explorer), ["../", "inside"]);
    }

    #[test]
    fn selecting_a_file_does_not_change_explorer_state() {
        let fixture = Fixture::new("enter-file");
        let file = fixture.root.join("file");
        std::fs::write(&file, b"data").unwrap();
        let mut explorer = Explorer::scan(&fixture.root).unwrap();
        explorer.cursor = explorer
            .entries
            .iter()
            .position(|entry| entry.path == file)
            .unwrap();
        let before = explorer.clone();

        assert_eq!(explorer.selection(), Some(Selection::File(file)));
        assert_eq!(explorer, before);
    }

    #[test]
    fn failed_directory_entry_is_transactional() {
        let fixture = Fixture::new("failed-enter");
        std::fs::write(fixture.root.join("file"), b"data").unwrap();
        let mut explorer = Explorer::scan(&fixture.root).unwrap();
        explorer.cursor = explorer.entries.len() - 1;
        let before = explorer.clone();

        assert!(
            explorer
                .enter_directory(&fixture.root.join("missing"))
                .is_err()
        );
        assert_eq!(explorer, before);
    }

    #[test]
    fn scan_rejects_missing_paths_and_regular_files() {
        let fixture = Fixture::new("scan-errors");
        let file = fixture.root.join("file");
        std::fs::write(&file, []).unwrap();

        assert_eq!(
            Explorer::scan(&fixture.root.join("missing"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(Explorer::scan(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn scan_skips_symlinks_and_uses_lossy_names_for_non_utf8_entries() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("unix-names");
        let target_file = fixture.root.join("target-file");
        let target_dir = fixture.root.join("target-dir");
        std::fs::write(&target_file, []).unwrap();
        std::fs::create_dir(&target_dir).unwrap();
        symlink(&target_file, fixture.root.join("file-link")).unwrap();
        symlink(&target_dir, fixture.root.join("dir-link")).unwrap();
        let invalid_name = OsString::from_vec(vec![b'x', 0xff]);
        std::fs::write(fixture.root.join(&invalid_name), []).unwrap();

        let explorer = Explorer::scan(&fixture.root).unwrap();
        let names = display_names(&explorer);

        assert!(!names.iter().any(|name| name == "file-link"));
        assert!(!names.iter().any(|name| name == "dir-link/"));
        assert!(names.iter().any(|name| name == "x�"));
        assert!(
            explorer
                .entries
                .iter()
                .any(|entry| entry.name == invalid_name)
        );
    }
}
