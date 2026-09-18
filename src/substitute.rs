//! Vim's `:s` substitute command.
//!
//! The command line is parsed here rather than by Cano's token language,
//! because a substitution is delimiter-structured rather than
//! whitespace-structured: `:%s/two words/one/g` is one command with spaces in
//! it, and the token lexer would tear it into pieces.
//!
//! Patterns are literal text, not regular expressions, with the two
//! zero-width assertions vim spells `\<` and `\>` so `\<foo\>` still means
//! whole words. Everything here is pure: the caller supplies the buffer.

use std::fmt;

use crate::buffer::{Buffer, is_word};

/// Which lines a substitution covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Range {
    /// No range given, so just the line the cursor is on.
    CurrentLine,
    /// `%`, the whole file.
    WholeFile,
    /// An explicit `N,M`, one-based and inclusive.
    Lines(usize, usize),
}

/// The trailing flag letters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Flags {
    /// `g`: every match on a line rather than only the first.
    pub global: bool,
    /// `c`: ask before each replacement.
    pub confirm: bool,
    /// `i`: match without regard to case.
    pub ignore_case: bool,
}

/// A literal search pattern, with vim's two word-boundary assertions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pattern {
    text: Vec<u8>,
    /// `\<`: the match must begin at the start of a word.
    start_boundary: bool,
    /// `\>`: the match must end at the end of a word.
    end_boundary: bool,
    ignore_case: bool,
}

impl Pattern {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    fn matches_at(&self, data: &[u8], at: usize) -> bool {
        let Some(candidate) = data.get(at..at.saturating_add(self.text.len())) else {
            return false;
        };
        let same = if self.ignore_case {
            candidate.eq_ignore_ascii_case(&self.text)
        } else {
            candidate == self.text
        };
        if !same {
            return false;
        }
        if self.start_boundary
            && at
                .checked_sub(1)
                .and_then(|index| data.get(index))
                .is_some_and(|byte| is_word(*byte))
        {
            return false;
        }
        if self.end_boundary
            && data
                .get(at + self.text.len())
                .is_some_and(|byte| is_word(*byte))
        {
            return false;
        }
        true
    }

    /// The start of the first match at or after `from`, within `limit`.
    pub fn find(&self, data: &[u8], from: usize, limit: usize) -> Option<usize> {
        if self.text.is_empty() {
            return None;
        }
        let last = limit.min(data.len()).checked_sub(self.text.len())?;
        (from..=last).find(|at| self.matches_at(data, *at))
    }
}

/// A parsed substitute command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Substitute {
    pub range: Range,
    pub pattern: Pattern,
    pub replacement: Vec<u8>,
    pub flags: Flags,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubstituteError {
    MissingPattern,
    UnknownFlag(u8),
}

impl fmt::Display for SubstituteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPattern => f.write_str("No previous substitute pattern"),
            Self::UnknownFlag(flag) => {
                write!(f, "Unknown substitute flag: {}", char::from(*flag))
            }
        }
    }
}

impl std::error::Error for SubstituteError {}

/// Recognizes and parses a substitute command line.
///
/// Returns `None` when the line is not a substitution at all, so the caller
/// can hand it to the ordinary command language untouched.
pub fn parse(input: &[u8]) -> Option<Result<Substitute, SubstituteError>> {
    let (range, at) = parse_range(input)?;
    // `s` may be spelled out, the way vim accepts any prefix of `substitute`.
    let name = b"substitute";
    let mut scan = at;
    while scan < input.len() && scan - at < name.len() && input[scan] == name[scan - at] {
        scan += 1;
    }
    if scan == at {
        return None;
    }
    let delimiter = *input.get(scan)?;
    // A letter or digit would make `set` and friends look like substitutions.
    if !delimiter.is_ascii_punctuation() || delimiter == b'\\' {
        return None;
    }

    let (pattern, scan) = split(input, scan + 1, delimiter);
    let (replacement, scan) = split(input, scan, delimiter);
    let mut flags = Flags::default();
    for byte in &input[scan.min(input.len())..] {
        match byte {
            b'g' => flags.global = true,
            b'c' => flags.confirm = true,
            b'i' => flags.ignore_case = true,
            b' ' | b'\t' => {}
            other => return Some(Err(SubstituteError::UnknownFlag(*other))),
        }
    }

    let pattern = compile(&pattern, flags.ignore_case);
    if pattern.is_empty() {
        return Some(Err(SubstituteError::MissingPattern));
    }
    Some(Ok(Substitute {
        range,
        pattern,
        replacement,
        flags,
    }))
}

/// Reads the optional line range in front of the command name.
fn parse_range(input: &[u8]) -> Option<(Range, usize)> {
    if input.first() == Some(&b'%') {
        return Some((Range::WholeFile, 1));
    }
    let (first, at) = parse_address(input, 0)?;
    let Some(first) = first else {
        return Some((Range::CurrentLine, at));
    };
    if input.get(at) != Some(&b',') {
        return Some((Range::Lines(first, first), at));
    }
    let (second, next) = parse_address(input, at + 1)?;
    Some((Range::Lines(first, second.unwrap_or(first)), next))
}

/// One address: a line number, `.` for the current line, or `$` for the last.
///
/// `.` and `$` are reported as `usize::MAX`/`0` sentinels rather than resolved
/// here, because parsing has no buffer to resolve them against; `resolve`
/// finishes the job.
fn parse_address(input: &[u8], at: usize) -> Option<(Option<usize>, usize)> {
    match input.get(at) {
        Some(b'.') => Some((Some(CURRENT_LINE), at + 1)),
        Some(b'$') => Some((Some(LAST_LINE), at + 1)),
        Some(byte) if byte.is_ascii_digit() => {
            let digits = input[at..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            let text = std::str::from_utf8(&input[at..at + digits]).ok()?;
            Some((Some(text.parse().ok()?), at + digits))
        }
        _ => Some((None, at)),
    }
}

/// Sentinel line numbers for `.` and `$`, resolved against the buffer later.
const CURRENT_LINE: usize = usize::MAX;
const LAST_LINE: usize = usize::MAX - 1;

/// Reads up to the next unescaped delimiter, decoding `\<delimiter>` and `\\`.
///
/// Returns the text and where to carry on, which is past the delimiter when
/// there was one and at the end of the input when the command was cut short.
fn split(input: &[u8], mut at: usize, delimiter: u8) -> (Vec<u8>, usize) {
    let mut text = Vec::new();
    while at < input.len() {
        let byte = input[at];
        if byte == b'\\' {
            match input.get(at + 1) {
                // Only the delimiter and a backslash lose their backslash;
                // `\<` and `\>` have to survive for the pattern to see them.
                Some(&next) if next == delimiter || next == b'\\' => text.push(next),
                Some(&next) => text.extend([byte, next]),
                None => text.push(byte),
            }
            at = (at + 2).min(input.len());
            continue;
        }
        if byte == delimiter {
            return (text, at + 1);
        }
        text.push(byte);
        at += 1;
    }
    (text, at)
}

/// Turns pattern text into a matcher, peeling off `\<` and `\>`.
fn compile(text: &[u8], ignore_case: bool) -> Pattern {
    let mut text = text;
    let mut start_boundary = false;
    let mut end_boundary = false;
    if let Some(rest) = text.strip_prefix(b"\\<") {
        start_boundary = true;
        text = rest;
    }
    if let Some(rest) = text.strip_suffix(b"\\>") {
        end_boundary = true;
        text = rest;
    }
    Pattern {
        text: text.to_vec(),
        start_boundary,
        end_boundary,
        ignore_case,
    }
}

impl Substitute {
    /// The byte range the command covers, or `None` when the range names no
    /// line that exists.
    pub fn resolve(&self, buffer: &Buffer, cursor_row: usize) -> Option<(usize, usize)> {
        let rows = &buffer.rows;
        if rows.is_empty() {
            return None;
        }
        let last = rows.len() - 1;
        let (first, end) = match self.range {
            Range::CurrentLine => (cursor_row.min(last), cursor_row.min(last)),
            Range::WholeFile => (0, last),
            Range::Lines(from, to) => {
                let resolve = |line: usize| match line {
                    CURRENT_LINE => cursor_row.min(last),
                    LAST_LINE => last,
                    // Addresses are one-based, and line 0 means the first.
                    line => line.saturating_sub(1).min(last),
                };
                let (from, to) = (resolve(from), resolve(to));
                (from.min(to), from.max(to))
            }
        };
        Some((rows[first].start, rows[end].end))
    }

    /// Every match in `range`, honoring the `g` flag.
    ///
    /// Without `g` only the first match on each line counts, which is why
    /// this walks rows rather than the range as one run of bytes.
    pub fn matches(&self, buffer: &Buffer, range: (usize, usize)) -> Vec<usize> {
        let (start, end) = range;
        let mut found = Vec::new();
        for row in &buffer.rows {
            if row.end < start || row.start > end {
                continue;
            }
            let from = row.start.max(start);
            let limit = row.end.min(end);
            let mut at = from;
            while let Some(index) = self.pattern.find(&buffer.data, at, limit) {
                found.push(index);
                if !self.flags.global {
                    break;
                }
                // An empty pattern is rejected at parse time, so every match
                // advances and the scan cannot stall.
                at = index + self.pattern.len();
            }
        }
        // Post: matches are in order, never overlap and lie inside `range`,
        // which is what applying them back to front relies on.
        debug_assert!(
            found
                .windows(2)
                .all(|pair| pair[0] + self.pattern.len() <= pair[1])
                && found
                    .iter()
                    .all(|&at| start <= at && at + self.pattern.len() <= end)
        );
        found
    }

    /// Replaces the match at `at`, returning how far the buffer grew or shrank.
    pub fn apply_one(&self, buffer: &mut Buffer, at: usize) -> Option<isize> {
        let end = at.checked_add(self.pattern.len())?;
        buffer.replace_region(at, end, &self.replacement)?;
        let before = self.pattern.len() as isize;
        let after = self.replacement.len() as isize;
        Some(after - before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &[u8]) -> Substitute {
        parse(input)
            .expect("a substitute command")
            .expect("a valid one")
    }

    #[test]
    fn the_documented_forms_all_parse() {
        let all = parsed(b"%s/foo/bar/g");
        assert_eq!(all.range, Range::WholeFile);
        assert_eq!(all.replacement, b"bar".to_vec());
        assert!(all.flags.global && !all.flags.confirm && !all.flags.ignore_case);

        assert!(parsed(b"%s/foo/bar/gc").flags.confirm);
        assert!(parsed(b"%s/foo/bar/gi").flags.ignore_case);
        assert_eq!(parsed(b"5,12s/foo/bar/g").range, Range::Lines(5, 12));
        // No range at all is the current line, and no flags is the first
        // match on it.
        let bare = parsed(b"s/foo/bar/");
        assert_eq!(bare.range, Range::CurrentLine);
        assert!(!bare.flags.global);
        // The trailing delimiter is optional.
        assert_eq!(parsed(b"s/foo/bar").replacement, b"bar".to_vec());
        // `substitute` may be spelled out.
        assert_eq!(parsed(b"%substitute/foo/bar/g").range, Range::WholeFile);
    }

    #[test]
    fn any_punctuation_can_be_the_delimiter() {
        let hash = parsed(b"%s#http://#https://#g");
        assert_eq!(hash.replacement, b"https://".to_vec());
        assert_eq!(hash.pattern.text, b"http://".to_vec());
        // With `/` as the delimiter the slashes have to be escaped instead.
        let slash = parsed(br"%s/http:\/\//https:\/\//g");
        assert_eq!(slash.pattern.text, b"http://".to_vec());
        assert_eq!(slash.replacement, b"https://".to_vec());
    }

    #[test]
    fn word_boundaries_and_case_folding_narrow_the_match() {
        let data = b"foo foobar seafood FOO";
        let plain = parsed(b"%s/foo/x/g");
        assert_eq!(plain.pattern.find(data, 0, data.len()), Some(0));
        assert_eq!(plain.pattern.find(data, 1, data.len()), Some(4));

        let word = parsed(br"%s/\<foo\>/x/g");
        assert_eq!(word.pattern.find(data, 0, data.len()), Some(0));
        // `foobar` and `seafood` are not the word `foo`.
        assert_eq!(word.pattern.find(data, 1, data.len()), None);

        let folded = parsed(b"%s/foo/x/gi");
        assert_eq!(folded.pattern.find(data, 4, data.len()), Some(4));
        // Only the case-folded pattern reaches the trailing `FOO`.
        assert_eq!(folded.pattern.find(data, 15, data.len()), Some(19));
        assert_eq!(plain.pattern.find(data, 15, data.len()), None);
    }

    #[test]
    fn lines_without_the_g_flag_take_only_their_first_match() {
        let buffer = Buffer::new(b"a a a\nb a a".to_vec());
        let once = parsed(b"%s/a/x/");
        assert_eq!(once.matches(&buffer, (0, buffer.data.len())), [0, 8]);
        let every = parsed(b"%s/a/x/g");
        assert_eq!(
            every.matches(&buffer, (0, buffer.data.len())),
            [0, 2, 4, 8, 10]
        );
    }

    #[test]
    fn ranges_resolve_against_the_buffer_and_stay_inside_it() {
        let buffer = Buffer::new(b"one\ntwo\nthree\nfour".to_vec());
        let rows = &buffer.rows;
        assert_eq!(
            parsed(b"%s/x/y/").resolve(&buffer, 0),
            Some((rows[0].start, rows[3].end))
        );
        assert_eq!(
            parsed(b"2,3s/x/y/").resolve(&buffer, 0),
            Some((rows[1].start, rows[2].end))
        );
        assert_eq!(
            parsed(b"s/x/y/").resolve(&buffer, 2),
            Some((rows[2].start, rows[2].end))
        );
        // `.` and `$` are addresses too, and a range past the end clamps.
        assert_eq!(
            parsed(b".,$s/x/y/").resolve(&buffer, 1),
            Some((rows[1].start, rows[3].end))
        );
        assert_eq!(
            parsed(b"9,99s/x/y/").resolve(&buffer, 0),
            Some((rows[3].start, rows[3].end))
        );
        // A backwards range is read as the span it covers.
        assert_eq!(
            parsed(b"3,2s/x/y/").resolve(&buffer, 0),
            Some((rows[1].start, rows[2].end))
        );
    }

    #[test]
    fn lines_that_are_not_substitutions_are_left_to_the_command_language() {
        for line in [
            &b"w"[..],
            b"set-var syntax 1",
            b"q!",
            b"nohl",
            // `s` with no delimiter, and a letter where one belongs.
            b"s",
            b"sort",
        ] {
            assert!(parse(line).is_none(), "{}", String::from_utf8_lossy(line));
        }
        assert_eq!(parse(b"%s//x/"), Some(Err(SubstituteError::MissingPattern)));
        assert_eq!(
            parse(b"%s/a/b/z"),
            Some(Err(SubstituteError::UnknownFlag(b'z')))
        );
    }
}
