//! How buffer bytes become screen cells.
//!
//! The buffer stays a byte vector so that any file -- valid UTF-8 or not --
//! is saved back exactly as it was read.  Everything that puts those bytes on
//! screen, or maps a screen column back onto them, goes through here instead
//! of assuming one byte is one cell:
//!
//! - A valid UTF-8 sequence is one character.  Bytes that do not form one
//!   are each a *unit* of their own and draw as U+FFFD, one cell apiece, so a
//!   binary or Latin-1 file still lays out predictably.
//! - A *character* in the cursor's sense is a unit plus any zero-width marks
//!   that follow it: `e` and a combining acute are one place to stand, one
//!   `x` to delete and one cell to draw.  A mark with nothing printable to
//!   sit on (a line that starts with one) is a character by itself.
//! - East Asian wide and fullwidth characters and most emoji take two cells;
//!   a tab takes `tabstop`; everything else takes one.
//!
//! The width table is hand-written rather than taken from `unicode-width`,
//! which ratatui uses internally but does not re-export.  It covers the
//! ranges that matter for an editor -- combining marks, CJK, Hangul, kana,
//! fullwidth forms and the emoji blocks -- and a test holds it to ratatui's
//! own idea of the width for a sample of each, because the two have to agree
//! for ratatui to skip the cell a wide character spills into.

/// The tab width before `:set tabstop` changes it.
pub const DEFAULT_TABSTOP: usize = 4;

/// The `tabstop` values `:set` accepts.  Zero would make a tab vanish, and a
/// tab wider than this is a typo rather than a layout.
pub const TABSTOP_RANGE: std::ops::RangeInclusive<usize> = 1..=32;

/// The unit that starts at `at`: the character it decodes to, or `None` for
/// a byte that is not part of a valid sequence, and its length in bytes.
///
/// `at` past the end decodes to nothing, zero bytes long.
pub fn decode(data: &[u8], at: usize) -> (Option<char>, usize) {
    let Some(&lead) = data.get(at) else {
        return (None, 0);
    };
    if lead.is_ascii() {
        return (Some(char::from(lead)), 1);
    }
    let len = match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return (None, 1),
    };
    // `from_utf8` rejects the overlong forms and surrogates the lead byte
    // alone cannot rule out, so a sequence is only a character if the
    // standard library would also call it one.
    match data
        .get(at..at + len)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    {
        Some(text) => (text.chars().next(), len),
        None => (None, 1),
    }
}

/// The start of the unit containing `at`.
///
/// A continuation byte belongs to the valid sequence that covers it, when
/// there is one within the three bytes before it; otherwise it is a stray
/// byte and a unit by itself.  This agrees with decoding forwards from any
/// earlier unit start, because a valid sequence's first byte can never be
/// inside another sequence.
pub fn unit_start(data: &[u8], at: usize) -> usize {
    if data.get(at).is_none_or(|byte| byte & 0xc0 != 0x80) {
        return at;
    }
    (at.saturating_sub(3)..at)
        .rev()
        .find(|&lead| {
            data[lead] & 0xc0 != 0x80 && {
                let (character, len) = decode(data, lead);
                character.is_some() && lead + len > at
            }
        })
        .unwrap_or(at)
}

/// Whether `character` draws nothing of its own and belongs to the character
/// before it: combining marks, joiners and variation selectors.
pub fn is_mark(character: char) -> bool {
    matches!(
        u32::from(character),
        0x0300..=0x036f
            | 0x0483..=0x0489
            | 0x0591..=0x05bd
            | 0x05bf
            | 0x05c1..=0x05c2
            | 0x05c4..=0x05c5
            | 0x05c7
            | 0x0610..=0x061a
            | 0x064b..=0x065f
            | 0x0670
            | 0x06d6..=0x06dc
            | 0x06df..=0x06e4
            | 0x06e7..=0x06e8
            | 0x06ea..=0x06ed
            | 0x0900..=0x0902
            | 0x093a
            | 0x093c
            | 0x0941..=0x0948
            | 0x094d
            | 0x0951..=0x0957
            | 0x0e31
            | 0x0e34..=0x0e3a
            | 0x0e47..=0x0e4e
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x200b..=0x200f
            | 0x20d0..=0x20ff
            | 0x302a..=0x302d
            | 0x3099..=0x309a
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
            | 0xe0100..=0xe01ef
    )
}

/// Whether `character` takes two cells: East Asian wide and fullwidth
/// characters, and the emoji that terminals draw double-width.
fn is_wide(character: char) -> bool {
    matches!(
        u32::from(character),
        0x1100..=0x115f
            | 0x231a..=0x231b
            | 0x2329..=0x232a
            | 0x23e9..=0x23ec
            | 0x23f0
            | 0x23f3
            | 0x25fd..=0x25fe
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267f
            | 0x2693
            | 0x26a1
            | 0x26aa..=0x26ab
            | 0x26bd..=0x26be
            | 0x26c4..=0x26c5
            | 0x26ce
            | 0x26d4
            | 0x26ea
            | 0x26f2..=0x26f3
            | 0x26f5
            | 0x26fa
            | 0x26fd
            | 0x2705
            | 0x270a..=0x270b
            | 0x2728
            | 0x274c
            | 0x274e
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27b0
            | 0x27bf
            | 0x2b1b..=0x2b1c
            | 0x2b50
            | 0x2b55
            | 0x2e80..=0x3029
            | 0x302e..=0x303e
            | 0x3041..=0x3098
            | 0x309b..=0x33ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xa000..=0xa4cf
            | 0xa960..=0xa97f
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x16fe0..=0x16fe4
            | 0x17000..=0x18aff
            | 0x1b000..=0x1b2ff
            | 0x1f004
            | 0x1f0cf
            | 0x1f18e
            | 0x1f191..=0x1f19a
            | 0x1f200..=0x1f202
            | 0x1f210..=0x1f23b
            | 0x1f240..=0x1f248
            | 0x1f250..=0x1f251
            | 0x1f260..=0x1f265
            | 0x1f300..=0x1f320
            | 0x1f32d..=0x1f335
            | 0x1f337..=0x1f37c
            | 0x1f37e..=0x1f393
            | 0x1f3a0..=0x1f3ca
            | 0x1f3cf..=0x1f3d3
            | 0x1f3e0..=0x1f3f0
            | 0x1f3f4
            | 0x1f3f8..=0x1f43e
            | 0x1f440
            | 0x1f442..=0x1f4fc
            | 0x1f4ff..=0x1f53d
            | 0x1f54b..=0x1f54e
            | 0x1f550..=0x1f567
            | 0x1f57a
            | 0x1f595..=0x1f596
            | 0x1f5a4
            | 0x1f5fb..=0x1f64f
            | 0x1f680..=0x1f6c5
            | 0x1f6cc
            | 0x1f6d0..=0x1f6d2
            | 0x1f6d5..=0x1f6d7
            | 0x1f6dc..=0x1f6df
            | 0x1f6eb..=0x1f6ec
            | 0x1f6f4..=0x1f6fc
            | 0x1f7e0..=0x1f7eb
            | 0x1f7f0
            | 0x1f90c..=0x1f93a
            | 0x1f93c..=0x1f945
            | 0x1f947..=0x1f9ff
            | 0x1fa70..=0x1faff
            | 0x20000..=0x2fffd
            | 0x30000..=0x3fffd
    )
}

/// The cells one character takes, a tab aside: zero for a mark, two for a
/// wide character, one for everything else.
///
/// A control character counts one because it is drawn as U+FFFD rather than
/// sent to the terminal.
pub fn char_width(character: char) -> usize {
    if is_mark(character) {
        0
    } else if is_wide(character) {
        2
    } else {
        1
    }
}

/// Whether a mark that follows `character` joins it.  Line breaks, tabs and
/// other control characters are drawn as something else or not at all, so a
/// mark after one starts a character of its own.
fn carries_marks(character: char) -> bool {
    !character.is_control()
}

/// Whether the unit at `at` is a zero-width mark.
fn mark_at(data: &[u8], at: usize) -> bool {
    decode(data, at).0.is_some_and(is_mark)
}

/// The start of the character containing byte `at`, which is where a cursor
/// resting anywhere inside it belongs.  `at` at or past the end is the end.
pub fn char_start(data: &[u8], at: usize) -> usize {
    if at >= data.len() {
        return data.len();
    }
    let mut start = unit_start(data, at);
    // Walk back over the marks to the character they are drawn on, if they
    // have one; a run of marks after a line break is its own character.
    while start > 0 && mark_at(data, start) {
        let previous = unit_start(data, start - 1);
        match decode(data, previous).0 {
            Some(character) if is_mark(character) || carries_marks(character) => {
                start = previous;
            }
            _ => break,
        }
    }
    // Post: the result starts a unit, so it is never a continuation byte of
    // a valid sequence.
    debug_assert_eq!(unit_start(data, start), start);
    start
}

/// The end of the character that starts at `start`: one past its last byte,
/// marks included.  At the end of the data it is the end.
pub fn char_end(data: &[u8], start: usize) -> usize {
    let (character, len) = decode(data, start);
    let mut end = start + len;
    if character.is_some_and(|c| is_mark(c) || carries_marks(c)) {
        while mark_at(data, end) {
            end += decode(data, end).1;
        }
    }
    end
}

/// Whether a cursor may rest on byte `at`: the start of a character, or the
/// end of the data.
pub fn is_char_boundary(data: &[u8], at: usize) -> bool {
    at <= data.len() && char_start(data, at) == at
}

/// The start of the character before the one containing `at`, or zero.
pub fn previous_char(data: &[u8], at: usize) -> usize {
    let start = char_start(data, at);
    if start == 0 {
        0
    } else {
        char_start(data, start - 1)
    }
}

/// The start of the character after the one containing `at`, clamped to the
/// end of the data.
pub fn next_char(data: &[u8], at: usize) -> usize {
    char_end(data, char_start(data, at)).min(data.len())
}

/// One character as laid out on a row: its bytes and the display columns it
/// occupies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell {
    pub start: usize,
    pub end: usize,
    /// The first display column, counted from the start of the row.
    pub column: usize,
    /// How many columns it takes: `tabstop` for a tab, otherwise one or two.
    pub width: usize,
}

/// The characters of `data[start..end]` with their display columns.
///
/// `start` has to be a character start -- a row's first byte always is --
/// and a character that runs past `end` is cut off there.  A tab takes
/// `tabstop` columns wherever it sits: Cano's tabs are a fixed width rather
/// than stops, so a tab is the same size in the middle of a line as at its
/// start, which is what retab and the existing layout rely on.
pub fn cells(data: &[u8], start: usize, end: usize, tabstop: usize) -> impl Iterator<Item = Cell> {
    debug_assert!(start >= end || is_char_boundary(data, start));
    let mut at = start;
    let mut column = 0usize;
    std::iter::from_fn(move || {
        if at >= end {
            return None;
        }
        let next = char_end(data, at).min(end);
        let width = match decode(data, at).0 {
            Some('\t') => tabstop,
            // A mark with nothing to sit on is drawn on a space, so it
            // still has a cell to occupy.
            Some(character) => char_width(character).max(1),
            None => 1,
        };
        let cell = Cell {
            start: at,
            end: next,
            column,
            width,
        };
        at = next;
        column = column.saturating_add(width);
        Some(cell)
    })
}

/// The columns `bytes` take on screen, counting each tab as `tabstop`.
pub fn display_columns(bytes: &[u8], tabstop: usize) -> usize {
    cells(bytes, 0, bytes.len(), tabstop)
        .last()
        .map_or(0, |cell| cell.column.saturating_add(cell.width))
}

/// The display column of the character containing `at`, within the row
/// `start..end`.  The row's end -- its newline, or the end of the file -- is
/// the column just past its last character.
pub fn column_of(data: &[u8], start: usize, end: usize, at: usize, tabstop: usize) -> usize {
    let at = at.clamp(start, end);
    let mut past = 0usize;
    for cell in cells(data, start, end, tabstop) {
        if at < cell.end {
            return cell.column;
        }
        past = cell.column.saturating_add(cell.width);
    }
    past
}

/// The start of the character drawn at display `column` in the row
/// `start..end`, or the row's end when the column is past its last one.
///
/// A column in the middle of a wide character or a tab belongs to that
/// character, so the answer is always a place the cursor can rest.
pub fn byte_at_column(
    data: &[u8],
    start: usize,
    end: usize,
    column: usize,
    tabstop: usize,
) -> usize {
    cells(data, start, end, tabstop)
        .find(|cell| column < cell.column.saturating_add(cell.width))
        .map_or(end, |cell| cell.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_takes_valid_sequences_whole_and_invalid_bytes_one_at_a_time() {
        let text = "aé日😀".as_bytes();
        assert_eq!(decode(text, 0), (Some('a'), 1));
        assert_eq!(decode(text, 1), (Some('é'), 2));
        assert_eq!(decode(text, 3), (Some('日'), 3));
        assert_eq!(decode(text, 6), (Some('😀'), 4));
        assert_eq!(decode(text, 10), (None, 0));
        // A truncated sequence, an overlong slash, a surrogate and a stray
        // continuation are all invalid, one byte each.
        for bytes in [&b"\xe6\x97"[..], b"\xc0\xaf", b"\xed\xa0\x80", b"\x80"] {
            assert_eq!(decode(bytes, 0), (None, 1), "{bytes:?}");
        }
    }

    #[test]
    fn widths_cover_marks_wide_characters_and_emoji() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('é'), 1);
        assert_eq!(char_width('\u{301}'), 0);
        assert_eq!(char_width('日'), 2);
        assert_eq!(char_width('한'), 2);
        assert_eq!(char_width('Ａ'), 2);
        assert_eq!(char_width('😀'), 2);
    }

    #[test]
    fn the_width_table_agrees_with_what_ratatui_measures() {
        use ratatui::text::Span;
        // Ratatui decides which cell a wide character spills into from its
        // own width table, so a disagreement would draw over the second half
        // of a character or leave a gap after it.
        for character in [
            'a', 'é', 'Ω', 'Я', '→', '\u{a0}', '日', '本', 'か', 'カ', '한', 'Ａ', '￥', '〇',
            '😀', '🚀', '🦀', '⌚', '⭐', '\u{301}', '\u{20d7}', '\u{fe0f}', '\u{200b}',
        ] {
            assert_eq!(
                char_width(character),
                Span::raw(character.to_string()).width(),
                "{character:?} U+{:04X}",
                u32::from(character)
            );
        }
    }

    #[test]
    fn a_character_is_a_unit_and_the_marks_that_follow_it() {
        let text = "e\u{301}x\n\u{301}y".as_bytes();
        // `e` and its acute are one character, three bytes long.
        assert_eq!(char_end(text, 0), 3);
        assert_eq!(char_start(text, 1), 0);
        assert_eq!(char_start(text, 2), 0);
        assert!(!is_char_boundary(text, 1));
        assert!(is_char_boundary(text, 3));
        // A mark after a line break has nothing to sit on and stands alone.
        assert_eq!(char_start(text, 5), 5);
        assert_eq!(char_end(text, 5), 7);
        assert_eq!(char_end(text, 4), 5);
        assert_eq!(previous_char(text, 3), 0);
        assert_eq!(next_char(text, 1), 3);
        assert!(is_char_boundary(text, text.len()));
    }

    #[test]
    fn continuation_bytes_belong_to_their_sequence_unless_it_is_broken() {
        let text = "日".as_bytes();
        assert_eq!(char_start(text, 2), 0);
        // The lead byte is gone, so the two continuations are strays, each a
        // character by itself.
        let broken = &text[1..];
        assert_eq!(char_start(broken, 1), 1);
        assert_eq!(char_end(broken, 0), 1);
    }

    #[test]
    fn cells_lay_a_row_out_in_display_columns() {
        let text = "a\t日é\u{301}\u{ff}".as_bytes();
        let laid: Vec<_> = cells(text, 0, text.len(), 3)
            .map(|cell| (cell.start, cell.end, cell.column, cell.width))
            .collect();
        assert_eq!(
            laid,
            vec![
                (0, 1, 0, 1),
                (1, 2, 1, 3),
                (2, 5, 4, 2),
                (5, 9, 6, 1),
                (9, 11, 7, 1),
            ]
        );
        assert_eq!(display_columns(text, 3), 8);
        assert_eq!(display_columns(b"\t\t", 8), 16);
        // Every column inside a wide character or a tab maps back to it.
        assert_eq!(byte_at_column(text, 0, text.len(), 3, 3), 1);
        assert_eq!(byte_at_column(text, 0, text.len(), 5, 3), 2);
        assert_eq!(byte_at_column(text, 0, text.len(), 99, 3), text.len());
        assert_eq!(column_of(text, 0, text.len(), 3, 3), 4);
        assert_eq!(column_of(text, 0, text.len(), text.len(), 3), 8);
    }
}
