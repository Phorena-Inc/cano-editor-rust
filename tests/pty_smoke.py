"""Disposable PTY smoke test for the freshly implemented Cano binary."""

import fcntl
import os
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b[()][B0]|[\x00-\x08\x0b-\x1f]")
ORIGINAL = b"alpha\nbeta\ngamma\n"


def resize(fd, rows, columns):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def drain(fd, timeout=0.1):
    chunks = []
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], min(0.05, deadline - time.monotonic()))
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        chunks.append(chunk)
    return b"".join(chunks)


def visible(output):
    return ANSI.sub(b"", output)


def read_until(fd, needles, timeout=3.0):
    if isinstance(needles, bytes):
        needles = (needles,)
    chunks = []
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], min(0.1, deadline - time.monotonic()))
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        chunks.append(chunk)
        output = visible(b"".join(chunks))
        if all(needle in output for needle in needles):
            return output
    output = visible(b"".join(chunks))
    raise AssertionError(f"timed out waiting for PTY output {needles!r}; tail={output[-300:]!r}")


def wait_file(path, expected, timeout=3.0):
    deadline = time.monotonic() + timeout
    actual = None
    while time.monotonic() < deadline:
        with open(path, "rb") as source:
            actual = source.read()
        if actual == expected:
            return
        time.sleep(0.02)
    raise AssertionError(f"saved bytes differ: {actual!r}")


class PtySession:
    def __init__(self, binary, home, work, *arguments, env=None):
        self.pid, self.master = os.forkpty()
        self.status = None
        if self.pid == 0:
            os.environ.clear()
            os.environ.update({"HOME": home, "TERM": "xterm-256color", "PATH": "/usr/bin:/bin"})
            os.environ.update(env or {})
            os.chdir(work)
            os.execv(binary, [binary, *arguments])
        resize(self.master, 24, 80)

    def send(self, keys):
        os.write(self.master, keys)

    def wait_exit(self, timeout=3.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done:
                self.status = status
                code = os.waitstatus_to_exitcode(status)
                if code != 0:
                    raise AssertionError(f"child exited with status {code}")
                return
            drain(self.master, 0.05)
        raise AssertionError("child did not exit")

    def is_alive(self):
        done, status = os.waitpid(self.pid, os.WNOHANG)
        if done:
            self.status = status
            return False
        return True

    def close(self):
        if self.status is None:
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done:
                self.status = status
            else:
                os.kill(self.pid, signal.SIGKILL)
                _, self.status = os.waitpid(self.pid, 0)
        os.close(self.master)

    def __enter__(self):
        return self

    def __exit__(self, _kind, _value, _traceback):
        self.close()


def write_target(path):
    with open(path, "wb") as output:
        output.write(ORIGINAL)


def command(session, name):
    drain(session.master)
    session.send(b":" + name + b"\r")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: pty_smoke.py /path/to/cano")
    binary = os.path.abspath(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix="cano-fresh-home-") as home, tempfile.TemporaryDirectory(
        prefix="cano-fresh-work-"
    ) as work:
        config_dir = os.path.join(home, ".config", "cano")
        os.makedirs(config_dir)
        with open(os.path.join(config_dir, "init.lua"), "w", encoding="utf-8") as config:
            config.write("setup({})")

        target = os.path.join(work, "sample.txt")
        write_target(target)
        with PtySession(binary, home, work, target) as session:
            read_until(session.master, (b"sample.txt", b"alpha"))
            session.send(b"ifresh \x1b")
            read_until(session.master, b"Modified")

            drain(session.master)
            resize(session.master, 12, 40)
            os.kill(session.pid, signal.SIGWINCH)
            if not drain(session.master, 1.0):
                raise AssertionError("shrink resize produced no redraw")
            resize(session.master, 40, 120)
            os.kill(session.pid, signal.SIGWINCH)
            read_until(session.master, (b"fresh", b"gamma"))

            command(session, b"q")
            read_until(session.master, b"No write since last change")
            if not session.is_alive():
                raise AssertionError(":q exited with unsaved changes")
            wait_file(target, ORIGINAL)

            command(session, b"w")
            first_save = b"fresh " + ORIGINAL
            wait_file(target, first_save)
            read_until(session.master, b"Saved")

            session.send(b"iagain \x1b")
            read_until(session.master, b"Modified")
            command(session, b"wq")
            session.wait_exit()
            wait_file(target, b"fresh again " + ORIGINAL)

        # `-h` is an alias for `--help`: both open the bundled general page.
        # The pages are only on disk when the smoke test runs from a checkout,
        # so an installed binary skips this.
        help_dir = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs", "help"
        )
        if os.path.isdir(help_dir):
            for spelling in ("-h", "--help"):
                with PtySession(
                    binary, home, work, spelling, env={"CANO_HELP_DIR": help_dir}
                ) as session:
                    # The usage block names every flag, so this also catches
                    # docs/help/general drifting from the parser.
                    read_until(session.master, (b"general", b"Usage", b"--version"))
                    command(session, b"q")
                    session.wait_exit()

        discard = os.path.join(work, "discard.txt")
        write_target(discard)
        with PtySession(binary, home, work, discard) as session:
            read_until(session.master, (b"discard.txt", b"alpha"))
            session.send(b"idiscard \x1b")
            read_until(session.master, b"Modified")
            command(session, b"q!")
            session.wait_exit()
            wait_file(discard, ORIGINAL)

    print("PASS: resize, :q refusal, :w, :wq, :q!, -h/--help help page")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as error:
        print("FAIL:", error)
        raise SystemExit(1)
