//! The fallback steps of
//! [vim-autoformat](https://github.com/vim-autoformat/vim-autoformat).
//!
//! With no external formatter configured, that plugin falls back to three
//! whole-file operations: re-indent, retab, and strip trailing whitespace.
//! Those are what `:autoformat` does here, in the same order and each behind
//! the same switch, because the order matters — re-indenting rewrites the
//! leading whitespace that retab would otherwise have converted first.
//!
//! Everything here is pure, and none of it changes how many lines a buffer
//! has: only whitespace within a line is touched.

use crate::render::TAB_WIDTH;

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
    (result != data).then_some(result)
}

/// Carries a byte region across a rewrite by counting lines rather than
/// bytes, since a rewrite moves bytes but never moves a line.
fn moved(before: &[u8], after: &[u8], region: (usize, usize)) -> (usize, usize) {
    let first = before[..region.0.min(before.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    let last = before[..region.1.min(before.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
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
/// No step adds or removes a line, so the two always pair up.
pub fn changed_lines(before: &[u8], after: &[u8]) -> usize {
    before
        .split(|byte| *byte == b'\n')
        .zip(after.split(|byte| *byte == b'\n'))
        .filter(|(before, after)| before != after)
        .count()
}

/// A running bracket depth.
///
/// This is [`crate::editor::brace_depth`] turned inside out so a whole buffer
/// can be walked once: asking that function for every line in turn would
/// rescan the file from the start each time.  The rules are identical, quotes
/// included, so both agree on any position.
#[derive(Default)]
struct Nesting {
    depth: usize,
    quote: Option<u8>,
    escaped: bool,
}

impl Nesting {
    fn feed(&mut self, byte: u8) {
        if let Some(active) = self.quote {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == active {
                self.quote = None;
            }
            return;
        }
        if matches!(byte, b'\'' | b'"') {
            self.quote = Some(byte);
        } else if matches!(byte, b'(' | b'[' | b'{') {
            self.depth += 1;
        } else if matches!(byte, b')' | b']' | b'}') {
            self.depth = self.depth.saturating_sub(1);
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
        let columns = line[..content].iter().fold(0usize, |columns, byte| {
            columns.saturating_add(if *byte == b'\t' { TAB_WIDTH } else { 1 })
        });
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

/// Where a line's leading whitespace ends.
fn leading_end(line: &[u8]) -> usize {
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
}
