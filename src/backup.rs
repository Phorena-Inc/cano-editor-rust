//! Backup copies of files that are about to be overwritten.
//!
//! A backup holds what was on disk *before* a save, which is the copy worth
//! having: if the buffer that just overwrote it was wrong, the previous
//! contents are the thing you want back. Backups live in a `.backups`
//! directory beside the file they belong to, the way vim's `backupdir=.`
//! default keeps them next to their original, so they travel with the project
//! and are found where anyone would look.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Directory backups are written to, beside the file they belong to.
pub const DIRECTORY: &str = ".backups";

/// How many backups of one file are kept before the oldest are dropped.
pub const KEEP: usize = 10;

/// The length of a `YYYYMMDD-HHMMSS` stamp.
const STAMP_LEN: usize = 15;

/// Copies what is currently on disk at `path` into its backup directory.
///
/// Returns where the copy went, or `None` when there was nothing to copy
/// because the file does not exist yet. A new file has no previous contents,
/// and refusing to save one for want of a backup would be absurd.
pub fn write(path: &Path) -> io::Result<Option<PathBuf>> {
    let Ok(contents) = fs::read(path) else {
        return Ok(None);
    };
    let Some(name) = path.file_name() else {
        return Ok(None);
    };
    let directory = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(DIRECTORY);
    fs::create_dir_all(&directory)?;

    let destination = destination(&directory, name, &stamp(SystemTime::now()));
    fs::write(&destination, contents)?;
    prune(&directory, name);
    Ok(Some(destination))
}

/// The first free backup name for `name` at `stamp`.
///
/// Two saves within the same second would otherwise lose the earlier copy.
/// The counter is appended, so it still sorts after the bare stamp and the
/// directory continues to read in the order it happened.
fn destination(directory: &Path, name: &OsStr, stamp: &str) -> PathBuf {
    let mut base = OsString::from(name);
    base.push(".");
    base.push(stamp);

    let mut candidate = directory.join(&base);
    // Bounded so a directory that refuses to accept anything cannot spin
    // here; past the cap the newest copy simply replaces the last one.
    for nth in 1..1000 {
        if !candidate.exists() {
            break;
        }
        let mut next = base.clone();
        next.push(format!(".{nth}"));
        candidate = directory.join(next);
    }
    candidate
}

/// Drops all but the newest [`KEEP`] backups of `name`.
///
/// Stamps sort the same way they read, so the oldest are simply the first.
/// A stale copy that refuses to be deleted is not a reason to fail a save.
fn prune(directory: &Path, name: &OsStr) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut existing: Vec<OsString> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|candidate| is_backup_of(candidate, name))
        .collect();
    if existing.len() <= KEEP {
        return;
    }
    existing.sort();
    for stale in &existing[..existing.len() - KEEP] {
        let _ = fs::remove_file(directory.join(stale));
    }
}

/// Whether `candidate` is a backup this module made of `name`.
///
/// The stamp is checked rather than just the prefix: backups of `a.txt` and
/// of `a.txt.orig` share a prefix and would otherwise prune each other.
fn is_backup_of(candidate: &OsStr, name: &OsStr) -> bool {
    let candidate = candidate.as_encoded_bytes();
    let Some(rest) = candidate.strip_prefix(name.as_encoded_bytes()) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(b".") else {
        return false;
    };
    if rest.len() < STAMP_LEN {
        return false;
    }
    let (stamp, tail) = rest.split_at(STAMP_LEN);
    let shaped = stamp[..8].iter().all(u8::is_ascii_digit)
        && stamp[8] == b'-'
        && stamp[9..].iter().all(u8::is_ascii_digit);
    let counted = tail.is_empty()
        || (tail.len() > 1 && tail[0] == b'.' && tail[1..].iter().all(u8::is_ascii_digit));
    shaped && counted
}

/// `YYYYMMDD-HHMMSS` in UTC, which sorts the same way it reads.
///
/// UTC rather than local time because the offset would need a timezone
/// database, and a backup ordering that shifts twice a year is worse than one
/// that is an hour off the wall clock.
fn stamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    )
}

/// Turns a count of days since the epoch into a civil date.
///
/// This is Howard Hinnant's `civil_from_days`, which is exact for the whole
/// proleptic Gregorian range and needs no calendar tables.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01, which puts the leap day at the end of
    // the year and makes every other month length regular.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-backup-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("fixture directory");
        root
    }

    #[test]
    fn a_backup_holds_what_the_save_is_about_to_replace() {
        let root = fixture("previous");
        let file = root.join("doc.txt");
        fs::write(&file, b"before").unwrap();

        let backup = write(&file).unwrap().expect("a backup");
        assert_eq!(fs::read(&backup).unwrap(), b"before");
        assert_eq!(backup.parent(), Some(root.join(DIRECTORY).as_path()));

        // The caller writes afterwards, so the copy keeps the old contents.
        fs::write(&file, b"after").unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"before");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_that_does_not_exist_yet_has_nothing_to_copy() {
        let root = fixture("absent");
        assert_eq!(write(&root.join("missing.txt")).unwrap(), None);
        // Nothing was created for it either.
        assert!(!root.join(DIRECTORY).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saves_within_one_second_do_not_overwrite_each_others_copies() {
        let root = fixture("collide");
        let file = root.join("doc.txt");

        for round in 0..3u8 {
            fs::write(&file, [round]).unwrap();
            write(&file).unwrap().expect("a backup");
        }
        let mut names: Vec<OsString> = fs::read_dir(root.join(DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names.len(), 3, "{names:?}");
        // The counter sorts after the bare stamp, so the directory still
        // reads in the order it happened.
        let contents: Vec<Vec<u8>> = names
            .iter()
            .map(|name| fs::read(root.join(DIRECTORY).join(name)).unwrap())
            .collect();
        assert_eq!(contents, [vec![0], vec![1], vec![2]]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_newest_copies_are_kept() {
        let root = fixture("prune");
        let directory = root.join(DIRECTORY);
        fs::create_dir_all(&directory).unwrap();
        // Stamps that sort the way they read, so the oldest are the first.
        for minute in 0..KEEP + 5 {
            fs::write(
                directory.join(format!("doc.txt.20260101-00{minute:02}00")),
                [u8::try_from(minute).unwrap()],
            )
            .unwrap();
        }
        // Something that is not a backup of this file must survive untouched.
        fs::write(directory.join("doc.txt.orig.20260101-000000"), b"x").unwrap();
        fs::write(directory.join("notes.txt.20260101-000000"), b"x").unwrap();

        prune(&directory, OsStr::new("doc.txt"));

        let mut kept: Vec<OsString> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| is_backup_of(name, OsStr::new("doc.txt")))
            .collect();
        kept.sort();
        assert_eq!(kept.len(), KEEP);
        assert_eq!(
            kept.first().map(|name| name.to_string_lossy().into_owned()),
            Some("doc.txt.20260101-000500".to_owned())
        );
        assert!(directory.join("doc.txt.orig.20260101-000000").exists());
        assert!(directory.join("notes.txt.20260101-000000").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_this_modules_own_copies_are_recognized() {
        let name = OsStr::new("a.txt");
        let backup = |candidate: &str| is_backup_of(OsStr::new(candidate), name);

        assert!(backup("a.txt.20260906-110900"));
        assert!(backup("a.txt.20260906-110900.7"));
        // A file that merely shares the prefix is a different file's backup,
        // and pruning one must not take the other with it.
        assert!(!backup("a.txt.orig.20260906-110900"));
        assert!(!backup("a.txt"));
        assert!(!backup("a.txt.notastamp"));
        assert!(!backup("a.txt.2026090-110900"));
        assert!(!backup("a.txt.20260906-110900."));
        assert!(!backup("b.txt.20260906-110900"));
    }

    #[test]
    fn stamps_read_and_sort_the_same_way() {
        let at = |seconds| stamp(UNIX_EPOCH + std::time::Duration::from_secs(seconds));

        assert_eq!(at(0), "19700101-000000");
        assert_eq!(at(86_399), "19700101-235959");
        // Both leap-day shapes, and the century that is not one.
        assert_eq!(at(951_782_400), "20000229-000000");
        assert_eq!(at(1_709_164_800), "20240229-000000");
        assert_eq!(at(4_102_444_800), "21000101-000000");
        // A clock before the epoch has no stamp to give, and must not panic.
        assert_eq!(stamp(UNIX_EPOCH - std::time::Duration::from_secs(1)), at(0));
    }
}
