use crate::buffer::{Buffer, Nesting, is_keyword};
use crate::history::{History, HistoryError, UndoRecord};
use crate::textobject::{self, Scope};

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
    Change,
}

impl Leader {
    pub fn is_operator(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// The shape of a visual selection: vim's `v`, `V` and `Ctrl-V`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VisualKind {
    #[default]
    Charwise,
    Linewise,
    /// A rectangle, addressed by the byte columns the anchor and the cursor
    /// sit in rather than by one run of bytes.
    Blockwise,
}

impl VisualKind {
    pub fn is_linewise(self) -> bool {
        matches!(self, Self::Linewise)
    }

    pub fn is_blockwise(self) -> bool {
        matches!(self, Self::Blockwise)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VisualSelection {
    pub start: usize,
    pub end: usize,
    /// Byte position where the selection was started.  Linewise updates
    /// derive `start`/`end` from the anchor row and the cursor row so the
    /// selection always spans whole rows in either direction, and a
    /// blockwise rectangle is derived from it and the cursor the same way.
    pub anchor: usize,
    pub kind: VisualKind,
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
    /// Set by the `i`/`a` of an operator-pending `diw`, and spent by the key
    /// that names the object.  It is separate from `leader` because the two
    /// are independent: every operator takes every object.
    object: Option<Scope>,
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
            object: None,
            active_insert: UndoRecord::default(),
        }
    }

    /// Abandons a half-typed operator command, `i`/`a` included.
    ///
    /// Callers used to clear `leader` directly; an armed object has to go with
    /// it, or the next key typed would be read as the object of an operator
    /// that is no longer pending.
    pub fn cancel_pending(&mut self) {
        self.leader = Leader::None;
        self.object = None;
    }

    /// Whether an operator is still waiting for the keys that complete it.
    pub fn pending_operator(&self) -> bool {
        self.leader.is_operator() || self.object.is_some()
    }

    fn begin_insert_record(&mut self) {
        self.active_insert = UndoRecord::delete_multiple(self.buffer.cursor, 0);
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
        let partner = Buffer::matching_brace(byte).filter(|_| Buffer::is_opening_brace(byte));
        if let Some(partner) = partner {
            self.active_insert.end = self.active_insert.end.saturating_sub(1);
            self.push_active_insert();
            let start = self.buffer.cursor.saturating_sub(1);
            let end = self.buffer.insert_byte(partner);
            self.history
                .push_undo(UndoRecord::delete_multiple(start, end));
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
        for byte in self.indentation() {
            self.active_insert.end = self.buffer.insert_byte(byte);
        }
        true
    }

    fn add_indent(&mut self, depth: usize) {
        for byte in self.indentation().repeat(depth) {
            self.buffer.insert_byte(byte);
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
        // The key after an operator's `i`/`a` names the object it works on.
        // It is taken before anything else so that `di` cannot fall through to
        // `i` and open Insert mode in the middle of a delete.
        if let Some(scope) = self.object.take() {
            let leader = self.leader;
            self.leader = Leader::None;
            if !leader.is_operator() {
                return false;
            }
            let found = textobject::range(&self.buffer, self.buffer.cursor, scope, key);
            if let Some((start, end)) = found {
                self.apply_operator(leader, start, end);
            }
            return true;
        }

        let operator = match key {
            b'd' => Leader::Delete,
            b'y' => Leader::Yank,
            b'c' => Leader::Change,
            _ => Leader::None,
        };
        if self.leader == Leader::None && operator.is_operator() {
            self.leader = operator;
            return true;
        }

        if self.leader.is_operator() && matches!(key, b'i' | b'a') {
            self.object = Some(if key == b'i' {
                Scope::Inner
            } else {
                Scope::Around
            });
            return true;
        }

        if self.leader.is_operator()
            && matches!(key, b'0' | b'$' | b'w' | b'b' | b'e' | b'g' | b'G')
        {
            let leader = self.leader;
            self.leader = Leader::None;
            if let Some((start, end)) = self.motion_range(leader, key) {
                self.apply_operator(leader, start, end);
            }
            return true;
        }

        match key {
            b'x' => self.delete_character(),
            b'd' if self.leader == Leader::Delete => self.delete_current_row(),
            b'y' if self.leader == Leader::Yank => self.yank_current_row(),
            b'c' if self.leader == Leader::Change => self.change_current_row(),
            b'p' if !self.clipboard.is_empty() => self.paste(),
            b'i' => self.enter_insert(InsertEntry::Cursor),
            b'I' => self.enter_insert(InsertEntry::FirstNonBlank),
            b'a' => self.enter_insert(InsertEntry::AfterCursor),
            b'A' => self.enter_insert(InsertEntry::LineEnd),
            b'o' => self.open_line(true),
            b'O' => self.open_line(false),
            b'v' => self.start_visual(VisualKind::Charwise),
            b'V' => self.start_visual(VisualKind::Linewise),
            _ => {
                if !self.motion(key) {
                    self.leader = Leader::None;
                    return false;
                }
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
            self.clipboard = self
                .buffer
                .copy_selection(start, start + 1)
                .unwrap_or_default();
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
        let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
        // Every row after the first starts after the newline that ends the
        // one before it; the first gets one supplied.  Either way the
        // clipboard leads with the `\n` that makes `p` paste linewise.
        self.clipboard = [&b"\n"[..], &self.buffer.data[row.start..row.end]].concat();
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
        self.cut(start, end);
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

    /// The byte range a motion covers when an operator is waiting on it.
    ///
    /// The cursor is moved to work the range out, exactly as the bare motion
    /// would move it; the caller's operator is what decides where it ends up.
    fn motion_range(&mut self, leader: Leader, key: u8) -> Option<(usize, usize)> {
        let original = self.buffer.cursor;
        let linewise_change = leader == Leader::Change;
        // Vim's one irregular operator-motion pair: `cw` on a non-blank
        // changes to the end of the word the way `ce` does, instead of taking
        // the blanks after it and leaving what you type jammed against the
        // next word.
        let key = if leader == Leader::Change
            && key == b'w'
            && self
                .buffer
                .data
                .get(original)
                .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            b'e'
        } else {
            key
        };
        let range = match key {
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
                // so no blank line is left behind.  A change keeps it, because
                // `cg` leaves you typing on a line that has to exist.
                let row = self.buffer.cursor_row().unwrap_or(0);
                let end = self.buffer.rows[row].end;
                self.buffer.move_file_start(0);
                (
                    self.buffer.cursor,
                    if end < self.buffer.data.len() && !linewise_change {
                        end + 1
                    } else {
                        end
                    },
                )
            }
            b'G' => {
                // Linewise to the final row: consume the newline that
                // terminates the preceding row, for the same reason.
                let row = self.buffer.cursor_row().unwrap_or(0);
                let start = self.buffer.rows[row].start;
                self.buffer.move_file_end(0);
                (
                    if linewise_change {
                        start
                    } else {
                        start.saturating_sub(usize::from(start > 0))
                    },
                    self.buffer.cursor,
                )
            }
            _ => return None,
        };
        // Post: given a cursor inside the buffer, every motion yields a
        // forward range inside it too.
        debug_assert!(
            range.0 <= range.1 && range.1 <= self.buffer.data.len(),
            "{range:?}"
        );
        Some(range)
    }

    /// Runs `leader` over `start..end`, the one place `d`, `c` and `y` differ.
    ///
    /// `d` and `c` share a deletion so the buffer edit and the undo record
    /// they leave are the same one; `c` then opens Insert mode where the text
    /// used to be.  A yank takes the range exactly, without the spare byte
    /// [`Buffer::copy_selection`] appends for the legacy clipboard, since a
    /// motion's range is half-open and already ends where it should.
    fn apply_operator(&mut self, leader: Leader, start: usize, end: usize) {
        if start > end || end > self.buffer.data.len() {
            return;
        }
        match leader {
            Leader::Yank => {
                self.clipboard = self.buffer.data[start..end].to_vec();
                self.buffer.cursor = start;
            }
            Leader::Delete | Leader::Change => {
                self.cut(start, end);
                if leader == Leader::Change {
                    self.buffer.cursor = start.min(self.buffer.data.len());
                    self.enter_insert(InsertEntry::Cursor);
                }
            }
            Leader::None => {}
        }
    }

    /// Deletes `start..end` into the clipboard, recorded with the legacy
    /// `InsertChars` inverse every cut uses.
    fn cut(&mut self, start: usize, end: usize) {
        if let Some(deleted) = self.buffer.delete_selection(start, end) {
            self.clipboard = deleted.clipboard;
            self.history
                .push_undo(UndoRecord::insert_chars(start, deleted.undo));
        }
    }

    /// `cc`: replaces the row's contents, keeping the row itself.
    ///
    /// Unlike `dd` this never takes the newline, because the whole point is to
    /// leave a line to type on.  Vim re-indents here; Cano has no `autoindent`
    /// to consult, so the line is left empty.
    fn change_current_row(&mut self) {
        let Some(index) = self.buffer.cursor_row() else {
            return;
        };
        let row = self.buffer.rows[index];
        self.apply_operator(Leader::Change, row.start, row.end);
    }

    pub fn start_visual(&mut self, kind: VisualKind) {
        if kind.is_linewise() {
            let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
            self.visual = VisualSelection {
                start: row.start,
                end: row.end,
                anchor: self.buffer.cursor,
                kind,
            };
        } else {
            self.visual = VisualSelection {
                start: self.buffer.cursor,
                end: self.buffer.cursor,
                anchor: self.buffer.cursor,
                kind,
            };
        }
        self.mode = Mode::Visual;
    }

    /// The rows and byte columns a blockwise selection covers, inclusive at
    /// both ends.
    ///
    /// Columns are byte offsets into their row rather than display columns,
    /// so the rectangle the operators cut is exactly the one the highlight
    /// draws even where a tab makes the two disagree on screen.
    pub fn block_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        if !self.visual.kind.is_blockwise() {
            return None;
        }
        let anchor_row = self.buffer.row_for_index(self.visual.anchor)?;
        let cursor_row = self.buffer.row_for_index(self.visual.end)?;
        let anchor_column = self
            .visual
            .anchor
            .saturating_sub(self.buffer.rows[anchor_row].start);
        let cursor_column = self
            .visual
            .end
            .saturating_sub(self.buffer.rows[cursor_row].start);
        Some((
            anchor_row.min(cursor_row),
            anchor_row.max(cursor_row),
            anchor_column.min(cursor_column),
            anchor_column.max(cursor_column),
        ))
    }

    /// The byte range one row of a blockwise selection contributes.
    ///
    /// A row shorter than the rectangle contributes an empty range rather
    /// than reaching into the next line.
    fn block_row_range(&self, row: usize, left: usize, right: usize) -> (usize, usize) {
        let bounds = self.buffer.rows[row];
        let start = bounds.start.saturating_add(left).min(bounds.end);
        let end = bounds
            .start
            .saturating_add(right)
            .saturating_add(1)
            .min(bounds.end);
        (start, end)
    }

    fn visual_bounds(&self) -> (usize, usize) {
        let (start, end) = (self.visual.start, self.visual.end);
        (start.min(end), start.max(end))
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
            b'd' | b'x' if self.visual.kind.is_blockwise() => {
                self.delete_block();
                self.mode = Mode::Normal;
            }
            b'y' if self.visual.kind.is_blockwise() => {
                self.yank_block();
                self.mode = Mode::Normal;
            }
            b'd' | b'x' => {
                let (start, end) = self.visual_bounds();
                // Linewise deletion removes whole rows, so it consumes the
                // trailing newline (or the preceding one on the final row).
                let (start, end) = if self.visual.kind.is_linewise() {
                    if end < self.buffer.data.len() {
                        (start, end + 1)
                    } else {
                        (start.saturating_sub(usize::from(start > 0)), end)
                    }
                } else {
                    (start, end)
                };
                self.cut(start, end);
                self.mode = Mode::Normal;
            }
            b'y' => {
                let (start, end) = self.visual_bounds();
                // A selection ending past EOF yanks only the legacy NUL.
                self.clipboard = self
                    .buffer
                    .copy_selection(start, end)
                    .unwrap_or_else(|| vec![0]);
                self.buffer.cursor = start;
                self.mode = Mode::Normal;
            }
            b'>' | b'<' => {
                let (first, last) = self.selected_rows();
                for row in first..=last {
                    self.shift_row(row, key == b'>');
                }
                self.mode = Mode::Normal;
            }
            _ => {
                if !self.motion(key) {
                    return false;
                }
            }
        }
        self.refresh_visual();
        true
    }

    /// The cursor motions Normal and Visual mode share.
    fn motion(&mut self, key: u8) -> bool {
        match key {
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
        if self.visual.kind.is_linewise() {
            let anchor = self.buffer.row_for_index(self.visual.anchor).unwrap_or(0);
            let current = self.buffer.cursor_row().unwrap_or(0);
            self.visual.start = self.buffer.rows[anchor.min(current)].start;
            self.visual.end = self.buffer.rows[anchor.max(current)].end;
        } else {
            self.visual.end = self.buffer.cursor;
        }
    }

    /// One shiftwidth of indentation, as the bytes it is written with.
    fn indentation(&self) -> Vec<u8> {
        if self.indent == 0 {
            vec![b'\t']
        } else {
            vec![b' '; self.indent]
        }
    }

    /// Adds or removes one shiftwidth at the start of `row`, reporting how
    /// many bytes the row grew, or shrank when the count is negative.
    ///
    /// The cursor is used as scratch space by the deletion path, so a caller
    /// that cares where it ends up has to put it back.
    fn shift_row(&mut self, row: usize, right: bool) -> isize {
        if right {
            let indentation = self.indentation();
            let at = self.buffer.rows[row].start;
            if !self.buffer.insert_selection(at, &indentation) {
                return 0;
            }
            self.history.push_undo(UndoRecord::delete_multiple_exact(
                at,
                at.saturating_add(indentation.len()),
            ));
            return isize::try_from(indentation.len()).unwrap_or(0);
        }
        let mut removed = 0usize;
        for _ in 0..self.indent.max(1) {
            // Only spaces and tabs are indentation.  `is_ascii_whitespace`
            // also covers CR and LF, and on a blank row the first byte is
            // the row's own terminator: eating it would join the row to the
            // next one and drop a row out from under the caller's loop.
            let Some(at) = self.buffer.rows.get(row).map(|bounds| bounds.start) else {
                break;
            };
            let Some(byte) = self
                .buffer
                .data
                .get(at)
                .copied()
                .filter(|byte| matches!(byte, b' ' | b'\t'))
            else {
                break;
            };
            self.buffer.cursor = at;
            if !self.buffer.delete_byte() {
                break;
            }
            self.history
                .push_undo(UndoRecord::insert_chars_exact(at, vec![byte]));
            removed = removed.saturating_add(1);
        }
        -isize::try_from(removed).unwrap_or(0)
    }

    /// The rows a visual selection touches, whatever its shape.
    fn selected_rows(&self) -> (usize, usize) {
        let (start, end) = self.visual_bounds();
        let first = self.buffer.row_for_index(start).unwrap_or(0);
        let last = self.buffer.row_for_index(end).unwrap_or(first);
        (first, last)
    }

    /// Deletes the rectangle a blockwise selection covers.
    ///
    /// The rows are cut from the bottom up so the offsets of the rows still
    /// to come do not move underneath the loop, and the whole rectangle goes
    /// onto the undo stack as a single region rewrite: a block delete is one
    /// command, and has to come back in one press of `u`.
    fn delete_block(&mut self) {
        let Some((first, last, left, right)) = self.block_bounds() else {
            return;
        };
        let region_start = self.buffer.rows[first].start;
        let region_end = self.buffer.rows[last].end;
        let Some(original) = self
            .buffer
            .data
            .get(region_start..region_end)
            .map(<[u8]>::to_vec)
        else {
            return;
        };
        // One entry per row, including the rows too short to reach the
        // rectangle: a block yank keeps its shape, blank lines and all.
        let mut pieces = vec![Vec::new(); last.saturating_sub(first).saturating_add(1)];
        for row in (first..=last).rev() {
            let (start, end) = self.block_row_range(row, left, right);
            if start >= end {
                continue;
            }
            if let Some(deleted) = self.buffer.delete_selection(start, end) {
                pieces[row - first] = deleted.undo;
            }
        }
        self.clipboard = pieces.join(&b'\n');
        let end = self.buffer.rows[last].end;
        self.history
            .push_undo(UndoRecord::replace_region(region_start, end, original));
        let landing = self.buffer.rows[first];
        self.buffer.cursor = landing.start.saturating_add(left).min(landing.end);
    }

    /// Copies the rectangle a blockwise selection covers, one row per line.
    ///
    /// Cano's clipboard is a flat run of bytes with no shape of its own, so
    /// what comes back out of `p` is those lines rather than a column.
    fn yank_block(&mut self) {
        let Some((first, last, left, right)) = self.block_bounds() else {
            return;
        };
        let mut pieces: Vec<Vec<u8>> = Vec::new();
        for row in first..=last {
            let (start, end) = self.block_row_range(row, left, right);
            pieces.push(
                self.buffer
                    .data
                    .get(start..end)
                    .unwrap_or_default()
                    .to_vec(),
            );
        }
        self.clipboard = pieces.join(&b'\n');
        let landing = self.buffer.rows[first];
        self.buffer.cursor = landing.start.saturating_add(left).min(landing.end);
    }

    /// Vim's `i_CTRL-W`: removes the word before the cursor.
    ///
    /// Whitespace before the cursor goes first, then one run of keyword
    /// bytes or one run of punctuation, which is how vim divides a line into
    /// words for this key.
    pub fn insert_delete_word(&mut self) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
        let mut target = self.buffer.cursor.clamp(row.start, row.end);
        while target > row.start && self.buffer.data[target - 1].is_ascii_whitespace() {
            target -= 1;
        }
        if target > row.start {
            let keyword = is_keyword(self.buffer.data[target - 1]);
            while target > row.start {
                let byte = self.buffer.data[target - 1];
                if byte.is_ascii_whitespace() || is_keyword(byte) != keyword {
                    break;
                }
                target -= 1;
            }
        }
        self.delete_back_to(target)
    }

    /// Vim's `i_CTRL-U`: removes what is in front of the cursor on this line.
    ///
    /// The indent is kept unless the cursor was already sitting in it, which
    /// is what vim does and what makes the key safe to lean on while typing
    /// an indented line.
    pub fn insert_delete_to_line_start(&mut self) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
        let mut target = row.start;
        while target < row.end && self.buffer.data[target].is_ascii_whitespace() {
            target += 1;
        }
        if target >= self.buffer.cursor {
            target = row.start;
        }
        self.delete_back_to(target)
    }

    /// Backspaces to `target`, so every byte removed is recorded the way a
    /// typed backspace would have recorded it.
    fn delete_back_to(&mut self, target: usize) -> bool {
        let count = self.buffer.cursor.saturating_sub(target);
        for _ in 0..count {
            self.insert_backspace();
        }
        count > 0
    }

    /// Vim's `i_CTRL-T` and `i_CTRL-D`: shifts the current line one
    /// shiftwidth, leaving the cursor on the text it was already on.
    pub fn insert_shift(&mut self, right: bool) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        let Some(row) = self.buffer.cursor_row() else {
            return false;
        };
        let cursor = self.buffer.cursor;
        // The edit lands at the start of the line rather than under the
        // cursor, so the cursor-derived record in flight cannot describe it:
        // close that record out, let the shift keep its own, and start a
        // fresh one over the cursor's new resting place.
        self.active_insert.end = cursor;
        self.push_active_insert();
        let delta = self.shift_row(row, right);
        let moved = cursor.saturating_add_signed(delta);
        let bounds = self.buffer.rows[row.min(self.buffer.rows.len().saturating_sub(1))];
        self.buffer.cursor = moved.clamp(bounds.start, bounds.end);
        self.begin_insert_record();
        delta != 0
    }

    /// Vim's `Ctrl-A` and `Ctrl-X`: adds `delta` to the number at or after
    /// the cursor on the current line.
    ///
    /// Decimal and `0x` hexadecimal are read, which is vim's default
    /// `nrformats` without its binary and octal forms.  Zero padding and the
    /// case of hex digits survive the edit, and the cursor lands on the last
    /// digit of the result, both as vim leaves them.
    pub fn adjust_number(&mut self, delta: i64) -> bool {
        let Some(row_index) = self.buffer.cursor_row() else {
            return false;
        };
        let row = self.buffer.rows[row_index];
        let column = self.buffer.cursor.saturating_sub(row.start);
        let line = &self.buffer.data[row.start..row.end];
        let Some(target) = numbers_on(line)
            .into_iter()
            .find(|number| number.end > column)
        else {
            return false;
        };
        let Some(text) = target.rewritten(line, delta) else {
            return false;
        };
        let start = row.start.saturating_add(target.start);
        let end = row.start.saturating_add(target.end);
        let Some(original) = self.buffer.replace_region(start, end, &text) else {
            return false;
        };
        let new_end = start.saturating_add(text.len());
        self.history
            .push_undo(UndoRecord::replace_region(start, new_end, original));
        self.buffer.cursor = new_end.saturating_sub(1);
        true
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

/// One number found on a line, and how it was written.
///
/// The spelling is kept alongside the range because `Ctrl-A` has to give the
/// answer back in the same notation it read: `007` counts up to `008`, and
/// `0x00FF` to `0x0100`.
struct Number {
    /// Byte offset into the line of the first byte of the number, including
    /// a minus sign or an `0x` prefix.
    start: usize,
    end: usize,
    radix: u32,
    negative: bool,
    /// How many digits were written, without any sign or prefix, so the
    /// answer can be padded back to the same width.
    digits: usize,
    /// Whether the hex digits were written in upper case.
    uppercase: bool,
}

impl Number {
    /// The bytes this number becomes once `delta` is added to it, or `None`
    /// when it does not fit in an `i64` and there is no sane answer to give.
    fn rewritten(&self, line: &[u8], delta: i64) -> Option<Vec<u8>> {
        let text = line.get(self.start..self.end)?;
        let body = if self.radix == 16 {
            text.get(2..)?
        } else {
            text.get(usize::from(self.negative)..)?
        };
        let magnitude = i64::from_str_radix(&String::from_utf8_lossy(body), self.radix).ok()?;
        let value = if self.negative {
            magnitude.checked_neg()?
        } else {
            magnitude
        };
        let updated = value.checked_add(delta)?;
        let width = self.digits;
        if self.radix == 16 {
            // Hex has no sign to carry, so counting below zero wraps the way
            // vim's does rather than growing a minus sign the notation has no
            // room for.
            let magnitude = updated as u64;
            let body = if self.uppercase {
                format!("{magnitude:0width$X}")
            } else {
                format!("{magnitude:0width$x}")
            };
            return Some(format!("0x{body}").into_bytes());
        }
        let body = format!("{:0width$}", updated.unsigned_abs());
        Some(if updated < 0 {
            format!("-{body}").into_bytes()
        } else {
            body.into_bytes()
        })
    }
}

/// Every number on one line, in the order they appear.
///
/// Hexadecimal is claimed first so the letters in `0xff` are not mistaken for
/// the end of a decimal run, and the decimal pass then steps over whatever a
/// hex literal already owns.
fn numbers_on(line: &[u8]) -> Vec<Number> {
    let mut found: Vec<Number> = Vec::new();
    let mut index = 0usize;
    while index.saturating_add(2) < line.len() {
        if line[index] != b'0'
            || !matches!(line[index + 1], b'x' | b'X')
            || !line[index + 2].is_ascii_hexdigit()
        {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = index + 2;
        while end < line.len() && line[end].is_ascii_hexdigit() {
            end += 1;
        }
        found.push(Number {
            start,
            end,
            radix: 16,
            negative: false,
            digits: end.saturating_sub(start).saturating_sub(2),
            uppercase: line[start + 2..end].iter().any(u8::is_ascii_uppercase),
        });
        index = end;
    }

    let hex = found.len();
    let mut index = 0usize;
    while index < line.len() {
        if let Some(claimed) = found[..hex]
            .iter()
            .find(|number| (number.start..number.end).contains(&index))
        {
            index = claimed.end;
            continue;
        }
        if !line[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < line.len() && line[index].is_ascii_digit() {
            index += 1;
        }
        // A minus sign in front belongs to the number: vim counts `-1` up to
        // `0` rather than to `-2`.
        let negative = start > 0 && line[start - 1] == b'-';
        found.push(Number {
            start: start.saturating_sub(usize::from(negative)),
            end: index,
            radix: 10,
            negative,
            digits: index.saturating_sub(start),
            uppercase: false,
        });
    }
    found.sort_by_key(|number| number.start);
    found
}

/// The nesting depth at `end`, ignoring brackets inside string literals.
pub fn brace_depth(data: &[u8], end: usize) -> usize {
    let mut nesting = Nesting::default();
    data.iter().take(end).for_each(|&byte| nesting.feed(byte));
    nesting.depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_a_counts_the_number_under_or_after_the_cursor() {
        let mut editor = Editor::new(b"value = 41;".to_vec());
        editor.buffer.cursor = 0;
        assert!(editor.adjust_number(1));
        assert_eq!(editor.buffer.data, b"value = 42;");
        // Vim leaves the cursor on the last digit of the answer.
        assert_eq!(editor.buffer.cursor, 9);

        assert!(editor.adjust_number(-10));
        assert_eq!(editor.buffer.data, b"value = 32;");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn control_a_keeps_zero_padding_negatives_and_hex_notation() {
        let mut padded = Editor::new(b"007".to_vec());
        assert!(padded.adjust_number(1));
        assert_eq!(padded.buffer.data, b"008");

        let mut negative = Editor::new(b"-1".to_vec());
        assert!(negative.adjust_number(1));
        assert_eq!(negative.buffer.data, b"0");

        let mut widening = Editor::new(b"99".to_vec());
        assert!(widening.adjust_number(1));
        assert_eq!(widening.buffer.data, b"100");

        let mut hex = Editor::new(b"0x00FF".to_vec());
        assert!(hex.adjust_number(1));
        assert_eq!(hex.buffer.data, b"0x0100");

        // The cursor sitting on a letter of a hex literal is still inside
        // that number rather than in front of the next one.
        let mut inside = Editor::new(b"0xff and 7".to_vec());
        inside.buffer.cursor = 3;
        assert!(inside.adjust_number(1));
        assert_eq!(inside.buffer.data, b"0x100 and 7");
    }

    #[test]
    fn control_a_undoes_in_one_step_and_reports_a_line_without_a_number() {
        let mut editor = Editor::new(b"x = 9".to_vec());
        assert!(editor.adjust_number(1));
        assert_eq!(editor.buffer.data, b"x = 10");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"x = 9");

        let mut wordy = Editor::new(b"no digits here".to_vec());
        assert!(!wordy.adjust_number(1));
        assert_eq!(wordy.buffer.data, b"no digits here");
    }

    #[test]
    fn a_blockwise_selection_cuts_a_rectangle_and_undoes_in_one_step() {
        let mut editor = Editor::new(b"abcd\nefgh\nijkl".to_vec());
        editor.buffer.cursor = 1;
        editor.start_visual(VisualKind::Blockwise);
        // Down two rows and right one, which is columns 1..=2 of all three.
        editor.visual_key(b'j');
        editor.visual_key(b'j');
        editor.visual_key(b'l');
        assert_eq!(editor.block_bounds(), Some((0, 2, 1, 2)));

        editor.visual_key(b'd');
        assert_eq!(editor.buffer.data, b"ad\neh\nil");
        assert_eq!(editor.clipboard, b"bc\nfg\njk");
        assert!(editor.buffer.invariants_hold());

        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"abcd\nefgh\nijkl");
    }

    #[test]
    fn a_blockwise_selection_yanks_short_rows_as_the_blanks_they_are() {
        let mut editor = Editor::new(b"abcd\nef\nijkl".to_vec());
        editor.buffer.cursor = 2;
        editor.start_visual(VisualKind::Blockwise);
        editor.visual_key(b'j');
        editor.visual_key(b'j');
        editor.visual_key(b'y');
        assert_eq!(editor.clipboard, b"c\n\nk");
        assert_eq!(editor.buffer.data, b"abcd\nef\nijkl");
        assert_eq!(editor.mode, Mode::Normal);
    }

    #[test]
    fn insert_control_w_and_control_u_take_back_a_word_and_a_line() {
        let mut editor = Editor::new(b"    one two".to_vec());
        editor.buffer.cursor = 11;
        editor.enter_insert(InsertEntry::Cursor);
        assert!(editor.insert_delete_word());
        assert_eq!(editor.buffer.data, b"    one ");

        // The indent survives while the cursor is still past it.
        assert!(editor.insert_delete_to_line_start());
        assert_eq!(editor.buffer.data, b"    ");
        // Sitting in the indent now, so a second press takes that too.
        assert!(editor.insert_delete_to_line_start());
        assert_eq!(editor.buffer.data, b"");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn insert_control_t_and_control_d_shift_the_line_under_the_cursor() {
        let mut editor = Editor::new(b"value".to_vec());
        editor.indent = 2;
        editor.buffer.cursor = 3;
        editor.enter_insert(InsertEntry::Cursor);

        assert!(editor.insert_shift(true));
        assert_eq!(editor.buffer.data, b"  value");
        // The cursor rode along with the text it was sitting in.
        assert_eq!(editor.buffer.cursor, 5);

        assert!(editor.insert_shift(false));
        assert_eq!(editor.buffer.data, b"value");
        assert_eq!(editor.buffer.cursor, 3);
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn unindenting_an_empty_row_leaves_its_newline_alone() {
        let mut editor = Editor::new(b"  a\n\n  b".to_vec());
        editor.indent = 2;
        editor.buffer.cursor = 0;
        editor.start_visual(VisualKind::Linewise);
        editor.visual_key(b'j');
        editor.visual_key(b'j');
        editor.visual_key(b'<');
        assert_eq!(editor.buffer.data, b"a\n\nb");
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn completions_are_offered_forwards_from_the_cursor_and_never_the_word_itself() {
        let buffer = Buffer::new(b"value verify\nva".to_vec());
        // The cursor sits just past the `va` on the second line.
        assert_eq!(buffer.keyword_before(15), b"va");
        assert_eq!(buffer.completions(b"va", 15), vec![b"value".to_vec()]);
        assert_eq!(
            buffer.completions(b"v", 15),
            vec![b"value".to_vec(), b"verify".to_vec()]
        );
    }

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
    fn unindent_leaves_blank_rows_and_their_terminators_alone() {
        // A blank row's first byte is its own newline, so an unindent that
        // treats every ASCII whitespace byte as indentation joins the row to
        // the next one -- and shrinks `rows` while the loop is still indexing
        // it.
        let mut single = Editor::new(b"a\n\nb".to_vec());
        single.buffer.cursor = 2;
        single.normal_key(b'V');
        single.visual_key(b'<');
        assert_eq!(single.buffer.data, b"a\n\nb");
        assert!(single.buffer.invariants_hold());

        let mut multiple = Editor::new(b"\ta\n\n\tb\n".to_vec());
        multiple.normal_key(b'V');
        multiple.visual_key(b'j');
        multiple.visual_key(b'j');
        multiple.visual_key(b'j');
        multiple.visual_key(b'<');
        assert_eq!(multiple.buffer.data, b"a\n\nb\n");
        assert!(multiple.buffer.invariants_hold());
    }

    #[test]
    fn unindent_keeps_crlf_line_endings_intact() {
        // With a multi-space indent the inner loop gets several passes at the
        // same row, so a CRLF blank row can lose the CR on one pass and the LF
        // on the next.
        let mut editor = Editor::new(b"    a\r\n\r\n    b\r\n".to_vec());
        editor.indent = 4;
        editor.normal_key(b'V');
        editor.visual_key(b'j');
        editor.visual_key(b'j');
        editor.visual_key(b'j');
        editor.visual_key(b'<');
        assert_eq!(editor.buffer.data, b"a\r\n\r\nb\r\n");
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

    fn keys(text: &[u8], cursor: usize, keys: &[u8]) -> Editor {
        let mut editor = Editor::new(text.to_vec());
        editor.buffer.cursor = cursor;
        for key in keys {
            editor.normal_key(*key);
        }
        assert!(editor.buffer.invariants_hold());
        editor
    }

    #[test]
    fn change_over_a_motion_deletes_it_and_opens_insert_mode() {
        let editor = keys(b"one two", 0, b"cw");
        // `cw` on a non-blank is `ce`: the blank before `two` survives, so
        // what gets typed does not run into the next word.
        assert_eq!(editor.buffer.data, b" two");
        assert_eq!(editor.mode, Mode::Insert);
        assert_eq!(editor.buffer.cursor, 0);

        assert_eq!(keys(b"abcd", 2, b"c$").buffer.data, b"ab");
        assert_eq!(keys(b"abcd", 2, b"c0").buffer.data, b"cd");
    }

    #[test]
    fn cw_on_a_blank_still_takes_the_blanks() {
        // The `ce` rule is only for a cursor on a word; on a blank `cw`
        // behaves like `dw` and stops at the word that follows.
        assert_eq!(keys(b"a  b", 1, b"cw").buffer.data, b"ab");
    }

    #[test]
    fn cc_empties_the_row_without_removing_it() {
        let editor = keys(b"one\ntwo\nthree", 5, b"cc");
        assert_eq!(editor.buffer.data, b"one\n\nthree");
        assert_eq!(editor.mode, Mode::Insert);
        assert_eq!(editor.buffer.rows.len(), 3);
    }

    #[test]
    fn dd_still_removes_the_row_that_cc_only_empties() {
        assert_eq!(
            keys(b"one\ntwo\nthree", 5, b"dd").buffer.data,
            b"one\nthree"
        );
    }

    #[test]
    fn operators_take_text_objects() {
        assert_eq!(
            keys(b"say (a, b) now", 6, b"di(").buffer.data,
            b"say () now"
        );
        assert_eq!(keys(b"say (a, b) now", 6, b"da(").buffer.data, b"say  now");

        let changed = keys(b"say (a, b) now", 6, b"ci(");
        assert_eq!(changed.buffer.data, b"say () now");
        assert_eq!(changed.mode, Mode::Insert);
        assert_eq!(changed.buffer.cursor, 5);

        assert_eq!(keys(b"the fox runs", 4, b"diw").buffer.data, b"the  runs");
        assert_eq!(keys(b"the fox runs", 4, b"daw").buffer.data, b"the runs");
    }

    #[test]
    fn a_yank_object_copies_the_range_exactly_and_changes_nothing() {
        let editor = keys(b"the fox runs", 4, b"yiw");
        assert_eq!(editor.buffer.data, b"the fox runs");
        // No spare byte: the object's range already ends where it should.
        assert_eq!(editor.clipboard, b"fox");
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.buffer.cursor, 4);
    }

    #[test]
    fn yank_takes_a_motion_now_that_every_operator_shares_one_path() {
        // `yy` was the only yank before; `y` reaching a motion falls out of
        // the operators being one implementation rather than three.
        let editor = keys(b"one two", 0, b"yw");
        assert_eq!(editor.buffer.data, b"one two");
        assert_eq!(editor.clipboard, b"one ");
        assert_eq!(editor.buffer.cursor, 0);

        assert_eq!(keys(b"abcd", 2, b"y$").clipboard, b"cd");
    }

    #[test]
    fn an_object_that_is_not_there_leaves_the_buffer_alone() {
        let editor = keys(b"no parens", 3, b"di(");
        assert_eq!(editor.buffer.data, b"no parens");
        assert_eq!(editor.mode, Mode::Normal);
        assert!(editor.history.undo.is_empty());
    }

    #[test]
    fn the_i_of_an_operator_never_opens_insert_mode() {
        // `di` waits for the object rather than falling through to `i`, which
        // is what it used to do.
        let mut editor = Editor::new(b"text".to_vec());
        assert!(editor.normal_key(b'd'));
        assert!(editor.normal_key(b'i'));
        assert_eq!(editor.mode, Mode::Normal);
        assert!(editor.pending_operator());

        // And an unknown object spends the operator without editing.
        assert!(editor.normal_key(b'z'));
        assert_eq!(editor.buffer.data, b"text");
        assert_eq!(editor.mode, Mode::Normal);
        assert!(!editor.pending_operator());
    }

    #[test]
    fn cancelling_clears_a_half_typed_object() {
        let mut editor = Editor::new(b"a(b)c".to_vec());
        editor.normal_key(b'd');
        editor.normal_key(b'i');
        editor.cancel_pending();
        assert!(!editor.pending_operator());
        // The `(` is now an ordinary unbound key, not an object.
        editor.normal_key(b'(');
        assert_eq!(editor.buffer.data, b"a(b)c");
    }

    #[test]
    fn a_change_is_one_undo_step_that_restores_what_it_took() {
        let mut editor = keys(b"the fox runs", 4, b"ciw");
        for byte in b"cat" {
            editor.insert_byte(*byte);
        }
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"the cat runs");

        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"the  runs");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"the fox runs");
        assert!(editor.buffer.invariants_hold());
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
