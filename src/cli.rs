/// Command-line values understood by Cano.
///
/// `filename` is optional here because startup applies the legacy `out.txt`
/// default. Help is a page name: `-h` and `--help` ask for `general`, and
/// `--help=<page>` names one. The parser does not know which pages exist --
/// startup checks the name against the shipped set and reports an unknown
/// one, so the list lives in one place beside the pages themselves.
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

/// Parse an argv-style slice whose first element is the program name.
///
/// Every flag matches exactly. `--help` and `--config` used to match as
/// prefixes, so `--helpful`, `--help=keys` and `--configuration x` were all
/// silently taken as something else -- `--help=keys` opened `general`, which
/// is what made a page impossible to choose. Now `--help`, `-h` and
/// `--help=<page>` are the help spellings, `--config <path>` and
/// `--config=<path>` the configuration ones, and any other dash argument is
/// an unexpected flag.
///
/// `--help` takes no separated value: `cano --help notes.txt` has always
/// treated the word after it as the (ignored) filename, and reading it as a
/// page name instead would turn that invocation into an unknown-page error.
/// Options may occur on either side of positional arguments; only the first
/// positional argument becomes the filename.
pub fn parse(args: &[String]) -> Result<Cli, CliError> {
    let mut cli = Cli::default();
    let mut index = 1;

    while index < args.len() {
        let argument = &args[index];
        if argument == "--help" || argument == "-h" {
            cli.help_page = Some("general".to_owned());
            index += 1;
        } else if let Some(page) = argument.strip_prefix("--help=") {
            // Passed through unchecked, empty included: startup owns the list
            // of pages and the message naming them.
            cli.help_page = Some(page.to_owned());
            index += 1;
        } else if argument == "--version" || argument == "-v" {
            cli.version = true;
            index += 1;
        } else if argument == "--config" || argument.starts_with("--config=") {
            if let Some(value) = argument.strip_prefix("--config=") {
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
            // The first positional is the file; later ones are ignored.
            cli.filename.get_or_insert_with(|| argument.clone());
            index += 1;
        }
    }

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
    fn config_matches_exactly_so_longer_spellings_are_unexpected_flags() {
        // These were prefix matches once: `--configuration one.lua` loaded
        // `one.lua`.  A misspelt flag is now an error, not a silent alias.
        for spelling in [
            "--configuration",
            "--config-file=two.lua",
            "--config-prefix",
        ] {
            assert_eq!(
                parse(&argv(&["cano", spelling, "one.lua"])),
                Err(CliError::UnexpectedFlag),
                "{spelling}"
            );
        }
        let attached = parse(&argv(&["cano", "--config=x"])).unwrap();
        assert_eq!(attached.config.as_deref(), Some("x"));
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
    fn help_equals_names_the_page_and_bare_help_means_general() {
        for (spelling, page) in [
            ("--help", "general"),
            ("-h", "general"),
            ("--help=keys", "keys"),
            ("--help=cmds", "cmds"),
            // Not validated here; startup reports the unknown name.
            ("--help=nope", "nope"),
            ("--help=", ""),
        ] {
            let parsed = parse(&argv(&["cano", spelling])).unwrap();
            assert_eq!(parsed.help_page.as_deref(), Some(page), "{spelling}");
        }
    }

    #[test]
    fn longer_help_spellings_are_unexpected_flags() {
        for spelling in ["--helpful", "--helper", "--help-keys", "-help"] {
            assert_eq!(
                parse(&argv(&["cano", spelling])),
                Err(CliError::UnexpectedFlag),
                "{spelling}"
            );
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
        // No flag prefix-matches, so a longer spelling is an error rather
        // than a silent alias.
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
