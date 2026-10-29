use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cano_fresh::app::{App, AppEffect, Jump};
use cano_fresh::assistant::Backend;
use cano_fresh::backup;
use cano_fresh::cli::{CliError, parse};
use cano_fresh::config::{load as load_config, load_or_default};
use cano_fresh::io::{
    HELP_PAGES, HelpError, help_search_directories, load_buffer, load_help, save_buffer,
};
use cano_fresh::process::{ShellJob, run_shell};
use cano_fresh::recent::Recent;
use cano_fresh::render::{RenderOptions, draw};
use cano_fresh::syntax::{Language, SyntaxConfig, load as load_syntax};
use cano_fresh::terminal::{TerminalSession, termination_signal};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, String> {
    let arguments = std::env::args().collect::<Vec<_>>();
    let cli = parse(&arguments).map_err(|error| match error {
        CliError::MissingConfigValue => {
            format!("usage: {} --config <init.lua> <filename>", arguments[0])
        }
        CliError::UnexpectedFlag => "Unexpected flag".to_owned(),
    })?;

    // Answered before the configuration is touched, so `--version` still
    // reports something with a broken init.lua or no home directory to put
    // one in.  It also wins over `--help`: a script that passes both wants a
    // line of text back, not an editor session waiting on a keystroke.
    if cli.version {
        println!("cano {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    let showing_help = cli.help_page.is_some();
    let (filename, bytes) = if let Some(page) = cli.help_page.as_deref() {
        // The runtime environment wins so an installed binary can be pointed
        // at relocated help pages; the remaining candidates cover installed
        // and development builds wherever they are run from, and the copy
        // built into the binary answers last.  `:help` searches the same way.
        match load_help(page, &help_search_directories()) {
            Ok(found) => found,
            Err(HelpError::UnknownPage) => {
                return Err(format!(
                    "No help page named {page}; pages: {}",
                    HELP_PAGES.join(", ")
                ));
            }
            Err(HelpError::Unreadable { path, error }) => {
                return Err(format!("Could not open {}: {error}", path.display()));
            }
        }
    } else {
        let filename = PathBuf::from(cli.filename.as_deref().unwrap_or("out.txt"));
        let bytes = match load_buffer(&filename) {
            Ok(bytes) => bytes,
            // A missing file starts as an empty buffer and is created on save.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("Could not open {}: {error}", filename.display())),
        };
        (filename, bytes)
    };
    let mut app = App::new(bytes, filename.clone());
    app.readonly = showing_help;
    // The assistant exists only when an API key does; without one its
    // button stays hidden and Ctrl-K just says how to turn it on.
    app.assistant.backend = Backend::from_env();

    let explicit_config = cli.config.is_some();
    let config_path = match cli.config {
        Some(path) => PathBuf::from(path),
        None => {
            // Windows shells define USERPROFILE rather than HOME.
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| {
                    "could not determine the home directory (HOME/USERPROFILE unset)".to_owned()
                })?;
            let directory = PathBuf::from(home).join(".config/cano");
            std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            directory.join("init.lua")
        }
    };
    let config = if explicit_config {
        load_config(&config_path)
    } else {
        load_or_default(&config_path)
    }
    .map_err(|error| format!("Failed to apply configuration: {error}"))?;
    if let Some(exit) = config.exit {
        println!(
            "Exiting as specified in the configuration, message: {}",
            exit.message
        );
        // Exit statuses are one byte.  A negative code is taken the way C
        // takes it, as its low byte, so `cano.exit(-1)` still reports
        // failure; a code wider than a byte saturates instead of silently
        // wrapping (256 would otherwise report success).
        return Ok(if exit.code < 0 {
            exit.code as u8
        } else {
            exit.code.min(255) as u8
        });
    }
    // Only slots the configuration actually set override the editor's
    // built-in defaults.
    let commands = &mut app.commands;
    commands.syntax = config.syntax.unwrap_or(commands.syntax);
    commands.auto_indent = config.auto_indent.unwrap_or(commands.auto_indent);
    commands.relative = config.relative.unwrap_or(commands.relative);
    commands.indent = config.indent.unwrap_or(commands.indent);
    commands.undo_size = config.undo_size.unwrap_or(commands.undo_size);
    commands.cursorline = config.cursorline.unwrap_or(commands.cursorline);
    commands.mouse = config.mouse.unwrap_or(commands.mouse);
    commands.backup = config.backup.unwrap_or(commands.backup);
    commands.list = config.list.unwrap_or(commands.list);
    commands.wrap = config.wrap.unwrap_or(commands.wrap);
    app.editor.indent = app.commands.indent.max(0) as usize;

    // Anything the configuration asked to run, in the order it asked. These
    // come last so a command can override a slot set above it.
    //
    // There is no screen yet, so a `!command` among them runs to completion
    // before the next line, the way it would from a script.
    for line in &config.commands {
        let effects = app.run_command(line);
        if apply_effects(&mut app, effects, None) {
            return Ok(0);
        }
    }

    // The recent list lives beside the effective configuration file, wherever
    // that configuration came from, the same way `.cyntax` palettes do.
    if let Some(directory) = config_path.parent() {
        let list = directory.join("recent");
        app.recent = Recent::load(&list);
        app.recent_path = Some(list);
        // So does the assistant's chat, which carries over between runs
        // until Ctrl-L clears it.
        if app.assistant.available() {
            app.assistant.restore(directory.join("ai-history.json"));
        }
    }
    // Built-in help pages are documentation, not the user's own files, so they
    // stay out of the history.
    if !showing_help {
        app.record_recent(&filename);
    }

    let mut palette = Palette::new(config_path);

    let mut terminal = TerminalSession::start().map_err(|error| error.to_string())?;
    let mut message_deadline = None::<Instant>;
    let mut shell = Background::default();
    loop {
        // SIGTERM or SIGHUP: the handler only raised a flag, and this is the
        // first place it is safe to act on it.  Returning drops the session,
        // which hands the terminal back; the buffer is deliberately not
        // saved -- a signal is not the user asking for a write.
        if let Some(signal) = termination_signal() {
            return Ok(signal_status(signal));
        }
        shell.poll(&mut app);
        // Mouse reporting follows the option, so `:set-var mouse 0` hands the
        // terminal its own selection back without a restart.
        terminal
            .set_mouse(app.commands.mouse != 0)
            .map_err(|error| error.to_string())?;
        let syntax = palette.refresh(&app.filename, app.commands.syntax != 0);
        // The wheel scrolls by writing to the viewport, so the frame starts
        // from whatever the application left there.
        let mut viewport = app.viewport;
        let prompt = String::from_utf8_lossy(&app.prompt);
        let pending = app.pending_hint();
        // Borrowed across the draw closure, so the labels have to outlive it.
        let jump = match &app.jump {
            Some(Jump::Target { targets, typed }) => Some((targets.as_slice(), typed.as_slice())),
            _ => None,
        };
        let filename = app
            .filename
            .file_name()
            .unwrap_or(app.filename.as_os_str())
            .to_string_lossy();
        terminal
            .update_cursor(app.editor.mode)
            .map_err(|error| error.to_string())?;
        terminal
            .terminal
            .draw(|frame| {
                draw(
                    frame,
                    &app.editor,
                    RenderOptions {
                        relative_numbers: app.commands.relative != 0,
                        prompt: &prompt,
                        prompt_cursor: app.prompt_cursor,
                        pending: &pending,
                        jump,
                        highlight: &app.highlight,
                        cursorline: app.commands.cursorline != 0,
                        list: (app.commands.list != 0).then_some(&app.commands.listchars),
                        wrap: app.commands.wrap != 0,
                        explorer: app.explorer.as_ref(),
                        recent: app.recent_open.then_some(&app.recent),
                        history: app
                            .history_open
                            .map(|_| (app.history_list.as_slice(), app.history_cursor)),
                        syntax,
                        markdown: app.markdown,
                        message: app.commands.message.as_deref(),
                        filename: &filename,
                        saved: app.saved(),
                        assistant: app.assistant.available().then_some(&app.assistant),
                    },
                    &mut viewport,
                );
            })
            .map_err(|error| error.to_string())?;

        app.viewport = viewport;
        app.mark_rendered();

        if app.message_pending {
            app.message_pending = false;
            message_deadline = Some(Instant::now() + Duration::from_secs(1));
        }
        // While the assistant is waiting on the network or a shell command
        // the loop wakes up regularly to collect the result and tick the
        // elapsed time, instead of sleeping until the next key.
        // A `:!` command in the background is collected the same way.
        let poll = (app.assistant.busy() || shell.job.is_some())
            .then(|| Instant::now() + Duration::from_millis(200));
        let deadline = [message_deadline, poll].into_iter().flatten().min();
        let input = match deadline {
            Some(deadline) => terminal.read_input_before(deadline),
            None => terminal.read_input().map(Some),
        };
        // A read cut short by SIGTERM or SIGHUP -- or failing because the
        // terminal hung up -- is an exit for the signal, not an error to
        // print on a terminal that may no longer be there.
        let input = match input {
            Ok(input) => input,
            Err(error) => match termination_signal() {
                Some(signal) => return Ok(signal_status(signal)),
                None => return Err(error.to_string()),
            },
        };
        let Some(input) = input else {
            if message_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                message_deadline = None;
                app.commands.message = None;
            }
            let effects = app.assistant_poll();
            app.assistant.persist();
            if apply_effects(&mut app, effects, Some(&mut shell)) {
                return Ok(0);
            }
            continue;
        };

        let mut effects = app.handle(input);
        effects.extend(app.assistant_poll());
        app.assistant.persist();
        // Suspending needs the terminal, which the effect applier does not
        // have, and it has to happen before anything is drawn again.
        if effects.contains(&AppEffect::Suspend) {
            terminal.suspend().map_err(|error| error.to_string())?;
        }
        // Ctrl-L: throw away what the terminal is showing so the next frame
        // is painted from nothing.  Ratatui otherwise sends only the cells
        // that changed, which cannot repair a screen something else wrote
        // over -- which is the whole reason to ask for a redraw.
        if effects.contains(&AppEffect::Redraw) {
            terminal.redraw().map_err(|error| error.to_string())?;
            message_deadline = None;
        }
        let quit = apply_effects(&mut app, effects, Some(&mut shell));
        // A pane held back by the save prompt opens only now that the write
        // has actually been applied.
        app.open_pending_pane();
        if quit {
            return Ok(0);
        }
    }
}

/// The exit status for a run ended by `signal`: 128 plus its number, the
/// way a shell reports a process the signal killed.
fn signal_status(signal: i32) -> u8 {
    // Pre: only SIGTERM and SIGHUP are caught, both well below 128.
    debug_assert!((1..128).contains(&signal));
    u8::try_from(128 + signal).unwrap_or(u8::MAX)
}

/// The `:!` command running in the background, if there is one.
///
/// Only one runs at a time: two finishing together would race for the single
/// message line, and the second result would hide the first.
#[derive(Debug, Default)]
struct Background {
    job: Option<ShellJob>,
    /// The "Running: …" text last put on the message line, so it is updated
    /// and taken down only while it is still what is shown -- a message
    /// that replaced it in the meantime is left alone.
    shown: Option<String>,
}

impl Background {
    fn start(&mut self, app: &mut App, command: &[u8]) {
        if let Some(job) = &self.job {
            app.set_message(format!(
                "A shell command is already running: {}",
                job.command
            ));
            return;
        }
        match ShellJob::start(command) {
            // The note goes up at once, over whatever was showing: the
            // command just typed is the news.
            Ok(job) => {
                let note = job.running_message();
                app.commands.message = Some(note.clone());
                self.shown = Some(note);
                self.job = Some(job);
            }
            Err(error) => app.set_message(format!("Shell command failed: {error}")),
        }
    }

    /// Collects a finished command's result, or keeps the running note on
    /// the message line while it is still going.
    ///
    /// The note is written straight into the message slot rather than with
    /// `set_message`, which would arm the one-second timeout and take it
    /// down while the command is still running.  When another message has
    /// expired or been cleared it simply comes back.
    fn poll(&mut self, app: &mut App) {
        let Some(job) = &self.job else {
            return;
        };
        let showing = app.commands.message.is_some() && app.commands.message == self.shown;
        match job.poll() {
            None => {
                if showing || app.commands.message.is_none() {
                    let note = job.running_message();
                    app.commands.message = Some(note.clone());
                    self.shown = Some(note);
                }
            }
            Some(result) => {
                self.job = None;
                self.shown = None;
                if showing {
                    app.commands.message = None;
                }
                match result {
                    Ok(Some(summary)) => app.set_message(summary),
                    Ok(None) => {}
                    Err(error) => app.set_message(format!("Shell command failed: {error}")),
                }
            }
        }
        // Post: a note is only remembered while there is a job it describes.
        debug_assert!(self.job.is_some() || self.shown.is_none());
    }
}

/// Keeps the syntax palette in step with the file being edited.
///
/// The buffer can be replaced at any time by the explorer or the recent-file
/// picker, and `syntax` can be toggled at runtime, so the palette cannot be
/// settled once at startup: doing that leaves a `.rs` file opened from a `.md`
/// one with markdown's highlighting, which is to say none. It is reloaded only
/// when the file or the flag actually changes, which keeps the `.cyntax`
/// lookup off every frame.
struct Palette {
    config_path: PathBuf,
    current: Option<SyntaxConfig>,
    source: Option<(PathBuf, bool)>,
}

impl Palette {
    fn new(config_path: PathBuf) -> Self {
        Self {
            config_path,
            current: None,
            source: None,
        }
    }

    fn refresh(&mut self, filename: &Path, enabled: bool) -> Option<&SyntaxConfig> {
        let stale = self
            .source
            .as_ref()
            .is_none_or(|(path, was)| path != filename || *was != enabled);
        if stale {
            self.current = highlighting(filename, &self.config_path, enabled);
            self.source = Some((filename.to_path_buf(), enabled));
        }
        self.current.as_ref()
    }
}

/// Chooses the highlighting for one file.
///
/// A `<extension>.cyntax` palette beside the effective configuration file wins,
/// so a user's own colors keep overriding the built-ins; otherwise Cano uses
/// its built-in lists for the languages it knows. An extension with neither is
/// left uncolored.
fn highlighting(filename: &Path, config_path: &Path, enabled: bool) -> Option<SyntaxConfig> {
    if !enabled {
        return None;
    }
    // Dotfiles such as `.vimrc` carry their language in the file name, so the
    // whole path decides it.
    let language = Language::for_path(filename);
    // A `.cyntax` palette is keyed by extension, so a file without one still
    // gets built-in highlighting but cannot be given a custom palette -- and
    // must not be handed whatever a literal `.cyntax` file happens to hold.
    let palette = filename
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| {
            config_path.parent().and_then(|parent| {
                // The palette is parsed for the detected language so its
                // omitted `k`/`t` word lists fall back to that language
                // rather than always to C.
                load_syntax(
                    &parent.join(format!("{extension}.cyntax")),
                    language.unwrap_or_default(),
                )
                .ok()
            })
        });
    palette.or_else(|| language.map(SyntaxConfig::for_language))
}

/// Applies what a key or command asked for.  `shell` is where a `:!` command
/// runs in the background; without one -- before the terminal is up -- it
/// runs to completion here.
fn apply_effects(app: &mut App, effects: Vec<AppEffect>, shell: Option<&mut Background>) -> bool {
    let mut shell = shell;
    let mut quit = false;
    let mut save_failed = false;
    for effect in effects {
        match effect {
            AppEffect::Save(_) if app.readonly => {
                app.set_message("Buffer is read-only");
                app.commands.quit = false;
                save_failed = true;
            }
            AppEffect::Save(path) => {
                // The copy is taken before the write, because what is worth
                // keeping is the version about to be replaced. A backup that
                // cannot be written is worth saying so, but not worth
                // refusing the save over: the unsaved edit is what is at risk.
                if app.commands.backup != 0
                    && let Err(error) = backup::write(&path)
                {
                    app.set_message(format!("Could not back up {}: {error}", path.display()));
                }
                match save_buffer(&path, &app.editor.buffer.data) {
                    Ok(()) => app.mark_saved(),
                    Err(error) => {
                        app.set_message(format!("Could not write {}: {error}", path.display()));
                        app.commands.quit = false;
                        save_failed = true;
                    }
                }
            }
            // A shell that cannot even be spawned is a status message, not a
            // reason to exit the editor and discard unsaved changes.
            AppEffect::Shell(command) => match shell.as_deref_mut() {
                Some(background) => background.start(app, &command),
                None => match run_shell(&command) {
                    Ok(Some(summary)) => app.set_message(summary),
                    Ok(None) => {}
                    Err(error) => app.set_message(format!("Shell command failed: {error}")),
                },
            },
            AppEffect::Quit if !save_failed => quit = true,
            AppEffect::Quit => {}
            // Both are handled by the caller, which is where the terminal
            // lives.
            AppEffect::Suspend | AppEffect::Redraw => {}
        }
    }
    quit
}

#[cfg(test)]
mod tests {
    use super::*;
    use cano_fresh::syntax::Rgb;
    use cano_fresh::terminal::Input;

    fn make_dirty(app: &mut App) {
        app.handle(Input::Byte(b'i'));
        app.handle(Input::Byte(b'X'));
        app.handle(Input::Escape);
        assert!(!app.saved());
    }

    fn ex(app: &mut App, command: &[u8]) -> Vec<AppEffect> {
        app.handle(Input::Byte(b':'));
        for &byte in command {
            app.handle(Input::Byte(byte));
        }
        app.handle(Input::Enter)
    }

    fn fixture(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "cano-fresh-main-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn the_palette_follows_the_file_rather_than_the_one_opened_at_startup() {
        let root = fixture("palette");
        std::fs::create_dir(&root).unwrap();
        let mut palette = Palette::new(root.join("init.lua"));
        let language = |palette: &mut Palette, name: &str, enabled: bool| {
            palette
                .refresh(Path::new(name), enabled)
                .map(|config| config.language)
        };

        // Markdown has no palette of its own, and opening a Rust file from it
        // has to pick Rust up rather than keep markdown's nothing.
        assert_eq!(language(&mut palette, "a/doc.md", true), None);
        assert_eq!(
            language(&mut palette, "a/code.rs", true),
            Some(Language::Rust)
        );
        assert_eq!(language(&mut palette, "a/doc.md", true), None);
        assert_eq!(
            language(&mut palette, "a/code.rs", true),
            Some(Language::Rust)
        );
        assert_eq!(
            language(&mut palette, "a/main.py", true),
            Some(Language::Python)
        );

        // The runtime `syntax` toggle is followed too, in both directions,
        // even though it was on when the palette was first asked for.
        assert_eq!(language(&mut palette, "a/main.py", false), None);
        assert_eq!(
            language(&mut palette, "a/main.py", true),
            Some(Language::Python)
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn highlighting_prefers_a_palette_and_falls_back_to_the_built_in_language() {
        let root = fixture("syntax");
        std::fs::create_dir(&root).unwrap();
        let config_path = root.join("init.lua");

        // A known extension with no palette beside it gets the built-in lists.
        let rust = highlighting(Path::new("a/b.rs"), &config_path, true).unwrap();
        assert_eq!(rust.language, Language::Rust);
        assert_eq!(rust.keyword.color, Rgb::new(255, 0, 0));
        let python = highlighting(Path::new("a/b.py"), &config_path, true).unwrap();
        assert_eq!(python.language, Language::Python);
        assert_eq!(
            highlighting(Path::new("a/b.cpp"), &config_path, true)
                .unwrap()
                .language,
            Language::Cpp
        );

        // A dotfile with no extension is still recognized by its name.
        assert_eq!(
            highlighting(Path::new("/home/u/.vimrc"), &config_path, true)
                .unwrap()
                .language,
            Language::Vim
        );
        assert_eq!(
            highlighting(Path::new(".bashrc"), &config_path, true)
                .unwrap()
                .language,
            Language::Bash
        );
        assert_eq!(
            highlighting(Path::new("a/init.lua"), &config_path, true)
                .unwrap()
                .language,
            Language::Lua
        );
        assert_eq!(
            highlighting(Path::new("a/data.json"), &config_path, true)
                .unwrap()
                .language,
            Language::Json
        );

        // An unknown extension, and syntax turned off, stay uncolored.
        assert!(highlighting(Path::new("a/b.go"), &config_path, true).is_none());
        assert!(highlighting(Path::new("plain"), &config_path, true).is_none());
        std::fs::write(root.join(".cyntax"), b"k,1,2,3.").unwrap();
        assert!(highlighting(Path::new("plain"), &config_path, true).is_none());
        assert!(highlighting(Path::new("a/b.rs"), &config_path, false).is_none());

        // A palette beside the config still wins, and its omitted word lists
        // fall back to the detected language rather than to C.
        std::fs::write(root.join("rs.cyntax"), b"k,1,2,3.").unwrap();
        let configured = highlighting(Path::new("a/b.rs"), &config_path, true).unwrap();
        assert_eq!(configured.keyword.color, Rgb::new(1, 2, 3));
        assert!(configured.keyword.words.contains(&b"fn".to_vec()));

        std::fs::remove_dir_all(root).unwrap();
    }

    /// Polls `shell` the way the main loop does until its job is collected.
    #[cfg(unix)]
    fn wait_for(app: &mut App, shell: &mut Background) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while shell.job.is_some() {
            assert!(
                Instant::now() < deadline,
                "the shell command never finished"
            );
            std::thread::sleep(Duration::from_millis(10));
            shell.poll(app);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_command_runs_in_the_background_and_reports_its_status() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        let mut shell = Background::default();

        let effects = ex(&mut app, b"!sleep 0.2; echo 'error: foo' >&2; exit 2");
        assert!(!apply_effects(&mut app, effects, Some(&mut shell)));
        // Back at once, with the command still going and the editor saying so.
        assert!(shell.job.is_some());
        assert_eq!(
            app.commands.message.as_deref(),
            Some("Running: sleep 0.2; echo 'error: foo' >&2; exit 2")
        );
        // An expired message does not lose the note while the job runs.
        app.commands.message = None;
        shell.poll(&mut app);
        assert!(
            app.commands
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with("Running: "))
        );

        wait_for(&mut app, &mut shell);
        assert_eq!(app.commands.message.as_deref(), Some("[exit 2] error: foo"));
        assert!(app.message_pending, "the result is timed like any message");

        // A quiet success takes the note down and says nothing more.
        let effects = ex(&mut app, b"!:");
        apply_effects(&mut app, effects, Some(&mut shell));
        wait_for(&mut app, &mut shell);
        assert_eq!(app.commands.message, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_second_shell_command_waits_for_the_first() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        let mut shell = Background::default();

        let effects = ex(&mut app, b"!sleep 0.3; echo first");
        apply_effects(&mut app, effects, Some(&mut shell));
        let effects = ex(&mut app, b"!echo second");
        apply_effects(&mut app, effects, Some(&mut shell));
        assert_eq!(
            app.commands.message.as_deref(),
            Some("A shell command is already running: sleep 0.3; echo first")
        );

        wait_for(&mut app, &mut shell);
        assert_eq!(app.commands.message.as_deref(), Some("first"));
    }

    #[cfg(unix)]
    #[test]
    fn a_startup_shell_command_runs_to_completion_before_returning() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        let effects = app.run_command(b"!echo configured");
        assert_eq!(effects, [AppEffect::Shell(b"echo configured".to_vec())]);
        apply_effects(&mut app, effects, None);
        assert_eq!(app.commands.message.as_deref(), Some("configured"));
    }

    #[test]
    fn a_signal_exit_status_is_128_plus_the_signal() {
        assert_eq!(signal_status(1), 129);
        assert_eq!(signal_status(15), 143);
    }

    #[test]
    fn successful_write_quits_only_after_marking_the_buffer_saved() {
        let root = fixture("save");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("file");
        let mut app = App::new(b"text".to_vec(), path.clone());
        make_dirty(&mut app);

        let quit = apply_effects(
            &mut app,
            vec![AppEffect::Save(path.clone()), AppEffect::Quit],
            None,
        );

        assert!(quit);
        assert!(app.saved());
        assert_eq!(std::fs::read(path).unwrap(), b"Xtext");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_write_keeps_the_dirty_buffer_open_and_reports_the_error() {
        let root = fixture("save-error");
        let path = root.join("missing").join("file");
        let mut app = App::new(b"text".to_vec(), path.clone());
        make_dirty(&mut app);

        let effects = ex(&mut app, b"wq");
        assert_eq!(effects, [AppEffect::Save(path.clone()), AppEffect::Quit]);
        assert!(app.commands.quit);

        let quit = apply_effects(&mut app, effects, None);

        assert!(!quit);
        assert!(!app.saved());
        assert!(!app.commands.quit);
        assert!(app.message_pending);
        assert!(
            app.commands
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with("Could not write "))
        );
        assert_eq!(ex(&mut app, b"w"), [AppEffect::Save(path)]);
    }
}
