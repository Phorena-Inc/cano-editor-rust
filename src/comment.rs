//! Line-comment toggling for a selection.
//!
//! One key both comments and uncomments, the way `gc` does in
//! [commentary.vim](https://github.com/tpope/vim-commentary): a selection that
//! is already fully commented comes back out, and anything else goes in.
//!
//! Everything here is pure and returns the whole buffer, the way
//! [`crate::autoformat::format`] does, so the caller can narrow the result to
//! a single undo step.

use crate::autoformat::leading_end;
use crate::syntax::Language;

/// Whether a toggle put markers in or took them out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Commented,
    Uncommented,
}

impl Direction {
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Commented => "Commented",
            Self::Uncommented => "Uncommented",
        }
    }
}

/// The line-comment marker for a language.
///
/// Line comments only. A block comment would have to stay balanced across a
/// toggle. JSON deliberately returns `None` because comments are not valid
/// JSON and the editor must not silently create an invalid document.
pub const fn token(language: Language) -> Option<&'static [u8]> {
    match language {
        Language::C | Language::Cpp | Language::Rust => Some(b"//"),
        Language::Python | Language::Bash => Some(b"#"),
        Language::Lua => Some(b"--"),
        Language::Vim => Some(b"\""),
        Language::Json => None,
    }
}

/// Toggles line comments across every line `region` touches.
///
/// `None` means there was nothing to do: no line the region reached had
/// anything on it, or the toggle would not have changed a byte.
pub fn toggle(data: &[u8], region: (usize, usize), token: &[u8]) -> Option<(Vec<u8>, Direction)> {
    if token.is_empty() {
        return None;
    }
    let lines: Vec<&[u8]> = data.split(|byte| *byte == b'\n').collect();

    // Blank lines are left out entirely.  A marker on an empty line is noise,
    // and counting them would keep a selection that ends on one from ever
    // being recognised as fully commented.
    let mut selected = Vec::new();
    let mut at = 0;
    for (index, line) in lines.iter().enumerate() {
        let end = at + line.len();
        // A region touching any part of a line takes the whole line: a
        // comment marker belongs to the line, not to a span inside it.
        let touched = end >= region.0 && at <= region.1;
        let indent = leading_end(line);
        if touched && indent < content(line).len() {
            selected.push((index, indent));
        }
        at = end + 1;
    }
    if selected.is_empty() {
        return None;
    }

    let commented = selected
        .iter()
        .all(|(index, indent)| content(lines[*index])[*indent..].starts_with(token));
    // Markers all go in one column so a block of code keeps its shape, and
    // the shallowest line decides which -- anything deeper would bury a
    // marker inside the indentation of the line above it.
    let column = selected
        .iter()
        .map(|(_, indent)| *indent)
        .min()
        .unwrap_or_default();

    let mut out = Vec::with_capacity(data.len() + selected.len() * (token.len() + 1));
    let mut selection = selected.iter().peekable();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push(b'\n');
        }
        let Some((_, indent)) = selection.next_if(|(selected, _)| *selected == index) else {
            out.extend_from_slice(line);
            continue;
        };
        if commented {
            out.extend_from_slice(&line[..*indent]);
            // The single space the other branch adds comes back off with the
            // marker.  A second space was the line's own and stays.
            let rest = &line[indent + token.len()..];
            out.extend_from_slice(rest.strip_prefix(b" ").unwrap_or(rest));
        } else {
            out.extend_from_slice(&line[..column]);
            out.extend_from_slice(token);
            out.push(b' ');
            out.extend_from_slice(&line[column..]);
        }
    }

    let direction = if commented {
        Direction::Uncommented
    } else {
        Direction::Commented
    };
    (out != data).then_some((out, direction))
}

/// A line without the carriage return a CRLF terminator leaves on it.
///
/// The `\r` belongs to the line ending rather than to the text, so a CRLF
/// blank line has to read as blank and a marker has to land in front of the
/// content rather than in front of the return.
fn content(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toggled(source: &[u8], region: (usize, usize), token: &[u8]) -> (String, Direction) {
        let (data, direction) = toggle(source, region, token).expect("toggle should apply");
        (String::from_utf8(data).expect("valid UTF-8"), direction)
    }

    #[test]
    fn a_selection_comments_then_comes_back_out() {
        let source = b"one\ntwo\n";
        let (commented, direction) = toggled(source, (0, source.len()), b"//");
        assert_eq!(commented, "// one\n// two\n");
        assert_eq!(direction, Direction::Commented);

        let (plain, direction) = toggled(commented.as_bytes(), (0, commented.len()), b"//");
        assert_eq!(plain, "one\ntwo\n");
        assert_eq!(direction, Direction::Uncommented);
    }

    #[test]
    fn a_partly_commented_selection_comments_the_rest() {
        // Uncommenting here would need every line to already be commented, so
        // a half-commented block goes further in rather than half out.
        let source = b"// one\ntwo\n";
        let (data, direction) = toggled(source, (0, source.len()), b"//");
        assert_eq!(data, "// // one\n// two\n");
        assert_eq!(direction, Direction::Commented);
    }

    #[test]
    fn markers_line_up_under_the_shallowest_line() {
        let source = b"    if x:\n        y()\n";
        let (data, _) = toggled(source, (0, source.len()), b"#");
        assert_eq!(data, "    # if x:\n    #     y()\n");

        // And come back off leaving the original indentation.
        let (plain, _) = toggled(data.as_bytes(), (0, data.len()), b"#");
        assert_eq!(plain, "    if x:\n        y()\n");
    }

    #[test]
    fn blank_lines_are_left_alone() {
        let source = b"one\n\n   \ntwo\n";
        let (data, _) = toggled(source, (0, source.len()), b"--");
        assert_eq!(data, "-- one\n\n   \n-- two\n");

        // A trailing blank line would otherwise never be "commented", so the
        // block could not be recognised as fully commented on the way back.
        let (plain, direction) = toggled(data.as_bytes(), (0, data.len()), b"--");
        assert_eq!(plain, "one\n\n   \ntwo\n");
        assert_eq!(direction, Direction::Uncommented);
    }

    #[test]
    fn only_the_space_the_toggle_added_comes_back_off() {
        let (kept, _) = toggled(b"//  two spaces", (0, 3), b"//");
        assert_eq!(kept, " two spaces");

        let (none, _) = toggled(b"//no space", (0, 3), b"//");
        assert_eq!(none, "no space");
    }

    #[test]
    fn a_region_touching_part_of_a_line_takes_the_whole_line() {
        // The region covers one byte in the middle of the second line.
        let source = b"one\ntwo\nthree\n";
        let (data, _) = toggled(source, (5, 5), b"#");
        assert_eq!(data, "one\n# two\nthree\n");
    }

    #[test]
    fn crlf_lines_keep_their_terminators() {
        let source = b"one\r\n\r\ntwo\r\n";
        let (data, _) = toggled(source, (0, source.len()), b"//");
        // The blank CRLF row is untouched -- its `\r` is not content.
        assert_eq!(data, "// one\r\n\r\n// two\r\n");

        let (plain, _) = toggled(data.as_bytes(), (0, data.len()), b"//");
        assert_eq!(plain, "one\r\n\r\ntwo\r\n");
    }

    #[test]
    fn nothing_to_do_reports_nothing() {
        // Only blank lines in reach.
        assert_eq!(toggle(b"\n\n\n", (0, 3), b"//"), None);
        // An empty marker cannot be added or found.
        assert_eq!(toggle(b"one\n", (0, 4), b""), None);
    }

    #[test]
    fn languages_report_whether_they_have_line_comments() {
        assert_eq!(token(Language::C), Some(&b"//"[..]));
        assert_eq!(token(Language::Cpp), Some(&b"//"[..]));
        assert_eq!(token(Language::Rust), Some(&b"//"[..]));
        assert_eq!(token(Language::Python), Some(&b"#"[..]));
        assert_eq!(token(Language::Bash), Some(&b"#"[..]));
        assert_eq!(token(Language::Lua), Some(&b"--"[..]));
        assert_eq!(token(Language::Vim), Some(&b"\""[..]));
        assert_eq!(token(Language::Json), None);
    }
}
