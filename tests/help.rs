//! `:help [page]` and `:h`, driven through the public command entry point.
//!
//! The pages are searched for the way the binary searches for them, so a
//! test run from the checkout finds `docs/help` -- the same bytes the binary
//! embeds, which is what the assertions compare against either way.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cano_fresh::app::App;
use cano_fresh::io::{CMDS_HELP, GENERAL_HELP, KEYS_HELP};
use cano_fresh::syntax::Language;
use cano_fresh::terminal::Input;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory removed on drop, so a failed assertion still cleans up.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-help-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn message(app: &App) -> Option<&str> {
    app.commands.message.as_deref()
}

fn page_name(app: &App) -> String {
    app.filename
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Types `text` in Insert mode and leaves it again, so the buffer is dirty.
fn type_text(app: &mut App, text: &[u8]) {
    app.handle(Input::Byte(b'i'));
    for &byte in text {
        app.handle(Input::Byte(byte));
    }
    app.handle(Input::Byte(27));
}

#[test]
fn help_cmds_opens_the_page_read_only_with_no_display_layers() {
    // A markdown file, so the markdown layer starts on and has to be turned
    // off by the switch rather than merely never enabled.
    let mut app = App::new(b"# notes\n".to_vec(), PathBuf::from("notes.md"));
    assert!(app.markdown);

    assert!(app.run_command(b"help cmds").is_empty());
    assert!(app.readonly);
    assert_eq!(app.editor.buffer.data, CMDS_HELP);
    assert_eq!(page_name(&app), "cmds");
    assert!(!app.markdown);
    assert_eq!(Language::for_path(&app.filename), None);
    assert!(app.saved());
}

#[test]
fn bare_help_and_the_h_alias_open_general_and_h_takes_a_page() {
    let mut app = App::new(Vec::new(), PathBuf::from("a.txt"));
    app.run_command(b"help");
    assert_eq!(app.editor.buffer.data, GENERAL_HELP);
    assert_eq!(page_name(&app), "general");

    // From one help page to another: the guard has nothing to protect.
    app.run_command(b":h keys");
    assert_eq!(app.editor.buffer.data, KEYS_HELP);
    assert!(app.readonly);

    app.run_command(b"h");
    assert_eq!(app.editor.buffer.data, GENERAL_HELP);
}

#[test]
fn an_unknown_help_page_is_reported_and_leaves_the_buffer_alone() {
    let mut app = App::new(b"mine\n".to_vec(), PathBuf::from("a.txt"));
    for line in [&b"help bogus"[..], b"h ../Cargo.toml", b"help keys cmds"] {
        app.run_command(line);
        assert_eq!(app.editor.buffer.data, b"mine\n");
        assert!(!app.readonly);
    }
    app.run_command(b"help bogus");
    assert_eq!(message(&app), Some("No help page named bogus"));
}

#[test]
fn a_longer_verb_is_not_the_help_command() {
    let mut app = App::new(b"mine\n".to_vec(), PathBuf::from("a.txt"));
    app.run_command(b"helpx");
    assert_eq!(app.editor.buffer.data, b"mine\n");
    assert!(!app.readonly);
    assert!(!message(&app).unwrap_or("").starts_with("No help page"));
}

#[test]
fn help_refuses_unsaved_work_and_help_bang_discards_it() {
    let mut app = App::new(b"mine\n".to_vec(), PathBuf::from("a.txt"));
    type_text(&mut app, b"new ");
    assert!(!app.saved());
    let edited = app.editor.buffer.data.clone();

    app.run_command(b"help keys");
    assert_eq!(
        message(&app),
        Some("No write since last change (add ! to override)")
    );
    assert_eq!(app.editor.buffer.data, edited);
    assert!(!app.readonly);

    // A typo is reported as one, even over unsaved work.
    app.run_command(b"help kyes");
    assert_eq!(message(&app), Some("No help page named kyes"));

    app.run_command(b"help! keys");
    assert_eq!(app.editor.buffer.data, KEYS_HELP);
    assert!(app.readonly);

    let mut short = App::new(b"x".to_vec(), PathBuf::from("b.txt"));
    type_text(&mut short, b"y");
    short.run_command(b"h!");
    assert_eq!(short.editor.buffer.data, GENERAL_HELP);
}

#[test]
fn the_file_left_for_help_is_in_the_recent_list_and_the_page_is_not() {
    let scratch = Scratch::new();
    let file = scratch.0.join("edited.txt");
    std::fs::write(&file, b"text\n").unwrap();
    let other = scratch.0.join("other.txt");
    std::fs::write(&other, b"other\n").unwrap();

    let mut app = App::new(b"text\n".to_vec(), file.clone());
    // Another file was opened since, so the one being edited is not first.
    app.record_recent(&other);
    app.record_recent(&file);
    app.record_recent(&other);

    app.run_command(b"help");
    app.run_command(b"help cmds");
    let canonical = cano_fresh::io::canonicalize(&file).unwrap();
    assert_eq!(app.recent.paths.first(), Some(&canonical));
    // Help pages are documentation, never the user's own files.
    assert!(
        app.recent
            .paths
            .iter()
            .all(|path| !path.ends_with(Path::new("general")) && !path.ends_with("cmds"))
    );
}
