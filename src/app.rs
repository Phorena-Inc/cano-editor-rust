use std::path::{Path, PathBuf};

/// Buffer rows one wheel notch moves, matching the usual terminal default.
const SCROLL_LINES: isize = 3;

use crate::autoformat::{self, Steps};
use crate::buffer::Highlight;
use crate::command::{Action, CommandState, ConfigVariable, ExternalEffect, key, lex, parse};
use crate::editor::{Editor, Leader, Mode, MoveDirection};
use crate::explorer::{Explorer, Selection};
use crate::history::UndoRecord;
use crate::io::load_buffer;
use crate::jump::{Kind, Target, targets};
use crate::listchars;
use crate::recent::Recent;
use crate::render::Viewport;
use crate::substitute::{self, Substitute};
use crate::terminal::{Input, Mouse, MouseKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppEffect {
    Save(PathBuf),
    Shell(Vec<u8>),
    Quit,
}

/// The one undo record a confirmed run accumulates.
///
/// A confirmation replaces matches one at a time, but it is still one
/// command, so the pieces are stitched into a single region rewrite as they
/// are accepted.  Old and new lengths are both tracked because they diverge
/// as soon as a replacement is a different length from what it replaced, and
/// the gaps between matches have to be read out of the buffer as it now
/// stands.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Rewrite {
    /// First byte changed, which is the same in both buffers.
    start: usize,
    /// The original bytes from `start` to the end of the last replacement.
    original: Vec<u8>,
    /// Length of that same span in the buffer as it now stands.
    current: usize,
}

/// A substitution part-way through its `c` confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Confirming {
    command: Substitute,
    /// Where the next match is, in the buffer as it stands now.
    at: usize,
    /// End of the command's range, moved along as replacements resize it.
    limit: usize,
    /// The single undo record being built up as answers come in.
    rewrite: Option<Rewrite>,
    replaced: usize,
    /// Distinct rows changed so far, so the report reads the same as the one
    /// a substitution without `c` produces.
    lines: usize,
    last_row: Option<usize>,
    /// Set by `a`, which stops asking and finishes the job.
    all: bool,
}

/// A full-pane list that replaces the buffer view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pane {
    Explorer,
    Recent,
}

/// Where an EasyMotion `s`/`t` motion has got to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Jump {
    /// The motion key was pressed; waiting for the character to search for.
    Character(Kind),
    /// Labels are on screen; waiting for the user to type one.  `typed` holds
    /// the prefix entered so far, which narrows the visible labels.
    Target {
        targets: Vec<Target>,
        typed: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub struct App {
    pub editor: Editor,
    pub commands: CommandState,
    pub prompt: Vec<u8>,
    pub prompt_cursor: usize,
    pub filename: PathBuf,
    pub explorer: Option<Explorer>,
    /// The most-recently-opened files, and whether Ctrl-R is showing them.
    pub recent: Recent,
    pub recent_open: bool,
    /// Where the recent list is persisted, when there is somewhere to put it.
    /// Tests and embedders that leave it unset simply keep the list in memory.
    pub recent_path: Option<PathBuf>,
    pub count: Vec<u8>,
    /// Renders the buffer as formatted markdown; toggled with Ctrl-M and
    /// preset from the file's extension.
    pub markdown: bool,
    /// The in-flight `s`/`t` motion, if one is collecting keys.
    pub jump: Option<Jump>,
    /// The pattern being highlighted, empty once `:nohl` clears it.
    pub highlight: Highlight,
    /// Set by the space leader; the next key selects the leader command.
    leader_pending: bool,
    /// Where the mouse was pressed, until a drag turns it into a selection.
    drag_anchor: Option<usize>,
    /// True while a drag that began on the scrollbar is still in progress, so
    /// it keeps scrolling even when the pointer strays off the column.
    scrollbar_drag: bool,
    /// The `:s///c` confirmation waiting for an answer.
    pub confirming: Option<Confirming>,
    /// Keys typed in Insert mode that may still complete an `:imap`.
    insert_pending: Vec<u8>,
    /// The pane Ctrl-N or Ctrl-R asked for while the buffer was unsaved; the
    /// prompt is waiting for an answer.
    pub save_prompt: Option<Pane>,
    /// The pane to open once a save the prompt asked for has been written.
    pending_pane: Option<Pane>,
    /// What the last frame drew.  Jump targets are limited to what the user
    /// can actually see, the way EasyMotion works, and a mouse position is
    /// meaningless without the geometry it was made against, so the renderer
    /// records both here.  The wheel writes back to it.
    pub viewport: Viewport,
    /// True until the renderer has reported a frame, while the whole buffer
    /// counts as visible rather than none of it.
    unrendered: bool,
    pub pending_replace: bool,
    pub saved: bool,
    pub message_pending: bool,
    /// Refuses writes; used for the built-in help pages so a save-and-quit
    /// cannot overwrite the installed documentation.
    pub readonly: bool,
    /// The needle used by `n`.  Kept separately from the prompt, which is
    /// cleared whenever a `:` command or Escape resets the prompt line.
    last_search: Vec<u8>,
    /// `*` repeats whole-word, `/` repeats by substring.
    last_search_word: bool,
    saved_buffer: Vec<u8>,
    buffer_replaced: bool,
}

impl App {
    pub fn new(bytes: Vec<u8>, filename: PathBuf) -> Self {
        let saved_buffer = bytes.clone();
        let markdown = is_markdown(&filename);
        Self {
            editor: Editor::new(bytes),
            commands: CommandState::new(Vec::new()),
            prompt: Vec::new(),
            prompt_cursor: 0,
            filename,
            explorer: None,
            recent: Recent::default(),
            recent_open: false,
            recent_path: None,
            count: Vec::new(),
            markdown,
            jump: None,
            highlight: Highlight::default(),
            leader_pending: false,
            drag_anchor: None,
            scrollbar_drag: false,
            confirming: None,
            insert_pending: Vec::new(),
            save_prompt: None,
            pending_pane: None,
            viewport: Viewport::default(),
            unrendered: true,
            pending_replace: false,
            saved: true,
            message_pending: false,
            readonly: false,
            last_search: Vec::new(),
            last_search_word: false,
            saved_buffer,
            buffer_replaced: false,
        }
    }

    pub fn mark_saved(&mut self) {
        self.saved_buffer.clone_from(&self.editor.buffer.data);
        self.saved = true;
    }

    pub fn handle(&mut self, input: Input) -> Vec<AppEffect> {
        self.buffer_replaced = false;
        let effects = self.handle_mapped(input, 0);
        if self.buffer_replaced {
            self.saved_buffer.clone_from(&self.editor.buffer.data);
        }
        self.saved = self.editor.buffer.data == self.saved_buffer;
        effects
    }

    fn handle_mapped(&mut self, input: Input, depth: usize) -> Vec<AppEffect> {
        if self.pending_replace {
            self.pending_replace = false;
            // Escape, arrows, and other non-character keys cancel the pending
            // replacement instead of writing raw control bytes into the file.
            match input {
                Input::Byte(byte) => {
                    self.editor.replace_next(byte);
                }
                Input::Enter => {
                    self.editor.replace_next(b'\n');
                }
                _ => {}
            }
            return Vec::new();
        }
        if input == Input::Control(17) {
            if self.saved {
                return vec![AppEffect::Quit];
            }
            self.set_message("No write since last change (add ! to override)");
            return Vec::new();
        }

        // The mouse is not a key: it neither answers a prompt nor completes a
        // mapping, so it is handled before either can consume it.
        if let Input::Mouse(mouse) = input {
            self.mouse_input(mouse);
            return Vec::new();
        }

        // A substitution asking about a match is a question too, and the same
        // rule applies: it takes the next key before anything can reinterpret
        // it.
        if self.confirming.is_some() {
            self.confirm_input(input);
            return Vec::new();
        }

        // The save prompt is a question, so it takes the next key before
        // anything else can interpret it.
        if let Some(pane) = self.save_prompt {
            return self.save_prompt_input(input, pane);
        }

        // A pending `s`/`t` motion owns the keyboard until it lands or is
        // cancelled, in Normal and Visual mode alike.  It sits ahead of key
        // mapping because a mapping firing here would move the cursor out
        // from under labels that are still on screen.
        if self.jump.is_some() {
            self.jump_input(input);
            return Vec::new();
        }

        if self.editor.mode == Mode::Normal
            && let Some(key) = mapping_key(input)
            && let Some(expansion) = self.commands.mapping(key).map(|bytes| bytes.to_vec())
        {
            if depth >= 64 {
                self.set_message("Recursive key map");
                return Vec::new();
            }
            return self.replay(&expansion, depth);
        }

        match self.editor.mode {
            Mode::Normal => self.normal_input(input),
            Mode::Insert => self.insert_input(input, depth),
            Mode::Visual => self.visual_input(input),
            Mode::Search => self.search_input(input),
            Mode::Command => self.command_input(input),
        }
    }

    fn normal_input(&mut self, input: Input) -> Vec<AppEffect> {
        if input == Input::Control(14) {
            self.toggle_pane(Pane::Explorer);
            self.editor.leader = Leader::None;
            return Vec::new();
        }

        if input == Input::Control(18) {
            self.toggle_pane(Pane::Recent);
            self.editor.leader = Leader::None;
            return Vec::new();
        }

        // The recent picker owns the keyboard the same way the explorer does,
        // so keys cannot edit the buffer hidden behind it.
        if self.recent_open {
            match input {
                Input::Byte(b'j') | Input::Down => self.recent.move_down(),
                Input::Byte(b'k') | Input::Up => self.recent.move_up(),
                Input::Enter => self.enter_recent(),
                Input::Escape | Input::Control(3) => self.recent_open = false,
                _ => {}
            }
            self.editor.leader = Leader::None;
            return Vec::new();
        }

        // While the explorer is open every key belongs to it; letting other
        // keys fall through would silently edit the buffer hidden behind it.
        if let Some(explorer) = self.explorer.as_mut() {
            match input {
                Input::Byte(b'j') | Input::Down => {
                    explorer.move_down();
                    self.editor.normal_key(b'j');
                }
                Input::Byte(b'k') | Input::Up => {
                    explorer.move_up();
                    self.editor.normal_key(b'k');
                }
                Input::Enter => {
                    self.enter_explorer();
                    self.editor.leader = Leader::None;
                }
                Input::Escape | Input::Control(3) => {
                    self.explorer = None;
                    self.editor.leader = Leader::None;
                }
                _ => {}
            }
            return Vec::new();
        }

        // The space leader takes exactly the next key, the way a mapping with
        // a leader prefix does; an unbound key just cancels it.
        if self.leader_pending {
            self.leader_pending = false;
            match input {
                Input::Byte(b'i') => self.search_word_under_cursor(),
                Input::Byte(b'o') => self.highlight = Highlight::default(),
                Input::Byte(b'n') => self.toggle_pane(Pane::Explorer),
                Input::Byte(b'r') => self.toggle_pane(Pane::Recent),
                Input::Byte(b'l') => {
                    let shown = i64::from(self.commands.list == 0);
                    self.assign(ConfigVariable::List, shown);
                }
                Input::Byte(b'f') => self.autoformat(),
                _ => {}
            }
            return Vec::new();
        }

        // A terminal without the keyboard disambiguation extension sends the
        // same byte for Ctrl-M and Enter, so both toggle markdown display.
        // Enter is otherwise unbound in Normal mode, so nothing is lost.
        if matches!(input, Input::Control(13) | Input::Enter) {
            self.toggle_markdown();
            self.editor.leader = Leader::None;
            return Vec::new();
        }

        if let Input::Byte(byte) = input {
            if byte.is_ascii_digit() && !(byte == b'0' && self.count.is_empty()) {
                self.count.push(byte);
                return Vec::new();
            }
            if !self.count.is_empty() {
                let repetitions = self.count.iter().fold(0usize, |value, digit| {
                    value
                        .saturating_mul(10)
                        .saturating_add(usize::from(digit - b'0'))
                });
                self.count.clear();
                if byte == b'd' {
                    self.editor.delete_rows(repetitions);
                    return Vec::new();
                }
                if byte == b'g' && self.editor.leader != Leader::Delete {
                    self.editor.buffer.move_file_start(repetitions);
                    self.editor.leader = Leader::None;
                    return Vec::new();
                }
                if byte == b'G' && self.editor.leader != Leader::Delete {
                    self.editor.buffer.move_file_end(repetitions);
                    self.editor.leader = Leader::None;
                    return Vec::new();
                }
                for _ in 0..repetitions {
                    self.dispatch_repeated(byte);
                }
                return Vec::new();
            }
        }

        match input {
            Input::Byte(b':') => self.enter_prompt(Mode::Command),
            Input::Byte(b'/') => self.enter_prompt(Mode::Search),
            Input::Byte(b'n') => {
                self.repeat_search();
                self.editor.leader = Leader::None;
            }
            Input::Byte(b'N') => {
                self.repeat_search_back();
                self.editor.leader = Leader::None;
            }
            Input::Byte(b'u') => {
                self.undo_with_report();
                self.editor.leader = Leader::None;
            }
            Input::Byte(b'U') => {
                self.redo_with_report();
                self.editor.leader = Leader::None;
            }
            Input::Byte(b'r') => {
                self.pending_replace = true;
                self.editor.leader = Leader::None;
            }
            // `let mapleader = " "`: space arms the leader, and `<leader>i` /
            // `<leader>o` are `*` and `:nohl`.
            Input::Byte(b' ') if self.editor.leader == Leader::None => {
                self.leader_pending = true;
            }
            // Vim's `*`, which `<leader>i` maps to.
            Input::Byte(b'*') if self.editor.leader == Leader::None => {
                self.search_word_under_cursor();
            }
            // EasyMotion's two find motions. Both keys are otherwise unbound
            // in Cano, and `d`/`y` still take their own motions, so starting
            // a jump only after the leader is clear keeps `dt` free to mean
            // something later.
            Input::Byte(b's') if self.editor.leader == Leader::None => {
                self.jump = Some(Jump::Character(Kind::Find));
            }
            Input::Byte(b't') if self.editor.leader == Leader::None => {
                self.jump = Some(Jump::Character(Kind::Till));
            }
            Input::Byte(byte) => {
                self.editor.normal_key(byte);
            }
            Input::Left => {
                self.editor.normal_key(b'h');
            }
            Input::Right => {
                self.editor.normal_key(b'l');
            }
            Input::Up => {
                self.editor.normal_key(b'k');
            }
            Input::Down => {
                self.editor.normal_key(b'j');
            }
            Input::Control(19) => {
                return vec![AppEffect::Save(self.output_path()), AppEffect::Quit];
            }
            Input::Control(15) => self.editor.open_line_normal(),
            Input::Control(3) | Input::Escape => {
                self.count.clear();
                self.prompt.clear();
                self.editor.leader = Leader::None;
            }
            _ => self.editor.leader = Leader::None,
        }
        Vec::new()
    }

    fn dispatch_repeated(&mut self, byte: u8) {
        match byte {
            b'n' => self.repeat_search(),
            b'N' => self.repeat_search_back(),
            b'u' => self.undo_with_report(),
            b'U' => self.redo_with_report(),
            _ => {
                self.editor.normal_key(byte);
            }
        }
    }

    /// Repeats the last completed search.  An empty needle would make
    /// `search_wrapped` walk the cursor forward one byte per press.
    fn repeat_search(&mut self) {
        if self.last_search.is_empty() {
            self.set_message("No previous search");
            return;
        }
        if self.last_search_word {
            self.move_to_next_match();
        } else {
            self.editor.buffer.cursor = self.editor.buffer.search_wrapped(&self.last_search);
        }
    }

    /// Vim's `*`, which `<leader>i` maps to: take the word under the cursor,
    /// highlight every whole-word occurrence, and move to the next one.
    pub fn search_word_under_cursor(&mut self) {
        let Some((start, end)) = self.editor.buffer.word_at(self.editor.buffer.cursor) else {
            self.set_message("No word under the cursor");
            return;
        };
        let needle = self.editor.buffer.data[start..end].to_vec();
        self.last_search.clone_from(&needle);
        self.last_search_word = true;
        self.highlight = Highlight {
            needle,
            whole_word: true,
        };
        self.move_to_next_match();
    }

    /// Repeats the last search backwards, which is vim's `N`.
    fn repeat_search_back(&mut self) {
        if self.last_search.is_empty() {
            self.set_message("No previous search");
            return;
        }
        self.move_to_previous_match();
    }

    /// The pattern `n` and `N` repeat, which remembers whether it came from
    /// `*` or from `/`.
    fn search_pattern(&self) -> Highlight {
        Highlight {
            needle: self.last_search.clone(),
            whole_word: self.last_search_word,
        }
    }

    /// Moves to the next occurrence of the last search, wrapping at EOF.
    ///
    /// The scan starts one byte past the cursor so a match the cursor is
    /// already sitting on does not count as the next one.
    fn move_to_next_match(&mut self) {
        let pattern = self.search_pattern();
        let from = self.editor.buffer.cursor.saturating_add(1);
        let data = &self.editor.buffer.data;
        if let Some(at) = pattern.find(data, from).or_else(|| pattern.find(data, 0)) {
            self.editor.buffer.cursor = at;
        }
    }

    /// Moves to the previous occurrence, wrapping around to the last one.
    ///
    /// The scan stops before the cursor for the same reason the forward scan
    /// starts after it: a match under the cursor is where you already are.
    fn move_to_previous_match(&mut self) {
        let pattern = self.search_pattern();
        let cursor = self.editor.buffer.cursor;
        let data = &self.editor.buffer.data;
        let wrapped = || pattern.rfind(data, data.len().saturating_add(1));
        if let Some(at) = pattern.rfind(data, cursor).or_else(wrapped) {
            self.editor.buffer.cursor = at;
        }
    }

    fn undo_with_report(&mut self) {
        if let Err(error) = self.editor.undo() {
            self.set_message(format!("Undo failed: {error}"));
        }
    }

    fn redo_with_report(&mut self) {
        if let Err(error) = self.editor.redo() {
            self.set_message(format!("Redo failed: {error}"));
        }
    }

    /// Replays a mapping's right-hand side through the ordinary input path.
    ///
    /// Bytes are translated back into the key variants the mode handlers
    /// expect, and the legacy NUL terminator is dropped: executing it would
    /// insert a literal 0x00.
    fn replay(&mut self, expansion: &[u8], depth: usize) -> Vec<AppEffect> {
        let mut effects = Vec::new();
        for byte in expansion {
            let step = match byte {
                0 => continue,
                10 | 13 => Input::Enter,
                27 => Input::Escape,
                127 => Input::Backspace,
                byte => Input::Byte(*byte),
            };
            effects.extend(self.handle_mapped(step, depth + 1));
        }
        effects
    }

    /// Inserts one byte, applying an `:imap` when the keys just typed finish
    /// one.
    ///
    /// The earlier keys of a multi-key mapping are already in the buffer,
    /// because they were ordinary insertions until the last one arrived; they
    /// are taken back out before the right-hand side is replayed.
    fn insert_byte_mapped(&mut self, byte: u8, depth: usize) -> Vec<AppEffect> {
        self.insert_pending.push(byte);
        // Keep only the longest tail that could still complete a mapping, so
        // a run that failed to match does not block one starting inside it.
        while !self.insert_pending.is_empty() && !self.commands.insert_prefix(&self.insert_pending)
        {
            self.insert_pending.remove(0);
        }
        let Some(mapping) = self.commands.insert_map(&self.insert_pending) else {
            self.editor.insert_byte(byte);
            return Vec::new();
        };
        let typed = mapping.from.len();
        let replacement = mapping.to.clone();
        self.insert_pending.clear();

        if depth >= 64 {
            self.set_message("Recursive key map");
            self.editor.insert_byte(byte);
            return Vec::new();
        }
        for _ in 0..typed.saturating_sub(1) {
            self.editor.insert_backspace();
        }
        self.replay(&replacement, depth)
    }

    fn insert_input(&mut self, input: Input, depth: usize) -> Vec<AppEffect> {
        // Only an uninterrupted run of typed bytes can complete a mapping;
        // anything else moves the cursor or leaves the mode, and the keys
        // before it are no longer adjacent to the ones after.
        if !matches!(input, Input::Byte(_)) {
            self.insert_pending.clear();
        }
        match input {
            Input::Escape | Input::Control(3) => {
                self.editor.leave_insert();
            }
            Input::Backspace => {
                self.editor.insert_backspace();
            }
            Input::Enter => {
                self.editor.insert_newline();
            }
            Input::Byte(b'\t') => {
                self.insert_pending.clear();
                self.editor.insert_tab();
            }
            Input::Byte(byte) => {
                return self.insert_byte_mapped(byte, depth);
            }
            Input::Left => {
                self.editor.insert_move(MoveDirection::Left);
            }
            Input::Right => {
                self.editor.insert_move(MoveDirection::Right);
            }
            Input::Up => {
                self.editor.insert_move(MoveDirection::Up);
            }
            Input::Down => {
                self.editor.insert_move(MoveDirection::Down);
            }
            Input::Control(19) => {
                return vec![AppEffect::Save(self.output_path()), AppEffect::Quit];
            }
            _ => {}
        }
        Vec::new()
    }

    fn visual_input(&mut self, input: Input) -> Vec<AppEffect> {
        match input {
            Input::Escape | Input::Control(3) => {
                self.editor.visual_key(27);
            }
            // EasyMotion's `s` is a motion, so in Visual mode it extends the
            // selection to the label instead of moving a bare cursor.
            Input::Byte(b's') => {
                self.jump = Some(Jump::Character(Kind::Find));
            }
            // `=` re-indents a selection in vim, and sits beside `>` and `<`
            // here for the same reason: all three rewrite whole lines.
            Input::Byte(b'=') => {
                if let Some(region) = self.editor.visual_rows() {
                    self.editor.visual_key(27);
                    self.autoformat_region(region);
                }
            }
            Input::Byte(byte) => {
                self.editor.visual_key(byte);
            }
            Input::Left => {
                self.editor.visual_key(b'h');
            }
            Input::Right => {
                self.editor.visual_key(b'l');
            }
            Input::Up => {
                self.editor.visual_key(b'k');
            }
            Input::Down => {
                self.editor.visual_key(b'j');
            }
            _ => {}
        }
        Vec::new()
    }

    fn command_input(&mut self, input: Input) -> Vec<AppEffect> {
        match input {
            Input::Escape | Input::Control(3) => {
                self.clear_prompt();
                self.editor.mode = Mode::Normal;
            }
            Input::Backspace => self.prompt_backspace(),
            Input::Left => self.prompt_cursor = self.prompt_cursor.saturating_sub(1),
            Input::Right => {
                self.prompt_cursor = (self.prompt_cursor + 1).min(self.prompt.len());
            }
            Input::Byte(byte) => self.prompt_insert(byte),
            Input::Enter => return self.execute_command(),
            _ => {}
        }
        Vec::new()
    }

    fn search_input(&mut self, input: Input) -> Vec<AppEffect> {
        match input {
            Input::Escape | Input::Control(3) => {
                self.clear_prompt();
                self.editor.mode = Mode::Normal;
            }
            Input::Backspace => self.prompt_backspace(),
            Input::Left => self.prompt_cursor = self.prompt_cursor.saturating_sub(1),
            Input::Right => {
                self.prompt_cursor = (self.prompt_cursor + 1).min(self.prompt.len());
            }
            Input::Byte(byte) => self.prompt_insert(byte),
            Input::Enter => {
                let mut destination = self.editor.buffer.search_wrapped(&self.prompt);
                let mut needle = self.prompt.clone();
                if let Some(replacement) = self.prompt.strip_prefix(b"s/") {
                    let mut parts = replacement.split(|byte| *byte == b'/');
                    if let (Some(old), Some(new)) = (parts.next(), parts.next()) {
                        destination = self.editor.buffer.search_wrapped(old);
                        needle = old.to_vec();
                        self.editor.buffer.replace_first_after_cursor(old, new);
                    }
                }
                if !needle.is_empty() {
                    // A `/` search highlights the same way vim's `hlsearch`
                    // does, which is what `<leader>o` exists to switch off.
                    self.last_search.clone_from(&needle);
                    self.last_search_word = false;
                    self.highlight = Highlight {
                        needle,
                        whole_word: false,
                    };
                }
                self.editor.buffer.cursor = destination;
                self.editor.mode = Mode::Normal;
            }
            _ => {}
        }
        Vec::new()
    }

    fn execute_command(&mut self) -> Vec<AppEffect> {
        self.editor.mode = Mode::Normal;
        // A substitution is delimiter-structured rather than
        // whitespace-structured, so it is recognized before the token lexer
        // can tear `:%s/two words/one/g` into pieces.
        if let Some(parsed) = substitute::parse(&self.prompt) {
            match parsed {
                Ok(command) => self.begin_substitute(command),
                Err(error) => self.set_message(error.to_string()),
            }
            self.clear_prompt();
            return Vec::new();
        }
        // `:set` is vim's spelling and takes values a token lexer would tear
        // apart -- `listchars=tab:>\ ,trail:.` has a space inside one
        // argument -- so it is read straight off the command line too. The
        // trailing space keeps `set-var` for the token language.
        if let Some(options) = self.prompt.strip_prefix(b"set ") {
            let options = options.to_vec();
            self.set_options(&options);
            self.clear_prompt();
            return Vec::new();
        }
        if let Some(command) = self.prompt.strip_prefix(b"!") {
            let effect = AppEffect::Shell(command.to_vec());
            self.clear_prompt();
            return vec![effect];
        }

        let effects = match lex(&self.prompt).and_then(|tokens| parse(&tokens)) {
            // `e` (exit) quits without writing, so it gets the same unsaved
            // guard as `q`; `q!` remains the explicit override.
            Ok(action)
                if matches!(&action, Action::Quit { force: false } | Action::Exit)
                    && !self.saved =>
            {
                self.set_message("No write since last change (add ! to override)");
                Vec::new()
            }
            Ok(action) => match self.commands.apply(action) {
                Ok(Some(ExternalEffect::Save(output))) => {
                    let path = if output.is_empty() {
                        self.filename.clone()
                    } else {
                        bytes_to_path(&output)
                    };
                    if self.commands.message.is_some() {
                        self.message_pending = true;
                    }
                    let mut effects = vec![AppEffect::Save(path)];
                    if self.commands.quit {
                        effects.push(AppEffect::Quit);
                    }
                    effects
                }
                Ok(Some(ExternalEffect::AutoFormat)) => {
                    self.autoformat();
                    self.commands
                        .quit
                        .then_some(AppEffect::Quit)
                        .into_iter()
                        .collect()
                }
                Ok(Some(ExternalEffect::ClearHighlight)) => {
                    self.highlight = Highlight::default();
                    if self.commands.message.is_some() {
                        self.message_pending = true;
                    }
                    self.commands
                        .quit
                        .then_some(AppEffect::Quit)
                        .into_iter()
                        .collect()
                }
                Ok(None) => {
                    if self.commands.message.is_some() {
                        self.message_pending = true;
                    }
                    self.commands
                        .quit
                        .then_some(AppEffect::Quit)
                        .into_iter()
                        .collect()
                }
                Err(error) => {
                    self.set_message(error.to_string());
                    Vec::new()
                }
            },
            Err(error) => {
                self.set_message(error.to_string());
                Vec::new()
            }
        };
        self.editor.indent = self.commands.indent.max(0) as usize;
        self.clear_prompt();
        effects
    }

    fn enter_prompt(&mut self, mode: Mode) {
        self.clear_prompt();
        self.editor.mode = mode;
        self.editor.leader = Leader::None;
    }

    fn clear_prompt(&mut self) {
        self.prompt.clear();
        self.prompt_cursor = 0;
    }

    fn prompt_insert(&mut self, byte: u8) {
        self.prompt.insert(self.prompt_cursor, byte);
        self.prompt_cursor += 1;
    }

    fn prompt_backspace(&mut self) {
        if self.prompt_cursor > 0 {
            self.prompt_cursor -= 1;
            self.prompt.remove(self.prompt_cursor);
        }
    }

    fn output_path(&self) -> PathBuf {
        if self.commands.output.is_empty() {
            self.filename.clone()
        } else {
            bytes_to_path(&self.commands.output)
        }
    }

    /// Runs one command line as if it had been typed at the `:` prompt.
    ///
    /// Startup configuration goes through here, so anything spelled as a
    /// command -- `set`, `imap`, `set-map`, `nohl` -- can be written in
    /// `init.lua` without each one needing a configuration slot of its own.
    /// A leading `:` is accepted, because that is how the line reads in vim.
    pub fn run_command(&mut self, line: &[u8]) -> Vec<AppEffect> {
        self.clear_prompt();
        self.prompt
            .extend_from_slice(line.strip_prefix(b":").unwrap_or(line));
        self.prompt_cursor = self.prompt.len();
        let effects = self.execute_command();
        self.clear_prompt();
        effects
    }

    /// Applies a `:set` line, which may carry several options at once.
    fn set_options(&mut self, spec: &[u8]) {
        for option in split_options(spec) {
            if let Err(error) = self.set_option(&option) {
                self.set_message(error);
                return;
            }
        }
        self.editor.indent = self.commands.indent.max(0) as usize;
    }

    /// Applies one `[no]name[!][=value]` option, or reports one with `name?`.
    fn set_option(&mut self, option: &[u8]) -> Result<(), String> {
        if let Some(name) = option.strip_suffix(b"?") {
            let shown = if matches!(name, b"listchars" | b"lcs") {
                format!("listchars={}", self.commands.listchars)
            } else {
                let variable = ConfigVariable::parse(name).ok_or_else(|| unknown(option))?;
                format!(
                    "{}={}",
                    String::from_utf8_lossy(name),
                    self.commands.variable(variable)
                )
            };
            self.set_message(shown);
            return Ok(());
        }
        if let Some(split) = option.iter().position(|byte| *byte == b'=') {
            let (name, value) = option.split_at(split);
            let value = &value[1..];
            if matches!(name, b"listchars" | b"lcs") {
                self.commands.listchars =
                    listchars::parse(value).map_err(|error| error.to_string())?;
                return Ok(());
            }
            let variable = named(name)?;
            let text = std::str::from_utf8(value).map_err(|_| unknown(name))?;
            let number = text.parse::<i64>().map_err(|_| {
                format!(
                    "Invalid value for {}: {text}",
                    String::from_utf8_lossy(name)
                )
            })?;
            self.assign(variable, number);
            return Ok(());
        }

        // `name!` toggles, `noname` clears, and a bare name sets.
        let (name, toggle) = match option.strip_suffix(b"!") {
            Some(name) => (name, true),
            None => (option, false),
        };
        let (name, off) = match name.strip_prefix(b"no") {
            // `nobackup` is the negation, but an option whose own name starts
            // with `no` would be shadowed; the full name wins.
            Some(rest) if ConfigVariable::parse(name).is_none() => (rest, true),
            _ => (name, false),
        };
        // Report the option as it was typed: `nosuchoption` is not a request
        // to unset something called `suchoption`.
        let variable = ConfigVariable::parse(name).ok_or_else(|| unknown(option))?;
        let value = if toggle {
            i64::from(self.commands.variable(variable) == 0)
        } else {
            i64::from(!off)
        };
        self.assign(variable, value);
        Ok(())
    }

    fn assign(&mut self, variable: ConfigVariable, value: i64) {
        // `apply` is the one place a variable is written, so `:set` and
        // `:set-var` cannot drift apart.
        let _ = self.commands.apply(Action::SetVar { variable, value });
    }

    /// Rewrites the buffer's whitespace, as vim-autoformat's fallback does.
    ///
    /// The whole rewrite is one undo step, and the record is narrowed to the
    /// span that actually changed so reformatting a file does not put a copy
    /// of all of it on the undo stack.
    pub fn autoformat(&mut self) {
        let region = (0, self.editor.buffer.data.len());
        self.autoformat_region(region);
    }

    /// Formats only the lines `region` touches.
    fn autoformat_region(&mut self, region: (usize, usize)) {
        if self.readonly {
            self.set_message("Buffer is read-only");
            return;
        }
        let steps = Steps {
            autoindent: self.commands.autoformat_autoindent != 0,
            retab: self.commands.autoformat_retab != 0,
            remove_trailing_spaces: self.commands.autoformat_remove_trailing_spaces != 0,
        };
        let indent = self.commands.indent.max(0) as usize;
        let before = &self.editor.buffer.data;
        let Some(after) = autoformat::format(before, region, steps, indent) else {
            self.set_message("Already formatted");
            return;
        };
        let lines = autoformat::changed_lines(before, &after);

        let (start, old_end, new_end) = changed_span(before, &after);
        let original = before[start..old_end].to_vec();
        // The cursor is put back on the line it was on, since the byte it was
        // on has almost certainly moved.
        let row = self.editor.buffer.cursor_row().unwrap_or(0);
        if self
            .editor
            .buffer
            .replace_region(start, old_end, &after[start..new_end])
            .is_some()
        {
            self.editor
                .history
                .push_undo(UndoRecord::replace_region(start, new_end, original));
        }
        let line = self
            .editor
            .buffer
            .rows
            .get(row)
            .or_else(|| self.editor.buffer.rows.last());
        self.editor.buffer.cursor = line.map_or(0, |row| row.start);
        let many = if lines == 1 { "" } else { "s" };
        self.set_message(format!("Formatted {lines} line{many}"));
    }

    /// Runs a substitution, or opens the `c` confirmation for it.
    fn begin_substitute(&mut self, command: Substitute) {
        if self.readonly {
            self.set_message("Buffer is read-only");
            return;
        }
        let cursor_row = self.editor.buffer.cursor_row().unwrap_or(0);
        let Some((start, limit)) = command.resolve(&self.editor.buffer, cursor_row) else {
            self.set_message("Pattern not found");
            return;
        };

        if command.flags.confirm {
            self.confirming = Some(Confirming {
                command,
                at: start,
                limit,
                rewrite: None,
                replaced: 0,
                lines: 0,
                last_row: None,
                all: false,
            });
            self.advance_confirm();
            return;
        }

        // Matches are collected before anything moves, then applied back to
        // front so the earlier offsets are still the ones they were found at.
        let matches = command.matches(&self.editor.buffer, (start, limit));
        if matches.is_empty() {
            self.set_message(format!(
                "Pattern not found: {}",
                String::from_utf8_lossy(&self.prompt)
            ));
            return;
        }
        let lines = self.rows_touched(&matches);
        let first = matches[0];
        let last_end = matches[matches.len() - 1] + command.pattern.len();
        let original = self.editor.buffer.data[first..last_end].to_vec();

        for at in matches.iter().rev() {
            command.apply_one(&mut self.editor.buffer, *at);
        }
        let grew = isize::try_from(command.replacement.len())
            .unwrap_or(0)
            .saturating_sub(isize::try_from(command.pattern.len()).unwrap_or(0))
            .saturating_mul(isize::try_from(matches.len()).unwrap_or(0));
        let new_end = last_end.saturating_add_signed(grew);
        self.editor
            .history
            .push_undo(UndoRecord::replace_region(first, new_end, original));
        self.editor.buffer.cursor = first.min(self.editor.buffer.data.len());
        self.report_substitutions(matches.len(), lines);
    }

    /// How many distinct rows a set of match offsets falls on.
    fn rows_touched(&self, matches: &[usize]) -> usize {
        let mut rows = 0;
        let mut last = None;
        for at in matches {
            let row = self.editor.buffer.row_for_index(*at);
            if row != last {
                rows += 1;
                last = row;
            }
        }
        rows
    }

    fn report_substitutions(&mut self, count: usize, lines: usize) {
        let many = if count == 1 { "" } else { "s" };
        let rows = if lines == 1 { "" } else { "s" };
        self.set_message(format!("{count} substitution{many} on {lines} line{rows}"));
    }

    /// Moves to the next match the confirmation should ask about, finishing
    /// the run when there is none left.
    fn advance_confirm(&mut self) {
        loop {
            let Some(state) = self.confirming.as_mut() else {
                return;
            };
            let Some(at) =
                state
                    .command
                    .pattern
                    .find(&self.editor.buffer.data, state.at, state.limit)
            else {
                self.finish_confirm();
                return;
            };
            state.at = at;
            self.editor.buffer.cursor = at;
            if !state.all {
                return;
            }
            // `a` answered for every remaining match, so keep going without
            // asking again.
            self.replace_confirmed();
        }
    }

    /// Applies the match the confirmation is sitting on.
    fn replace_confirmed(&mut self) {
        let Some(state) = self.confirming.as_mut() else {
            return;
        };
        let at = state.at;
        let row = self.editor.buffer.row_for_index(at);
        if row != state.last_row {
            state.lines += 1;
            state.last_row = row;
        }
        let end = at.saturating_add(state.command.pattern.len());
        let original = self.editor.buffer.data[at..end].to_vec();
        let Some(delta) = state.command.apply_one(&mut self.editor.buffer, at) else {
            return;
        };
        state.replaced += 1;
        state.limit = state.limit.saturating_add_signed(delta);
        // The record grows to cover everything touched so far, so the whole
        // confirmed run comes back with one press of `u`.  The untouched gap
        // since the last replacement reads the same in either buffer, but it
        // has to be located in the current one.
        let replacement = state.command.replacement.len();
        match &mut state.rewrite {
            Some(rewrite) => {
                let gap_start = rewrite.start.saturating_add(rewrite.current);
                let gap = self
                    .editor
                    .buffer
                    .data
                    .get(gap_start..at)
                    .unwrap_or_default()
                    .to_vec();
                rewrite.original.extend_from_slice(&gap);
                rewrite.original.extend_from_slice(&original);
                rewrite.current = rewrite
                    .current
                    .saturating_add(gap.len())
                    .saturating_add(replacement);
            }
            None => {
                state.rewrite = Some(Rewrite {
                    start: at,
                    original,
                    current: replacement,
                });
            }
        }
        state.at = at.saturating_add(state.command.replacement.len());
        if !state.command.flags.global {
            // Without `g` the rest of this line is not eligible.
            let row_end = self
                .editor
                .buffer
                .row_for_index(state.at)
                .and_then(|row| self.editor.buffer.rows.get(row))
                .map_or(state.at, |row| row.end);
            state.at = state.at.max(row_end);
        }
    }

    /// Ends the confirmation, recording it as one undoable step.
    fn finish_confirm(&mut self) {
        let Some(state) = self.confirming.take() else {
            return;
        };
        if let Some(rewrite) = state.rewrite {
            let end = rewrite
                .start
                .saturating_add(rewrite.current)
                .min(self.editor.buffer.data.len());
            self.editor.history.push_undo(UndoRecord::replace_region(
                rewrite.start,
                end,
                rewrite.original,
            ));
        }
        if state.replaced == 0 {
            self.set_message("No substitutions");
            return;
        }
        self.report_substitutions(state.replaced, state.lines);
    }

    /// Answers the `c` confirmation for the match under the cursor.
    fn confirm_input(&mut self, input: Input) {
        match input {
            Input::Byte(b'y' | b'Y') => {
                self.replace_confirmed();
                self.advance_confirm();
            }
            Input::Byte(b'n' | b'N') => {
                if let Some(state) = self.confirming.as_mut() {
                    state.at = state.at.saturating_add(1);
                }
                self.advance_confirm();
            }
            Input::Byte(b'a' | b'A') => {
                if let Some(state) = self.confirming.as_mut() {
                    state.all = true;
                }
                self.replace_confirmed();
                self.advance_confirm();
            }
            // `q` stops, and so does anything that is plainly not an answer.
            Input::Byte(b'q' | b'Q') | Input::Escape | Input::Control(3) => self.finish_confirm(),
            _ => {}
        }
    }

    /// Advances the in-flight `s`/`t` motion by one key.
    ///
    /// Escape cancels at any point, and so does any key that cannot continue
    /// the motion: leaving stale labels on screen after an unrelated keypress
    /// would be worse than starting over.
    fn jump_input(&mut self, input: Input) {
        let Some(jump) = self.jump.take() else {
            return;
        };
        if matches!(input, Input::Escape | Input::Control(3)) {
            return;
        }
        let Input::Byte(byte) = input else {
            return;
        };

        match jump {
            Jump::Character(kind) => {
                let found = targets(
                    &self.editor.buffer.data,
                    self.visible_range(),
                    self.editor.buffer.cursor,
                    kind,
                    byte,
                );
                match found.len() {
                    0 => self.set_message("No jump targets"),
                    // A lone match needs no label; asking for one would just
                    // cost a keystroke to confirm the only choice.
                    1 => self.land_jump(found[0].destination),
                    _ => {
                        self.jump = Some(Jump::Target {
                            targets: found,
                            typed: Vec::new(),
                        });
                    }
                }
            }
            Jump::Target {
                targets: found,
                mut typed,
            } => {
                typed.push(byte);
                if let Some(target) = found.iter().find(|target| target.label == typed) {
                    self.land_jump(target.destination);
                } else if found.iter().any(|target| target.label.starts_with(&typed)) {
                    self.jump = Some(Jump::Target {
                        targets: found,
                        typed,
                    });
                } else {
                    self.set_message("No such jump target");
                }
            }
        }
    }

    /// Moves the cursor to where a jump landed, taking any visual selection
    /// with it.
    fn land_jump(&mut self, destination: usize) {
        self.editor.buffer.cursor = destination;
        self.editor.refresh_visual();
    }

    /// The byte range the last frame drew, which bounds every jump target.
    fn visible_range(&self) -> (usize, usize) {
        let rows = &self.editor.buffer.rows;
        if rows.is_empty() {
            return (0, 0);
        }
        let (first_row, past) = if self.unrendered {
            (0, rows.len())
        } else {
            self.viewport.visible_rows()
        };
        if past <= first_row {
            return (0, 0);
        }
        let first = first_row.min(rows.len() - 1);
        let last = past.min(rows.len()).saturating_sub(1).max(first);
        (rows[first].start, rows[last].end)
    }

    /// Records that the renderer has reported a frame.  Called once per frame.
    pub fn mark_rendered(&mut self) {
        self.unrendered = false;
    }

    /// Acts on one mouse gesture.
    ///
    /// Whichever full-pane list is up owns the mouse, exactly as it owns the
    /// keyboard; behind them a click positions the cursor and a drag selects.
    fn mouse_input(&mut self, mouse: Mouse) {
        // The option is the authority, not the terminal: a report already in
        // flight when reporting was switched off must not still act.
        if self.commands.mouse == 0 {
            return;
        }
        if self.explorer.is_some() || self.recent_open {
            self.pane_mouse(mouse);
            return;
        }
        match mouse.kind {
            MouseKind::Press | MouseKind::Drag if self.scrollbar_gesture(&mouse) => {
                self.scroll_to_track(mouse.row);
            }
            MouseKind::Press => {
                self.drag_anchor = self.viewport.byte_at(&self.editor, mouse.column, mouse.row);
                if let Some(byte) = self.drag_anchor {
                    self.editor.buffer.cursor = byte;
                    self.editor.refresh_visual();
                }
            }
            MouseKind::Drag => {
                let Some(byte) = self.viewport.byte_at(&self.editor, mouse.column, mouse.row)
                else {
                    return;
                };
                // The first drag after a press turns the press into the
                // anchor of a new selection; later ones just extend it.
                if self.drag_anchor.take().is_some() && self.editor.mode == Mode::Normal {
                    self.editor.start_visual(false);
                }
                self.editor.buffer.cursor = byte;
                self.editor.refresh_visual();
            }
            MouseKind::ScrollUp => {
                self.scrollbar_drag = false;
                self.scroll_lines(-SCROLL_LINES);
            }
            MouseKind::ScrollDown => {
                self.scrollbar_drag = false;
                self.scroll_lines(SCROLL_LINES);
            }
        }
    }

    fn pane_mouse(&mut self, mouse: Mouse) {
        let total = if self.recent_open {
            self.recent.paths.len()
        } else {
            self.explorer.as_ref().map_or(0, |e| e.entries.len())
        };
        match mouse.kind {
            MouseKind::Press | MouseKind::Drag if self.scrollbar_gesture(&mouse) => {
                // The bar addresses the list by position, so it moves the
                // selection: a pane's view follows its selection rather than
                // the other way round.
                let first = self.viewport.item_from_track(mouse.row, total);
                let cursor = first
                    .saturating_add(self.viewport.rows.saturating_sub(1))
                    .min(total.saturating_sub(1));
                if self.recent_open {
                    self.recent.cursor = cursor;
                } else if let Some(explorer) = self.explorer.as_mut() {
                    explorer.cursor = cursor;
                }
            }
            MouseKind::Press => {
                let Some(index) = self.viewport.item_at(total, mouse.row) else {
                    return;
                };
                // Clicking the entry already under the cursor opens it, which
                // makes a double click do the obvious thing.
                let selected = if self.recent_open {
                    std::mem::replace(&mut self.recent.cursor, index) == index
                } else {
                    let explorer = self.explorer.as_mut().expect("a pane is open");
                    std::mem::replace(&mut explorer.cursor, index) == index
                };
                if selected {
                    if self.recent_open {
                        self.enter_recent();
                    } else {
                        self.enter_explorer();
                    }
                }
            }
            MouseKind::ScrollUp | MouseKind::ScrollDown => {
                self.scrollbar_drag = false;
                let down = mouse.kind == MouseKind::ScrollDown;
                for _ in 0..SCROLL_LINES {
                    if self.recent_open {
                        if down {
                            self.recent.move_down();
                        } else {
                            self.recent.move_up();
                        }
                    } else if let Some(explorer) = self.explorer.as_mut() {
                        if down {
                            explorer.move_down();
                        } else {
                            explorer.move_up();
                        }
                    }
                }
            }
            MouseKind::Drag => {}
        }
    }

    /// Whether this gesture belongs to the scrollbar, remembering a drag that
    /// began on it so straying off the column does not hand the rest of the
    /// drag to the text underneath.
    fn scrollbar_gesture(&mut self, mouse: &Mouse) -> bool {
        if mouse.kind == MouseKind::Press {
            self.scrollbar_drag = self.viewport.on_scrollbar(mouse.column);
        }
        self.scrollbar_drag
    }

    /// Scrolls so the scrollbar thumb sits at track row `row`.
    fn scroll_to_track(&mut self, row: u16) {
        let rows = self.editor.buffer.rows.len();
        if rows == 0 {
            return;
        }
        self.viewport.row = self.viewport.item_from_track(row, rows);
        self.pull_cursor_into_view();
    }

    /// Scrolls the view and brings the cursor along only as far as it must.
    ///
    /// The origin is pinned to the cursor on every frame, so scrolling away
    /// from it and leaving the cursor behind would simply be undone.
    fn scroll_lines(&mut self, delta: isize) {
        let rows = self.editor.buffer.rows.len();
        if rows == 0 {
            return;
        }
        self.viewport.scroll_by(delta, rows);
        self.pull_cursor_into_view();
    }

    /// Brings the cursor back inside the viewport after the view has moved.
    ///
    /// The origin is pinned to the cursor on every frame, so a scroll that
    /// left the cursor outside it would simply be undone.
    fn pull_cursor_into_view(&mut self) {
        let rows = self.editor.buffer.rows.len();
        if rows == 0 {
            return;
        }
        let (first, past) = self.viewport.visible_rows();
        let last = past.saturating_sub(1).min(rows.saturating_sub(1));
        let current = self.editor.buffer.cursor_row().unwrap_or(0);
        let wanted = current.clamp(first.min(last), last);
        if wanted != current {
            let column = self.editor.buffer.cursor_column().unwrap_or(0);
            let row = self.editor.buffer.rows[wanted];
            self.editor.buffer.cursor = row.start.saturating_add(column).min(row.end);
            self.editor.refresh_visual();
        }
    }

    /// The pending-input hint for the prompt line: the count being typed, or
    /// the jump prompt while `s`/`t` is collecting keys.
    pub fn pending_hint(&self) -> String {
        if self.save_prompt.is_some() {
            return "Save changes? (y/n, Esc cancels)".to_owned();
        }
        if self.confirming.is_some() {
            return "Replace? (y/n/a/q)".to_owned();
        }
        match &self.jump {
            Some(Jump::Character(Kind::Find)) => "s-".to_owned(),
            Some(Jump::Character(Kind::Till)) => "t-".to_owned(),
            Some(Jump::Target { typed, .. }) if !typed.is_empty() => {
                format!("jump {}", String::from_utf8_lossy(typed))
            }
            Some(Jump::Target { .. }) => "jump".to_owned(),
            None => String::from_utf8_lossy(&self.count).into_owned(),
        }
    }

    /// Turns the markdown display layer on or off and reports the new state,
    /// which is otherwise only visible in the status line.
    pub fn toggle_markdown(&mut self) {
        self.markdown = !self.markdown;
        self.set_message(if self.markdown {
            "Markdown display on"
        } else {
            "Markdown display off"
        });
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.commands.message = Some(message.into());
        self.message_pending = true;
    }

    /// Opens or closes one of the full-pane lists.
    ///
    /// Opening one is a step towards leaving the buffer behind, so unsaved
    /// work is settled first rather than after a file has already been
    /// chosen.  Closing one never needs to ask.
    pub fn toggle_pane(&mut self, pane: Pane) {
        let open = match pane {
            Pane::Explorer => self.explorer.is_some(),
            Pane::Recent => self.recent_open,
        };
        if open {
            self.close_panes();
            return;
        }
        if self.saved {
            self.open_pane(pane);
        } else {
            self.save_prompt = Some(pane);
        }
    }

    fn close_panes(&mut self) {
        self.explorer = None;
        self.recent_open = false;
    }

    /// Answers the unsaved-buffer prompt raised by Ctrl-N or Ctrl-R.
    fn save_prompt_input(&mut self, input: Input, pane: Pane) -> Vec<AppEffect> {
        self.save_prompt = None;
        match input {
            Input::Byte(b'y' | b'Y') => {
                // The pane waits for the write to land: opening it now and
                // failing the save would leave unsaved work one keystroke
                // from being replaced.
                self.pending_pane = Some(pane);
                vec![AppEffect::Save(self.output_path())]
            }
            Input::Byte(b'n' | b'N') => {
                self.open_pane(pane);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Opens the pane a save prompt was holding, once the write has been
    /// applied.  A failed write leaves the buffer dirty, so the pane is
    /// dropped rather than opened over work that was meant to be kept.
    pub fn open_pending_pane(&mut self) {
        let Some(pane) = self.pending_pane.take() else {
            return;
        };
        if self.saved {
            self.open_pane(pane);
        }
    }

    /// Shows one of the full-pane lists.  Only one can be up at a time.
    ///
    /// Recent entries are pruned as the picker opens, so a file deleted since
    /// it was recorded is never offered.
    fn open_pane(&mut self, pane: Pane) {
        self.close_panes();
        match pane {
            Pane::Explorer => {
                if let Err(error) = self.open_explorer(Path::new(".")) {
                    self.set_message(error.to_string());
                }
            }
            Pane::Recent => {
                self.recent.prune();
                if self.recent.is_empty() {
                    self.set_message("No recent files");
                    return;
                }
                self.recent.cursor = 0;
                self.recent_open = true;
            }
        }
    }

    /// Records a file as most recently opened and persists the list.
    ///
    /// A list that cannot be written is not worth interrupting an edit
    /// session over, so the failure is dropped rather than reported.
    pub fn record_recent(&mut self, path: &Path) {
        self.recent.record(path);
        if let Some(destination) = self.recent_path.clone() {
            let _ = self.recent.save(&destination);
        }
    }

    fn enter_recent(&mut self) {
        let Some(path) = self.recent.selection() else {
            return;
        };
        self.recent_open = false;
        self.open_file(&path);
    }

    pub fn open_explorer(&mut self, directory: &Path) -> std::io::Result<()> {
        self.explorer = Some(Explorer::scan(directory)?);
        Ok(())
    }

    fn enter_explorer(&mut self) {
        let choice = self.explorer.as_ref().and_then(Explorer::selection);
        match choice {
            Some(Selection::Directory(path)) => {
                let result = self
                    .explorer
                    .as_mut()
                    .expect("explorer exists")
                    .enter_directory(&path);
                if let Err(error) = result {
                    self.set_message(error.to_string());
                }
            }
            Some(Selection::File(path)) => self.open_file(&path),
            None => {}
        }
    }

    /// Replaces the buffer with `path`, as both the explorer and the recent
    /// picker do when a file is chosen.
    fn open_file(&mut self, path: &Path) {
        match load_buffer(path) {
            Ok(bytes) => {
                self.editor = Editor::new(bytes);
                self.editor.indent = self.commands.indent.max(0) as usize;
                self.markdown = is_markdown(path);
                self.filename = path.to_path_buf();
                self.commands.output.clear();
                self.explorer = None;
                self.saved = true;
                self.buffer_replaced = true;
                // A file opened from the explorer or the picker is the user's
                // own, even when the session started on a read-only help page.
                self.readonly = false;
                self.record_recent(path);
            }
            Err(error) => self.set_message(error.to_string()),
        }
    }
}

/// The span that differs between two versions of a buffer, as
/// `(start, old end, new end)`.
///
/// Formatting usually leaves the head and tail of a file alone, so narrowing
/// the undo record to what moved keeps it small.
fn changed_span(before: &[u8], after: &[u8]) -> (usize, usize, usize) {
    let start = before
        .iter()
        .zip(after)
        .position(|(before, after)| before != after)
        .unwrap_or(before.len().min(after.len()));
    let tail = before[start..]
        .iter()
        .rev()
        .zip(after[start..].iter().rev())
        .position(|(before, after)| before != after)
        .unwrap_or_else(|| before.len().min(after.len()) - start);
    (start, before.len() - tail, after.len() - tail)
}

fn unknown(name: &[u8]) -> String {
    format!("Unknown option: {}", String::from_utf8_lossy(name))
}

fn named(name: &[u8]) -> Result<ConfigVariable, String> {
    ConfigVariable::parse(name).ok_or_else(|| unknown(name))
}

/// Splits a `:set` line into its options.
///
/// A backslash escapes the character after it, which is how a `listchars`
/// value gets to hold a space: `tab:>\ ` is one argument, not two.
fn split_options(spec: &[u8]) -> Vec<Vec<u8>> {
    let mut options: Vec<Vec<u8>> = vec![Vec::new()];
    let mut escaped = false;
    for byte in spec {
        if escaped {
            options
                .last_mut()
                .expect("one option is always open")
                .push(*byte);
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if byte.is_ascii_whitespace() {
            options.push(Vec::new());
        } else {
            options
                .last_mut()
                .expect("one option is always open")
                .push(*byte);
        }
    }
    options.retain(|option| !option.is_empty());
    options
}

/// Markdown display starts on for the file types it was written for.
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdx"
            )
        })
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn mapping_key(input: Input) -> Option<i32> {
    match input {
        Input::Byte(byte) | Input::Control(byte) => Some(i32::from(byte)),
        Input::Escape => Some(27),
        Input::Enter => Some(10),
        Input::Backspace => Some(key::BACKSPACE),
        Input::Left => Some(key::LEFT),
        Input::Right => Some(key::RIGHT),
        Input::Up => Some(key::UP),
        Input::Down => Some(key::DOWN),
        Input::Home => Some(key::HOME),
        Input::End => Some(key::END),
        Input::Delete => Some(key::DELETE),
        Input::Insert => Some(key::INSERT),
        Input::PageUp => Some(key::PAGE_UP),
        Input::PageDown => Some(key::PAGE_DOWN),
        // A mouse gesture has no key code to map, and is handled before the
        // mapping table is consulted anyway.
        Input::Mouse(_) | Input::Resize | Input::Unsupported => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::KeyMap;

    fn ex(app: &mut App, command: &[u8]) -> Vec<AppEffect> {
        assert!(app.handle(Input::Byte(b':')).is_empty());
        for &byte in command {
            assert!(app.handle(Input::Byte(byte)).is_empty());
        }
        app.handle(Input::Enter)
    }

    fn make_dirty(app: &mut App) {
        assert!(app.handle(Input::Byte(b'i')).is_empty());
        assert!(app.handle(Input::Byte(b'X')).is_empty());
        assert!(app.handle(Input::Escape).is_empty());
        assert!(!app.saved);
    }

    #[test]
    fn counts_modes_and_typed_effects_flow_through_the_app() {
        let mut app = App::new(b"a\nb\nc\nd".to_vec(), PathBuf::from("file"));
        app.handle(Input::Byte(b'3'));
        app.handle(Input::Byte(b'g'));
        assert_eq!(app.editor.buffer.cursor_row(), Some(2));

        app.handle(Input::Byte(b':'));
        app.handle(Input::Byte(b'w'));
        assert_eq!(
            app.handle(Input::Enter),
            [AppEffect::Save(PathBuf::from("file"))]
        );
    }

    #[test]
    fn q_quits_clean_buffers_but_refuses_unsaved_changes() {
        let mut clean = App::new(b"text".to_vec(), PathBuf::from("file"));
        assert_eq!(ex(&mut clean, b"q"), [AppEffect::Quit]);

        let mut dirty = App::new(b"text".to_vec(), PathBuf::from("file"));
        make_dirty(&mut dirty);
        assert!(ex(&mut dirty, b"q").is_empty());
        assert!(!dirty.saved);
        assert!(!dirty.commands.quit);
        assert_eq!(
            dirty.commands.message.as_deref(),
            Some("No write since last change (add ! to override)")
        );
        assert!(dirty.message_pending);

        assert_eq!(
            ex(&mut dirty, b"w"),
            [AppEffect::Save(PathBuf::from("file"))]
        );
        assert!(!dirty.commands.quit);
        dirty.mark_saved();
        assert_eq!(ex(&mut dirty, b"q"), [AppEffect::Quit]);
    }

    #[test]
    fn q_bang_discards_changes_by_quitting_without_saving() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        make_dirty(&mut app);

        assert_eq!(ex(&mut app, b"q!"), [AppEffect::Quit]);
        assert!(!app.saved);
        assert_eq!(app.editor.buffer.data, b"Xtext");
    }

    #[test]
    fn q_allows_quitting_after_undo_restores_the_saved_contents() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        make_dirty(&mut app);

        assert!(app.handle(Input::Byte(b'u')).is_empty());
        assert_eq!(app.editor.buffer.data, b"text");
        assert!(app.saved);
        assert_eq!(ex(&mut app, b"q"), [AppEffect::Quit]);
    }

    #[test]
    fn wq_requests_save_before_quit() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        make_dirty(&mut app);

        assert_eq!(
            ex(&mut app, b"wq"),
            [AppEffect::Save(PathBuf::from("file")), AppEffect::Quit]
        );
    }

    #[cfg(unix)]
    #[test]
    fn w_preserves_non_utf8_current_and_configured_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let current = PathBuf::from(OsString::from_vec(b"current-\xff".to_vec()));
        let mut app = App::new(b"text".to_vec(), current.clone());
        assert_eq!(ex(&mut app, b"w"), [AppEffect::Save(current)]);

        assert!(ex(&mut app, b"set-output \"next-\xfe\"").is_empty());
        let configured = PathBuf::from(OsString::from_vec(b"next-\xfe".to_vec()));
        assert_eq!(ex(&mut app, b"w"), [AppEffect::Save(configured)]);
    }

    #[test]
    fn prompt_editing_and_pending_replacement_are_independent() {
        let mut app = App::new(b"abc".to_vec(), PathBuf::from("file"));
        app.handle(Input::Byte(b':'));
        app.handle(Input::Byte(b'a'));
        app.handle(Input::Byte(b'b'));
        app.handle(Input::Left);
        app.handle(Input::Backspace);
        assert_eq!(app.prompt, b"b");

        app.handle(Input::Escape);
        app.editor.buffer.cursor = 1;
        app.handle(Input::Byte(b'r'));
        app.handle(Input::Escape);
        assert_eq!(app.editor.buffer.data, b"abc"); // Escape cancels

        app.handle(Input::Byte(b'r'));
        app.handle(Input::Byte(b'X'));
        assert_eq!(app.editor.buffer.data, b"aXc");
    }

    #[test]
    fn control_s_saves_and_quits_only_from_normal_and_insert_modes() {
        for mode in [Mode::Normal, Mode::Insert] {
            let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
            app.editor.mode = mode;
            assert_eq!(
                app.handle(Input::Control(19)),
                [AppEffect::Save(PathBuf::from("file")), AppEffect::Quit]
            );
        }

        for mode in [Mode::Visual, Mode::Search, Mode::Command] {
            let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
            app.editor.mode = mode;
            assert!(app.handle(Input::Control(19)).is_empty());
            assert_eq!(app.editor.mode, mode);
        }
    }

    #[test]
    fn every_named_special_key_can_dispatch_a_configured_mapping() {
        let inputs = [
            Input::Left,
            Input::Right,
            Input::Up,
            Input::Down,
            Input::Home,
            Input::End,
            Input::Delete,
            Input::Insert,
            Input::PageUp,
            Input::PageDown,
        ];
        let mut app = App::new(b"abcdefghijkl".to_vec(), PathBuf::from("file"));
        for input in inputs {
            app.commands.maps.push(KeyMap {
                key: mapping_key(input).expect("named special key"),
                expansion: b"l\0".to_vec(),
            });
        }

        for (expected_cursor, input) in inputs.into_iter().enumerate() {
            assert!(app.handle(input).is_empty());
            assert_eq!(app.editor.buffer.cursor, expected_cursor + 1);
        }
    }

    #[test]
    fn explorer_swallows_keys_that_are_not_navigation() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-swallow-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();

        let mut app = App::new(b"buffer".to_vec(), PathBuf::from("file"));
        app.open_explorer(&root).unwrap();
        for input in [Input::Byte(b'x'), Input::Byte(b'i'), Input::Byte(b'm')] {
            assert!(app.handle(input).is_empty());
        }
        assert_eq!(app.editor.buffer.data, b"buffer");
        assert_eq!(app.editor.mode, Mode::Normal);
        assert!(app.saved);

        assert!(app.handle(Input::Escape).is_empty());
        assert!(app.explorer.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn control_q_refuses_to_discard_unsaved_changes() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        make_dirty(&mut app);
        assert!(app.handle(Input::Control(17)).is_empty());
        assert_eq!(
            app.commands.message.as_deref(),
            Some("No write since last change (add ! to override)")
        );

        let mut clean = App::new(b"text".to_vec(), PathBuf::from("file"));
        assert_eq!(clean.handle(Input::Control(17)), [AppEffect::Quit]);
    }

    #[test]
    fn e_exit_command_respects_the_unsaved_guard() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        make_dirty(&mut app);
        assert!(ex(&mut app, b"e").is_empty());
        assert!(!app.commands.quit);

        app.mark_saved();
        app.saved = true;
        assert_eq!(ex(&mut app, b"e"), [AppEffect::Quit]);
    }

    #[test]
    fn a_mapping_can_execute_a_colon_command() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        assert!(ex(&mut app, b"set-map <c-p> \":w\n\"").is_empty());
        assert_eq!(
            app.handle(Input::Control(16)),
            [AppEffect::Save(PathBuf::from("file"))]
        );
        assert_eq!(app.editor.mode, Mode::Normal);
    }

    #[test]
    fn a_mapping_expansion_does_not_insert_its_nul_terminator() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        assert!(ex(&mut app, b"set-map <c-a> \"ihi\"").is_empty());
        app.handle(Input::Control(1));
        app.handle(Input::Escape);
        assert_eq!(app.editor.buffer.data, b"hitext");
        assert!(!app.editor.buffer.data.contains(&0));
    }

    #[test]
    fn search_pattern_survives_colon_commands_and_escape() {
        let mut app = App::new(b"foo bar foo".to_vec(), PathBuf::from("file"));
        app.handle(Input::Byte(b'/'));
        for &byte in b"foo" {
            app.handle(Input::Byte(byte));
        }
        app.handle(Input::Enter);
        assert_eq!(app.editor.buffer.cursor, 8);

        assert_eq!(ex(&mut app, b"w"), [AppEffect::Save(PathBuf::from("file"))]);
        app.handle(Input::Escape);
        app.handle(Input::Byte(b'n'));
        assert_eq!(app.editor.buffer.cursor, 0);
    }

    #[test]
    fn n_without_a_previous_search_does_not_move_the_cursor() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("file"));
        app.handle(Input::Byte(b'n'));
        assert_eq!(app.editor.buffer.cursor, 0);
        assert_eq!(app.commands.message.as_deref(), Some("No previous search"));
    }

    #[test]
    fn markdown_display_follows_the_extension_and_toggles_in_normal_mode() {
        let mut plain = App::new(b"# Title".to_vec(), PathBuf::from("notes.txt"));
        assert!(!plain.markdown);
        assert!(plain.handle(Input::Control(13)).is_empty());
        assert!(plain.markdown);
        assert_eq!(
            plain.commands.message.as_deref(),
            Some("Markdown display on")
        );
        // Ctrl-M reaches a terminal without keyboard disambiguation as Enter,
        // so the same key has to work through both spellings.
        assert!(plain.handle(Input::Enter).is_empty());
        assert!(!plain.markdown);
        assert!(plain.saved);

        assert!(App::new(Vec::new(), PathBuf::from("README.MD")).markdown);
        assert!(App::new(Vec::new(), PathBuf::from("a/b/notes.markdown")).markdown);

        // Only Normal mode rebinds Enter; Insert mode still splits the line.
        let mut insert = App::new(b"a".to_vec(), PathBuf::from("notes.md"));
        assert!(insert.handle(Input::Byte(b'i')).is_empty());
        assert!(insert.handle(Input::Enter).is_empty());
        assert_eq!(insert.editor.buffer.data, b"\na");
        assert!(insert.markdown);
    }

    #[test]
    fn easymotion_s_labels_visible_matches_and_jumps_to_the_typed_label() {
        //                    0123456789
        let mut app = App::new(b"xo..o.x.ox".to_vec(), PathBuf::from("f"));
        app.editor.buffer.cursor = 5;

        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert_eq!(app.jump, Some(Jump::Character(Kind::Find)));
        assert_eq!(app.pending_hint(), "s-");

        assert!(app.handle(Input::Byte(b'o')).is_empty());
        let Some(Jump::Target { targets, .. }) = &app.jump else {
            panic!("expected labels, got {:?}", app.jump);
        };
        // Nearest first: 4 is one away, 8 is three away, 1 is four away.
        assert_eq!(
            targets.iter().map(|t| t.match_start).collect::<Vec<_>>(),
            [4, 8, 1]
        );

        assert!(app.handle(Input::Byte(b'd')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 1);
        assert!(app.jump.is_none());
        // A jump is a motion, so it must not dirty the buffer.
        assert!(app.saved);
    }

    #[test]
    fn easymotion_t_is_forward_only_and_stops_before_the_match() {
        //                    0123456789
        let mut app = App::new(b"o.o...o..o".to_vec(), PathBuf::from("f"));
        app.editor.buffer.cursor = 3;

        assert!(app.handle(Input::Byte(b't')).is_empty());
        assert!(app.handle(Input::Byte(b'o')).is_empty());
        assert!(app.handle(Input::Byte(b's')).is_empty());
        // The second forward match is at 9, so `t` lands on 8.
        assert_eq!(app.editor.buffer.cursor, 8);

        // Nothing ahead of the cursor means nothing to label.
        app.editor.buffer.cursor = app.editor.buffer.data.len();
        assert!(app.handle(Input::Byte(b't')).is_empty());
        assert!(app.handle(Input::Byte(b'o')).is_empty());
        assert!(app.jump.is_none());
        assert_eq!(app.commands.message.as_deref(), Some("No jump targets"));
    }

    #[test]
    fn a_lone_match_needs_no_label_and_escape_abandons_the_jump() {
        let mut app = App::new(b"abcZdef".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert!(app.handle(Input::Byte(b'Z')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 3);
        assert!(app.jump.is_none());

        // Escape at either phase leaves the buffer and cursor alone.
        for cancel in [Input::Escape, Input::Control(3)] {
            app.editor.buffer.cursor = 0;
            assert!(app.handle(Input::Byte(b's')).is_empty());
            assert!(app.handle(cancel).is_empty());
            assert!(app.jump.is_none());
            assert_eq!(app.editor.buffer.cursor, 0);
        }

        // A key that matches no label ends the jump rather than lingering.
        let mut many = App::new(b"o.o.o".to_vec(), PathBuf::from("f"));
        assert!(many.handle(Input::Byte(b's')).is_empty());
        assert!(many.handle(Input::Byte(b'o')).is_empty());
        assert!(many.handle(Input::Byte(b'Z')).is_empty());
        assert!(many.jump.is_none());
        assert_eq!(
            many.commands.message.as_deref(),
            Some("No such jump target")
        );
    }

    #[test]
    fn a_jump_only_targets_the_rows_the_renderer_reported() {
        let mut app = App::new(
            b"o
o
o
o"
            .to_vec(),
            PathBuf::from("f"),
        );
        app.viewport = Viewport {
            rows: 2,
            ..Viewport::default()
        };
        app.mark_rendered();
        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert!(app.handle(Input::Byte(b'o')).is_empty());
        // Row 0 holds the cursor and is skipped, leaving only row 1 on screen,
        // so the single remaining match jumps without asking for a label.
        assert_eq!(app.editor.buffer.cursor, 2);
        assert!(app.jump.is_none());
    }

    #[test]
    fn visual_s_extends_the_selection_to_the_label() {
        //                    0123456789
        let mut app = App::new(b"xo..o.x.ox".to_vec(), PathBuf::from("f"));
        app.editor.buffer.cursor = 5;
        assert!(app.handle(Input::Byte(b'v')).is_empty());
        assert_eq!(app.editor.mode, Mode::Visual);

        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert_eq!(app.jump, Some(Jump::Character(Kind::Find)));
        assert!(app.handle(Input::Byte(b'o')).is_empty());
        // The third-nearest match is at 1, behind the anchor, so the
        // selection reaches backwards.
        assert!(app.handle(Input::Byte(b'd')).is_empty());

        assert_eq!(app.editor.buffer.cursor, 1);
        assert_eq!(app.editor.mode, Mode::Visual);
        assert_eq!(app.editor.visual.anchor, 5);
        assert_eq!(app.editor.visual.end, 1);
        // Yanking proves the selection really covers 1..=5.
        assert!(app.handle(Input::Byte(b'y')).is_empty());
        assert_eq!(app.editor.clipboard, b"o..o.");
        assert!(app.saved);
    }

    #[test]
    fn a_linewise_visual_s_still_selects_whole_rows() {
        let mut app = App::new(b"aa\nbb\ncc\ndZ".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b'V')).is_empty());
        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert!(app.handle(Input::Byte(b'Z')).is_empty());
        // `Z` occurs once, so the jump lands without asking for a label and
        // the selection grows to cover the whole last row.
        assert_eq!(app.editor.buffer.cursor, 10);
        assert_eq!(app.editor.visual.start, 0);
        assert_eq!(app.editor.visual.end, 11);
    }

    #[test]
    fn cancelling_a_visual_jump_keeps_the_selection_it_started_from() {
        let mut app = App::new(b"abcdef".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b'v')).is_empty());
        assert!(app.handle(Input::Byte(b'l')).is_empty());
        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert!(app.handle(Input::Escape).is_empty());

        // Escape ends the jump, not the selection.
        assert!(app.jump.is_none());
        assert_eq!(app.editor.mode, Mode::Visual);
        assert_eq!(app.editor.visual.end, 1);
        // A second Escape does leave Visual mode.
        assert!(app.handle(Input::Escape).is_empty());
        assert_eq!(app.editor.mode, Mode::Normal);
    }

    #[test]
    fn a_key_mapping_cannot_fire_while_a_jump_is_collecting_keys() {
        let mut app = App::new(b"o.o.o".to_vec(), PathBuf::from("f"));
        app.commands.maps.push(KeyMap {
            key: i32::from(b'a'),
            expansion: b"x\0".to_vec(),
        });

        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert!(app.handle(Input::Byte(b'o')).is_empty());
        // `a` is the nearest label here, not the mapping that would delete a
        // byte, and the mapping is reachable again once the jump is over.
        assert!(app.handle(Input::Byte(b'a')).is_empty());
        assert_eq!(app.editor.buffer.data, b"o.o.o");
        assert_eq!(app.editor.buffer.cursor, 2);
        assert!(app.saved);

        // With the jump over, the same key reaches its mapping again.
        assert!(app.handle(Input::Byte(b'a')).is_empty());
        assert_eq!(app.editor.buffer.data, b"o..o");
        assert!(!app.saved);
    }

    #[test]
    fn space_leader_maps_i_to_star_and_o_to_nohl() {
        //                    0123456789012345678901234
        let mut app = App::new(b"the fox and the other fox".to_vec(), PathBuf::from("f"));

        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'i')).is_empty());
        // `*` matches whole words, so `other` does not count and the cursor
        // lands on the second `the`.
        assert_eq!(app.editor.buffer.cursor, 12);
        assert_eq!(app.highlight.needle, b"the".to_vec());
        assert!(app.highlight.whole_word);

        // `n` keeps repeating it whole-word, wrapping back to the first.
        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 0);

        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'o')).is_empty());
        assert!(app.highlight.is_empty());
        // Clearing the highlight keeps the pattern `n` repeats.
        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 12);

        // The bare `*` the mapping stands for behaves the same way.
        let mut star = App::new(b"the fox and the".to_vec(), PathBuf::from("f"));
        assert!(star.handle(Input::Byte(b'*')).is_empty());
        assert_eq!(star.editor.buffer.cursor, 12);
        assert!(star.saved);
    }

    #[test]
    fn a_leader_key_with_no_mapping_is_dropped_rather_than_acted_on() {
        let mut app = App::new(b"abc".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b' ')).is_empty());
        // `x` would delete a byte if the leader had let it through.
        assert!(app.handle(Input::Byte(b'x')).is_empty());
        assert_eq!(app.editor.buffer.data, b"abc");
        assert!(app.saved);
    }

    #[test]
    fn search_highlighting_is_set_by_slash_and_cleared_by_nohl() {
        let mut app = App::new(b"the other then".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b'/')).is_empty());
        for byte in b"the" {
            assert!(app.handle(Input::Byte(*byte)).is_empty());
        }
        assert!(app.handle(Input::Enter).is_empty());
        // A `/` search is a substring search, unlike `*`.
        assert_eq!(app.highlight.needle, b"the".to_vec());
        assert!(!app.highlight.whole_word);
        assert_eq!(app.highlight.matches(&app.editor.buffer.data).len(), 3);

        assert!(ex(&mut app, b"nohl").is_empty());
        assert!(app.highlight.is_empty());
        assert!(ex(&mut app, b"nohlsearch").is_empty());
    }

    #[test]
    fn capital_n_repeats_the_search_backwards_and_wraps() {
        //                    0    5    10   15
        let mut app = App::new(b"a x b x c x".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b'/')).is_empty());
        assert!(app.handle(Input::Byte(b'x')).is_empty());
        assert!(app.handle(Input::Enter).is_empty());
        // Matches sit at 2, 6 and 10; the search lands on the first.
        assert_eq!(app.editor.buffer.cursor, 2);

        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 6);
        assert!(app.handle(Input::Byte(b'N')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 2);
        // Backwards past the first match wraps to the last.
        assert!(app.handle(Input::Byte(b'N')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 10);

        // A count repeats it, and `N` is a motion so nothing is edited.
        assert!(app.handle(Input::Byte(b'2')).is_empty());
        assert!(app.handle(Input::Byte(b'N')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 2);
        assert_eq!(app.editor.buffer.data, b"a x b x c x");
        assert!(app.saved);
    }

    #[test]
    fn capital_n_follows_star_whole_word_and_reports_an_empty_search() {
        let mut app = App::new(b"the then the other the".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b'N')).is_empty());
        assert_eq!(app.commands.message.as_deref(), Some("No previous search"));
        assert_eq!(app.editor.buffer.cursor, 0);

        // `*` from the first `the` moves forward to the third one at 9.
        assert!(app.handle(Input::Byte(b'*')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 9);
        // `N` goes back whole-word, so `then` and `other` are skipped.
        assert!(app.handle(Input::Byte(b'N')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 0);
        assert!(app.handle(Input::Byte(b'N')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 19);
    }

    #[test]
    fn cursorline_is_off_until_it_is_configured() {
        let mut app = App::new(
            b"a
b"
            .to_vec(),
            PathBuf::from("f"),
        );
        assert_eq!(app.commands.cursorline, 0);
        assert!(ex(&mut app, b"set-var cursorline 1").is_empty());
        assert_eq!(app.commands.cursorline, 1);
        // The dashed spelling is accepted the way auto-indent's is.
        assert!(ex(&mut app, b"set-var cursor-line 0").is_empty());
        assert_eq!(app.commands.cursorline, 0);
    }

    #[test]
    fn star_without_a_word_under_the_cursor_reports_instead_of_moving() {
        let mut app = App::new(b"...".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Byte(b'*')).is_empty());
        assert_eq!(app.editor.buffer.cursor, 0);
        assert_eq!(
            app.commands.message.as_deref(),
            Some("No word under the cursor")
        );
    }

    #[test]
    fn control_r_picks_a_recent_file_and_records_what_it_opens() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-recent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let older = root.join("older.txt");
        let newer = root.join("newer.txt");
        std::fs::write(&older, b"older body").unwrap();
        std::fs::write(&newer, b"newer body").unwrap();

        let mut app = App::new(b"start".to_vec(), PathBuf::from("start.txt"));
        // Nothing to show yet, so the picker reports instead of opening blank.
        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(!app.recent_open);
        assert_eq!(app.commands.message.as_deref(), Some("No recent files"));

        app.record_recent(&older);
        app.record_recent(&newer);
        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(app.recent_open);
        assert_eq!(app.recent.cursor, 0);

        // Keys belong to the picker while it is open, so the buffer behind it
        // is left alone.
        assert!(app.handle(Input::Byte(b'x')).is_empty());
        assert_eq!(app.editor.buffer.data, b"start");

        assert!(app.handle(Input::Byte(b'j')).is_empty());
        assert_eq!(app.recent.cursor, 1);
        assert!(app.handle(Input::Enter).is_empty());

        assert!(!app.recent_open);
        assert_eq!(app.editor.buffer.data, b"older body");
        assert_eq!(app.filename, older);
        assert!(app.saved);
        // Opening it makes it the most recent entry in turn.
        assert_eq!(
            app.recent.paths.first(),
            Some(&older.canonicalize().unwrap())
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_a_file_leaves_the_read_only_help_buffer_behind() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-readonly-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("own.txt");
        std::fs::write(&file, b"body").unwrap();

        let mut app = App::new(b"help page".to_vec(), PathBuf::from("general"));
        app.readonly = true;
        app.record_recent(&file);

        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(app.handle(Input::Enter).is_empty());

        assert_eq!(app.editor.buffer.data, b"body");
        // Otherwise the file just opened could never be written.
        assert!(!app.readonly);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_insert_mapping_replaces_the_keys_that_triggered_it() {
        let mut app = App::new(b"tail".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut app, b"imap ;; <Esc>").is_empty());

        assert!(app.handle(Input::Byte(b'i')).is_empty());
        for byte in b"ab;;" {
            assert!(app.handle(Input::Byte(*byte)).is_empty());
        }
        // The first `;` was inserted as ordinary text and has to come back
        // out; the second was never inserted at all.
        assert_eq!(app.editor.buffer.data, b"abtail");
        assert_eq!(app.editor.mode, Mode::Normal);

        // A run that fails to match still leaves a later one reachable.
        assert!(app.handle(Input::Byte(b'i')).is_empty());
        for byte in b";x;;" {
            assert!(app.handle(Input::Byte(*byte)).is_empty());
        }
        assert_eq!(app.editor.buffer.data, b"ab;xtail");
        assert_eq!(app.editor.mode, Mode::Normal);
    }

    #[test]
    fn an_insert_mapping_only_fires_on_keys_typed_in_a_row() {
        let mut app = App::new(Vec::new(), PathBuf::from("f"));
        assert!(ex(&mut app, b"imap ;; <Esc>").is_empty());

        // A cursor move between the two keys breaks the run, so the second
        // `;` is ordinary text rather than the end of a mapping.
        assert!(app.handle(Input::Byte(b'i')).is_empty());
        assert!(app.handle(Input::Byte(b';')).is_empty());
        assert!(app.handle(Input::Left).is_empty());
        assert!(app.handle(Input::Byte(b';')).is_empty());
        assert_eq!(app.editor.buffer.data, b";;");
        assert_eq!(app.editor.mode, Mode::Insert);
    }

    #[test]
    fn an_insert_mapping_can_expand_to_more_than_one_key() {
        let mut app = App::new(Vec::new(), PathBuf::from("f"));
        assert!(ex(&mut app, b"imap ,d hello").is_empty());
        assert!(app.handle(Input::Byte(b'i')).is_empty());
        for byte in b",d" {
            assert!(app.handle(Input::Byte(*byte)).is_empty());
        }
        assert_eq!(app.editor.buffer.data, b"hello");
        assert_eq!(app.editor.mode, Mode::Insert);
    }

    #[test]
    fn opening_a_pane_asks_before_leaving_unsaved_work() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-prompt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("file.txt");
        std::fs::write(&path, b"disk").unwrap();

        let mut app = App::new(b"disk".to_vec(), path.clone());
        make_dirty(&mut app);

        // Escape backs out and leaves everything alone.
        assert!(app.handle(Input::Control(14)).is_empty());
        assert_eq!(app.save_prompt, Some(Pane::Explorer));
        assert_eq!(app.pending_hint(), "Save changes? (y/n, Esc cancels)");
        assert!(app.handle(Input::Escape).is_empty());
        assert!(app.save_prompt.is_none());
        assert!(app.explorer.is_none());
        assert!(!app.saved);

        // `n` discards and opens the pane without writing anything.
        assert!(app.handle(Input::Control(14)).is_empty());
        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert!(app.explorer.is_some());
        assert!(!app.saved);
        assert_eq!(std::fs::read(&path).unwrap(), b"disk");

        // `y` asks for the write, and the pane waits for it to land.
        assert!(app.handle(Input::Control(14)).is_empty());
        assert!(app.handle(Input::Control(14)).is_empty());
        assert_eq!(app.save_prompt, Some(Pane::Explorer));
        assert_eq!(
            app.handle(Input::Byte(b'y')),
            [AppEffect::Save(path.clone())]
        );
        assert!(app.explorer.is_none());
        // Until the buffer is actually saved, the pane stays shut.
        app.open_pending_pane();
        assert!(app.explorer.is_none());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_clean_buffer_opens_a_pane_without_asking_and_the_leader_agrees() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("f"));
        assert!(app.handle(Input::Control(14)).is_empty());
        assert!(app.save_prompt.is_none());
        assert!(app.explorer.is_some());
        // An open explorer owns the keyboard, so it takes a chord to close.
        assert!(app.handle(Input::Control(14)).is_empty());
        assert!(app.explorer.is_none());

        // `<leader>n` is Ctrl-N, and `<leader>r` is Ctrl-R.
        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert!(app.explorer.is_some());
        assert!(app.handle(Input::Escape).is_empty());
        assert!(app.explorer.is_none());

        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'r')).is_empty());
        assert_eq!(app.commands.message.as_deref(), Some("No recent files"));

        // The leader guards unsaved work the same way the chords do.
        make_dirty(&mut app);
        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert_eq!(app.save_prompt, Some(Pane::Explorer));
    }

    fn shown(app: &mut App, rows: usize) {
        app.viewport = Viewport {
            rows,
            content_x: 5,
            content_width: 40,
            ..Viewport::default()
        };
        app.mark_rendered();
    }

    fn click(kind: MouseKind, column: u16, row: u16) -> Input {
        Input::Mouse(Mouse { kind, column, row })
    }

    #[test]
    fn a_click_moves_the_cursor_and_a_drag_selects() {
        let mut app = App::new(b"alpha\nbeta\ngamma".to_vec(), PathBuf::from("f"));
        shown(&mut app, 3);

        assert!(app.handle(click(MouseKind::Press, 7, 1)).is_empty());
        assert_eq!(app.editor.buffer.cursor, 8);
        assert_eq!(app.editor.mode, Mode::Normal);
        // Positioning the cursor is a motion, not an edit.
        assert!(app.saved);

        // The first drag turns the press into the anchor of a selection.
        assert!(app.handle(click(MouseKind::Drag, 8, 2)).is_empty());
        assert_eq!(app.editor.mode, Mode::Visual);
        assert_eq!(app.editor.visual.anchor, 8);
        assert_eq!(app.editor.buffer.cursor, 14);
        assert_eq!(app.editor.visual.end, 14);

        // Later drags only extend it.
        assert!(app.handle(click(MouseKind::Drag, 5, 2)).is_empty());
        assert_eq!(app.editor.visual.anchor, 8);
        assert_eq!(app.editor.buffer.cursor, 11);

        // Yanking proves the selection is the range that was dragged over.
        assert!(app.handle(Input::Byte(b'y')).is_empty());
        assert_eq!(app.editor.clipboard, b"ta\ng");
    }

    #[test]
    fn the_wheel_scrolls_the_view_and_brings_the_cursor_along() {
        let mut app = App::new(b"a\nb\nc\nd\ne\nf\ng\nh".to_vec(), PathBuf::from("f"));
        shown(&mut app, 3);

        assert!(app.handle(click(MouseKind::ScrollDown, 0, 0)).is_empty());
        assert_eq!(app.viewport.row, 3);
        // The origin is pinned to the cursor every frame, so the cursor has
        // to come far enough for the scroll to survive the next one.
        assert_eq!(app.editor.buffer.cursor_row(), Some(3));

        assert!(app.handle(click(MouseKind::ScrollUp, 0, 0)).is_empty());
        assert_eq!(app.viewport.row, 0);
        // Coming back up it only moves as far as it must: row 3 is one past
        // the new window, so it stops at its last row rather than its first.
        assert_eq!(app.editor.buffer.cursor_row(), Some(2));

        // Neither direction can run off the buffer, and a cursor already in
        // view is left where it is.
        for _ in 0..20 {
            assert!(app.handle(click(MouseKind::ScrollUp, 0, 0)).is_empty());
        }
        assert_eq!(app.viewport.row, 0);
        assert_eq!(app.editor.buffer.cursor_row(), Some(2));
        assert!(app.saved);
    }

    #[test]
    fn the_scrollbar_scrolls_and_keeps_a_drag_that_strays_off_it() {
        let mut app = App::new(
            (1..=40)
                .map(|n| format!("line {n}\n"))
                .collect::<String>()
                .into_bytes(),
            PathBuf::from("f"),
        );
        app.viewport = Viewport {
            rows: 10,
            content_x: 5,
            content_width: 54,
            scrollbar_x: Some(59),
            ..Viewport::default()
        };
        app.mark_rendered();

        // Clicking down the bar scrolls proportionally rather than putting
        // the cursor at the end of a line.
        assert!(app.handle(click(MouseKind::Press, 59, 5)).is_empty());
        let scrolled = app.viewport.row;
        assert!(scrolled > 0, "the bar did not scroll");
        assert!(
            app.editor
                .buffer
                .cursor_row()
                .is_some_and(|row| row >= scrolled)
        );
        assert!(app.saved);

        // A drag keeps following the bar even once the pointer leaves the
        // column, which is what makes the thumb usable.
        assert!(app.handle(click(MouseKind::Drag, 20, 1)).is_empty());
        assert!(app.viewport.row < scrolled);
        let dragged = app.viewport.row;

        // The wheel ends the drag, so the next drag is text again.
        assert!(app.handle(click(MouseKind::ScrollDown, 20, 1)).is_empty());
        assert!(app.viewport.row > dragged);
        assert!(app.handle(click(MouseKind::Press, 7, 1)).is_empty());
        assert!(app.handle(click(MouseKind::Drag, 9, 2)).is_empty());
        assert_eq!(app.editor.mode, Mode::Visual);

        // Clicking the top of the bar goes back to the start of the file.
        assert!(app.handle(click(MouseKind::Press, 59, 0)).is_empty());
        assert_eq!(app.viewport.row, 0);
    }

    #[test]
    fn a_pane_scrollbar_moves_the_selection() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-bar-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut app = App::new(b"start".to_vec(), PathBuf::from("start.txt"));
        for index in 0..20 {
            let path = root.join(format!("file{index:02}.txt"));
            std::fs::write(&path, b"body").unwrap();
            app.record_recent(&path);
        }
        assert!(app.handle(Input::Control(18)).is_empty());
        app.viewport = Viewport {
            rows: 5,
            content_x: 5,
            content_width: 54,
            scrollbar_x: Some(59),
            ..Viewport::default()
        };
        app.mark_rendered();
        assert_eq!(app.recent.cursor, 0);

        // The bar addresses the list, so it moves the selection down it.
        assert!(app.handle(click(MouseKind::Press, 59, 4)).is_empty());
        assert_eq!(app.recent.cursor, app.recent.paths.len() - 1);
        // It never opens anything, however many times it is clicked.
        assert!(app.handle(click(MouseKind::Press, 59, 4)).is_empty());
        assert!(app.recent_open);
        assert_eq!(app.editor.buffer.data, b"start");

        assert!(app.handle(click(MouseKind::Press, 59, 0)).is_empty());
        assert!(app.recent.cursor < app.recent.paths.len() - 1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_click_in_a_pane_selects_and_a_second_click_opens() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-mouse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        std::fs::write(&first, b"first body").unwrap();
        std::fs::write(&second, b"second body").unwrap();

        let mut app = App::new(b"start".to_vec(), PathBuf::from("start.txt"));
        app.record_recent(&first);
        app.record_recent(&second);
        assert!(app.handle(Input::Control(18)).is_empty());
        shown(&mut app, 5);

        // One click selects without opening.
        assert!(app.handle(click(MouseKind::Press, 8, 1)).is_empty());
        assert_eq!(app.recent.cursor, 1);
        assert!(app.recent_open);
        assert_eq!(app.editor.buffer.data, b"start");

        // Clicking the row already under the cursor opens it.
        assert!(app.handle(click(MouseKind::Press, 8, 1)).is_empty());
        assert!(!app.recent_open);
        assert_eq!(app.editor.buffer.data, b"first body");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_mouse_option_is_what_decides_whether_a_report_acts() {
        let mut app = App::new(b"alpha\nbeta".to_vec(), PathBuf::from("f"));
        shown(&mut app, 2);
        assert_eq!(app.commands.mouse, 1);

        assert!(ex(&mut app, b"set-var mouse 0").is_empty());
        assert!(app.handle(click(MouseKind::Press, 7, 1)).is_empty());
        assert_eq!(app.editor.buffer.cursor, 0);

        assert!(ex(&mut app, b"set-var mouse 1").is_empty());
        assert!(app.handle(click(MouseKind::Press, 7, 1)).is_empty());
        assert_eq!(app.editor.buffer.cursor, 8);
    }

    #[test]
    fn a_click_is_not_a_key_and_cannot_answer_a_prompt_or_a_jump() {
        let mut app = App::new(b"alpha\nbeta".to_vec(), PathBuf::from("f"));
        shown(&mut app, 2);

        // A pending jump keeps waiting for its label.
        assert!(app.handle(Input::Byte(b's')).is_empty());
        assert!(app.handle(click(MouseKind::Press, 7, 1)).is_empty());
        assert_eq!(app.jump, Some(Jump::Character(Kind::Find)));
        assert_eq!(app.editor.buffer.cursor, 8);
        assert!(app.handle(Input::Escape).is_empty());

        // So does an unanswered save prompt.
        make_dirty(&mut app);
        assert!(app.handle(Input::Control(14)).is_empty());
        assert!(app.handle(click(MouseKind::Press, 6, 0)).is_empty());
        assert_eq!(app.save_prompt, Some(Pane::Explorer));
    }

    #[test]
    fn a_global_substitute_rewrites_the_file_and_undoes_in_one_press() {
        let mut app = App::new(
            b"foo bar foo\nfoobar\nlast foo".to_vec(),
            PathBuf::from("f"),
        );
        assert!(ex(&mut app, b"%s/foo/XX/g").is_empty());
        assert_eq!(app.editor.buffer.data, b"XX bar XX\nXXbar\nlast XX");
        assert_eq!(
            app.commands.message.as_deref(),
            Some("4 substitutions on 3 lines")
        );
        assert!(!app.saved);

        // One command is one undo step, and one redo step.
        assert!(app.handle(Input::Byte(b'u')).is_empty());
        assert_eq!(app.editor.buffer.data, b"foo bar foo\nfoobar\nlast foo");
        assert!(app.saved);
        assert!(app.handle(Input::Byte(b'U')).is_empty());
        assert_eq!(app.editor.buffer.data, b"XX bar XX\nXXbar\nlast XX");
        assert!(app.editor.buffer.invariants_hold());
    }

    #[test]
    fn substitute_flags_and_ranges_narrow_what_is_rewritten() {
        let source = b"foo foo\nfoobar FOO\nfoo end".to_vec();

        // Without `g`, only the first match on each line.
        let mut once = App::new(source.clone(), PathBuf::from("f"));
        assert!(ex(&mut once, b"%s/foo/X/").is_empty());
        assert_eq!(once.editor.buffer.data, b"X foo\nXbar FOO\nX end");

        // `i` folds case, `\<..\>` demands a whole word.
        let mut folded = App::new(source.clone(), PathBuf::from("f"));
        assert!(ex(&mut folded, b"%s/foo/X/gi").is_empty());
        assert_eq!(folded.editor.buffer.data, b"X X\nXbar X\nX end");

        let mut word = App::new(source.clone(), PathBuf::from("f"));
        assert!(ex(&mut word, br"%s/\<foo\>/X/g").is_empty());
        assert_eq!(word.editor.buffer.data, b"X X\nfoobar FOO\nX end");

        // A line range, and the bare form that means the cursor's line.
        let mut ranged = App::new(source.clone(), PathBuf::from("f"));
        assert!(ex(&mut ranged, b"2,3s/foo/X/g").is_empty());
        assert_eq!(ranged.editor.buffer.data, b"foo foo\nXbar FOO\nX end");

        let mut current = App::new(source.clone(), PathBuf::from("f"));
        current.editor.buffer.cursor = 8;
        assert!(ex(&mut current, b"s/foo/X/g").is_empty());
        assert_eq!(current.editor.buffer.data, b"foo foo\nXbar FOO\nfoo end");

        // Any punctuation can delimit, which is what saves escaping slashes.
        let mut urls = App::new(b"http://a http://b".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut urls, b"%s#http://#https://#g").is_empty());
        assert_eq!(urls.editor.buffer.data, b"https://a https://b");
    }

    #[test]
    fn a_confirmed_substitute_asks_per_match_and_stays_one_undo_step() {
        let mut app = App::new(b"foo foo\nfoo".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut app, b"%s/foo/LONGER/gc").is_empty());
        assert!(app.confirming.is_some());
        assert_eq!(app.pending_hint(), "Replace? (y/n/a/q)");
        // The cursor sits on the match being asked about.
        assert_eq!(app.editor.buffer.cursor, 0);

        assert!(app.handle(Input::Byte(b'y')).is_empty());
        assert!(app.handle(Input::Byte(b'n')).is_empty());
        assert!(app.handle(Input::Byte(b'y')).is_empty());
        // The buffer is exhausted, so the run ends on its own.
        assert!(app.confirming.is_none());
        assert_eq!(app.editor.buffer.data, b"LONGER foo\nLONGER");
        assert_eq!(
            app.commands.message.as_deref(),
            Some("2 substitutions on 2 lines")
        );

        // Replacements of a different length still undo as one step.
        assert!(app.handle(Input::Byte(b'u')).is_empty());
        assert_eq!(app.editor.buffer.data, b"foo foo\nfoo");
        assert!(app.editor.buffer.invariants_hold());
    }

    #[test]
    fn a_confirmation_answers_all_with_a_and_stops_with_q() {
        let mut app = App::new(b"a a a a".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut app, b"%s/a/b/gc").is_empty());
        assert!(app.handle(Input::Byte(b'y')).is_empty());
        assert!(app.handle(Input::Byte(b'a')).is_empty());
        assert!(app.confirming.is_none());
        assert_eq!(app.editor.buffer.data, b"b b b b");

        // `q` keeps what was already answered and abandons the rest.
        let mut stopped = App::new(b"a a a a".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut stopped, b"%s/a/b/gc").is_empty());
        assert!(stopped.handle(Input::Byte(b'y')).is_empty());
        assert!(stopped.handle(Input::Byte(b'q')).is_empty());
        assert!(stopped.confirming.is_none());
        assert_eq!(stopped.editor.buffer.data, b"b a a a");
        assert_eq!(
            stopped.commands.message.as_deref(),
            Some("1 substitution on 1 line")
        );

        // Refusing every match changes nothing at all.
        let mut refused = App::new(b"a a".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut refused, b"%s/a/b/gc").is_empty());
        assert!(refused.handle(Input::Byte(b'n')).is_empty());
        assert!(refused.handle(Input::Byte(b'n')).is_empty());
        assert_eq!(refused.editor.buffer.data, b"a a");
        assert_eq!(
            refused.commands.message.as_deref(),
            Some("No substitutions")
        );
        assert!(refused.saved);
    }

    #[test]
    fn substitute_reports_a_miss_and_leaves_other_commands_alone() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("f"));
        assert!(ex(&mut app, b"%s/absent/x/g").is_empty());
        assert_eq!(app.editor.buffer.data, b"text");
        assert!(
            app.commands
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with("Pattern not found"))
        );

        // A command that merely starts with `s` still reaches the token
        // language.
        assert!(ex(&mut app, b"set-var relative 1").is_empty());
        assert_eq!(app.commands.relative, 1);
        assert_eq!(ex(&mut app, b"w"), [AppEffect::Save(PathBuf::from("f"))]);

        // Help pages refuse to be rewritten, the way they refuse a save.
        let mut help = App::new(b"help text".to_vec(), PathBuf::from("general"));
        help.readonly = true;
        assert!(ex(&mut help, b"%s/help/HELP/g").is_empty());
        assert_eq!(help.editor.buffer.data, b"help text");
        assert_eq!(
            help.commands.message.as_deref(),
            Some("Buffer is read-only")
        );
    }

    #[test]
    fn set_reads_the_documented_listchars_line() {
        let mut app = App::new(b"a\tb  \n".to_vec(), PathBuf::from("f"));
        // The escaped space belongs to the tab fill, so the whole value is
        // one argument even though it contains a space.
        assert!(
            ex(
                &mut app,
                "set listchars=tab:\u{25b8}\\ ,trail:\u{b7},eol:\u{21b2},nbsp:\u{23b5},space:\u{b7}"
                    .as_bytes()
            )
            .is_empty()
        );
        let chars = app.commands.listchars;
        assert_eq!(chars.tab, Some(('\u{25b8}', ' ')));
        assert_eq!(chars.trail, Some('\u{b7}'));
        assert_eq!(chars.eol, Some('\u{21b2}'));
        assert_eq!(chars.nbsp, Some('\u{23b5}'));
        assert_eq!(chars.space, Some('\u{b7}'));
        // Setting the glyphs does not turn `list` on by itself.
        assert_eq!(app.commands.list, 0);
        assert!(app.saved);
    }

    #[test]
    fn list_toggles_from_set_and_from_the_leader() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("f"));
        assert_eq!(app.commands.list, 0);

        assert!(ex(&mut app, b"set list").is_empty());
        assert_eq!(app.commands.list, 1);
        assert!(ex(&mut app, b"set nolist").is_empty());
        assert_eq!(app.commands.list, 0);
        assert!(ex(&mut app, b"set list!").is_empty());
        assert_eq!(app.commands.list, 1);

        // `<leader>l` is the same toggle.
        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'l')).is_empty());
        assert_eq!(app.commands.list, 0);
        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'l')).is_empty());
        assert_eq!(app.commands.list, 1);
        assert!(app.saved);
    }

    #[test]
    fn set_reaches_the_other_options_and_says_what_it_cannot() {
        let mut app = App::new(b"text".to_vec(), PathBuf::from("f"));

        // Several options at once, vim's spellings included.
        assert!(ex(&mut app, b"set cursorline rnu sw=2").is_empty());
        assert_eq!(app.commands.cursorline, 1);
        assert_eq!(app.commands.relative, 1);
        assert_eq!(app.commands.indent, 2);
        // An indent change reaches the editor, not just the option table.
        assert_eq!(app.editor.indent, 2);

        assert!(ex(&mut app, b"set nomouse").is_empty());
        assert_eq!(app.commands.mouse, 0);
        assert!(ex(&mut app, b"set mouse!").is_empty());
        assert_eq!(app.commands.mouse, 1);

        // Querying reports the value rather than changing it.
        assert!(ex(&mut app, b"set sw?").is_empty());
        assert_eq!(app.commands.message.as_deref(), Some("sw=2"));
        assert_eq!(app.commands.indent, 2);

        for (line, message) in [
            (&b"set nosuchoption"[..], "Unknown option: nosuchoption"),
            (b"set sw=lots", "Invalid value for sw: lots"),
            (b"set listchars=bogus:x", "Unknown listchars item: bogus"),
            (b"set listchars=eos:$", "Unknown listchars item: eos"),
            (b"set listchars=eol:xy", "listchars eol takes one character"),
        ] {
            assert!(ex(&mut app, line).is_empty());
            assert_eq!(app.commands.message.as_deref(), Some(message));
        }

        // `set-var` is a different command and still reaches the lexer.
        assert!(ex(&mut app, b"set-var relative 0").is_empty());
        assert_eq!(app.commands.relative, 0);
    }

    #[test]
    fn run_command_is_the_colon_prompt_without_the_typing() {
        let mut app = App::new(b"a\tb".to_vec(), PathBuf::from("f"));

        // The escaped space is what keeps the tab fill in one argument, the
        // same way it does in a vimrc.
        assert!(
            app.run_command("set listchars=tab:>\\ ,trail:.,eol:$".as_bytes())
                .is_empty()
        );
        assert_eq!(app.commands.listchars.tab, Some(('>', ' ')));
        assert_eq!(app.commands.listchars.trail, Some('.'));
        assert_eq!(app.commands.listchars.eol, Some('$'));

        // A leading colon is accepted, because that is how the line reads in
        // vim and how it will be copied.
        assert!(app.run_command(b":set list").is_empty());
        assert_eq!(app.commands.list, 1);

        // Any command works, not just `set`, and the prompt is left clean.
        assert!(app.run_command(b"imap ;; <Esc>").is_empty());
        assert_eq!(app.commands.insert_maps.len(), 1);
        assert!(app.prompt.is_empty());
        assert_eq!(app.editor.mode, Mode::Normal);

        // Effects come back to the caller rather than being swallowed.
        assert_eq!(app.run_command(b"w"), [AppEffect::Save(PathBuf::from("f"))]);
        // A mistake is reported, not applied.
        assert!(app.run_command(b"set nosuchoption").is_empty());
        assert_eq!(
            app.commands.message.as_deref(),
            Some("Unknown option: nosuchoption")
        );
    }

    #[test]
    fn autoformat_rewrites_the_whitespace_and_undoes_in_one_press() {
        let messy = b"f() {\nlet x = 1;   \n  if x {\n\tbody;\n}\n}\n".to_vec();
        let mut app = App::new(messy.clone(), PathBuf::from("f.rs"));
        assert!(ex(&mut app, b"set sw=4").is_empty());

        assert!(ex(&mut app, b"autoformat").is_empty());
        assert_eq!(
            app.editor.buffer.data,
            b"f() {\n    let x = 1;\n    if x {\n        body;\n    }\n}\n"
        );
        assert_eq!(app.commands.message.as_deref(), Some("Formatted 4 lines"));
        assert!(!app.saved);
        assert!(app.editor.buffer.invariants_hold());

        // The whole rewrite is one step, and one step back.
        assert!(app.handle(Input::Byte(b'u')).is_empty());
        assert_eq!(app.editor.buffer.data, messy);
        assert!(app.saved);
        assert!(app.handle(Input::Byte(b'U')).is_empty());
        assert_eq!(
            app.editor.buffer.data,
            b"f() {\n    let x = 1;\n    if x {\n        body;\n    }\n}\n"
        );

        // A second pass has nothing left to do.
        assert!(ex(&mut app, b"autoformat").is_empty());
        assert_eq!(app.commands.message.as_deref(), Some("Already formatted"));
        // The plugin's own spelling works too.
        assert!(ex(&mut app, b"Autoformat").is_empty());
        assert_eq!(app.commands.message.as_deref(), Some("Already formatted"));
    }

    #[test]
    fn the_leader_runs_autoformat_too() {
        let messy = b"f() {\nbody;  \n}\n".to_vec();
        let mut app = App::new(messy.clone(), PathBuf::from("f.rs"));
        assert!(ex(&mut app, b"set sw=4").is_empty());

        assert!(app.handle(Input::Byte(b' ')).is_empty());
        assert!(app.handle(Input::Byte(b'f')).is_empty());
        assert_eq!(app.editor.buffer.data, b"f() {\n    body;\n}\n");
        assert_eq!(app.commands.message.as_deref(), Some("Formatted 1 line"));

        // It is the same one undo step the command produces.
        assert!(app.handle(Input::Byte(b'u')).is_empty());
        assert_eq!(app.editor.buffer.data, messy);
        assert!(app.saved);
    }

    #[test]
    fn visual_equals_formats_only_the_selected_lines() {
        let messy = b"f() {\nbad;\n  worse;\nalso bad;\n}\n".to_vec();
        let mut app = App::new(messy, PathBuf::from("f.rs"));
        assert!(ex(&mut app, b"set sw=4").is_empty());

        // Select the middle line only.
        app.editor.buffer.cursor = 11;
        assert!(app.handle(Input::Byte(b'V')).is_empty());
        assert!(app.handle(Input::Byte(b'=')).is_empty());

        assert_eq!(
            app.editor.buffer.data,
            b"f() {\nbad;\n    worse;\nalso bad;\n}\n"
        );
        assert_eq!(app.commands.message.as_deref(), Some("Formatted 1 line"));
        // `=` finishes the operator, the way `>` and `<` do.
        assert_eq!(app.editor.mode, Mode::Normal);
        assert!(app.editor.buffer.invariants_hold());

        // Still one undo step.
        assert!(app.handle(Input::Byte(b'u')).is_empty());
        assert!(app.saved);
    }

    #[test]
    fn a_charwise_selection_still_takes_the_lines_it_touches() {
        let mut app = App::new(b"f() {\nbad;\nworse;\n}\n".to_vec(), PathBuf::from("f.rs"));
        assert!(ex(&mut app, b"set sw=4").is_empty());

        // A few bytes spanning the middle of two lines, not whole ones.
        app.editor.buffer.cursor = 8;
        assert!(app.handle(Input::Byte(b'v')).is_empty());
        for _ in 0..4 {
            assert!(app.handle(Input::Byte(b'l')).is_empty());
        }
        assert!(app.handle(Input::Byte(b'=')).is_empty());

        // Indentation belongs to a line, so both lines were taken whole.
        assert_eq!(app.editor.buffer.data, b"f() {\n    bad;\n    worse;\n}\n");
    }

    #[test]
    fn each_autoformat_step_can_be_switched_off() {
        let messy = b"f() {\nbody;  \n}\n".to_vec();
        let formatted = |line: &[u8]| {
            let mut app = App::new(messy.clone(), PathBuf::from("f.rs"));
            assert!(ex(&mut app, line).is_empty());
            assert!(ex(&mut app, b"autoformat").is_empty());
            app.editor.buffer.data.clone()
        };

        assert_eq!(formatted(b"set sw=4"), b"f() {\n    body;\n}\n");
        // Without re-indenting, only the trailing whitespace goes.
        assert_eq!(
            formatted(b"set sw=4 noautoformat_autoindent"),
            b"f() {\nbody;\n}\n"
        );
        // Without the trailing step, the spaces at the end survive.
        assert_eq!(
            formatted(b"set sw=4 noautoformat_remove_trailing_spaces"),
            b"f() {\n    body;  \n}\n"
        );
        // Tabs are what a zero indent width means.
        assert_eq!(formatted(b"set sw=0"), b"f() {\n\tbody;\n}\n");

        // Retab on its own converts without re-indenting: a tab is four
        // columns, so it becomes four spaces.
        let mut tabbed = App::new(b"\tone\n".to_vec(), PathBuf::from("f.rs"));
        assert!(ex(&mut tabbed, b"set sw=4 noautoformat_autoindent").is_empty());
        assert!(ex(&mut tabbed, b"autoformat").is_empty());
        assert_eq!(tabbed.editor.buffer.data, b"    one\n");
    }

    #[test]
    fn autoformat_leaves_the_cursor_on_its_line_and_refuses_a_help_page() {
        let mut app = App::new(b"f() {\nbody;\n}\n".to_vec(), PathBuf::from("f.rs"));
        assert!(ex(&mut app, b"set sw=4").is_empty());
        // Put the cursor on the line that is about to move.
        app.editor.buffer.cursor = 6;
        assert_eq!(app.editor.buffer.cursor_row(), Some(1));
        assert!(ex(&mut app, b"autoformat").is_empty());
        // The byte it was on has moved, so it lands at the start of the line
        // it was on rather than somewhere arbitrary.
        assert_eq!(app.editor.buffer.cursor_row(), Some(1));
        assert!(app.editor.buffer.invariants_hold());

        let mut help = App::new(b"  help\n".to_vec(), PathBuf::from("general"));
        help.readonly = true;
        assert!(ex(&mut help, b"autoformat").is_empty());
        assert_eq!(help.editor.buffer.data, b"  help\n");
        assert_eq!(
            help.commands.message.as_deref(),
            Some("Buffer is read-only")
        );
    }

    #[test]
    fn control_r_toggles_and_shares_the_pane_with_the_explorer() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-panes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, b"body").unwrap();

        let mut app = App::new(b"start".to_vec(), PathBuf::from("start.txt"));
        app.record_recent(&file);

        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(app.recent_open);
        // Ctrl-R again closes it, and Escape does too.
        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(!app.recent_open);
        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(app.handle(Input::Escape).is_empty());
        assert!(!app.recent_open);

        // Only one full-pane list can be up at a time.
        app.open_explorer(&root).unwrap();
        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(app.recent_open);
        assert!(app.explorer.is_none());
        assert!(app.handle(Input::Control(14)).is_empty());
        assert!(!app.recent_open);
        assert!(app.explorer.is_some());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_recent_entry_that_has_been_deleted_is_never_offered() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-stale-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, b"body").unwrap();

        let mut app = App::new(b"start".to_vec(), PathBuf::from("start.txt"));
        app.record_recent(&file);
        std::fs::remove_file(&file).unwrap();

        // Pruning happens as the picker opens, so the only entry disappears.
        assert!(app.handle(Input::Control(18)).is_empty());
        assert!(!app.recent_open);
        assert!(app.recent.is_empty());
        assert_eq!(app.commands.message.as_deref(), Some("No recent files"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_an_explorer_file_resets_the_saved_baseline() {
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-app-explorer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let opened = root.join("opened.txt");
        std::fs::write(&opened, b"from disk").unwrap();

        let mut app = App::new(b"dirty old data".to_vec(), PathBuf::from("old.md"));
        app.saved = false;
        app.open_explorer(&root).unwrap();
        app.explorer.as_mut().unwrap().cursor = app
            .explorer
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .position(|entry| entry.path == opened)
            .unwrap();

        assert!(app.handle(Input::Enter).is_empty());
        assert_eq!(app.editor.buffer.data, b"from disk");
        assert_eq!(app.filename, opened);
        assert!(app.saved);
        // Enter still opens the selection instead of toggling markdown, and
        // the new file decides the display for itself.
        assert!(!app.markdown);

        std::fs::remove_dir_all(root).unwrap();
    }
}
