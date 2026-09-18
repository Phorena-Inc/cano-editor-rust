//! Vim's inner and around text objects: the `iw`, `i(`, `i"` and `ip` targets
//! an operator takes in place of a motion.
//!
//! Every range is half-open, the way [`Buffer::delete_selection`] wants it.
//! `None` means the cursor is not inside an object of that kind, which is what
//! makes `di(` outside any parentheses do nothing rather than guess at one.

use crate::buffer::{Buffer, Row, is_keyword, quoted_bytes};

/// Whether the delimiters belong to the range: vim's `i` and `a`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Inner,
    Around,
}

/// The classes `iw` groups a run of bytes by.
///
/// Vim's `iw` is "the run of like bytes under the cursor", where keyword bytes,
/// blanks and punctuation are each like themselves and unlike the other two.
/// That is why `iw` on the `.` of `a.b` takes the dot alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    Keyword,
    Blank,
    Punctuation,
}

fn class(byte: u8) -> Class {
    if is_keyword(byte) {
        Class::Keyword
    } else if matches!(byte, b' ' | b'\t') {
        Class::Blank
    } else {
        Class::Punctuation
    }
}

/// The byte range `object` covers at `cursor`, or `None` when there is none.
///
/// `object` is the key typed after `i` or `a`, so the aliases vim accepts for
/// the bracket pairs (`b` for `(`, `B` for `{`, and either end of a pair
/// standing for the pair) are resolved here rather than by the caller.
pub fn range(buffer: &Buffer, cursor: usize, scope: Scope, object: u8) -> Option<(usize, usize)> {
    match object {
        b'w' => word(buffer, cursor, scope),
        b'p' => paragraph(buffer, cursor, scope),
        b'(' | b')' | b'b' => pair(buffer, cursor, scope, b'(', b')'),
        b'[' | b']' => pair(buffer, cursor, scope, b'[', b']'),
        b'{' | b'}' | b'B' => pair(buffer, cursor, scope, b'{', b'}'),
        b'<' | b'>' => pair(buffer, cursor, scope, b'<', b'>'),
        b'"' | b'\'' | b'`' => quotes(buffer, cursor, scope, object),
        _ => None,
    }
}

/// The row holding `cursor`, and the byte in it the object starts from.
///
/// A row's `end` is its newline, so a cursor resting there belongs to the last
/// byte of the row rather than to the line break.  An empty row has no byte to
/// take, and every object that needs one gives up on it.
fn row_and_anchor(buffer: &Buffer, cursor: usize) -> Option<(Row, usize)> {
    let row = *buffer.rows.get(buffer.row_for_index(cursor)?)?;
    if row.is_empty() {
        return None;
    }
    let anchor = cursor.clamp(row.start, row.end - 1);
    Some((row, anchor))
}

/// Extends `end` over the blanks that follow it on `row`.
fn trailing_blanks(buffer: &Buffer, row: Row, end: usize) -> usize {
    let mut scan = end;
    while scan < row.end && class(buffer.data[scan]) == Class::Blank {
        scan += 1;
    }
    scan
}

fn word(buffer: &Buffer, cursor: usize, scope: Scope) -> Option<(usize, usize)> {
    let (row, anchor) = row_and_anchor(buffer, cursor)?;
    let wanted = class(buffer.data[anchor]);

    let mut start = anchor;
    while start > row.start && class(buffer.data[start - 1]) == wanted {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < row.end && class(buffer.data[end]) == wanted {
        end += 1;
    }

    if scope == Scope::Inner {
        return Some((start, end));
    }

    // `aw` takes the blanks after the word.  When there are none — the word
    // ends the line — it takes the blanks before it instead, which is what
    // keeps `daw` from leaving a double space behind at either end.  A run of
    // blanks is already the object when the cursor sits on one, so it only
    // reaches forward for the word that follows.
    if wanted == Class::Blank {
        let mut reach = end;
        while reach < row.end && class(buffer.data[reach]) != Class::Blank {
            reach += 1;
        }
        return Some((start, reach));
    }
    let trailing = trailing_blanks(buffer, row, end);
    if trailing > end {
        return Some((start, trailing));
    }
    let mut leading = start;
    while leading > row.start && class(buffer.data[leading - 1]) == Class::Blank {
        leading -= 1;
    }
    Some((leading, end))
}

/// The innermost `open`/`close` pair enclosing the cursor.
///
/// The backward scan is what makes this an *enclosing* search rather than
/// [`Buffer::matching_brace_index`]'s "partner of the byte under the cursor":
/// `di(` is nearly always typed from somewhere inside the parentheses.  Both
/// share [`quoted_bytes`], so a brace inside a string literal is invisible to
/// both for the same reason.
fn pair(
    buffer: &Buffer,
    cursor: usize,
    scope: Scope,
    open: u8,
    close: u8,
) -> Option<(usize, usize)> {
    let data = &buffer.data;
    let quoted = quoted_bytes(data);
    let unquoted = |at: usize| !quoted.get(at).copied().unwrap_or(false);

    // A cursor resting on either delimiter names that pair, so `di(` works
    // from the parenthesis itself and not only from between them.
    let open_at = if data.get(cursor) == Some(&open) && unquoted(cursor) {
        cursor
    } else {
        let mut depth = 0usize;
        let mut scan = cursor.min(data.len());
        loop {
            if scan == 0 {
                return None;
            }
            scan -= 1;
            if !unquoted(scan) {
                continue;
            }
            if data[scan] == close {
                depth += 1;
            } else if data[scan] == open {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
        }
        scan
    };

    let mut depth = 0usize;
    let mut close_at = open_at + 1;
    loop {
        if close_at >= data.len() {
            return None;
        }
        if unquoted(close_at) {
            if data[close_at] == open {
                depth += 1;
            } else if data[close_at] == close {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
        }
        close_at += 1;
    }

    match scope {
        Scope::Inner => Some((open_at + 1, close_at)),
        Scope::Around => Some((open_at, close_at + 1)),
    }
}

/// The quoted run the cursor is in, or the first one after it on the row.
///
/// Quotes are paired from the start of the row, the way vim pairs them, so
/// which run the cursor belongs to never depends on the side it arrived from.
/// A cursor ahead of every quote still finds the first run on the row, which
/// is what makes `ci"` work from the start of the line.
fn quotes(buffer: &Buffer, cursor: usize, scope: Scope, quote: u8) -> Option<(usize, usize)> {
    let (row, anchor) = row_and_anchor(buffer, cursor)?;

    let mut open: Option<usize> = None;
    let mut escaped = false;
    let mut first: Option<(usize, usize)> = None;
    let mut scan = row.start;
    while scan < row.end {
        let byte = buffer.data[scan];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            match open {
                None => open = Some(scan),
                Some(start) => {
                    if (start..=scan).contains(&anchor) {
                        return Some(bound_quotes(buffer, row, scope, start, scan));
                    }
                    if start >= anchor {
                        first.get_or_insert((start, scan));
                    }
                    open = None;
                }
            }
        }
        scan += 1;
    }

    let (start, end) = first?;
    Some(bound_quotes(buffer, row, scope, start, end))
}

/// `a"` takes the blanks after the closing quote, but never the ones before
/// the opening one — vim only reaches backwards for `aw`.
fn bound_quotes(
    buffer: &Buffer,
    row: Row,
    scope: Scope,
    open: usize,
    close: usize,
) -> (usize, usize) {
    match scope {
        Scope::Inner => (open + 1, close),
        Scope::Around => (open, trailing_blanks(buffer, row, close + 1)),
    }
}

/// A run of blank or non-blank rows, the way vim splits a file into paragraphs.
///
/// `ap` takes the blank rows that follow the paragraph, or the ones before it
/// when it ends the file, matching `aw`'s rule one row up.
fn paragraph(buffer: &Buffer, cursor: usize, scope: Scope) -> Option<(usize, usize)> {
    let index = buffer.row_for_index(cursor)?;
    // A row of nothing but blanks separates paragraphs as surely as an empty
    // one does, so both count as blank here.
    let blank = |row: usize| {
        let row = buffer.rows[row];
        buffer.data[row.start..row.end]
            .iter()
            .all(|byte| class(*byte) == Class::Blank)
    };
    let wanted = blank(index);

    let mut first = index;
    while first > 0 && blank(first - 1) == wanted {
        first -= 1;
    }
    let mut last = index;
    while last + 1 < buffer.rows.len() && blank(last + 1) == wanted {
        last += 1;
    }

    if scope == Scope::Around {
        let mut reach = last;
        while reach + 1 < buffer.rows.len() && blank(reach + 1) != wanted {
            reach += 1;
        }
        if reach > last {
            last = reach;
        } else {
            while first > 0 && blank(first - 1) != wanted {
                first -= 1;
            }
        }
    }

    let start = buffer.rows[first].start;
    // Take the newline that ends the last row so the paragraph leaves no blank
    // line behind; at the end of the file there is none to take.
    let end = buffer.rows[last].end;
    Some((start, (end + 1).min(buffer.data.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(text: &[u8], cursor: usize, scope: Scope, object: u8) -> Option<Vec<u8>> {
        let buffer = Buffer::new(text.to_vec());
        let (start, end) = range(&buffer, cursor, scope, object)?;
        assert!(start <= end, "inverted range {start}..{end}");
        Some(buffer.data[start..end].to_vec())
    }

    #[test]
    fn inner_word_takes_the_run_of_like_bytes_under_the_cursor() {
        let text = b"the fox_1 runs";
        // Anywhere inside a word gives the whole word, underscores and digits
        // included, because they are keyword bytes.
        for cursor in 4..9 {
            assert_eq!(
                slice(text, cursor, Scope::Inner, b'w').as_deref(),
                Some(&b"fox_1"[..])
            );
        }
        // Punctuation is its own class, so `iw` on a dot takes the dot alone.
        assert_eq!(
            slice(b"a.b", 1, Scope::Inner, b'w').as_deref(),
            Some(&b"."[..])
        );
        // And a run of blanks is an object in its own right.
        assert_eq!(
            slice(text, 3, Scope::Inner, b'w').as_deref(),
            Some(&b" "[..])
        );
    }

    #[test]
    fn around_word_takes_trailing_blanks_or_leading_ones_at_the_line_end() {
        assert_eq!(
            slice(b"one two", 0, Scope::Around, b'w').as_deref(),
            Some(&b"one "[..])
        );
        // The last word of a line has no trailing blank to take, so `aw`
        // reaches back instead and `daw` still leaves no double space.
        assert_eq!(
            slice(b"one two", 4, Scope::Around, b'w').as_deref(),
            Some(&b" two"[..])
        );
        // On a blank, `aw` reaches forward for the word that follows it.
        assert_eq!(
            slice(b"one two", 3, Scope::Around, b'w').as_deref(),
            Some(&b" two"[..])
        );
    }

    #[test]
    fn a_word_object_never_crosses_a_line_break() {
        let text = b"one\ntwo";
        assert_eq!(
            slice(text, 2, Scope::Around, b'w').as_deref(),
            Some(&b"one"[..])
        );
        // A cursor resting on the newline belongs to the row's last byte.
        assert_eq!(
            slice(text, 3, Scope::Inner, b'w').as_deref(),
            Some(&b"one"[..])
        );
        // An empty row has no word to take.
        assert_eq!(slice(b"\nx", 0, Scope::Inner, b'w'), None);
    }

    #[test]
    fn bracket_objects_find_the_innermost_enclosing_pair() {
        let text = b"a(b(c)d)e";
        assert_eq!(
            slice(text, 4, Scope::Inner, b'(').as_deref(),
            Some(&b"c"[..])
        );
        assert_eq!(
            slice(text, 4, Scope::Around, b'(').as_deref(),
            Some(&b"(c)"[..])
        );
        // From between the two pairs the outer one encloses the cursor.
        assert_eq!(
            slice(text, 2, Scope::Inner, b'(').as_deref(),
            Some(&b"b(c)d"[..])
        );
        // `b` and either delimiter are vim's aliases for the same object.
        assert_eq!(
            slice(text, 4, Scope::Inner, b'b'),
            slice(text, 4, Scope::Inner, b'(')
        );
        assert_eq!(
            slice(text, 4, Scope::Inner, b')'),
            slice(text, 4, Scope::Inner, b'(')
        );
    }

    #[test]
    fn a_cursor_on_either_delimiter_names_that_pair() {
        let text = b"f(x)";
        assert_eq!(
            slice(text, 1, Scope::Inner, b'(').as_deref(),
            Some(&b"x"[..])
        );
        assert_eq!(
            slice(text, 3, Scope::Around, b'(').as_deref(),
            Some(&b"(x)"[..])
        );
    }

    #[test]
    fn brackets_spanning_lines_and_the_other_pairs_work_the_same_way() {
        assert_eq!(
            slice(b"fn x() {\n  body\n}\n", 11, Scope::Inner, b'{').as_deref(),
            Some(&b"\n  body\n"[..])
        );
        assert_eq!(
            slice(b"a[b]c", 2, Scope::Around, b'[').as_deref(),
            Some(&b"[b]"[..])
        );
        // Angle brackets are an object even though `%` does not jump them.
        assert_eq!(
            slice(b"Vec<u8>", 5, Scope::Inner, b'<').as_deref(),
            Some(&b"u8"[..])
        );
        assert_eq!(
            slice(b"a{b}c", 2, Scope::Inner, b'B').as_deref(),
            Some(&b"b"[..])
        );
    }

    #[test]
    fn a_brace_inside_a_string_literal_does_not_open_a_pair() {
        // The `(` in the literal is quoted, so the enclosing pair is the real
        // one around it rather than the text after it.
        let text = br#"f("(", x)"#;
        assert_eq!(
            slice(text, 7, Scope::Inner, b'(').as_deref(),
            Some(&br#""(", x"#[..])
        );
    }

    #[test]
    fn an_unmatched_or_absent_pair_is_not_an_object() {
        assert_eq!(slice(b"no parens here", 3, Scope::Inner, b'('), None);
        assert_eq!(slice(b"a(b", 2, Scope::Inner, b'('), None);
        assert_eq!(slice(b"a)b", 0, Scope::Inner, b'('), None);
    }

    #[test]
    fn quote_objects_pair_from_the_start_of_the_row() {
        let text = br#"a "one" b "two""#;
        assert_eq!(
            slice(text, 4, Scope::Inner, b'"').as_deref(),
            Some(&b"one"[..])
        );
        assert_eq!(
            slice(text, 12, Scope::Inner, b'"').as_deref(),
            Some(&b"two"[..])
        );
        // The blank between the runs is outside both, and pairing from the row
        // start is what puts it after the first rather than inside the second.
        assert_eq!(
            slice(text, 8, Scope::Inner, b'"').as_deref(),
            Some(&b"two"[..])
        );
    }

    #[test]
    fn a_cursor_before_every_quote_finds_the_first_run_on_the_row() {
        assert_eq!(
            slice(br#"x = "hi""#, 0, Scope::Inner, b'"').as_deref(),
            Some(&b"hi"[..])
        );
        // But a row with no complete pair has no object at all.
        assert_eq!(slice(br#"x = "hi"#, 0, Scope::Inner, b'"'), None);
    }

    #[test]
    fn around_quotes_take_the_blanks_after_the_closing_one_only() {
        assert_eq!(
            slice(br#"a "b"  c"#, 3, Scope::Around, b'"').as_deref(),
            Some(&br#""b"  "#[..])
        );
        // Nothing follows the closing quote here, and `a"` does not reach back
        // the way `aw` does.
        assert_eq!(
            slice(br#"a "b""#, 3, Scope::Around, b'"').as_deref(),
            Some(&br#""b""#[..])
        );
    }

    #[test]
    fn an_escaped_quote_does_not_close_a_run() {
        let text = br#"s = "a\"b" end"#;
        assert_eq!(
            slice(text, 6, Scope::Inner, b'"').as_deref(),
            Some(&br#"a\"b"#[..])
        );
    }

    #[test]
    fn paragraph_objects_group_rows_by_blankness() {
        let text = b"one\ntwo\n\nthree\n";
        assert_eq!(
            slice(text, 0, Scope::Inner, b'p').as_deref(),
            Some(&b"one\ntwo\n"[..])
        );
        // `ap` takes the blank rows that follow the paragraph.
        assert_eq!(
            slice(text, 0, Scope::Around, b'p').as_deref(),
            Some(&b"one\ntwo\n\n"[..])
        );
        // From inside the blank run, the run itself is the inner object.
        assert_eq!(
            slice(text, 8, Scope::Inner, b'p').as_deref(),
            Some(&b"\n"[..])
        );
    }

    #[test]
    fn an_unknown_object_key_is_not_an_object() {
        assert_eq!(slice(b"anything", 0, Scope::Inner, b'z'), None);
    }
}
