use std::error::Error;
use std::fmt;

/// Command-line values understood by Cano.
///
/// `filename` is optional here because startup applies the legacy `out.txt`
/// default. Help is represented as a page name even though every legacy help
/// spelling resolves to `general`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Cli {
    pub filename: Option<String>,
    pub config: Option<String>,
    pub help_page: Option<String>,
    pub version: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliError {
    MissingConfigValue,
    UnexpectedFlag,
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingConfigValue => formatter.write_str("missing value for --config"),
            Self::UnexpectedFlag => formatter.write_str("unexpected command-line flag"),
        }
    }
}

impl Error for CliError {}

/// Parse an argv-style slice whose first element is the program name.
///
/// Two prefix matches are intentional compatibility behavior: every argument
/// beginning with `--help` requests the general page, and every argument
/// beginning with `--config` is parsed as the configuration option. The short
/// spellings `-h` and `-v` and the long `--version` match exactly instead --
/// the prefix quirk is compatibility baggage, not a rule to extend. Options
/// may occur on either side of positional arguments; only the first positional
/// argument becomes the filename.
pub fn parse(args: &[String]) -> Result<Cli, CliError> {
    let mut cli = Cli::default();
    let mut first_positional = None;
    let mut index = 1;

    while index < args.len() {
        let argument = &args[index];
        if argument.starts_with("--help") || argument == "-h" {
            cli.help_page = Some("general".to_owned());
            index += 1;
        } else if argument == "--version" || argument == "-v" {
            cli.version = true;
            index += 1;
        } else if argument.starts_with("--config") {
            if let Some((_, value)) = argument.split_once('=') {
                // `--config=` was accepted by the characterized parser. An
                // empty path will fail later at the filesystem boundary.
                cli.config = Some(value.to_owned());
                index += 1;
            } else {
                let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-'))
                else {
                    return Err(CliError::MissingConfigValue);
                };
                cli.config = Some(value.clone());
                index += 2;
            }
        } else if argument.starts_with('-') {
            return Err(CliError::UnexpectedFlag);
        } else {
            if first_positional.is_none() {
                first_positional = Some(argument.clone());
            }
            index += 1;
        }
    }

    cli.filename = first_positional;
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn empty_argv_and_program_only_have_no_explicit_values() {
        assert_eq!(parse(&[]).unwrap(), Cli::default());
        assert_eq!(parse(&argv(&["cano"])).unwrap(), Cli::default());
    }

    #[test]
    fn first_positional_is_the_filename_and_later_positionals_are_ignored() {
        let parsed = parse(&argv(&["cano", "first", "second", "third"])).unwrap();
        assert_eq!(parsed.filename.as_deref(), Some("first"));
        assert_eq!(parsed.config, None);
        assert_eq!(parsed.help_page, None);
    }

    #[test]
    fn separated_config_can_precede_or_follow_the_filename() {
        let before = parse(&argv(&["cano", "--config", "settings.lua", "notes.txt"])).unwrap();
        let after = parse(&argv(&["cano", "notes.txt", "--config", "settings.lua"])).unwrap();

        assert_eq!(before, after);
        assert_eq!(before.filename.as_deref(), Some("notes.txt"));
        assert_eq!(before.config.as_deref(), Some("settings.lua"));
    }

    #[test]
    fn equals_config_preserves_everything_after_the_first_equals() {
        let parsed = parse(&argv(&["cano", "--config=dir=a/init.lua", "file"])).unwrap();
        assert_eq!(parsed.config.as_deref(), Some("dir=a/init.lua"));
        assert_eq!(parsed.filename.as_deref(), Some("file"));

        let empty = parse(&argv(&["cano", "--config="])).unwrap();
        assert_eq!(empty.config.as_deref(), Some(""));
    }

    #[test]
    fn config_uses_the_documented_prefix_match_quirk() {
        let separated = parse(&argv(&["cano", "--configuration", "one.lua"])).unwrap();
        assert_eq!(separated.config.as_deref(), Some("one.lua"));

        let attached = parse(&argv(&["cano", "--config-file=two.lua"])).unwrap();
        assert_eq!(attached.config.as_deref(), Some("two.lua"));
    }

    #[test]
    fn the_last_config_option_wins() {
        let parsed = parse(&argv(&[
            "cano",
            "--config=first.lua",
            "--config",
            "second.lua",
        ]))
        .unwrap();
        assert_eq!(parsed.config.as_deref(), Some("second.lua"));
    }

    #[test]
    fn every_help_prefix_resolves_to_general() {
        for spelling in ["--help", "--help=keys", "--helpful", "--helper"] {
            let parsed = parse(&argv(&["cano", spelling])).unwrap();
            assert_eq!(parsed.help_page.as_deref(), Some("general"), "{spelling}");
        }
    }

    #[test]
    fn help_takes_no_value_so_the_next_argument_is_a_filename() {
        let parsed = parse(&argv(&["cano", "--help", "keys"])).unwrap();
        assert_eq!(parsed.help_page.as_deref(), Some("general"));
        assert_eq!(parsed.filename.as_deref(), Some("keys"));
    }

    #[test]
    fn separated_config_requires_a_non_flag_value() {
        assert_eq!(
            parse(&argv(&["cano", "--config"])),
            Err(CliError::MissingConfigValue)
        );
        assert_eq!(
            parse(&argv(&["cano", "--config", "--help"])),
            Err(CliError::MissingConfigValue)
        );
        assert_eq!(
            parse(&argv(&["cano", "--config-prefix"])),
            Err(CliError::MissingConfigValue)
        );
    }

    #[test]
    fn unknown_dash_arguments_are_rejected() {
        for spelling in ["-x", "--nope", "-hv", "-"] {
            assert_eq!(
                parse(&argv(&["cano", spelling])),
                Err(CliError::UnexpectedFlag),
                "{spelling}"
            );
        }
    }

    #[test]
    fn short_help_is_the_long_spelling() {
        let short = parse(&argv(&["cano", "-h"])).unwrap();
        let long = parse(&argv(&["cano", "--help"])).unwrap();
        assert_eq!(short, long);
        assert_eq!(short.help_page.as_deref(), Some("general"));
    }

    #[test]
    fn both_version_spellings_set_the_flag() {
        for spelling in ["-v", "--version"] {
            let parsed = parse(&argv(&["cano", spelling])).unwrap();
            assert!(parsed.version, "{spelling}");
            assert_eq!(parsed.filename, None, "{spelling}");
            assert_eq!(parsed.help_page, None, "{spelling}");
        }
    }

    #[test]
    fn version_takes_no_value_and_sits_beside_other_arguments() {
        let parsed = parse(&argv(&["cano", "--version", "notes.txt"])).unwrap();
        assert!(parsed.version);
        assert_eq!(parsed.filename.as_deref(), Some("notes.txt"));

        let trailing = parse(&argv(&["cano", "notes.txt", "-v"])).unwrap();
        assert_eq!(parsed, trailing);
    }

    #[test]
    fn the_new_short_and_long_flags_match_exactly() {
        // `--help` and `--config` prefix-match for legacy compatibility.  The
        // flags added since do not, so a longer spelling is still an error
        // rather than a silent alias.
        for spelling in ["--versions", "--version=1", "-vv"] {
            assert_eq!(
                parse(&argv(&["cano", spelling])),
                Err(CliError::UnexpectedFlag),
                "{spelling}"
            );
        }
    }

    #[test]
    fn version_wins_when_help_is_also_requested() {
        // Both are recorded; startup resolves the precedence.
        let parsed = parse(&argv(&["cano", "--help", "--version"])).unwrap();
        assert!(parsed.version);
        assert_eq!(parsed.help_page.as_deref(), Some("general"));
    }

    #[test]
    fn the_program_name_is_not_parsed_as_an_argument() {
        let parsed = parse(&argv(&["--help", "file.txt"])).unwrap();
        assert_eq!(parsed.help_page, None);
        assert_eq!(parsed.filename.as_deref(), Some("file.txt"));
    }
}
