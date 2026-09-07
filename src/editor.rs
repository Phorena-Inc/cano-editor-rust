use crate::buffer::Buffer;
use crate::history::{History, HistoryError, UndoKind, UndoRecord};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Search,
    Command,
    Visual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertEntry {
    Cursor,
    FirstNonBlank,
    AfterCursor,
    LineEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Leader {
    #[default]
    None,
    Delete,
    Yank,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VisualSelection {
    pub start: usize,
    pub end: usize,
    /// Byte position where the selection was started.  Linewise updates
    /// derive `start`/`end` from the anchor row and the cursor row so the
    /// selection always spans whole rows in either direction.
    pub anchor: usize,
    pub linewise: bool,
}

#[derive(Clone, Debug)]
pub struct Editor {
    pub buffer: Buffer,
    pub mode: Mode,
    pub history: History,
    pub leader: Leader,
    pub clipboard: Vec<u8>,
    pub visual: VisualSelection,
    pub indent: usize,
    active_insert: UndoRecord,
}

impl Editor {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            buffer: Buffer::new(bytes),
            mode: Mode::Normal,
            history: History::default(),
            leader: Leader::None,
            clipboard: Vec::new(),
            visual: VisualSelection::default(),
            indent: 0,
            active_insert: UndoRecord::default(),
        }
    }

    fn begin_insert_record(&mut self) {
        self.active_insert = UndoRecord {
            kind: UndoKind::DeleteMultiple,
            start: self.buffer.cursor,
            ..UndoRecord::default()
        };
    }

    pub fn enter_insert(&mut self, entry: InsertEntry) {
        match entry {
            InsertEntry::Cursor => {}
            InsertEntry::FirstNonBlank => {
                self.buffer.move_line_start();
                let row = self.buffer.cursor_row().unwrap_or(0);
                let end = self.buffer.rows[row].end;
                while self.buffer.cursor < end
                    && self.buffer.data[self.buffer.cursor].is_ascii_whitespace()
                {
                    self.buffer.cursor += 1;
                }
            }
            InsertEntry::AfterCursor => {
                if self.buffer.cursor < self.buffer.data.len() {
                    self.buffer.cursor += 1;
                }
            }
            InsertEntry::LineEnd => self.buffer.move_line_end(),
        }
        self.mode = Mode::Insert;
        self.begin_insert_record();
    }

    /// Pushes the active insertion record when it covers at least one byte.
    /// Empty records would consume an undo press without changing anything.
    fn push_active_insert(&mut self) {
        if self.active_insert.start != self.active_insert.end {
            self.history.push_undo(self.active_insert.clone());
        }
    }

    pub fn leave_insert(&mut self) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        self.active_insert.end = self.buffer.cursor;
        self.push_active_insert();
        self.mode = Mode::Normal;
        self.begin_insert_record();
        true
    }

    pub fn insert_byte(&mut self, byte: u8) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        if matches!(byte, b')' | b']' | b'}')
            && self.buffer.data.get(self.buffer.cursor) == Some(&byte)
        {
            self.buffer.move_right();
            return true;
        }

        self.active_insert.end = self.buffer.insert_byte(byte);
        let partner = match byte {
            b'(' => Some(b')'),
            b'[' => Some(b']'),
            b'{' => Some(b'}'),
            _ => None,
        };
        if let Some(partner) = partner {
            self.active_insert.end = self.active_insert.end.saturating_sub(1);
            self.push_active_insert();
            let start = self.buffer.cursor.saturating_sub(1);
            let end = self.buffer.insert_byte(partner);
            self.history.push_undo(UndoRecord {
                kind: UndoKind::DeleteMultiple,
                start,
                end,
                ..UndoRecord::default()
            });
            self.buffer.move_left();
            self.begin_insert_record();
        }
        true
    }

    pub fn insert_backspace(&mut self) -> bool {
        if self.mode != Mode::Insert || self.buffer.cursor == 0 {
            return false;
        }
        if self.buffer.cursor > self.active_insert.start {
            // Removing a byte inserted by the active record: the record's
            // cursor-derived endpoint shrinks with the deletion.
            self.buffer.move_left();
            return self.buffer.delete_byte();
        }
        // Deleting a byte that predates the active record.  It needs its own
        // record, otherwise the pending record inverts (start > end) and every
        // older record applies at stale offsets.
        self.buffer.move_left();
        let deleted = self.buffer.data.get(self.buffer.cursor).copied();
        let removed = self.buffer.delete_byte();
        if removed {
            if let Some(byte) = deleted {
                self.history.push_undo(UndoRecord::insert_chars_exact(
                    self.buffer.cursor,
                    vec![byte],
                ));
            }
            self.begin_insert_record();
        }
        removed
    }

    pub fn insert_tab(&mut self) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        if self.indent == 0 {
            self.active_insert.end = self.buffer.insert_byte(b'\t');
        } else {
            for _ in 0..self.indent {
                self.active_insert.end = self.buffer.insert_byte(b' ');
            }
        }
        true
    }

    fn add_indent(&mut self, depth: usize) {
        if self.indent == 0 {
            for _ in 0..depth {
                self.buffer.insert_byte(b'\t');
            }
        } else {
            for _ in 0..self.indent.saturating_mul(depth) {
                self.buffer.insert_byte(b' ');
            }
        }
    }

    pub fn insert_newline(&mut self) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        self.active_insert.end = self.buffer.cursor;
        self.push_active_insert();
        let before = self.buffer.cursor;
        let closing_follows = self
            .buffer
            .data
            .get(before)
            .is_some_and(|byte| matches!(byte, b')' | b']' | b'}'));
        let depth = brace_depth(&self.buffer.data, before);
        self.begin_insert_record();
        self.buffer.insert_byte(b'\n');
        self.add_indent(depth);
        if closing_follows {
            // The expansion continues past where the cursor will rest, so the
            // cursor-derived active record cannot describe it.  Close out the
            // record for the first newline, give the expansion its own exact
            // record, and restart tracking at the cursor's resting position.
            self.active_insert.end = self.buffer.cursor;
            self.push_active_insert();
            let target = self.buffer.cursor;
            self.buffer.insert_byte(b'\n');
            self.add_indent(depth.saturating_sub(1));
            self.history.push_undo(UndoRecord::delete_multiple_exact(
                target,
                self.buffer.cursor,
            ));
            self.buffer.cursor = target;
            self.begin_insert_record();
        }
        self.active_insert.end = self.buffer.cursor;
        true
    }

    pub fn insert_move(&mut self, direction: MoveDirection) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        self.active_insert.end = self.buffer.cursor;
        self.push_active_insert();
        match direction {
            MoveDirection::Left => self.buffer.move_left(),
            MoveDirection::Right => self.buffer.move_right(),
            MoveDirection::Up => self.buffer.move_up(),
            MoveDirection::Down => self.buffer.move_down(),
        }
        self.begin_insert_record();
        true
    }

    pub fn open_line(&mut self, below: bool) {
        let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
        self.mode = Mode::Insert;
        if below {
            self.buffer.cursor = row.end;
            self.begin_insert_record();
            self.insert_newline();
            // `insert_newline` leaves the newline in the active record; push
            // it now so leaving insert mode cannot discard it.
            self.active_insert.end = self.buffer.cursor;
            self.push_active_insert();
        } else {
            // Terminate a fresh empty row above the current one, then rest
            // the cursor on that row with the surrounding indentation.
            self.buffer.cursor = row.start;
            let depth = brace_depth(&self.buffer.data, row.start);
            self.buffer.insert_byte(b'\n');
            self.buffer.cursor = row.start;
            self.add_indent(depth);
            self.history.push_undo(UndoRecord::delete_multiple_exact(
                row.start,
                self.buffer.cursor.saturating_add(1),
            ));
        }
        self.begin_insert_record();
    }

    pub fn open_line_normal(&mut self) {
        self.open_line(true);
        self.mode = Mode::Normal;
        self.active_insert = UndoRecord::default();
    }

    pub fn replace_next(&mut self, byte: u8) -> bool {
        let Some(slot) = self.buffer.data.get_mut(self.buffer.cursor) else {
            return false;
        };
        let displaced = std::mem::replace(slot, byte);
        // Either the displaced or the written byte may be a newline, so the
        // row table must be rederived to keep the buffer invariant.
        self.buffer.calculate_rows();
        self.history
            .push_undo(UndoRecord::replace_char(self.buffer.cursor, displaced));
        true
    }

    pub fn normal_key(&mut self, key: u8) -> bool {
        if self.mode != Mode::Normal {
            return false;
        }
        if self.leader == Leader::None {
            match key {
                b'd' => {
                    self.leader = Leader::Delete;
                    return true;
                }
                b'y' => {
                    self.leader = Leader::Yank;
                    return true;
                }
                _ => {}
            }
        }

        if self.leader == Leader::Delete
            && matches!(key, b'0' | b'$' | b'w' | b'b' | b'e' | b'g' | b'G')
        {
            self.delete_motion(key);
            self.leader = Leader::None;
            return true;
        }

        match key {
            b'x' => self.delete_character(),
            b'd' if self.leader == Leader::Delete => self.delete_current_row(),
            b'y' if self.leader == Leader::Yank => self.yank_current_row(),
            b'p' if !self.clipboard.is_empty() => self.paste(),
            b'i' => self.enter_insert(InsertEntry::Cursor),
            b'I' => self.enter_insert(InsertEntry::FirstNonBlank),
            b'a' => self.enter_insert(InsertEntry::AfterCursor),
            b'A' => self.enter_insert(InsertEntry::LineEnd),
            b'o' => self.open_line(true),
            b'O' => self.open_line(false),
            b'v' => self.start_visual(false),
            b'V' => self.start_visual(true),
            b'h' => self.buffer.move_left(),
            b'j' => self.buffer.move_down(),
            b'k' => self.buffer.move_up(),
            b'l' => self.buffer.move_right(),
            b'0' => self.buffer.move_line_start(),
            b'$' => self.buffer.move_line_end(),
            b'w' => self.buffer.move_word_next(),
            b'b' => self.buffer.move_word_back(),
            b'e' => self.buffer.move_word_end(),
            b'g' => self.buffer.move_file_start(0),
            b'G' => self.buffer.move_file_end(0),
            b'%' => {
                self.buffer.move_matching_brace();
            }
            _ => {
                self.leader = Leader::None;
                return false;
            }
        }
        self.leader = Leader::None;
        true
    }

    fn delete_character(&mut self) {
        let start = self.buffer.cursor;
        if let Some(deleted) = self.buffer.data.get(start).copied() {
            // The two-byte clipboard (with a synthetic NUL past EOF) is
            // characterized legacy behavior; a no-op `x` at EOF must not
            // clobber the clipboard, so it is only captured on deletion.
            self.clipboard = (0..2)
                .map(|offset| self.buffer.data.get(start + offset).copied().unwrap_or(0))
                .collect();
            self.buffer.delete_byte();
            self.history
                .push_undo(UndoRecord::insert_chars_exact(start, vec![deleted]));
        }
    }

    fn paste(&mut self) {
        if self.clipboard.first() == Some(&b'\n') {
            self.buffer.move_line_end();
        }
        let start = self.buffer.cursor;
        let end = start.saturating_add(self.clipboard.len());
        if self.buffer.insert_selection(start, &self.clipboard) {
            self.history
                .push_undo(UndoRecord::delete_multiple_exact(start, end));
        }
        if self.clipboard.first() == Some(&b'\n') && self.buffer.cursor < self.buffer.data.len() {
            self.buffer.cursor += 1;
        }
    }

    fn yank_current_row(&mut self) {
        let index = self.buffer.cursor_row().unwrap_or(0);
        let row = self.buffer.rows[index];
        self.clipboard.clear();
        if index == 0 {
            self.clipboard.push(b'\n');
            self.clipboard
                .extend_from_slice(&self.buffer.data[row.start..row.end]);
        } else {
            self.clipboard
                .extend_from_slice(&self.buffer.data[row.start - 1..row.end]);
        }
    }

    fn delete_current_row(&mut self) {
        let index = self.buffer.cursor_row().unwrap_or(0);
        let row = self.buffer.rows[index];
        let column = self.buffer.cursor.saturating_sub(row.start);
        let (start, end) = if index == 0 {
            (
                row.start,
                if self.buffer.rows.len() > 1 {
                    row.end + 1
                } else {
                    row.end
                },
            )
        } else {
            (row.start - 1, row.end)
        };
        if let Some(deleted) = self.buffer.delete_selection(start, end) {
            self.clipboard = deleted.clipboard;
            self.history.push_undo(UndoRecord {
                kind: UndoKind::InsertChars,
                data: deleted.undo,
                start,
                end,
            });
        }
        let target_index = index.min(self.buffer.rows.len().saturating_sub(1));
        let target = self.buffer.rows[target_index];
        self.buffer.cursor = (target.start + column).min(target.end);
    }

    pub fn delete_rows(&mut self, count: usize) {
        let first = self.buffer.cursor_row().unwrap_or(0);
        let available = self.buffer.rows.len().saturating_sub(first);
        for _ in 0..count.max(1).min(available) {
            self.delete_current_row();
        }
        self.leader = Leader::None;
    }

    fn delete_motion(&mut self, key: u8) {
        let original = self.buffer.cursor;
        let (start, end) = match key {
            b'0' => {
                self.buffer.move_line_start();
                (self.buffer.cursor, original)
            }
            b'$' => {
                self.buffer.move_line_end();
                (original, self.buffer.cursor)
            }
            b'w' => {
                self.buffer.move_word_next();
                (original, self.buffer.cursor)
            }
            b'b' => {
                self.buffer.move_word_back();
                (self.buffer.cursor, original)
            }
            b'e' => {
                // `e` rests on the last byte of the word; deletion includes it.
                self.buffer.move_word_end();
                (
                    original,
                    self.buffer
                        .cursor
                        .saturating_add(1)
                        .min(self.buffer.data.len()),
                )
            }
            b'g' => {
                // Linewise to the first row: consume the current row's newline
                // so no blank line is left behind.
                let row = self.buffer.cursor_row().unwrap_or(0);
                let end = self.buffer.rows[row].end;
                self.buffer.move_file_start(0);
                (
                    self.buffer.cursor,
                    if end < self.buffer.data.len() {
                        end + 1
                    } else {
                        end
                    },
                )
            }
            b'G' => {
                // Linewise to the final row: consume the newline that
                // terminates the preceding row.
                let row = self.buffer.cursor_row().unwrap_or(0);
                let start = self.buffer.rows[row].start;
                self.buffer.move_file_end(0);
                (
                    start.saturating_sub(usize::from(start > 0)),
                    self.buffer.cursor,
                )
            }
            _ => return,
        };
        if let Some(deleted) = self.buffer.delete_selection(start, end) {
            self.clipboard = deleted.clipboard;
            self.history.push_undo(UndoRecord {
                kind: UndoKind::InsertChars,
                data: deleted.undo,
                start,
                end,
            });
        }
    }

    pub fn start_visual(&mut self, linewise: bool) {
        if linewise {
            let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
            self.visual = VisualSelection {
                start: row.start,
                end: row.end,
                anchor: self.buffer.cursor,
                linewise,
            };
        } else {
            self.visual = VisualSelection {
                start: self.buffer.cursor,
                end: self.buffer.cursor,
                anchor: self.buffer.cursor,
                linewise,
            };
        }
        self.mode = Mode::Visual;
    }

    fn visual_bounds(&self) -> (usize, usize) {
        if self.visual.start <= self.visual.end {
            (self.visual.start, self.visual.end)
        } else {
            (self.visual.end, self.visual.start)
        }
    }

    pub fn visual_key(&mut self, key: u8) -> bool {
        if self.mode != Mode::Visual {
            return false;
        }
        match key {
            3 | 27 => {
                self.mode = Mode::Normal;
                self.visual = VisualSelection::default();
            }
            b'd' | b'x' => {
                let (start, end) = self.visual_bounds();
                // Linewise deletion removes whole rows, so it consumes the
                // trailing newline (or the preceding one on the final row).
                let (start, end) = if self.visual.linewise {
                    if end < self.buffer.data.len() {
                        (start, end + 1)
                    } else {
                        (start.saturating_sub(usize::from(start > 0)), end)
                    }
                } else {
                    (start, end)
                };
                if let Some(deleted) = self.buffer.delete_selection(start, end) {
                    self.clipboard = deleted.clipboard;
                    self.history.push_undo(UndoRecord {
                        kind: UndoKind::InsertChars,
                        data: deleted.undo,
                        start,
                        end,
                    });
                }
                self.mode = Mode::Normal;
            }
            b'y' => {
                let (start, end) = self.visual_bounds();
                self.clipboard = self
                    .buffer
                    .data
                    .get(start..end)
                    .unwrap_or_default()
                    .to_vec();
                self.clipboard
                    .push(self.buffer.data.get(end).copied().unwrap_or(0));
                self.buffer.cursor = start;
                self.mode = Mode::Normal;
            }
            b'>' => {
                self.indent_visual();
                self.mode = Mode::Normal;
            }
            b'<' => {
                self.unindent_visual();
                self.mode = Mode::Normal;
            }
            b'h' => self.buffer.move_left(),
            b'j' => self.buffer.move_down(),
            b'k' => self.buffer.move_up(),
            b'l' => self.buffer.move_right(),
            b'0' => self.buffer.move_line_start(),
            b'$' => self.buffer.move_line_end(),
            b'w' => self.buffer.move_word_next(),
            b'b' => self.buffer.move_word_back(),
            b'e' => self.buffer.move_word_end(),
            b'g' => self.buffer.move_file_start(0),
            b'G' => self.buffer.move_file_end(0),
            b'%' => {
                self.buffer.move_matching_brace();
            }
            _ => return false,
        }
        self.refresh_visual();
        true
    }

    /// The byte range of the whole rows the selection covers.
    ///
    /// Indentation belongs to a line rather than to a span inside it, so an
    /// operator that rewrites it takes every line the selection touches.
    pub fn visual_rows(&self) -> Option<(usize, usize)> {
        if self.mode != Mode::Visual {
            return None;
        }
        let (start, end) = self.visual_bounds();
        let first = self.buffer.row_for_index(start)?;
        let last = self.buffer.row_for_index(end)?;
        Some((self.buffer.rows[first].start, self.buffer.rows[last].end))
    }

    /// Recomputes the selection from its anchor and the cursor.
    ///
    /// Every visual motion ends here, so anything else that moves the cursor
    /// while Visual mode is active has to call it too or the selection is
    /// left behind: a jump would move the cursor without taking the
    /// highlighted range with it.
    pub fn refresh_visual(&mut self) {
        if self.mode != Mode::Visual {
            return;
        }
        if self.visual.linewise {
            let anchor = self.buffer.row_for_index(self.visual.anchor).unwrap_or(0);
            let current = self.buffer.cursor_row().unwrap_or(0);
            let (first, last) = if anchor <= current {
                (anchor, current)
            } else {
                (current, anchor)
            };
            self.visual.start = self.buffer.rows[first].start;
            self.visual.end = self.buffer.rows[last].end;
        } else {
            self.visual.end = self.buffer.cursor;
        }
    }

    fn indent_visual(&mut self) {
        let (start, end) = self.visual_bounds();
        let first = self.buffer.row_for_index(start).unwrap_or(0);
        let last = self.buffer.row_for_index(end).unwrap_or(first);
        let indentation = if self.indent == 0 {
            vec![b'\t']
        } else {
            vec![b' '; self.indent]
        };
        for row in first..=last {
            let at = self.buffer.rows[row].start;
            if self.buffer.insert_selection(at, &indentation) {
                self.history.push_undo(UndoRecord::delete_multiple_exact(
                    at,
                    at.saturating_add(indentation.len()),
                ));
            }
        }
    }

    fn unindent_visual(&mut self) {
        let (start, end) = self.visual_bounds();
        let first = self.buffer.row_for_index(start).unwrap_or(0);
        let last = self.buffer.row_for_index(end).unwrap_or(first);
        for row in first..=last {
            for _ in 0..self.indent.max(1) {
                let at = self.buffer.rows[row].start;
                let Some(byte) = self
                    .buffer
                    .data
                    .get(at)
                    .copied()
                    .filter(u8::is_ascii_whitespace)
                else {
                    continue;
                };
                self.buffer.cursor = at;
                if self.buffer.delete_byte() {
                    self.history
                        .push_undo(UndoRecord::insert_chars_exact(at, vec![byte]));
                }
            }
        }
    }

    /// A failed application is reported instead of silently consuming the
    /// history step; the caller surfaces it as a status message.
    pub fn undo(&mut self) -> Result<bool, HistoryError> {
        self.history.undo(&mut self.buffer)
    }

    pub fn redo(&mut self) -> Result<bool, HistoryError> {
        self.history.redo(&mut self.buffer)
    }
}

/// The nesting depth at `end`, ignoring brackets inside string literals.
pub fn brace_depth(data: &[u8], end: usize) -> usize {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for byte in data.iter().copied().take(end) {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if matches!(byte, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(byte, b')' | b']' | b'}') {
            depth = depth.saturating_sub(1);
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_entries_autoclose_and_indentation_are_documented() {
        let mut editor = Editor::new(b"  value".to_vec());
        editor.enter_insert(InsertEntry::FirstNonBlank);
        assert_eq!(editor.buffer.cursor, 2);
        editor.insert_byte(b'{');
        assert_eq!(&editor.buffer.data[2..], b"{}value");
        assert_eq!(editor.buffer.cursor, 3);
        editor.insert_newline();
        assert_eq!(&editor.buffer.data[2..], b"{\n\t\n}value");
    }

    #[test]
    fn normal_and_visual_ranges_preserve_mixed_endpoint_rules() {
        let mut editor = Editor::new(b"abcd".to_vec());
        editor.buffer.cursor = 1;
        editor.normal_key(b'x');
        assert_eq!(editor.buffer.data, b"acd");
        assert_eq!(editor.clipboard, b"bc");

        let mut visual = Editor::new(b"abcd".to_vec());
        visual.buffer.cursor = 1;
        visual.normal_key(b'v');
        visual.visual_key(b'd');
        assert_eq!(visual.buffer.data, b"abcd");
        assert_eq!(visual.clipboard, b"b");
    }

    #[test]
    fn normal_x_undo_and_redo_round_trip_repeatedly() {
        let mut editor = Editor::new(b"abcd".to_vec());
        editor.buffer.cursor = 1;

        assert!(editor.normal_key(b'x'));
        assert_eq!(editor.buffer.data, b"acd");
        assert_eq!(editor.clipboard, b"bc");
        assert!(editor.buffer.invariants_hold());

        for _ in 0..2 {
            assert!(editor.undo().unwrap());
            assert_eq!(editor.buffer.data, b"abcd");
            assert_eq!(editor.buffer.cursor, 1);
            assert!(editor.buffer.invariants_hold());

            assert!(editor.redo().unwrap());
            assert_eq!(editor.buffer.data, b"acd");
            assert_eq!(editor.buffer.cursor, 1);
            assert!(editor.buffer.invariants_hold());
        }
    }

    #[test]
    fn normal_paste_undo_and_redo_use_the_inserted_byte_range() {
        let mut editor = Editor::new(b"ad".to_vec());
        editor.buffer.cursor = 1;
        editor.clipboard = b"bc".to_vec();

        assert!(editor.normal_key(b'p'));
        assert_eq!(editor.buffer.data, b"abcd");
        assert_eq!(editor.buffer.cursor, 1);
        assert!(editor.buffer.invariants_hold());

        for _ in 0..2 {
            assert!(editor.undo().unwrap());
            assert_eq!(editor.buffer.data, b"ad");
            assert_eq!(editor.buffer.cursor, 1);
            assert!(editor.buffer.invariants_hold());

            assert!(editor.redo().unwrap());
            assert_eq!(editor.buffer.data, b"abcd");
            assert_eq!(editor.buffer.cursor, 1);
            assert!(editor.buffer.invariants_hold());
        }
    }

    #[test]
    fn linewise_paste_records_the_post_motion_insertion_bounds() {
        let mut editor = Editor::new(b"a\nb".to_vec());
        editor.clipboard = b"\nrow".to_vec();

        assert!(editor.normal_key(b'p'));
        assert_eq!(editor.buffer.data, b"a\nrow\nb");
        assert!(editor.buffer.invariants_hold());

        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"a\nb");
        assert_eq!(editor.buffer.cursor, 1);
        assert!(editor.buffer.invariants_hold());

        assert!(editor.redo().unwrap());
        assert_eq!(editor.buffer.data, b"a\nrow\nb");
        assert_eq!(editor.buffer.cursor, 1);
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn backspace_over_preexisting_text_stays_undoable() {
        let mut editor = Editor::new(b"xy".to_vec());
        editor.buffer.cursor = 1;
        editor.enter_insert(InsertEntry::Cursor);
        assert!(editor.insert_backspace());
        assert_eq!(editor.buffer.data, b"y");
        editor.leave_insert();

        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"xy");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn open_line_below_keeps_the_newline_undoable() {
        let mut editor = Editor::new(b"old".to_vec());
        editor.open_line(true);
        editor.insert_byte(b'x');
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"old\nx");

        assert!(editor.undo().unwrap()); // typed text
        assert!(editor.undo().unwrap()); // opened line
        assert_eq!(editor.buffer.data, b"old");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn open_line_above_places_the_cursor_on_the_new_line() {
        let mut editor = Editor::new(b"old".to_vec());
        editor.normal_key(b'O');
        editor.insert_byte(b'x');
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"x\nold");

        assert!(editor.undo().unwrap());
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"old");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn replace_with_a_newline_keeps_row_metadata_valid() {
        let mut editor = Editor::new(b"abc".to_vec());
        editor.buffer.cursor = 1;
        assert!(editor.replace_next(b'\n'));
        assert_eq!(editor.buffer.data, b"a\nc");
        assert!(editor.buffer.invariants_hold());
        assert_eq!(editor.buffer.rows.len(), 2);
    }

    #[test]
    fn linewise_visual_selection_upward_deletes_whole_rows() {
        let mut editor = Editor::new(b"aa\nbb\ncc".to_vec());
        editor.buffer.cursor = 6; // on "cc"
        editor.normal_key(b'V');
        editor.visual_key(b'k');
        editor.visual_key(b'k');
        editor.visual_key(b'd');
        assert_eq!(editor.buffer.data, b"");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn linewise_visual_selection_downward_leaves_no_stray_newline() {
        let mut editor = Editor::new(b"aa\nbb\ncc".to_vec());
        editor.normal_key(b'V');
        editor.visual_key(b'j');
        editor.visual_key(b'd');
        assert_eq!(editor.buffer.data, b"cc");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn visual_indent_and_unindent_are_undoable() {
        let mut editor = Editor::new(b"a\nb".to_vec());
        editor.normal_key(b'V');
        editor.visual_key(b'j');
        editor.visual_key(b'>');
        assert_eq!(editor.buffer.data, b"\ta\n\tb");

        assert!(editor.undo().unwrap());
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"a\nb");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn delete_motions_remove_exactly_the_motion_span() {
        let mut to_end = Editor::new(b"abcd".to_vec());
        to_end.normal_key(b'd');
        to_end.normal_key(b'$');
        assert_eq!(to_end.buffer.data, b"");

        let mut word = Editor::new(b"one two".to_vec());
        word.normal_key(b'd');
        word.normal_key(b'w');
        assert_eq!(word.buffer.data, b"two");

        let mut to_first = Editor::new(b"aa\nbb\ncc".to_vec());
        to_first.buffer.cursor = 3;
        to_first.normal_key(b'd');
        to_first.normal_key(b'g');
        assert_eq!(to_first.buffer.data, b"cc");

        let mut to_last = Editor::new(b"aa\nbb\ncc".to_vec());
        to_last.buffer.cursor = 3;
        to_last.normal_key(b'd');
        to_last.normal_key(b'G');
        assert_eq!(to_last.buffer.data, b"aa");
    }

    #[test]
    fn x_at_eof_does_not_clobber_the_clipboard() {
        let mut editor = Editor::new(b"ab".to_vec());
        editor.clipboard = b"kept".to_vec();
        editor.buffer.cursor = editor.buffer.data.len();
        editor.normal_key(b'x');
        assert_eq!(editor.clipboard, b"kept");
    }

    #[test]
    fn entering_and_leaving_insert_mode_pushes_no_empty_record() {
        let mut editor = Editor::new(b"text".to_vec());
        editor.enter_insert(InsertEntry::Cursor);
        editor.leave_insert();
        assert!(editor.history.undo.is_empty());
        assert!(!editor.undo().unwrap());
    }

    #[test]
    fn normal_x_at_eof_does_not_create_an_unusable_history_record() {
        let mut editor = Editor::new(b"abc".to_vec());
        editor.buffer.cursor = editor.buffer.data.len();

        assert!(editor.normal_key(b'x'));
        assert_eq!(editor.buffer.data, b"abc");
        assert!(editor.history.undo.is_empty());
        assert!(!editor.undo().unwrap());
        assert!(!editor.redo().unwrap());
        assert!(editor.buffer.invariants_hold());
    }
}
