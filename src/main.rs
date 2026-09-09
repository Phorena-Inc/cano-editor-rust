use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cano_fresh::app::{App, AppEffect, Jump};
use cano_fresh::backup;
use cano_fresh::cli::{CliError, parse};
use cano_fresh::config::{load as load_config, load_or_default};
use cano_fresh::io::{help_directories, help_page, load_buffer, save_buffer};
use cano_fresh::process::run_shell;
use cano_fresh::recent::Recent;
use cano_fresh::render::{RenderOptions, draw};
use cano_fresh::syntax::{Language, SyntaxConfig, load as load_syntax};
use cano_fresh::terminal::TerminalSession;

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
    let filename = if showing_help {
        // The runtime environment wins so an installed binary can be pointed
        // at relocated help pages; the remaining candidates cover installed
        // and development builds wherever they are run from.
        let runtime = std::env::var_os("CANO_HELP_DIR");
        let executable = std::env::current_exe().ok();
        let directories = help_directories(
            runtime.as_deref(),
            option_env!("CANO_HELP_DIR"),
            executable.as_deref(),
        );
        directories
            .iter()
            .find_map(|directory| help_page(directory, "general"))
            .ok_or_else(|| {
                // Naming the directories searched turns "check for typos" into
                // something the reader can act on: set CANO_HELP_DIR, or put
                // the pages where one of these points.
                let searched = directories
                    .iter()
                    .map(|directory| format!("\n  {}", directory.display()))
                    .collect::<String>();
                format!(
                    "Failed to open help page. Check for typos or if you installed cano \
                     properly. Searched:{searched}"
                )
            })?
    } else {
        PathBuf::from(cli.filename.as_deref().unwrap_or("out.txt"))
    };
    let bytes = match load_buffer(&filename) {
        Ok(bytes) => bytes,
        // A missing file starts as an empty buffer and is created on save.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !showing_help => Vec::new(),
        Err(error) => return Err(format!("Could not open {}: {error}", filename.display())),
    };
    let mut app = App::new(bytes, filename.clone());
    app.readonly = showing_help;

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
        // Exit statuses are one byte; wide codes saturate instead of
        // silently wrapping (256 would otherwise report success).
        return Ok(exit.code.clamp(0, 255) as u8);
    }
    // Only slots the configuration actually set override the editor's
    // built-in defaults.
    if let Some(syntax) = config.syntax {
        app.commands.syntax = syntax;
    }
    if let Some(auto_indent) = config.auto_indent {
        app.commands.auto_indent = auto_indent;
    }
    if let Some(relative) = config.relative {
        app.commands.relative = relative;
    }
    if let Some(indent) = config.indent {
        app.commands.indent = indent;
    }
    if let Some(undo_size) = config.undo_size {
        app.commands.undo_size = undo_size;
    }
    if let Some(cursorline) = config.cursorline {
        app.commands.cursorline = cursorline;
    }
    if let Some(mouse) = config.mouse {
        app.commands.mouse = mouse;
    }
    if let Some(backup) = config.backup {
        app.commands.backup = backup;
    }
    if let Some(list) = config.list {
        app.commands.list = list;
    }
    app.editor.indent = app.commands.indent.max(0) as usize;

    // Anything the configuration asked to run, in the order it asked. These
    // come last so a command can override a slot set above it.
    for line in &config.commands {
        let effects = app.run_command(line);
        if apply_effects(&mut app, effects)? {
            return Ok(0);
        }
    }

    // The recent list lives beside the effective configuration file, wherever
    // that configuration came from, the same way `.cyntax` palettes do.
    if let Some(directory) = config_path.parent() {
        let list = directory.join("recent");
        app.recent = Recent::load(&list);
        app.recent_path = Some(list);
    }
    // Built-in help pages are documentation, not the user's own files, so they
    // stay out of the history.
    if !showing_help {
        app.record_recent(&filename);
    }

    let mut palette = Palette::new(config_path);

    let mut terminal = TerminalSession::start().map_err(|error| error.to_string())?;
    let mut message_deadline = None::<Instant>;
    loop {
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
                        explorer: app.explorer.as_ref(),
                        recent: app.recent_open.then_some(&app.recent),
                        syntax,
                        markdown: app.markdown,
                        message: app.commands.message.as_deref(),
                        filename: &filename,
                        saved: app.saved,
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
        let input = match message_deadline {
            Some(deadline) => terminal
                .read_input_before(deadline)
                .map_err(|error| error.to_string())?,
            None => Some(terminal.read_input().map_err(|error| error.to_string())?),
        };
        let Some(input) = input else {
            message_deadline = None;
            app.commands.message = None;
            continue;
        };

        let effects = app.handle(input);
        // Suspending needs the terminal, which the effect applier does not
        // have, and it has to happen before anything is drawn again.
        if effects.contains(&AppEffect::Suspend) {
            terminal.suspend().map_err(|error| error.to_string())?;
        }
        let quit = apply_effects(&mut app, effects)?;
        // A pane held back by the save prompt opens only now that the write
        // has actually been applied.
        app.open_pending_pane();
        if quit {
            return Ok(0);
        }
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

fn apply_effects(app: &mut App, effects: Vec<AppEffect>) -> Result<bool, String> {
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
            AppEffect::Shell(command) => match run_shell(&command) {
                Ok(Some(output)) => {
                    app.set_message(String::from_utf8_lossy(&output).into_owned());
                }
                Ok(None) => {}
                Err(error) => app.set_message(format!("Shell command failed: {error}")),
            },
            AppEffect::Quit if !save_failed => quit = true,
            AppEffect::Quit => {}
            // Handled by the caller, which is where the terminal lives.
            AppEffect::Suspend => {}
        }
    }
    Ok(quit)
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
        assert!(!app.saved);
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
        )
        .unwrap();

        assert!(quit);
        assert!(app.saved);
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

        let quit = apply_effects(&mut app, effects).unwrap();

        assert!(!quit);
        assert!(!app.saved);
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
