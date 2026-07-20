use std::io;
use std::process::Command;

/// Execute command text through the platform's `sh -c` compatibility boundary.
///
/// The process exit status and stderr are deliberately not surfaced here. Cano's
/// status message is the final stdout line read by the legacy `fgets` loop.
pub fn run_shell(command: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command_arg(command))
        .output()?;
    Ok(last_stdout_line(&output.stdout))
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

fn last_stdout_line(stdout: &[u8]) -> Option<Vec<u8>> {
    stdout
        .split_inclusive(|byte| *byte == b'\n')
        .next_back()
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_line_extraction_handles_every_line_ending_shape() {
        assert_eq!(last_stdout_line(b""), None);
        assert_eq!(last_stdout_line(b"only"), Some(b"only".to_vec()));
        assert_eq!(last_stdout_line(b"only\n"), Some(b"only\n".to_vec()));
        assert_eq!(last_stdout_line(b"first\nlast"), Some(b"last".to_vec()));
        assert_eq!(last_stdout_line(b"first\nlast\n"), Some(b"last\n".to_vec()));
        assert_eq!(last_stdout_line(b"first\n\n"), Some(b"\n".to_vec()));
        assert_eq!(last_stdout_line(b"\n"), Some(b"\n".to_vec()));
        assert_eq!(
            last_stdout_line(b"windows\r\n"),
            Some(b"windows\r\n".to_vec())
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_adapter_returns_the_last_stdout_line() {
        assert_eq!(
            run_shell(b"printf 'first\\nlast\\n'").unwrap(),
            Some(b"last\n".to_vec())
        );
        assert_eq!(
            run_shell(b"printf 'first\\nlast'").unwrap(),
            Some(b"last".to_vec())
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_adapter_returns_none_when_stdout_is_empty() {
        assert_eq!(run_shell(b":").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn shell_adapter_ignores_stderr_and_nonzero_status() {
        assert_eq!(run_shell(b"printf 'problem\\n' >&2; exit 7").unwrap(), None);
        assert_eq!(
            run_shell(b"printf 'visible\\n'; printf 'hidden\\n' >&2; exit 9").unwrap(),
            Some(b"visible\n".to_vec())
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_metacharacters_are_interpreted_by_sh_c() {
        assert_eq!(
            run_shell(b"value=cano; printf '%s\\n' \"$value\" | tr a-z A-Z").unwrap(),
            Some(b"CANO\n".to_vec())
        );
    }

    #[cfg(unix)]
    #[test]
    fn embedded_nul_is_reported_as_an_invalid_process_argument() {
        assert_eq!(
            run_shell(b"printf ok\0printf bad").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
