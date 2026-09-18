use std::ffi::OsStr;
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

/// Directories that may hold help pages, in search order.
///
/// Only the first two are configured -- `runtime` from the environment and
/// `compiled` from the build.  The rest are derived from where the binary
/// actually is, because a bare `docs/help` only resolves when the process
/// happens to be run from a checkout root: `cargo` leaves the binary in
/// `target/<profile>/`, `make` copies it to `build/`, and an installed binary
/// sits in `<prefix>/bin` beside `<prefix>/share/cano/help`.
///
/// The bare relative path stays last rather than being dropped: it is what
/// shipped, and it still answers for anyone running from a checkout root.
pub fn help_directories(
    runtime: Option<&OsStr>,
    compiled: Option<&str>,
    executable: Option<&Path>,
) -> Vec<PathBuf> {
    let mut directories = Vec::new();

    // An empty setting names no directory.  Taking it literally would join
    // the page onto nothing and search the current directory, so it counts as
    // unset instead.
    for configured in [runtime.map(PathBuf::from), compiled.map(PathBuf::from)] {
        if let Some(directory) = configured.filter(|path| !path.as_os_str().is_empty()) {
            directories.push(directory);
        }
    }

    if let Some(prefix) = executable.and_then(Path::parent).and_then(Path::parent) {
        // `build/cano` from `make`, then `<prefix>/bin/cano` once installed.
        directories.push(prefix.join("docs/help"));
        directories.push(prefix.join("share/cano/help"));
        if let Some(root) = prefix.parent() {
            // `target/<profile>/cano` from a plain `cargo build`.
            directories.push(root.join("docs/help"));
        }
    }

    directories.push(PathBuf::from("docs/help"));
    directories
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;

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
    fn help_directories_search_configured_paths_before_derived_ones() {
        let directories = help_directories(
            Some(OsStr::new("/run/help")),
            Some("/build/help"),
            Some(Path::new("/opt/cano/bin/cano")),
        );
        assert_eq!(directories[0], PathBuf::from("/run/help"));
        assert_eq!(directories[1], PathBuf::from("/build/help"));
        assert_eq!(directories.last().unwrap(), &PathBuf::from("docs/help"));
    }

    #[test]
    fn help_directories_cover_both_layouts_the_build_produces() {
        // `cargo` leaves the binary two levels below the checkout root, and
        // `make` copies it one level below.  Neither can rely on the process
        // being run from the root, so both are derived from the binary.
        let cargo = help_directories(None, None, Some(Path::new("/w/cano/target/release/cano")));
        assert!(
            cargo.contains(&PathBuf::from("/w/cano/docs/help")),
            "{cargo:?}"
        );

        let make = help_directories(None, None, Some(Path::new("/w/cano/build/cano")));
        assert!(
            make.contains(&PathBuf::from("/w/cano/docs/help")),
            "{make:?}"
        );

        let installed = help_directories(None, None, Some(Path::new("/usr/bin/cano")));
        assert!(
            installed.contains(&PathBuf::from("/usr/share/cano/help")),
            "{installed:?}"
        );
    }

    #[test]
    fn an_empty_setting_is_not_a_directory() {
        // Kept out because joining a page onto it would search the current
        // directory, which is exactly what the derived paths exist to avoid.
        let directories = help_directories(Some(OsStr::new("")), Some(""), None);
        assert_eq!(directories, vec![PathBuf::from("docs/help")]);
    }

    #[test]
    fn help_directories_still_answer_without_a_known_executable() {
        assert_eq!(
            help_directories(None, None, None),
            vec![PathBuf::from("docs/help")]
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
