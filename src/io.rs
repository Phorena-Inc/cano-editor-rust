use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Load a file without text decoding or newline normalization.
pub fn load_buffer(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

/// The absolute, symlink-free form of `path`, spelled the way a user would.
///
/// On Windows `fs::canonicalize` answers with a verbatim `\\?\` path, which
/// then turns up in the status line and in every file opened from it.  The
/// prefix is dropped again for ordinary drive and UNC paths.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map(plain_path)
}

#[cfg(windows)]
fn plain_path(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(not(windows))]
fn plain_path(path: PathBuf) -> PathBuf {
    path
}

/// Write exactly the logical buffer bytes to `path`, atomically.
///
/// The bytes go to a temporary file beside the destination, are flushed to
/// the disk, and only then renamed over it.  Truncating in place -- what
/// Cano used to do -- leaves a half-written or empty file behind when the
/// disk fills up or the machine goes down mid-write, and a reader can see
/// the file empty even when nothing goes wrong.  A rename is all or nothing.
///
/// The replacement keeps what the old file had that a fresh one would not:
///
/// - Its permissions -- the whole unix mode, or the read-only flag elsewhere
///   -- so a script stays executable and a private file stays private.  On
///   unix the owner and group are carried over too.
/// - Its symlinks.  A symlink is followed to the file it names, and that file
///   is what gets replaced, so the link itself survives and still points at
///   the new contents.
///
/// Where a rename cannot do that, the save falls back to rewriting the file
/// in place, the old way:
///
/// - The directory is not writable but the file is, so no temporary file
///   can be made beside it.
/// - The destination is not a regular file, or (on unix) has other hard
///   links, which a rename would leave holding the old contents, or an owner
///   this process cannot give a new file.
///
/// A file this process may not write is still refused, exactly as before,
/// even though the rename alone would not need its permission.
pub fn save_buffer(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = resolve_symlinks(path)?;
    let existing = fs::metadata(&target).ok();
    if existing.as_ref().is_some_and(must_write_in_place) {
        return fs::write(&target, bytes);
    }
    // A rename only needs the directory to be writable, so it would happily
    // replace a file its owner made read-only.  Opening it for writing --
    // without truncating -- asks the question the old in-place write asked,
    // and refuses the save with the same error it always gave.
    if existing.is_some() {
        OpenOptions::new().write(true).open(&target)?;
    }
    match write_beside(&target, bytes, existing.as_ref()) {
        Ok(()) => Ok(()),
        // Only the temporary file could not be made: the directory refuses
        // new entries, which says nothing about the file itself.
        Err(Failure::Create(error))
            if error.kind() == io::ErrorKind::PermissionDenied && existing.is_some() =>
        {
            fs::write(&target, bytes)
        }
        Err(Failure::Create(error) | Failure::Write(error)) => Err(error),
        #[cfg(unix)]
        Err(Failure::Owner) => fs::write(&target, bytes),
    }
}

/// How far symlinks are followed before the chain is taken to be a loop.
/// The same limit Linux applies to path resolution.
const MAX_SYMLINK_HOPS: usize = 40;

/// The file `path` finally names, following symlinks one hop at a time.
///
/// `fs::canonicalize` would answer for an existing file, but a dangling link
/// is a legitimate save destination -- writing creates the file it names, as
/// it always did -- and canonicalizing refuses one.  A relative link is
/// relative to the directory holding the link, not to the current one.
fn resolve_symlinks(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        let is_link = fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        if !is_link {
            return Ok(current);
        }
        let link = fs::read_link(&current)?;
        current = match current.parent() {
            Some(parent) if link.is_relative() => parent.join(link),
            _ => link,
        };
    }
    Err(io::Error::other(format!(
        "too many levels of symbolic links: {}",
        path.display()
    )))
}

/// Whether replacing this file would lose something only an in-place write
/// keeps: what it is, when it is not a regular file (a FIFO or a device must
/// not turn into a plain file), or its other hard links.  A directory lands
/// here too, and the in-place write reports it as before.
#[cfg(unix)]
fn must_write_in_place(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    !metadata.is_file() || metadata.nlink() > 1
}

#[cfg(not(unix))]
fn must_write_in_place(metadata: &fs::Metadata) -> bool {
    !metadata.is_file()
}

/// Why an atomic write did not happen, which decides whether the in-place
/// fallback is worth trying.
enum Failure {
    /// The temporary file could not be created.
    Create(io::Error),
    /// Anything after that.
    Write(io::Error),
    /// The new file could not be given the old one's owner.
    #[cfg(unix)]
    Owner,
}

/// Writes `bytes` to a new file in `target`'s directory and renames it over
/// `target`, removing the temporary file again on every failure.
fn write_beside(
    target: &Path,
    bytes: &[u8],
    existing: Option<&fs::Metadata>,
) -> Result<(), Failure> {
    let directory = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| Failure::Write(io::Error::from(io::ErrorKind::InvalidInput)))?;
    let (temporary, mut file) =
        create_temporary(directory, name, existing.is_some()).map_err(Failure::Create)?;

    let result = (|| {
        file.write_all(bytes).map_err(Failure::Write)?;
        if let Some(metadata) = existing {
            // Ownership first: changing it clears set-id bits, which the
            // permissions then put back.
            restore_owner(&file, metadata)?;
            file.set_permissions(metadata.permissions())
                .map_err(Failure::Write)?;
        }
        // On the disk before the rename, or a crash could leave the new name
        // pointing at a file whose contents never got there.
        file.sync_all().map_err(Failure::Write)?;
        drop(file);
        fs::rename(&temporary, target).map_err(Failure::Write)
    })();
    match result {
        Ok(()) => {
            // The rename itself lives in the directory.  A directory that
            // cannot be synced is no reason to report a failed save: the
            // data is already safe and the rename has happened.
            if let Ok(handle) = File::open(directory) {
                let _ = handle.sync_all();
            }
            Ok(())
        }
        Err(failure) => {
            let _ = fs::remove_file(&temporary);
            Err(failure)
        }
    }
}

/// Creates a fresh temporary file for `name` in `directory`.
///
/// The name is hidden and marked as Cano's so a crash leaves something
/// recognisable rather than a stray copy of the file.  `create_new` refuses
/// anything already there -- including a symlink planted in its place --
/// so the file written is always one this call made.  On unix, a file that
/// replaces an existing one starts private, and only gets the original's
/// mode once its contents are in.
fn create_temporary(
    directory: &Path,
    name: &OsStr,
    replacing: bool,
) -> io::Result<(PathBuf, File)> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..100 {
        let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut candidate = OsString::from(".");
        candidate.push(name);
        candidate.push(format!(".cano-{}-{nth}.tmp", std::process::id()));
        let path = directory.join(candidate);

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if replacing {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = replacing;
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Gives the new file the old one's owner and group, where this process is
/// allowed to.  A file it cannot hand back to its owner is written in place
/// instead, so saving someone else's file never quietly takes it over.
#[cfg(unix)]
fn restore_owner(file: &File, metadata: &fs::Metadata) -> Result<(), Failure> {
    use std::os::unix::fs::{MetadataExt, fchown};

    let current = file.metadata().map_err(Failure::Write)?;
    if current.uid() == metadata.uid() && current.gid() == metadata.gid() {
        return Ok(());
    }
    fchown(file, Some(metadata.uid()), Some(metadata.gid())).map_err(|_| Failure::Owner)
}

#[cfg(not(unix))]
fn restore_owner(_file: &File, _metadata: &fs::Metadata) -> Result<(), Failure> {
    Ok(())
}

/// The help pages as they were when the binary was built.  Each answers when
/// none of [`help_directories`] holds that page, so a binary copied on its
/// own still has all of its help, not just the overview.
pub const GENERAL_HELP: &[u8] = include_bytes!("../docs/help/general");
pub const KEYS_HELP: &[u8] = include_bytes!("../docs/help/keys");
pub const CMDS_HELP: &[u8] = include_bytes!("../docs/help/cmds");

/// Every help page Cano ships, in the order an error message lists them.
///
/// A page name is checked against this before any directory is searched, so
/// `:help ../../etc/passwd` is an unknown page rather than a file read: the
/// name is joined onto a directory, and only these names are documentation.
pub const HELP_PAGES: &[&str] = &["general", "keys", "cmds"];

/// The built-in copy of a shipped page, or `None` for any other name.
pub fn embedded_help(page: &str) -> Option<&'static [u8]> {
    match page {
        "general" => Some(GENERAL_HELP),
        "keys" => Some(KEYS_HELP),
        "cmds" => Some(CMDS_HELP),
        _ => None,
    }
}

/// Why a help page could not be produced.
#[derive(Debug)]
pub enum HelpError {
    /// Not one of [`HELP_PAGES`]; the caller words the message, since the
    /// command line lists the pages and the status line has no room to.
    UnknownPage,
    /// The page exists on disk but reading it failed.  The embedded copy is
    /// deliberately not substituted: a file that is there but unreadable is
    /// something the user should hear about, the same as for any file.
    Unreadable { path: PathBuf, error: io::Error },
}

/// The directories this process searches for help pages: `$CANO_HELP_DIR`,
/// then the build-time `CANO_HELP_DIR`, then the paths derived from the
/// executable (see [`help_directories`]).
pub fn help_search_directories() -> Vec<PathBuf> {
    let runtime = std::env::var_os("CANO_HELP_DIR");
    let executable = std::env::current_exe().ok();
    help_directories(
        runtime.as_deref(),
        option_env!("CANO_HELP_DIR"),
        executable.as_deref(),
    )
}

/// Loads help `page` from the first of `directories` holding it, falling
/// back to the copy built into the binary.
///
/// Returns the name the buffer should carry with the bytes: the page's path
/// on disk, or the bare page name for the embedded copy.  Neither has an
/// extension, so a help buffer gets no syntax palette and no markdown layer.
/// Startup `--help` and `:help` both come through here, so they cannot
/// disagree about which copy of a page wins.
pub fn load_help(page: &str, directories: &[PathBuf]) -> Result<(PathBuf, Vec<u8>), HelpError> {
    let embedded = embedded_help(page).ok_or(HelpError::UnknownPage)?;
    debug_assert!(
        HELP_PAGES.contains(&page),
        "embedded_help and HELP_PAGES agree"
    );
    match directories
        .iter()
        .find_map(|directory| help_page(directory, page))
    {
        Some(path) => match load_buffer(&path) {
            Ok(bytes) => Ok((path, bytes)),
            Err(error) => Err(HelpError::Unreadable { path, error }),
        },
        None => Ok((PathBuf::from(page), embedded.to_vec())),
    }
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

    #[cfg(windows)]
    #[test]
    fn canonical_paths_drop_the_verbatim_prefix_only_where_it_is_redundant() {
        let plain = |text: &str| plain_path(PathBuf::from(text));
        assert_eq!(plain(r"\\?\C:\work\a.txt"), PathBuf::from(r"C:\work\a.txt"));
        assert_eq!(
            plain(r"\\?\UNC\server\share\a.txt"),
            PathBuf::from(r"\\server\share\a.txt")
        );
        // Device namespaces have no plain spelling, so they are left alone.
        assert_eq!(
            plain(r"\\?\Volume{0}\a.txt"),
            PathBuf::from(r"\\?\Volume{0}\a.txt")
        );

        let fixture = Fixture::new("canonical");
        let file = fixture.root.join("a.txt");
        fs::write(&file, b"x").unwrap();
        let canonical = canonicalize(&file).unwrap();
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
        assert!(canonical.is_file());
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

    /// Whatever is in `directory` besides `keep`: a save must not leave its
    /// temporary file behind, whether it succeeded or not.
    fn leftovers(directory: &Path, keep: &[&str]) -> Vec<OsString> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| !keep.iter().any(|kept| name == kept))
            .collect()
    }

    /// Root ignores permission bits, so tests about refusals cannot run as it.
    #[cfg(unix)]
    fn running_as_root() -> bool {
        // Safety: `geteuid` takes nothing and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn save_replaces_the_file_through_a_temporary_and_leaves_nothing_behind() {
        let fixture = Fixture::new("save-atomic");
        let path = fixture.root.join("doc.txt");
        fs::write(&path, b"old contents that are longer").unwrap();

        save_buffer(&path, b"new\0\xff\r\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\0\xff\r\n");
        assert!(leftovers(&fixture.root, &["doc.txt"]).is_empty());

        // The file is a new one renamed into place rather than the old one
        // truncated: anything holding the old file still sees it whole.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mut reader = fs::File::open(&path).unwrap();
            let before = fs::metadata(&path).unwrap().ino();
            save_buffer(&path, b"replaced").unwrap();
            assert_ne!(fs::metadata(&path).unwrap().ino(), before);
            let mut held = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut held).unwrap();
            assert_eq!(held, b"new\0\xff\r\n");
        }
    }

    #[test]
    fn a_failed_save_leaves_the_original_and_no_temporary() {
        let fixture = Fixture::new("save-failed");
        // A directory where the file should be: the temporary can be made,
        // but nothing may replace the directory.
        let path = fixture.root.join("dir");
        fs::create_dir(&path).unwrap();
        assert!(save_buffer(&path, b"data").is_err());
        assert!(path.is_dir());
        assert!(leftovers(&fixture.root, &["dir"]).is_empty());
    }

    #[test]
    fn save_refuses_a_read_only_file_as_it_always_did() {
        #[cfg(unix)]
        if running_as_root() {
            return;
        }
        let fixture = Fixture::new("save-readonly");
        let path = fixture.root.join("locked.txt");
        fs::write(&path, b"keep me").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        // The directory is writable, so a rename alone would go through.
        let error = save_buffer(&path, b"overwritten").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"keep me");
        assert!(leftovers(&fixture.root, &["locked.txt"]).is_empty());

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_the_files_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new("save-mode");
        for mode in [0o600, 0o755, 0o640, 0o4755] {
            let path = fixture.root.join(format!("file-{mode:o}"));
            fs::write(&path, b"before").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();

            save_buffer(&path, b"after").unwrap();
            let kept = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(kept, mode, "a {mode:o} file");
            assert_eq!(fs::read(&path).unwrap(), b"after");
        }
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_a_symlink_and_keeps_the_link() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let fixture = Fixture::new("save-symlink-kept");
        let real = fixture.root.join("real");
        fs::create_dir(&real).unwrap();
        let target = real.join("target.sh");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
        // A relative link resolves against its own directory, and a chain of
        // links is followed to its end.
        symlink("real/target.sh", fixture.root.join("first")).unwrap();
        symlink("first", fixture.root.join("second")).unwrap();

        save_buffer(&fixture.root.join("second"), b"new contents").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new contents");
        for link in ["first", "second"] {
            let metadata = fs::symlink_metadata(fixture.root.join(link)).unwrap();
            assert!(metadata.file_type().is_symlink(), "{link} is still a link");
        }
        assert_eq!(
            fs::read_link(fixture.root.join("second")).unwrap(),
            PathBuf::from("first")
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o750
        );
        // The temporary was made beside the target, and is gone.
        assert!(leftovers(&real, &["target.sh"]).is_empty());
        assert!(leftovers(&fixture.root, &["real", "first", "second"]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn save_through_a_dangling_symlink_creates_what_it_names() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("save-dangling");
        let link = fixture.root.join("link");
        symlink("made-on-save", &link).unwrap();

        save_buffer(&link, b"fresh").unwrap();
        assert_eq!(
            fs::read(fixture.root.join("made-on-save")).unwrap(),
            b"fresh"
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_is_an_error_rather_than_a_hang() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("save-loop");
        symlink("b", fixture.root.join("a")).unwrap();
        symlink("a", fixture.root.join("b")).unwrap();
        assert!(save_buffer(&fixture.root.join("a"), b"x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_in_place_when_the_directory_is_read_only() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if running_as_root() {
            return;
        }
        let fixture = Fixture::new("save-locked-dir");
        let directory = fixture.root.join("locked");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("open.txt");
        fs::write(&path, b"long old contents").unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();

        let result = save_buffer(&path, b"short");
        // Unlocked before asserting, so a failure still cleans up.
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        result.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"short");
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert!(leftovers(&directory, &["open.txt"]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_hard_links_sharing_the_new_contents() {
        let fixture = Fixture::new("save-hardlink");
        let path = fixture.root.join("one");
        let other = fixture.root.join("two");
        fs::write(&path, b"shared").unwrap();
        fs::hard_link(&path, &other).unwrap();

        save_buffer(&path, b"both see this").unwrap();
        assert_eq!(fs::read(&other).unwrap(), b"both see this");
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
    fn load_help_falls_back_to_the_embedded_copy_of_every_page() {
        let fixture = Fixture::new("help-embedded");
        let empty = vec![fixture.root.join("no-pages-here")];
        for page in HELP_PAGES {
            let (name, bytes) = load_help(page, &empty).unwrap();
            assert_eq!(name, PathBuf::from(page));
            assert_eq!(Some(bytes.as_slice()), embedded_help(page), "{page}");
            assert!(!bytes.is_empty(), "{page}");
        }
        assert_eq!(load_help("keys", &empty).unwrap().1, KEYS_HELP);
        assert_eq!(load_help("cmds", &empty).unwrap().1, CMDS_HELP);
    }

    #[test]
    fn load_help_prefers_a_page_on_disk_and_searches_in_order() {
        let fixture = Fixture::new("help-disk");
        let first = fixture.root.join("first");
        let second = fixture.root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(second.join("keys"), b"second keys").unwrap();
        fs::write(first.join("cmds"), b"first cmds").unwrap();
        fs::write(second.join("cmds"), b"second cmds").unwrap();
        let directories = [first.clone(), second.clone()];

        let (path, bytes) = load_help("keys", &directories).unwrap();
        assert_eq!(
            (path, bytes),
            (second.join("keys"), b"second keys".to_vec())
        );
        let (path, bytes) = load_help("cmds", &directories).unwrap();
        assert_eq!((path, bytes), (first.join("cmds"), b"first cmds".to_vec()));
        // Not on disk anywhere: the embedded copy.
        assert_eq!(load_help("general", &directories).unwrap().1, GENERAL_HELP);
    }

    #[test]
    fn load_help_rejects_names_outside_the_shipped_set_even_when_a_file_exists() {
        let fixture = Fixture::new("help-unknown");
        let help = fixture.root.join("help");
        fs::create_dir(&help).unwrap();
        fs::write(help.join("extra"), b"not documentation").unwrap();
        fs::write(fixture.root.join("secret"), b"private").unwrap();
        let directories = [help];
        for page in ["extra", "../secret", "", "General"] {
            assert!(
                matches!(load_help(page, &directories), Err(HelpError::UnknownPage)),
                "{page:?}"
            );
        }
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
