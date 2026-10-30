//! `:!command`, run through `sh -c`.
//!
//! The editor only ever shows one line of what a command printed, so the
//! whole of its output is captured and boiled down by [`summarize`]: the last
//! line that says anything, from stdout or else from stderr, with a failing
//! exit status in front of it.  A command that writes nothing to stdout and
//! fails is exactly the one whose stderr matters, and dropping both -- as the
//! legacy `fgets` loop did -- left `:!make` saying nothing at all on failure.
//!
//! While the terminal is up a command runs as a [`ShellJob`]: the process is
//! started on the main thread, so a shell that cannot be spawned is reported
//! at once, and waited on by a worker thread, so a slow command never holds
//! the screen still.  The main loop collects the result between frames, the
//! same way the assistant's own `run_shell` tool is collected.

use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How a finished command ended, reduced to what the status line shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    Code(i32),
    /// Killed by this signal before it could exit.
    Signal(i32),
    /// Neither, which only a platform without signals could produce.
    Unknown,
}

impl Exit {
    fn from_status(status: ExitStatus) -> Self {
        if let Some(code) = status.code() {
            return Self::Code(code);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Signal(signal);
            }
        }
        Self::Unknown
    }
}

/// Starts `sh -c command` with its output captured and no terminal input.
///
/// Stdin is closed rather than inherited: the editor owns the terminal in raw
/// mode, and a command reading from it would steal keystrokes from the
/// editor -- or, on the worker thread, wait forever on keys the editor reads
/// first.
fn spawn(command: &[u8]) -> io::Result<Child> {
    Command::new("sh")
        .arg("-c")
        .arg(command_arg(command))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Waits for a started command and summarises how it went.
fn finish(child: Child) -> io::Result<Option<String>> {
    let output = child.wait_with_output()?;
    Ok(summarize(
        Exit::from_status(output.status),
        &output.stdout,
        &output.stderr,
    ))
}

/// Runs a command to completion on the calling thread.
///
/// Used before the terminal is opened, for `cano.command("!…")` lines in the
/// configuration: there is no screen to keep responsive yet, and running them
/// in order is what lets a later line rely on an earlier one having finished.
pub fn run_shell(command: &[u8]) -> io::Result<Option<String>> {
    finish(spawn(command)?)
}

/// The one line the status bar shows for a finished command, or `None` when
/// it succeeded without printing anything worth showing.
///
/// The last non-blank line of stdout wins, falling back to stderr's, because
/// that is where a command puts its verdict -- `make`'s final error, `wc`'s
/// total.  A failing status is prefixed as `[exit N]` (or `[signal N]`), so
/// a failure is visible even when the line itself reads like success.
pub fn summarize(exit: Exit, stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let line = last_line(stdout).or_else(|| last_line(stderr));
    let status = match exit {
        Exit::Code(0) => None,
        Exit::Code(code) => Some(format!("[exit {code}]")),
        Exit::Signal(signal) => Some(format!("[signal {signal}]")),
        Exit::Unknown => Some("[exit ?]".to_owned()),
    };
    let summary = match (status, line) {
        (None, None) => None,
        (None, Some(line)) => Some(line),
        (Some(status), None) => Some(status),
        (Some(status), Some(line)) => Some(format!("{status} {line}")),
    };
    // Post: whatever is shown is a single line; a line break here would be
    // drawn as a control character in the middle of the status bar.
    debug_assert!(
        summary
            .as_deref()
            .is_none_or(|text| !text.contains(['\n', '\r']))
    );
    summary
}

/// The last line of `bytes` that is not blank, without its line ending or
/// trailing whitespace.
///
/// A carriage return inside a line is a progress meter redrawing itself, so
/// only what follows the last one is kept -- what a terminal would have been
/// left showing.
fn last_line(bytes: &[u8]) -> Option<String> {
    bytes
        .split(|byte| *byte == b'\n')
        .map(|line| line.trim_ascii_end())
        .rfind(|line| !line.is_empty())
        .map(|line| {
            let shown = line.rsplit(|byte| *byte == b'\r').next().unwrap_or(line);
            String::from_utf8_lossy(shown).into_owned()
        })
}

/// The command language is byte based, so non-UTF-8 paths must survive the
/// trip to `sh` wherever the platform allows arbitrary argument bytes.
#[cfg(unix)]
fn command_arg(command: &[u8]) -> &std::ffi::OsStr {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::OsStr::from_bytes(command)
}

#[cfg(not(unix))]
fn command_arg(command: &[u8]) -> std::ffi::OsString {
    String::from_utf8_lossy(command).into_owned().into()
}

/// What a worker thread hands back: the summary, or why waiting failed.
type Slot = Arc<Mutex<Option<io::Result<Option<String>>>>>;

/// A `:!` command running in the background.
#[derive(Debug)]
pub struct ShellJob {
    /// The command as typed, for the status line while it runs.
    pub command: String,
    pub started: Instant,
    slot: Slot,
}

impl ShellJob {
    /// Starts `command` and hands the wait to a worker thread.
    ///
    /// The spawn itself happens here, so a shell that cannot be started is
    /// an error returned to the caller straight away rather than a result
    /// that turns up a frame later.
    pub fn start(command: &[u8]) -> io::Result<Self> {
        let child = spawn(command)?;
        let slot: Slot = Arc::default();
        let writer = Arc::clone(&slot);
        std::thread::spawn(move || {
            let result = finish(child);
            *writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        });
        Ok(Self {
            command: String::from_utf8_lossy(command).trim().to_owned(),
            started: Instant::now(),
            slot,
        })
    }

    /// The result, once the command has finished; `None` while it runs.
    /// It is handed over once, so a second call after that is `None` too.
    pub fn poll(&self) -> Option<io::Result<Option<String>>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// What the status line says while the command runs.
    pub fn running_message(&self) -> String {
        let elapsed = self.started.elapsed().as_secs();
        if elapsed == 0 {
            format!("Running: {}", self.command)
        } else {
            format!("Running: {} ({elapsed}s)", self.command)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_is_the_last_non_blank_stdout_line() {
        let ok = Exit::Code(0);
        assert_eq!(summarize(ok, b"", b""), None);
        assert_eq!(summarize(ok, b"only", b""), Some("only".to_owned()));
        assert_eq!(summarize(ok, b"only\n", b""), Some("only".to_owned()));
        assert_eq!(summarize(ok, b"first\nlast", b""), Some("last".to_owned()));
        assert_eq!(
            summarize(ok, b"first\nlast\n\n  \n", b""),
            Some("last".to_owned())
        );
        assert_eq!(
            summarize(ok, b"windows\r\n", b""),
            Some("windows".to_owned())
        );
        // Blank output is no output: nothing to show for a quiet success.
        assert_eq!(summarize(ok, b"\n\n", b" \t\n"), None);
        // Leading indentation is part of what was printed.
        assert_eq!(
            summarize(ok, b"  indented\n", b""),
            Some("  indented".to_owned())
        );
    }

    #[test]
    fn the_summary_falls_back_to_stderr_only_when_stdout_is_silent() {
        assert_eq!(
            summarize(Exit::Code(0), b"", b"warning: odd\n"),
            Some("warning: odd".to_owned())
        );
        assert_eq!(
            summarize(Exit::Code(0), b"visible\n", b"hidden\n"),
            Some("visible".to_owned())
        );
    }

    #[test]
    fn a_failing_status_is_shown_in_front_of_the_line() {
        assert_eq!(
            summarize(Exit::Code(2), b"", b"error: foo\n"),
            Some("[exit 2] error: foo".to_owned())
        );
        assert_eq!(
            summarize(Exit::Code(1), b"partial\n", b"error\n"),
            Some("[exit 1] partial".to_owned())
        );
        // A failure with nothing printed still says it failed.
        assert_eq!(
            summarize(Exit::Code(7), b"", b""),
            Some("[exit 7]".to_owned())
        );
        assert_eq!(
            summarize(Exit::Signal(9), b"", b""),
            Some("[signal 9]".to_owned())
        );
        assert_eq!(
            summarize(Exit::Unknown, b"x\n", b""),
            Some("[exit ?] x".to_owned())
        );
    }

    #[test]
    fn a_progress_meter_shows_only_where_it_ended() {
        assert_eq!(
            summarize(Exit::Code(0), b"progress 10%\rprogress 100%\n", b""),
            Some("progress 100%".to_owned())
        );
        assert_eq!(
            summarize(Exit::Code(0), b"\xffbad utf8\n", b""),
            Some("\u{fffd}bad utf8".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_adapter_returns_the_last_stdout_line() {
        assert_eq!(
            run_shell(b"printf 'first\\nlast\\n'").unwrap(),
            Some("last".to_owned())
        );
        assert_eq!(run_shell(b":").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn shell_adapter_reports_stderr_and_a_nonzero_status() {
        assert_eq!(
            run_shell(b"printf 'problem\\n' >&2; exit 7").unwrap(),
            Some("[exit 7] problem".to_owned())
        );
        assert_eq!(
            run_shell(b"printf 'visible\\n'; printf 'hidden\\n' >&2; exit 9").unwrap(),
            Some("[exit 9] visible".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_metacharacters_are_interpreted_by_sh_c() {
        assert_eq!(
            run_shell(b"value=cano; printf '%s\\n' \"$value\" | tr a-z A-Z").unwrap(),
            Some("CANO".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_command_gets_no_terminal_input() {
        // `cat` reading an inherited terminal would wait for keys forever.
        assert_eq!(
            run_shell(b"cat; echo done").unwrap(),
            Some("done".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn embedded_nul_is_reported_as_an_invalid_process_argument() {
        assert_eq!(
            run_shell(b"printf ok\0printf bad").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            ShellJob::start(b"printf ok\0printf bad")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_background_job_is_collected_once_it_finishes() {
        let job = ShellJob::start(b"  sleep 0.2; echo late >&2; exit 3 ").unwrap();
        assert_eq!(job.command, "sleep 0.2; echo late >&2; exit 3");
        assert!(job.running_message().starts_with("Running: sleep 0.2"));
        // The start returned without waiting for the sleep.
        assert!(job.poll().is_none());
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            assert!(Instant::now() < deadline, "the job never finished");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(result.unwrap(), Some("[exit 3] late".to_owned()));
        assert!(job.poll().is_none(), "a result is handed over once");
    }
}
