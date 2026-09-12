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

/// A language whose lexical conventions Cano knows without a `.cyntax` file.
///
/// The variant selects both the built-in keyword/type lists and the scanner
/// used for comments, strings and directives: `//` opens a comment in C but
/// is floor division in Python, and `'a` is a lifetime in Rust but a string
/// delimiter everywhere else.  Getting that wrong miscolors whole files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    C,
    Cpp,
    Rust,
    Python,
    Bash,
    Vim,
    Lua,
    Json,
}

impl Language {
    /// Maps a file extension to its language, case-insensitively.
    ///
    /// `.h` is claimed by C: it is shared with C++, and the C lists are the
    /// subset, so a C++ header loses a few keyword colors rather than
    /// coloring C code with words that are not reserved in it.
    /// Picks the language for a whole path.
    ///
    /// Dotfiles like `.vimrc` and `.bashrc` have no extension at all, so the
    /// file name has to be consulted before falling back to one.
    pub fn for_path(path: &Path) -> Option<Self> {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| Self::for_name(&name.to_ascii_lowercase()))
            .or_else(|| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .and_then(Self::for_extension)
            })
    }

    /// Matches the well-known configuration file names that carry no
    /// extension of their own.
    fn for_name(name: &str) -> Option<Self> {
        match name {
            ".vimrc" | "_vimrc" | "vimrc" | ".gvimrc" | "_gvimrc" | "gvimrc" | ".exrc" => {
                Some(Self::Vim)
            }
            ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".profile"
            | ".zshrc" | ".zprofile" | ".zshenv" | ".zlogin" | ".zlogout" | ".kshrc" | "bashrc"
            | "profile" => Some(Self::Bash),
            _ => None,
        }
    }

    pub fn for_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "c" | "h" => Some(Self::C),
            "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "ipp" | "tpp" => {
                Some(Self::Cpp)
            }
            "rs" => Some(Self::Rust),
            "py" | "pyi" | "pyw" => Some(Self::Python),
            "sh" | "bash" | "zsh" | "ksh" | "ash" | "dash" => Some(Self::Bash),
            "vim" | "vimrc" => Some(Self::Vim),
            "lua" => Some(Self::Lua),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// The reserved words this language colors as keywords.
    pub fn keywords(self) -> Vec<Vec<u8>> {
        match self {
            Self::C => builtin_keywords(),
            Self::Cpp => cpp_keywords(),
            Self::Rust => rust_keywords(),
            Self::Python => python_keywords(),
            Self::Bash => bash_keywords(),
            Self::Vim => vim_keywords(),
            Self::Lua => lua_keywords(),
            Self::Json => copied(&[b"false", b"null", b"true"]),
        }
    }

    /// The words this language colors as types.
    pub fn types(self) -> Vec<Vec<u8>> {
        match self {
            Self::C => builtin_types(),
            Self::Cpp => cpp_types(),
            Self::Rust => rust_types(),
            Self::Python => python_types(),
            Self::Bash => bash_builtins(),
            Self::Vim => vim_options(),
            Self::Lua => lua_builtins(),
            Self::Json => Vec::new(),
        }
    }
}

/// Parsed syntax-highlighting configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxConfig {
    /// Chooses the scanner; a `.cyntax` file supplies colors and words but
    /// never changes how the source is lexed.
    pub language: Language,
    pub keyword: SyntaxGroup,
    pub type_name: SyntaxGroup,
    pub word: SyntaxGroup,
}

impl Default for SyntaxConfig {
    fn default() -> Self {
        Self::for_language(Language::C)
    }
}

impl SyntaxConfig {
    /// The built-in palette and word lists for one language.
    pub fn for_language(language: Language) -> Self {
        Self {
            language,
            keyword: SyntaxGroup {
                color: Rgb::new(255, 0, 0),
                words: language.keywords(),
            },
            type_name: SyntaxGroup {
                color: Rgb::new(255, 255, 0),
                words: language.types(),
            },
            word: SyntaxGroup {
                color: Rgb::new(0, 0, 255),
                words: Vec::new(),
            },
        }
    }

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

/// C++ adds to C rather than replacing it, so a `.cpp` file still colors the
/// C keywords it inherits.
fn cpp_keywords() -> Vec<Vec<u8>> {
    let mut words = builtin_keywords();
    words.extend(copied(&[
        b"alignas",
        b"alignof",
        b"and",
        b"and_eq",
        b"asm",
        b"bitand",
        b"bitor",
        b"catch",
        b"class",
        b"compl",
        b"concept",
        b"const_cast",
        b"consteval",
        b"constexpr",
        b"constinit",
        b"co_await",
        b"co_return",
        b"co_yield",
        b"decltype",
        b"delete",
        b"dynamic_cast",
        b"explicit",
        b"export",
        b"false",
        b"final",
        b"friend",
        b"mutable",
        b"namespace",
        b"new",
        b"noexcept",
        b"not",
        b"not_eq",
        b"nullptr",
        b"operator",
        b"or",
        b"or_eq",
        b"override",
        b"private",
        b"protected",
        b"public",
        b"reinterpret_cast",
        b"requires",
        b"static_assert",
        b"static_cast",
        b"template",
        b"this",
        b"thread_local",
        b"throw",
        b"true",
        b"try",
        b"typeid",
        b"typename",
        b"using",
        b"virtual",
        b"xor",
        b"xor_eq",
    ]));
    words
}

fn cpp_types() -> Vec<Vec<u8>> {
    let mut words = builtin_types();
    words.extend(copied(&[
        b"char16_t",
        b"char32_t",
        b"char8_t",
        b"nullptr_t",
        b"ptrdiff_t",
        b"size_t",
        b"wchar_t",
        // Standard-library names are not reserved, but a C++ file without
        // them colored reads as if half the types were missing.
        b"array",
        b"map",
        b"optional",
        b"pair",
        b"set",
        b"shared_ptr",
        b"string",
        b"string_view",
        b"unique_ptr",
        b"unordered_map",
        b"unordered_set",
        b"vector",
        b"wstring",
    ]));
    words
}

fn rust_keywords() -> Vec<Vec<u8>> {
    copied(&[
        b"as",
        b"async",
        b"await",
        b"break",
        b"const",
        b"continue",
        b"crate",
        b"dyn",
        b"else",
        b"enum",
        b"extern",
        b"false",
        b"fn",
        b"for",
        b"if",
        b"impl",
        b"in",
        b"let",
        b"loop",
        b"match",
        b"mod",
        b"move",
        b"mut",
        b"pub",
        b"ref",
        b"return",
        b"self",
        b"static",
        b"struct",
        b"super",
        b"trait",
        b"true",
        b"type",
        b"union",
        b"unsafe",
        b"use",
        b"where",
        b"while",
        // Reserved for future use; coloring them warns before the compiler
        // does.
        b"abstract",
        b"become",
        b"box",
        b"do",
        b"final",
        b"gen",
        b"macro",
        b"override",
        b"priv",
        b"try",
        b"typeof",
        b"unsized",
        b"virtual",
        b"yield",
    ])
}

fn rust_types() -> Vec<Vec<u8>> {
    copied(&[
        b"Self",
        b"bool",
        b"char",
        b"f32",
        b"f64",
        b"i8",
        b"i16",
        b"i32",
        b"i64",
        b"i128",
        b"isize",
        b"str",
        b"u8",
        b"u16",
        b"u32",
        b"u64",
        b"u128",
        b"usize",
        // Prelude names, including the variants that read as constructors.
        b"Arc",
        b"BTreeMap",
        b"BTreeSet",
        b"Box",
        b"Cow",
        b"Err",
        b"HashMap",
        b"HashSet",
        b"None",
        b"Ok",
        b"Option",
        b"PathBuf",
        b"Rc",
        b"RefCell",
        b"Result",
        b"Some",
        b"String",
        b"Vec",
    ])
}

fn python_keywords() -> Vec<Vec<u8>> {
    copied(&[
        b"False",
        b"None",
        b"True",
        b"and",
        b"as",
        b"assert",
        b"async",
        b"await",
        b"break",
        b"case",
        b"class",
        b"continue",
        b"def",
        b"del",
        b"elif",
        b"else",
        b"except",
        b"finally",
        b"for",
        b"from",
        b"global",
        b"if",
        b"import",
        b"in",
        b"is",
        b"lambda",
        b"match",
        b"nonlocal",
        b"not",
        b"or",
        b"pass",
        b"raise",
        b"return",
        b"try",
        b"while",
        b"with",
        b"yield",
        // Not reserved, but every Python file binds them the same way.
        b"cls",
        b"self",
    ])
}

fn python_types() -> Vec<Vec<u8>> {
    copied(&[
        b"bool",
        b"bytearray",
        b"bytes",
        b"complex",
        b"dict",
        b"float",
        b"frozenset",
        b"int",
        b"list",
        b"memoryview",
        b"object",
        b"range",
        b"set",
        b"str",
        b"tuple",
        b"type",
        // The typing spellings that appear in annotations.
        b"Any",
        b"Callable",
        b"Dict",
        b"Iterable",
        b"Iterator",
        b"List",
        b"Optional",
        b"Sequence",
        b"Set",
        b"Tuple",
        b"Union",
    ])
}

fn bash_keywords() -> Vec<Vec<u8>> {
    copied(&[
        b"alias",
        b"break",
        b"case",
        b"continue",
        b"coproc",
        b"declare",
        b"do",
        b"done",
        b"elif",
        b"else",
        b"esac",
        b"eval",
        b"exec",
        b"exit",
        b"export",
        b"fi",
        b"for",
        b"function",
        b"if",
        b"in",
        b"local",
        b"readonly",
        b"return",
        b"select",
        b"set",
        b"shift",
        b"source",
        b"then",
        b"time",
        b"trap",
        b"typeset",
        b"unalias",
        b"unset",
        b"until",
        b"while",
    ])
}

/// Shell has no types, so the second group colors the builtins instead: a
/// script with only its keywords colored reads as if half of it were missing.
fn bash_builtins() -> Vec<Vec<u8>> {
    copied(&[
        b"bg",
        b"builtin",
        b"cd",
        b"command",
        b"echo",
        b"false",
        b"fg",
        b"getopts",
        b"hash",
        b"jobs",
        b"kill",
        b"let",
        b"mapfile",
        b"printf",
        b"pwd",
        b"read",
        b"readarray",
        b"test",
        b"true",
        b"type",
        b"ulimit",
        b"umask",
        b"wait",
    ])
}

fn vim_keywords() -> Vec<Vec<u8>> {
    copied(&[
        b"abbreviate",
        b"augroup",
        b"autocmd",
        b"behave",
        b"break",
        b"call",
        b"catch",
        b"cabbrev",
        b"cmap",
        b"cnoremap",
        b"colorscheme",
        b"command",
        b"continue",
        b"delcommand",
        b"echo",
        b"echoerr",
        b"echom",
        b"echomsg",
        b"else",
        b"elseif",
        b"endfor",
        b"endfunc",
        b"endfunction",
        b"endif",
        b"endtry",
        b"endwhile",
        b"execute",
        b"filetype",
        b"finally",
        b"finish",
        b"for",
        b"function",
        b"hi",
        b"highlight",
        b"iabbrev",
        b"if",
        b"imap",
        b"inoremap",
        b"let",
        b"map",
        b"nmap",
        b"nnoremap",
        b"nohl",
        b"nohlsearch",
        b"noremap",
        b"normal",
        b"omap",
        b"onoremap",
        b"packadd",
        b"return",
        b"runtime",
        b"set",
        b"setglobal",
        b"setlocal",
        b"silent",
        b"source",
        b"syntax",
        b"try",
        b"unlet",
        b"unmap",
        b"vmap",
        b"vnoremap",
        b"while",
        b"xmap",
        b"xnoremap",
    ])
}

/// The option and variable names a vimrc is mostly made of.
fn vim_options() -> Vec<Vec<u8>> {
    copied(&[
        b"autoindent",
        b"background",
        b"backup",
        b"clipboard",
        b"cursorline",
        b"encoding",
        b"expandtab",
        b"foldlevel",
        b"foldmethod",
        b"hlsearch",
        b"ignorecase",
        b"incsearch",
        b"laststatus",
        b"list",
        b"listchars",
        b"mapleader",
        b"mouse",
        b"number",
        b"relativenumber",
        b"ruler",
        b"scrolloff",
        b"shiftwidth",
        b"showcmd",
        b"signcolumn",
        b"smartcase",
        b"smartindent",
        b"splitbelow",
        b"splitright",
        b"swapfile",
        b"tabstop",
        b"termguicolors",
        b"timeoutlen",
        b"undofile",
        b"updatetime",
        b"wildmenu",
        b"wildmode",
        b"wrap",
    ])
}

fn lua_keywords() -> Vec<Vec<u8>> {
    copied(&[
        b"and",
        b"break",
        b"do",
        b"else",
        b"elseif",
        b"end",
        b"false",
        b"for",
        b"function",
        b"goto",
        b"if",
        b"in",
        b"local",
        b"nil",
        b"not",
        b"or",
        b"repeat",
        b"return",
        b"then",
        b"true",
        b"until",
        b"while",
    ])
}

/// Lua's standard library, which is not reserved but is what a Lua file spends
/// most of its words on.
fn lua_builtins() -> Vec<Vec<u8>> {
    copied(&[
        b"assert",
        b"collectgarbage",
        b"coroutine",
        b"debug",
        b"dofile",
        b"error",
        b"getmetatable",
        b"io",
        b"ipairs",
        b"math",
        b"next",
        b"os",
        b"package",
        b"pairs",
        b"pcall",
        b"print",
        b"rawequal",
        b"rawget",
        b"rawlen",
        b"rawset",
        b"require",
        b"select",
        b"self",
        b"setmetatable",
        b"string",
        b"table",
        b"tonumber",
        b"tostring",
        b"type",
        b"unpack",
        b"xpcall",
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

/// Parses `.cyntax` bytes for one language.
///
/// Groups are dot terminated and contain comma-separated `tag,r,g,b,words…`
/// fields.  `k` and `t` groups without word fields keep `language`'s built-in
/// keyword/type lists; custom words replace those lists.  The language itself
/// is never overridable from the file: it decides how the source is lexed,
/// which a palette has no business changing.
pub fn parse(source: &[u8], language: Language) -> Result<SyntaxConfig, SyntaxError> {
    let source = trim(source);
    if source.is_empty() {
        return Ok(SyntaxConfig::for_language(language));
    }
    if source.last() != Some(&b'.') {
        return Err(SyntaxError::UnterminatedGroup);
    }

    let mut config = SyntaxConfig::for_language(language);
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
                    language.keywords()
                } else {
                    words
                };
            }
            b"t" => {
                config.type_name.color = parsed_color;
                config.type_name.words = if words.is_empty() {
                    language.types()
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

/// Reads and parses a `.cyntax` file for one language.
pub fn load(path: &Path, language: Language) -> Result<SyntaxConfig, SyntaxError> {
    let source = fs::read(path).map_err(|source| SyntaxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&source, language)
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

/// One scanned span: where it ends, and the color it takes.
///
/// A `None` kind consumes bytes without coloring them.  That is how a Rust
/// lifetime is kept from being read as an unterminated character literal,
/// which would color everything up to the next apostrophe on the line.
#[derive(Clone, Copy)]
struct Scan {
    kind: Option<SyntaxKind>,
    end: usize,
}

impl Scan {
    const fn colored(kind: SyntaxKind, end: usize) -> Option<Self> {
        Some(Self {
            kind: Some(kind),
            end,
        })
    }

    const fn plain(end: usize) -> Option<Self> {
        Some(Self { kind: None, end })
    }
}

fn line_end(source: &[u8], at: usize) -> usize {
    source
        .get(at..)
        .and_then(|rest| rest.iter().position(|byte| *byte == b'\n'))
        .map_or(source.len(), |offset| at + offset)
}

fn word_end(source: &[u8], mut at: usize) -> usize {
    while at < source.len() && is_word_byte(source[at]) {
        at += 1;
    }
    at
}

/// Consumes a `/* … */` comment, nesting it when the language allows that.
/// An unterminated comment clamps to EOF rather than being dropped.
fn block_comment(source: &[u8], at: usize, nesting: bool) -> usize {
    let mut scan = at + 2;
    let mut depth = 1usize;
    while scan < source.len() {
        if nesting && source[scan] == b'/' && source.get(scan + 1) == Some(&b'*') {
            depth += 1;
            scan += 2;
            continue;
        }
        if source[scan] == b'*' && source.get(scan + 1) == Some(&b'/') {
            scan += 2;
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return scan;
            }
            continue;
        }
        scan += 1;
    }
    scan
}

/// Consumes a quoted literal, honoring backslash escapes, and reports whether
/// it actually closed before `limit`.  Callers pass the end of the line as the
/// limit for literals that may not span one.
fn quoted(source: &[u8], at: usize, quote: u8, limit: usize) -> (usize, bool) {
    let limit = limit.min(source.len());
    let mut scan = at + 1;
    let mut escaped = false;
    while scan < limit {
        let byte = source[scan];
        scan += 1;
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return (scan, true);
        }
    }
    (scan, false)
}

fn c_scan(source: &[u8], at: usize) -> Option<Scan> {
    match source[at] {
        b'/' if source.get(at + 1) == Some(&b'/') => {
            Scan::colored(SyntaxKind::Comment, line_end(source, at))
        }
        b'/' if source.get(at + 1) == Some(&b'*') => {
            Scan::colored(SyntaxKind::Comment, block_comment(source, at, false))
        }
        b'"' => Scan::colored(SyntaxKind::String, quoted(source, at, b'"', source.len()).0),
        // A character literal must close on the same line; otherwise a stray
        // apostrophe (say, inside a comment) would swallow the rest of the
        // file as one string span.
        b'\'' => {
            let (end, terminated) = quoted(source, at, b'\'', line_end(source, at));
            terminated.then_some(Scan {
                kind: Some(SyntaxKind::String),
                end,
            })
        }
        b'#' => Scan::colored(SyntaxKind::Preprocessor, word_end(source, at + 1)),
        _ => None,
    }
}

fn rust_scan(source: &[u8], at: usize) -> Option<Scan> {
    // Raw strings carry no escapes and may contain quotes, so they have to be
    // recognized before the ordinary string scanner sees them.
    if let Some(end) = rust_raw_string(source, at) {
        return Scan::colored(SyntaxKind::String, end);
    }
    match source[at] {
        b'/' if source.get(at + 1) == Some(&b'/') => {
            Scan::colored(SyntaxKind::Comment, line_end(source, at))
        }
        b'/' if source.get(at + 1) == Some(&b'*') => {
            Scan::colored(SyntaxKind::Comment, block_comment(source, at, true))
        }
        b'"' => Scan::colored(SyntaxKind::String, quoted(source, at, b'"', source.len()).0),
        // `'a'` is a character literal but `'a` is a lifetime, and the two are
        // told apart by what follows the second byte.
        b'\'' => {
            let literal = source.get(at + 1) == Some(&b'\\') || source.get(at + 2) == Some(&b'\'');
            if literal {
                let (end, terminated) = quoted(source, at, b'\'', line_end(source, at));
                if terminated {
                    return Scan::colored(SyntaxKind::String, end);
                }
            }
            Scan::plain(word_end(source, at + 1))
        }
        b'#' => {
            rust_attribute(source, at).and_then(|end| Scan::colored(SyntaxKind::Preprocessor, end))
        }
        _ => None,
    }
}

/// Consumes `r"…"`, `r#"…"#`, `br##"…"##` and the other raw spellings.
fn rust_raw_string(source: &[u8], at: usize) -> Option<usize> {
    let mut scan = at;
    if matches!(source.get(scan), Some(b'b' | b'c')) {
        scan += 1;
    }
    if source.get(scan) != Some(&b'r') {
        return None;
    }
    scan += 1;
    let first_hash = scan;
    while source.get(scan) == Some(&b'#') {
        scan += 1;
    }
    let hashes = scan - first_hash;
    if source.get(scan) != Some(&b'"') {
        return None;
    }
    scan += 1;
    while scan < source.len() {
        if source[scan] == b'"' {
            let closing = scan + 1;
            let seen = source[closing..]
                .iter()
                .take(hashes)
                .take_while(|byte| **byte == b'#')
                .count();
            if seen == hashes {
                return Some(closing + hashes);
            }
        }
        scan += 1;
    }
    Some(source.len())
}

/// Consumes `#[…]` or `#![…]`, which is how Rust spells a directive.
fn rust_attribute(source: &[u8], at: usize) -> Option<usize> {
    let mut scan = at + 1;
    if source.get(scan) == Some(&b'!') {
        scan += 1;
    }
    if source.get(scan) != Some(&b'[') {
        return None;
    }
    let mut depth = 0usize;
    while scan < source.len() {
        match source[scan] {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                scan += 1;
                if depth == 0 {
                    return Some(scan);
                }
                continue;
            }
            _ => {}
        }
        scan += 1;
    }
    Some(scan)
}

fn python_scan(source: &[u8], at: usize) -> Option<Scan> {
    if let Some(end) = python_string(source, at) {
        return Scan::colored(SyntaxKind::String, end);
    }
    match source[at] {
        // Python has no `//` comment: `//` is floor division, and treating it
        // as one would grey out the rest of the line.
        b'#' => Scan::colored(SyntaxKind::Comment, line_end(source, at)),
        // A decorator opens its line; `@` anywhere else is matrix multiply.
        b'@' if only_blanks_before(source, at) => {
            let mut end = at + 1;
            while end < source.len() && (is_word_byte(source[end]) || source[end] == b'.') {
                end += 1;
            }
            Scan::colored(SyntaxKind::Preprocessor, end)
        }
        _ => None,
    }
}

/// Consumes a Python literal, including any `r`/`b`/`u`/`f` prefix and the
/// triple-quoted forms.
fn python_string(source: &[u8], at: usize) -> Option<usize> {
    let mut scan = at;
    while scan < at + 2
        && matches!(
            source.get(scan),
            Some(b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F')
        )
    {
        scan += 1;
    }
    let quote = *source.get(scan)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }

    if source.get(scan + 1) == Some(&quote) && source.get(scan + 2) == Some(&quote) {
        let mut probe = scan + 3;
        while probe < source.len() {
            if source[probe] == b'\\' {
                probe += 2;
                continue;
            }
            if source[probe] == quote
                && source.get(probe + 1) == Some(&quote)
                && source.get(probe + 2) == Some(&quote)
            {
                return Some(probe + 3);
            }
            probe += 1;
        }
        return Some(source.len());
    }

    // A single-quoted literal ends with its line, so an unbalanced quote
    // cannot color the rest of the file.
    Some(quoted(source, scan, quote, line_end(source, scan)).0)
}

fn bash_scan(source: &[u8], at: usize) -> Option<Scan> {
    match source[at] {
        // A `#` only opens a comment at the start of a word.  That is the
        // POSIX rule, and it is what keeps `${name#prefix}` and `$#` from
        // greying out the rest of the line.
        b'#' if at == 0 || source[at - 1].is_ascii_whitespace() => {
            Scan::colored(SyntaxKind::Comment, line_end(source, at))
        }
        // Double quotes and command substitution may span lines, the way a
        // heredoc-free multi-line message in a script does.
        b'"' | b'`' => Scan::colored(
            SyntaxKind::String,
            quoted(source, at, source[at], source.len()).0,
        ),
        // Single quotes are literal in shell: no escape can end them early,
        // and an unbalanced one stops at its line rather than the file.
        b'\'' => {
            let limit = line_end(source, at);
            source
                .get(at + 1..limit)?
                .iter()
                .position(|byte| *byte == b'\'')
                .and_then(|offset| Scan::colored(SyntaxKind::String, at + offset + 2))
        }
        b'$' => bash_expansion(source, at),
        _ => None,
    }
}

/// Consumes `$name`, `${...}` and the one-character specials like `$?`.
fn bash_expansion(source: &[u8], at: usize) -> Option<Scan> {
    let next = *source.get(at + 1)?;
    let end = if next == b'{' {
        let mut scan = at + 2;
        while scan < source.len() && source[scan] != b'}' {
            scan += 1;
        }
        scan.saturating_add(1).min(source.len())
    } else if is_word_byte(next) {
        word_end(source, at + 1)
    } else if matches!(next, b'?' | b'!' | b'#' | b'@' | b'*' | b'$' | b'-') {
        at + 2
    } else {
        return None;
    };
    Scan::colored(SyntaxKind::Preprocessor, end)
}

fn vim_scan(source: &[u8], at: usize) -> Option<Scan> {
    match source[at] {
        // `"` is both the comment marker and a string delimiter in vimscript.
        // It opens a comment when it starts the line, and when nothing closes
        // it before the line ends -- which is how a trailing `set nu " why`
        // comment is told from `let g:x = "value"`.
        b'"' => {
            let limit = line_end(source, at);
            let (end, terminated) = quoted(source, at, b'"', limit);
            if terminated && !only_blanks_before(source, at) {
                Scan::colored(SyntaxKind::String, end)
            } else {
                Scan::colored(SyntaxKind::Comment, limit)
            }
        }
        b'\'' => {
            let (end, terminated) = quoted(source, at, b'\'', line_end(source, at));
            terminated.then_some(Scan {
                kind: Some(SyntaxKind::String),
                end,
            })
        }
        // `<CR>`, `<leader>` and `<C-x>` are vim's notation for keys.
        b'<' => {
            let limit = line_end(source, at);
            let inside = source.get(at + 1..limit)?;
            let length = inside
                .iter()
                .take_while(|byte| is_word_byte(**byte) || **byte == b'-')
                .count();
            // `a < b` is a comparison, not a key: the brackets have to hold
            // something and close immediately after it.
            if length == 0 || inside.get(length) != Some(&b'>') {
                return None;
            }
            Scan::colored(SyntaxKind::Preprocessor, at + length + 2)
        }
        _ => None,
    }
}

fn lua_scan(source: &[u8], at: usize) -> Option<Scan> {
    match source[at] {
        b'-' if source.get(at + 1) == Some(&b'-') => {
            // `--[[ … ]]` is a block comment; anything else runs to the line
            // end.
            let end = lua_long_bracket(source, at + 2).unwrap_or_else(|| line_end(source, at));
            Scan::colored(SyntaxKind::Comment, end)
        }
        b'[' => lua_long_bracket(source, at).and_then(|end| Scan::colored(SyntaxKind::String, end)),
        // Lua's quoted strings do not span lines, so an unbalanced quote is
        // clamped to its own rather than colouring the file.
        b'"' | b'\'' => Scan::colored(
            SyntaxKind::String,
            quoted(source, at, source[at], line_end(source, at)).0,
        ),
        _ => None,
    }
}

/// JSON has only double-quoted strings and no comments. Numbers use the type
/// color so values remain distinct from both strings and literal keywords.
fn json_scan(source: &[u8], at: usize) -> Option<Scan> {
    match source[at] {
        b'"' => Scan::colored(
            SyntaxKind::String,
            quoted(source, at, b'"', line_end(source, at)).0,
        ),
        b'-' | b'0'..=b'9' => {
            json_number_end(source, at).and_then(|end| Scan::colored(SyntaxKind::Type, end))
        }
        _ => None,
    }
}

fn json_number_end(source: &[u8], at: usize) -> Option<usize> {
    let mut scan = at;
    if source[scan] == b'-' {
        scan += 1;
    }
    match source.get(scan)? {
        b'0' => scan += 1,
        b'1'..=b'9' => {
            scan += 1;
            while source.get(scan).is_some_and(u8::is_ascii_digit) {
                scan += 1;
            }
        }
        _ => return None,
    }
    if source.get(scan) == Some(&b'.') && source.get(scan + 1).is_some_and(u8::is_ascii_digit) {
        scan += 2;
        while source.get(scan).is_some_and(u8::is_ascii_digit) {
            scan += 1;
        }
    }
    if matches!(source.get(scan), Some(b'e' | b'E')) {
        let exponent = scan;
        scan += 1;
        if matches!(source.get(scan), Some(b'+' | b'-')) {
            scan += 1;
        }
        let digits = scan;
        while source.get(scan).is_some_and(u8::is_ascii_digit) {
            scan += 1;
        }
        if scan == digits {
            scan = exponent;
        }
    }
    Some(scan)
}

/// Consumes a `[[ … ]]` long bracket, at any `[=[ … ]=]` level.
fn lua_long_bracket(source: &[u8], at: usize) -> Option<usize> {
    if source.get(at) != Some(&b'[') {
        return None;
    }
    let mut scan = at + 1;
    let first_equals = scan;
    while source.get(scan) == Some(&b'=') {
        scan += 1;
    }
    let level = scan - first_equals;
    if source.get(scan) != Some(&b'[') {
        return None;
    }
    scan += 1;
    while scan < source.len() {
        if source[scan] == b']' {
            let closing = scan + 1;
            let seen = source[closing..]
                .iter()
                .take(level)
                .take_while(|byte| **byte == b'=')
                .count();
            if seen == level && source.get(closing + level) == Some(&b']') {
                return Some(closing + level + 1);
            }
        }
        scan += 1;
    }
    Some(source.len())
}

fn only_blanks_before(source: &[u8], at: usize) -> bool {
    source[..at]
        .iter()
        .rev()
        .take_while(|byte| **byte != b'\n')
        .all(|byte| byte.is_ascii_whitespace())
}

/// Classifies the colorable spans in arbitrary source bytes.
///
/// Comments, strings and directives follow `config.language`; keywords, types
/// and words follow its word lists.  Strings and line comments may end at EOF,
/// escapes are honored, and no scan can advance beyond `source.len()`,
/// including on malformed input.
pub fn tokens(source: &[u8], config: &SyntaxConfig) -> Vec<SyntaxToken> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < source.len() {
        let scanned = match config.language {
            Language::C | Language::Cpp => c_scan(source, at),
            Language::Rust => rust_scan(source, at),
            Language::Python => python_scan(source, at),
            Language::Bash => bash_scan(source, at),
            Language::Vim => vim_scan(source, at),
            Language::Lua => lua_scan(source, at),
            Language::Json => json_scan(source, at),
        };
        if let Some(scan) = scanned {
            // Every scanner consumes at least the byte it started on, so a
            // construct it could not close can never stall the loop.
            let end = scan.end.clamp(at + 1, source.len());
            if let Some(kind) = scan.kind {
                result.push(SyntaxToken {
                    kind,
                    start: at,
                    end,
                });
            }
            at = end;
            continue;
        }

        if is_word_byte(source[at]) {
            let start = at;
            at = word_end(source, at + 1);
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

    /// The `.cyntax` format is language independent, and C is its historical
    /// default, so the format cases below stay spelled the way they were.
    fn parse(source: &[u8]) -> Result<SyntaxConfig, SyntaxError> {
        super::parse(source, Language::C)
    }

    fn load(path: &Path) -> Result<SyntaxConfig, SyntaxError> {
        super::load(path, Language::C)
    }

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

    /// Renders one tag per source byte so a whole scan can be asserted at
    /// once.  Newlines pass through to keep multi-line cases readable.
    fn tags(source: &str, language: Language) -> String {
        let config = SyntaxConfig::for_language(language);
        let mut tags = vec![b'.'; source.len()];
        for token in tokens(source.as_bytes(), &config) {
            let tag = match token.kind {
                SyntaxKind::Keyword => b'K',
                SyntaxKind::Type => b'T',
                SyntaxKind::Preprocessor => b'P',
                SyntaxKind::String => b'S',
                SyntaxKind::Comment => b'C',
                SyntaxKind::Word => b'W',
            };
            tags[token.start..token.end].fill(tag);
        }
        source
            .bytes()
            .zip(tags)
            .map(|(byte, tag)| if byte == b'\n' { '\n' } else { char::from(tag) })
            .collect()
    }

    #[test]
    fn extensions_select_their_language_case_insensitively() {
        for (extension, expected) in [
            ("c", Language::C),
            ("H", Language::C),
            ("cpp", Language::Cpp),
            ("HXX", Language::Cpp),
            ("rs", Language::Rust),
            ("py", Language::Python),
            ("pyi", Language::Python),
            ("json", Language::Json),
        ] {
            assert_eq!(Language::for_extension(extension), Some(expected));
        }
        // An extension Cano does not know stays uncolored unless a `.cyntax`
        // file supplies a palette for it.
        assert_eq!(Language::for_extension("go"), None);
        assert_eq!(Language::for_extension(""), None);
    }

    #[test]
    fn json_colors_strings_numbers_and_literals_without_inventing_comments() {
        assert_eq!(
            tags(
                r#"{"name":"cano","n":-12.5e+2,"ok":true,"x":null}"#,
                Language::Json
            ),
            ".SSSSSS.SSSSSS.SSS.TTTTTTTT.SSSS.KKKK.SSS.KKKK."
        );
        assert_eq!(
            tags("{\"url\":\"a//b\"} // plain", Language::Json),
            ".SSSSS.SSSSSS.........."
        );
        assert_eq!(tags("- 3. 4e+", Language::Json), "..T..T..");
    }

    #[test]
    fn rust_tells_lifetimes_from_character_literals() {
        // A lifetime read as a literal would color everything up to the next
        // apostrophe, which is the whole rest of the signature here.
        assert_eq!(
            tags("fn f<'a>(x: &'a str) -> &'a str", Language::Rust),
            "KK..............TTT.........TTT"
        );
        assert_eq!(tags("let c = 'x';", Language::Rust), "KKK.....SSS.");
        assert_eq!(tags("let n = '\\n';", Language::Rust), "KKK.....SSSS.");
    }

    #[test]
    fn rust_scans_raw_strings_attributes_and_nested_comments() {
        assert_eq!(
            tags(r#"let s = r"a\b";"#, Language::Rust),
            "KKK.....SSSSSS."
        );
        assert_eq!(
            tags(r##"let s = r#"has "quotes""#;"##, Language::Rust),
            "KKK.....SSSSSSSSSSSSSSSSS."
        );
        assert_eq!(tags("#[derive(Debug)]", Language::Rust), "PPPPPPPPPPPPPPPP");
        assert_eq!(tags("#![no_std]", Language::Rust), "PPPPPPPPPP");
        // Rust block comments nest; stopping at the first `*/` would leave the
        // tail of the line colored as code.
        assert_eq!(
            tags("/* a /* b */ c */ d", Language::Rust),
            "CCCCCCCCCCCCCCCCC.."
        );
        assert_eq!(
            tags("i32 usize Option Self", Language::Rust),
            "TTT.TTTTT.TTTTTT.TTTT"
        );
    }

    #[test]
    fn python_uses_hash_comments_and_leaves_floor_division_alone() {
        assert_eq!(tags("# note", Language::Python), "CCCCCC");
        // `//` is floor division; treating it as a comment would grey out the
        // rest of every line that divides.
        assert_eq!(tags("halves = a // b", Language::Python), "...............");
        assert_eq!(tags("@decorator", Language::Python), "PPPPPPPPPP");
        // An `@` that does not open its line is matrix multiplication.
        assert_eq!(tags("m = a @ b", Language::Python), ".........");
    }

    #[test]
    fn python_scans_triple_quoted_and_prefixed_strings() {
        assert_eq!(
            tags("\"\"\"doc\n# not a comment\n\"\"\"\nx", Language::Python),
            "SSSSSS\nSSSSSSSSSSSSSSS\nSSS\n."
        );
        assert_eq!(tags("f\"a {b} c\"", Language::Python), "SSSSSSSSSS");
        assert_eq!(tags("rb'raw'", Language::Python), "SSSSSSS");
        // A prefix letter only binds to a quote that follows it immediately.
        assert_eq!(tags("for x in y", Language::Python), "KKK...KK..");
        // An unclosed quote stops at its line instead of coloring the file.
        assert_eq!(
            tags("a = 'open\nreturn", Language::Python),
            "....SSSSS\nKKKKKK"
        );
    }

    #[test]
    fn well_known_names_carry_their_language_without_an_extension() {
        use std::path::Path;

        assert_eq!(
            Language::for_path(Path::new("~/.vimrc")),
            Some(Language::Vim)
        );
        assert_eq!(Language::for_path(Path::new("_vimrc")), Some(Language::Vim));
        assert_eq!(
            Language::for_path(Path::new("/home/u/.bashrc")),
            Some(Language::Bash)
        );
        assert_eq!(
            Language::for_path(Path::new(".zshrc")),
            Some(Language::Bash)
        );
        // Extensions still work, and still lose to nothing when unknown.
        assert_eq!(Language::for_path(Path::new("a.lua")), Some(Language::Lua));
        assert_eq!(Language::for_path(Path::new("a.sh")), Some(Language::Bash));
        assert_eq!(Language::for_path(Path::new("a.vim")), Some(Language::Vim));
        assert_eq!(Language::for_path(Path::new("notes.txt")), None);
        assert_eq!(Language::for_path(Path::new("plain")), None);
    }

    #[test]
    fn bash_comments_only_open_at_the_start_of_a_word() {
        assert_eq!(tags("#!/bin/sh", Language::Bash), "CCCCCCCCC");
        assert_eq!(tags("echo hi  # note", Language::Bash), "TTTT.....CCCCCC");
        // A `#` inside a word is parameter expansion, not a comment; treating
        // it as one would grey out the rest of the line.
        assert_eq!(tags("${name#pre} x", Language::Bash), "PPPPPPPPPPP..");
        assert_eq!(tags("echo $? $HOME", Language::Bash), "TTTT.PP.PPPPP");
        assert_eq!(tags("if true; then fi", Language::Bash), "KK.TTTT..KKKK.KK");
    }

    #[test]
    fn bash_quotes_follow_the_shell_rules() {
        // Single quotes are literal, so a backslash cannot end them early.
        assert_eq!(tags("a='x\\' b", Language::Bash), "..SSSS..");
        assert_eq!(tags("a=\"x $v\"", Language::Bash), "..SSSSSS");
        // An unbalanced single quote stops at its line rather than the file.
        assert_eq!(tags("a='open\nnext", Language::Bash), ".......\n....");
    }

    #[test]
    fn vim_tells_a_comment_quote_from_a_string_quote() {
        assert_eq!(tags("\" a note", Language::Vim), "CCCCCCCC");
        assert_eq!(tags("let x = \"v\"", Language::Vim), "KKK.....SSS");
        // A quote that never closes is a trailing comment, which is how most
        // of a vimrc is annotated.
        assert_eq!(tags("set number \" why", Language::Vim), "KKK.TTTTTT.CCCCC");
        // Key notation is its own thing, and `a < b` is not key notation.
        assert_eq!(tags("map <leader>i *", Language::Vim), "KKK.PPPPPPPP...");
        assert_eq!(tags("if a < b", Language::Vim), "KK......");
    }

    #[test]
    fn lua_handles_long_brackets_for_both_comments_and_strings() {
        assert_eq!(tags("-- note", Language::Lua), "CCCCCCC");
        assert_eq!(tags("--[[ a\nb ]] x", Language::Lua), "CCCCCC\nCCCC..");
        assert_eq!(tags("s = [[a\nb]]", Language::Lua), "....SSS\nSSS");
        assert_eq!(tags("s = [==[a]==]", Language::Lua), "....SSSSSSSSS");
        assert_eq!(tags("local x = \"t\"", Language::Lua), "KKKKK.....SSS");
        // A bare `-` is arithmetic, and `[` alone is an index.
        assert_eq!(tags("x = a-b", Language::Lua), ".......");
        assert_eq!(tags("t[1] = math", Language::Lua), ".......TTTT");
    }

    #[test]
    fn cpp_extends_the_c_lists_without_disturbing_them() {
        let c = SyntaxConfig::for_language(Language::C);
        let cpp = SyntaxConfig::for_language(Language::Cpp);
        assert!(in_group(b"return", &c.keyword) && in_group(b"return", &cpp.keyword));
        assert!(!in_group(b"class", &c.keyword) && in_group(b"class", &cpp.keyword));
        assert!(!in_group(b"string", &c.type_name) && in_group(b"string", &cpp.type_name));
        assert_eq!(
            tags("template <class T> constexpr size_t n = 0;", Language::Cpp),
            "KKKKKKKK..KKKKK....KKKKKKKKK.TTTTTT......."
        );
        // C++ shares C's scanner, so directives and comments are unchanged.
        assert_eq!(
            tags("#include <a> // c", Language::Cpp),
            "PPPPPPPP.....CCCC"
        );
    }

    #[test]
    fn cyntax_word_defaults_follow_the_language_it_is_parsed_for() {
        let rust = super::parse(b"k,1,2,3.t,4,5,6.", Language::Rust).unwrap();
        assert_eq!(rust.language, Language::Rust);
        assert!(in_group(b"fn", &rust.keyword));
        assert!(in_group(b"usize", &rust.type_name));
        assert!(!in_group(b"typedef", &rust.keyword));

        // A palette still only supplies colors and words: it cannot change
        // how the source is lexed.
        let python = super::parse(b"k,9,9,9,def.", Language::Python).unwrap();
        assert_eq!(python.keyword.words, [b"def".to_vec()]);
        assert_eq!(python.keyword.color, Rgb::new(9, 9, 9));
        assert_eq!(python.language, Language::Python);
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
