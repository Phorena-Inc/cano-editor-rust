use crate::buffer::{Buffer, Nesting, is_keyword};
use crate::display::{self, DEFAULT_TABSTOP};
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
    /// Vim's `tabstop`: the columns a tab is drawn across.  The editor needs
    /// it as well as the renderer, because `j` and `k` keep the display
    /// column rather than the byte offset.
    pub tabstop: usize,
    /// Set by the `i`/`a` of an operator-pending `diw`, and spent by the key
    /// that names the object.  It is separate from `leader` because the two
    /// are independent: every operator takes every object.
    object: Option<Scope>,
    /// The count typed in front of a pending operator, which multiplies the
    /// count of its motion: `2d3w` deletes six words.  One when none was.
    operator_count: usize,
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
            tabstop: DEFAULT_TABSTOP,
            object: None,
            operator_count: 1,
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
        self.operator_count = 1;
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
            // After the whole character under the cursor, not after its
            // first byte.
            InsertEntry::AfterCursor => self.buffer.move_right(),
            InsertEntry::LineEnd => self.buffer.move_line_end(),
        }
        self.mode = Mode::Insert;
        self.begin_insert_record();
    }

    /// Pushes the active insertion record when it covers at least one byte.
    /// Empty records would consume an undo press without changing anything.
    fn push_active_insert(&mut self) {
        // Pre: the record covers what was typed, forwards.  An inverted one
        // means the cursor left the insertion without closing it.
        debug_assert!(
            self.active_insert.start <= self.active_insert.end,
            "{:?}",
            self.active_insert
        );
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
        // Whatever step the insertion was part of (the deletion of a `c`,
        // the line an `o` opened) ends with it.
        self.history.end_group();
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

    /// Insert-mode Backspace and Ctrl-H: takes back one code point, all of
    /// its bytes at once (see [`Buffer::previous_code_point`]).
    pub fn insert_backspace(&mut self) -> bool {
        if self.mode != Mode::Insert || self.buffer.cursor == 0 {
            return false;
        }
        let end = self.buffer.cursor;
        let start = self.buffer.previous_code_point();
        if end > self.active_insert.start {
            // Removing bytes inserted by the active record: the record's
            // cursor-derived endpoint shrinks with the deletion.  A sequence
            // that began before the record (a lead byte that was already
            // there, completed by typing) loses only the typed part, so the
            // record never reaches back over text it did not insert.
            let start = start.max(self.active_insert.start);
            return self
                .buffer
                .delete_selection(start, end)
                .is_some_and(|deleted| !deleted.is_empty());
        }
        // Deleting text that predates the active record.  It needs its own
        // record, otherwise the pending record inverts (start > end) and every
        // older record applies at stale offsets.
        let Some(deleted) = self.buffer.delete_selection(start, end) else {
            return false;
        };
        if deleted.is_empty() {
            return false;
        }
        self.history
            .push_undo(UndoRecord::insert_chars(start, deleted));
        self.begin_insert_record();
        true
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
            self.history
                .push_undo(UndoRecord::delete_multiple(target, self.buffer.cursor));
            self.buffer.cursor = target;
            self.begin_insert_record();
        }
        self.active_insert.end = self.buffer.cursor;
        true
    }

    /// Puts the cursor at `at`, closing any insertion in flight first.
    ///
    /// An Insert-mode record derives its end from the cursor, so a jump that
    /// is not itself typing has to close the record out and start a fresh one
    /// at the new position.  Without that, the record would claim every byte
    /// between the two positions, and undo would delete text that was there
    /// before the insertion started.
    pub fn place_cursor(&mut self, at: usize) {
        if self.mode != Mode::Insert {
            self.buffer.cursor = at;
            return;
        }
        self.active_insert.end = self.buffer.cursor;
        self.push_active_insert();
        // Vim starts a new undo step when the cursor moves in Insert mode, so
        // a change is one step only up to the first move.
        self.history.end_group();
        self.buffer.cursor = at;
        self.begin_insert_record();
    }

    pub fn insert_move(&mut self, direction: MoveDirection) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        self.active_insert.end = self.buffer.cursor;
        self.push_active_insert();
        self.history.end_group();
        match direction {
            MoveDirection::Left => self.buffer.move_left(),
            MoveDirection::Right => self.buffer.move_right(),
            MoveDirection::Up => self.buffer.move_up(self.tabstop),
            MoveDirection::Down => self.buffer.move_down(self.tabstop),
        }
        self.begin_insert_record();
        true
    }

    /// `o` and `O`.  The opened line and what is typed on it are one undo
    /// step, the way vim records them.
    pub fn open_line(&mut self, below: bool) {
        let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
        self.history.begin_group();
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
            self.history.push_undo(UndoRecord::delete_multiple(
                row.start,
                self.buffer.cursor.saturating_add(1),
            ));
        }
        self.begin_insert_record();
    }

    pub fn open_line_normal(&mut self) {
        self.open_line(true);
        self.history.end_group();
        self.mode = Mode::Normal;
        self.active_insert = UndoRecord::default();
    }

    /// Vim's `r{char}` without a count: the whole character under the
    /// cursor, marks and all, becomes `text` -- one character's bytes, or a
    /// newline.
    pub fn replace_next(&mut self, text: &[u8]) -> bool {
        let start = self.buffer.cursor;
        if start >= self.buffer.data.len() || text.is_empty() {
            return false;
        }
        let end = display::char_end(&self.buffer.data, start);
        // A byte for a byte keeps the compact record the legacy `r` always
        // left; anything wider rewrites the character as a region.
        if let ([byte], 1) = (text, end - start) {
            let displaced = std::mem::replace(&mut self.buffer.data[start], *byte);
            // Either the displaced or the written byte may be a newline, so
            // the row table must be rederived to keep the buffer invariant.
            self.buffer.calculate_rows();
            self.history
                .push_undo(UndoRecord::replace_char(start, displaced));
            return true;
        }
        let Some(original) = self.buffer.replace_region(start, end, text) else {
            return false;
        };
        self.history.push_undo(UndoRecord::replace_region(
            start,
            start + text.len(),
            original,
        ));
        self.buffer.cursor = start;
        true
    }

    /// Vim's `{count}r{char}`: replaces `count` characters from the cursor
    /// with `text`, one character's bytes.
    ///
    /// A count that reaches past the end of the line fails without changing
    /// anything, as it does in vim, and a counted `r<CR>` puts a single line
    /// break in place of all of them.  An uncounted `r` keeps Cano's rule of
    /// replacing whatever character the cursor is on.  The cursor rests on
    /// the last character written, and the whole replacement is one undo
    /// step.
    pub fn replace_chars(&mut self, text: &[u8], count: usize) -> bool {
        if count <= 1 {
            return self.replace_next(text);
        }
        let start = self.buffer.cursor;
        let row = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)];
        let mut end = start;
        for _ in 0..count {
            if end >= row.end {
                return false;
            }
            end = display::char_end(&self.buffer.data, end);
        }
        let newline = text == b"\n";
        let text = if newline {
            vec![b'\n']
        } else {
            text.repeat(count)
        };
        let Some(original) = self.buffer.replace_region(start, end, &text) else {
            return false;
        };
        let new_end = start + text.len();
        self.history
            .push_undo(UndoRecord::replace_region(start, new_end, original));
        self.buffer.cursor = if newline {
            new_end
        } else {
            display::char_start(&self.buffer.data, new_end - 1)
        };
        true
    }

    pub fn normal_key(&mut self, key: u8) -> bool {
        self.normal(key, None)
    }

    /// `key` with a count typed in front of it.
    ///
    /// The count of an operator and the count of its motion multiply, so
    /// `3dw`, `d3w` and `3d` followed by `w` are all the same command; a
    /// doubled operator (`3dd`, `3yy`, `3cc`) takes that many lines.
    pub fn normal_counted(&mut self, key: u8, count: usize) -> bool {
        self.normal(key, Some(count.max(1)))
    }

    fn normal(&mut self, key: u8, count: Option<usize>) -> bool {
        if self.mode != Mode::Normal {
            return false;
        }
        // The key after an operator's `i`/`a` names the object it works on.
        // It is taken before anything else so that `di` cannot fall through to
        // `i` and open Insert mode in the middle of a delete.
        if let Some(scope) = self.object.take() {
            let leader = self.leader;
            let total = self.operator_count.saturating_mul(count.unwrap_or(1));
            self.cancel_pending();
            if !leader.is_operator() {
                return false;
            }
            if let Some((start, end)) = self.object_range(scope, key, total) {
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
            self.operator_count = count.unwrap_or(1);
            return true;
        }

        if self.leader.is_operator() && matches!(key, b'i' | b'a') {
            self.object = Some(if key == b'i' {
                Scope::Inner
            } else {
                Scope::Around
            });
            self.operator_count = self.operator_count.saturating_mul(count.unwrap_or(1));
            return true;
        }

        if self.leader.is_operator()
            && matches!(key, b'0' | b'$' | b'w' | b'b' | b'e' | b'g' | b'G')
        {
            let leader = self.leader;
            // An uncounted `g`/`G` is the whole way to the first or last
            // line, and a counted one goes to that line, so whether there
            // was a count at all matters, not just its value.
            let total = match count {
                Some(count) => Some(count.saturating_mul(self.operator_count)),
                None => (self.operator_count > 1).then_some(self.operator_count),
            };
            self.cancel_pending();
            if let Some((start, end)) = self.motion_range(leader, key, total) {
                self.apply_operator(leader, start, end);
            }
            return true;
        }

        // A doubled operator works on whole lines, as many as were counted.
        if operator.is_operator() && operator == self.leader {
            let rows = self.operator_count.saturating_mul(count.unwrap_or(1));
            self.cancel_pending();
            match operator {
                Leader::Delete => self.delete_rows(rows),
                Leader::Yank => self.yank_rows(rows),
                _ => self.change_rows(rows),
            }
            return true;
        }

        let repeat = count.unwrap_or(1);
        match key {
            b'x' => self.delete_characters(repeat),
            b'p' if !self.clipboard.is_empty() => self.paste(repeat),
            b'i' => self.enter_insert(InsertEntry::Cursor),
            b'I' => self.enter_insert(InsertEntry::FirstNonBlank),
            b'a' => self.enter_insert(InsertEntry::AfterCursor),
            b'A' => self.enter_insert(InsertEntry::LineEnd),
            b'o' => self.open_line(true),
            b'O' => self.open_line(false),
            b'v' => self.start_visual(VisualKind::Charwise),
            b'V' => self.start_visual(VisualKind::Linewise),
            _ => {
                let mut moved = false;
                for _ in 0..repeat {
                    moved |= self.motion(key);
                }
                if !moved {
                    self.cancel_pending();
                    return false;
                }
            }
        }
        self.cancel_pending();
        true
    }

    /// `x`, with vim's count: that many characters, stopping at the end of
    /// the line, as one deletion and one undo step.
    ///
    /// The clipboard gets exactly what was deleted.  A bare `x` still takes
    /// the byte under the cursor whatever it is, newline included, as Cano
    /// always has; at EOF there is nothing to take, and the clipboard is left
    /// alone rather than emptied.
    fn delete_characters(&mut self, count: usize) {
        let start = self.buffer.cursor;
        if start >= self.buffer.data.len() {
            return;
        }
        let row_end = self.buffer.rows[self.buffer.cursor_row().unwrap_or(0)].end;
        // The first character goes even when it is the newline; the rest
        // stop at the end of the line.
        let mut end = display::char_end(&self.buffer.data, start);
        for _ in 1..count.max(1) {
            if end >= row_end {
                break;
            }
            end = display::char_end(&self.buffer.data, end).min(row_end);
        }
        self.cut(start, end);
    }

    /// `p`, putting the clipboard `count` times as one insertion.
    fn paste(&mut self, count: usize) {
        let text = self.clipboard.repeat(count.max(1));
        let linewise = text.first() == Some(&b'\n');
        if linewise {
            self.buffer.move_line_end();
        }
        let start = self.buffer.cursor;
        let end = start.saturating_add(text.len());
        if self.buffer.insert_selection(start, &text) {
            self.history
                .push_undo(UndoRecord::delete_multiple(start, end));
        }
        if linewise && self.buffer.cursor < self.buffer.data.len() {
            self.buffer.cursor += 1;
        }
    }

    /// The `count` rows from the cursor's, clipped to the end of the buffer.
    fn counted_rows(&self, count: usize) -> (usize, usize) {
        let first = self.buffer.cursor_row().unwrap_or(0);
        let last = first
            .saturating_add(count.max(1) - 1)
            .min(self.buffer.rows.len() - 1);
        (first, last)
    }

    /// The clipboard form of rows `first..=last`: their text behind the `\n`
    /// that makes `p` put it on lines of its own.
    fn linewise_text(&self, first: usize, last: usize) -> Vec<u8> {
        let (start, end) = (self.buffer.rows[first].start, self.buffer.rows[last].end);
        [&b"\n"[..], &self.buffer.data[start..end]].concat()
    }

    /// The bytes deleting rows `first..=last` removes: the rows and one line
    /// break, the one after them unless they run to the end of the buffer.
    fn row_span(&self, first: usize, last: usize) -> (usize, usize) {
        let start = self.buffer.rows[first].start;
        let end = self.buffer.rows[last].end;
        if last + 1 < self.buffer.rows.len() {
            (start, end + 1)
        } else {
            (start.saturating_sub(usize::from(first > 0)), end)
        }
    }

    fn yank_rows(&mut self, count: usize) {
        let (first, last) = self.counted_rows(count);
        self.clipboard = self.linewise_text(first, last);
    }

    /// `dd`, or `{count}dd`: the rows go as one deletion and one undo step,
    /// and all of them go into the clipboard, linewise.
    pub fn delete_rows(&mut self, count: usize) {
        let (first, last) = self.counted_rows(count);
        let column = self
            .buffer
            .cursor
            .saturating_sub(self.buffer.rows[first].start);
        let text = self.linewise_text(first, last);
        let (start, end) = self.row_span(first, last);
        self.cut(start, end);
        self.clipboard = text;
        let target = self.buffer.rows[first.min(self.buffer.rows.len() - 1)];
        self.buffer.cursor = (target.start + column).min(target.end);
        // An object armed by `d i` has to go too, or the key after a counted
        // `dd` is read as the object of an operator that has already run.
        self.cancel_pending();
    }

    /// The range of a text object, extended over `count` of them for the
    /// word objects.  Other objects ignore a count.
    fn object_range(&self, scope: Scope, key: u8, count: usize) -> Option<(usize, usize)> {
        let (start, mut end) = textobject::range(&self.buffer, self.buffer.cursor, scope, key)?;
        if matches!(key, b'w' | b'W') {
            for _ in 1..count {
                match textobject::range(&self.buffer, end, scope, key) {
                    Some((_, next)) if next > end => end = next,
                    _ => break,
                }
            }
        }
        Some((start, end))
    }

    /// The byte range a motion covers when an operator is waiting on it.
    ///
    /// The cursor is moved to work the range out, exactly as the bare motion
    /// would move it `count` times; the caller's operator is what decides
    /// where it ends up.  `count` is `None` when none was typed, which only
    /// `g` and `G` tell apart from a count of one.
    fn motion_range(
        &mut self,
        leader: Leader,
        key: u8,
        count: Option<usize>,
    ) -> Option<(usize, usize)> {
        let original = self.buffer.cursor;
        let linewise_change = leader == Leader::Change;
        let repeat = count.unwrap_or(1).max(1);
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
                // `2$` runs to the end of the next line.
                for _ in 1..repeat {
                    self.buffer.move_down(self.tabstop);
                }
                self.buffer.move_line_end();
                (original, self.buffer.cursor)
            }
            b'w' => {
                for _ in 0..repeat {
                    self.buffer.move_word_next();
                }
                (original, self.buffer.cursor)
            }
            b'b' => {
                for _ in 0..repeat {
                    self.buffer.move_word_back();
                }
                (self.buffer.cursor, original)
            }
            b'e' => {
                // `e` rests on the last character of the word; deletion
                // includes all of it.
                for _ in 0..repeat {
                    self.buffer.move_word_end();
                }
                (
                    original,
                    display::char_end(&self.buffer.data, self.buffer.cursor)
                        .min(self.buffer.data.len()),
                )
            }
            b'g' | b'G' if count.is_some() => {
                // A counted `g`/`G` is linewise to that line, in whichever
                // direction it lies.
                let current = self.buffer.cursor_row().unwrap_or(0);
                let target = repeat.min(self.buffer.rows.len()) - 1;
                let (first, last) = (current.min(target), current.max(target));
                self.buffer.cursor = self.buffer.rows[first].start;
                if linewise_change {
                    (self.buffer.rows[first].start, self.buffer.rows[last].end)
                } else {
                    self.row_span(first, last)
                }
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
    /// used to be, and the deletion and everything typed before Esc are one
    /// undo step.
    fn apply_operator(&mut self, leader: Leader, start: usize, end: usize) {
        if start > end || end > self.buffer.data.len() {
            return;
        }
        match leader {
            Leader::Yank => {
                self.clipboard = self.buffer.data[start..end].to_vec();
                self.buffer.cursor = start;
            }
            Leader::Delete => self.cut(start, end),
            Leader::Change => {
                self.history.begin_group();
                self.cut(start, end);
                self.buffer.cursor = start.min(self.buffer.data.len());
                self.enter_insert(InsertEntry::Cursor);
            }
            Leader::None => {}
        }
    }

    /// Deletes `start..end` into the clipboard, which gets exactly the bytes
    /// deleted.  An empty range moves the cursor there and records nothing:
    /// an undo step that changes nothing would only waste a press of `u`.
    fn cut(&mut self, start: usize, end: usize) {
        let Some(deleted) = self.buffer.delete_selection(start, end) else {
            return;
        };
        if deleted.is_empty() {
            return;
        }
        self.history
            .push_undo(UndoRecord::insert_chars(start, deleted.clone()));
        self.clipboard = deleted;
    }

    /// `cc`, or `{count}cc`: replaces the rows' contents with one line to
    /// type on.
    ///
    /// Unlike `dd` this never takes the last newline, because the whole point
    /// is to leave a line to type on.  Vim re-indents here; Cano has no
    /// `autoindent` to consult, so the line is left empty.
    fn change_rows(&mut self, count: usize) {
        let (first, last) = self.counted_rows(count);
        let (start, end) = (self.buffer.rows[first].start, self.buffer.rows[last].end);
        self.apply_operator(Leader::Change, start, end);
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
    ///
    /// Both edges widen to whole characters, so a column that falls inside a
    /// multi-byte character takes all of it rather than splitting it.
    fn block_row_range(&self, row: usize, left: usize, right: usize) -> (usize, usize) {
        let bounds = self.buffer.rows[row];
        let data = &self.buffer.data;
        let start = display::char_start(data, bounds.start.saturating_add(left).min(bounds.end));
        let last = bounds.start.saturating_add(right);
        let end = if last < bounds.end {
            display::char_end(data, display::char_start(data, last)).min(bounds.end)
        } else {
            bounds.end
        };
        (start, end.max(start))
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
            3 | 27 => self.leave_visual(),
            // Pressing the key of the selection's own shape again leaves
            // Visual mode; the other one reshapes the selection in place.
            b'v' | b'V' => {
                let kind = if key == b'v' {
                    VisualKind::Charwise
                } else {
                    VisualKind::Linewise
                };
                if self.visual.kind == kind {
                    self.leave_visual();
                } else {
                    self.set_visual_kind(kind);
                }
            }
            b'o' => self.swap_visual_ends(),
            b'd' | b'x' if self.visual.kind.is_blockwise() => {
                self.delete_block();
                self.mode = Mode::Normal;
            }
            b'y' if self.visual.kind.is_blockwise() => {
                self.yank_block();
                self.mode = Mode::Normal;
            }
            b'd' | b'x' if self.visual.kind.is_linewise() => {
                // Whole rows go, with a line break, the way `dd` takes them.
                let (first, last) = self.selected_rows();
                let text = self.linewise_text(first, last);
                let (start, end) = self.row_span(first, last);
                self.cut(start, end);
                self.clipboard = text;
                let landing = first.min(self.buffer.rows.len() - 1);
                self.buffer.cursor = self.buffer.rows[landing].start;
                self.mode = Mode::Normal;
            }
            b'd' | b'x' => {
                let (start, end) = self.charwise_span();
                self.cut(start, end);
                self.mode = Mode::Normal;
            }
            b'y' => {
                if self.visual.kind.is_linewise() {
                    let (first, last) = self.selected_rows();
                    self.clipboard = self.linewise_text(first, last);
                    self.buffer.cursor = self.buffer.rows[first].start;
                } else {
                    let (start, end) = self.charwise_span();
                    self.clipboard = self.buffer.data[start..end].to_vec();
                    self.buffer.cursor = start;
                }
                self.mode = Mode::Normal;
            }
            b'c' => self.change_selection(),
            b'p' => self.put_over_selection(),
            b'>' | b'<' => {
                let (first, last) = self.selected_rows();
                // One undo step for the whole selection, as in vim.
                self.history.begin_group();
                for row in first..=last {
                    self.shift_row(row, key == b'>');
                }
                self.history.end_group();
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

    fn leave_visual(&mut self) {
        self.mode = Mode::Normal;
        self.visual = VisualSelection::default();
    }

    /// Reshapes the selection without moving either end of it.
    ///
    /// A linewise selection keeps its row bounds in `start`, so becoming
    /// charwise or blockwise puts the anchor back there first.
    pub fn set_visual_kind(&mut self, kind: VisualKind) {
        self.visual.kind = kind;
        if !kind.is_linewise() {
            self.visual.start = self.visual.anchor;
        }
        self.refresh_visual();
    }

    /// Vim's Visual `o`: the cursor goes to the other end of the selection,
    /// and the end it left becomes the anchor.
    fn swap_visual_ends(&mut self) {
        let anchor = self.visual.anchor;
        self.visual.anchor = self.buffer.cursor;
        if !self.visual.kind.is_linewise() {
            self.visual.start = self.buffer.cursor;
        }
        self.buffer.cursor = anchor.min(self.buffer.data.len());
    }

    /// The bytes a charwise selection covers, as a half-open range.  Both
    /// ends are inclusive in vim, so the character under the cursor is part
    /// of it, every byte of it.
    fn charwise_span(&self) -> (usize, usize) {
        let (start, end) = self.visual_bounds();
        let data = &self.buffer.data;
        let len = data.len();
        let end = if end < len {
            display::char_end(data, display::char_start(data, end))
        } else {
            len
        };
        (start.min(len), end.min(len))
    }

    /// Visual `c`: the selection goes and Insert mode opens where it was.
    ///
    /// A linewise selection leaves one empty line to type on, as `cc` does,
    /// and a block is simply deleted with the cursor at its top-left corner.
    /// The deletion and what is typed after it are one undo step.
    fn change_selection(&mut self) {
        self.history.begin_group();
        match self.visual.kind {
            VisualKind::Blockwise => self.delete_block(),
            VisualKind::Linewise => {
                let (first, last) = self.selected_rows();
                let text = self.linewise_text(first, last);
                let (start, end) = (self.buffer.rows[first].start, self.buffer.rows[last].end);
                self.cut(start, end);
                self.clipboard = text;
                self.buffer.cursor = start;
            }
            VisualKind::Charwise => {
                let (start, end) = self.charwise_span();
                self.cut(start, end);
                self.buffer.cursor = start;
            }
        }
        self.visual = VisualSelection::default();
        self.enter_insert(InsertEntry::Cursor);
    }

    /// Visual `p`: the selection is replaced by the clipboard, and what it
    /// held goes into the clipboard, which is what vim's unnamed register
    /// does.  It is one undo step.
    ///
    /// Linewise text put over a charwise selection goes on lines of its own,
    /// and charwise text put over whole lines becomes their contents.
    fn put_over_selection(&mut self) {
        let register = self.clipboard.clone();
        let register_linewise = register.first() == Some(&b'\n');
        if self.visual.kind.is_blockwise() {
            self.history.begin_group();
            self.delete_block();
            let at = self.buffer.cursor;
            if !register.is_empty() && self.buffer.insert_selection(at, &register) {
                self.history
                    .push_undo(UndoRecord::delete_multiple(at, at + register.len()));
            }
            self.history.end_group();
            self.leave_visual();
            return;
        }
        let (start, end, replaced, text) = if self.visual.kind.is_linewise() {
            let (first, last) = self.selected_rows();
            let text = if register_linewise {
                register[1..].to_vec()
            } else {
                register
            };
            let (start, end) = (self.buffer.rows[first].start, self.buffer.rows[last].end);
            (start, end, self.linewise_text(first, last), text)
        } else {
            let (start, end) = self.charwise_span();
            let text = if register_linewise {
                [&register[..], b"\n"].concat()
            } else {
                register
            };
            (start, end, self.buffer.data[start..end].to_vec(), text)
        };
        if let Some(original) = self.buffer.replace_region(start, end, &text) {
            let new_end = start + text.len();
            self.history
                .push_undo(UndoRecord::replace_region(start, new_end, original));
            self.clipboard = replaced;
            // On the first byte of a line put on its own, otherwise on the
            // last byte put, which is where vim leaves it.
            self.buffer.cursor = if self.visual.kind.is_linewise() {
                start
            } else if register_linewise {
                (start + 1).min(self.buffer.data.len())
            } else {
                new_end.saturating_sub(1).max(start)
            };
        }
        self.leave_visual();
    }

    /// Visual `r{char}`: every selected character but the line breaks
    /// becomes `replacement` -- one character's bytes -- as one undo step,
    /// and the cursor goes to the selection's start.
    pub fn visual_replace(&mut self, replacement: &[u8]) -> bool {
        if self.mode != Mode::Visual {
            return false;
        }
        let (first, last) = self.selected_rows();
        let region_start = self.buffer.rows[first].start;
        let region_end = self.buffer.rows[last].end;
        let block = self.block_bounds();
        let (span_start, span_end) = self.charwise_span();
        let data = &self.buffer.data;
        let mut text = Vec::with_capacity(region_end - region_start);
        let mut copied = region_start;
        for row in first..=last {
            let bounds = self.buffer.rows[row];
            let (start, end) = match (self.visual.kind, block) {
                (VisualKind::Blockwise, Some((_, _, left, right))) => {
                    self.block_row_range(row, left, right)
                }
                (VisualKind::Linewise, _) => (bounds.start, bounds.end),
                _ => (
                    span_start.max(bounds.start),
                    span_end.min(bounds.end).max(span_start.max(bounds.start)),
                ),
            };
            // Whatever lies between the selected spans -- line breaks, and
            // the rest of a row outside a block -- is kept as it was.
            text.extend_from_slice(&data[copied..start]);
            let mut at = start;
            while at < end {
                text.extend_from_slice(replacement);
                at = display::char_end(data, at).min(end);
            }
            copied = end;
        }
        text.extend_from_slice(&data[copied..region_end]);
        let landing = match (self.visual.kind, block) {
            (VisualKind::Blockwise, Some((_, _, left, _))) => {
                let row = self.buffer.rows[first];
                row.start.saturating_add(left).min(row.end)
            }
            (VisualKind::Linewise, _) => region_start,
            _ => span_start,
        };
        if text != self.buffer.data[region_start..region_end]
            && let Some(original) = self.buffer.replace_region(region_start, region_end, &text)
        {
            self.history.push_undo(UndoRecord::replace_region(
                region_start,
                region_start + text.len(),
                original,
            ));
        }
        self.leave_visual();
        self.buffer.cursor = landing.min(self.buffer.data.len());
        true
    }

    /// The cursor motions Normal and Visual mode share.
    fn motion(&mut self, key: u8) -> bool {
        match key {
            b'h' => self.buffer.move_left(),
            b'j' => self.buffer.move_down(self.tabstop),
            b'k' => self.buffer.move_up(self.tabstop),
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
        // Post: every motion lands on the start of a character (or the end
        // of the file), never inside a multi-byte one.  The renderer and the
        // character-wise edits all count on it.
        debug_assert!(
            display::is_char_boundary(&self.buffer.data, self.buffer.cursor),
            "cursor {} inside a character",
            self.buffer.cursor
        );
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
            self.history.push_undo(UndoRecord::delete_multiple(
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
                .push_undo(UndoRecord::insert_chars(at, vec![byte]));
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
                pieces[row - first] = deleted;
            }
        }
        self.clipboard = pieces.join(&b'\n');
        let end = self.buffer.rows[last].end;
        self.history
            .push_undo(UndoRecord::replace_region(region_start, end, original));
        let landing = self.buffer.rows[first];
        self.buffer.cursor = display::char_start(
            &self.buffer.data,
            landing.start.saturating_add(left).min(landing.end),
        );
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
        self.buffer.cursor = display::char_start(
            &self.buffer.data,
            landing.start.saturating_add(left).min(landing.end),
        );
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
    fn a_counted_delete_disarms_the_object_it_never_used() {
        let mut editor = Editor::new(b"one\ntwo\nthree\nfour\n".to_vec());
        // `d` `i` arms an object; the count that follows turns the command
        // into `3dd` instead, and the object is never used.
        assert!(editor.normal_key(b'd'));
        assert!(editor.normal_key(b'i'));
        editor.delete_rows(3);
        assert_eq!(editor.buffer.data, b"four\n");

        // So the next key is a command again, not the object of an operator
        // that has already run.
        assert!(editor.normal_key(b'x'));
        assert_eq!(editor.buffer.data, b"our\n");
    }

    #[test]
    fn x_and_charwise_visual_operators_take_exactly_the_bytes_vim_does() {
        // `x` used to put the byte after the deleted one in the clipboard too.
        let mut editor = Editor::new(b"abcd".to_vec());
        editor.buffer.cursor = 1;
        editor.normal_key(b'x');
        assert_eq!(editor.buffer.data, b"acd");
        assert_eq!(editor.clipboard, b"b");

        // And a charwise selection deleted everything but the byte under the
        // cursor, although it yanked that byte: both ends are inclusive now.
        let mut visual = Editor::new(b"abcd".to_vec());
        visual.buffer.cursor = 1;
        visual.normal_key(b'v');
        visual.visual_key(b'd');
        assert_eq!(visual.buffer.data, b"acd");
        assert_eq!(visual.clipboard, b"b");

        let mut yanked = keys(b"abcd", 1, b"v");
        yanked.visual_key(b'l');
        yanked.visual_key(b'y');
        assert_eq!(yanked.clipboard, b"bc");
        let mut deleted = keys(b"abcd", 1, b"v");
        deleted.visual_key(b'l');
        deleted.visual_key(b'x');
        assert_eq!(deleted.clipboard, b"bc");
        assert_eq!(deleted.buffer.data, b"ad");

        // A selection that runs onto EOF takes no made-up NUL with it.
        let mut tail = keys(b"ab", 1, b"v");
        tail.visual_key(b'l');
        tail.visual_key(b'y');
        assert_eq!(tail.clipboard, b"b");
    }

    #[test]
    fn normal_x_undo_and_redo_round_trip_repeatedly() {
        let mut editor = Editor::new(b"abcd".to_vec());
        editor.buffer.cursor = 1;

        assert!(editor.normal_key(b'x'));
        assert_eq!(editor.buffer.data, b"acd");
        assert_eq!(editor.clipboard, b"b");
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

        // The opened line and the text typed on it are one step.
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"old");
        assert!(editor.buffer.invariants_hold());
        assert!(editor.redo().unwrap());
        assert_eq!(editor.buffer.data, b"old\nx");
    }

    #[test]
    fn open_line_above_places_the_cursor_on_the_new_line() {
        let mut editor = Editor::new(b"old".to_vec());
        editor.normal_key(b'O');
        editor.insert_byte(b'x');
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"x\nold");

        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"old");
        assert!(editor.history.undo.is_empty());
        assert!(editor.buffer.invariants_hold());
    }

    #[test]
    fn replace_with_a_newline_keeps_row_metadata_valid() {
        let mut editor = Editor::new(b"abc".to_vec());
        editor.buffer.cursor = 1;
        assert!(editor.replace_next(b"\n"));
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

        // Every row of the selection comes back with one press.
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"a\nb");
        assert!(editor.history.undo.is_empty());
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
    fn h_l_and_x_work_on_whole_utf8_characters() {
        let text = "héllo".as_bytes();
        // `l` from `h` steps over both bytes of `é`, and `h` comes back.
        let moved = keys(text, 0, b"ll");
        assert_eq!(moved.buffer.cursor, 3);
        assert_eq!(keys(text, 3, b"h").buffer.cursor, 1);
        assert_eq!(keys(text, 3, b"$0").buffer.cursor, 0);

        // `x` takes the whole character, and the clipboard gets its bytes.
        let cut = keys(text, 1, b"x");
        assert_eq!(cut.buffer.data, b"hllo");
        assert_eq!(cut.clipboard, "é".as_bytes());
        // A count counts characters, not bytes.
        let wide = keys("日本語x".as_bytes(), 0, b"");
        let mut wide = wide;
        wide.normal_counted(b'x', 2);
        assert_eq!(wide.buffer.data, "語x".as_bytes());
        // A letter and the mark on it are one character.
        let marked = keys("e\u{301}z".as_bytes(), 0, b"lx");
        assert_eq!(marked.buffer.data, "e\u{301}".as_bytes());
    }

    #[test]
    fn j_and_k_keep_the_display_column_not_the_byte_offset() {
        let text = "日本語\nabcdef\n\tx".as_bytes();
        // `本` starts at byte 3 but in column 2, which on the next row is `c`.
        let down = keys(text, 3, b"j");
        assert_eq!(down.buffer.cursor, 12);
        // Column 2 on the last row falls inside the tab, so it is the tab.
        assert_eq!(keys(text, 12, b"j").buffer.cursor, 17);
        // Column 3 is the second half of `本`, which means `本`.
        assert_eq!(keys(text, 13, b"k").buffer.cursor, 3);
        // With a two-column tab, `x` is in column 2 and so is `c`.
        let mut narrow = Editor::new(text.to_vec());
        narrow.tabstop = 2;
        narrow.buffer.cursor = 18;
        narrow.normal_key(b'k');
        assert_eq!(narrow.buffer.cursor, 12);
    }

    #[test]
    fn word_motions_never_stop_inside_a_character() {
        let text = "héllo wörld".as_bytes();
        let mut editor = Editor::new(text.to_vec());
        for key in b"wwwweeebbbb" {
            editor.normal_key(*key);
            assert!(
                display::is_char_boundary(&editor.buffer.data, editor.buffer.cursor),
                "{} after {}",
                editor.buffer.cursor,
                char::from(*key)
            );
        }
        // `de` takes the last character of the word whole.
        let deleted = keys("aé b".as_bytes(), 0, b"de");
        assert!(std::str::from_utf8(&deleted.buffer.data).is_ok());
    }

    #[test]
    fn r_replaces_a_whole_character_with_a_whole_character() {
        let mut editor = Editor::new("日x".as_bytes().to_vec());
        assert!(editor.replace_chars(b"a", 1));
        assert_eq!(editor.buffer.data, b"ax");
        editor.undo().unwrap();
        assert_eq!(editor.buffer.data, "日x".as_bytes());

        let mut counted = Editor::new("日本x".as_bytes().to_vec());
        assert!(counted.replace_chars("é".as_bytes(), 2));
        assert_eq!(counted.buffer.data, "ééx".as_bytes());
        // On the last character written, at its first byte.
        assert_eq!(counted.buffer.cursor, 2);
        assert!(!counted.replace_chars(b"z", 4));
        counted.undo().unwrap();
        assert_eq!(counted.buffer.data, "日本x".as_bytes());

        // Visual `r` writes one replacement per selected character.
        let mut visual = Editor::new("ab日c".as_bytes().to_vec());
        visual.buffer.cursor = 1;
        visual.normal_key(b'v');
        visual.visual_key(b'l');
        assert!(visual.visual_replace("ü".as_bytes()));
        assert_eq!(visual.buffer.data, "aüüc".as_bytes());
    }

    #[test]
    fn insert_backspace_and_arrows_step_over_whole_characters() {
        // Typed a byte at a time, the way a non-ASCII key arrives.
        let mut editor = Editor::new(b"a".to_vec());
        editor.buffer.cursor = 1;
        editor.enter_insert(InsertEntry::Cursor);
        for byte in "é日".bytes() {
            editor.insert_byte(byte);
        }
        assert!(editor.insert_backspace());
        assert_eq!(editor.buffer.data, "aé".as_bytes());
        assert!(editor.insert_move(MoveDirection::Left));
        assert_eq!(editor.buffer.cursor, 1);
        assert!(editor.insert_move(MoveDirection::Right));
        assert_eq!(editor.buffer.cursor, 3);
        // Text from before the insertion goes the same way and comes back
        // with one undo.
        editor.leave_insert();
        let mut old = Editor::new("xé".as_bytes().to_vec());
        old.buffer.cursor = 3;
        old.enter_insert(InsertEntry::Cursor);
        assert!(old.insert_backspace());
        assert_eq!(old.buffer.data, b"x");
        old.leave_insert();
        old.undo().unwrap();
        assert_eq!(old.buffer.data, "xé".as_bytes());
        // `a` appends after the whole character under the cursor.
        let mut append = Editor::new("éz".as_bytes().to_vec());
        append.normal_key(b'a');
        assert_eq!(append.buffer.cursor, 2);
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

        // The deletion and the text typed in its place come back together,
        // and go forward again together.
        for _ in 0..2 {
            assert!(editor.undo().unwrap());
            assert_eq!(editor.buffer.data, b"the fox runs");
            assert!(editor.history.undo.is_empty());
            assert!(editor.redo().unwrap());
            assert_eq!(editor.buffer.data, b"the cat runs");
            assert!(editor.history.redo.is_empty());
        }
        assert!(editor.buffer.invariants_hold());
    }

    fn counted(text: &[u8], cursor: usize, keys: &[(u8, Option<usize>)]) -> Editor {
        let mut editor = Editor::new(text.to_vec());
        editor.buffer.cursor = cursor;
        for (key, count) in keys {
            match count {
                Some(count) => editor.normal_counted(*key, *count),
                None => editor.normal_key(*key),
            };
        }
        assert!(editor.buffer.invariants_hold());
        editor
    }

    #[test]
    fn every_change_is_one_undo_step_whatever_was_typed_into_it() {
        // `cc`, then a line break and an auto-closed bracket, each of which
        // pushes records of its own while the insertion is being typed.
        let mut editor = keys(b"one\ntwo\nthree", 5, b"cc");
        for byte in b"f(" {
            editor.insert_byte(*byte);
        }
        editor.insert_newline();
        editor.insert_byte(b'x');
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"one\nf(\n\tx\n)\nthree");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"one\ntwo\nthree");
        assert!(editor.redo().unwrap());
        assert_eq!(editor.buffer.data, b"one\nf(\n\tx\n)\nthree");

        // `c$` that types nothing is still one step: the deletion alone.
        let mut empty = keys(b"abcd", 2, b"c$");
        empty.leave_insert();
        assert!(empty.undo().unwrap());
        assert_eq!(empty.buffer.data, b"abcd");
        assert!(!empty.undo().unwrap());
    }

    #[test]
    fn moving_the_cursor_in_insert_mode_ends_the_step_of_a_change() {
        // Vim breaks the undo step at an arrow key, so what is typed after
        // one is its own step and the change before it is another.
        let mut editor = keys(b"one two", 0, b"cw");
        editor.insert_byte(b'1');
        editor.insert_move(MoveDirection::Right);
        editor.insert_byte(b'!');
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"1 !two");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"1 two");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"one two");
        assert!(!editor.history.grouping());
    }

    #[test]
    fn a_new_edit_after_undo_cannot_redo_the_undone_one() {
        let mut editor = keys(b"abc", 0, b"x");
        assert!(editor.undo().unwrap());
        editor.normal_key(b'l');
        editor.normal_key(b'x');
        assert!(!editor.redo().unwrap());
        assert_eq!(editor.buffer.data, b"ac");
    }

    #[test]
    fn a_replacement_redoes_after_it_is_undone() {
        let mut editor = Editor::new(b"abc".to_vec());
        assert!(editor.replace_next(b"X"));
        for _ in 0..2 {
            assert!(editor.undo().unwrap());
            assert_eq!(editor.buffer.data, b"abc");
            assert!(editor.redo().unwrap());
            assert_eq!(editor.buffer.data, b"Xbc");
        }
    }

    #[test]
    fn counted_replace_writes_that_many_bytes_or_nothing() {
        let mut editor = Editor::new(b"abcd\nef".to_vec());
        assert!(editor.replace_chars(b"X", 3));
        assert_eq!(editor.buffer.data, b"XXXd\nef");
        assert_eq!(editor.buffer.cursor, 2);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"abcd\nef");

        // Five do not fit before the end of the line, so none are written.
        editor.buffer.cursor = 0;
        assert!(!editor.replace_chars(b"X", 5));
        assert_eq!(editor.buffer.data, b"abcd\nef");

        // A counted line break replaces them all with one.
        assert!(editor.replace_chars(b"\n", 2));
        assert_eq!(editor.buffer.data, b"\ncd\nef");
        assert_eq!(editor.buffer.cursor, 1);
    }

    #[test]
    fn counts_reach_the_motion_of_an_operator_and_multiply() {
        let text = b"a b c d e f g h";
        let d3w = counted(text, 0, &[(b'd', None), (b'w', Some(3))]);
        assert_eq!(d3w.buffer.data, b"d e f g h");
        let three_dw = counted(text, 0, &[(b'd', Some(3)), (b'w', None)]);
        assert_eq!(three_dw.buffer.data, b"d e f g h");
        let six = counted(text, 0, &[(b'd', Some(2)), (b'w', Some(3))]);
        assert_eq!(six.buffer.data, b"g h");
        assert!(!six.pending_operator());

        let yank = counted(b"one two three", 0, &[(b'y', None), (b'e', Some(2))]);
        assert_eq!(yank.clipboard, b"one two");
        assert_eq!(yank.buffer.data, b"one two three");

        // `c2w` is `c2e`, leaving the blank before the third word.
        let change = counted(b"one two three", 0, &[(b'c', None), (b'w', Some(2))]);
        assert_eq!(change.buffer.data, b" three");
        assert_eq!(change.mode, Mode::Insert);
        let change = counted(b"one two three", 0, &[(b'c', Some(3)), (b'w', None)]);
        assert_eq!(change.buffer.data, b"");

        // A count on a word object takes that many objects.
        let objects = counted(
            b"one two three",
            0,
            &[(b'd', Some(2)), (b'a', None), (b'w', None)],
        );
        assert_eq!(objects.buffer.data, b"three");
    }

    #[test]
    fn a_doubled_operator_takes_the_counted_lines_and_leaves_nothing_pending() {
        let text = b"a\nb\nc\nd\ne";
        let dd = counted(text, 0, &[(b'd', Some(3)), (b'd', None)]);
        assert_eq!(dd.buffer.data, b"d\ne");
        assert_eq!(dd.clipboard, b"\na\nb\nc");
        assert!(!dd.pending_operator());
        let mut dd = dd;
        assert!(dd.undo().unwrap());
        assert_eq!(dd.buffer.data, text);
        assert!(!dd.undo().unwrap());

        // `d3d` is the same command, and a count past the end stops there.
        assert_eq!(
            counted(text, 2, &[(b'd', None), (b'd', Some(3))])
                .buffer
                .data,
            b"a\ne"
        );
        assert_eq!(
            counted(text, 6, &[(b'd', Some(9)), (b'd', None)])
                .buffer
                .data,
            b"a\nb\nc"
        );

        let yy = counted(text, 2, &[(b'y', Some(2)), (b'y', None)]);
        assert_eq!(yy.clipboard, b"\nb\nc");
        assert!(!yy.pending_operator());

        let cc = counted(text, 2, &[(b'c', Some(2)), (b'c', None)]);
        assert_eq!(cc.buffer.data, b"a\n\nd\ne");
        assert_eq!(cc.mode, Mode::Insert);
        assert_eq!(cc.buffer.cursor, 2);
    }

    #[test]
    fn counted_x_and_p_are_one_edit_each() {
        let mut editor = counted(b"abcdef\ngh", 1, &[(b'x', Some(3))]);
        assert_eq!(editor.buffer.data, b"aef\ngh");
        assert_eq!(editor.clipboard, b"bcd");
        // The count stops at the end of the line.
        editor.normal_counted(b'x', 9);
        assert_eq!(editor.buffer.data, b"a\ngh");
        assert_eq!(editor.clipboard, b"ef");

        editor.buffer.cursor = 0;
        editor.normal_counted(b'p', 3);
        assert_eq!(editor.buffer.data, b"efefefa\ngh");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"a\ngh");
    }

    #[test]
    fn an_operator_count_goes_to_a_counted_line() {
        let text = b"a\nb\nc\nd";
        assert_eq!(
            counted(text, 0, &[(b'd', None), (b'G', Some(2))])
                .buffer
                .data,
            b"c\nd"
        );
        assert_eq!(
            counted(text, 6, &[(b'd', None), (b'g', Some(3))])
                .buffer
                .data,
            b"a\nb"
        );
    }

    fn visual(text: &[u8], cursor: usize, kind: VisualKind, moves: &[u8]) -> Editor {
        let mut editor = Editor::new(text.to_vec());
        editor.buffer.cursor = cursor;
        editor.start_visual(kind);
        for key in moves {
            editor.visual_key(*key);
        }
        editor
    }

    #[test]
    fn visual_c_replaces_the_selection_with_what_is_typed_in_one_step() {
        let mut editor = visual(b"one two three", 4, VisualKind::Charwise, b"ll");
        editor.visual_key(b'c');
        assert_eq!(editor.mode, Mode::Insert);
        assert_eq!(editor.buffer.data, b"one  three");
        assert_eq!(editor.clipboard, b"two");
        editor.insert_byte(b'2');
        editor.leave_insert();
        assert_eq!(editor.buffer.data, b"one 2 three");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"one two three");

        // Whole lines leave one empty line to type on.
        let mut lines = visual(b"a\nb\nc", 0, VisualKind::Linewise, b"j");
        lines.visual_key(b'c');
        assert_eq!(lines.buffer.data, b"\nc");
        assert_eq!(lines.buffer.cursor, 0);
        assert_eq!(lines.mode, Mode::Insert);

        // A block is deleted and typing starts at its top-left corner.
        let mut block = visual(b"abcd\nefgh", 1, VisualKind::Blockwise, b"jl");
        block.visual_key(b'c');
        assert_eq!(block.buffer.data, b"ad\neh");
        assert_eq!(block.buffer.cursor, 1);
        block.insert_byte(b'X');
        block.leave_insert();
        assert!(block.undo().unwrap());
        assert_eq!(block.buffer.data, b"abcd\nefgh");
    }

    #[test]
    fn visual_p_swaps_the_selection_and_the_clipboard() {
        let mut editor = visual(b"one two", 4, VisualKind::Charwise, b"ll");
        editor.clipboard = b"2".to_vec();
        editor.visual_key(b'p');
        assert_eq!(editor.buffer.data, b"one 2");
        assert_eq!(editor.clipboard, b"two");
        assert_eq!(editor.mode, Mode::Normal);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"one two");

        // Lines put over a charwise selection go on lines of their own.
        let mut split = visual(b"abcdef", 2, VisualKind::Charwise, b"l");
        split.clipboard = b"\nrow".to_vec();
        split.visual_key(b'p');
        assert_eq!(split.buffer.data, b"ab\nrow\nef");
        assert_eq!(split.clipboard, b"cd");

        // Over whole lines, the lines are replaced and come back linewise.
        let mut lines = visual(b"a\nb\nc", 2, VisualKind::Linewise, b"");
        lines.clipboard = b"\nx\ny".to_vec();
        lines.visual_key(b'p');
        assert_eq!(lines.buffer.data, b"a\nx\ny\nc");
        assert_eq!(lines.clipboard, b"\nb");
    }

    #[test]
    fn visual_o_swaps_the_ends_and_v_and_v_switch_or_leave() {
        let mut editor = visual(b"abcdef", 1, VisualKind::Charwise, b"ll");
        editor.visual_key(b'o');
        assert_eq!(editor.buffer.cursor, 1);
        editor.visual_key(b'h');
        editor.visual_key(b'y');
        assert_eq!(editor.clipboard, b"abcd");

        let mut lines = visual(b"a\nb\nc", 0, VisualKind::Linewise, b"j");
        lines.visual_key(b'o');
        assert_eq!(lines.buffer.cursor, 0);
        assert_eq!(lines.visual_rows(), Some((0, 3)));

        // `V` in a charwise selection makes it linewise, `v` brings it
        // back, and the key of the shape already selected leaves.
        let mut shapes = visual(b"ab\ncd", 1, VisualKind::Charwise, b"j");
        shapes.visual_key(b'V');
        assert_eq!(shapes.visual.kind, VisualKind::Linewise);
        assert_eq!((shapes.visual.start, shapes.visual.end), (0, 5));
        shapes.visual_key(b'v');
        assert_eq!(shapes.visual.kind, VisualKind::Charwise);
        assert_eq!((shapes.visual.start, shapes.visual.end), (1, 4));
        shapes.visual_key(b'v');
        assert_eq!(shapes.mode, Mode::Normal);

        let mut block = visual(b"ab", 0, VisualKind::Blockwise, b"");
        block.visual_key(b'V');
        assert_eq!(block.visual.kind, VisualKind::Linewise);
        block.visual_key(b'V');
        assert_eq!(block.mode, Mode::Normal);
    }

    #[test]
    fn visual_r_overwrites_the_selection_but_not_its_line_breaks() {
        let mut editor = visual(b"abc\ndef", 1, VisualKind::Charwise, b"j");
        assert!(editor.visual_replace(b"x"));
        assert_eq!(editor.buffer.data, b"axx\nxxf");
        assert_eq!(editor.buffer.cursor, 1);
        assert_eq!(editor.mode, Mode::Normal);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.buffer.data, b"abc\ndef");
        assert!(!editor.undo().unwrap());

        let mut lines = visual(b"ab\ncd\nef", 3, VisualKind::Linewise, b"");
        lines.visual_replace(b"-");
        assert_eq!(lines.buffer.data, b"ab\n--\nef");

        let mut block = visual(b"abcd\nefgh", 1, VisualKind::Blockwise, b"jl");
        block.visual_replace(b".");
        assert_eq!(block.buffer.data, b"a..d\ne..h");
        assert_eq!(block.buffer.cursor, 1);
    }

    #[test]
    fn deleting_lines_puts_them_in_the_clipboard_linewise() {
        let mut editor = keys(b"one\ntwo\nthree", 0, b"dd");
        assert_eq!(editor.clipboard, b"\none");
        editor.normal_key(b'p');
        assert_eq!(editor.buffer.data, b"two\none\nthree");

        let mut lines = visual(b"a\nb\nc", 0, VisualKind::Linewise, b"j");
        lines.visual_key(b'd');
        assert_eq!(lines.clipboard, b"\na\nb");
        assert_eq!(lines.buffer.data, b"c");
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
