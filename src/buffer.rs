//! Byte-oriented text storage and legacy cursor semantics.
//!
//! Cano indexes arbitrary file contents by byte.  A row's `end` is the byte
//! index of its newline, or `data.len()` for the final row.  Consequently the
//! newline belongs to the preceding row for cursor movement, while
//! `start..end` is the visible row body.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

impl Row {
    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionDeletion {
    /// The legacy clipboard uses `start..=end` (with a synthetic NUL at EOF).
    pub clipboard: Vec<u8>,
    /// Undo data uses the deletion's half-open `start..end` range.
    pub undo: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvariantError {
    CursorOutOfBounds {
        cursor: usize,
        len: usize,
    },
    RowsOutOfDate {
        expected: Vec<Row>,
        actual: Vec<Row>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Buffer {
    pub data: Vec<u8>,
    pub rows: Vec<Row>,
    pub cursor: usize,
}

/// A search pattern that `hlsearch` is currently showing.
///
/// An empty needle means nothing is highlighted, which is the state `:nohl`
/// restores.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Highlight {
    pub needle: Vec<u8>,
    /// `*` matches whole words the way vim's `\<word\>` does; `/` matches any
    /// substring.  Without the distinction, `*` on `the` would light up every
    /// `then` and `other` on screen.
    pub whole_word: bool,
}

impl Highlight {
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Reports whether the occurrence starting at `at` counts as a match.
    fn accepts(&self, data: &[u8], at: usize) -> bool {
        if !self.whole_word {
            return true;
        }
        let before = at.checked_sub(1).map(|index| data[index]);
        let after = data.get(at + self.needle.len()).copied();
        !before.is_some_and(Buffer::is_word) && !after.is_some_and(Buffer::is_word)
    }

    /// The start of the first occurrence at or after `from`.
    pub fn find(&self, data: &[u8], from: usize) -> Option<usize> {
        if self.needle.is_empty() || self.needle.len() > data.len() {
            return None;
        }
        (from.min(data.len())..=data.len() - self.needle.len())
            .find(|at| data[*at..].starts_with(&self.needle) && self.accepts(data, *at))
    }

    /// The start of the last occurrence that begins before `before`.
    pub fn rfind(&self, data: &[u8], before: usize) -> Option<usize> {
        if self.needle.is_empty() || self.needle.len() > data.len() {
            return None;
        }
        let limit = before.min(data.len() - self.needle.len() + 1);
        (0..limit)
            .rev()
            .find(|at| data[*at..].starts_with(&self.needle) && self.accepts(data, *at))
    }

    /// Every occurrence in `data`, as half-open byte ranges.
    pub fn matches(&self, data: &[u8]) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(start) = self.find(data, at) {
            let end = start + self.needle.len();
            found.push((start, end));
            // Overlapping matches would paint the same cells twice; step past
            // the one just taken.
            at = end.max(start + 1);
        }
        found
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Buffer {
    pub fn new(data: Vec<u8>) -> Self {
        let mut buffer = Self {
            data,
            rows: Vec::new(),
            cursor: 0,
        };
        buffer.calculate_rows();
        buffer
    }

    fn derived_rows(data: &[u8]) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut start = 0;
        for (index, byte) in data.iter().enumerate() {
            if *byte == b'\n' {
                rows.push(Row { start, end: index });
                start = index + 1;
            }
        }
        rows.push(Row {
            start,
            end: data.len(),
        });
        rows
    }

    /// Rebuilds row metadata.  Every buffer has at least one row.
    pub fn calculate_rows(&mut self) {
        self.rows = Self::derived_rows(&self.data);
    }

    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.cursor > self.data.len() {
            return Err(InvariantError::CursorOutOfBounds {
                cursor: self.cursor,
                len: self.data.len(),
            });
        }
        let expected = Self::derived_rows(&self.data);
        if self.rows != expected {
            return Err(InvariantError::RowsOutOfDate {
                expected,
                actual: self.rows.clone(),
            });
        }
        Ok(())
    }

    pub fn invariants_hold(&self) -> bool {
        self.validate().is_ok()
    }

    /// Returns the row containing an index.  A newline index belongs to the
    /// row it terminates; logical EOF belongs to the final row.
    pub fn row_for_index(&self, index: usize) -> Option<usize> {
        if index > self.data.len() {
            return None;
        }
        self.rows.iter().position(|row| index <= row.end)
    }

    /// The byte range of the word `*` would search for from `index`.
    ///
    /// Vim looks under the cursor first and then forward on the same line, so
    /// a cursor resting on punctuation still picks up the next word.
    pub fn word_at(&self, index: usize) -> Option<(usize, usize)> {
        let row = *self.rows.get(self.row_for_index(index)?)?;
        let mut start = index.clamp(row.start, row.end);
        while start < row.end && !Self::is_word(self.data[start]) {
            start += 1;
        }
        if start >= row.end {
            return None;
        }
        // The cursor may have landed inside a word rather than on its first
        // byte, so walk back to where the word actually starts.
        while start > row.start && Self::is_word(self.data[start - 1]) {
            start -= 1;
        }
        let mut end = start;
        while end < row.end && Self::is_word(self.data[end]) {
            end += 1;
        }
        Some((start, end))
    }

    pub fn cursor_row(&self) -> Option<usize> {
        self.row_for_index(self.cursor)
    }

    pub fn cursor_column(&self) -> Option<usize> {
        let row = self.cursor_row()?;
        Some(self.cursor.saturating_sub(self.rows[row].start))
    }

    /// Inserts immediately before the cursor.  An invalid cursor is clamped
    /// to EOF, and the returned value is the advanced cursor/undo endpoint.
    pub fn insert_byte(&mut self, byte: u8) -> usize {
        self.cursor = self.cursor.min(self.data.len());
        self.data.insert(self.cursor, byte);
        self.cursor += 1;
        self.calculate_rows();
        self.cursor
    }

    /// Deletes the byte under the cursor without moving the cursor.
    pub fn delete_byte(&mut self) -> bool {
        if self.cursor >= self.data.len() {
            return false;
        }
        self.data.remove(self.cursor);
        self.calculate_rows();
        true
    }

    /// Copies a visual selection using Cano's inclusive clipboard endpoint.
    /// At logical EOF the old spare byte was zero-filled, so a synthetic NUL
    /// is retained for compatibility.
    pub fn copy_selection(&self, start: usize, end: usize) -> Option<Vec<u8>> {
        if start > end || end > self.data.len() {
            return None;
        }
        let mut bytes = self.data.get(start..end)?.to_vec();
        bytes.push(self.data.get(end).copied().unwrap_or(0));
        Some(bytes)
    }

    /// Deletes `start..end`, but reports clipboard bytes from `start..=end`.
    /// Reversed and out-of-bounds ranges are rejected instead of invoking the
    /// legacy implementation's undefined memory behavior.
    pub fn delete_selection(&mut self, start: usize, end: usize) -> Option<SelectionDeletion> {
        let clipboard = self.copy_selection(start, end)?;
        let undo = self.data.get(start..end)?.to_vec();
        self.cursor = start;
        self.data.drain(start..end);
        self.calculate_rows();
        Some(SelectionDeletion { clipboard, undo })
    }

    /// Replaces `start..end` with `bytes`, returning what was taken out.
    ///
    /// A substitution changes a whole region in one step, and has to be able
    /// to come back in one step; composing it from a delete and an insert
    /// would put two entries on the undo stack for one command.
    pub fn replace_region(&mut self, start: usize, end: usize, bytes: &[u8]) -> Option<Vec<u8>> {
        if start > end || end > self.data.len() {
            return None;
        }
        let removed = self.data[start..end].to_vec();
        self.data.splice(start..end, bytes.iter().copied());
        self.calculate_rows();
        self.cursor = self.cursor.min(self.data.len());
        Some(removed)
    }

    /// Inserts bytes at `start`.  Legacy paste leaves the cursor at the first
    /// inserted byte rather than advancing it.
    pub fn insert_selection(&mut self, start: usize, selection: &[u8]) -> bool {
        if start > self.data.len() {
            return false;
        }
        self.cursor = start;
        self.data.splice(start..start, selection.iter().copied());
        self.calculate_rows();
        true
    }

    pub fn move_up(&mut self) {
        let Some(row_index) = self.cursor_row() else {
            return;
        };
        if row_index == 0 {
            return;
        }
        let column = self.cursor - self.rows[row_index].start;
        let target = self.rows[row_index - 1];
        self.cursor = (target.start + column).min(target.end);
    }

    pub fn move_down(&mut self) {
        let Some(row_index) = self.cursor_row() else {
            return;
        };
        if row_index + 1 >= self.rows.len() {
            return;
        }
        let column = self.cursor - self.rows[row_index].start;
        let target = self.rows[row_index + 1];
        self.cursor = (target.start + column).min(target.end);
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.cursor.saturating_add(1).min(self.data.len());
    }

    pub fn move_line_start(&mut self) {
        if let Some(row) = self.cursor_row() {
            self.cursor = self.rows[row].start;
        }
    }

    pub fn move_line_end(&mut self) {
        if let Some(row) = self.cursor_row() {
            self.cursor = self.rows[row].end;
        }
    }

    /// `repeat == 0` is the uncounted `g` case; both zero and one select row 1.
    pub fn move_file_start(&mut self, repeat: usize) {
        let row = repeat.max(1).min(self.rows.len());
        self.cursor = self.rows[row - 1].start;
    }

    /// Uncounted `G` selects logical EOF.  A count selects that one-based row.
    pub fn move_file_end(&mut self, repeat: usize) {
        if repeat == 0 {
            self.cursor = self.data.len();
        } else {
            let row = repeat.min(self.rows.len());
            self.cursor = self.rows[row - 1].start;
        }
    }

    fn is_word(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    fn is_space(byte: u8) -> bool {
        matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
    }

    /// Legacy `e`: byte/ASCII oriented, including its whitespace behavior.
    pub fn move_word_end(&mut self) {
        if self.cursor + 1 < self.data.len() && !Self::is_word(self.data[self.cursor + 1]) {
            self.cursor += 1;
        }
        while self.cursor + 1 < self.data.len()
            && (Self::is_word(self.data[self.cursor + 1]) || Self::is_space(self.data[self.cursor]))
        {
            self.cursor += 1;
        }
    }

    /// Legacy `w`, whose transition rules intentionally differ from Vim.
    pub fn move_word_next(&mut self) {
        while self.cursor < self.data.len()
            && (Self::is_word(self.data[self.cursor])
                || self
                    .data
                    .get(self.cursor + 1)
                    .is_some_and(|byte| Self::is_space(*byte)))
        {
            self.cursor += 1;
        }
        if self.cursor < self.data.len() {
            self.cursor += 1;
        }
    }

    /// Legacy `b`, made bounds-safe while retaining the characterized rules.
    pub fn move_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.cursor.min(self.data.len());
        if self.cursor > 1 && !Self::is_word(self.data[self.cursor - 1]) {
            self.cursor -= 1;
        }
        while self.cursor > 1
            && (Self::is_word(self.data[self.cursor - 1])
                || self
                    .data
                    .get(self.cursor + 1)
                    .is_some_and(|byte| Self::is_space(*byte)))
        {
            self.cursor -= 1;
        }
        if self.cursor == 1 {
            self.cursor = 0;
        }
    }

    /// Finds the next match without changing the buffer.  The current logical
    /// byte position is excluded and matching wraps.  Legacy absence is the
    /// unchanged cursor, which is deliberately ambiguous at byte zero.
    pub fn search_wrapped(&self, needle: &[u8]) -> usize {
        if self.data.is_empty() {
            return self.cursor;
        }

        let base = self.cursor % self.data.len();
        for offset in 1..self.data.len() {
            let position = (base + offset) % self.data.len();
            if needle.is_empty() {
                return position;
            }
            let Some(end) = position.checked_add(needle.len()) else {
                continue;
            };
            if self.data.get(position..end) == Some(needle) {
                return position;
            }
        }
        self.cursor
    }

    /// Searches, replaces only when the search moved, and leaves the cursor
    /// immediately after the inserted bytes.
    pub fn replace_first_after_cursor(&mut self, old: &[u8], new: &[u8]) -> bool {
        let position = self.search_wrapped(old);
        if position == self.cursor {
            return false;
        }
        self.cursor = position;
        for _ in 0..old.len() {
            let _ = self.delete_byte();
        }
        for byte in new {
            self.insert_byte(*byte);
        }
        true
    }

    pub fn matching_brace(byte: u8) -> Option<u8> {
        match byte {
            b'(' => Some(b')'),
            b')' => Some(b'('),
            b'[' => Some(b']'),
            b']' => Some(b'['),
            b'{' => Some(b'}'),
            b'}' => Some(b'{'),
            _ => None,
        }
    }

    pub fn is_opening_brace(byte: u8) -> bool {
        matches!(byte, b'(' | b'[' | b'{')
    }

    pub fn is_closing_brace(byte: u8) -> bool {
        matches!(byte, b')' | b']' | b'}')
    }

    pub fn matching_brace_index(&self, index: usize) -> Option<usize> {
        let &initial = self.data.get(index)?;
        let opposite = Self::matching_brace(initial)?;
        let quoted = quoted_bytes(&self.data);
        if quoted.get(index).copied().unwrap_or(false) {
            return None;
        }

        let mut depth = 0usize;
        if Self::is_opening_brace(initial) {
            for (position, is_quoted) in quoted.iter().copied().enumerate().skip(index + 1) {
                if is_quoted {
                    continue;
                }
                let byte = self.data[position];
                if byte == initial {
                    depth += 1;
                } else if byte == opposite {
                    if depth == 0 {
                        return Some(position);
                    }
                    depth -= 1;
                }
            }
        } else {
            for (position, is_quoted) in quoted[..index].iter().copied().enumerate().rev() {
                if is_quoted {
                    continue;
                }
                let byte = self.data[position];
                if byte == initial {
                    depth += 1;
                } else if byte == opposite {
                    if depth == 0 {
                        return Some(position);
                    }
                    depth -= 1;
                }
            }
        }
        None
    }

    /// Moves `%` only when the current byte has a safely bounded match.
    pub fn move_matching_brace(&mut self) -> bool {
        let Some(position) = self.matching_brace_index(self.cursor) else {
            return false;
        };
        self.cursor = position;
        true
    }
}

/// Marks bytes inside single- or double-quoted regions.  Backslash escapes are
/// honored so braces in quoted literals cannot affect matching or indentation.
fn quoted_bytes(data: &[u8]) -> Vec<bool> {
    let mut result = vec![false; data.len()];
    let mut quote = None;
    let mut escaped = false;

    for (index, byte) in data.iter().copied().enumerate() {
        if let Some(active) = quote {
            result[index] = true;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            result[index] = true;
            quote = Some(byte);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_at_reads_under_the_cursor_then_forward_on_the_line() {
        //                          0123456789012345
        let buffer = Buffer::new(b"the fox_1 ... go\nnext".to_vec());
        // Anywhere inside a word gives the whole word.
        assert_eq!(buffer.word_at(0), Some((0, 3)));
        assert_eq!(buffer.word_at(2), Some((0, 3)));
        assert_eq!(buffer.word_at(6), Some((4, 9)));
        // On punctuation, vim looks forward on the same line.
        assert_eq!(buffer.word_at(3), Some((4, 9)));
        assert_eq!(buffer.word_at(10), Some((14, 16)));
        // A line with nothing left ahead of the cursor has no word, and the
        // search never runs on into the next line.
        let trailing = Buffer::new(b"go ..\nword".to_vec());
        assert_eq!(trailing.word_at(3), None);
        assert_eq!(buffer.word_at(usize::MAX), None);
    }

    #[test]
    fn highlight_matches_whole_words_or_substrings_on_request() {
        let data = b"the then other the";
        let substring = Highlight {
            needle: b"the".to_vec(),
            whole_word: false,
        };
        assert_eq!(
            substring.matches(data),
            [(0, 3), (4, 7), (10, 13), (15, 18)]
        );

        let word = Highlight {
            needle: b"the".to_vec(),
            whole_word: true,
        };
        assert_eq!(word.matches(data), [(0, 3), (15, 18)]);
        assert_eq!(word.find(data, 1), Some(15));
        assert_eq!(word.find(data, 16), None);

        // Backwards, `rfind` stops short of its bound the way `find` starts
        // past it, so `n` and `N` never land where the cursor already is.
        assert_eq!(word.rfind(data, 18), Some(15));
        assert_eq!(word.rfind(data, 15), Some(0));
        assert_eq!(word.rfind(data, 0), None);
        assert_eq!(substring.rfind(data, 15), Some(10));

        // An empty or oversized needle matches nothing instead of looping.
        assert!(Highlight::default().matches(data).is_empty());
        assert!(Highlight::default().is_empty());
        let oversized = Highlight {
            needle: vec![b'x'; data.len() + 1],
            whole_word: false,
        };
        assert!(oversized.matches(data).is_empty());
        assert_eq!(oversized.rfind(data, data.len()), None);
        assert_eq!(Highlight::default().rfind(data, data.len()), None);
    }

    #[test]
    fn rows_cover_empty_final_newline_blank_binary_and_utf8_bytes() {
        assert_eq!(Buffer::new(vec![]).rows, vec![Row { start: 0, end: 0 }]);
        assert_eq!(
            Buffer::new(b"a".to_vec()).rows,
            vec![Row { start: 0, end: 1 }]
        );
        assert_eq!(
            Buffer::new(b"a\n".to_vec()).rows,
            vec![Row { start: 0, end: 1 }, Row { start: 2, end: 2 }]
        );
        assert_eq!(
            Buffer::new(b"\n\n".to_vec()).rows,
            vec![
                Row { start: 0, end: 0 },
                Row { start: 1, end: 1 },
                Row { start: 2, end: 2 }
            ]
        );
        let bytes = vec![0, 0xc3, 0xa9, b'\n', 0xff];
        let buffer = Buffer::new(bytes.clone());
        assert_eq!(buffer.data, bytes);
        assert_eq!(
            buffer.rows,
            vec![Row { start: 0, end: 3 }, Row { start: 4, end: 5 }]
        );
        assert!(buffer.invariants_hold());
    }

    #[test]
    fn row_queries_assign_newline_to_preceding_row_and_eof_to_final_row() {
        let mut buffer = Buffer::new(b"ab\ncd\n".to_vec());
        assert_eq!(buffer.row_for_index(2), Some(0));
        assert_eq!(buffer.row_for_index(3), Some(1));
        assert_eq!(buffer.row_for_index(5), Some(1));
        assert_eq!(buffer.row_for_index(6), Some(2));
        assert_eq!(buffer.row_for_index(7), None);
        buffer.cursor = 4;
        assert_eq!(buffer.cursor_row(), Some(1));
        assert_eq!(buffer.cursor_column(), Some(1));
    }

    #[test]
    fn inserting_clamps_advances_and_recalculates_rows() {
        let mut buffer = Buffer::new(b"ac".to_vec());
        buffer.cursor = 1;
        assert_eq!(buffer.insert_byte(b'b'), 2);
        assert_eq!(buffer.data, b"abc");
        buffer.cursor = 99;
        assert_eq!(buffer.insert_byte(b'\n'), 4);
        assert_eq!(buffer.data, b"abc\n");
        assert_eq!(
            buffer.rows,
            vec![Row { start: 0, end: 3 }, Row { start: 4, end: 4 }]
        );
        assert!(buffer.invariants_hold());
    }

    #[test]
    fn deleting_removes_under_cursor_and_eof_is_a_noop() {
        let mut buffer = Buffer::new(b"a\nb".to_vec());
        buffer.cursor = 1;
        assert!(buffer.delete_byte());
        assert_eq!(buffer.data, b"ab");
        assert_eq!(buffer.cursor, 1);
        assert_eq!(buffer.rows, vec![Row { start: 0, end: 2 }]);
        buffer.cursor = buffer.data.len();
        assert!(!buffer.delete_byte());
        assert_eq!(buffer.data, b"ab");
    }

    #[test]
    fn selection_copy_is_inclusive_but_deletion_and_undo_are_half_open() {
        let mut buffer = Buffer::new(b"ab\ncd".to_vec());
        let deletion = buffer.delete_selection(0, 3).unwrap();
        assert_eq!(deletion.clipboard, b"ab\nc");
        assert_eq!(deletion.undo, b"ab\n");
        assert_eq!(buffer.data, b"cd");
        assert_eq!(buffer.cursor, 0);

        let mut one = Buffer::new(b"xyz".to_vec());
        let deletion = one.delete_selection(1, 1).unwrap();
        assert_eq!(deletion.clipboard, b"y");
        assert!(deletion.undo.is_empty());
        assert_eq!(one.data, b"xyz");
    }

    #[test]
    fn selection_at_eof_copies_the_legacy_spare_nul() {
        let mut buffer = Buffer::new(b"abc".to_vec());
        let deletion = buffer.delete_selection(1, 3).unwrap();
        assert_eq!(deletion.clipboard, b"bc\0");
        assert_eq!(deletion.undo, b"bc");
        assert_eq!(buffer.data, b"a");
        assert!(buffer.delete_selection(1, 2).is_none());
        assert!(buffer.delete_selection(2, 1).is_none());
    }

    #[test]
    fn selection_insertion_keeps_cursor_at_start() {
        let mut buffer = Buffer::new(b"ad".to_vec());
        assert!(buffer.insert_selection(1, b"bc"));
        assert_eq!(buffer.data, b"abcd");
        assert_eq!(buffer.cursor, 1);
        assert!(!buffer.insert_selection(99, b"x"));
    }

    #[test]
    fn vertical_motion_rederives_column_and_has_no_sticky_column() {
        let mut buffer = Buffer::new(b"abcd\nx\nwxyz".to_vec());
        buffer.cursor = 3;
        buffer.move_down();
        assert_eq!(buffer.cursor, 6); // newline ending the short row
        buffer.move_down();
        assert_eq!(buffer.cursor, 8); // column one, not the original column three
        buffer.move_up();
        assert_eq!(buffer.cursor, 6);
        buffer.move_up();
        assert_eq!(buffer.cursor, 1);
        buffer.move_up();
        assert_eq!(buffer.cursor, 1);
    }

    #[test]
    fn horizontal_line_and_file_motions_use_byte_endpoints() {
        let mut buffer = Buffer::new(b"ab\ncd\n".to_vec());
        buffer.move_left();
        assert_eq!(buffer.cursor, 0);
        buffer.move_right();
        buffer.move_line_end();
        assert_eq!(buffer.cursor, 2);
        buffer.move_line_start();
        assert_eq!(buffer.cursor, 0);
        buffer.move_file_end(0);
        assert_eq!(buffer.cursor, 6);
        buffer.move_right();
        assert_eq!(buffer.cursor, 6);
        buffer.move_file_start(2);
        assert_eq!(buffer.cursor, 3);
        buffer.move_file_end(99);
        assert_eq!(buffer.cursor, 6);
        buffer.move_file_start(0);
        assert_eq!(buffer.cursor, 0);
    }

    #[test]
    fn word_motions_preserve_legacy_ascii_rules_and_are_bounded() {
        let mut next = Buffer::new(b"one  two.three".to_vec());
        next.move_word_next();
        assert_eq!(next.cursor, 5);
        next.move_word_next();
        assert_eq!(next.cursor, 9);

        let mut end = Buffer::new(b"one  two".to_vec());
        end.move_word_end();
        assert_eq!(end.cursor, 2);
        end.cursor = 3;
        end.move_word_end();
        assert_eq!(end.cursor, 7);

        let mut back = Buffer::new(b"one  two".to_vec());
        back.cursor = back.data.len();
        back.move_word_back();
        assert_eq!(back.cursor, 5);
        back.cursor = 1;
        back.move_word_back();
        assert_eq!(back.cursor, 0);

        let mut binary = Buffer::new(vec![0xff, b'_', b'a']);
        binary.move_word_next();
        assert_eq!(binary.cursor, 1);
    }

    #[test]
    fn search_excludes_current_wraps_overlaps_and_reports_absence_as_unchanged() {
        let mut buffer = Buffer::new(b"ababa".to_vec());
        assert_eq!(buffer.search_wrapped(b"aba"), 2);
        buffer.cursor = 3;
        assert_eq!(buffer.search_wrapped(b"aba"), 0);
        buffer.cursor = 1;
        assert_eq!(buffer.search_wrapped(b"missing"), 1);
        assert_eq!(buffer.search_wrapped(b""), 2);
        assert_eq!(Buffer::new(Vec::new()).search_wrapped(b""), 0);
    }

    #[test]
    fn replacement_requires_movement_and_places_cursor_after_new_bytes() {
        let mut shorter = Buffer::new(b"xx old yy old".to_vec());
        assert!(shorter.replace_first_after_cursor(b"old", b"Q"));
        assert_eq!(shorter.data, b"xx Q yy old");
        assert_eq!(shorter.cursor, 4);

        let mut longer = Buffer::new(b"old xx old".to_vec());
        assert!(longer.replace_first_after_cursor(b"old", b"long"));
        assert_eq!(longer.data, b"old xx long");
        assert_eq!(longer.cursor, 11);

        let mut only_current = Buffer::new(b"old".to_vec());
        assert!(!only_current.replace_first_after_cursor(b"old", b"new"));
        assert_eq!(only_current.data, b"old");
    }

    #[test]
    fn brace_matching_handles_nesting_quotes_escapes_and_unmatched_input() {
        let mut buffer = Buffer::new(br#"{ "}" {'}'} { [()] } }"#.to_vec());
        let final_brace = buffer.data.len() - 1;
        assert_eq!(buffer.matching_brace_index(0), Some(final_brace));
        assert!(buffer.move_matching_brace());
        assert_eq!(buffer.cursor, final_brace);
        assert!(buffer.move_matching_brace());
        assert_eq!(buffer.cursor, 0);

        let escaped = Buffer::new(br#"{ "quoted \\" }"#.to_vec());
        assert_eq!(
            escaped.matching_brace_index(0),
            Some(escaped.data.len() - 1)
        );

        let mut unmatched = Buffer::new(b"{ abc".to_vec());
        assert!(!unmatched.move_matching_brace());
        assert_eq!(unmatched.cursor, 0);
        unmatched.cursor = 2;
        assert!(!unmatched.move_matching_brace());
        assert_eq!(unmatched.cursor, 2);
    }

    #[test]
    fn validation_detects_public_field_corruption() {
        let mut buffer = Buffer::new(b"ok".to_vec());
        buffer.cursor = 3;
        assert_eq!(
            buffer.validate(),
            Err(InvariantError::CursorOutOfBounds { cursor: 3, len: 2 })
        );
        buffer.cursor = 0;
        buffer.rows.clear();
        assert!(matches!(
            buffer.validate(),
            Err(InvariantError::RowsOutOfDate { .. })
        ));
    }
}
