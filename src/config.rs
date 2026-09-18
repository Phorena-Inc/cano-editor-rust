//! Lua configuration adapter.
//!
//! Cano configurations are trusted programs: compatibility requires Lua's
//! complete standard library, including the `io`, `os`, `package`, and
//! `debug` libraries.  All editor effects are nevertheless returned as data;
//! in particular, the Lua `exit` callback records an [`ExitRequest`] instead
//! of terminating the Rust test or application process itself.

use mlua::{Lua, Table, Value};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A process outcome requested by a configuration program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitRequest {
    pub code: i64,
    pub message: String,
}

/// The five source-compatible integer configuration slots.
///
/// Despite the integer representation, Lua updates a slot only when the
/// corresponding table value is a Boolean.  `false` becomes zero and `true`
/// becomes one; numbers and all other Lua types are ignored.  A slot the
/// configuration never set stays `None` so the editor's built-in defaults
/// survive a partial (or absent) configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LuaConfig {
    pub syntax: Option<i64>,
    pub relative: Option<i64>,
    pub auto_indent: Option<i64>,
    pub indent: Option<i64>,
    pub undo_size: Option<i64>,
    pub cursorline: Option<i64>,
    pub mouse: Option<i64>,
    pub backup: Option<i64>,
    pub list: Option<i64>,
    /// Command lines the configuration asked to run at startup, in the order
    /// it asked. Anything spelled as a `:` command can be configured this way
    /// without needing a slot of its own.
    pub commands: Vec<Vec<u8>>,
    pub exit: Option<ExitRequest>,
}

/// A failure to read or execute a Lua configuration.
#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Lua(mlua::Error),
    Poisoned,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Self::Lua(source) => write!(f, "Lua configuration error: {source}"),
            Self::Poisoned => f.write_str("Lua configuration state was poisoned"),
        }
    }
}

impl Error for ConfigError {}

impl From<mlua::Error> for ConfigError {
    fn from(value: mlua::Error) -> Self {
        Self::Lua(value)
    }
}

fn boolean_slot(table: &Table, name: &str) -> mlua::Result<Option<i64>> {
    Ok(match table.get::<Value>(name)? {
        Value::Boolean(value) => Some(i64::from(value)),
        _ => None,
    })
}

fn with_config<T>(
    state: &Arc<Mutex<LuaConfig>>,
    operation: impl FnOnce(&mut LuaConfig) -> T,
) -> mlua::Result<T> {
    let mut config = state
        .lock()
        .map_err(|_| mlua::Error::RuntimeError("configuration state was poisoned".into()))?;
    Ok(operation(&mut config))
}

fn evaluate_named(source: &[u8], name: &str) -> Result<LuaConfig, ConfigError> {
    // SAFETY: Cano configuration is explicitly a trusted, unrestricted
    // privilege boundary.  `unsafe_new` is the mlua constructor that loads
    // every Lua standard library, including `debug`, matching that contract.
    let lua = unsafe { Lua::unsafe_new() };
    let state = Arc::new(Mutex::new(LuaConfig::default()));
    let setup_state = Arc::clone(&state);

    let setup = lua.create_function(move |lua, table: Table| {
        with_config(&setup_state, |config| {
            for (name, slot) in [
                ("syntax", &mut config.syntax),
                ("relative", &mut config.relative),
                ("auto_indent", &mut config.auto_indent),
                ("indent", &mut config.indent),
                ("undo_size", &mut config.undo_size),
                ("cursorline", &mut config.cursorline),
                ("mouse", &mut config.mouse),
                ("backup", &mut config.backup),
                ("list", &mut config.list),
            ] {
                *slot = boolean_slot(&table, name)?.or(*slot);
            }
            Ok::<_, mlua::Error>(())
        })??;

        let api = lua.create_table()?;
        let exit_state = Arc::clone(&setup_state);
        let exit = lua.create_function(
            move |_, (code, message): (i64, Option<mlua::LuaString>)| -> mlua::Result<()> {
                with_config(&exit_state, |config| {
                    if config.exit.is_none() {
                        config.exit = Some(ExitRequest {
                            code,
                            message: message
                                .map(|message| message.to_string_lossy())
                                .unwrap_or_default(),
                        });
                    }
                })?;

                // A real process exit does not return.  Raising here stops
                // ordinary Lua execution while the adapter converts the
                // recorded request back into a successful typed result.
                Err(mlua::Error::RuntimeError(
                    "Cano configuration requested exit".into(),
                ))
            },
        )?;
        api.set("exit", exit)?;

        let command_state = Arc::clone(&setup_state);
        let command = lua.create_function(move |_, line: mlua::LuaString| -> mlua::Result<()> {
            // Commands are byte strings like everything else Cano parses, so
            // a `listchars` glyph or a non-UTF-8 path survives the trip.
            let line = line.as_bytes().to_vec();
            with_config(&command_state, |config| config.commands.push(line))
        })?;
        api.set("command", command)?;
        Ok(api)
    })?;
    lua.globals().set("setup", setup)?;

    let result = lua.load(source).set_name(name).exec();
    let config = state.lock().map_err(|_| ConfigError::Poisoned)?.clone();
    if config.exit.is_some() {
        // The callback's deliberate non-return is not a configuration error.
        return Ok(config);
    }
    result.map_err(ConfigError::Lua)?;
    Ok(config)
}

/// Evaluates an in-memory Lua configuration.
pub fn evaluate(source: &[u8]) -> Result<LuaConfig, ConfigError> {
    evaluate_named(source, "init.lua")
}

/// Reads and evaluates a Lua configuration file.
pub fn load(path: &Path) -> Result<LuaConfig, ConfigError> {
    let source = fs::read(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    evaluate_named(&source, &path.to_string_lossy())
}

/// Reads and evaluates a Lua configuration file, or uses the built-in
/// defaults when the file does not exist.
pub fn load_or_default(path: &Path) -> Result<LuaConfig, ConfigError> {
    // `load` only reports `Io` for the read itself.
    match load(path) {
        Err(ConfigError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(LuaConfig::default())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn empty_setup_leaves_every_slot_unset() {
        let config = evaluate(b"setup({})").unwrap();
        assert_eq!(config, LuaConfig::default());
        assert_eq!(config.syntax, None);
        assert_eq!(config.indent, None);
    }

    #[test]
    fn only_booleans_update_every_slot() {
        let config = evaluate(
            br#"
                setup({
                    syntax = true,
                    relative = false,
                    auto_indent = true,
                    indent = 4,
                    undo_size = "32",
                    cursorline = true,
                    mouse = false,
                    backup = false
                })
            "#,
        )
        .unwrap();
        assert_eq!(config.syntax, Some(1));
        assert_eq!(config.relative, Some(0));
        assert_eq!(config.auto_indent, Some(1));
        assert_eq!(config.indent, None);
        assert_eq!(config.undo_size, None);
        assert_eq!(config.cursorline, Some(1));
        assert_eq!(config.mouse, Some(0));
        assert_eq!(config.backup, Some(0));
    }

    #[test]
    fn commands_are_collected_in_the_order_they_were_asked_for() {
        let config = evaluate(
            br#"
                local cano = setup({ list = true })
                cano.command("set listchars=eol:$")
                cano.command([[set listchars=tab:>\ ,trail:.]])
                cano.command("imap ;; <Esc>")
            "#,
        )
        .unwrap();

        assert_eq!(config.list, Some(1));
        assert_eq!(
            config.commands,
            [
                b"set listchars=eol:$".to_vec(),
                br"set listchars=tab:>\ ,trail:.".to_vec(),
                b"imap ;; <Esc>".to_vec(),
            ]
        );
    }

    #[test]
    fn a_configuration_that_asks_for_nothing_runs_no_commands() {
        assert!(evaluate(b"setup({})").unwrap().commands.is_empty());
    }

    #[test]
    fn repeated_partial_setup_retains_previous_boolean_values() {
        let config = evaluate(
            br#"
                setup({ syntax = true, indent = true })
                setup({ relative = true, indent = 8 })
                setup({ syntax = nil, undo_size = false })
            "#,
        )
        .unwrap();
        assert_eq!(config.syntax, Some(1));
        assert_eq!(config.relative, Some(1));
        assert_eq!(config.indent, Some(1));
        assert_eq!(config.undo_size, Some(0));
    }

    #[test]
    fn complete_standard_libraries_are_available() {
        let config = evaluate(
            br#"
                setup({
                    syntax = type(io.open) == "function",
                    relative = type(os.date) == "function",
                    auto_indent = type(package.searchpath) == "function",
                    indent = type(debug.getinfo) == "function",
                    undo_size = type(math.sqrt) == "function"
                })
            "#,
        )
        .unwrap();
        assert_eq!(
            (
                config.syntax,
                config.relative,
                config.auto_indent,
                config.indent,
                config.undo_size
            ),
            (Some(1), Some(1), Some(1), Some(1), Some(1))
        );
    }

    #[test]
    fn returned_exit_is_typed_and_stops_normal_execution() {
        let config = evaluate(
            br#"
                local cano = setup({ syntax = true })
                cano.exit(42, "configured stop")
                setup({ syntax = false })
            "#,
        )
        .unwrap();
        assert_eq!(config.syntax, Some(1));
        assert_eq!(
            config.exit,
            Some(ExitRequest {
                code: 42,
                message: "configured stop".into()
            })
        );
    }

    #[test]
    fn exit_code_is_not_prematurely_narrowed() {
        let config = evaluate(b"setup({}).exit(300, 'wide')").unwrap();
        assert_eq!(config.exit.unwrap().code, 300);
    }

    #[test]
    fn exit_without_a_message_is_accepted() {
        let config = evaluate(b"setup({}).exit(0)").unwrap();
        assert_eq!(
            config.exit,
            Some(ExitRequest {
                code: 0,
                message: String::new()
            })
        );
    }

    #[test]
    fn syntax_runtime_and_argument_errors_are_reported() {
        assert!(matches!(evaluate(b"setup({"), Err(ConfigError::Lua(_))));
        assert!(matches!(
            evaluate(b"setup('not a table')"),
            Err(ConfigError::Lua(_))
        ));
        assert!(matches!(
            evaluate(b"error('runtime')"),
            Err(ConfigError::Lua(_))
        ));
    }

    #[test]
    fn load_preserves_io_and_lua_failures() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let missing = std::env::temp_dir().join(format!("cano-missing-{unique}.lua"));
        assert!(matches!(load(&missing), Err(ConfigError::Io { .. })));
    }

    #[test]
    fn missing_file_uses_built_in_defaults() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let missing = std::env::temp_dir().join(format!("cano-default-{unique}.lua"));

        assert_eq!(load_or_default(&missing).unwrap(), LuaConfig::default());
    }
}
