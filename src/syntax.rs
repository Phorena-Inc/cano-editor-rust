//! Safe `.cyntax` parsing and byte-oriented source tokenization.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// An RGB color in the `.cyntax` 0–255 range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

/// One configurable word/color group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxGroup {
    pub color: Rgb,
    pub words: Vec<Vec<u8>>,
}

/// Parsed syntax-highlighting configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxConfig {
    pub keyword: SyntaxGroup,
    pub type_name: SyntaxGroup,
    pub word: SyntaxGroup,
}

impl Default for SyntaxConfig {
    fn default() -> Self {
        Self {
            keyword: SyntaxGroup {
                color: Rgb::new(255, 0, 0),
                words: builtin_keywords(),
            },
            type_name: SyntaxGroup {
                color: Rgb::new(255, 255, 0),
                words: builtin_types(),
            },
            word: SyntaxGroup {
                color: Rgb::new(0, 0, 255),
                words: Vec::new(),
            },
        }
    }
}

impl SyntaxConfig {
    pub const fn preprocessor_color() -> Rgb {
        Rgb::new(0, 255, 255)
    }

    pub const fn string_color() -> Rgb {
        Rgb::new(255, 0, 255)
    }

    pub const fn comment_color() -> Rgb {
        Rgb::new(0, 255, 0)
    }
}

fn copied(words: &[&[u8]]) -> Vec<Vec<u8>> {
    words.iter().map(|word| word.to_vec()).collect()
}

fn builtin_keywords() -> Vec<Vec<u8>> {
    copied(&[
        b"auto",
        b"break",
        b"case",
        b"const",
        b"continue",
        b"default",
        b"do",
        b"else",
        b"enum",
        b"extern",
        b"for",
        b"goto",
        b"if",
        b"inline",
        b"register",
        b"restrict",
        b"return",
        b"sizeof",
        b"static",
        b"struct",
        b"switch",
        b"typedef",
        b"union",
        b"volatile",
        b"while",
        b"_Alignas",
        b"_Alignof",
        b"_Atomic",
        b"_Generic",
        b"_Noreturn",
        b"_Static_assert",
        b"_Thread_local",
    ])
}

fn builtin_types() -> Vec<Vec<u8>> {
    copied(&[
        b"_Bool",
        b"_Complex",
        b"_Imaginary",
        b"bool",
        b"char",
        b"double",
        b"float",
        b"int",
        b"long",
        b"short",
        b"signed",
        b"unsigned",
        b"void",
    ])
}

/// A structured `.cyntax` loading/parsing error.
#[derive(Debug)]
pub enum SyntaxError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    UnterminatedGroup,
    UnknownGroup {
        group: usize,
        tag: Vec<u8>,
    },
    MissingFields {
        group: usize,
    },
    InvalidColor {
        group: usize,
        component: Vec<u8>,
    },
    ColorOutOfRange {
        group: usize,
        value: i64,
    },
    EmptyWord {
        group: usize,
    },
}

impl PartialEq for SyntaxError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Io { path: left, .. }, Self::Io { path: right, .. }) => left == right,
            (Self::UnterminatedGroup, Self::UnterminatedGroup) => true,
            (
                Self::UnknownGroup {
                    group: left_group,
                    tag: left_tag,
                },
                Self::UnknownGroup {
                    group: right_group,
                    tag: right_tag,
                },
            ) => left_group == right_group && left_tag == right_tag,
            (Self::MissingFields { group: left }, Self::MissingFields { group: right }) => {
                left == right
            }
            (
                Self::InvalidColor {
                    group: left_group,
                    component: left_component,
                },
                Self::InvalidColor {
                    group: right_group,
                    component: right_component,
                },
            ) => left_group == right_group && left_component == right_component,
            (
                Self::ColorOutOfRange {
                    group: left_group,
                    value: left_value,
                },
                Self::ColorOutOfRange {
                    group: right_group,
                    value: right_value,
                },
            ) => left_group == right_group && left_value == right_value,
            (Self::EmptyWord { group: left }, Self::EmptyWord { group: right }) => left == right,
            _ => false,
        }
    }
}

impl Eq for SyntaxError {}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Self::UnterminatedGroup => f.write_str("unterminated .cyntax group"),
            Self::UnknownGroup { group, tag } => write!(
                f,
                "unknown .cyntax group {group}: {}",
                String::from_utf8_lossy(tag)
            ),
            Self::MissingFields { group } => {
                write!(f, ".cyntax group {group} needs a tag and three colors")
            }
            Self::InvalidColor { group, component } => write!(
                f,
                "invalid color in .cyntax group {group}: {}",
                String::from_utf8_lossy(component)
            ),
            Self::ColorOutOfRange { group, value } => {
                write!(
                    f,
                    "color {value} in .cyntax group {group} is outside 0..=255"
                )
            }
            Self::EmptyWord { group } => write!(f, "empty word in .cyntax group {group}"),
        }
    }
}

impl Error for SyntaxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn color(component: &[u8], group: usize) -> Result<u8, SyntaxError> {
    let component = trim(component);
    let text = std::str::from_utf8(component).map_err(|_| SyntaxError::InvalidColor {
        group,
        component: component.to_vec(),
    })?;
    let value = text.parse::<i64>().map_err(|_| SyntaxError::InvalidColor {
        group,
        component: component.to_vec(),
    })?;
    u8::try_from(value).map_err(|_| SyntaxError::ColorOutOfRange { group, value })
}

/// Parses `.cyntax` bytes.
///
/// Groups are dot terminated and contain comma-separated `tag,r,g,b,words…`
/// fields.  `k` and `t` groups without word fields use Cano's built-in C
/// keyword/type lists; custom words replace those lists.
pub fn parse(source: &[u8]) -> Result<SyntaxConfig, SyntaxError> {
    let source = trim(source);
    if source.is_empty() {
        return Ok(SyntaxConfig::default());
    }
    if source.last() != Some(&b'.') {
        return Err(SyntaxError::UnterminatedGroup);
    }

    let mut config = SyntaxConfig::default();
    for (group_index, raw_group) in source.split(|byte| *byte == b'.').enumerate() {
        let raw_group = trim(raw_group);
        if raw_group.is_empty() {
            continue;
        }
        let fields: Vec<&[u8]> = raw_group.split(|byte| *byte == b',').collect();
        if fields.len() < 4 {
            return Err(SyntaxError::MissingFields { group: group_index });
        }
        let tag = trim(fields[0]);
        if !matches!(tag, b"k" | b"t" | b"w") {
            return Err(SyntaxError::UnknownGroup {
                group: group_index,
                tag: tag.to_vec(),
            });
        }
        let parsed_color = Rgb::new(
            color(fields[1], group_index)?,
            color(fields[2], group_index)?,
            color(fields[3], group_index)?,
        );

        let mut words = Vec::new();
        for field in &fields[4..] {
            let word = trim(field);
            if word.is_empty() {
                return Err(SyntaxError::EmptyWord { group: group_index });
            }
            words.push(word.to_vec());
        }

        match tag {
            b"k" => {
                config.keyword.color = parsed_color;
                config.keyword.words = if words.is_empty() {
                    builtin_keywords()
                } else {
                    words
                };
            }
            b"t" => {
                config.type_name.color = parsed_color;
                config.type_name.words = if words.is_empty() {
                    builtin_types()
                } else {
                    words
                };
            }
            b"w" => {
                config.word.color = parsed_color;
                config.word.words = words;
            }
            _ => unreachable!("tag was checked above"),
        }
    }
    Ok(config)
}

/// Reads and parses a `.cyntax` file.
pub fn load(path: &Path) -> Result<SyntaxConfig, SyntaxError> {
    let source = fs::read(path).map_err(|source| SyntaxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&source)
}

/// The six source token categories Cano colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxKind {
    Keyword,
    Type,
    Preprocessor,
    String,
    Comment,
    Word,
}

/// A half-open, safely bounded byte span in source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxToken {
    pub kind: SyntaxKind,
    pub start: usize,
    pub end: usize,
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn in_group(word: &[u8], group: &SyntaxGroup) -> bool {
    group.words.iter().any(|candidate| candidate == word)
}

/// Classifies the colorable spans in arbitrary source bytes.
///
/// Strings and line comments may end at EOF.  Escaped quotes are honored and
/// no scan can advance beyond `source.len()`, including malformed input.
pub fn tokens(source: &[u8], config: &SyntaxConfig) -> Vec<SyntaxToken> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < source.len() {
        if source[at] == b'/' && source.get(at + 1) == Some(&b'/') {
            let start = at;
            at += 2;
            while at < source.len() && source[at] != b'\n' {
                at += 1;
            }
            result.push(SyntaxToken {
                kind: SyntaxKind::Comment,
                start,
                end: at,
            });
            continue;
        }

        if source[at] == b'/' && source.get(at + 1) == Some(&b'*') {
            let start = at;
            at += 2;
            while at < source.len() {
                if source[at] == b'*' && source.get(at + 1) == Some(&b'/') {
                    at += 2;
                    break;
                }
                at += 1;
            }
            result.push(SyntaxToken {
                kind: SyntaxKind::Comment,
                start,
                end: at,
            });
            continue;
        }

        if source[at] == b'"' {
            let start = at;
            at += 1;
            let mut escaped = false;
            while at < source.len() {
                let byte = source[at];
                at += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    break;
                }
            }
            result.push(SyntaxToken {
                kind: SyntaxKind::String,
                start,
                end: at,
            });
            continue;
        }

        // A character literal must close on the same line; otherwise a stray
        // apostrophe (say, inside a comment) would swallow the rest of the
        // file as one string span.
        if source[at] == b'\'' {
            let start = at;
            let mut scan = at + 1;
            let mut escaped = false;
            let mut terminated = false;
            while scan < source.len() && source[scan] != b'\n' {
                let byte = source[scan];
                scan += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'\'' {
                    terminated = true;
                    break;
                }
            }
            if terminated {
                result.push(SyntaxToken {
                    kind: SyntaxKind::String,
                    start,
                    end: scan,
                });
                at = scan;
            } else {
                at += 1;
            }
            continue;
        }

        if source[at] == b'#' {
            let start = at;
            at += 1;
            while at < source.len() && is_word_byte(source[at]) {
                at += 1;
            }
            result.push(SyntaxToken {
                kind: SyntaxKind::Preprocessor,
                start,
                end: at,
            });
            continue;
        }

        if is_word_byte(source[at]) {
            let start = at;
            at += 1;
            while at < source.len() && is_word_byte(source[at]) {
                at += 1;
            }
            let word = &source[start..at];
            let kind = if in_group(word, &config.keyword) {
                Some(SyntaxKind::Keyword)
            } else if in_group(word, &config.type_name) {
                Some(SyntaxKind::Type)
            } else if in_group(word, &config.word) {
                Some(SyntaxKind::Word)
            } else {
                None
            };
            if let Some(kind) = kind {
                result.push(SyntaxToken {
                    kind,
                    start,
                    end: at,
                });
            }
            continue;
        }

        at += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn empty_and_default_groups_select_builtin_c_words() {
        let config = parse(b"k,1,2,3.t,4,5,6.w,7,8,9.").unwrap();
        assert_eq!(config.keyword.color, Rgb::new(1, 2, 3));
        assert!(in_group(b"return", &config.keyword));
        assert!(in_group(b"int", &config.type_name));
        assert!(config.word.words.is_empty());
    }

    #[test]
    fn custom_groups_replace_defaults_and_trim_fields() {
        let config =
            parse(b" k, 10, 20, 30,when, otherwise . t,4,5,6,Thing .w,7,8,9,TODO .").unwrap();
        assert_eq!(
            config.keyword.words,
            [b"when".to_vec(), b"otherwise".to_vec()]
        );
        assert!(!in_group(b"if", &config.keyword));
        assert_eq!(config.type_name.words, [b"Thing".to_vec()]);
        assert_eq!(config.word.words, [b"TODO".to_vec()]);
    }

    #[test]
    fn malformed_groups_have_structured_errors() {
        assert_eq!(parse(b"k,1,2,3"), Err(SyntaxError::UnterminatedGroup));
        assert_eq!(
            parse(b"x,1,2,3."),
            Err(SyntaxError::UnknownGroup {
                group: 0,
                tag: b"x".to_vec()
            })
        );
        assert_eq!(
            parse(b"k,1,2."),
            Err(SyntaxError::MissingFields { group: 0 })
        );
        assert_eq!(
            parse(b"k,256,0,0."),
            Err(SyntaxError::ColorOutOfRange {
                group: 0,
                value: 256
            })
        );
        assert_eq!(
            parse(b"k,no,0,0."),
            Err(SyntaxError::InvalidColor {
                group: 0,
                component: b"no".to_vec()
            })
        );
    }

    #[test]
    fn all_six_token_categories_are_bounded_and_classified() {
        let config = parse(b"k,1,2,3,when.t,4,5,6,Thing.w,7,8,9,TODO.").unwrap();
        let source = b"#define when Thing TODO \"a\\\"b\" 'c' // note\nplain";
        let found = tokens(source, &config);
        assert_eq!(
            found.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![
                SyntaxKind::Preprocessor,
                SyntaxKind::Keyword,
                SyntaxKind::Type,
                SyntaxKind::Word,
                SyntaxKind::String,
                SyntaxKind::String,
                SyntaxKind::Comment,
            ]
        );
        assert!(found.iter().all(|token| token.start < token.end));
        assert!(found.iter().all(|token| token.end <= source.len()));
    }

    #[test]
    fn unterminated_strings_are_safely_clamped_to_eof() {
        let source = b"\"never closed";
        assert_eq!(
            tokens(source, &SyntaxConfig::default()),
            vec![SyntaxToken {
                kind: SyntaxKind::String,
                start: 0,
                end: source.len()
            }]
        );
    }

    #[test]
    fn block_comments_span_lines_and_clamp_to_eof() {
        let source = b"a /* one\ntwo */ b /* open";
        let found = tokens(source, &SyntaxConfig::default());
        assert_eq!(
            found,
            vec![
                SyntaxToken {
                    kind: SyntaxKind::Comment,
                    start: 2,
                    end: 15
                },
                SyntaxToken {
                    kind: SyntaxKind::Comment,
                    start: 18,
                    end: source.len()
                },
            ]
        );
    }

    #[test]
    fn a_stray_apostrophe_does_not_swallow_the_rest_of_the_file() {
        let source = b"don't stop\n'x' next";
        let found = tokens(source, &SyntaxConfig::default());
        assert_eq!(
            found,
            vec![SyntaxToken {
                kind: SyntaxKind::String,
                start: 11,
                end: 14
            }]
        );
    }

    #[test]
    fn comments_inside_strings_do_not_start_comment_tokens() {
        let source = b"\"// string\" // comment";
        let found = tokens(source, &SyntaxConfig::default());
        assert_eq!(found[0].kind, SyntaxKind::String);
        assert_eq!(found[1].kind, SyntaxKind::Comment);
    }

    #[test]
    fn token_storage_grows_past_the_legacy_initial_capacity() {
        let config = parse(b"w,0,0,255,x.").unwrap();
        let source = std::iter::repeat_n("x", 1024).collect::<Vec<_>>().join(" ");
        let found = tokens(source.as_bytes(), &config);
        assert_eq!(found.len(), 1024);
        assert_eq!(found.last().unwrap().end, source.len());
    }

    #[test]
    fn load_reports_missing_files_without_unsafe_fallbacks() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let missing = std::env::temp_dir().join(format!("missing-{unique}.cyntax"));
        assert!(matches!(load(&missing), Err(SyntaxError::Io { .. })));
    }
}
