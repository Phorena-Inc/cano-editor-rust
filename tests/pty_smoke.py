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


VIM_KEYS = b"alpha 41 beta\nabcd\nefgh\nijkl\nvalue verify\n"


def vim_keys(binary, home, work):
    """The Control-key commands, driven through a real terminal.

    Most of them are covered by the unit tests, which drive the same
    handlers directly.  What only a terminal can show is whether a key that
    reaches outside the buffer behaves: Ctrl-L repaints the screen, and the
    obvious way to do that asks the terminal where its cursor is and waits
    on the same input the editor reads keys from.
    """
    target = os.path.join(work, "keys.txt")

    def session_with(keys, expect):
        with open(target, "wb") as handle:
            handle.write(VIM_KEYS)
        with PtySession(binary, home, work, target) as session:
            read_until(session.master, (b"keys.txt", b"alpha"))
            for chunk in keys:
                drain(session.master, 0.05)
                session.send(chunk)
                time.sleep(0.1)
            session.send(b":wq\r")
            session.wait_exit()
        wait_file(target, expect)

    # Ctrl-A adds one; a count multiplies what Ctrl-X takes off.
    session_with(
        [b"\x01", b"3", b"\x18"],
        b"alpha 39 beta\nabcd\nefgh\nijkl\nvalue verify\n",
    )
    # Ctrl-R redoes what u took back.
    session_with(
        [b"x", b"u", b"\x12"],
        b"lpha 41 beta\nabcd\nefgh\nijkl\nvalue verify\n",
    )
    # Ctrl-V cuts a rectangle out of the three short rows.
    session_with(
        [b"j", b"l", b"\x16", b"jj", b"l", b"d"],
        b"alpha 41 beta\nad\neh\nil\nvalue verify\n",
    )
    # Insert mode: Ctrl-W takes back a word, Ctrl-N completes one, and
    # Ctrl-O runs a single Normal-mode command without leaving Insert.
    session_with(
        [b"A", b"\x17", b"\x1b"],
        b"alpha 41 \nabcd\nefgh\nijkl\nvalue verify\n",
    )
    session_with(
        [b"jjjj", b"A", b" ver", b"\x0e", b"\x1b"],
        b"alpha 41 beta\nabcd\nefgh\nijkl\nvalue verify verify\n",
    )
    session_with(
        [b"i", b"\x0f", b"$", b"!", b"\x1b"],
        b"alpha 41 beta!\nabcd\nefgh\nijkl\nvalue verify\n",
    )

    # Ctrl-L has to repaint without asking the terminal anything: a query
    # that goes unanswered would hang the editor here rather than redraw.
    with open(target, "wb") as handle:
        handle.write(VIM_KEYS)
    with PtySession(binary, home, work, target) as session:
        read_until(session.master, (b"keys.txt", b"alpha"))
        session.send(b"\x07")
        read_until(session.master, b"lines --")
        session.send(b"\x0c")
        # Cells that are blank are skipped rather than written, so the
        # spaces between words never reach the terminal: match on words.
        read_until(session.master, (b"alpha", b"verify"))
        session.send(b"\x11")
        session.wait_exit()


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
            # The session runs in a temp directory, so the unset case only
            # finds the pages by way of the binary's own location.  It used to
            # look for a bare `docs/help` and fail everywhere but a checkout
            # root.  The set case proves the override still wins.
            for spelling, env in (("-h", None), ("--help", {"CANO_HELP_DIR": help_dir})):
                with PtySession(binary, home, work, spelling, env=env) as session:
                    # The usage block names every flag, so this also catches
                    # docs/help/general drifting from the parser.
                    read_until(session.master, (b"general", b"Usage", b"--version"))
                    command(session, b"q")
                    session.wait_exit()

        # The comment toggle on both spellings, round-tripped: the second
        # pass has to put the buffer back exactly as the first found it.
        source = os.path.join(work, "block.rs")
        for chord in (b"\x05", b" e"):
            with open(source, "wb") as handle:
                handle.write(b"fn main() {\n    body();\n}\n")
            with PtySession(binary, home, work, source) as session:
                # Unchanged cells are not redrawn, so needles cannot span a space.
                read_until(session.master, (b"block.rs", b"body();"))
                session.send(b"Vj" + chord)
                read_until(session.master, b"Commented")
                session.send(b"kVj" + chord)
                read_until(session.master, b"Uncommented")
                command(session, b"wq")
                session.wait_exit()
                wait_file(source, b"fn main() {\n    body();\n}\n")

        discard = os.path.join(work, "discard.txt")
        write_target(discard)
        with PtySession(binary, home, work, discard) as session:
            read_until(session.master, (b"discard.txt", b"alpha"))
            session.send(b"idiscard \x1b")
            read_until(session.master, b"Modified")
            command(session, b"q!")
            session.wait_exit()
            wait_file(discard, ORIGINAL)

        vim_keys(binary, home, work)

    print(
        "PASS: resize, :q refusal, :w, :wq, :q!, -h/--help help page,"
        " comment toggle, vim Control keys"
    )


if __name__ == "__main__":
    try:
        main()
    except AssertionError as error:
        print("FAIL:", error)
        raise SystemExit(1)
