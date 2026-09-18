//! The fallback steps of
//! [vim-autoformat](https://github.com/vim-autoformat/vim-autoformat).
//!
//! With no external formatter configured, that plugin falls back to three
//! whole-file operations: re-indent, retab, and strip trailing whitespace.
//! Those are what `:autoformat` does here, in the same order and each behind
//! the same switch, because the order matters — re-indenting rewrites the
//! leading whitespace that retab would otherwise have converted first.
//!
//! Everything here is pure. The fallback only changes whitespace within a
//! line; JSON pretty-printing may also add or remove lines.

use crate::buffer::Nesting;
use crate::render::{TAB_WIDTH, display_columns};
use serde_json::value::RawValue;

/// Which steps `:autoformat` runs, each on by default as in the plugin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Steps {
    /// Re-indent every line from its bracket nesting, as `gg=G` does.
    pub autoindent: bool,
    /// Rewrite leading whitespace as tabs or spaces to match `indent`.
    pub retab: bool,
    /// Drop whitespace at the end of every line.
    pub remove_trailing_spaces: bool,
}

/// Formats the lines `region` touches, or returns `None` when they are
/// already formatted.
///
/// `region` is a byte range; every line it reaches into is rewritten whole.
/// Bracket depth is still measured from the start of the buffer, because how
/// deep the first formatted line sits depends on everything above it.
///
/// `indent` is the editor's indent width, where zero means a tab.
pub fn format(data: &[u8], region: (usize, usize), steps: Steps, indent: usize) -> Option<Vec<u8>> {
    let mut result = data.to_vec();
    if steps.autoindent {
        result = autoindent(&result, region, indent);
    }
    // Each step reads the region against the buffer it is handed, and the
    // earlier steps can only have changed the width of leading whitespace,
    // never which lines the region reaches.
    if steps.retab {
        let region = moved(data, &result, region);
        result = retab(&result, region, indent);
    }
    if steps.remove_trailing_spaces {
        let region = moved(data, &result, region);
        result = remove_trailing_spaces(&result, region);
    }
    // Post: every step rewrites within lines and never adds or drops one,
    // which is what lets `moved` carry the region between them.
    debug_assert_eq!(line_count(&result), line_count(data));
    (result != data).then_some(result)
}

/// Validates and pretty-prints a complete JSON document.
///
/// The formatting pass works on the original lexemes after `serde_json`
/// validates them. That preserves object order, duplicate keys, number
/// spellings and string escapes rather than round-tripping through a value
/// representation that could rewrite them.
pub fn format_json(data: &[u8], indent: usize) -> Result<Option<Vec<u8>>, serde_json::Error> {
    let _: Box<RawValue> = serde_json::from_slice(data)?;

    let trailing_newline = data
        .iter()
        .rev()
        .take_while(|byte| byte.is_ascii_whitespace())
        .any(|byte| *byte == b'\n');
    let mut out = Vec::with_capacity(data.len());
    let mut containers = Vec::new();
    let mut at = 0;
    while at < data.len() {
        match data[at] {
            byte if byte.is_ascii_whitespace() => at += 1,
            b'"' => {
                let start = at;
                at += 1;
                let mut escaped = false;
                while at < data.len() {
                    let byte = data[at];
                    at += 1;
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        break;
                    }
                }
                out.extend_from_slice(&data[start..at]);
            }
            opening @ (b'{' | b'[') => {
                out.push(opening);
                let closing = if opening == b'{' { b'}' } else { b']' };
                let empty = data[at + 1..]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|byte| *byte == closing);
                containers.push(empty);
                at += 1;
                if !empty {
                    push_json_line(&mut out, containers.len(), indent);
                }
            }
            closing @ (b'}' | b']') => {
                let empty = containers.pop().unwrap_or(true);
                if !empty {
                    push_json_line(&mut out, containers.len(), indent);
                }
                out.push(closing);
                at += 1;
            }
            b',' => {
                out.push(b',');
                push_json_line(&mut out, containers.len(), indent);
                at += 1;
            }
            b':' => {
                out.extend_from_slice(b": ");
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    if trailing_newline {
        out.push(b'\n');
    }
    // Post: only whitespace between lexemes changed, so the document still
    // parses.
    debug_assert!(serde_json::from_slice::<Box<RawValue>>(&out).is_ok());
    Ok((out != data).then_some(out))
}

fn push_json_line(out: &mut Vec<u8>, depth: usize, indent: usize) {
    out.push(b'\n');
    push_indent(out, depth, indent);
}

/// Carries a byte region across a rewrite by counting lines rather than
/// bytes, since a rewrite moves bytes but never moves a line.
fn moved(before: &[u8], after: &[u8], region: (usize, usize)) -> (usize, usize) {
    let first = line_count(&before[..region.0.min(before.len())]);
    let last = line_count(&before[..region.1.min(before.len())]);
    let mut bounds = (after.len(), after.len());
    let mut at = 0;
    for (index, line) in after.split(|byte| *byte == b'\n').enumerate() {
        if index == first {
            bounds.0 = at;
        }
        if index == last {
            bounds.1 = at + line.len();
        }
        at += line.len() + 1;
    }
    bounds
}

/// How many lines differ between two versions of a buffer.
///
pub fn changed_lines(before: &[u8], after: &[u8]) -> usize {
    let mut before = before.split(|byte| *byte == b'\n');
    let mut after = after.split(|byte| *byte == b'\n');
    let mut changed = 0;
    loop {
        match (before.next(), after.next()) {
            (None, None) => return changed,
            (Some(before), Some(after)) if before == after => {}
            _ => changed += 1,
        }
    }
}

/// Rewrites every line's indentation from its bracket nesting.
fn autoindent(data: &[u8], region: (usize, usize), indent: usize) -> Vec<u8> {
    let mut nesting = Nesting::default();
    rewrite(data, region, |line, inside, out| {
        let content = leading_end(line);
        // A line that closes what an earlier one opened belongs a level out,
        // so the bracket lines up with what it closes.
        let closes = line
            .get(content)
            .is_some_and(|byte| matches!(byte, b')' | b']' | b'}'));
        let depth = if closes {
            nesting.depth.saturating_sub(1)
        } else {
            nesting.depth
        };
        if !inside {
            out.extend_from_slice(line);
        } else if content < line.len() {
            // A blank line is left truly blank rather than filled with the
            // indentation of a block it holds nothing in.
            push_indent(out, depth, indent);
            out.extend_from_slice(&line[content..]);
        }
        // The depth follows the original line, not the rewritten one, and it
        // follows every line, not just the ones being rewritten.
        for byte in line {
            nesting.feed(*byte);
        }
    })
}

/// Rewrites leading whitespace as tabs or spaces without moving the text.
///
/// The column count is preserved rather than snapped to a multiple of the
/// indent width, the way vim's own `:retab` preserves it.
fn retab(data: &[u8], region: (usize, usize), indent: usize) -> Vec<u8> {
    rewrite(data, region, |line, inside, out| {
        if !inside {
            out.extend_from_slice(line);
            return;
        }
        let content = leading_end(line);
        let columns = display_columns(&line[..content]);
        if indent == 0 {
            out.extend(std::iter::repeat_n(b'\t', columns / TAB_WIDTH));
            out.extend(std::iter::repeat_n(b' ', columns % TAB_WIDTH));
        } else {
            out.extend(std::iter::repeat_n(b' ', columns));
        }
        out.extend_from_slice(&line[content..]);
    })
}

fn remove_trailing_spaces(data: &[u8], region: (usize, usize)) -> Vec<u8> {
    rewrite(data, region, |line, inside, out| {
        if !inside {
            out.extend_from_slice(line);
            return;
        }
        let mut end = line.len();
        while end > 0 && matches!(line[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        out.extend_from_slice(&line[..end]);
    })
}

/// Offers every line to `step`, saying whether it lies in the region, and
/// puts the newlines back exactly as they were: a file that did not end with
/// one still does not.
///
/// Lines outside the region are still offered, because a step may need to
/// read them even when it must not rewrite them.
fn rewrite(
    data: &[u8],
    region: (usize, usize),
    mut step: impl FnMut(&[u8], bool, &mut Vec<u8>),
) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0;
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            out.push(b'\n');
        }
        let end = at + line.len();
        // A region touching any part of a line takes the whole line, because
        // indentation belongs to the line rather than to a span inside it.
        step(line, end >= region.0 && at <= region.1, &mut out);
        at = end + 1;
    }
    out
}

fn line_count(data: &[u8]) -> usize {
    data.iter().filter(|byte| **byte == b'\n').count()
}

/// Where a line's leading whitespace ends.
///
/// Spaces and tabs only: CR and LF are terminators, and treating them as
/// indentation would walk past the end of the line.
pub(crate) fn leading_end(line: &[u8]) -> usize {
    line.iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(line.len())
}

fn push_indent(out: &mut Vec<u8>, depth: usize, indent: usize) {
    if indent == 0 {
        out.extend(std::iter::repeat_n(b'\t', depth));
    } else {
        out.extend(std::iter::repeat_n(b' ', indent.saturating_mul(depth)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Steps = Steps {
        autoindent: true,
        retab: true,
        remove_trailing_spaces: true,
    };

    /// Formats the whole buffer, which is what most of these cases want.
    fn whole(data: &[u8], steps: Steps, indent: usize) -> Option<Vec<u8>> {
        format(data, (0, data.len()), steps, indent)
    }

    fn only(autoindent: bool, retab: bool, remove_trailing_spaces: bool) -> Steps {
        Steps {
            autoindent,
            retab,
            remove_trailing_spaces,
        }
    }

    #[test]
    fn indentation_follows_the_nesting_and_closers_sit_a_level_out() {
        let messy = b"fn f() {\nlet x = 1;\n  if x {\nbody;\n}\n}\n";
        let tidy = whole(messy, only(true, false, false), 4).unwrap();
        assert_eq!(
            tidy,
            b"fn f() {\n    let x = 1;\n    if x {\n        body;\n    }\n}\n"
        );
        // Zero means a tab, the way the editor's own indentation reads it.
        let tabbed = whole(messy, only(true, false, false), 0).unwrap();
        assert_eq!(
            tabbed,
            b"fn f() {\n\tlet x = 1;\n\tif x {\n\t\tbody;\n\t}\n}\n"
        );
    }

    #[test]
    fn brackets_inside_strings_do_not_move_anything() {
        // The `{` here is text, so the line after it is not a block.
        let source = b"a = \"{\";\nb = 1;\n";
        assert_eq!(whole(source, only(true, false, false), 4), None);
        // An escaped quote leaves the string open across the brace.
        let escaped = b"a = \"\\\"{\";\nb = 1;\n";
        assert_eq!(whole(escaped, only(true, false, false), 4), None);
    }

    #[test]
    fn a_blank_line_is_left_blank_rather_than_filled_with_indentation() {
        let source = b"f() {\n\nbody;\n}\n";
        let tidy = whole(source, only(true, false, false), 4).unwrap();
        assert_eq!(tidy, b"f() {\n\n    body;\n}\n");
        // Whitespace-only lines inside a block collapse too.
        let padded = b"f() {\n    \nbody;\n}\n";
        let tidy = whole(padded, only(true, false, false), 4).unwrap();
        assert_eq!(tidy, b"f() {\n\n    body;\n}\n");
    }

    #[test]
    fn retab_preserves_the_column_it_found() {
        // A tab is four columns, so it becomes four spaces and back again.
        let tabbed = b"\tone\n\t\ttwo\n  three\n";
        assert_eq!(
            whole(tabbed, only(false, true, false), 4).unwrap(),
            b"    one\n        two\n  three\n"
        );
        let spaced = b"    one\n      two\n";
        assert_eq!(
            whole(spaced, only(false, true, false), 0).unwrap(),
            b"\tone\n\t  two\n"
        );
        // Only leading whitespace moves; a tab inside a line stays put.
        assert_eq!(whole(b"a\tb\n", only(false, true, false), 4), None);
    }

    #[test]
    fn trailing_whitespace_goes_from_every_line() {
        assert_eq!(
            whole(b"a  \nb\t\n\t\nc", only(false, false, true), 4).unwrap(),
            b"a\nb\n\nc"
        );
    }

    #[test]
    fn formatting_never_adds_or_removes_a_line() {
        for source in [
            &b""[..],
            b"\n",
            b"\n\n\n",
            b"no trailing newline",
            b"  \t  ",
            b"}\n}\n}\n",
            b"f({[\n",
        ] {
            let lines = |bytes: &[u8]| bytes.split(|byte| *byte == b'\n').count();
            let formatted = whole(source, ALL, 4).unwrap_or_else(|| source.to_vec());
            assert_eq!(
                lines(&formatted),
                lines(source),
                "{}",
                String::from_utf8_lossy(source)
            );
        }
    }

    #[test]
    fn an_already_formatted_buffer_reports_no_change() {
        let tidy = b"fn f() {\n    body;\n}\n";
        assert_eq!(whole(tidy, ALL, 4), None);
        // And formatting is idempotent: a second pass changes nothing.
        let messy = b"fn f() {\nbody;   \n}\n";
        let once = whole(messy, ALL, 4).unwrap();
        assert_eq!(whole(&once, ALL, 4), None);
        assert_eq!(changed_lines(messy, &once), 1);
        assert_eq!(changed_lines(&once, &once), 0);
    }

    #[test]
    fn a_region_takes_whole_lines_and_leaves_the_rest_alone() {
        //            0        9              24            38
        let source = b"f() {\nbad;\n  worse;\nalso bad;\n}\n";
        // A region covering only the middle line rewrites that line whole and
        // nothing else, even where the rest is just as badly indented.
        let middle = format(source, (11, 19), ALL, 4).unwrap();
        assert_eq!(middle, b"f() {\nbad;\n    worse;\nalso bad;\n}\n");

        // Depth still comes from the whole buffer: the region does not start
        // at column zero just because the formatting does.
        let deeper = b"a {\nb {\ninner;\n}\n}\n";
        let inner = format(deeper, (8, 14), ALL, 4).unwrap();
        assert_eq!(inner, b"a {\nb {\n        inner;\n}\n}\n");

        // An empty region at a line still claims that line, since a byte
        // range of zero width sits inside one.
        assert_eq!(
            format(source, (6, 6), ALL, 4).unwrap(),
            b"f() {\n    bad;\n  worse;\nalso bad;\n}\n"
        );
        // A region outside everything changes nothing.
        assert_eq!(format(source, (1000, 1000), ALL, 4), None);
    }

    #[test]
    fn steps_after_the_first_still_see_the_region_they_were_given() {
        // Re-indenting moves bytes, so a later step working from stale byte
        // offsets would strip or retab the wrong lines.
        let source = b"f() {\nkeep;   \ntrim;   \n}\n";
        let second = format(source, (6, 14), ALL, 4).unwrap();
        assert_eq!(second, b"f() {\n    keep;\ntrim;   \n}\n");
    }

    #[test]
    fn the_running_depth_agrees_with_the_editors_own() {
        // Walking the buffer once has to give the same answer as asking the
        // editor for the depth at each position, or `:autoformat` and a typed
        // newline would indent differently.
        let source = b"a{\n  \"}{\" b(\n'\\''[\n]\n)\n}\n";
        let mut nesting = Nesting::default();
        for at in 0..source.len() {
            assert_eq!(
                nesting.depth,
                crate::editor::brace_depth(source, at),
                "at byte {at}"
            );
            nesting.feed(source[at]);
        }
    }

    #[test]
    fn json_format_validates_and_pretty_prints_without_rewriting_values() {
        let compact = br#"{"z":[1,-2.50e+3,{"escaped":"a\\nb"}],"z":null}"#;
        assert_eq!(
            format_json(compact, 2).unwrap().unwrap(),
            b"{\n  \"z\": [\n    1,\n    -2.50e+3,\n    {\n      \"escaped\": \"a\\\\nb\"\n    }\n  ],\n  \"z\": null\n}"
        );
        assert!(format_json(b"{\n  \"ok\": true\n}", 2).unwrap().is_none());
    }

    #[test]
    fn json_format_preserves_a_final_newline_and_supports_tab_indentation() {
        assert_eq!(
            format_json(b"{\"a\":[1]}\n", 0).unwrap().unwrap(),
            b"{\n\t\"a\": [\n\t\t1\n\t]\n}\n"
        );
    }

    #[test]
    fn invalid_json_is_rejected_without_a_partial_result() {
        let error = format_json(b"{\"missing\":}", 2).unwrap_err();
        assert_eq!(error.line(), 1);
        assert!(error.column() > 0);
    }

    #[test]
    fn changed_lines_counts_lines_added_by_json_formatting() {
        assert_eq!(changed_lines(b"[1,2]", b"[\n  1,\n  2\n]"), 4);
    }
}
