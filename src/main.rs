use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cano_fresh::app::{App, AppEffect};
use cano_fresh::cli::{CliError, parse};
use cano_fresh::config::{load as load_config, load_or_default};
use cano_fresh::io::{help_page, load_buffer, save_buffer};
use cano_fresh::process::run_shell;
use cano_fresh::render::{RenderOptions, Scroll, draw};
use cano_fresh::syntax::{SyntaxConfig, load as load_syntax};
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

    let showing_help = cli.help_page.is_some();
    let filename = if showing_help {
        // The runtime environment wins so an installed binary can be pointed
        // at relocated help pages; the compile-time value and the in-repo
        // fallback cover installed and development builds.
        let directory = std::env::var_os("CANO_HELP_DIR")
            .map(PathBuf::from)
            .or_else(|| option_env!("CANO_HELP_DIR").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("docs/help"));
        help_page(&directory, "general").ok_or_else(|| {
            "Failed to open help page. Check for typos or if you installed cano properly."
                .to_owned()
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
    app.editor.indent = app.commands.indent.max(0) as usize;

    // The `.cyntax` palette lives next to the configuration file, wherever
    // that configuration came from.
    let syntax: Option<SyntaxConfig> = if app.commands.syntax != 0 {
        filename
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| {
                config_path.parent().and_then(|parent| {
                    load_syntax(&parent.join(format!("{extension}.cyntax"))).ok()
                })
            })
    } else {
        None
    };

    let mut terminal = TerminalSession::start().map_err(|error| error.to_string())?;
    let mut message_deadline = None::<Instant>;
    let mut scroll = Scroll::default();
    loop {
        let prompt = String::from_utf8_lossy(&app.prompt);
        let count = String::from_utf8_lossy(&app.count);
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
                        count: &count,
                        explorer: app.explorer.as_ref(),
                        syntax: (app.commands.syntax != 0)
                            .then_some(syntax.as_ref())
                            .flatten(),
                        message: app.commands.message.as_deref(),
                        filename: &filename,
                        saved: app.saved,
                    },
                    &mut scroll,
                );
            })
            .map_err(|error| error.to_string())?;

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
        if apply_effects(&mut app, effects)? {
            return Ok(0);
        }
    }
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
            AppEffect::Save(path) => match save_buffer(&path, &app.editor.buffer.data) {
                Ok(()) => app.mark_saved(),
                Err(error) => {
                    app.set_message(format!("Could not write {}: {error}", path.display()));
                    app.commands.quit = false;
                    save_failed = true;
                }
            },
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
        }
    }
    Ok(quit)
}

#[cfg(test)]
mod tests {
    use super::*;
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
