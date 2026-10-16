//! Operation-specific undo/redo inversion.
//!
//! Every edit pushes the operation that takes it back; applying that record
//! yields the operation that puts the edit back again, which goes on the
//! other stack.  The C editor this was ported from had four defects here that
//! the port used to reproduce, and no longer does: a new edit did not clear
//! redo, the stack ignored `undo_size`, the inverse of inserting several bytes
//! stopped one byte short, and redoing a replacement (`r`) failed because its
//! inverse was empty.  Single-byte deletion also kept no redo data; it does
//! now.
//!
//! Records can be joined into one step.  A change (`cw` and the text typed
//! after it) is a deletion followed by one or more insertions, and vim takes
//! the whole of it back with one `u`.

use std::fmt;

use crate::buffer::Buffer;

/// The `undo_size` Cano starts with, which is also the configuration default.
pub const DEFAULT_UNDO_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UndoKind {
    /// Insert `data` at `start`.
    InsertChars,
    /// Delete the half-open range `start..end`.
    #[default]
    DeleteMultiple,
    /// Delete one byte at `start`.
    DeleteChar,
    /// Replace the byte at `start` with `data[0]`.
    ReplaceChar,
    /// Replace the half-open range `start..end` with `data`.
    ///
    /// A substitution rewrites a whole region at once, so it has to be undone
    /// at once: built from a delete and an insert it would cost two presses
    /// of `u` to take back one command.
    ReplaceRegion,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UndoRecord {
    pub kind: UndoKind,
    pub data: Vec<u8>,
    pub start: usize,
    pub end: usize,
    /// True when the record below this one on the same stack belongs to the
    /// same step, so one `u` (or one redo) carries on through it.
    ///
    /// The flag is set by [`History`] as records are pushed and moved between
    /// the stacks; the constructors always leave it clear.
    pub joined: bool,
}

impl UndoRecord {
    pub fn new(kind: UndoKind, data: Vec<u8>, start: usize, end: usize) -> Self {
        Self {
            kind,
            data,
            start,
            end,
            joined: false,
        }
    }

    pub fn insert_chars(start: usize, data: Vec<u8>) -> Self {
        let end = start.saturating_add(data.len());
        Self::new(UndoKind::InsertChars, data, start, end)
    }

    pub fn delete_multiple(start: usize, end: usize) -> Self {
        Self::new(UndoKind::DeleteMultiple, Vec::new(), start, end)
    }

    /// Deletes the byte at `at`.  `byte` is what is expected to be there,
    /// which is what redo writes back.
    pub fn delete_char(at: usize, byte: u8) -> Self {
        Self::new(UndoKind::DeleteChar, vec![byte], at, at.saturating_add(1))
    }

    pub fn replace_region(start: usize, end: usize, data: Vec<u8>) -> Self {
        Self::new(UndoKind::ReplaceRegion, data, start, end)
    }

    pub fn replace_char(at: usize, byte: u8) -> Self {
        Self::new(UndoKind::ReplaceChar, vec![byte], at, at.saturating_add(1))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryError {
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    MissingReplacementByte,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange { start, end, len } => {
                write!(
                    formatter,
                    "invalid history range {start}..{end} for {len} bytes"
                )
            }
            Self::MissingReplacementByte => formatter.write_str("missing replacement byte"),
        }
    }
}

impl std::error::Error for HistoryError {}

/// Applies one inverse operation and returns the inverse to put on the other
/// stack.  Every inverse is exact, so a record survives any number of
/// undo/redo round trips; unsafe ranges are errors.
pub fn apply_record(buffer: &mut Buffer, record: UndoRecord) -> Result<UndoRecord, HistoryError> {
    // Every error is raised before the buffer changes, so it reports the
    // length the record was checked against.
    let (start, len) = (record.start, buffer.data.len());
    let invalid = |end| HistoryError::InvalidRange { start, end, len };
    let inverse = match record.kind {
        UndoKind::InsertChars => {
            if !buffer.insert_selection(start, &record.data) {
                return Err(invalid(start));
            }
            // Half-open, like every other range here.  The C code wrote
            // `start + count - 1`, which left the last inserted byte behind
            // on the next undo.
            UndoRecord::delete_multiple(start, start + record.data.len())
        }
        UndoKind::DeleteMultiple => {
            let deleted = buffer
                .delete_selection(start, record.end)
                .ok_or(invalid(record.end))?;
            UndoRecord::insert_chars(start, deleted)
        }
        UndoKind::DeleteChar => {
            let Some(&displaced) = buffer.data.get(start) else {
                return Err(invalid(start.saturating_add(1)));
            };
            buffer.cursor = start;
            let _ = buffer.delete_byte();
            UndoRecord::insert_chars(start, vec![displaced])
        }
        UndoKind::ReplaceChar => {
            let replacement = record
                .data
                .first()
                .copied()
                .ok_or(HistoryError::MissingReplacementByte)?;
            let slot = buffer
                .data
                .get_mut(start)
                .ok_or(invalid(start.saturating_add(1)))?;
            let displaced = std::mem::replace(slot, replacement);
            buffer.cursor = start;
            buffer.calculate_rows();
            // The C code wrote the displaced byte onto the consumed record
            // rather than onto its inverse, so redoing an `r` failed.  The
            // inverse carries it now.
            UndoRecord::replace_char(start, displaced)
        }
        UndoKind::ReplaceRegion => {
            // The inverse is the same shape with the two halves swapped, so
            // undo and redo of a substitution are one press each.
            let displaced = buffer
                .replace_region(start, record.end, &record.data)
                .ok_or(invalid(record.end))?;
            buffer.cursor = start;
            UndoRecord::replace_region(start, start + record.data.len(), displaced)
        }
    };
    // Post: replaying never leaves the buffer inconsistent, and the inverse
    // names a forward range at the same place, which is what the next replay
    // will index with.
    debug_assert_eq!(buffer.validate(), Ok(()));
    debug_assert!(
        inverse.start == start && start <= inverse.end && !inverse.joined,
        "{inverse:?}"
    );
    Ok(inverse)
}

/// Moves the newest step on `from` through the buffer and puts its inverse
/// on `to`.  A step is one record, or several joined ones; they are applied
/// newest first, so the inverses land on `to` in the opposite order and the
/// joins have to be laid the other way round: the first inverse pushed ends
/// the step there, and each one after it joins the one before.
///
/// A record that fails to apply is dropped, as the legacy editor did, so the
/// other stack only grows on success.
fn step(
    from: &mut Vec<UndoRecord>,
    to: &mut Vec<UndoRecord>,
    buffer: &mut Buffer,
) -> Result<bool, HistoryError> {
    let mut applied = false;
    while let Some(record) = from.pop() {
        let joined = record.joined;
        let mut inverse = apply_record(buffer, record)?;
        inverse.joined = applied;
        to.push(inverse);
        applied = true;
        if !joined {
            break;
        }
    }
    Ok(applied)
}

/// Whether the records being pushed are being joined into one step.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Group {
    #[default]
    Closed,
    /// A group is open; `started` once its first record is on the stack, so
    /// that record is left unjoined and starts the step.
    Open { started: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct History {
    pub undo: Vec<UndoRecord>,
    pub redo: Vec<UndoRecord>,
    /// The most steps `undo` keeps, which is the `undo_size` option; the
    /// oldest go first.  Zero keeps every step: a non-positive `undo_size`
    /// is read as "no limit" rather than "no undo", because losing history
    /// to a mistyped option is worse than keeping too much of it.
    pub limit: usize,
    /// How many records fresh edits have pushed, ever.  Unlike the stack's
    /// height this still grows once the stack is full, so a caller can tell
    /// whether a command edited anything.
    pub edits: usize,
    group: Group,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: DEFAULT_UNDO_SIZE,
            edits: 0,
            group: Group::Closed,
        }
    }
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes the record that undoes a fresh edit.
    ///
    /// A fresh edit starts a new branch of history, so whatever was undone
    /// before it can no longer be redone: vim's redo stack is cleared the
    /// same way.  Undo and redo move records between the stacks without
    /// coming through here.
    pub fn push_undo(&mut self, mut record: UndoRecord) {
        record.joined = self.group == Group::Open { started: true };
        if let Group::Open { started } = &mut self.group {
            *started = true;
        }
        self.undo.push(record);
        self.redo.clear();
        self.edits = self.edits.saturating_add(1);
        self.trim();
    }

    /// Joins every record pushed from here to [`History::end_group`] into
    /// one undo step.  Opening a group that is already open does nothing, so
    /// the step is the whole of the outermost one.
    pub fn begin_group(&mut self) {
        if self.group == Group::Closed {
            self.group = Group::Open { started: false };
        }
    }

    pub fn end_group(&mut self) {
        self.group = Group::Closed;
    }

    pub fn grouping(&self) -> bool {
        self.group != Group::Closed
    }

    /// Changes the cap and drops whatever no longer fits under it.
    pub fn set_limit(&mut self, limit: usize) {
        if self.limit != limit {
            self.limit = limit;
            self.trim();
        }
    }

    /// The number of undo steps on the stack, joined records counted once.
    pub fn steps(&self) -> usize {
        self.undo.iter().filter(|record| !record.joined).count()
    }

    /// Drops the oldest steps until the stack is within `limit`.
    ///
    /// A step is removed whole: leaving half of one behind would let `u`
    /// apply part of a change at offsets that only made sense with the rest.
    fn trim(&mut self) {
        if self.limit == 0 {
            return;
        }
        let mut excess = self.steps().saturating_sub(self.limit);
        let mut cut = 0;
        while excess > 0 && cut < self.undo.len() {
            cut += 1;
            while self.undo.get(cut).is_some_and(|record| record.joined) {
                cut += 1;
            }
            excess -= 1;
        }
        self.undo.drain(..cut);
        // Post: the stack is within its cap, and does not start halfway
        // through a step.
        debug_assert!(self.steps() <= self.limit);
        debug_assert!(self.undo.first().is_none_or(|record| !record.joined));
    }

    pub fn undo(&mut self, buffer: &mut Buffer) -> Result<bool, HistoryError> {
        step(&mut self.undo, &mut self.redo, buffer)
    }

    pub fn redo(&mut self, buffer: &mut Buffer) -> Result<bool, HistoryError> {
        step(&mut self.redo, &mut self.undo, buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_replacement_is_its_own_inverse() {
        let mut buffer = Buffer::new(b"one two three".to_vec());
        // Swap `two` for something longer, then take it back.
        let forward = UndoRecord::replace_region(4, 7, b"FOUR!".to_vec());
        let inverse = apply_record(&mut buffer, forward).unwrap();
        assert_eq!(buffer.data, b"one FOUR! three");
        assert_eq!(inverse, UndoRecord::replace_region(4, 9, b"two".to_vec()));

        let redo = apply_record(&mut buffer, inverse).unwrap();
        assert_eq!(buffer.data, b"one two three");
        assert_eq!(redo, UndoRecord::replace_region(4, 7, b"FOUR!".to_vec()));
        assert!(buffer.invariants_hold());

        // A range outside the buffer is a bounded error, not a panic.
        let bad = UndoRecord::replace_region(99, 120, b"x".to_vec());
        assert!(matches!(
            apply_record(&mut buffer, bad),
            Err(HistoryError::InvalidRange { .. })
        ));
        assert_eq!(buffer.data, b"one two three");
    }

    #[test]
    fn empty_stacks_are_noops() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc".to_vec());
        assert_eq!(history.undo(&mut buffer), Ok(false));
        assert_eq!(history.redo(&mut buffer), Ok(false));
        assert_eq!(buffer.data, b"abc");
    }

    #[test]
    fn delete_multiple_then_insert_round_trips_once() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc!".to_vec());
        history.push_undo(UndoRecord::delete_multiple(0, 3));

        assert_eq!(history.undo(&mut buffer), Ok(true));
        assert_eq!(buffer.data, b"!");
        assert_eq!(buffer.cursor, 0);
        assert_eq!(
            history.redo,
            vec![UndoRecord::new(
                UndoKind::InsertChars,
                b"abc".to_vec(),
                0,
                3
            )]
        );

        assert_eq!(history.redo(&mut buffer), Ok(true));
        assert_eq!(buffer.data, b"abc!");
        assert_eq!(buffer.cursor, 0); // selection insertion does not advance
        assert_eq!(
            history.undo.last(),
            Some(&UndoRecord::delete_multiple(0, 3))
        );
    }

    #[test]
    fn the_inverse_of_an_insertion_covers_every_inserted_byte() {
        // The C code ended the inverse at `start + count - 1`, so the second
        // undo of the same insertion left its last byte behind.
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc!".to_vec());
        history.push_undo(UndoRecord::delete_multiple(0, 3));
        for _ in 0..3 {
            history.undo(&mut buffer).unwrap();
            assert_eq!(buffer.data, b"!");
            assert_eq!(history.redo.last().unwrap().data, b"abc");
            history.redo(&mut buffer).unwrap();
            assert_eq!(buffer.data, b"abc!");
        }
        let inverse = apply_record(&mut buffer, UndoRecord::insert_chars(1, b"xy".to_vec()));
        assert_eq!(inverse, Ok(UndoRecord::delete_multiple(1, 3)));
    }

    #[test]
    fn a_one_byte_multiple_range_deletes_nothing() {
        let mut buffer = Buffer::new(b"abc".to_vec());
        let inverse = apply_record(&mut buffer, UndoRecord::delete_multiple(1, 1)).unwrap();
        assert_eq!(buffer.data, b"abc");
        assert!(inverse.data.is_empty());
        assert_eq!(buffer.cursor, 1);
    }

    #[test]
    fn single_character_delete_keeps_the_byte_for_redo() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc".to_vec());
        history.push_undo(UndoRecord::delete_char(1, b'b'));
        history.undo(&mut buffer).unwrap();
        assert_eq!(buffer.data, b"ac");
        assert_eq!(
            history.redo,
            vec![UndoRecord::insert_chars(1, b"b".to_vec())]
        );

        history.redo(&mut buffer).unwrap();
        assert_eq!(buffer.data, b"abc");
    }

    #[test]
    fn a_replacement_undoes_and_redoes_as_often_as_asked() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"new\n".to_vec());
        history.push_undo(UndoRecord::replace_char(0, b'o'));

        for _ in 0..3 {
            history.undo(&mut buffer).unwrap();
            assert_eq!(buffer.data, b"oew\n");
            assert_eq!(history.redo, vec![UndoRecord::replace_char(0, b'n')]);
            history.redo(&mut buffer).unwrap();
            assert_eq!(buffer.data, b"new\n");
            assert_eq!(history.undo, vec![UndoRecord::replace_char(0, b'o')]);
        }

        // A record with no byte to write is still a bounded error.
        let empty = UndoRecord::new(UndoKind::ReplaceChar, Vec::new(), 0, 1);
        assert_eq!(
            apply_record(&mut buffer, empty),
            Err(HistoryError::MissingReplacementByte)
        );
    }

    #[test]
    fn replacement_recalculates_rows_when_the_byte_is_a_newline() {
        let mut buffer = Buffer::new(b"abc".to_vec());
        let inverse = apply_record(&mut buffer, UndoRecord::replace_char(1, b'\n')).unwrap();
        assert_eq!(buffer.data, b"a\nc");
        assert_eq!(buffer.rows.len(), 2);
        assert_eq!(inverse, UndoRecord::replace_char(1, b'b'));
        assert!(buffer.invariants_hold());
    }

    #[test]
    fn a_fresh_edit_clears_redo_but_undo_and_redo_do_not() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc".to_vec());
        history.push_undo(UndoRecord::delete_multiple(0, 1));
        history.push_undo(UndoRecord::delete_multiple(0, 1));
        history.undo(&mut buffer).unwrap();
        history.undo(&mut buffer).unwrap();
        assert_eq!(history.redo.len(), 2);
        // Moving between the stacks is not an edit.
        history.redo(&mut buffer).unwrap();
        assert_eq!(history.redo.len(), 1);

        history.push_undo(UndoRecord::delete_multiple(0, 1));
        assert!(history.redo.is_empty());
        assert_eq!(history.redo(&mut buffer), Ok(false));
    }

    #[test]
    fn the_undo_stack_keeps_at_most_limit_steps_dropping_the_oldest() {
        let mut history = History::new();
        assert_eq!(history.limit, DEFAULT_UNDO_SIZE);
        for index in 0..128 {
            history.push_undo(UndoRecord::delete_multiple(index, index));
        }
        assert_eq!(history.undo.len(), DEFAULT_UNDO_SIZE);
        assert_eq!(history.undo[0].start, 128 - DEFAULT_UNDO_SIZE);
        assert_eq!(history.edits, 128);

        // Lowering the cap takes effect at once; zero means no cap at all.
        history.set_limit(4);
        assert_eq!(history.undo.len(), 4);
        assert_eq!(history.undo[0].start, 124);
        history.set_limit(0);
        for index in 0..100 {
            history.push_undo(UndoRecord::delete_multiple(index, index));
        }
        assert_eq!(history.undo.len(), 104);
    }

    #[test]
    fn the_cap_counts_a_joined_group_as_one_step_and_drops_it_whole() {
        let mut history = History::new();
        history.set_limit(2);
        history.begin_group();
        for index in 0..3 {
            history.push_undo(UndoRecord::delete_multiple(index, index));
        }
        history.end_group();
        assert_eq!(history.steps(), 1);
        history.push_undo(UndoRecord::delete_multiple(7, 7));
        assert_eq!(history.steps(), 2);
        assert_eq!(history.undo.len(), 4);

        // A third step pushes the whole group out, not just its oldest part.
        history.push_undo(UndoRecord::delete_multiple(8, 8));
        assert_eq!(history.undo.len(), 2);
        assert_eq!(history.undo[0].start, 7);
        assert!(!history.undo[0].joined);
    }

    #[test]
    fn a_joined_group_undoes_and_redoes_in_one_press() {
        // `cw` then typing: the deletion's inverse first, then the typed text.
        let mut buffer = Buffer::new(b"new two".to_vec());
        let mut history = History::new();
        history.begin_group();
        history.push_undo(UndoRecord::insert_chars(0, b"one".to_vec()));
        history.push_undo(UndoRecord::delete_multiple(0, 2));
        history.push_undo(UndoRecord::delete_multiple(2, 3));
        history.end_group();
        assert_eq!(history.steps(), 1);

        for _ in 0..2 {
            assert_eq!(history.undo(&mut buffer), Ok(true));
            assert_eq!(buffer.data, b"one two");
            assert!(history.undo.is_empty());
            assert_eq!(history.redo(&mut buffer), Ok(true));
            assert_eq!(buffer.data, b"new two");
            assert!(history.redo.is_empty());
        }
        // Records pushed after the group closes are steps of their own.
        history.push_undo(UndoRecord::delete_multiple(0, 1));
        assert_eq!(history.steps(), 2);
    }

    #[test]
    fn invalid_ranges_are_bounded_errors() {
        let mut buffer = Buffer::new(b"abc".to_vec());
        assert_eq!(
            apply_record(&mut buffer, UndoRecord::delete_multiple(3, 2)),
            Err(HistoryError::InvalidRange {
                start: 3,
                end: 2,
                len: 3
            })
        );
        assert_eq!(
            apply_record(&mut buffer, UndoRecord::insert_chars(4, b"x".to_vec())),
            Err(HistoryError::InvalidRange {
                start: 4,
                end: 4,
                len: 3
            })
        );
        assert_eq!(
            apply_record(&mut buffer, UndoRecord::delete_char(3, b'x')),
            Err(HistoryError::InvalidRange {
                start: 3,
                end: 4,
                len: 3
            })
        );
        assert_eq!(buffer.data, b"abc");
    }

    #[test]
    fn zero_byte_insert_is_a_bounded_noop() {
        let mut buffer = Buffer::new(b"abc".to_vec());
        let inverse = apply_record(&mut buffer, UndoRecord::insert_chars(1, Vec::new())).unwrap();
        assert_eq!(buffer.data, b"abc");
        assert_eq!(inverse, UndoRecord::delete_multiple(1, 1));
    }
}
