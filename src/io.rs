use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Load a file without text decoding or newline normalization.
pub fn load_buffer(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

/// Create or truncate `path`, then write exactly the logical buffer bytes.
///
/// This intentionally preserves Cano's non-atomic, truncating save behavior.
pub fn save_buffer(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)
}

/// Resolve a help page only when it names an existing regular file.
pub fn help_page(help_dir: &Path, page: &str) -> Option<PathBuf> {
    let path = help_dir.join(page);
    path.is_file().then_some(path)
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
                "cano-fresh-io-{label}-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create IO fixture");
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn load_returns_empty_and_binary_files_byte_for_byte() {
        let fixture = Fixture::new("load");
        let empty = fixture.root.join("empty");
        let binary = fixture.root.join("binary");
        fs::write(&empty, []).unwrap();
        let expected = b"\0\xff\r\ntext\0\n";
        fs::write(&binary, expected).unwrap();

        assert_eq!(load_buffer(&empty).unwrap(), Vec::<u8>::new());
        assert_eq!(load_buffer(&binary).unwrap(), expected);
    }

    #[test]
    fn load_propagates_missing_path_and_directory_errors() {
        let fixture = Fixture::new("load-errors");
        assert_eq!(
            load_buffer(&fixture.root.join("missing"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(load_buffer(&fixture.root).is_err());
    }

    #[test]
    fn save_creates_a_file_and_preserves_all_bytes() {
        let fixture = Fixture::new("save-create");
        let path = fixture.root.join("new.bin");
        let expected = b"first\0second\xff\n";

        save_buffer(&path, expected).unwrap();

        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(fs::metadata(&path).unwrap().len(), expected.len() as u64);
    }

    #[test]
    fn save_truncates_longer_destinations_including_to_empty() {
        let fixture = Fixture::new("save-truncate");
        let path = fixture.root.join("existing");
        fs::write(&path, b"long existing contents").unwrap();

        save_buffer(&path, b"a\0b\n").unwrap();
        assert_eq!(load_buffer(&path).unwrap(), b"a\0b\n");
        assert_eq!(fs::metadata(&path).unwrap().len(), 4);

        save_buffer(&path, &[]).unwrap();
        assert_eq!(load_buffer(&path).unwrap(), Vec::<u8>::new());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn save_reports_invalid_destinations() {
        let fixture = Fixture::new("save-errors");
        let missing_parent = fixture.root.join("missing").join("file");
        assert_eq!(
            save_buffer(&missing_parent, b"data").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(save_buffer(&fixture.root, b"data").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn save_follows_a_destination_symlink_and_truncates_its_target() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("save-symlink");
        let target = fixture.root.join("target");
        let link = fixture.root.join("link");
        fs::write(&target, b"target was longer").unwrap();
        symlink(&target, &link).unwrap();

        save_buffer(&link, b"new\0").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new\0");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn help_page_returns_only_existing_files() {
        let fixture = Fixture::new("help");
        let help = fixture.root.join("help");
        fs::create_dir(&help).unwrap();
        fs::write(help.join("general"), b"general help").unwrap();
        fs::create_dir(help.join("keys")).unwrap();

        assert_eq!(help_page(&help, "general"), Some(help.join("general")));
        assert_eq!(help_page(&help, "missing"), None);
        assert_eq!(help_page(&help, "keys"), None);
        assert_eq!(help_page(&fixture.root.join("absent"), "general"), None);
    }

    #[cfg(unix)]
    #[test]
    fn help_page_accepts_a_symlink_to_a_file_but_not_a_dangling_one() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("help-symlink");
        let help = fixture.root.join("help");
        fs::create_dir(&help).unwrap();
        fs::write(help.join("general"), b"help").unwrap();
        symlink(help.join("general"), help.join("alias")).unwrap();
        symlink(help.join("missing"), help.join("dangling")).unwrap();

        assert_eq!(help_page(&help, "alias"), Some(help.join("alias")));
        assert_eq!(help_page(&help, "dangling"), None);
    }
}
