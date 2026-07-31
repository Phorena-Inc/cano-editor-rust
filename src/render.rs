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

use crate::editor::{Editor, Mode};
use crate::explorer::Explorer;
use crate::syntax::{Rgb, SyntaxConfig, SyntaxKind, tokens};

const LINE_NUMBER_WIDTH: u16 = 5;
const STATUS_ROWS: u16 = 2;
const TAB_WIDTH: usize = 4;

/// Persistent viewport origin.
///
/// The viewport only moves when the cursor would leave it, so context above
/// and below the cursor stays visible while moving through a long file.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scroll {
    pub row: usize,
    pub column: usize,
}

impl Scroll {
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
    pub count: &'a str,
    pub explorer: Option<&'a Explorer>,
    pub syntax: Option<&'a SyntaxConfig>,
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
    scroll: &mut Scroll,
) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let line_count = if options.explorer.is_none() {
        editor.buffer.rows.len()
    } else {
        0
    };
    let layout = ScreenLayout::new(area, line_count);
    let cursor = {
        let buffer = frame.buffer_mut();
        clear_area(buffer, area);

        let cursor = if let Some(explorer) = options.explorer {
            draw_explorer(buffer, layout, explorer)
        } else {
            draw_editor(buffer, layout, editor, &options, scroll)
        };

        draw_status(buffer, layout, editor, &options);
        draw_prompt(buffer, layout, editor, &options).or(cursor)
    };

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
    scroll: &mut Scroll,
) -> Option<(u16, u16)> {
    if layout.editor_height == 0 {
        return None;
    }

    let cursor_row = editor.buffer.cursor_row().unwrap_or(0);
    let cursor_column = cursor_display_column(editor);
    scroll.row = Scroll::follow(scroll.row, cursor_row, usize::from(layout.editor_height));
    scroll.column = Scroll::follow(
        scroll.column,
        cursor_column,
        usize::from(layout.content_width),
    );
    let first_row = scroll.row;
    let first_column = scroll.column;
    let syntax_colors = syntax_colors(&editor.buffer.data, options.syntax);

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
            &syntax_colors,
        );
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

fn draw_buffer_row(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    editor: &Editor,
    row_index: usize,
    y: u16,
    first_column: usize,
    syntax_colors: &[Option<Color>],
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
            let mut style = syntax_colors
                .get(byte_index)
                .and_then(|color| *color)
                .map_or_else(Style::default, |color| Style::default().fg(color));
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

fn syntax_colors(source: &[u8], syntax: Option<&SyntaxConfig>) -> Vec<Option<Color>> {
    // With highlighting off there is nothing to color; do not allocate a
    // buffer-sized table on every frame just to hold `None`s.
    let Some(syntax) = syntax else {
        return Vec::new();
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
    colors
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

fn draw_explorer(
    buffer: &mut TuiBuffer,
    layout: ScreenLayout,
    explorer: &Explorer,
) -> Option<(u16, u16)> {
    if layout.editor_height == 0 {
        return None;
    }
    let first_entry = viewport_start(
        explorer.cursor,
        layout.editor_height,
        explorer.entries.len(),
    );
    let style = Style::default().fg(Color::Blue);

    for screen_row in 0..usize::from(layout.editor_height) {
        let entry_index = first_entry.saturating_add(screen_row);
        let Some(entry) = explorer.entries.get(entry_index) else {
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
            &entry.display_name(),
            style,
        );
    }

    draw_scrollbar(buffer, layout, explorer.entries.len(), first_entry);

    if layout.content_width == 0 || explorer.entries.is_empty() || explorer.cursor < first_entry {
        return None;
    }
    let screen_row = explorer.cursor - first_entry;
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
    let total_items = total_items.max(1);
    let visible_items = track_height.min(total_items);
    let thumb_height = visible_items
        .saturating_mul(track_height)
        .saturating_add(total_items.saturating_sub(1))
        / total_items;
    let thumb_height = thumb_height.clamp(1, track_height);
    let scrollable_items = total_items.saturating_sub(visible_items);
    let scrollable_track = track_height.saturating_sub(thumb_height);
    let thumb_start = first_item
        .min(scrollable_items)
        .saturating_mul(scrollable_track)
        .checked_div(scrollable_items)
        .unwrap_or(0);

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
    let status = format!(
        "{} {} ({column},{row}) {state}",
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
            options.count,
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

    fn options<'a>() -> RenderOptions<'a> {
        RenderOptions {
            relative_numbers: false,
            prompt: "",
            prompt_cursor: 0,
            count: "",
            explorer: None,
            syntax: None,
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
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
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
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
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
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(line(buffer, 0).starts_with("    1 x"));
        assert!(line(buffer, 1).starts_with("10000 x"));

        let backend = TestBackend::new(5, 3);
        let mut narrow_terminal = Terminal::new(backend).unwrap();
        narrow_terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
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
            .draw(|frame| draw(frame, &editor, options(), &mut Scroll::default()))
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
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((5, 0)).unwrap().fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer.cell((8, 0)).unwrap().fg, Color::Rgb(4, 5, 6));
        assert_eq!(buffer.cell((12, 0)).unwrap().fg, Color::Rgb(7, 8, 9));
        assert_eq!(buffer.cell((7, 0)).unwrap().fg, Color::Reset);
    }

    #[test]
    fn viewport_and_scrollbar_follow_the_cursor() {
        let mut editor = Editor::new(b"0\n1\n2\n3\n4\n5".to_vec());
        editor.buffer.cursor = editor.buffer.data.len();
        let backend = TestBackend::new(14, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut render_options = options();
        render_options.count = "42";
        terminal
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
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
            .draw(|frame| draw(frame, &editor, render_options, &mut Scroll::default()))
            .unwrap();

        let backend = terminal.backend();
        assert!(line(backend.buffer(), 0).starts_with("     dir/"));
        assert!(line(backend.buffer(), 1).starts_with("     z"));
        assert_eq!(backend.buffer().cell((5, 1)).unwrap().fg, Color::Blue);
        assert_eq!(backend.cursor_position(), Position::new(5, 1));
    }

    #[test]
    fn tiny_terminals_do_not_underflow_or_place_an_invalid_cursor() {
        let editor = Editor::new(b"content".to_vec());
        for (width, height) in [(1, 1), (4, 2), (5, 3), (6, 1)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| draw(frame, &editor, options(), &mut Scroll::default()))
                .unwrap();
            let cursor = terminal.backend().cursor_position();
            assert!(cursor.x < width);
            assert!(cursor.y < height);
        }
    }
}
