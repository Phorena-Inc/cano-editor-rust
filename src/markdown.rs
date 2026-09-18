//! Markdown display faces.
//!
//! Markdown mode is a display layer, not a preview: every buffer byte still
//! occupies exactly the cell it occupies in Normal display, so the cursor,
//! selections, search and every motion keep working while the text is
//! formatted.  Syntax punctuation is therefore dimmed rather than hidden.
//!
//! Parsing is byte oriented and line scoped.  Nothing here indexes outside
//! the source, and the recursive constructs (emphasis, link text) carry a
//! depth budget so pathological nesting cannot exhaust the stack.

/// The largest number of nested inline constructs that are still parsed.
/// Beyond this the remaining text simply renders unformatted.
const MAX_DEPTH: usize = 6;

/// What one byte contributes to the rendered markdown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Kind {
    /// Ordinary prose.
    #[default]
    Text,
    /// Structural punctuation: `#`, emphasis runs, backticks, brackets.
    Marker,
    /// Heading text, at level 1 through 6.
    Heading(u8),
    /// An inline code span or the body of a fenced code block.
    Code,
    /// Block quote body.
    Quote,
    /// A bullet, ordered-list or task-list marker.
    List,
    /// The visible text of a link.
    Link,
    /// A link destination or reference label, which is shown but subdued.
    Url,
    /// A thematic break or a front-matter line.
    Rule,
}

/// The display attributes of one buffer byte.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Face {
    pub kind: Kind,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
}

/// Classifies every byte of `source` for markdown display.
///
/// The result always has exactly `source.len()` entries, so a renderer can
/// index it with a buffer byte offset.
pub fn faces(source: &[u8]) -> Vec<Face> {
    let mut faces = vec![Face::default(); source.len()];
    let mut blocks = Blocks::default();
    let mut start = 0;
    loop {
        let end = line_end(source, start);
        blocks.line(&mut faces, source, start, end);
        if end >= source.len() {
            break;
        }
        start = end + 1;
    }
    faces
}

#[derive(Clone, Copy, Debug)]
struct Fence {
    byte: u8,
    length: usize,
}

/// Block-level state carried from one line to the next.
#[derive(Default)]
struct Blocks {
    fence: Option<Fence>,
    front_matter: bool,
    /// Byte span of the previous line when it was plain paragraph text, which
    /// is the only thing a setext underline is allowed to promote.
    paragraph: Option<(usize, usize)>,
}

impl Blocks {
    fn line(&mut self, faces: &mut [Face], source: &[u8], start: usize, end: usize) {
        let paragraph = self.paragraph.take();

        // Front matter only ever opens on the very first line, so a `---`
        // rule anywhere else still reads as a thematic break.
        if self.front_matter {
            paint(faces, start, end, Kind::Rule);
            self.front_matter = !closes_front_matter(source, start, end);
            return;
        }
        if start == 0 && opens_front_matter(source, start, end) {
            paint(faces, start, end, Kind::Rule);
            self.front_matter = true;
            return;
        }

        if let Some(fence) = self.fence {
            if closes_fence(source, start, end, fence) {
                paint(faces, start, end, Kind::Marker);
                self.fence = None;
            } else {
                paint(faces, start, end, Kind::Code);
            }
            return;
        }

        let content = skip_blanks(source, start, end);
        if let Some(fence) = opens_fence(source, content, end) {
            paint(faces, content, content + fence.length, Kind::Marker);
            // The info string names the language; colouring it as code keeps
            // the fence reading as one unit.
            paint(faces, content + fence.length, end, Kind::Code);
            self.fence = Some(fence);
            return;
        }

        if content == end {
            return;
        }

        // A setext underline retroactively promotes the line above it, so it
        // has to be tested before `---` is claimed as a thematic break.
        if let Some((previous_start, previous_end)) = paragraph
            && let Some(level) = setext_level(source, content, end)
        {
            for face in span(faces, previous_start, previous_end) {
                face.kind = Kind::Heading(level);
                face.bold = true;
            }
            paint(faces, start, end, Kind::Marker);
            return;
        }

        if is_thematic_break(source, content, end) {
            paint(faces, start, end, Kind::Rule);
            return;
        }

        let mut body = content;
        let mut plain = true;
        while body < end && source[body] == b'>' {
            paint(faces, body, body + 1, Kind::Marker);
            body = skip_blanks(source, body + 1, end);
            plain = false;
        }
        if !plain {
            paint(faces, body, end, Kind::Quote);
        }

        if let Some(level) = atx_level(source, body, end) {
            let text = skip_blanks(source, body + usize::from(level), end);
            paint(faces, body, text, Kind::Marker);
            let mut text_end = trim_blanks(source, text, end);
            let closing = trim_closing_hashes(source, text, text_end);
            if closing < text_end {
                paint(faces, closing, end, Kind::Marker);
                text_end = trim_blanks(source, text, closing);
            }
            for face in span(faces, text, text_end) {
                face.kind = Kind::Heading(level);
                face.bold = true;
            }
            inline(faces, source, text, text_end, 0);
            return;
        }

        if let Some(marker_end) = list_marker(source, body, end) {
            paint(faces, body, marker_end, Kind::List);
            body = skip_blanks(source, marker_end, end);
            if let Some(box_end) = task_box(source, body, end) {
                paint(faces, body, box_end, Kind::Marker);
                paint(faces, body + 1, box_end - 1, Kind::List);
                body = skip_blanks(source, box_end, end);
            }
            plain = false;
        }

        // Only rows that open with a pipe are treated as table markup; prose
        // that merely contains a `|` keeps its own formatting.
        if body < end && source[body] == b'|' {
            if is_table_rule(source, body, end) {
                paint(faces, body, end, Kind::Marker);
                return;
            }
            for (offset, byte) in source[body..end].iter().enumerate() {
                if *byte == b'|' {
                    paint(faces, body + offset, body + offset + 1, Kind::Marker);
                }
            }
            plain = false;
        }

        inline(faces, source, body, end, 0);
        if plain {
            self.paragraph = Some((body, end));
        }
    }
}

fn inline(faces: &mut [Face], source: &[u8], start: usize, end: usize, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut at = start;
    while at < end {
        let next = match source[at] {
            b'\\' => escape(faces, source, at, end),
            b'`' => code_span(faces, source, at, end),
            b'<' => autolink(faces, source, at, end),
            b'!' | b'[' => link(faces, source, at, end, depth),
            b'*' | b'_' | b'~' => emphasis(faces, source, at, end, depth),
            b'h' => bare_url(faces, source, at, end),
            _ => None,
        };
        // Every helper either consumes at least one byte of the line or
        // declines, so the scan cannot stall on a construct it failed to
        // close.  The `max` keeps release builds moving regardless.
        debug_assert!(
            next.is_none_or(|next| at < next && next <= end),
            "{at}..{next:?}"
        );
        at = next.unwrap_or(at + 1).max(at + 1);
    }
}

fn escape(faces: &mut [Face], source: &[u8], at: usize, end: usize) -> Option<usize> {
    let escaped = *source.get(at + 1)?;
    if at + 1 >= end || !escaped.is_ascii_punctuation() {
        return None;
    }
    paint(faces, at, at + 1, Kind::Marker);
    Some(at + 2)
}

fn code_span(faces: &mut [Face], source: &[u8], at: usize, end: usize) -> Option<usize> {
    let opening = run(source, at, end, b'`');
    let mut scan = at + opening;
    while scan < end {
        if source[scan] != b'`' {
            scan += 1;
            continue;
        }
        let closing = run(source, scan, end, b'`');
        if closing == opening {
            paint(faces, at, at + opening, Kind::Marker);
            paint(faces, at + opening, scan, Kind::Code);
            paint(faces, scan, scan + closing, Kind::Marker);
            return Some(scan + closing);
        }
        scan += closing;
    }
    None
}

fn autolink(faces: &mut [Face], source: &[u8], at: usize, end: usize) -> Option<usize> {
    let mut scan = at + 1;
    while scan < end && !source[scan].is_ascii_whitespace() && source[scan] != b'>' {
        scan += 1;
    }
    if scan >= end || source[scan] != b'>' {
        return None;
    }
    let inner = &source[at + 1..scan];
    if !inner.windows(3).any(|window| window == b"://") && !inner.contains(&b'@') {
        return None;
    }
    paint(faces, at, at + 1, Kind::Marker);
    // An autolink is its own link text, so it is styled like one rather than
    // like the subdued destination of a `[text](url)` pair.
    paint(faces, at + 1, scan, Kind::Link);
    paint(faces, scan, scan + 1, Kind::Marker);
    Some(scan + 1)
}

fn link(faces: &mut [Face], source: &[u8], at: usize, end: usize, depth: usize) -> Option<usize> {
    let open = at + usize::from(source[at] == b'!');
    if source.get(open) != Some(&b'[') || open >= end {
        return None;
    }
    let close = matching(source, open, end, b'[', b']')?;
    let mut after = close + 1;
    // An inline destination, or a reference label; a bare `[text]` shortcut
    // reference has neither and still renders as link text.
    if let Some(destination) = matching(source, after, end, b'(', b')')
        .or_else(|| matching(source, after, end, b'[', b']'))
    {
        paint(faces, after, after + 1, Kind::Marker);
        paint(faces, after + 1, destination, Kind::Url);
        paint(faces, destination, destination + 1, Kind::Marker);
        after = destination + 1;
    }

    paint(faces, at, open + 1, Kind::Marker);
    paint(faces, open + 1, close, Kind::Link);
    paint(faces, close, close + 1, Kind::Marker);
    inline(faces, source, open + 1, close, depth + 1);
    Some(after)
}

/// Returns the index of the delimiter closing the one at `at`, honouring
/// nesting and backslash escapes.
fn matching(source: &[u8], at: usize, end: usize, open: u8, close: u8) -> Option<usize> {
    if at >= end || source[at] != open {
        return None;
    }
    let mut depth = 0usize;
    let mut scan = at;
    while scan < end {
        match source[scan] {
            b'\\' => {
                scan += 2;
                continue;
            }
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(scan);
                }
            }
            _ => {}
        }
        scan += 1;
    }
    None
}

fn emphasis(
    faces: &mut [Face],
    source: &[u8],
    at: usize,
    end: usize,
    depth: usize,
) -> Option<usize> {
    let byte = source[at];
    let opening = run(source, at, end, byte);
    // `~` only ever means strikethrough, which is always a pair.
    if byte == b'~' && opening != 2 {
        return None;
    }
    let opening = opening.min(3);
    let content = at + opening;
    if content >= end || source[content].is_ascii_whitespace() {
        return None;
    }
    // `snake_case` identifiers must not turn into emphasis.
    if byte == b'_' && at > 0 && source[at - 1].is_ascii_alphanumeric() {
        return None;
    }

    let mut scan = content;
    while scan < end {
        if source[scan] == b'\\' {
            scan += 2;
            continue;
        }
        if source[scan] != byte {
            scan += 1;
            continue;
        }
        let closing = run(source, scan, end, byte);
        let closes = closing >= opening
            && !source[scan - 1].is_ascii_whitespace()
            && !(byte == b'_'
                && source
                    .get(scan + closing)
                    .is_some_and(u8::is_ascii_alphanumeric));
        if !closes {
            scan += closing;
            continue;
        }
        paint(faces, at, content, Kind::Marker);
        paint(faces, scan, scan + opening, Kind::Marker);
        inline(faces, source, content, scan, depth + 1);
        for face in span(faces, content, scan) {
            match (byte, opening) {
                (b'~', _) => face.strikethrough = true,
                (_, 1) => face.italic = true,
                (_, 2) => face.bold = true,
                _ => {
                    face.bold = true;
                    face.italic = true;
                }
            }
        }
        return Some(scan + opening);
    }
    None
}

fn bare_url(faces: &mut [Face], source: &[u8], at: usize, end: usize) -> Option<usize> {
    let rest = source.get(at..end)?;
    if !rest.starts_with(b"http://") && !rest.starts_with(b"https://") {
        return None;
    }
    let mut scan = at;
    while scan < end
        && !source[scan].is_ascii_whitespace()
        && !matches!(source[scan], b'<' | b'>' | b'(' | b')' | b'"' | b'\'')
    {
        scan += 1;
    }
    // Sentence punctuation that happens to follow the URL is not part of it.
    while scan > at && matches!(source[scan - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
        scan -= 1;
    }
    paint(faces, at, scan, Kind::Link);
    Some(scan)
}

fn line_end(source: &[u8], start: usize) -> usize {
    source
        .get(start..)
        .and_then(|rest| rest.iter().position(|byte| *byte == b'\n'))
        .map_or(source.len(), |offset| start + offset)
}

fn skip_blanks(source: &[u8], mut at: usize, end: usize) -> usize {
    while at < end && matches!(source[at], b' ' | b'\t') {
        at += 1;
    }
    at
}

fn trim_blanks(source: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && matches!(source[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    end
}

fn run(source: &[u8], at: usize, end: usize, byte: u8) -> usize {
    source
        .get(at..end)
        .unwrap_or_default()
        .iter()
        .take_while(|found| **found == byte)
        .count()
}

fn opens_fence(source: &[u8], content: usize, end: usize) -> Option<Fence> {
    let byte = *source.get(content)?;
    if content >= end || !matches!(byte, b'`' | b'~') {
        return None;
    }
    let length = run(source, content, end, byte);
    (length >= 3).then_some(Fence { byte, length })
}

fn closes_fence(source: &[u8], start: usize, end: usize, fence: Fence) -> bool {
    let content = skip_blanks(source, start, end);
    let length = run(source, content, end, fence.byte);
    length >= fence.length && trim_blanks(source, content, end) == content + length
}

fn opens_front_matter(source: &[u8], start: usize, end: usize) -> bool {
    trim_blanks(source, start, end) == start + 3 && run(source, start, end, b'-') == 3
}

fn closes_front_matter(source: &[u8], start: usize, end: usize) -> bool {
    trim_blanks(source, start, end) == start + 3
        && (run(source, start, end, b'-') == 3 || run(source, start, end, b'.') == 3)
}

fn setext_level(source: &[u8], content: usize, end: usize) -> Option<u8> {
    let end = trim_blanks(source, content, end);
    let byte = *source.get(content)?;
    if content >= end || !matches!(byte, b'=' | b'-') {
        return None;
    }
    (content + run(source, content, end, byte) == end).then_some(u8::from(byte == b'-') + 1)
}

fn is_thematic_break(source: &[u8], content: usize, end: usize) -> bool {
    let Some(byte) = source.get(content).copied() else {
        return false;
    };
    if !matches!(byte, b'-' | b'*' | b'_') {
        return false;
    }
    let mut count = 0;
    for candidate in &source[content..end] {
        match *candidate {
            b' ' | b'\t' => {}
            other if other == byte => count += 1,
            _ => return false,
        }
    }
    count >= 3
}

fn atx_level(source: &[u8], body: usize, end: usize) -> Option<u8> {
    let hashes = run(source, body, end, b'#');
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = body + hashes;
    (after == end || matches!(source[after], b' ' | b'\t')).then_some(hashes as u8)
}

/// Returns where a heading's optional closing `###` run begins, or `end` when
/// the heading has none.
fn trim_closing_hashes(source: &[u8], start: usize, end: usize) -> usize {
    let mut closing = end;
    while closing > start && source[closing - 1] == b'#' {
        closing -= 1;
    }
    if closing == end || (closing > start && !matches!(source[closing - 1], b' ' | b'\t')) {
        return end;
    }
    closing
}

fn list_marker(source: &[u8], body: usize, end: usize) -> Option<usize> {
    let byte = *source.get(body)?;
    if body >= end {
        return None;
    }
    let marker_end = if matches!(byte, b'-' | b'*' | b'+') {
        body + 1
    } else {
        let digits = body
            + source[body..end]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
        if digits == body || digits >= end || !matches!(source[digits], b'.' | b')') {
            return None;
        }
        digits + 1
    };
    (marker_end == end || matches!(source[marker_end], b' ' | b'\t')).then_some(marker_end)
}

fn task_box(source: &[u8], body: usize, end: usize) -> Option<usize> {
    if body + 3 > end || source[body] != b'[' || source[body + 2] != b']' {
        return None;
    }
    if !matches!(source[body + 1], b' ' | b'x' | b'X') {
        return None;
    }
    let box_end = body + 3;
    (box_end == end || matches!(source[box_end], b' ' | b'\t')).then_some(box_end)
}

fn is_table_rule(source: &[u8], body: usize, end: usize) -> bool {
    let mut dashes = false;
    for byte in &source[body..end] {
        match *byte {
            b'-' => dashes = true,
            b'|' | b':' | b' ' | b'\t' => {}
            _ => return false,
        }
    }
    dashes
}

fn span(faces: &mut [Face], start: usize, end: usize) -> &mut [Face] {
    let end = end.min(faces.len());
    &mut faces[start.min(end)..end]
}

fn paint(faces: &mut [Face], start: usize, end: usize, kind: Kind) {
    for face in span(faces, start, end) {
        face.kind = kind;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders one tag per source byte so a whole layout can be asserted at
    /// once.  Newlines pass through to keep multi-line cases readable.
    fn kinds(source: &str) -> String {
        faces(source.as_bytes())
            .into_iter()
            .zip(source.bytes())
            .map(|(face, byte)| match face.kind {
                _ if byte == b'\n' => '\n',
                Kind::Text => '.',
                Kind::Marker => '#',
                Kind::Heading(level) => char::from(b'0' + level),
                Kind::Code => 'c',
                Kind::Quote => 'q',
                Kind::List => 'l',
                Kind::Link => 'k',
                Kind::Url => 'u',
                Kind::Rule => 'r',
            })
            .collect()
    }

    /// The same, for the attributes that are carried alongside the kind.
    fn emphasis_of(source: &str) -> String {
        faces(source.as_bytes())
            .into_iter()
            .zip(source.bytes())
            .map(|(face, byte)| match face {
                _ if byte == b'\n' => '\n',
                Face {
                    strikethrough: true,
                    ..
                } => 's',
                Face {
                    bold: true,
                    italic: true,
                    ..
                } => 'x',
                Face { bold: true, .. } => 'b',
                Face { italic: true, .. } => 'i',
                _ => '.',
            })
            .collect()
    }

    #[test]
    fn atx_headings_color_by_level_and_drop_their_closing_run() {
        assert_eq!(kinds("# Title"), "##11111");
        assert_eq!(kinds("###### Deep"), "#######6666");
        assert_eq!(kinds("## Title ##"), "###22222.##");
        assert_eq!(emphasis_of("# Title"), "..bbbbb");
        // Seven hashes is not a heading, and a hash run needs a separator.
        assert_eq!(kinds("####### Deep"), "............");
        assert_eq!(kinds("#Title"), "......");
    }

    #[test]
    fn setext_underlines_promote_only_the_paragraph_line_above_them() {
        assert_eq!(kinds("Title\n=====\n"), "11111\n#####\n");
        assert_eq!(kinds("Title\n-----\n"), "22222\n#####\n");
        assert_eq!(emphasis_of("Title\n=====\n"), "bbbbb\n.....\n");
        // A dash run with no paragraph above it, or one that follows a list
        // item rather than prose, stays a thematic break.
        assert_eq!(kinds("\n-----\n"), "\nrrrrr\n");
        assert_eq!(kinds("- item\n-----\n"), "l.....\nrrrrr\n");
    }

    #[test]
    fn inline_emphasis_pairs_delimiters_and_spares_identifiers() {
        assert_eq!(kinds("*i* **b** ***x*** ~~s~~"), "#.#.##.##.###.###.##.##");
        assert_eq!(
            emphasis_of("*i* **b** ***x*** ~~s~~"),
            ".i....b......x......s.."
        );
        // Intraword underscores and unpaired runs are literal text.
        assert_eq!(emphasis_of("snake_case_word"), "...............");
        assert_eq!(emphasis_of("2 * 3 * 4"), ".........");
        assert_eq!(emphasis_of("an *unclosed run"), "................");
        assert_eq!(
            emphasis_of("**outer *inner* rest**"),
            "..bbbbbbbxxxxxbbbbbb.."
        );
    }

    #[test]
    fn code_spans_and_fences_suppress_the_formatting_inside_them() {
        assert_eq!(kinds("a `code` b"), "..#cccc#..");
        assert_eq!(kinds("``a ` b``"), "##ccccc##");
        assert_eq!(
            kinds("```rust\nlet x = *v*;\n```\n"),
            "###cccc\ncccccccccccc\n###\n"
        );
        assert_eq!(emphasis_of("`*not emphasis*`"), "................");
        // An unclosed span leaves the rest of the line alone.
        assert_eq!(kinds("a `code"), ".......");
    }

    #[test]
    fn links_separate_their_text_from_their_destination() {
        assert_eq!(kinds("[docs](http://a.b)"), "#kkkk##uuuuuuuuuu#");
        assert_eq!(kinds("![alt](x.png)"), "##kkk##uuuuu#");
        assert_eq!(kinds("[text][ref]"), "#kkkk##uuu#");
        assert_eq!(kinds("<http://a.b>"), "#kkkkkkkkkk#");
        assert_eq!(kinds("see https://a.b/c."), "....kkkkkkkkkkkkk.");
        assert_eq!(emphasis_of("[**bold**](x)"), "...bbbb......");
        // A bracket that never closes is ordinary text.
        assert_eq!(kinds("[open"), ".....");
    }

    #[test]
    fn block_structure_marks_lists_quotes_tables_and_breaks() {
        assert_eq!(
            kinds("- one\n2. two\n+ [x] done"),
            "l....\nll....\nl.#l#....."
        );
        assert_eq!(kinds("> a\n> > b"), "#.q\n#.#.q");
        assert_eq!(kinds("| a | b |\n|---|:-:|"), "#...#...#\n#########");
        assert_eq!(kinds("***\n"), "rrr\n");
        // A pipe inside prose is not table markup.
        assert_eq!(kinds("a | b"), ".....");
        // A bullet needs a separator; `*x*` is emphasis, not a list.
        assert_eq!(kinds("*x*"), "#.#");
    }

    #[test]
    fn front_matter_is_recognized_only_at_the_top_of_the_buffer() {
        assert_eq!(kinds("---\nkey: 1\n---\n# H"), "rrr\nrrrrrr\nrrr\n##1");
        assert_eq!(kinds("text\n\n---\n"), "....\n\nrrr\n");
    }

    #[test]
    fn every_byte_gets_exactly_one_face_however_malformed_the_input() {
        let cases: [&[u8]; 8] = [
            b"",
            b"\n\n\n",
            b"no trailing newline",
            b"\xff\x00\x1b[31m",
            b"[[[[[[[[[[]]]]]]]]]]",
            b"**********",
            b"``````",
            b"> # [a](b) `c` *d*",
        ];
        for case in cases {
            assert_eq!(faces(case).len(), case.len());
        }
    }
}
