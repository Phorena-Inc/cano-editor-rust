use std::path::{Path, PathBuf};

use crate::command::{Action, CommandState, ExternalEffect, key, lex, parse};
use crate::editor::{Editor, Leader, Mode, MoveDirection};
use crate::explorer::{Explorer, Selection};
use crate::io::load_buffer;
use crate::terminal::Input;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppEffect {
    Save(PathBuf),
    Shell(Vec<u8>),
    Quit,
}

#[derive(Clone, Debug)]
pub struct App {
    pub editor: Editor,
    pub commands: CommandState,
    pub prompt: Vec<u8>,
    pub prompt_cursor: usize,
    pub filename: PathBuf,
    pub explorer: Option<Explorer>,
    pub count: Vec<u8>,
    pub pending_replace: bool,
    pub saved: bool,
    pub message_pending: bool,
    /// Refuses writes; used for the built-in help pages so a save-and-quit
    /// cannot overwrite the installed documentation.
    pub readonly: bool,
    /// The needle used by `n`.  Kept separately from the prompt, which is
    /// cleared whenever a `:` command or Escape resets the prompt line.
    last_search: Vec<u8>,
    saved_buffer: Vec<u8>,
    buffer_replaced: bool,
}

impl App {
    pub fn new(bytes: Vec<u8>, filename: PathBuf) -> Self {
        let saved_buffer = bytes.clone();
        Self {
            editor: Editor::new(bytes),
            commands: CommandState::new(Vec::new()),
            prompt: Vec::new(),
            prompt_cursor: 0,
            filename,
            explorer: None,
            count: Vec::new(),
            pending_replace: false,
            saved: true,
            message_pending: false,
            readonly: false,
            last_search: Vec::new(),
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

        if self.editor.mode == Mode::Normal
            && let Some(key) = mapping_key(input)
            && let Some(expansion) = self.commands.mapping(key).map(|bytes| bytes.to_vec())
        {
            if depth >= 64 {
                self.set_message("Recursive key map");
                return Vec::new();
            }
            let mut effects = Vec::new();
            for byte in expansion {
                // Translate replayed bytes back into the key variants the
                // mode handlers expect, and drop the legacy NUL terminator
                // (executing it would insert a literal 0x00).
                let step = match byte {
                    0 => continue,
                    10 | 13 => Input::Enter,
                    27 => Input::Escape,
                    127 => Input::Backspace,
                    byte => Input::Byte(byte),
                };
                effects.extend(self.handle_mapped(step, depth + 1));
            }
            return effects;
        }

        match self.editor.mode {
            Mode::Normal => self.normal_input(input),
            Mode::Insert => self.insert_input(input),
            Mode::Visual => self.visual_input(input),
            Mode::Search => self.search_input(input),
            Mode::Command => self.command_input(input),
        }
    }

    fn normal_input(&mut self, input: Input) -> Vec<AppEffect> {
        if input == Input::Control(14) {
            if self.explorer.is_some() {
                self.explorer = None;
            } else if let Err(error) = self.open_explorer(Path::new(".")) {
                self.set_message(error.to_string());
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
        self.editor.buffer.cursor = self.editor.buffer.search_wrapped(&self.last_search);
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

    fn insert_input(&mut self, input: Input) -> Vec<AppEffect> {
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
                self.editor.insert_tab();
            }
            Input::Byte(byte) => {
                self.editor.insert_byte(byte);
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
                    self.last_search = needle;
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

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.commands.message = Some(message.into());
        self.message_pending = true;
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
            Some(Selection::File(path)) => match load_buffer(&path) {
                Ok(bytes) => {
                    self.editor = Editor::new(bytes);
                    self.editor.indent = self.commands.indent.max(0) as usize;
                    self.filename = path.clone();
                    self.commands.output.clear();
                    self.explorer = None;
                    self.saved = true;
                    self.buffer_replaced = true;
                }
                Err(error) => self.set_message(error.to_string()),
            },
            None => {}
        }
    }
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
        Input::Resize | Input::Unsupported => None,
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

        let mut app = App::new(b"dirty old data".to_vec(), PathBuf::from("old.txt"));
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

        std::fs::remove_dir_all(root).unwrap();
    }
}
