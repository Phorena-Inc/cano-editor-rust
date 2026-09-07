//! Cano's small command language.
//!
//! The language is deliberately byte based.  Paths, mapping expansions, and
//! variable names therefore do not have to be UTF-8.  Parsing is kept free of
//! filesystem and process effects; [`CommandState::apply`] returns those as a
//! typed [`ExternalEffect`].

use std::error::Error;
use std::fmt;

use crate::listchars::ListChars;

/// A half-open byte span in the original command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// The lexical family assigned to a command token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Command,
    Config,
    Operator,
    Integer,
    Float,
    String,
    SpecialKey,
    Identifier,
}

impl TokenKind {
    fn name(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Config => "config",
            Self::Operator => "operator",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::String => "string",
            Self::SpecialKey => "special key",
            Self::Identifier => "identifier",
        }
    }
}

/// One owned command token and its location in the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub bytes: Vec<u8>,
    pub span: Span,
}

impl Token {
    /// Returns the token payload.  Quotes are removed and the two useful
    /// quote escapes (`\\` and an escaped quote) are decoded.
    pub fn value(&self) -> Vec<u8> {
        if self.kind != TokenKind::String || self.bytes.len() < 2 {
            return self.bytes.clone();
        }

        let quote = self.bytes[0];
        let mut value = Vec::with_capacity(self.bytes.len().saturating_sub(2));
        let mut at = 1;
        let end = self.bytes.len() - 1;
        while at < end {
            if self.bytes[at] == b'\\' && at + 1 < end {
                let escaped = self.bytes[at + 1];
                if escaped == quote || escaped == b'\\' {
                    value.push(escaped);
                    at += 2;
                    continue;
                }
            }
            value.push(self.bytes[at]);
            at += 1;
        }
        value
    }
}

/// A safe parsing/evaluation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    EmptyCommand,
    UnterminatedString {
        at: usize,
    },
    NotEnoughArgs,
    TooManyArgs,
    InvalidSpecialKey,
    InvalidArg {
        expected: &'static str,
        found: &'static str,
    },
    InvalidExpression,
    IntegerOverflow,
    UnknownCommand(Vec<u8>),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => f.write_str("Empty command"),
            Self::UnterminatedString { .. } => f.write_str("Unterminated string"),
            Self::NotEnoughArgs => f.write_str("Not enough args"),
            Self::TooManyArgs => f.write_str("Too many args"),
            Self::InvalidSpecialKey => f.write_str("Invalid special key"),
            Self::InvalidArg { expected, found } => {
                write!(f, "Invalid arg, expected {expected} but found {found}")
            }
            Self::InvalidExpression => f.write_str("Invalid expression"),
            Self::IntegerOverflow => f.write_str("Integer overflow"),
            Self::UnknownCommand(name) => {
                write!(f, "Unknown command: {}", String::from_utf8_lossy(name))
            }
        }
    }
}

impl Error for CommandError {}

const COMMANDS: &[&[u8]] = &[
    b"set-var",
    b"set-output",
    b"set-map",
    b"let",
    b"echo",
    b"w",
    b"q",
    b"q!",
    b"wq",
    b"e",
    b"we",
    b"nohl",
    b"nohlsearch",
    b"imap",
    b"autoformat",
    b"Autoformat",
];

const CONFIGS: &[&[u8]] = &[
    b"syntax",
    b"relative",
    b"auto_indent",
    b"indent",
    b"undo_size",
    b"cursorline",
    b"mouse",
    b"backup",
    b"list",
    b"autoformat_autoindent",
    b"autoformat_retab",
    b"autoformat_remove_trailing_spaces",
];

fn is_config(bytes: &[u8]) -> bool {
    CONFIGS.contains(&bytes) || matches!(bytes, b"auto-indent" | b"undo-size" | b"cursor-line")
}

fn classify(bytes: &[u8]) -> TokenKind {
    if COMMANDS.contains(&bytes) {
        TokenKind::Command
    } else if is_config(bytes) {
        TokenKind::Config
    } else if matches!(bytes, b"+" | b"-" | b"*" | b"/" | b"=") {
        TokenKind::Operator
    } else if bytes.first().is_some_and(u8::is_ascii_digit) {
        if bytes.contains(&b'.') {
            TokenKind::Float
        } else {
            TokenKind::Integer
        }
    } else {
        TokenKind::Identifier
    }
}

/// Tokenizes one command line.
///
/// Quoted and angle-bracketed values stay in one token and all spans use byte
/// offsets.  Unlike the legacy implementation, consecutive whitespace is
/// coalesced and can never create an empty-token dereference.
pub fn lex(input: &[u8]) -> Result<Vec<Token>, CommandError> {
    let mut result = Vec::new();
    let mut at = 0;

    while at < input.len() {
        while input.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if at == input.len() {
            break;
        }

        let start = at;
        let kind;
        if matches!(input[at], b'\'' | b'"') {
            let quote = input[at];
            at += 1;
            let mut escaped = false;
            let mut terminated = false;
            while at < input.len() {
                let byte = input[at];
                at += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == quote {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(CommandError::UnterminatedString { at: start });
            }
            kind = TokenKind::String;
        } else if input[at] == b'<' {
            at += 1;
            while at < input.len() && input[at] != b'>' {
                at += 1;
            }
            if at < input.len() {
                at += 1;
            }
            kind = TokenKind::SpecialKey;
        } else {
            while at < input.len() && !input[at].is_ascii_whitespace() {
                at += 1;
            }
            kind = classify(&input[start..at]);
        }

        result.push(Token {
            kind,
            bytes: input[start..at].to_vec(),
            span: Span { start, end: at },
        });
    }

    Ok(result)
}

/// A configuration variable accepted by `set-var`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigVariable {
    Syntax,
    Relative,
    AutoIndent,
    Indent,
    UndoSize,
    CursorLine,
    Mouse,
    Backup,
    List,
    AutoFormatIndent,
    AutoFormatRetab,
    AutoFormatTrailing,
}

impl ConfigVariable {
    /// Resolves an option name, accepting vim's spelling alongside Cano's
    /// where the two differ, so a `:set` line copied out of a vimrc works.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"syntax" | b"syn" => Some(Self::Syntax),
            b"relative" | b"relativenumber" | b"rnu" => Some(Self::Relative),
            b"auto_indent" | b"auto-indent" | b"autoindent" | b"ai" => Some(Self::AutoIndent),
            b"indent" | b"shiftwidth" | b"sw" | b"tabstop" | b"ts" => Some(Self::Indent),
            b"undo_size" | b"undo-size" => Some(Self::UndoSize),
            b"cursorline" | b"cursor-line" | b"cul" => Some(Self::CursorLine),
            b"mouse" => Some(Self::Mouse),
            b"backup" | b"bk" => Some(Self::Backup),
            b"list" => Some(Self::List),
            b"autoformat_autoindent" => Some(Self::AutoFormatIndent),
            b"autoformat_retab" => Some(Self::AutoFormatRetab),
            b"autoformat_remove_trailing_spaces" => Some(Self::AutoFormatTrailing),
            _ => None,
        }
    }
}

/// A value displayed by `echo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EchoValue {
    Variable(Vec<u8>),
    Literal(Vec<u8>),
}

/// A parsed, side-effect-free command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SetVar {
        variable: ConfigVariable,
        value: i64,
    },
    SetOutput(Vec<u8>),
    SetMap {
        key: i32,
        expansion: Vec<u8>,
    },
    Let {
        name: Vec<u8>,
        value: i64,
    },
    Echo(EchoValue),
    Write,
    Quit {
        force: bool,
    },
    Exit,
    WriteExit,
    /// `:nohl`, which stops showing the current search highlight without
    /// forgetting the pattern `n` repeats.
    NoHighlight,
    /// `:imap`, an Insert-mode mapping whose left-hand side may be several
    /// keys long.
    InsertMap {
        from: Vec<u8>,
        to: Vec<u8>,
    },
    /// `:autoformat`, which rewrites the whole buffer's whitespace.
    AutoFormat,
}

fn invalid(expected: &'static str, token: &Token) -> CommandError {
    CommandError::InvalidArg {
        expected,
        found: token.kind.name(),
    }
}

fn exact_arity(tokens: &[Token], count: usize) -> Result<(), CommandError> {
    if tokens.len() < count {
        Err(CommandError::NotEnoughArgs)
    } else if tokens.len() > count {
        Err(CommandError::TooManyArgs)
    } else {
        Ok(())
    }
}

fn parse_integer_prefix(bytes: &[u8]) -> Result<i64, CommandError> {
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .copied()
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return Err(CommandError::InvalidExpression);
    }
    let text = std::str::from_utf8(&digits).map_err(|_| CommandError::InvalidExpression)?;
    text.parse::<i64>()
        .map_err(|_| CommandError::IntegerOverflow)
}

fn expression(tokens: &[Token]) -> Result<i64, CommandError> {
    let Some(first) = tokens.first() else {
        return Err(CommandError::NotEnoughArgs);
    };
    if first.kind != TokenKind::Integer {
        return Err(invalid("integer", first));
    }
    if tokens.len().is_multiple_of(2) {
        return Err(CommandError::InvalidExpression);
    }

    let mut value = parse_integer_prefix(&first.bytes)?;
    for pair in tokens[1..].chunks_exact(2) {
        let operator = &pair[0];
        let rhs_token = &pair[1];
        if operator.kind != TokenKind::Operator
            || !matches!(operator.bytes.as_slice(), b"+" | b"-" | b"*" | b"/")
        {
            return Err(invalid("operator", operator));
        }
        if rhs_token.kind != TokenKind::Integer {
            return Err(invalid("integer", rhs_token));
        }
        let rhs = parse_integer_prefix(&rhs_token.bytes)?;

        // Source compatibility: a literal zero right operand caused the
        // direct operation to be skipped.  A zero arising from an evaluated
        // divisor is still reported by callers rather than allowed to trap.
        if operator.bytes == b"/" && rhs == 0 {
            continue;
        }

        // Division cannot fail here: a zero divisor was skipped above and the
        // digit-only parser never yields a negative right operand, so only
        // the additive/multiplicative operators can overflow.
        value = match operator.bytes.as_slice() {
            b"+" => value.checked_add(rhs),
            b"-" => value.checked_sub(rhs),
            b"*" => value.checked_mul(rhs),
            b"/" => value.checked_div(rhs),
            _ => unreachable!("operator was checked above"),
        }
        .ok_or(CommandError::IntegerOverflow)?;
    }
    Ok(value)
}

/// Stable key values used for named mapping keys.  They match the common
/// ncurses values used by Cano while ordinary and control keys retain their
/// byte value.
pub mod key {
    pub const DOWN: i32 = 258;
    pub const UP: i32 = 259;
    pub const LEFT: i32 = 260;
    pub const RIGHT: i32 = 261;
    pub const HOME: i32 = 262;
    pub const BACKSPACE: i32 = 263;
    pub const DELETE: i32 = 330;
    pub const INSERT: i32 = 331;
    pub const PAGE_DOWN: i32 = 338;
    pub const PAGE_UP: i32 = 339;
    pub const END: i32 = 360;
}

/// Decodes a `<...>` key spelling into the integer used by the mapping table.
pub fn decode_special_key(bytes: &[u8]) -> Result<i32, CommandError> {
    if bytes.len() < 3 || bytes.first() != Some(&b'<') || bytes.last() != Some(&b'>') {
        return Err(CommandError::InvalidSpecialKey);
    }
    let inside = &bytes[1..bytes.len() - 1];
    // Both the short `<c-x>` and the documented long `<ctrl-x>` spellings
    // name a control chord.
    let chord = if inside.len() == 3 && inside[0].eq_ignore_ascii_case(&b'c') && inside[1] == b'-' {
        Some(inside[2])
    } else if inside.len() == 6 && inside[..5].eq_ignore_ascii_case(b"ctrl-") {
        Some(inside[5])
    } else {
        None
    };
    if let Some(byte) = chord.filter(u8::is_ascii) {
        return Ok(match byte {
            b'?' => 127,
            byte => i32::from(byte.to_ascii_uppercase() & 0x1f),
        });
    }

    let lower = inside.to_ascii_lowercase();
    let value = match lower.as_slice() {
        b"esc" | b"escape" => 27,
        b"tab" => 9,
        b"cr" | b"enter" | b"return" => 10,
        b"space" => 32,
        b"nul" => 0,
        b"bs" | b"backspace" => key::BACKSPACE,
        b"up" => key::UP,
        b"down" => key::DOWN,
        b"left" => key::LEFT,
        b"right" => key::RIGHT,
        b"home" => key::HOME,
        b"end" => key::END,
        b"delete" | b"del" => key::DELETE,
        b"insert" | b"ins" => key::INSERT,
        b"pageup" | b"page-up" => key::PAGE_UP,
        b"pagedown" | b"page-down" => key::PAGE_DOWN,
        _ => return Err(CommandError::InvalidSpecialKey),
    };
    Ok(value)
}

fn mapping_key(token: &Token) -> Result<i32, CommandError> {
    match token.kind {
        TokenKind::SpecialKey => decode_special_key(&token.bytes),
        TokenKind::String | TokenKind::Identifier => {
            let value = token.value();
            if value.len() == 1 {
                Ok(i32::from(value[0]))
            } else {
                Err(CommandError::InvalidSpecialKey)
            }
        }
        _ => Err(invalid("special key", token)),
    }
}

/// Parses a tokenized command line into an action.
pub fn parse(tokens: &[Token]) -> Result<Action, CommandError> {
    let Some(command) = tokens.first() else {
        return Err(CommandError::EmptyCommand);
    };
    let name = command.bytes.as_slice();
    match name {
        b"set-var" => {
            if tokens.len() < 3 {
                return Err(CommandError::NotEnoughArgs);
            }
            let variable = ConfigVariable::parse(&tokens[1].bytes)
                .ok_or_else(|| invalid("config", &tokens[1]))?;
            if tokens[2].kind != TokenKind::Integer {
                return Err(invalid("integer", &tokens[2]));
            }
            Ok(Action::SetVar {
                variable,
                value: expression(&tokens[2..])?,
            })
        }
        b"set-output" => {
            exact_arity(tokens, 2)?;
            if tokens[1].kind != TokenKind::String {
                return Err(invalid("string", &tokens[1]));
            }
            Ok(Action::SetOutput(tokens[1].value()))
        }
        b"set-map" => {
            if tokens.len() != 3 {
                // This counter-intuitive diagnostic is observable legacy
                // behavior for both too few and too many mapping arguments.
                return Err(CommandError::NotEnoughArgs);
            }
            let key = mapping_key(&tokens[1])?;
            if tokens[2].kind != TokenKind::String {
                return Err(invalid("string", &tokens[2]));
            }
            Ok(Action::SetMap {
                key,
                expansion: tokens[2].value(),
            })
        }
        b"let" => {
            if tokens.len() < 3 {
                return Err(CommandError::NotEnoughArgs);
            }
            if !matches!(tokens[1].kind, TokenKind::Identifier | TokenKind::String) {
                return Err(invalid("identifier", &tokens[1]));
            }
            let expression_start = if tokens.get(2).is_some_and(|token| token.bytes == b"=") {
                3
            } else {
                2
            };
            if expression_start == tokens.len() {
                return Err(CommandError::NotEnoughArgs);
            }
            Ok(Action::Let {
                name: tokens[1].value(),
                value: expression(&tokens[expression_start..])?,
            })
        }
        b"echo" => {
            exact_arity(tokens, 2)?;
            let value = if tokens[1].kind == TokenKind::String {
                EchoValue::Literal(tokens[1].value())
            } else {
                EchoValue::Variable(tokens[1].value())
            };
            Ok(Action::Echo(value))
        }
        b"w" => {
            exact_arity(tokens, 1)?;
            Ok(Action::Write)
        }
        b"q" => {
            exact_arity(tokens, 1)?;
            Ok(Action::Quit { force: false })
        }
        b"q!" => {
            exact_arity(tokens, 1)?;
            Ok(Action::Quit { force: true })
        }
        b"wq" => {
            exact_arity(tokens, 1)?;
            Ok(Action::WriteExit)
        }
        b"e" => {
            exact_arity(tokens, 1)?;
            Ok(Action::Exit)
        }
        b"imap" => {
            if tokens.len() < 3 {
                return Err(CommandError::NotEnoughArgs);
            }
            if !matches!(tokens[1].kind, TokenKind::Identifier | TokenKind::String) {
                return Err(invalid("identifier", &tokens[1]));
            }
            let from = tokens[1].value();
            if from.is_empty() {
                return Err(CommandError::NotEnoughArgs);
            }
            // The right-hand side is a key sequence: `<...>` spellings become
            // their byte, everything else is taken literally.  Whitespace
            // between tokens is a separator, so a run of spaces has to be
            // quoted to survive.
            let mut to = Vec::new();
            for token in &tokens[2..] {
                if token.kind == TokenKind::SpecialKey {
                    let code = decode_special_key(&token.bytes)?;
                    // Replay feeds the right-hand side back through the input
                    // path one byte at a time, so a key with no byte spelling
                    // (an arrow, say) cannot be expressed here.
                    let byte = u8::try_from(code).map_err(|_| CommandError::InvalidSpecialKey)?;
                    to.push(byte);
                } else {
                    to.extend_from_slice(&token.value());
                }
            }
            Ok(Action::InsertMap { from, to })
        }
        b"autoformat" | b"Autoformat" => {
            exact_arity(tokens, 1)?;
            Ok(Action::AutoFormat)
        }
        b"nohl" | b"nohlsearch" => {
            exact_arity(tokens, 1)?;
            Ok(Action::NoHighlight)
        }
        b"we" => {
            exact_arity(tokens, 1)?;
            Ok(Action::WriteExit)
        }
        _ => Err(CommandError::UnknownCommand(command.bytes.clone())),
    }
}

/// An operation deliberately left for the application boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEffect {
    Save(Vec<u8>),
    /// Stop showing the search highlight.  The pattern itself belongs to the
    /// application, so the command state only reports the request.
    ClearHighlight,
    /// Rewrite the buffer's whitespace.  The buffer belongs to the
    /// application, so the command state only reports the request.
    AutoFormat,
}

/// One Insert-mode mapping, as declared by `:imap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertMap {
    /// The keys that trigger it, which may be more than one.
    pub from: Vec<u8>,
    /// The keys replayed in their place.
    pub to: Vec<u8>,
}

/// One key mapping.  Expansions include the source-compatible trailing NUL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMap {
    pub key: i32,
    pub expansion: Vec<u8>,
}

/// One integer variable.  Entries are retained in insertion order so `echo`
/// resolves the first duplicate, matching the characterized implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: Vec<u8>,
    pub value: i64,
}

/// Mutable state owned by the command layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandState {
    pub syntax: i64,
    pub relative: i64,
    pub auto_indent: i64,
    pub indent: i64,
    pub undo_size: i64,
    /// Vim's `cursorline`, off by default the way vim leaves it.
    pub cursorline: i64,
    /// Terminal mouse reporting.  On by default; turning it off hands the
    /// mouse back to the terminal, whose own selection it otherwise takes.
    pub mouse: i64,
    /// Keep a copy of what a save is about to overwrite.  On by default,
    /// because the copy is only ever wanted after it is too late to ask for.
    pub backup: i64,
    /// Vim's `list`: draw the invisible characters named by `listchars`.
    pub list: i64,
    /// Vim's `listchars`, which decides what `list` draws.
    pub listchars: ListChars,
    /// The three steps `:autoformat` runs, each on as the plugin has them.
    pub autoformat_autoindent: i64,
    pub autoformat_retab: i64,
    pub autoformat_remove_trailing_spaces: i64,
    pub output: Vec<u8>,
    pub maps: Vec<KeyMap>,
    /// Insert-mode mappings, in the order they were declared.
    pub insert_maps: Vec<InsertMap>,
    pub variables: Vec<Variable>,
    pub message: Option<String>,
    pub quit: bool,
}

impl Default for CommandState {
    fn default() -> Self {
        Self::new(b"out.txt".to_vec())
    }
}

impl CommandState {
    pub fn new(output: Vec<u8>) -> Self {
        Self {
            syntax: 1,
            relative: 0,
            auto_indent: 1,
            indent: 4,
            undo_size: 32,
            cursorline: 0,
            mouse: 1,
            backup: 1,
            list: 0,
            listchars: ListChars::default(),
            autoformat_autoindent: 1,
            autoformat_retab: 1,
            autoformat_remove_trailing_spaces: 1,
            output,
            maps: Vec::new(),
            insert_maps: Vec::new(),
            variables: Vec::new(),
            message: None,
            quit: false,
        }
    }

    /// The current value of one option, which `:set name!` toggles.
    pub fn variable(&self, variable: ConfigVariable) -> i64 {
        match variable {
            ConfigVariable::Syntax => self.syntax,
            ConfigVariable::Relative => self.relative,
            ConfigVariable::AutoIndent => self.auto_indent,
            ConfigVariable::Indent => self.indent,
            ConfigVariable::UndoSize => self.undo_size,
            ConfigVariable::CursorLine => self.cursorline,
            ConfigVariable::Mouse => self.mouse,
            ConfigVariable::Backup => self.backup,
            ConfigVariable::List => self.list,
            ConfigVariable::AutoFormatIndent => self.autoformat_autoindent,
            ConfigVariable::AutoFormatRetab => self.autoformat_retab,
            ConfigVariable::AutoFormatTrailing => self.autoformat_remove_trailing_spaces,
        }
    }

    /// The Insert mapping the keys just typed complete exactly.
    ///
    /// The first match wins rather than the longest: without an input timeout
    /// there is nothing to wait on, so a mapping fires as soon as its keys are
    /// all in.  A longer mapping sharing a shorter one's prefix is therefore
    /// unreachable.
    pub fn insert_map(&self, typed: &[u8]) -> Option<&InsertMap> {
        self.insert_maps.iter().find(|map| map.from == typed)
    }

    /// Whether more keys could still complete some Insert mapping.
    pub fn insert_prefix(&self, typed: &[u8]) -> bool {
        self.insert_maps
            .iter()
            .any(|map| map.from.starts_with(typed))
    }

    /// Returns the first mapping for a key, including its terminating NUL.
    pub fn mapping(&self, key: i32) -> Option<&[u8]> {
        self.maps
            .iter()
            .find(|mapping| mapping.key == key)
            .map(|mapping| mapping.expansion.as_slice())
    }

    /// Applies one parsed action and returns any external operation it asks
    /// the application to perform.
    pub fn apply(&mut self, action: Action) -> Result<Option<ExternalEffect>, CommandError> {
        match action {
            Action::SetVar { variable, value } => match variable {
                ConfigVariable::Syntax => self.syntax = value,
                ConfigVariable::Relative => self.relative = value,
                ConfigVariable::AutoIndent => self.auto_indent = value,
                ConfigVariable::Indent => self.indent = value,
                ConfigVariable::UndoSize => self.undo_size = value,
                ConfigVariable::CursorLine => self.cursorline = value,
                ConfigVariable::Mouse => self.mouse = value,
                ConfigVariable::Backup => self.backup = value,
                ConfigVariable::List => self.list = value,
                ConfigVariable::AutoFormatIndent => self.autoformat_autoindent = value,
                ConfigVariable::AutoFormatRetab => self.autoformat_retab = value,
                ConfigVariable::AutoFormatTrailing => {
                    self.autoformat_remove_trailing_spaces = value;
                }
            },
            Action::SetOutput(output) => self.output = output,
            Action::SetMap { key, mut expansion } => {
                expansion.push(0);
                self.maps.push(KeyMap { key, expansion });
            }
            Action::Let { name, value } => self.variables.push(Variable { name, value }),
            Action::Echo(value) => {
                self.message = Some(match value {
                    EchoValue::Literal(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    EchoValue::Variable(name) => self
                        .variables
                        .iter()
                        .find(|variable| variable.name == name)
                        .map(|variable| variable.value.to_string())
                        .unwrap_or_default(),
                });
            }
            Action::Write => return Ok(Some(ExternalEffect::Save(self.output.clone()))),
            Action::Quit { .. } => self.quit = true,
            Action::Exit => self.quit = true,
            Action::WriteExit => {
                self.quit = true;
                return Ok(Some(ExternalEffect::Save(self.output.clone())));
            }
            Action::NoHighlight => return Ok(Some(ExternalEffect::ClearHighlight)),
            Action::AutoFormat => return Ok(Some(ExternalEffect::AutoFormat)),
            Action::InsertMap { from, to } => {
                // A repeated left-hand side replaces the earlier binding, the
                // way re-running `:imap` in vim does.
                self.insert_maps.retain(|map| map.from != from);
                self.insert_maps.push(InsertMap { from, to });
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(source: &[u8]) -> Result<Action, CommandError> {
        parse(&lex(source)?)
    }

    #[test]
    fn lex_is_byte_located_and_keeps_compound_tokens_together() {
        let tokens = lex(b"  set-map   <C-X> \"two words\"  ").unwrap();
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].span, Span { start: 2, end: 9 });
        assert_eq!(tokens[1].kind, TokenKind::SpecialKey);
        assert_eq!(tokens[1].bytes, b"<C-X>");
        assert_eq!(tokens[2].kind, TokenKind::String);
        assert_eq!(tokens[2].value(), b"two words");
    }

    #[test]
    fn numeric_classification_uses_leading_digit_and_dot_quirk() {
        let tokens = lex(b"12suffix 1.2.3 nope.2").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer);
        assert_eq!(tokens[1].kind, TokenKind::Float);
        assert_eq!(tokens[2].kind, TokenKind::Identifier);
    }

    #[test]
    fn malformed_quote_is_a_bounded_error() {
        assert_eq!(
            lex(b"set-output \"unfinished"),
            Err(CommandError::UnterminatedString { at: 11 })
        );
    }

    #[test]
    fn expressions_are_left_to_right_and_skip_literal_zero_division() {
        assert_eq!(expression(&lex(b"2 + 3 * 4").unwrap()).unwrap(), 20);
        assert_eq!(expression(&lex(b"8 / 0").unwrap()).unwrap(), 8);
    }

    #[test]
    fn set_var_accepts_the_complete_arithmetic_expression() {
        assert_eq!(
            action(b"set-var indent 2 + 3 * 4").unwrap(),
            Action::SetVar {
                variable: ConfigVariable::Indent,
                value: 20,
            }
        );
        assert_eq!(
            action(b"set-var undo_size 8 / 0").unwrap(),
            Action::SetVar {
                variable: ConfigVariable::UndoSize,
                value: 8,
            }
        );
    }

    #[test]
    fn set_var_keeps_missing_and_malformed_expression_errors() {
        assert_eq!(action(b"set-var indent"), Err(CommandError::NotEnoughArgs));
        assert_eq!(
            action(b"set-var indent 2 +"),
            Err(CommandError::InvalidExpression)
        );
        assert_eq!(
            action(b"set-var indent 2 nope 3"),
            Err(CommandError::InvalidArg {
                expected: "operator",
                found: "identifier",
            })
        );
    }

    #[test]
    fn expression_overflow_is_reported() {
        let source = format!("{} + 1", i64::MAX);
        assert_eq!(
            expression(&lex(source.as_bytes()).unwrap()),
            Err(CommandError::IntegerOverflow)
        );
    }

    #[test]
    fn imap_takes_a_multi_key_left_side_and_a_key_sequence_right_side() {
        assert_eq!(
            action(b"imap ;; <Esc>"),
            Ok(Action::InsertMap {
                from: b";;".to_vec(),
                to: vec![27]
            })
        );
        // Several right-hand tokens concatenate, so `<Esc>` can be followed
        // by more keys.
        assert_eq!(
            action(b"imap jk <Esc> :w <CR>"),
            Ok(Action::InsertMap {
                from: b"jk".to_vec(),
                to: b"\x1b:w\n".to_vec()
            })
        );
        // A quoted left-hand side keeps whatever it holds.
        assert_eq!(
            action(b"imap \",,\" x"),
            Ok(Action::InsertMap {
                from: b",,".to_vec(),
                to: b"x".to_vec()
            })
        );
        assert_eq!(action(b"imap ;;"), Err(CommandError::NotEnoughArgs));
        // Replay works a byte at a time, so a key with no byte spelling
        // cannot be the right-hand side.
        assert_eq!(
            action(b"imap ;; <Left>"),
            Err(CommandError::InvalidSpecialKey)
        );
    }

    #[test]
    fn insert_mappings_replace_a_repeated_left_side_and_match_by_prefix() {
        let mut state = CommandState::default();
        state
            .apply(Action::InsertMap {
                from: b";;".to_vec(),
                to: vec![27],
            })
            .unwrap();
        state
            .apply(Action::InsertMap {
                from: b";;".to_vec(),
                to: b"x".to_vec(),
            })
            .unwrap();

        assert_eq!(state.insert_maps.len(), 1);
        assert_eq!(
            state.insert_map(b";;").map(|map| map.to.clone()),
            Some(b"x".to_vec())
        );
        assert!(state.insert_map(b";").is_none());
        // A partial left-hand side is not a match, but it is still worth
        // waiting on.
        assert!(state.insert_prefix(b";"));
        assert!(state.insert_prefix(b";;"));
        assert!(!state.insert_prefix(b"q"));
    }

    #[test]
    fn parser_preserves_documented_arity_messages() {
        assert_eq!(action(b"set-output"), Err(CommandError::NotEnoughArgs));
        assert_eq!(
            action(b"set-output \"a\" \"b\""),
            Err(CommandError::TooManyArgs)
        );
        assert_eq!(
            action(b"set-map x \"a\" extra"),
            Err(CommandError::NotEnoughArgs)
        );
    }

    #[test]
    fn invalid_special_keys_are_typed() {
        assert_eq!(
            action(b"set-map <not-a-key> \"x\""),
            Err(CommandError::InvalidSpecialKey)
        );
        assert_eq!(decode_special_key(b"<C-Q>").unwrap(), 17);
        assert_eq!(decode_special_key(b"<left>").unwrap(), key::LEFT);
    }

    #[test]
    fn all_actions_parse() {
        assert_eq!(
            action(b"set-var indent 8").unwrap(),
            Action::SetVar {
                variable: ConfigVariable::Indent,
                value: 8
            }
        );
        assert_eq!(
            action(b"let answer = 2 + 3 * 4").unwrap(),
            Action::Let {
                name: b"answer".to_vec(),
                value: 20
            }
        );
        assert_eq!(action(b"w").unwrap(), Action::Write);
        assert_eq!(action(b"q").unwrap(), Action::Quit { force: false });
        assert_eq!(action(b"q!").unwrap(), Action::Quit { force: true });
        assert_eq!(action(b"wq").unwrap(), Action::WriteExit);
        assert_eq!(action(b"e").unwrap(), Action::Exit);
        assert_eq!(action(b"we").unwrap(), Action::WriteExit);
    }

    #[test]
    fn vim_file_commands_are_classified_and_take_no_arguments() {
        for source in [b"q".as_slice(), b"q!", b"w", b"wq"] {
            assert_eq!(lex(source).unwrap()[0].kind, TokenKind::Command);
        }
        for source in [b"q later".as_slice(), b"q! later", b"w later", b"wq later"] {
            assert_eq!(action(source), Err(CommandError::TooManyArgs));
        }
    }

    #[test]
    fn state_keeps_duplicate_variables_and_echoes_the_first() {
        let mut state = CommandState::default();
        state.apply(action(b"let value 1").unwrap()).unwrap();
        state.apply(action(b"let value 2").unwrap()).unwrap();
        state.apply(action(b"echo value").unwrap()).unwrap();
        assert_eq!(state.variables.len(), 2);
        assert_eq!(state.message.as_deref(), Some("1"));
    }

    #[test]
    fn mappings_keep_duplicate_order_and_include_the_allocated_nul() {
        let mut state = CommandState::default();
        state
            .apply(action(b"set-map <C-X> \"ab\"").unwrap())
            .unwrap();
        state
            .apply(action(b"set-map <C-X> \"later\"").unwrap())
            .unwrap();
        assert_eq!(state.mapping(24), Some(b"ab\0".as_slice()));
    }

    #[test]
    fn save_and_exit_are_kept_as_typed_state_and_effects() {
        let mut state = CommandState::new(b"first".to_vec());
        state
            .apply(action(b"set-output \"next file\"").unwrap())
            .unwrap();
        assert_eq!(
            state.apply(Action::Write).unwrap(),
            Some(ExternalEffect::Save(b"next file".to_vec()))
        );
        assert_eq!(
            state.apply(Action::WriteExit).unwrap(),
            Some(ExternalEffect::Save(b"next file".to_vec()))
        );
        assert!(state.quit);
    }
}
