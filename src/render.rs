//! Ratatui rendering for Cano's byte-oriented editor state.
//!
//! The renderer deliberately keeps display coordinates separate from buffer
//! indexes.  Bytes are laid out by [`crate::display`]: a valid UTF-8
//! character occupies its display width (two cells for a wide one, its marks
//! drawn in the cell of the letter they decorate), a tab occupies `tabstop`
//! cells, and a byte that is not part of a valid character is one U+FFFD
//! cell.  Selections, matches and syntax spans continue to use byte offsets;
//! a cell takes its style from the bytes it was drawn from.

use ratatui::Frame;
use ratatui::buffer::Buffer as TuiBuffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Clear;

use crate::assistant::{Assistant, Role};
use crate::buffer::Highlight;
use crate::display::{self, Cell};
use crate::editor::{Editor, Mode};
use crate::explorer::{Entry, Explorer};
use crate::jump::Target;
use crate::listchars::ListChars;
use crate::markdown::{Face, Kind, faces};
use crate::recent::Recent;
use crate::syntax::{Rgb, SyntaxConfig, SyntaxKind, tokens};

const LINE_NUMBER_WIDTH: u16 = 5;
const STATUS_ROWS: u16 = 2;

/// Markdown display palette.  One color per heading level, then the shared
/// colors for the remaining constructs.  These stay mid-toned rather than
/// fully saturated so they read on both dark and light terminal themes.
const HEADING_COLORS: [Color; 6] = [
    Color::Rgb(88, 166, 255),
    Color::Rgb(86, 194, 129),
    Color::Rgb(214, 164, 62),
    Color::Rgb(226, 125, 65),
    Color::Rgb(224, 108, 158),
    Color::Rgb(173, 128, 245),
];
const MARKER_COLOR: Color = Color::Rgb(122, 130, 143);
/// Jump labels sit on top of the text they replace, so they need to win
/// against every syntax and markdown color underneath them.
const JUMP_FOREGROUND: Color = Color::Rgb(255, 236, 170);
const JUMP_BACKGROUND: Color = Color::Rgb(150, 26, 26);
/// `hlsearch` tints the background so the syntax color of the matched text
/// still shows through.
const HIGHLIGHT_BACKGROUND: Color = Color::Rgb(96, 82, 24);
/// `list` glyphs stand in for characters rather than being any, so they are
/// drawn subdued and take their color from neither the syntax nor the text.
const LISTCHAR_COLOR: Color = Color::Rgb(96, 104, 118);
const CODE_COLOR: Color = Color::Rgb(230, 120, 110);
const QUOTE_COLOR: Color = Color::Rgb(139, 148, 158);
const LIST_COLOR: Color = Color::Rgb(214, 164, 62);
const LINK_COLOR: Color = Color::Rgb(88, 166, 255);

/// What the last frame drew, and the origin the next one starts from.
///
/// The origin only moves when the cursor would leave it, so context above and
/// below the cursor stays visible while moving through a long file.  The rest
/// is the frame's own geometry, recorded so input can be mapped back onto the
/// pixels the reader is actually looking at: jump targets are limited to the
/// visible rows, and a mouse click means nothing without the gutter width and
/// scroll origin that were in force when it was made.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Viewport {
    /// First buffer row shown.
    pub row: usize,
    /// First display column shown, once the text is scrolled sideways.
    /// Always zero while `wrap` is on, because nothing is off to the side.
    pub column: usize,
    /// Screen rows of the first buffer row hidden above the top edge.  Only
    /// `wrap` makes it nonzero, and only when the cursor's row is too long
    /// to fit on the screen whole.
    pub skip: usize,
    /// Whether the frame wrapped long rows instead of scrolling sideways.
    pub wrap: bool,
    /// Rows of buffer text the frame had room for.
    pub rows: usize,
    /// Screen column where the text begins, past the line-number gutter.
    pub content_x: u16,
    /// Screen columns of text, not counting the gutter or the scrollbar.
    pub content_width: u16,
    /// First entry shown when a full-pane list took the frame instead.
    pub first_item: usize,
    /// Screen column of the scrollbar, when the pane was wide enough for one.
    pub scrollbar_x: Option<u16>,
    /// The assistant panel's first row and height, while it is open.
    pub panel: Option<(u16, u16)>,
    /// The status-bar AI button: its columns `x0..x1` and its row.
    pub ai_button: Option<(u16, u16, u16)>,
    /// The transcript's scrollbar: its column, first row and height.
    pub panel_track: Option<(u16, u16, u16)>,
    /// How far back the wrapped transcript can scroll before its oldest line
    /// is at the top.  Input clamps to this, so scrolling past the top does
    /// not pile up wheel notches that then have to be scrolled away.
    pub panel_scroll_max: usize,
    pub screen_height: u16,
}

impl Viewport {
    /// Whether a screen column is the scrollbar rather than text.
    pub fn on_scrollbar(&self, column: u16) -> bool {
        self.scrollbar_x == Some(column)
    }

    /// The first item that puts the scrollbar thumb at track row `row`.
    ///
    /// This is the inverse of the thumb placement in the drawing code, so the
    /// thumb lands under the pointer that dragged it.
    pub fn item_from_track(&self, row: u16, total: usize) -> usize {
        Track::new(self.rows, total).item_at(row)
    }

    /// How far back the assistant transcript should scroll for its scrollbar
    /// to be grabbed at screen row `row`.  `None` when the panel has no bar.
    pub fn panel_scroll_from_track(&self, row: u16) -> Option<usize> {
        let (_, top, height) = self.panel_track?;
        // An overflowing transcript is one track's height plus how far back
        // it scrolls; a shorter one cannot scroll, which this also says.
        let track = Track::new(
            usize::from(height),
            self.panel_scroll_max + usize::from(height),
        );
        let first = track.item_at(row.saturating_sub(top));
        Some(self.panel_scroll_max.saturating_sub(first))
    }

    /// The buffer byte under a screen position, if the text is there.
    ///
    /// A position in the gutter clamps to the start of that row rather than
    /// missing, because clicking a line number plainly means that line.  A
    /// position past the last row, or on the status line, is not text and
    /// returns nothing.
    pub fn byte_at(&self, editor: &Editor, column: u16, row: u16) -> Option<usize> {
        let screen_row = usize::from(row);
        if screen_row >= self.rows {
            return None;
        }
        // The scrollbar and anything past the text area are not text, so a
        // click there is not a cursor position.
        if column >= self.content_x.saturating_add(self.content_width) {
            return None;
        }
        let shown = *self.lines(editor).get(screen_row)?;
        let line = editor.buffer.rows[shown.row];

        // Columns are display columns: a tab is `tabstop` wide and a wide
        // character two, so the byte under one has to be found by walking
        // the row rather than by arithmetic.  Either half of a wide
        // character is that character, and past the end of the line the
        // cursor belongs at its end, which is where the newline sits.
        let mut wanted = shown
            .start
            .saturating_add(usize::from(column.saturating_sub(self.content_x)));
        // Past the text of a wrapped row's earlier screen row is still that
        // screen row, not the start of the next one.
        if !shown.last {
            wanted = wanted.min(shown.end.saturating_sub(1));
        }
        let byte = display::byte_at_column(
            &editor.buffer.data,
            line.start,
            line.end,
            wanted,
            editor.tabstop,
        );
        // Post: a click never puts the cursor inside a character.
        debug_assert!(display::is_char_boundary(&editor.buffer.data, byte));
        Some(byte)
    }

    /// The index in a full-pane list under a screen position.
    pub fn item_at(&self, total: usize, row: u16) -> Option<usize> {
        let screen_row = usize::from(row);
        if screen_row >= self.rows {
            return None;
        }
        let index = self.first_item.checked_add(screen_row)?;
        (index < total).then_some(index)
    }

    /// Scrolls by whole rows without letting the origin run off the buffer.
    ///
    /// The caller still has to bring the cursor back inside afterwards, or
    /// [`Viewport::follow`] will simply undo this on the next frame.
    pub fn scroll_by(&mut self, delta: isize, total_rows: usize) {
        let last = total_rows.saturating_sub(1);
        self.skip = 0;
        self.row = if delta < 0 {
            self.row.saturating_sub(delta.unsigned_abs())
        } else {
            self.row.saturating_add(delta as usize).min(last)
        };
    }

    /// The row range the frame showed, as buffer row indexes.
    ///
    /// While `wrap` is on a row can take several screen rows, so the range
    /// is worked out from the text.  It holds only the rows that are on
    /// screen whole, or the first row when not even that one fits: a row
    /// cut off at the bottom is not somewhere the cursor can be put without
    /// the view scrolling to show it.
    pub fn visible_rows(&self, editor: &Editor) -> (usize, usize) {
        if !self.wrap {
            return (self.row, self.row.saturating_add(self.rows));
        }
        let whole = self.lines(editor).iter().filter(|line| line.last).count();
        (self.row, self.row.saturating_add(whole.max(1)))
    }

    /// The screen rows of text this viewport shows, top to bottom.
    fn lines(&self, editor: &Editor) -> Vec<ScreenLine> {
        screen_lines(
            editor,
            self.row,
            self.skip,
            self.rows,
            self.wrap.then_some(usize::from(self.content_width)),
            self.column,
            usize::from(self.content_width),
        )
    }

    /// Moves the origin just far enough for the cursor's screen row to be
    /// on screen while `wrap` is on.
    ///
    /// This is [`Viewport::follow`] counted in screen rows: rows above the
    /// cursor stay in view as long as they fit, and the top of the view is
    /// always the start of a row unless the cursor's own row is taller than
    /// the screen.
    fn follow_wrapped(
        &mut self,
        editor: &Editor,
        cursor_row: usize,
        cursor_segment: usize,
        height: usize,
        width: usize,
    ) {
        if cursor_row < self.row {
            self.row = cursor_row;
            self.skip = 0;
        }
        if cursor_row > self.row {
            // Walk up from the cursor, adding whole rows while they fit.
            let mut used = cursor_segment.saturating_add(1);
            let mut top = cursor_row;
            while top > self.row {
                let mut above = wrap_starts(editor, top - 1, width).len();
                if top - 1 == self.row {
                    above = above.saturating_sub(self.skip);
                }
                if used.saturating_add(above) > height {
                    break;
                }
                used += above;
                top -= 1;
            }
            if top > self.row {
                self.row = top;
                self.skip = 0;
            }
        }
        if cursor_row == self.row {
            self.skip = self.skip.min(cursor_segment);
            if cursor_segment >= self.skip.saturating_add(height) {
                self.skip = cursor_segment.saturating_add(1).saturating_sub(height);
            }
        } else {
            // The text may have changed under a stale offset.
            let rows = wrap_starts(editor, self.row, width).len();
            self.skip = self.skip.min(rows.saturating_sub(1));
        }
    }

    /// Returns the new origin on one axis: unchanged while the cursor is
    /// inside the viewport, otherwise moved just far enough to contain it.
    fn follow(origin: usize, cursor: usize, extent: usize) -> usize {
        if extent == 0 {
            return cursor;
        }
        let origin = origin.min(cursor);
        let origin = if cursor >= origin.saturating_add(extent) {
            cursor.saturating_add(1).saturating_sub(extent)
        } else {
            origin
        };
        // Post: the cursor is inside the window that starts at the new origin.
        debug_assert!(origin <= cursor && cursor - origin < extent);
        origin
    }
}

/// One screen row of text: display columns `start..end` of buffer row `row`.
///
/// Without `wrap` every buffer row is one screen row, and the columns are
/// the window the view is scrolled to.  With it a long row is cut into
/// several, and the columns are the piece each one shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScreenLine {
    row: usize,
    start: usize,
    end: usize,
    /// The row's first screen row, which carries its line number.
    first: bool,
    /// The row's last screen row, which holds the end of the line.
    last: bool,
}

/// The display columns where each screen row of a wrapped row begins.
///
/// A row breaks every `width` columns, as vim's `wrap` does.  A wide
/// character that would be cut by the edge moves to the next screen row
/// whole, leaving the column it did not fit in blank; a tab is split
/// across the edge.  A row exactly as wide as the screen gets one more
/// screen row while the cursor is at its end, so the cursor has a place
/// to be drawn.
fn wrap_starts(editor: &Editor, row_index: usize, width: usize) -> Vec<usize> {
    let mut starts = vec![0];
    let Some(row) = editor.buffer.rows.get(row_index).copied() else {
        return starts;
    };
    if width == 0 {
        return starts;
    }
    let data = &editor.buffer.data;
    let mut start = 0usize;
    let mut past = 0usize;
    for cell in display::cells(data, row.start, row.end, editor.tabstop) {
        past = cell.column.saturating_add(cell.width);
        while past > start.saturating_add(width) {
            start = if Glyph::of(data, cell).wide && cell.column > start {
                cell.column
            } else {
                start.saturating_add(width)
            };
            starts.push(start);
        }
    }
    if past > 0 && past == start.saturating_add(width) && editor.buffer.cursor == row.end {
        starts.push(past);
    }
    // Post: the screen rows start in order, each past the one before.
    debug_assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
    starts
}

/// Up to `height` screen rows of text, starting `skip` screen rows into
/// buffer row `first_row`.
///
/// `wrap` is the width to wrap at, or `None` to show columns
/// `first_column..first_column + width` of every row instead.
fn screen_lines(
    editor: &Editor,
    first_row: usize,
    skip: usize,
    height: usize,
    wrap: Option<usize>,
    first_column: usize,
    width: usize,
) -> Vec<ScreenLine> {
    let mut lines = Vec::with_capacity(height);
    let mut skip = skip;
    let mut row = first_row;
    while lines.len() < height && row < editor.buffer.rows.len() {
        let Some(wrap) = wrap else {
            lines.push(ScreenLine {
                row,
                start: first_column,
                end: first_column.saturating_add(width),
                first: true,
                last: true,
            });
            row += 1;
            continue;
        };
        let starts = wrap_starts(editor, row, wrap);
        for (index, &start) in starts.iter().enumerate().skip(skip) {
            if lines.len() == height {
                break;
            }
            lines.push(ScreenLine {
                row,
                start,
                end: starts
                    .get(index + 1)
                    .copied()
                    .unwrap_or(start.saturating_add(wrap)),
                first: index == 0,
                last: index + 1 == starts.len(),
            });
        }
        skip = 0;
        row += 1;
    }
    lines
}

/// Transient application state needed to render one frame.
#[derive(Clone, Copy, Debug)]
pub struct RenderOptions<'a> {
    pub relative_numbers: bool,
    pub prompt: &'a str,
    pub prompt_cursor: usize,
    /// Pending input echoed on the prompt line: a motion count, or the `s`/`t`
    /// jump prompt.
    pub pending: &'a str,
    /// Labeled `s`/`t` jump targets to overlay, and the label prefix typed so
    /// far.  Targets whose label does not start with it are already ruled out
    /// and are not drawn.
    pub jump: Option<(&'a [Target], &'a [u8])>,
    /// The search pattern `hlsearch` is showing, empty after `:nohl`.
    pub highlight: &'a Highlight,
    /// Vim's `cursorline`: marks the row the cursor is on.
    pub cursorline: bool,
    /// Vim's `list`: the glyphs to draw for invisible characters, or `None`
    /// while `list` is off.
    pub list: Option<&'a ListChars>,
    /// Vim's `wrap`: long rows continue on the next screen row instead of
    /// scrolling the view sideways.
    pub wrap: bool,
    pub explorer: Option<&'a Explorer>,
    /// The Ctrl-P recent-file picker, which takes the pane when it is open.
    pub recent: Option<&'a Recent>,
    /// The Ctrl-F prompt-history picker, and the entry it has selected.  It
    /// takes the pane the same way the other two do.
    pub history: Option<(&'a [Vec<u8>], usize)>,
    pub syntax: Option<&'a SyntaxConfig>,
    /// Renders the buffer as formatted markdown instead of source code.
    pub markdown: bool,
    pub message: Option<&'a str>,
    pub filename: &'a str,
    pub saved: bool,
    /// The AI panel, or `None` when no API key switched the feature on.
    pub assistant: Option<&'a Assistant>,
}

#[derive(Clone, Copy, Debug)]
struct ScreenLayout {
    area: Rect,
    editor_height: u16,
    gutter_digit_width: u16,
    content_x: u16,
    content_width: u16,
    scrollbar_x: Option<u16>,
    status_y: u16,
    prompt_y: Option<u16>,
}

impl ScreenLayout {
    fn new(area: Rect, line_count: usize, panel_rows: u16) -> Self {
        let editor_height = area.height.saturating_sub(STATUS_ROWS + panel_rows);
        let line_digits = u16::try_from(line_count.max(1).to_string().len()).unwrap_or(u16::MAX);
        let desired_digit_width = LINE_NUMBER_WIDTH.saturating_sub(1).max(line_digits);
        let gutter_width = area.width.min(desired_digit_width.saturating_add(1));
        // On a pane narrower than the desired gutter, use every available
        // column for digits instead of reserving a separator and truncating
        // one additional leading digit.
        let gutter_digit_width = gutter_width.min(desired_digit_width);
        let editor_width = area.width.saturating_sub(gutter_width);
        let scrollbar_x = (editor_height > 0 && editor_width > 0)
            .then(|| area.x.saturating_add(area.width.saturating_sub(1)));
        let content_width = editor_width.saturating_sub(u16::from(scrollbar_x.is_some()));
        // Invariant: gutter, text and scrollbar partition the width exactly.
        debug_assert_eq!(
            gutter_width + content_width + u16::from(scrollbar_x.is_some()),
            area.width
        );

        Self {
            area,
            editor_height,
            gutter_digit_width,
            content_x: area.x.saturating_add(gutter_width),
            content_width,
            scrollbar_x,
            status_y: area.y.saturating_add(editor_height + panel_rows),
            prompt_y: (area.height >= STATUS_ROWS)
                .then(|| area.y.saturating_add(area.height.saturating_sub(1))),
        }
    }
}

/// Draw one complete Cano frame, updating the persistent scroll origin.
pub fn draw(
    frame: &mut Frame<'_>,
    editor: &Editor,
    options: RenderOptions<'_>,
    scroll: &mut Viewport,
) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    // A full-pane list has no buffer lines to number, so the gutter collapses
    // to its minimum width behind it.
    let line_count =
        if options.explorer.is_none() && options.recent.is_none() && options.history.is_none() {
            editor.buffer.rows.len()
        } else {
            0
        };
    let panel_rows = options.assistant.map_or(0, |a| a.rows(area.height));
    let layout = ScreenLayout::new(area, line_count, panel_rows);
    scroll.screen_height = area.height;
    scroll.panel = (panel_rows > 0).then(|| (area.y + layout.editor_height, panel_rows));
    scroll.content_x = layout.content_x;
    scroll.content_width = layout.content_width;
    scroll.scrollbar_x = layout.scrollbar_x;
    scroll.rows = usize::from(layout.editor_height);

    let mut first_item = 0;
    let panel_focused;
    frame.render_widget(Clear, area);
    let cursor = {
        let buffer = frame.buffer_mut();

        let cursor = if let Some((entries, selected)) = options.history {
            let width = usize::from(layout.content_width);
            first_item = viewport_start(selected, layout.editor_height, entries.len());
            draw_list(
                buffer,
                layout,
                entries,
                first_item,
                selected,
                Style::default().fg(Color::Yellow),
                |entry| elide_start(&String::from_utf8_lossy(entry), width),
            )
        } else if let Some(recent) = options.recent {
            let width = usize::from(layout.content_width);
            first_item = viewport_start(recent.cursor, layout.editor_height, recent.paths.len());
            draw_list(
                buffer,
                layout,
                &recent.paths,
                first_item,
                recent.cursor,
                Style::default().fg(Color::Cyan),
                // The whole path, because two recent files often share a
                // base name.
                |path| elide_start(&path.to_string_lossy(), width),
            )
        } else if let Some(explorer) = options.explorer {
            first_item = viewport_start(
                explorer.cursor,
                layout.editor_height,
                explorer.entries.len(),
            );
            draw_list(
                buffer,
                layout,
                &explorer.entries,
                first_item,
                explorer.cursor,
                Style::default().fg(Color::Blue),
                Entry::display_name,
            )
        } else {
            draw_editor(buffer, layout, editor, &options, scroll)
        };

        scroll.ai_button = draw_status(buffer, layout, editor, &options);
        let panel = match (options.assistant, scroll.panel) {
            (Some(assistant), Some((top, rows))) => draw_assistant(
                buffer,
                Rect::new(area.x, top, area.width, rows),
                assistant,
                scroll,
            ),
            _ => None,
        };
        let prompt = draw_prompt(buffer, layout, editor, &options);
        // Focus decides whose cursor shows: the panel's input line while it
        // has the keyboard, the editor's otherwise.
        panel_focused = panel.is_some() && options.assistant.is_some_and(|a| a.focused);
        if panel_focused {
            panel
        } else if options.history.is_some() {
            // The list shows which entry is selected by putting the terminal
            // cursor on it, so while the history picker is up it outranks the
            // prompt it was opened from; everywhere else the prompt wins.
            cursor.or(prompt)
        } else {
            prompt.or(cursor)
        }
    };

    scroll.first_item = first_item;
    // Post: whichever pane placed it, the cursor is inside the frame.
    debug_assert!(cursor.is_none_or(|(x, y)| area.contains(Position::new(x, y))));

    if (panel_focused || editor.mode != Mode::Visual)
        && let Some(position) = cursor
    {
        frame.set_cursor_position(position);
    }
}

fn draw_editor(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    options: &RenderOptions<'_>,
    scroll: &mut Viewport,
) -> Option<(u16, u16)> {
    if layout.editor_height == 0 {
        return None;
    }

    let cursor_row = editor.buffer.cursor_row().unwrap_or(0);
    let cursor_column = cursor_display_column(editor);
    let height = usize::from(layout.editor_height);
    let content_width = usize::from(layout.content_width);
    scroll.wrap = options.wrap;
    if options.wrap {
        // Nothing is off to the side, so the view never scrolls sideways.
        scroll.column = 0;
        let starts = wrap_starts(editor, cursor_row, content_width);
        let segment = starts
            .iter()
            .rposition(|start| *start <= cursor_column)
            .unwrap_or(0);
        scroll.follow_wrapped(editor, cursor_row, segment, height, content_width);
    } else {
        scroll.skip = 0;
        scroll.row = Viewport::follow(scroll.row, cursor_row, height);
        // A wide character under the cursor is brought fully into view, its
        // second half first so the first half wins when only one fits.
        scroll.column = Viewport::follow(
            scroll.column,
            cursor_column.saturating_add(cursor_glyph_width(editor).saturating_sub(1)),
            content_width,
        );
        scroll.column = Viewport::follow(scroll.column, cursor_column, content_width);
    }
    let first_row = scroll.row;
    let lines = scroll.lines(editor);
    let overlays = Overlays {
        styles: &CellStyles::build(&editor.buffer.data, options),
        highlighted: &options.highlight.matches(&editor.buffer.data),
        list: options.list,
    };

    for (screen_row, line) in lines.iter().enumerate() {
        let y = shifted(layout.area.y, screen_row);
        // Only a row's first screen row is numbered, as in vim.
        if line.first {
            draw_line_number(
                buffer,
                layout,
                y,
                line.row,
                cursor_row,
                options.relative_numbers,
            );
        }
        draw_buffer_row(buffer, layout, editor, line, y, &overlays);
    }

    draw_jump_labels(buffer, layout, editor, options, &lines);
    if options.cursorline {
        draw_cursor_line(buffer, layout, cursor_row, &lines);
    }
    draw_scrollbar(buffer, layout, editor.buffer.rows.len(), first_row);

    if layout.content_width == 0 {
        return None;
    }
    // The cursor is on the last screen row of its row that starts at or
    // before its column; none, when that part of the row is off screen.
    let (screen_row, line) = lines
        .iter()
        .enumerate()
        .rfind(|(_, line)| line.row == cursor_row && line.start <= cursor_column)?;
    let screen_column = cursor_column - line.start;
    if screen_column >= content_width {
        return None;
    }

    Some((
        shifted(layout.content_x, screen_column),
        shifted(layout.area.y, screen_row),
    ))
}

fn viewport_start(cursor: usize, height: u16, total: usize) -> usize {
    if height == 0 || total == 0 {
        return 0;
    }
    cursor
        .min(total.saturating_sub(1))
        .saturating_sub(usize::from(height).saturating_sub(1))
}

fn cursor_display_column(editor: &Editor) -> usize {
    editor.buffer.cursor_row().map_or(0, |row| {
        row_display_column(editor, row, editor.buffer.cursor)
    })
}

/// The columns the character under the cursor needs to be seen whole: two
/// for a wide one, otherwise one.  A tab only needs its first column, which
/// is where the cursor is drawn on it.
fn cursor_glyph_width(editor: &Editor) -> usize {
    match display::decode(&editor.buffer.data, editor.buffer.cursor).0 {
        Some('\t') | None => 1,
        Some(character) => display::char_width(character).max(1),
    }
}

/// `base + by` in screen space, saturating at the edge of a `u16`.
fn shifted(base: u16, by: usize) -> u16 {
    base.saturating_add(u16::try_from(by).unwrap_or(u16::MAX))
}

fn draw_line_number(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    y: u16,
    row: usize,
    cursor_row: usize,
    relative: bool,
) {
    if layout.gutter_digit_width == 0 {
        return;
    }
    let number = if relative && row != cursor_row {
        row.abs_diff(cursor_row)
    } else {
        row.saturating_add(1)
    };
    let number = number.to_string();
    let visible = &number[number
        .len()
        .saturating_sub(usize::from(layout.gutter_digit_width))..];
    // No longer than `gutter_digit_width`, so the conversion is exact.
    let width = visible.len() as u16;
    write_text(
        buffer,
        // Right-aligned: the digits end where the gutter's digit columns do.
        layout
            .area
            .x
            .saturating_add(layout.gutter_digit_width - width),
        y,
        width,
        visible,
        // A terminal multiplexer may remap fixed ANSI palette entries. The
        // terminal's default foreground is the only color guaranteed to
        // contrast with its configured background.
        Style::default(),
    );
}

/// The per-frame layers painted over the buffer bytes.
struct Overlays<'a> {
    styles: &'a CellStyles,
    /// Byte ranges `hlsearch` is tinting.
    highlighted: &'a [(usize, usize)],
    /// The `list` glyphs, while `list` is on.
    list: Option<&'a ListChars>,
}

fn draw_buffer_row(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    line: &ScreenLine,
    y: u16,
    overlays: &Overlays<'_>,
) {
    if layout.content_width == 0 {
        return;
    }
    let row_index = line.row;
    let row = editor.buffer.rows[row_index];
    let data = &editor.buffer.data;
    let first_column = line.start;
    let last_column = line.end;
    let mut display_column = 0usize;
    // Where this row's run of trailing whitespace begins, so `trail` can be
    // told from `space` without rescanning the row for every byte.
    let trailing = overlays.list.map_or(row.end, |_| trailing_start(data, row));

    for cell in display::cells(data, row.start, row.end, editor.tabstop) {
        if cell.column >= last_column {
            return;
        }
        display_column = cell.column.saturating_add(cell.width);
        if display_column <= first_column {
            continue;
        }
        // A cell takes its style from the bytes it was drawn from: its first
        // byte for the highlighters, any of them for a match or the
        // selection, so a character is never half highlighted.
        let mut style = overlays.styles.style(cell.start);
        if overlays
            .highlighted
            .iter()
            .any(|(start, end)| *start < cell.end && cell.start < *end)
        {
            style = style.bg(HIGHLIGHT_BACKGROUND);
        }
        if (cell.start..cell.end).any(|byte| is_selected(editor, row_index, byte)) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let glyph = Glyph::of(data, cell);
        // A wide character cut by either edge of the text area cannot be
        // drawn in half, so the half that is on screen is left blank.
        let clipped = glyph.wide && (cell.column < first_column || display_column > last_column);
        for offset in 0..cell.width {
            let column = cell.column.saturating_add(offset);
            if column >= last_column {
                break;
            }
            if column < first_column {
                continue;
            }
            let x = shifted(layout.content_x, column - first_column);
            let Some(target) = buffer.cell_mut((x, y)) else {
                continue;
            };
            let listed = overlays
                .list
                .and_then(|chars| list_glyph(chars, data, cell, offset, trailing));
            match listed {
                Some(mark) => {
                    target.set_char(mark).set_style(style.fg(LISTCHAR_COLOR));
                }
                // The first column holds the character; the rest of a tab,
                // the second half of a wide character and a clipped half are
                // blank, carrying the style so a selection stays solid.
                None if offset == 0 && !clipped => {
                    glyph.draw(target, data, cell);
                    target.set_style(style);
                }
                None => {
                    target.set_char(' ').set_style(style);
                }
            }
        }
    }

    // Newlines and logical EOF have a cursor/selection position but no byte
    // glyph. Styling the following blank cell keeps an inclusive selection
    // visible on empty lines and at line ends.
    if display_column >= first_column && display_column < last_column {
        let x = shifted(layout.content_x, display_column - first_column);
        // `eol` marks the line ending, so it is only drawn where there is
        // one: the last row of a file without a final newline has none.
        if let Some(eol) = overlays.list.and_then(|chars| chars.eol)
            && editor.buffer.data.get(row.end) == Some(&b'\n')
            && let Some(cell) = buffer.cell_mut((x, y))
        {
            cell.set_char(eol)
                .set_style(Style::default().fg(LISTCHAR_COLOR));
        }
        if is_selected(editor, row_index, row.end)
            && let Some(cell) = buffer.cell_mut((x, y))
        {
            cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }
}

/// Whether one byte of `row_index` is inside the visual selection.
///
/// The row is passed in rather than looked up because the caller is already
/// walking one row at a time, and a blockwise selection has to know which
/// column a byte sits in to decide.
fn is_selected(editor: &Editor, row_index: usize, index: usize) -> bool {
    if editor.mode != Mode::Visual {
        return false;
    }
    if let Some((first, last, left, right)) = editor.block_bounds() {
        let Some(row) = editor.buffer.rows.get(row_index) else {
            return false;
        };
        // The rectangle covers text, not the blank past the end of a short
        // line, so the cell standing in for the newline is never part of it.
        if index >= row.end || !(first..=last).contains(&row_index) {
            return false;
        }
        let column = index.saturating_sub(row.start);
        return (left..=right).contains(&column);
    }
    let start = editor.visual.start.min(editor.visual.end);
    let end = editor.visual.start.max(editor.visual.end);
    (start..=end).contains(&index)
}

/// The per-byte style source for one frame.
///
/// The table is kept in the compact form each highlighter produces and
/// expanded to a [`Style`] one cell at a time; a buffer-sized `Vec<Style>`
/// would cost several times the memory of the data it is built from.  With
/// neither highlighter active nothing is allocated at all.
///
/// Markdown display replaces source highlighting rather than layering over
/// it: the two describe the same bytes and would otherwise fight.
enum CellStyles {
    Plain,
    Syntax(Vec<Option<Color>>),
    Markdown(Vec<Face>),
}

impl CellStyles {
    fn build(source: &[u8], options: &RenderOptions<'_>) -> Self {
        if options.markdown {
            return Self::Markdown(faces(source));
        }
        let Some(syntax) = options.syntax else {
            return Self::Plain;
        };
        let mut colors = vec![None; source.len()];

        for token in tokens(source, syntax) {
            colors[token.start..token.end].fill(Some(syntax_color(&token.kind, syntax)));
        }
        Self::Syntax(colors)
    }

    fn style(&self, index: usize) -> Style {
        match self {
            Self::Plain => Style::default(),
            Self::Syntax(colors) => colors
                .get(index)
                .copied()
                .flatten()
                .map_or_else(Style::default, |color| Style::default().fg(color)),
            Self::Markdown(faces) => faces
                .get(index)
                .copied()
                .map_or_else(Style::default, markdown_style),
        }
    }
}

fn markdown_style(face: Face) -> Style {
    let mut style = match face.kind {
        Kind::Text => Style::default(),
        Kind::Marker | Kind::Rule => Style::default().fg(MARKER_COLOR),
        // A malformed level cannot occur, but clamping keeps the palette
        // lookup total instead of relying on the parser to stay in range.
        Kind::Heading(level) => {
            Style::default().fg(HEADING_COLORS[usize::from(level).clamp(1, 6) - 1])
        }
        Kind::Code => Style::default().fg(CODE_COLOR),
        Kind::Quote => Style::default()
            .fg(QUOTE_COLOR)
            .add_modifier(Modifier::ITALIC),
        Kind::List => Style::default().fg(LIST_COLOR),
        Kind::Link => Style::default()
            .fg(LINK_COLOR)
            .add_modifier(Modifier::UNDERLINED),
        Kind::Url => Style::default().fg(MARKER_COLOR),
    };
    if face.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if face.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if face.strikethrough {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

fn syntax_color(kind: &SyntaxKind, syntax: &SyntaxConfig) -> Color {
    match kind {
        SyntaxKind::Keyword => rgb_color(&syntax.keyword.color),
        SyntaxKind::Type => rgb_color(&syntax.type_name.color),
        SyntaxKind::Word => rgb_color(&syntax.word.color),
        SyntaxKind::Preprocessor => rgb_color(&SyntaxConfig::PREPROCESSOR),
        SyntaxKind::String => rgb_color(&SyntaxConfig::STRING),
        SyntaxKind::Comment => rgb_color(&SyntaxConfig::COMMENT),
    }
}

fn rgb_color(color: &Rgb) -> Color {
    Color::Rgb(color.red, color.green, color.blue)
}

/// Keeps the end of `text` when it does not fit.
///
/// Recent files share long directory prefixes, so truncating from the right
/// the way every other label does would render a column of identical rows.
/// The file name is the part that tells them apart.
fn elide_start(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if width == 0 || count <= width {
        return text.to_owned();
    }
    // One column goes to the ellipsis standing in for the removed head.
    let mut result = String::from('\u{2026}');
    result.extend(text.chars().skip(count - width + 1));
    debug_assert_eq!(result.chars().count(), width);
    result
}

/// Draws a full-pane list -- the explorer, or the recent-file picker -- and
/// returns where the terminal cursor belongs.
///
/// Only the visible rows are labeled, so a directory with thousands of entries
/// does not build thousands of strings for every frame.
fn draw_list<T>(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    items: &[T],
    first_item: usize,
    cursor: usize,
    style: Style,
    label: impl Fn(&T) -> String,
) -> Option<(u16, u16)> {
    if layout.editor_height == 0 {
        return None;
    }

    for screen_row in 0..usize::from(layout.editor_height) {
        let Some(item) = items.get(first_item.saturating_add(screen_row)) else {
            break;
        };
        let y = shifted(layout.area.y, screen_row);
        write_text(
            buffer,
            layout.content_x,
            y,
            layout.content_width,
            &label(item),
            style,
        );
    }

    draw_scrollbar(buffer, layout, items.len(), first_item);

    if layout.content_width == 0 || items.is_empty() || cursor < first_item {
        return None;
    }
    let screen_row = cursor - first_item;
    if screen_row >= usize::from(layout.editor_height) {
        return None;
    }
    Some((layout.content_x, shifted(layout.area.y, screen_row)))
}

/// Marks the cursor's row for vim's `cursorline`.
///
/// Vim's own terminal default for `CursorLine` is `cterm=underline` rather
/// than a background color, and that is the right choice here for the same
/// reason: the editor cannot know whether the terminal's background is light
/// or dark, so any fixed tint would be unreadable against half of them.
///
/// The mark is applied after every other layer so it covers jump labels and
/// highlighted matches too, and it stops short of the scrollbar, which is not
/// part of the line.
fn draw_cursor_line(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    cursor_row: usize,
    lines: &[ScreenLine],
) {
    // The mark stops at the text's right edge; the scrollbar is past it.
    let end = layout.content_x.saturating_add(layout.content_width);
    // A wrapped row is marked on every screen row it takes.
    for (screen_row, _) in lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.row == cursor_row)
    {
        let y = shifted(layout.area.y, screen_row);
        buffer.set_style(
            Rect::new(layout.area.x, y, end - layout.area.x, 1),
            Style::default().add_modifier(Modifier::UNDERLINED),
        );
    }
}

/// Draws the `s`/`t` labels over the text they select.
///
/// A label is written on top of the matched byte and, when it needs more than
/// one key, the bytes after it: EasyMotion covers the text rather than
/// reflowing it, which keeps every other column where the reader left it.
fn draw_jump_labels(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    options: &RenderOptions<'_>,
    lines: &[ScreenLine],
) {
    let Some((targets, typed)) = options.jump else {
        return;
    };
    if layout.content_width == 0 {
        return;
    }
    // A label inside a visual selection would otherwise inherit its reverse
    // video and come out inverted; labels have to look the same everywhere.
    let style = Style::default()
        .fg(JUMP_FOREGROUND)
        .bg(JUMP_BACKGROUND)
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::REVERSED);

    for target in targets {
        let Some(label) = target.label.strip_prefix(typed) else {
            continue;
        };
        let Some(row_index) = editor.buffer.row_for_index(target.match_start) else {
            continue;
        };
        let start = row_display_column(editor, row_index, target.match_start);

        for (offset, key) in label.iter().enumerate() {
            let column = start.saturating_add(offset);
            // A label key goes on whichever screen row shows its column,
            // which for a wrapped row need not be the one the label began on.
            let Some((screen_row, line)) = lines.iter().enumerate().find(|(_, line)| {
                line.row == row_index && line.start <= column && column < line.end
            }) else {
                continue;
            };
            let y = shifted(layout.area.y, screen_row);
            let x = shifted(layout.content_x, column - line.start);
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_char(char::from(*key)).set_style(style);
            }
        }
    }
}

/// Where a row's run of trailing whitespace begins.
fn trailing_start(data: &[u8], row: crate::buffer::Row) -> usize {
    let mut at = row.end;
    while at > row.start && matches!(data[at - 1], b' ' | b'\t') {
        at -= 1;
    }
    at
}

/// The `list` glyph for column `offset` of one laid-out character, or `None`
/// when the character is drawn as itself.
fn list_glyph(
    chars: &ListChars,
    data: &[u8],
    cell: Cell,
    offset: usize,
    trailing: usize,
) -> Option<char> {
    match display::decode(data, cell.start).0 {
        // A tab keeps its full width: the head goes in the first cell and the
        // fill in the rest, so nothing moves when `list` is switched on.
        Some('\t') => chars
            .tab
            .map(|(head, fill)| if offset == 0 { head } else { fill }),
        // A non-breaking space is one character, `C2 A0` in UTF-8, or a lone
        // `A0` in a Latin-1 file, which is not valid UTF-8 but means the same.
        Some('\u{a0}') => chars.nbsp,
        None if data[cell.start] == 0xa0 => chars.nbsp,
        Some(' ') if cell.start >= trailing => chars.trail.or(chars.space),
        Some(' ') => chars.space,
        _ => None,
    }
}

/// What one laid-out character draws in its first cell.
struct Glyph {
    /// The first code point, or `None` for a byte that is not valid UTF-8.
    first: Option<char>,
    /// Whether it takes two cells, which a clipped edge cannot show half of.
    wide: bool,
}

impl Glyph {
    fn of(data: &[u8], cell: Cell) -> Self {
        let first = display::decode(data, cell.start).0;
        Self {
            first,
            wide: first.is_some_and(|c| c != '\t' && display::char_width(c) == 2),
        }
    }

    /// Writes the character into `target`.
    ///
    /// Invalid bytes and control characters become U+FFFD so the terminal is
    /// never handed a command (see [`display_char`]).  A character with
    /// marks is written as one symbol so the terminal composes them, and a
    /// mark with no letter to sit on is drawn on a space.
    fn draw(&self, target: &mut ratatui::buffer::Cell, data: &[u8], cell: Cell) {
        let Some(first) = self.first else {
            target.set_char('\u{fffd}');
            return;
        };
        if first == '\t' {
            target.set_char(' ');
            return;
        }
        let text = std::str::from_utf8(&data[cell.start..cell.end]).unwrap_or_default();
        if text.len() == first.len_utf8() {
            target.set_char(display_char(first));
            return;
        }
        let mut symbol = String::with_capacity(text.len() + 1);
        if display::is_mark(first) {
            symbol.push(' ');
        }
        symbol.extend(text.chars().map(display_char));
        target.set_symbol(&symbol);
    }
}

/// The display column of the character holding `index` within its row,
/// laid out the way the text itself is drawn.
fn row_display_column(editor: &Editor, row_index: usize, index: usize) -> usize {
    let Some(row) = editor.buffer.rows.get(row_index) else {
        return 0;
    };
    display::column_of(
        &editor.buffer.data,
        row.start,
        row.end,
        index,
        editor.tabstop,
    )
}

/// The geometry of a scrollbar track.
///
/// Placing the thumb and reading a position back off it are inverses, so they
/// are derived from one place: if they disagreed by a row, the bar would jump
/// away from the pointer the moment it was dragged.
struct Track {
    thumb_height: usize,
    /// Track rows the thumb can move through.
    scrollable_track: usize,
    /// Items that lie outside the visible window.
    scrollable_items: usize,
}

impl Track {
    fn new(track_height: usize, total_items: usize) -> Self {
        let total_items = total_items.max(1);
        let visible_items = track_height.min(total_items);
        let thumb_height = visible_items
            .saturating_mul(track_height)
            .saturating_add(total_items.saturating_sub(1))
            / total_items;
        let thumb_height = thumb_height.clamp(1, track_height.max(1));
        let track = Self {
            thumb_height,
            scrollable_track: track_height.saturating_sub(thumb_height),
            scrollable_items: total_items.saturating_sub(visible_items),
        };
        // Invariant: the thumb fits the track, and it never has more rows to
        // travel than there are items to scroll, so every row maps back to an
        // item (see `Viewport::item_from_track`).
        debug_assert!(track_height == 0 || track.scrollable_track + thumb_height == track_height);
        debug_assert!(track.scrollable_track <= track.scrollable_items);
        track
    }

    /// The first item that puts the thumb at track row `row`.
    ///
    /// This is the inverse of [`Track::thumb_start`], so the thumb lands under
    /// the pointer that dragged it.
    fn item_at(&self, row: u16) -> usize {
        if self.scrollable_track == 0 {
            return 0;
        }
        // The thumb is placed by truncating division, so the exact inverse is
        // the smallest item count that still reaches this row -- rounding
        // instead leaves the thumb a row behind the pointer that dragged it.
        let item = usize::from(row)
            .saturating_mul(self.scrollable_items)
            .div_ceil(self.scrollable_track)
            .min(self.scrollable_items);
        // Post: the thumb drawn for `item` starts on the row that was dragged.
        debug_assert_eq!(
            self.thumb_start(item),
            usize::from(row).min(self.scrollable_track)
        );
        item
    }

    /// The track row the thumb starts on when `first_item` is at the top.
    fn thumb_start(&self, first_item: usize) -> usize {
        first_item
            .min(self.scrollable_items)
            .saturating_mul(self.scrollable_track)
            .checked_div(self.scrollable_items)
            .unwrap_or(0)
    }
}

fn draw_scrollbar(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    total_items: usize,
    first_item: usize,
) {
    let Some(x) = layout.scrollbar_x else {
        return;
    };
    draw_track(
        buffer,
        x,
        layout.area.y,
        usize::from(layout.editor_height),
        total_items,
        first_item,
    );
}

/// Draws a vertical scrollbar in column `x` from row `top` down.
fn draw_track(
    buffer: &mut TuiBuffer,
    x: u16,
    top: u16,
    track_height: usize,
    total_items: usize,
    first_item: usize,
) {
    if track_height == 0 {
        return;
    }
    let track = Track::new(track_height, total_items);
    let thumb_height = track.thumb_height;
    let thumb_start = track.thumb_start(first_item);

    for row in 0..track_height {
        let y = shifted(top, row);
        let in_thumb = (thumb_start..thumb_start.saturating_add(thumb_height)).contains(&row);
        if let Some(cell) = buffer.cell_mut((x, y)) {
            if in_thumb {
                cell.set_char('█')
                    .set_style(Style::default().fg(Color::Gray));
            } else {
                cell.set_char('│')
                    .set_style(Style::default().fg(Color::DarkGray));
            }
        }
    }
}

fn draw_status(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    options: &RenderOptions<'_>,
) -> Option<(u16, u16, u16)> {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    buffer.set_style(
        Rect::new(layout.area.x, layout.status_y, layout.area.width, 1),
        style,
    );

    // The column is the 1-based display column, as vim's ruler shows its
    // virtual column: what the eye counts, not how many bytes UTF-8 or a tab
    // happens to take.
    let column = cursor_display_column(editor).saturating_add(1);
    let row = editor.buffer.cursor_row().unwrap_or(0).saturating_add(1);
    let state = if options.saved { "Saved" } else { "Modified" };
    // Markdown display is not a mode of its own, so it is reported next to
    // the file name rather than replacing NORMAL/INSERT.
    let markdown = if options.markdown { " [MD]" } else { "" };
    let status = format!(
        "{} {}{markdown} ({column},{row}) {state}",
        mode_name(editor.mode),
        options.filename
    );
    write_text(
        buffer,
        layout.area.x,
        layout.status_y,
        layout.area.width,
        &status,
        style,
    );

    // The assistant's switch sits at the right end, in a color of its own
    // so it reads as a button rather than more status text.
    let assistant = options.assistant?;
    let (label, button) = if assistant.open {
        (
            " AI ON ",
            Style::default().fg(Color::Black).bg(Color::Green),
        )
    } else {
        (" AI ", Style::default().fg(Color::White).bg(Color::Magenta))
    };
    let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    if layout.area.width <= width {
        return None;
    }
    let x = layout.area.x + layout.area.width - width;
    write_text(
        buffer,
        x,
        layout.status_y,
        width,
        label,
        button.add_modifier(Modifier::BOLD),
    );
    Some((x, x + width, layout.status_y))
}

/// Draws the assistant panel into `area`: a title bar that doubles as the
/// resize handle, the transcript, and the input line.  Returns where the
/// input cursor is.
fn draw_assistant(
    buffer: &mut TuiBuffer,
    area: Rect,
    assistant: &Assistant,
    scroll: &mut Viewport,
) -> Option<(u16, u16)> {
    scroll.panel_track = None;
    scroll.panel_scroll_max = 0;
    if area.height < 2 || area.width == 0 {
        return None;
    }
    let width = usize::from(area.width);

    let bar = Style::default().fg(Color::Black).bg(Color::Green);
    buffer.set_style(Rect::new(area.x, area.y, area.width, 1), bar);
    let model = assistant.backend.as_ref().map_or("", |b| b.model.as_str());
    let status = assistant.status();
    let mut title = format!(
        "═ AI assistant · {model}{}{status} ═  Esc: editor  Ctrl-K: close  Ctrl-L: new chat  drag here to resize ",
        if status.is_empty() { "" } else { " · " },
    );
    let fill = width.saturating_sub(title.chars().count());
    title.extend(std::iter::repeat_n('═', fill));
    write_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &title,
        bar.add_modifier(Modifier::BOLD),
    );

    // The transcript, wrapped to the panel and pinned to its newest line
    // unless the reader has scrolled back.
    let rows = usize::from(area.height.saturating_sub(2));
    // The rightmost column is kept for the scrollbar, so text never runs
    // under it and does not rewrap when the bar appears.
    let bar = area.width > 1 && rows > 0;
    let text_width = area.width - u16::from(bar);
    let mut lines: Vec<(String, Style)> = Vec::new();
    for entry in &assistant.transcript {
        let (label, style) = match entry.role {
            Role::User => ("you › ", Style::default().fg(Color::Cyan)),
            Role::Assistant => ("ai  › ", Style::default()),
            Role::Tool => ("      ", Style::default().fg(Color::DarkGray)),
            Role::Error => ("  !   ", Style::default().fg(Color::Red)),
            Role::Info => ("      ", Style::default().fg(Color::Yellow)),
        };
        let indent = label.chars().count();
        let room = usize::from(text_width).saturating_sub(indent).max(1);
        let mut first = true;
        for paragraph in entry.text.split('\n') {
            let characters: Vec<char> = paragraph.chars().collect();
            let chunks: Vec<String> = if characters.is_empty() {
                vec![String::new()]
            } else {
                characters
                    .chunks(room)
                    .map(|c| c.iter().collect())
                    .collect()
            };
            for chunk in chunks {
                let prefix = if first { label } else { "      " };
                first = false;
                lines.push((format!("{prefix}{chunk}"), style));
            }
        }
    }
    let most = lines.len().saturating_sub(rows);
    let back = assistant.scroll.min(most);
    let start = most - back;
    for (offset, (line, style)) in lines.iter().skip(start).take(rows).enumerate() {
        let y = area.y + 1 + u16::try_from(offset).unwrap_or(0);
        write_text(buffer, area.x, y, text_width, line, *style);
    }
    scroll.panel_scroll_max = most;
    if bar {
        let x = area.x + area.width - 1;
        draw_track(buffer, x, area.y + 1, rows, lines.len(), start);
        scroll.panel_track = Some((x, area.y + 1, area.height - 2));
    }
    if back > 0 {
        let note = format!(" ↓ {back} more ");
        let x =
            area.x + text_width.saturating_sub(u16::try_from(note.chars().count()).unwrap_or(0));
        write_text(
            buffer,
            x,
            area.y + 1,
            area.width,
            &note,
            Style::default().fg(Color::Black).bg(Color::Yellow),
        );
    }

    let y = area.y + area.height - 1;
    if let Some(question) = assistant.approving() {
        let text = format!("Allow {question} ?  y: yes  n: no  a: always  Esc: stop");
        write_text(
            buffer,
            area.x,
            y,
            area.width,
            &text,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        let x = area.x + u16::try_from(text.chars().count().min(width - 1)).unwrap_or(0);
        return Some((x, y));
    }
    let input = String::from_utf8_lossy(&assistant.input);
    if input.is_empty() && !assistant.focused {
        write_text(
            buffer,
            area.x,
            y,
            area.width,
            "> click here or press Ctrl-K to ask…",
            Style::default().fg(Color::DarkGray),
        );
        return None;
    }
    let before = String::from_utf8_lossy(
        &assistant.input[..assistant.input_cursor.min(assistant.input.len())],
    )
    .chars()
    .count();
    let available = width.saturating_sub(3).max(1);
    let skip = before.saturating_sub(available);
    write_text(
        buffer,
        area.x,
        y,
        area.width,
        "> ",
        Style::default().fg(Color::Green),
    );
    write_characters(
        buffer,
        area.x + 2,
        y,
        area.width.saturating_sub(2),
        input.chars().skip(skip),
        Style::default(),
    );
    let x = area.x + 2 + u16::try_from(before - skip).unwrap_or(0);
    Some((x.min(area.x + area.width - 1), y))
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::Search => "SEARCH",
        Mode::Command => "COMMAND",
        Mode::Visual => "VISUAL",
    }
}

fn draw_prompt(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    options: &RenderOptions<'_>,
) -> Option<(u16, u16)> {
    let y = layout.prompt_y?;
    if matches!(editor.mode, Mode::Command | Mode::Search) {
        let sigil = if editor.mode == Mode::Search {
            '/'
        } else {
            ':'
        };
        if let Some(cell) = buffer.cell_mut((layout.area.x, y)) {
            cell.set_char(sigil);
        }
        if layout.area.width == 1 {
            return Some((layout.area.x, y));
        }

        let available = usize::from(layout.area.width.saturating_sub(1));
        let prompt_length = options.prompt.chars().count();
        let prompt_cursor = options.prompt_cursor.min(prompt_length);
        let first_character = prompt_cursor.saturating_sub(available.saturating_sub(1));
        write_characters(
            buffer,
            layout.area.x.saturating_add(1),
            y,
            layout.area.width.saturating_sub(1),
            options.prompt.chars().skip(first_character),
            Style::default(),
        );
        let cursor_column = prompt_cursor
            .saturating_sub(first_character)
            .min(available - 1);
        return Some((shifted(layout.area.x.saturating_add(1), cursor_column), y));
    }

    if let Some(message) = options.message {
        write_text(
            buffer,
            layout.area.x,
            y,
            layout.area.width,
            message,
            Style::default().fg(Color::Yellow),
        );
    } else {
        write_text(
            buffer,
            layout.area.x,
            y,
            layout.area.width,
            options.pending,
            Style::default().fg(Color::Cyan),
        );
    }
    None
}

fn write_text(buffer: &mut TuiBuffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    write_characters(buffer, x, y, width, text.chars(), style);
}

fn write_characters(
    buffer: &mut TuiBuffer,
    x: u16,
    y: u16,
    width: u16,
    characters: impl Iterator<Item = char>,
    style: Style,
) {
    for (offset, character) in characters.take(usize::from(width)).enumerate() {
        let x = shifted(x, offset);
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_char(display_char(character)).set_style(style);
        }
    }
}

/// Substitutes a placeholder for anything the terminal would read as a command
/// rather than a glyph.
///
/// The text drawn here is not the editor's own: file names, explorer and
/// recent-file entries carry whatever bytes the filesystem holds.  The backend
/// prints a cell's symbol verbatim, so an ESC surviving to a cell hands the
/// terminal an escape sequence -- one crafted name could recolour the screen,
/// or drive OSC 52 to write the clipboard.  Buffer bytes pass through here
/// too, widened to `char`: C0, DEL and C1 are exactly the control bytes.
fn display_char(character: char) -> char {
    if character.is_control() {
        '\u{fffd}'
    } else {
        character
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    use super::*;
    use crate::editor::{VisualKind, VisualSelection};
    use crate::explorer::Entry;

    static EMPTY_HIGHLIGHT: Highlight = Highlight {
        needle: Vec::new(),
        whole_word: false,
    };

    fn options<'a>() -> RenderOptions<'a> {
        RenderOptions {
            history: None,
            relative_numbers: false,
            prompt: "",
            prompt_cursor: 0,
            pending: "",
            jump: None,
            highlight: &EMPTY_HIGHLIGHT,
            cursorline: false,
            list: None,
            wrap: false,
            explorer: None,
            recent: None,
            syntax: None,
            markdown: false,
            message: None,
            filename: "file.txt",
            saved: true,
            assistant: None,
        }
    }

    fn line(buffer: &TuiBuffer, y: u16) -> String {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer.cell((x, y)).expect("cell in test buffer").symbol());
        }
        line
    }

    #[test]
    fn renders_gutter_tabs_status_prompt_and_cursor() {
        let mut editor = Editor::new(b"a\tb\nsecond".to_vec());
        editor.buffer.cursor = 2;
        editor.mode = Mode::Command;
        let backend = TestBackend::new(32, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.relative_numbers = true;
        render_options.prompt = "write";
        render_options.prompt_cursor = 2;

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let backend = terminal.backend();
        assert!(line(backend.buffer(), 0).starts_with("   1 a    b"));
        assert!(line(backend.buffer(), 1).starts_with("   1 second"));
        assert_eq!(backend.buffer().cell((3, 0)).unwrap().fg, Color::Reset);
        assert_eq!(backend.buffer().cell((3, 1)).unwrap().fg, Color::Reset);
        // The status column is the display column: `b` is the third byte but
        // sits in the sixth column, past the four-column tab.
        assert!(line(backend.buffer(), 4).starts_with("COMMAND file.txt (6,1) Saved"));
        assert!(line(backend.buffer(), 5).starts_with(":write"));
        assert_eq!(backend.cursor_position(), Position::new(3, 5));
    }

    #[test]
    fn relative_numbers_keep_the_cursor_line_absolute() {
        let mut editor = Editor::new(b"first\nsecond\nthird".to_vec());
        editor.buffer.cursor = 6;
        let backend = TestBackend::new(16, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.relative_numbers = true;

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("   1 first"));
        assert!(line(buffer, 1).starts_with("   2 second"));
        assert!(line(buffer, 2).starts_with("   1 third"));
    }

    #[test]
    fn gutter_expands_without_truncating_large_line_numbers() {
        let mut data = b"x\n".repeat(9_999);
        data.push(b'x');
        let mut editor = Editor::new(data);
        editor.buffer.cursor = editor.buffer.data.len();
        let mut render_options = options();
        render_options.relative_numbers = true;

        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("    1 x"));
        assert!(line(buffer, 1).starts_with("10000 x"));

        let backend = TestBackend::new(5, 3);
        let mut narrow_terminal = Terminal::new(backend).unwrap();
        narrow_terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();
        assert_eq!(line(narrow_terminal.backend().buffer(), 0), "10000");
    }

    #[test]
    fn visual_selection_uses_inclusive_byte_endpoints() {
        let mut editor = Editor::new(b"abcd".to_vec());
        editor.mode = Mode::Visual;
        editor.visual = VisualSelection {
            start: 1,
            end: 2,
            anchor: 1,
            kind: VisualKind::Charwise,
        };
        let backend = TestBackend::new(16, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &editor, options(), &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(
            !buffer
                .cell((5, 0))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            buffer
                .cell((6, 0))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            buffer
                .cell((7, 0))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !buffer
                .cell((8, 0))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn syntax_spans_color_their_exact_byte_cells() {
        let editor = Editor::new(b"if int name".to_vec());
        let mut syntax = SyntaxConfig::default();
        syntax.keyword.color = Rgb::new(1, 2, 3);
        syntax.type_name.color = Rgb::new(4, 5, 6);
        syntax.word.color = Rgb::new(7, 8, 9);
        syntax.word.words = vec![b"name".to_vec()];
        let backend = TestBackend::new(24, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.syntax = Some(&syntax);
        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((5, 0)).unwrap().fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer.cell((8, 0)).unwrap().fg, Color::Rgb(4, 5, 6));
        assert_eq!(buffer.cell((12, 0)).unwrap().fg, Color::Rgb(7, 8, 9));
        assert_eq!(buffer.cell((7, 0)).unwrap().fg, Color::Reset);
    }

    #[test]
    fn markdown_display_styles_cells_and_overrides_source_highlighting() {
        let editor = Editor::new(b"# Title\n**bold** `code`".to_vec());
        // A syntax palette that would claim `bold` proves markdown wins.
        let mut syntax = SyntaxConfig::default();
        syntax.word.color = Rgb::new(1, 2, 3);
        syntax.word.words = vec![b"bold".to_vec()];
        let backend = TestBackend::new(32, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.markdown = true;
        render_options.syntax = Some(&syntax);

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let heading = buffer.cell((7, 0)).unwrap();
        assert_eq!(buffer.cell((5, 0)).unwrap().fg, MARKER_COLOR);
        assert_eq!(heading.fg, HEADING_COLORS[0]);
        assert!(heading.modifier.contains(Modifier::BOLD));

        let emphasized = buffer.cell((7, 1)).unwrap();
        assert_eq!(buffer.cell((5, 1)).unwrap().fg, MARKER_COLOR);
        assert_eq!(emphasized.fg, Color::Reset);
        assert!(emphasized.modifier.contains(Modifier::BOLD));
        assert_eq!(buffer.cell((15, 1)).unwrap().fg, CODE_COLOR);

        assert!(line(buffer, 3).starts_with("NORMAL file.txt [MD] (1,1) Saved"));
    }

    #[test]
    fn jump_labels_and_search_highlighting_reach_their_exact_cells() {
        let editor = Editor::new(b"the fox\nthe end".to_vec());
        let targets = [
            Target {
                match_start: 4,
                destination: 4,
                label: b"a".to_vec(),
            },
            Target {
                match_start: 12,
                destination: 12,
                label: b"sd".to_vec(),
            },
        ];
        let highlight = Highlight {
            needle: b"the".to_vec(),
            whole_word: true,
        };
        let backend = TestBackend::new(24, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.jump = Some((&targets, b""));
        render_options.highlight = &highlight;
        render_options.pending = "jump";

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        // A one-key label covers its match; a two-key label also covers the
        // byte after it.
        assert_eq!(buffer.cell((9, 0)).unwrap().symbol(), "a");
        assert_eq!(buffer.cell((9, 0)).unwrap().bg, JUMP_BACKGROUND);
        assert_eq!(buffer.cell((10, 0)).unwrap().symbol(), "o");
        assert_eq!(buffer.cell((9, 1)).unwrap().symbol(), "s");
        assert_eq!(buffer.cell((10, 1)).unwrap().symbol(), "d");

        // Both `the`s are tinted, and the space after the first is not.
        for y in 0..2 {
            for x in 5..8 {
                assert_eq!(
                    buffer.cell((x, y)).unwrap().bg,
                    HIGHLIGHT_BACKGROUND,
                    "cell ({x},{y})"
                );
            }
        }
        assert_eq!(buffer.cell((8, 0)).unwrap().bg, Color::Reset);
        assert!(line(buffer, 4).starts_with("jump"));
    }

    #[test]
    fn a_typed_label_prefix_hides_the_targets_it_rules_out() {
        let editor = Editor::new(b"aXbXc".to_vec());
        let targets = [
            Target {
                match_start: 1,
                destination: 1,
                label: b"qa".to_vec(),
            },
            Target {
                match_start: 3,
                destination: 3,
                label: b"wa".to_vec(),
            },
        ];
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.jump = Some((&targets, b"q"));

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        // The surviving target shows only what is left to type; the other is
        // gone and its text is untouched.
        assert_eq!(buffer.cell((6, 0)).unwrap().symbol(), "a");
        assert_eq!(buffer.cell((6, 0)).unwrap().bg, JUMP_BACKGROUND);
        assert_eq!(buffer.cell((8, 0)).unwrap().symbol(), "X");
        assert_eq!(buffer.cell((8, 0)).unwrap().bg, Color::Reset);
    }

    #[test]
    fn cursorline_marks_the_whole_cursor_row_and_nothing_else() {
        let mut editor = Editor::new(b"first\nsecond".to_vec());
        editor.buffer.cursor = 8;
        let backend = TestBackend::new(16, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.cursorline = true;

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let underlined = |x, y| {
            buffer
                .cell((x, y))
                .unwrap()
                .modifier
                .contains(Modifier::UNDERLINED)
        };
        // The whole of row 1, gutter through the last text column, and none of
        // row 0.
        for x in 0..15 {
            assert!(underlined(x, 1), "column {x} of the cursor row");
            assert!(!underlined(x, 0), "column {x} of another row");
        }
        // The scrollbar is not part of the line.
        assert!(!underlined(15, 1));
        // Off by default.
        terminal
            .draw(|frame| draw(frame, &editor, options(), &mut Viewport::default()))
            .unwrap();
        assert!(
            !terminal
                .backend()
                .buffer()
                .cell((5, 1))
                .unwrap()
                .modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn list_draws_the_invisible_characters_without_moving_anything() {
        // `a`, tab, `b`, space, `c`, two trailing spaces, then a line that
        // holds a UTF-8 non-breaking space.
        let editor = Editor::new("a\tb c  \nx\u{a0}y".as_bytes().to_vec());
        let chars = crate::listchars::parse(
            "tab:\u{25b8}-,trail:\u{b7},eol:\u{21b2},nbsp:\u{23b5},space:\u{b7}".as_bytes(),
        )
        .unwrap();
        let backend = TestBackend::new(24, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.list = Some(&chars);

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        // A tab keeps its four columns: the head, then the fill.
        assert!(line(buffer, 0).starts_with("   1 a\u{25b8}---b\u{b7}c\u{b7}\u{b7}\u{21b2}"));
        // The non-breaking space is two bytes but one character, so it takes
        // one cell, and that cell is the marker.
        assert!(line(buffer, 1).starts_with("   2 x\u{23b5}y"));
        // The final row has no line ending, so it gets no `eol`.
        assert!(!line(buffer, 1).contains('\u{21b2}'));
        // Glyphs are drawn subdued, and the text between them is not.
        // Column 5 is the `a`; 6 through 9 are the tab it precedes.
        assert_eq!(buffer.cell((6, 0)).unwrap().fg, LISTCHAR_COLOR);
        assert_eq!(buffer.cell((9, 0)).unwrap().fg, LISTCHAR_COLOR);
        assert_eq!(buffer.cell((5, 0)).unwrap().fg, Color::Reset);

        // With `list` off the same buffer is drawn as it always was.
        terminal
            .draw(|frame| draw(frame, &editor, options(), &mut Viewport::default()))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer().cell((6, 0)).unwrap().symbol(),
            " "
        );
    }

    #[test]
    fn viewport_and_scrollbar_follow_the_cursor() {
        let mut editor = Editor::new(b"0\n1\n2\n3\n4\n5".to_vec());
        editor.buffer.cursor = editor.buffer.data.len();
        let backend = TestBackend::new(14, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.pending = "42";
        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let backend = terminal.backend();
        assert!(line(backend.buffer(), 0).starts_with("   4 3"));
        assert!(line(backend.buffer(), 2).starts_with("   6 5"));
        assert!(line(backend.buffer(), 4).starts_with("42"));
        assert_eq!(backend.buffer().cell((13, 2)).unwrap().symbol(), "█");
        assert_eq!(backend.cursor_position(), Position::new(6, 2));
    }

    #[test]
    fn control_characters_in_names_never_reach_a_cell() {
        // A name is whatever the filesystem holds.  The backend prints a
        // cell's symbol verbatim, so ESC/OSC/BEL/CR/LF reaching a cell would
        // be handed to the terminal as commands rather than drawn.
        let editor = Editor::new(Vec::new());
        let explorer = Explorer {
            directory: PathBuf::from("."),
            entries: vec![Entry {
                name: OsString::from("a\u{1b}]52;c;QQ==\u{7}b"),
                path: PathBuf::from("a"),
                directory: false,
            }],
            cursor: 0,
        };
        let backend = TestBackend::new(40, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.explorer = Some(&explorer);
        render_options.filename = "n\u{1b}[31mame\r\n";
        render_options.message = Some("do\u{8}ne");

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("     a\u{fffd}]52;c;QQ==\u{fffd}b"));
        assert!(line(buffer, 2).starts_with("NORMAL n\u{fffd}[31mame\u{fffd}\u{fffd} (1,1) Saved"));
        assert!(line(buffer, 3).starts_with("do\u{fffd}ne"));
        for y in 0..buffer.area.height {
            assert!(
                !line(buffer, y).contains(char::is_control),
                "control character rendered on row {y}"
            );
        }
    }

    #[test]
    fn control_characters_typed_into_the_prompt_are_not_drawn() {
        let mut editor = Editor::new(Vec::new());
        editor.mode = Mode::Command;
        let backend = TestBackend::new(24, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.prompt = "w\u{1b}[2Jrite";
        render_options.prompt_cursor = 2;

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 2).starts_with(":w\u{fffd}[2Jrite"));
    }

    #[test]
    fn explorer_uses_its_own_viewport_and_blue_entries() {
        let editor = Editor::new(Vec::new());
        let explorer = Explorer {
            directory: PathBuf::from("."),
            entries: vec![
                Entry {
                    name: OsString::from("a"),
                    path: PathBuf::from("a"),
                    directory: false,
                },
                Entry {
                    name: OsString::from("dir"),
                    path: PathBuf::from("dir"),
                    directory: true,
                },
                Entry {
                    name: OsString::from("z"),
                    path: PathBuf::from("z"),
                    directory: false,
                },
            ],
            cursor: 2,
        };
        let backend = TestBackend::new(18, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.explorer = Some(&explorer);
        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let backend = terminal.backend();
        assert!(line(backend.buffer(), 0).starts_with("     dir/"));
        assert!(line(backend.buffer(), 1).starts_with("     z"));
        assert_eq!(backend.buffer().cell((5, 1)).unwrap().fg, Color::Blue);
        assert_eq!(backend.cursor_position(), Position::new(5, 1));
    }

    #[test]
    fn the_recent_picker_takes_the_pane_and_keeps_the_end_of_long_paths() {
        assert_eq!(elide_start("short", 10), "short");
        assert_eq!(elide_start("abcdef", 6), "abcdef");
        assert_eq!(
            elide_start("/a/very/long/path.txt", 10),
            "\u{2026}/path.txt"
        );
        assert_eq!(elide_start("abc", 1), "\u{2026}");
        assert_eq!(elide_start("abc", 0), "abc");

        let editor = Editor::new(b"buffer text".to_vec());
        let recent = Recent {
            paths: vec![PathBuf::from("/tmp/one.txt"), PathBuf::from("/tmp/two.txt")],
            cursor: 1,
        };
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.recent = Some(&recent);

        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Viewport::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        // The list replaces the buffer entirely, and the gutter carries no
        // line numbers behind it.
        assert!(line(buffer, 0).starts_with("     /tmp/one.txt"));
        assert!(line(buffer, 1).starts_with("     /tmp/two.txt"));
        assert_eq!(buffer.cell((5, 0)).unwrap().fg, Color::Cyan);
        assert_eq!(terminal.backend().cursor_position(), Position::new(5, 1));
    }

    /// Draws `editor` on a `width` by `height` terminal with the default
    /// options, returning the terminal so its cells and cursor can be read.
    fn drawn(editor: &Editor, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, editor, options(), &mut Viewport::default()))
            .unwrap();
        terminal
    }

    #[test]
    fn tabstop_sets_how_wide_a_tab_is_drawn_and_where_the_cursor_goes() {
        let mut editor = Editor::new(b"a\tb".to_vec());
        editor.tabstop = 8;
        editor.buffer.cursor = 2;
        let terminal = drawn(&editor, 32, 4);
        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("   1 a        b"));
        // `b` is in column 9 of the text, which starts at screen column 5.
        assert_eq!(terminal.backend().cursor_position(), Position::new(14, 0));
        assert!(line(buffer, 2).starts_with("NORMAL file.txt (10,1)"));

        // A click anywhere on the eight columns of the tab is the tab.
        let viewport = Viewport {
            rows: 1,
            content_x: 5,
            content_width: 20,
            ..Viewport::default()
        };
        assert_eq!(viewport.byte_at(&editor, 6, 0), Some(1));
        assert_eq!(viewport.byte_at(&editor, 13, 0), Some(1));
        assert_eq!(viewport.byte_at(&editor, 14, 0), Some(2));

        // `list` fills the whole width too.
        let chars = crate::listchars::parse(b"tab:>-").unwrap();
        let mut listed = options();
        listed.list = Some(&chars);
        let mut terminal = Terminal::new(TestBackend::new(32, 4)).unwrap();
        terminal
            .draw(|frame| draw(frame, &editor, listed, &mut Viewport::default()))
            .unwrap();
        assert!(line(terminal.backend().buffer(), 0).starts_with("   1 a>-------b"));
    }

    #[test]
    fn utf8_text_is_drawn_as_characters_at_their_display_width() {
        // The last row ends in a lone 0xff, which is not UTF-8 at all.
        let mut text = "héllo\n日本x\ne\u{301}".as_bytes().to_vec();
        text.extend_from_slice(b"\xff!");
        let mut editor = Editor::new(text);
        // On the `x`, after two wide characters: byte 13, display column 4.
        editor.buffer.cursor = 13;
        let terminal = drawn(&editor, 24, 6);
        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("   1 héllo "));
        // Each wide character is one cell holding it and one blank cell it
        // spills into, which ratatui skips when it writes to the terminal.
        assert!(line(buffer, 1).starts_with("   2 日 本 x "));
        assert_eq!(buffer.cell((5, 1)).unwrap().symbol(), "日");
        assert_eq!(buffer.cell((7, 1)).unwrap().symbol(), "本");
        // A combining mark shares the cell of the letter it decorates, and a
        // byte that is not UTF-8 is one replacement character.
        assert_eq!(buffer.cell((5, 2)).unwrap().symbol(), "e\u{301}");
        assert_eq!(buffer.cell((6, 2)).unwrap().symbol(), "\u{fffd}");
        assert_eq!(buffer.cell((7, 2)).unwrap().symbol(), "!");
        assert_eq!(terminal.backend().cursor_position(), Position::new(9, 1));
        assert!(line(buffer, 4).starts_with("NORMAL file.txt (5,2)"));
    }

    #[test]
    fn a_selection_covers_every_cell_of_a_wide_character() {
        let mut editor = Editor::new("a日b".as_bytes().to_vec());
        editor.mode = Mode::Visual;
        editor.visual = VisualSelection {
            start: 1,
            end: 1,
            anchor: 1,
            kind: VisualKind::Charwise,
        };
        editor.buffer.cursor = 1;
        let terminal = drawn(&editor, 16, 4);
        let buffer = terminal.backend().buffer();
        let reversed = |x: u16| {
            buffer
                .cell((x, 0))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        };
        // The terminal paints a wide character's second cell with its first
        // one's style -- ratatui does not even send it -- so the selected
        // character is the cell holding it, and nothing either side is.
        assert!(!reversed(5));
        assert_eq!(buffer.cell((6, 0)).unwrap().symbol(), "日");
        assert!(reversed(6));
        assert!(!reversed(8));
    }

    #[test]
    fn a_wide_character_cut_by_the_right_edge_is_blank_not_half_drawn() {
        // Four columns of text: `abc` and the first half of `日`.
        let editor = Editor::new("abc日".as_bytes().to_vec());
        let terminal = drawn(&editor, 10, 4);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((7, 0)).unwrap().symbol(), "c");
        assert_eq!(buffer.cell((8, 0)).unwrap().symbol(), " ");
        // The scrollbar column is untouched by it.
        assert_ne!(buffer.cell((9, 0)).unwrap().symbol(), "日");

        // With the cursor on it, the view scrolls until it fits whole.
        let mut on_it = editor.clone();
        on_it.buffer.cursor = 3;
        let mut scroll = Viewport::default();
        let mut terminal = Terminal::new(TestBackend::new(10, 4)).unwrap();
        terminal
            .draw(|frame| draw(frame, &on_it, options(), &mut scroll))
            .unwrap();
        assert_eq!(scroll.column, 1);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((7, 0)).unwrap().symbol(), "日");
        assert_eq!(terminal.backend().cursor_position(), Position::new(7, 0));
    }

    #[test]
    fn a_click_on_either_half_of_a_wide_character_lands_on_its_start() {
        let editor = Editor::new("a日b\nxyz".as_bytes().to_vec());
        let viewport = Viewport {
            rows: 2,
            content_x: 5,
            content_width: 20,
            ..Viewport::default()
        };
        let byte = |column| viewport.byte_at(&editor, column, 0);
        assert_eq!(byte(5), Some(0));
        assert_eq!(byte(6), Some(1));
        assert_eq!(byte(7), Some(1));
        assert_eq!(byte(8), Some(4));
        assert_eq!(byte(9), Some(5));
    }

    #[test]
    fn a_screen_position_maps_back_onto_the_byte_it_was_drawn_from() {
        let editor = Editor::new(b"ab\tcd\nsecond\nthird".to_vec());
        let viewport = Viewport {
            row: 0,
            column: 0,
            rows: 3,
            content_x: 5,
            content_width: 20,
            first_item: 0,
            scrollbar_x: Some(25),
            ..Viewport::default()
        };
        let byte = |column, row| viewport.byte_at(&editor, column, row);

        assert_eq!(byte(5, 0), Some(0));
        assert_eq!(byte(6, 0), Some(1));
        // A tab is four columns wide, so every one of them is the same byte.
        assert_eq!(byte(7, 0), Some(2));
        assert_eq!(byte(10, 0), Some(2));
        assert_eq!(byte(11, 0), Some(3));
        // Past the end of a line, but still inside the text area, the cursor
        // belongs at the line's end.
        assert_eq!(byte(20, 0), Some(5));
        assert_eq!(byte(5, 1), Some(6));
        // The gutter clamps to the start of the row it labels; the
        // scrollbar, past the text area, and below the last drawn row are
        // not text at all.
        assert_eq!(byte(0, 2), Some(13));
        assert_eq!(byte(25, 0), None);
        assert_eq!(byte(40, 0), None);
        assert_eq!(byte(5, 3), None);
        assert_eq!(byte(5, 9), None);

        // A scrolled viewport offsets both axes.
        let scrolled = Viewport {
            row: 1,
            column: 2,
            ..viewport
        };
        assert_eq!(scrolled.byte_at(&editor, 5, 0), Some(8));
    }

    #[test]
    fn list_positions_and_wheel_scrolling_stay_inside_their_bounds() {
        let viewport = Viewport {
            rows: 3,
            first_item: 4,
            ..Viewport::default()
        };
        assert_eq!(viewport.item_at(10, 0), Some(4));
        assert_eq!(viewport.item_at(10, 2), Some(6));
        // Below the drawn rows, and past the end of a short list, is nothing.
        assert_eq!(viewport.item_at(10, 3), None);
        assert_eq!(viewport.item_at(5, 2), None);

        let mut scroll = Viewport {
            rows: 3,
            ..Viewport::default()
        };
        scroll.scroll_by(3, 10);
        assert_eq!(scroll.row, 3);
        // The origin stops at the last row going down and at zero going up.
        scroll.scroll_by(99, 10);
        assert_eq!(scroll.row, 9);
        scroll.scroll_by(-99, 10);
        assert_eq!(scroll.row, 0);
        assert_eq!(scroll.visible_rows(&Editor::new(Vec::new())), (0, 3));
    }

    #[test]
    fn reading_a_scrollbar_position_back_is_the_exact_inverse_of_drawing_it() {
        // Whatever the shape of the track, clicking a row has to put the
        // thumb on that row, or a drag walks away from the pointer.
        for (height, total) in [(10, 41), (3, 100), (20, 21), (9, 9), (5, 1), (1, 50)] {
            let viewport = Viewport {
                rows: height,
                scrollbar_x: Some(59),
                ..Viewport::default()
            };
            let track = Track::new(height, total);
            for row in 0..=track.scrollable_track {
                let first = viewport.item_from_track(u16::try_from(row).unwrap(), total);
                assert!(
                    first <= track.scrollable_items,
                    "{height}/{total} row {row}"
                );
                assert_eq!(
                    track.thumb_start(first),
                    row,
                    "track {height} of {total}, row {row} landed on item {first}"
                );
            }
            // A row past the thumb's travel clamps to the end rather than
            // running off it.  A thumb that fills its track cannot express a
            // position at all, and stays put.
            let past = viewport.item_from_track(u16::try_from(height).unwrap(), total);
            if track.scrollable_track == 0 {
                assert_eq!(past, 0, "track {height} of {total} cannot scroll");
            } else {
                assert_eq!(past, track.scrollable_items, "track {height} of {total}");
            }
        }

        let viewport = Viewport {
            scrollbar_x: Some(59),
            ..Viewport::default()
        };
        assert!(viewport.on_scrollbar(59));
        assert!(!viewport.on_scrollbar(58));
        // A pane too narrow to draw a bar has no bar to click.
        assert!(!Viewport::default().on_scrollbar(59));
    }

    /// Draws `editor` with `wrap` on, returning the terminal and the
    /// viewport the frame left behind.
    fn wrapped(editor: &Editor, width: u16, height: u16) -> (Terminal<TestBackend>, Viewport) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut viewport = Viewport::default();
        let mut wrap = options();
        wrap.wrap = true;
        terminal
            .draw(|frame| draw(frame, editor, wrap, &mut viewport))
            .unwrap();
        (terminal, viewport)
    }

    #[test]
    fn wrap_continues_a_long_row_on_the_next_screen_row() {
        // Twelve columns leave six for text past the gutter and scrollbar.
        let mut editor = Editor::new(b"abcdefghij\nxy".to_vec());
        editor.buffer.cursor = 8;
        let (terminal, viewport) = wrapped(&editor, 12, 6);
        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("   1 abcdef"));
        // Only the first screen row of a row carries its number.
        assert!(line(buffer, 1).starts_with("     ghij  "));
        assert!(line(buffer, 2).starts_with("   2 xy"));
        // `i` is the third column of the second screen row.
        assert_eq!(terminal.backend().cursor_position(), Position::new(7, 1));
        assert_eq!((viewport.row, viewport.column, viewport.skip), (0, 0, 0));

        // A click maps back through the wrapped layout.
        let byte = |column, row| viewport.byte_at(&editor, column, row);
        assert_eq!(byte(5, 0), Some(0));
        assert_eq!(byte(5, 1), Some(6));
        assert_eq!(byte(8, 1), Some(9));
        // Past the end of the last piece is the end of the line.
        assert_eq!(byte(10, 1), Some(10));
        assert_eq!(byte(5, 2), Some(11));
        assert_eq!(viewport.visible_rows(&editor), (0, 2));
    }

    #[test]
    fn wrap_moves_a_wide_character_cut_by_the_edge_to_the_next_row() {
        let editor = Editor::new("abcde漢x".as_bytes().to_vec());
        let (terminal, viewport) = wrapped(&editor, 12, 6);
        let buffer = terminal.backend().buffer();
        // The column it did not fit in is left blank.
        assert_eq!(buffer.cell((10, 0)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((5, 1)).unwrap().symbol(), "漢");
        assert_eq!(buffer.cell((7, 1)).unwrap().symbol(), "x");
        // The blank column is still the first screen row's, not the next.
        assert_eq!(viewport.byte_at(&editor, 10, 0), Some(4));
        assert_eq!(viewport.byte_at(&editor, 6, 1), Some(5));
    }

    #[test]
    fn wrap_gives_a_full_row_one_more_screen_row_for_the_cursor_at_its_end() {
        let mut editor = Editor::new(b"abcdef".to_vec());
        editor.buffer.cursor = 6;
        let (terminal, _) = wrapped(&editor, 12, 6);
        assert_eq!(terminal.backend().cursor_position(), Position::new(5, 1));

        // Anywhere else on the row, it takes one screen row.
        editor.buffer.cursor = 5;
        let (terminal, _) = wrapped(&editor, 12, 6);
        assert_eq!(terminal.backend().cursor_position(), Position::new(10, 0));
        assert!(line(terminal.backend().buffer(), 1).starts_with(&" ".repeat(11)));
    }

    #[test]
    fn wrap_scrolls_by_screen_rows_and_keeps_the_cursor_on_screen() {
        // Every row takes two screen rows, and four fit.
        let mut editor = Editor::new(b"aaaaaaa\nbbbbbbb\nccccccc\nddddddd\n".to_vec());
        editor.buffer.cursor = 24;
        let (terminal, viewport) = wrapped(&editor, 12, 6);
        assert_eq!(viewport.row, 2);
        assert!(line(terminal.backend().buffer(), 0).starts_with("   3 cccccc"));
        assert_eq!(terminal.backend().cursor_position(), Position::new(5, 2));
        assert_eq!(viewport.visible_rows(&editor), (2, 4));

        // A row taller than the screen scrolls inside itself.
        let mut editor = Editor::new([b'x'; 30].to_vec());
        editor.buffer.cursor = 29;
        let (terminal, viewport) = wrapped(&editor, 12, 4);
        assert_eq!((viewport.row, viewport.skip), (0, 3));
        let buffer = terminal.backend().buffer();
        // Neither screen row is the row's first, so neither is numbered.
        assert!(line(buffer, 0).starts_with("     xxxxxx"));
        assert_eq!(terminal.backend().cursor_position(), Position::new(10, 1));
        assert_eq!(viewport.byte_at(&editor, 5, 0), Some(18));
    }

    #[test]
    fn tiny_terminals_do_not_underflow_or_place_an_invalid_cursor() {
        let editor = Editor::new(b"content".to_vec());
        for (width, height) in [(1, 1), (4, 2), (5, 3), (6, 1)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| draw(frame, &editor, options(), &mut Viewport::default()))
                .unwrap();
            let cursor = terminal.backend().cursor_position();
            assert!(cursor.x < width);
            assert!(cursor.y < height);
        }
    }
}
