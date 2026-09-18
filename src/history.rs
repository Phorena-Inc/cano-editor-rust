//! Operation-specific undo/redo inversion.
//!
//! This intentionally models Cano's characterized history behavior rather
//! than a conventional snapshot stack.  In particular, a new edit does not
//! clear redo, the inverse produced by inserting multiple bytes has a legacy
//! off-by-one endpoint, single-byte deletion loses redo data, and replacement
//! produces an unusable empty inverse.

use std::fmt;

use crate::buffer::Buffer;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UndoKind {
    /// Insert `data` at `start`.
    InsertChars,
    /// Insert `data` at `start` and create an exact half-open inverse.
    ///
    /// Normal-mode character deletion uses this variant so its redo remains
    /// usable without changing the characterized `InsertChars` off-by-one.
    InsertCharsExact,
    /// Delete the half-open range `start..end`.
    #[default]
    DeleteMultiple,
    /// Delete the exact half-open range `start..end` and retain an exact
    /// insertion inverse.
    ///
    /// Paste uses this variant because `insert_selection` deliberately leaves
    /// the cursor at the insertion start, so cursor-derived endpoints cannot
    /// describe the inserted bytes.
    DeleteMultipleExact,
    /// Delete one byte at `start` without retaining redo data.
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
}

impl UndoRecord {
    pub fn new(kind: UndoKind, data: Vec<u8>, start: usize, end: usize) -> Self {
        Self {
            kind,
            data,
            start,
            end,
        }
    }

    pub fn insert_chars(start: usize, data: Vec<u8>) -> Self {
        let end = start.saturating_add(data.len());
        Self::new(UndoKind::InsertChars, data, start, end)
    }

    pub fn delete_multiple(start: usize, end: usize) -> Self {
        Self::new(UndoKind::DeleteMultiple, Vec::new(), start, end)
    }

    pub fn insert_chars_exact(start: usize, data: Vec<u8>) -> Self {
        let end = start.saturating_add(data.len());
        Self::new(UndoKind::InsertCharsExact, data, start, end)
    }

    pub fn delete_multiple_exact(start: usize, end: usize) -> Self {
        Self::new(UndoKind::DeleteMultipleExact, Vec::new(), start, end)
    }

    pub fn delete_char(at: usize) -> Self {
        Self::new(UndoKind::DeleteChar, Vec::new(), at, at.saturating_add(1))
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
/// stack.  Defined legacy quirks are preserved; unsafe ranges are errors.
pub fn apply_record(buffer: &mut Buffer, record: UndoRecord) -> Result<UndoRecord, HistoryError> {
    // Every error is raised before the buffer changes, so it reports the
    // length the record was checked against.
    let (start, len) = (record.start, buffer.data.len());
    let invalid = |end| HistoryError::InvalidRange { start, end, len };
    let inverse = match record.kind {
        UndoKind::InsertChars | UndoKind::InsertCharsExact => {
            if !buffer.insert_selection(start, &record.data) {
                return Err(invalid(start));
            }
            // Characterized compatibility defect: legacy `InsertChars` uses
            // `start + count - 1`, although deletion interprets it as
            // half-open. Exact editor actions opt into the correct endpoint.
            // Empty insertion remains a bounded no-op in either case.
            let count = record.data.len();
            if record.kind == UndoKind::InsertCharsExact {
                UndoRecord::delete_multiple_exact(start, start + count)
            } else {
                UndoRecord::delete_multiple(start, start + count.saturating_sub(1))
            }
        }
        UndoKind::DeleteMultiple | UndoKind::DeleteMultipleExact => {
            let deletion = buffer
                .delete_selection(start, record.end)
                .ok_or(invalid(record.end))?;
            if record.kind == UndoKind::DeleteMultipleExact {
                UndoRecord::insert_chars_exact(start, deletion.undo)
            } else {
                UndoRecord::insert_chars(start, deletion.undo)
            }
        }
        UndoKind::DeleteChar => {
            if start >= len {
                return Err(invalid(start.saturating_add(1)));
            }
            buffer.cursor = start;
            let _ = buffer.delete_byte();

            // The legacy handler never copied the displaced byte into the
            // generated redo record.
            UndoRecord::insert_chars(start, Vec::new())
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
            *slot = replacement;
            buffer.cursor = start;
            buffer.calculate_rows();

            // The C code writes the displaced byte onto the consumed record,
            // not onto its inverse, so the inverse is empty: it applies once
            // and the next application is reported as an error.
            UndoRecord::new(UndoKind::ReplaceChar, Vec::new(), start, record.end)
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
        inverse.start == start && start <= inverse.end,
        "{inverse:?}"
    );
    Ok(inverse)
}

/// Moves the newest record on `from` through the buffer and puts its inverse
/// on `to`.  A record that fails to apply is dropped, as the legacy editor
/// did, so the other stack only grows on success.
fn step(
    from: &mut Vec<UndoRecord>,
    to: &mut Vec<UndoRecord>,
    buffer: &mut Buffer,
) -> Result<bool, HistoryError> {
    let Some(record) = from.pop() else {
        return Ok(false);
    };
    to.push(apply_record(buffer, record)?);
    Ok(true)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct History {
    pub undo: Vec<UndoRecord>,
    pub redo: Vec<UndoRecord>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes a new undo operation.  Redo is deliberately not cleared and no
    /// configured capacity is enforced, matching the characterized editor.
    pub fn push_undo(&mut self, record: UndoRecord) {
        self.undo.push(record);
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
            Some(&UndoRecord::delete_multiple(0, 2))
        );
    }

    #[test]
    fn second_undo_preserves_the_legacy_one_byte_short_defect() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc!".to_vec());
        history.push_undo(UndoRecord::delete_multiple(0, 3));
        history.undo(&mut buffer).unwrap();
        history.redo(&mut buffer).unwrap();
        history.undo(&mut buffer).unwrap();
        assert_eq!(buffer.data, b"c!");
        assert_eq!(history.redo.last().unwrap().data, b"ab");
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
    fn single_character_delete_intentionally_loses_redo_bytes() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"abc".to_vec());
        history.push_undo(UndoRecord::delete_char(1));
        history.undo(&mut buffer).unwrap();
        assert_eq!(buffer.data, b"ac");
        assert_eq!(history.redo[0].kind, UndoKind::InsertChars);
        assert!(history.redo[0].data.is_empty());

        history.redo(&mut buffer).unwrap();
        assert_eq!(buffer.data, b"ac");
    }

    #[test]
    fn replacement_works_once_then_reports_the_unsafe_empty_inverse() {
        let mut history = History::new();
        let mut buffer = Buffer::new(b"new\n".to_vec());
        history.push_undo(UndoRecord::replace_char(0, b'o'));

        history.undo(&mut buffer).unwrap();
        assert_eq!(buffer.data, b"oew\n");
        assert_eq!(history.redo[0].data, Vec::<u8>::new());
        assert_eq!(
            history.redo(&mut buffer),
            Err(HistoryError::MissingReplacementByte)
        );
        assert_eq!(buffer.data, b"oew\n");
        assert!(history.redo.is_empty());
    }

    #[test]
    fn replacement_recalculates_rows_when_the_byte_is_a_newline() {
        let mut buffer = Buffer::new(b"abc".to_vec());
        let inverse = apply_record(&mut buffer, UndoRecord::replace_char(1, b'\n')).unwrap();
        assert_eq!(buffer.data, b"a\nc");
        assert_eq!(buffer.rows.len(), 2);
        assert!(inverse.data.is_empty());
        assert!(buffer.invariants_hold());
    }

    #[test]
    fn pushing_a_new_edit_does_not_clear_redo_or_apply_a_capacity() {
        let mut history = History::new();
        history.redo.push(UndoRecord::delete_char(0));
        for index in 0..128 {
            history.push_undo(UndoRecord::delete_multiple(index, index));
        }
        assert_eq!(history.undo.len(), 128);
        assert_eq!(history.redo.len(), 1);
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
