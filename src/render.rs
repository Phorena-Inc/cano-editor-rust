//! Ratatui rendering for Cano's byte-oriented editor state.
//!
//! The renderer deliberately keeps display coordinates separate from buffer
//! indexes. A byte always occupies one display cell except for a tab, which
//! occupies four, while selections and syntax spans continue to use byte
//! offsets.

use ratatui::Frame;
use ratatui::buffer::Buffer as TuiBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::buffer::Highlight;
use crate::editor::{Editor, Mode};
use crate::explorer::{Entry, Explorer};
use crate::jump::Target;
use crate::markdown::{Face, Kind, faces};
use crate::recent::Recent;
use crate::syntax::{Rgb, SyntaxConfig, SyntaxKind, tokens};

const LINE_NUMBER_WIDTH: u16 = 5;
const STATUS_ROWS: u16 = 2;
const TAB_WIDTH: usize = 4;

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
    pub column: usize,
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
        let track = Track::new(self.rows, total);
        if track.scrollable_track == 0 {
            return 0;
        }
        // The thumb is placed by truncating division, so the exact inverse is
        // the smallest item count that still reaches this row -- rounding
        // instead leaves the thumb a row behind the pointer that dragged it.
        usize::from(row)
            .saturating_mul(track.scrollable_items)
            .div_ceil(track.scrollable_track)
            .min(track.scrollable_items)
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
        let index = self.row.checked_add(screen_row)?;
        let line = *editor.buffer.rows.get(index)?;

        // Columns are display columns: a tab is four wide, so the byte under
        // one has to be found by walking the row rather than by arithmetic.
        let wanted = self
            .column
            .saturating_add(usize::from(column.saturating_sub(self.content_x)));
        let mut display = 0usize;
        for byte in line.start..line.end {
            let width = display_width(editor.buffer.data[byte]);
            if wanted < display.saturating_add(width) {
                return Some(byte);
            }
            display = display.saturating_add(width);
        }
        // Past the end of the line, the cursor belongs at its end, which is
        // where the newline sits.
        Some(line.end)
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
        self.row = if delta < 0 {
            self.row.saturating_sub(delta.unsigned_abs())
        } else {
            self.row.saturating_add(delta as usize).min(last)
        };
    }

    /// The row range the frame showed, as buffer row indexes.
    pub fn visible_rows(&self) -> (usize, usize) {
        (self.row, self.row.saturating_add(self.rows))
    }

    /// Returns the new origin on one axis: unchanged while the cursor is
    /// inside the viewport, otherwise moved just far enough to contain it.
    fn follow(origin: usize, cursor: usize, extent: usize) -> usize {
        if extent == 0 {
            return cursor;
        }
        let origin = origin.min(cursor);
        if cursor >= origin.saturating_add(extent) {
            cursor.saturating_add(1).saturating_sub(extent)
        } else {
            origin
        }
    }
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
    pub explorer: Option<&'a Explorer>,
    /// The Ctrl-R recent-file picker, which takes the pane when it is open.
    pub recent: Option<&'a Recent>,
    pub syntax: Option<&'a SyntaxConfig>,
    /// Renders the buffer as formatted markdown instead of source code.
    pub markdown: bool,
    pub message: Option<&'a str>,
    pub filename: &'a str,
    pub saved: bool,
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
    fn new(area: Rect, line_count: usize) -> Self {
        let editor_height = area.height.saturating_sub(STATUS_ROWS);
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

        Self {
            area,
            editor_height,
            gutter_digit_width,
            content_x: area.x.saturating_add(gutter_width),
            content_width,
            scrollbar_x,
            status_y: area.y.saturating_add(editor_height),
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
    let line_count = if options.explorer.is_none() && options.recent.is_none() {
        editor.buffer.rows.len()
    } else {
        0
    };
    let layout = ScreenLayout::new(area, line_count);
    scroll.content_x = layout.content_x;
    scroll.content_width = layout.content_width;
    scroll.scrollbar_x = layout.scrollbar_x;
    scroll.rows = usize::from(layout.editor_height);

    let mut first_item = 0;
    let cursor = {
        let buffer = frame.buffer_mut();
        clear_area(buffer, area);

        let cursor = if let Some(recent) = options.recent {
            let width = usize::from(layout.content_width);
            first_item = viewport_start(recent.cursor, layout.editor_height, recent.paths.len());
            draw_list(
                buffer,
                layout,
                &recent.paths,
                first_item,
                recent.cursor,
                Style::default().fg(Color::Cyan),
                |path| elide_start(&Recent::display_name(path), width),
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

        draw_status(buffer, layout, editor, &options);
        draw_prompt(buffer, layout, editor, &options).or(cursor)
    };

    scroll.first_item = first_item;

    if editor.mode != Mode::Visual
        && let Some(position) = cursor
    {
        frame.set_cursor_position(position);
    }
}

fn clear_area(buffer: &mut TuiBuffer, area: Rect) {
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.reset();
            }
        }
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
    scroll.row = Viewport::follow(scroll.row, cursor_row, usize::from(layout.editor_height));
    scroll.column = Viewport::follow(
        scroll.column,
        cursor_column,
        usize::from(layout.content_width),
    );
    let first_row = scroll.row;
    let first_column = scroll.column;
    let overlays = Overlays {
        styles: &CellStyles::build(&editor.buffer.data, options),
        highlighted: &options.highlight.matches(&editor.buffer.data),
    };

    for screen_row in 0..usize::from(layout.editor_height) {
        let row_index = first_row.saturating_add(screen_row);
        if row_index >= editor.buffer.rows.len() {
            break;
        }
        let y = layout
            .area
            .y
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
        draw_line_number(
            buffer,
            layout,
            y,
            row_index,
            cursor_row,
            options.relative_numbers,
        );
        draw_buffer_row(
            buffer,
            layout,
            editor,
            row_index,
            y,
            first_column,
            &overlays,
        );
    }

    draw_jump_labels(buffer, layout, editor, options, first_row, first_column);
    if options.cursorline {
        draw_cursor_line(buffer, layout, cursor_row, first_row);
    }
    draw_scrollbar(buffer, layout, editor.buffer.rows.len(), first_row);

    if layout.content_width == 0 || cursor_row < first_row {
        return None;
    }
    let screen_row = cursor_row - first_row;
    if screen_row >= usize::from(layout.editor_height) || cursor_column < first_column {
        return None;
    }
    let screen_column = cursor_column - first_column;
    if screen_column >= usize::from(layout.content_width) {
        return None;
    }

    Some((
        layout
            .content_x
            .saturating_add(u16::try_from(screen_column).unwrap_or(u16::MAX)),
        layout
            .area
            .y
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX)),
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
    let Some(row_index) = editor.buffer.cursor_row() else {
        return 0;
    };
    let row = editor.buffer.rows[row_index];
    editor.buffer.data[row.start..editor.buffer.cursor.min(row.end)]
        .iter()
        .fold(0usize, |column, byte| {
            column.saturating_add(display_width(*byte))
        })
}

fn display_width(byte: u8) -> usize {
    if byte == b'\t' { TAB_WIDTH } else { 1 }
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
    let digit_width = usize::from(layout.gutter_digit_width);
    let visible_start = number.len().saturating_sub(digit_width);
    let visible = &number[visible_start..];
    let padding = digit_width.saturating_sub(visible.len());
    write_text(
        buffer,
        layout.area.x.saturating_add(
            u16::try_from(padding)
                .unwrap_or(u16::MAX)
                .min(layout.gutter_digit_width),
        ),
        y,
        layout.gutter_digit_width.saturating_sub(
            u16::try_from(padding)
                .unwrap_or(u16::MAX)
                .min(layout.gutter_digit_width),
        ),
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
}

fn draw_buffer_row(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    row_index: usize,
    y: u16,
    first_column: usize,
    overlays: &Overlays<'_>,
) {
    if layout.content_width == 0 {
        return;
    }
    let row = editor.buffer.rows[row_index];
    let last_column = first_column.saturating_add(usize::from(layout.content_width));
    let mut display_column = 0usize;

    for byte_index in row.start..row.end {
        let byte = editor.buffer.data[byte_index];
        let width = display_width(byte);
        for tab_column in 0..width {
            let column = display_column.saturating_add(tab_column);
            if column >= last_column {
                return;
            }
            if column < first_column {
                continue;
            }
            let x = layout
                .content_x
                .saturating_add(u16::try_from(column - first_column).unwrap_or(u16::MAX));
            let symbol = if byte == b'\t' {
                ' '
            } else {
                display_byte(byte)
            };
            let mut style = overlays.styles.style(byte_index);
            if overlays
                .highlighted
                .iter()
                .any(|(start, end)| (*start..*end).contains(&byte_index))
            {
                style = style.bg(HIGHLIGHT_BACKGROUND);
            }
            if is_selected(editor, byte_index) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_char(symbol).set_style(style);
            }
        }
        display_column = display_column.saturating_add(width);
    }

    // Newlines and logical EOF have a cursor/selection position but no byte
    // glyph. Styling the following blank cell keeps an inclusive selection
    // visible on empty lines and at line ends.
    if display_column >= first_column && display_column < last_column {
        let x = layout
            .content_x
            .saturating_add(u16::try_from(display_column - first_column).unwrap_or(u16::MAX));
        if is_selected(editor, row.end)
            && let Some(cell) = buffer.cell_mut((x, y))
        {
            cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }
}

fn display_byte(byte: u8) -> char {
    match byte {
        b' '..=b'~' => char::from(byte),
        0xa0..=u8::MAX => char::from(byte),
        _ => '\u{fffd}',
    }
}

fn is_selected(editor: &Editor, index: usize) -> bool {
    if editor.mode != Mode::Visual {
        return false;
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
            let color = syntax_color(&token.kind, syntax);
            let start = token.start.min(source.len());
            let end = token.end.min(source.len());
            for slot in colors.iter_mut().take(end).skip(start) {
                *slot = Some(color);
            }
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
        SyntaxKind::Preprocessor => rgb_color(&SyntaxConfig::preprocessor_color()),
        SyntaxKind::String => rgb_color(&SyntaxConfig::string_color()),
        SyntaxKind::Comment => rgb_color(&SyntaxConfig::comment_color()),
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
        let y = layout
            .area
            .y
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
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
    Some((
        layout.content_x,
        layout
            .area
            .y
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX)),
    ))
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
    first_row: usize,
) {
    let Some(screen_row) = cursor_row.checked_sub(first_row) else {
        return;
    };
    if screen_row >= usize::from(layout.editor_height) {
        return;
    }
    let y = layout
        .area
        .y
        .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
    let style = Style::default().add_modifier(Modifier::UNDERLINED);
    let end = layout.content_x.saturating_add(layout.content_width);
    for x in layout.area.x..end {
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_style(style);
        }
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
    first_row: usize,
    first_column: usize,
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
    let last_column = first_column.saturating_add(usize::from(layout.content_width));

    for target in targets {
        let Some(label) = target.label.strip_prefix(typed) else {
            continue;
        };
        let Some(row_index) = editor.buffer.row_for_index(target.match_start) else {
            continue;
        };
        let Some(screen_row) = row_index.checked_sub(first_row) else {
            continue;
        };
        if screen_row >= usize::from(layout.editor_height) {
            continue;
        }
        let y = layout
            .area
            .y
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
        let start = row_display_column(editor, row_index, target.match_start);

        for (offset, key) in label.iter().enumerate() {
            let column = start.saturating_add(offset);
            if column < first_column || column >= last_column {
                continue;
            }
            let x = layout
                .content_x
                .saturating_add(u16::try_from(column - first_column).unwrap_or(u16::MAX));
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_char(char::from(*key)).set_style(style);
            }
        }
    }
}

/// The display column of `index` within its row, counting a tab as its full
/// width the way the text itself is drawn.
fn row_display_column(editor: &Editor, row_index: usize, index: usize) -> usize {
    let Some(row) = editor.buffer.rows.get(row_index) else {
        return 0;
    };
    editor.buffer.data[row.start..index.clamp(row.start, row.end)]
        .iter()
        .fold(0usize, |column, byte| {
            column.saturating_add(display_width(*byte))
        })
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
        Self {
            thumb_height,
            scrollable_track: track_height.saturating_sub(thumb_height),
            scrollable_items: total_items.saturating_sub(visible_items),
        }
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
    let track_height = usize::from(layout.editor_height);
    if track_height == 0 {
        return;
    }
    let track = Track::new(track_height, total_items);
    let thumb_height = track.thumb_height;
    let thumb_start = track.thumb_start(first_item);

    for row in 0..track_height {
        let y = layout
            .area
            .y
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
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
) {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    for x in layout.area.x..layout.area.x.saturating_add(layout.area.width) {
        if let Some(cell) = buffer.cell_mut((x, layout.status_y)) {
            cell.set_style(style);
        }
    }

    let column = editor.buffer.cursor_column().unwrap_or(0).saturating_add(1);
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
        return Some((
            layout
                .area
                .x
                .saturating_add(1)
                .saturating_add(u16::try_from(cursor_column).unwrap_or(u16::MAX)),
            y,
        ));
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
        let x = x.saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_char(character).set_style(style);
        }
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
    use crate::editor::VisualSelection;
    use crate::explorer::Entry;

    static EMPTY_HIGHLIGHT: Highlight = Highlight {
        needle: Vec::new(),
        whole_word: false,
    };

    fn options<'a>() -> RenderOptions<'a> {
        RenderOptions {
            relative_numbers: false,
            prompt: "",
            prompt_cursor: 0,
            pending: "",
            jump: None,
            highlight: &EMPTY_HIGHLIGHT,
            cursorline: false,
            explorer: None,
            recent: None,
            syntax: None,
            markdown: false,
            message: None,
            filename: "file.txt",
            saved: true,
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
        assert!(line(backend.buffer(), 4).starts_with("COMMAND file.txt (3,1) Saved"));
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
            linewise: false,
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
        assert_eq!(scroll.visible_rows(), (0, 3));
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
