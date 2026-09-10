use std::collections::VecDeque;
use std::io::{self, Stdout, stdout};
use std::time::{Duration, Instant};

use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::editor::Mode;

const SIZE_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// What the mouse did, reduced to the four gestures Cano acts on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseKind {
    Press,
    Drag,
    ScrollUp,
    ScrollDown,
}

/// One mouse gesture, in screen cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    Byte(u8),
    Mouse(Mouse),
    Control(u8),
    Escape,
    Enter,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    Insert,
    PageUp,
    PageDown,
    Resize,
    Unsupported,
}

pub fn translate_key(key: KeyEvent) -> Input {
    match key.code {
        KeyCode::Char(character)
            if character.is_ascii() && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Input::Control((character as u8) & 0x1f)
        }
        KeyCode::Char(character) if character.is_ascii() => Input::Byte(character as u8),
        KeyCode::Esc => Input::Escape,
        KeyCode::Enter => Input::Enter,
        KeyCode::Tab => Input::Byte(b'\t'),
        KeyCode::Backspace => Input::Backspace,
        KeyCode::Left => Input::Left,
        KeyCode::Right => Input::Right,
        KeyCode::Up => Input::Up,
        KeyCode::Down => Input::Down,
        KeyCode::Home => Input::Home,
        KeyCode::End => Input::End,
        KeyCode::Delete => Input::Delete,
        KeyCode::Insert => Input::Insert,
        KeyCode::PageUp => Input::PageUp,
        KeyCode::PageDown => Input::PageDown,
        _ => Input::Unsupported,
    }
}

/// Reduces a crossterm mouse event to a gesture, or `None` for the ones Cano
/// ignores.
///
/// Button releases and bare motion are dropped rather than delivered: an
/// unhandled input still clears a pending operator, so a mouse merely crossing
/// the window would cancel a half-typed `d`.
pub fn translate_mouse(mouse: MouseEvent) -> Option<Mouse> {
    let kind = match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Press,
        MouseEventKind::Drag(MouseButton::Left) => MouseKind::Drag,
        MouseEventKind::ScrollUp => MouseKind::ScrollUp,
        MouseEventKind::ScrollDown => MouseKind::ScrollDown,
        _ => return None,
    };
    Some(Mouse {
        kind,
        column: mouse.column,
        row: mouse.row,
    })
}

/// The UTF-8 bytes a key outside ASCII contributes to the buffer.
///
/// Cano's buffer is bytes, so such a key is delivered as the bytes that spell
/// it rather than dropped: `translate_key` has only one byte to give and
/// cannot carry a character that needs several.
pub fn key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    match key.code {
        KeyCode::Char(character)
            if !character.is_ascii() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Some(character.to_string().into_bytes())
        }
        _ => None,
    }
}

pub struct TerminalSession {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    last_size: (u16, u16),
    mouse: bool,
    /// The remaining bytes of a multi-byte key, still to be delivered.
    pending: VecDeque<u8>,
}

/// Restores the terminal before the default panic output runs.
///
/// The release profile aborts on panic, so `Drop for TerminalSession` never
/// runs on that path; without this hook a panic leaves the user's shell in
/// raw mode on the alternate screen, and the panic message is either
/// invisible or erased along with that screen.
/// Takes the terminal: raw mode, the alternate screen and a block cursor.
fn enter_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    if let Err(error) = execute!(stdout(), EnterAlternateScreen, SetCursorStyle::SteadyBlock) {
        release_terminal();
        return Err(error);
    }
    Ok(())
}

/// Gives the terminal back, exactly as it was taken.
///
/// Every path out goes through here -- the panic hook, `Drop`, a failed
/// start, and Ctrl-Z -- so none of them can drift apart from the others.
fn release_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        Show
    );
}

fn install_panic_hook() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            release_terminal();
            default(info);
        }));
    });
}

impl TerminalSession {
    pub fn start() -> io::Result<Self> {
        install_panic_hook();
        enter_terminal()?;

        let terminal = match Terminal::new(CrosstermBackend::new(stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                release_terminal();
                return Err(error);
            }
        };
        let mut session = Self {
            terminal,
            last_size: (0, 0),
            mouse: false,
            pending: VecDeque::new(),
        };
        session.last_size = size()?;
        Ok(session)
    }

    /// Throws away what the terminal is showing so the next frame is painted
    /// from nothing, for vim's Ctrl-L.
    ///
    /// `Terminal::clear` would be the obvious call, but it snapshots the
    /// cursor first by asking the terminal where it is and waiting for the
    /// reply on the same input the editor reads its keys from.  A terminal
    /// that answers late puts that reply in front of the next keystroke, and
    /// one that never answers -- some multiplexers, or input that is not a
    /// terminal at all -- hangs the editor on a key that is meant to be a
    /// no-op.  Resizing to the size already in force clears the screen and
    /// resets the back buffer without asking the terminal anything.
    pub fn redraw(&mut self) -> io::Result<()> {
        let size = self.terminal.size()?;
        self.terminal.resize(size.into())
    }

    /// Stops the editor and hands the terminal back to the shell, the way
    /// Ctrl-Z does everywhere else.
    ///
    /// Raw mode turns off the terminal's own signal generation, so Ctrl-Z
    /// arrives as an ordinary key and the stop has to be asked for. The
    /// terminal is given back first, so the shell that takes over finds it as
    /// it left it, and taken again when `fg` resumes this call.
    pub fn suspend(&mut self) -> io::Result<()> {
        release_terminal();
        // Mouse reporting went with the terminal; the main loop reinstates it
        // from the `mouse` option on its next pass.
        self.mouse = false;

        stop_process();

        enter_terminal()?;
        // The screen belonged to the shell in the meantime, and may have been
        // resized while this process was stopped, so nothing drawn before can
        // be assumed to still be there.
        //
        // `Terminal::clear` would be the obvious way to say so, but it first
        // asks the terminal where the cursor is and waits for the reply.  A
        // terminal that is slow to answer, or does not, would take the editor
        // down on the way back from a suspend.  Declaring an impossible area
        // instead makes the next draw notice the size does not match, resize
        // for real, and repaint everything -- with nothing to wait for.
        self.terminal.resize(Rect::ZERO)?;
        self.last_size = size()?;
        Ok(())
    }

    /// Turns terminal mouse reporting on or off to match the `mouse` option.
    ///
    /// While it is on the terminal hands the mouse to Cano, which means its
    /// own click-to-select stops working; most terminals still offer it with
    /// Shift held.
    pub fn set_mouse(&mut self, enabled: bool) -> io::Result<()> {
        if self.mouse == enabled {
            return Ok(());
        }
        self.mouse = enabled;
        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)
        }
    }

    fn size_changed(&mut self) -> io::Result<bool> {
        let current = size()?;
        let changed = current != self.last_size;
        self.last_size = current;
        Ok(changed)
    }

    fn poll_input(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        // The rest of a multi-byte key comes out before anything new is read,
        // so its bytes stay adjacent -- an `:imap` run or an insertion must
        // not have another key land in the middle of one character.
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(Input::Byte(byte)));
        }
        if event::poll(timeout)? {
            let input = self.next_input()?;
            let _ = self.size_changed()?;
            return Ok(Some(input));
        }
        Ok(self.size_changed()?.then_some(Input::Resize))
    }

    /// Reads one event, queueing the tail of a key that spells more than one
    /// byte and returning its first.
    fn next_input(&mut self) -> io::Result<Input> {
        loop {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Release => continue,
                Event::Key(key) => {
                    let Some(bytes) = key_bytes(key) else {
                        return Ok(translate_key(key));
                    };
                    let mut bytes = bytes.into_iter();
                    let Some(first) = bytes.next() else {
                        continue;
                    };
                    self.pending.extend(bytes);
                    return Ok(Input::Byte(first));
                }
                Event::Mouse(mouse) => match translate_mouse(mouse) {
                    Some(mouse) => return Ok(Input::Mouse(mouse)),
                    None => continue,
                },
                Event::Resize(_, _) => return Ok(Input::Resize),
                _ => return Ok(Input::Unsupported),
            }
        }
    }

    pub fn read_input(&mut self) -> io::Result<Input> {
        loop {
            if let Some(input) = self.poll_input(SIZE_CHECK_INTERVAL)? {
                return Ok(input);
            }
        }
    }

    pub fn read_input_before(&mut self, deadline: Instant) -> io::Result<Option<Input>> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            if let Some(input) = self.poll_input(remaining.min(SIZE_CHECK_INTERVAL))? {
                return Ok(Some(input));
            }
        }
    }

    pub fn update_cursor(&mut self, mode: Mode) -> io::Result<()> {
        if mode == Mode::Visual {
            self.terminal.hide_cursor()?;
        } else {
            self.terminal.show_cursor()?;
        }
        execute!(
            self.terminal.backend_mut(),
            if mode == Mode::Insert {
                SetCursorStyle::BlinkingBar
            } else {
                SetCursorStyle::DefaultUserShape
            }
        )
    }
}

/// Stops this process, and returns when something resumes it.
///
/// `SIGTSTP` is what a terminal raises for Ctrl-Z, and its default action is
/// to stop the process; the shell reports it as stopped and `fg` continues it
/// from here. The standard library has no way to raise a signal.
#[cfg(unix)]
fn stop_process() {
    // Safety: `raise` takes a signal number and touches nothing else.
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
}

/// Windows has no job control to suspend into, so Ctrl-Z simply redraws.
#[cfg(not(unix))]
fn stop_process() {}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        release_terminal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_ascii_control_and_navigation_input() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Input::Byte(b'x')
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Input::Control(19)
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Input::Backspace
        );
        // A key outside ASCII has no single byte to translate to; the session
        // delivers the bytes that spell it instead.
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE)),
            Input::Unsupported
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE)),
            Some("é".as_bytes().to_vec())
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Char('\u{25b8}'), KeyModifiers::NONE)),
            Some("\u{25b8}".as_bytes().to_vec())
        );
        // ASCII and chords still go through `translate_key`.
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::CONTROL)),
            None
        );

        let special_keys = [
            (KeyCode::Left, Input::Left),
            (KeyCode::Right, Input::Right),
            (KeyCode::Up, Input::Up),
            (KeyCode::Down, Input::Down),
            (KeyCode::Home, Input::Home),
            (KeyCode::End, Input::End),
            (KeyCode::Delete, Input::Delete),
            (KeyCode::Insert, Input::Insert),
            (KeyCode::PageUp, Input::PageUp),
            (KeyCode::PageDown, Input::PageDown),
        ];
        for (key, expected) in special_keys {
            assert_eq!(
                translate_key(KeyEvent::new(key, KeyModifiers::NONE)),
                expected
            );
        }
    }
}
