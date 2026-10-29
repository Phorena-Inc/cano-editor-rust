# Cano — Product Requirements Document

| | |
|---|---|
| **Product** | Cano, a modal text editor for the terminal (Rust port) |
| **Package / binary** | `cano-fresh` 0.26.1029 / `cano` |
| **Status** | Implemented. This document was derived from the source code, then updated for the 0.26.1016 fixes. |
| **Date** | 2026-10-17 |
| **License** | Apache-2.0 |
| **Related** | [README.md](README.md) (user guide), [MIGRATION_NOTES.md](MIGRATION_NOTES.md) (compatibility choices), [docs/dbc-ponytail-report.md](docs/dbc-ponytail-report.md) (contracts audit) |

Each requirement has an ID (`FR-…` for functional, `NFR-…` for
non-functional). Each section heading names the modules that implement its
requirements. Each requirement has one of these statuses:

| Status | Meaning |
|--------|---------|
| ✅ | Implemented as stated |
| ⚠️ | Implemented, with a known limitation listed in [§12](#12-known-limitations) |
| ⛔ | Not implemented, by decision (see the non-goals) |

---

## 1. Overview

Cano is a small, fast terminal editor in the style of Vim. It started as a C
project ([Cano-Projects/Cano](https://github.com/Cano-Projects/Cano)). This
repository is an independent Rust implementation with three aims:

- Keep the keys, commands and configuration of the C editor.
- Replace the unsafe memory behaviour of the C editor with bounded results.
- Add the Vim features that users expect.

Cano uses Ratatui and Crossterm, and you configure it in Lua 5.4. It ships as
one binary of about 1 MB for Linux, macOS and Windows.

Cano has an optional AI assistant panel. The assistant teaches users how Cano
works. It can also do editing tasks through a tool loop that asks for approval
before risky actions.

## 2. Problem statement

Developers who work in the terminal want a modal editor that:

1. starts at once and works over SSH with no setup;
2. gives the core Vim habits (motions, operators, text objects, `.`, counts,
   `:s`) without the size and configuration surface of Vim;
3. is easy to understand completely: a small codebase, plain-text help, and
   no plugin ecosystem to learn;
4. helps new users learn modal editing, which is the hardest part of any
   vi-like editor.

The C editor met needs 1 and 3. It did not meet needs 2 and 4: it had
memory-safety hazards, and it had no operators, text objects or repeat.

## 3. Goals and non-goals

### Goals

- **G1 — Compatibility.** Keep the keys, commands, Lua `setup()` semantics and
  `.cyntax` format of the C editor, so that existing configs keep working.
  Where the C behaviour was a defect, follow Vim instead and record the change
  in `MIGRATION_NOTES.md`. Examples of such defects: the clipboard took an
  extra byte, Visual delete left out the cursor character, and `r` had an
  empty undo record.
- **G2 — Safety.** Never crash or corrupt data, for any input.
- **G3 — Vim fluency.** Give the commonly used Vim subset: operators, text
  objects, `.` repeat with counts, `:s`, `:set`, `*`, the full set of Visual
  operators, Insert-mode completion, scroll keys, and `Ctrl-A`/`Ctrl-X`.
- **G4 — Small footprint.** One binary, optimized for size. The only runtime
  dependencies are `sh` and `curl`, and both are optional.
- **G5 — Learnability.** Build the help pages into the binary and open them
  with `:help`. Give an AI assistant that bases its answers on those pages.
- **G6 — Data safety.** Saves are atomic and keep permissions. Cano backs up
  the previous version. A pane switch, `:help`, a quit or a signal never loses
  unsaved work.

### Non-goals

- Multiple windows, splits, tabs or buffers.
- A regular-expression engine. Search and substitute use literal text.
- A plugin system beyond Lua `setup()` and `cano.command()`.
- Integration with LSP, tree-sitter or external formatters.
- Named registers or integration with the system clipboard.
- These Vim keys and features:
  - character search `f` `F` `;` `,` (in Cano, `s`/`t` are jump labels);
  - `P`, `J`, `D`, `C`, `S`, `Y`, `~`, `^`, `H` `M` `L`, `{` `}`, `#`;
  - Normal-mode `>>`/`<<`;
  - macros (`q`, `@`) and marks (`m`, `` ` ``);
  - `:e file` and `:w file`.
- Tabs aligned to tab stops. Each tab is `tabstop` columns wide.

## 4. Users and personas

| Persona | Needs | Key features |
|---------|-------|--------------|
| **User of the C editor** | Their config and habits still work | `setup()` booleans, `.cyntax`, legacy `:e`/`:we`, `set-map`, `/s/old/new` |
| **Vim user who wants a lighter editor** | Their Vim habits work | Operators with counts, text objects, `.`, `:s`, `:set` with Vim names, `*`, Visual `c`/`p`/`o`/`r`, `Ctrl-V`, `Ctrl-A` |
| **New user of modal editing** | Learn in the editor | `:help`, an AI assistant that explains keys, mouse support, clear status messages |
| **Operations or SSH user** | Quick edits of config files on a remote machine | Fast start, small prebuilt binary, syntax highlighting for shell, JSON, Lua and Vim script, atomic saves with backups, `:autoformat` for JSON |

## 5. User stories

1. As a user, I can open a file, edit it in modes and save it. Cano stops me
   from quitting with unsaved changes, unless I tell it to discard them.
2. As a Vim user, I can type `ciw`, `3dd`, `d3w`, `dap`, `3.`, `:%s/a/b/gc`,
   `*` and `Ctrl-V` and get the results I expect. One `u` undoes one change.
3. As a user, I can browse the current directory or my recent files and open
   a file without losing unsaved work.
4. As a user, I can recover the overwritten version of a file from
   `.backups/`. A crash during a save never leaves a half-written file.
5. As a user, I can set options and mappings in `init.lua`, with the Vim
   names that I know.
6. As a new user, I can type `:help keys`, or press `Ctrl-K` and ask "how do I
   delete a word?", and get the exact keys.
7. As a user, I can ask the assistant to make an edit, and then undo it with
   `u`. I can also ask it to run a build. The build runs only after I approve
   it.
8. As a user, I can edit text that contains accents, CJK characters or emoji,
   and Cano draws them correctly.

---

## 6. Functional requirements

### 6.1 Startup and command line (`cli.rs`, `main.rs`, `io.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-CLI-1 | The syntax is `cano [--config <path>] [file]`. The first positional argument is the file. Cano ignores other positional arguments. | ✅ |
| FR-CLI-2 | `-v`/`--version` prints `cano <version>` and exits with status 0, before Cano reads any config. It takes precedence over the help flags. | ✅ |
| FR-CLI-3 | `-h`/`--help` opens the `general` help page. `--help=<page>` opens `general`, `keys` or `cmds`. The page opens as a read-only buffer, and Cano does not add it to the recent-files list. For an unknown page, Cano prints `No help page named <x>; pages: general, keys, cmds` and exits with status 1. | ✅ |
| FR-CLI-4 | Flags must match exactly. `--config X` and `--config=X` work. `--helpful` and `--configuration` give `Unexpected flag`. | ✅ |
| FR-CLI-5 | Cano looks for help pages in this order: `$CANO_HELP_DIR`, the `CANO_HELP_DIR` set at build time, paths relative to the executable, `./docs/help`, and last, the copies of all three pages built into the binary. | ✅ |
| FR-CLI-6 | If the file does not exist, Cano opens an empty buffer and creates the file at the first save. For other read errors, Cano prints `Could not open …` and exits with status 1. | ✅ |
| FR-CLI-7 | If there is no file argument, the buffer is named `out.txt`. | ✅ |
| FR-CLI-8 | For an unknown flag, or `--config` without a value, Cano prints the usage to stderr and exits with status 1. | ✅ |
| FR-CLI-9 | Exit status: 0 for a normal exit; 1 for an argument, config, file-open or terminal error; 128 + signal after SIGTERM or SIGHUP; in all other cases, the code given to `cano.exit`. A negative code keeps its low byte. A code above 255 becomes 255. | ✅ |

### 6.2 Configuration (`config.rs`, `command.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-CFG-1 | Cano loads `~/.config/cano/init.lua` if it exists, or the `--config` path, which must exist. The home directory is `HOME`. If `HOME` is not set, Cano uses `USERPROFILE`. | ✅ |
| FR-CFG-2 | `setup(table)` accepts the keys `syntax relative auto_indent indent undo_size cursorline mouse backup list wrap`. Cano uses only Lua booleans (`true` = 1, `false` = 0). A second call merges with the earlier calls. A non-table argument gives a configuration error. It does not crash Cano. | ✅ |
| FR-CFG-3 | `setup` returns `{command, exit}`. `command(line)` queues a `:` command line. The leading `:` is optional. The queued lines run in order before the terminal opens. Cano reports each line that fails. | ✅ |
| FR-CFG-4 | `exit(code[, msg])` stops startup. Cano prints `Exiting as specified in the configuration, message: <msg>` and exits with the code. If `exit` is called more than once, the first call wins. | ✅ |
| FR-CFG-5 | Defaults: `syntax` on, `relative` off, `indent` 4, `tabstop` 4, `undo_size` 32, `cursorline` off, `mouse` on, `backup` on, `list` off, `wrap` on, and all three autoformat steps on. | ✅ |
| FR-CFG-6 | Cano accepts `auto_indent` for compatibility, but the option has no effect. `Enter`, `o` and `O` always auto-indent. | ✅ |
| FR-CFG-7 | These files are in the directory of the active config file: `<ext>.cyntax`, `recent` and `ai-history.json`. | ✅ |

### 6.3 Modes and input routing (`app.rs`, `editor.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-MODE-1 | There are five modes: Normal, Insert, Visual (charwise, linewise, blockwise), Search and Command. `Esc`/`Ctrl-C` goes back to Normal mode from any mode and clears a pending count or operator. | ✅ |
| FR-MODE-2 | Cano gives input to handlers in this order: the assistant (`Ctrl-K` or a focused panel), a pending `r`, `Ctrl-Q`, the mouse, the `:s` confirm prompt, the save prompt, the history list, a pending jump, `set-map` expansion, and last, the handler of the current mode. | ✅ |
| FR-MODE-3 | `Ctrl-Q` quits, but refuses while there are unsaved changes. `Ctrl-S` saves and then quits. | ✅ |
| FR-MODE-4 | In Normal mode, `Ctrl-Z` suspends the process (SIGTSTP on Unix). Cano restores the screen after `fg`. | ✅ |
| FR-MODE-5 | `Ctrl-W` is a window prefix. Cano ignores the next key and shows `Cano has one window; splits are not supported`. | ✅ |
| FR-MODE-6 | The status line reads `MODE file [MD] (col,row) Saved/Modified`, with an `AI` button at the right end. The column is the 1-based display column. The cursor is a bar in Insert mode, hidden in Visual mode, and the terminal default in other modes. | ✅ |

### 6.4 Normal-mode motions and editing (`buffer.rs`, `editor.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-NAV-1 | Motions: `h j k l 0 $ w b e g G %` and the arrow keys. `%` ignores brackets inside quotes. `h`/`l` move over whole UTF-8 characters. `j`/`k` keep the display column. | ✅ |
| FR-NAV-2 | Counts work as in Vim. `N` + motion repeats the motion. `Ng`/`NG` go to line N. `3x`, `3p`, `3rX`, `3*` and `3.` repeat. A count before a Ctrl key multiplies it. Cano drops a count before `s`, `t`, `:`, `/` or space. | ✅ |
| FR-NAV-3 | Scroll keys: `Ctrl-F/B` move one full page with 2 lines of overlap. `Ctrl-D/U` move half a page. `Ctrl-E/Y` move one line. The cursor moves only when it must. | ✅ |
| FR-NAV-4 | EasyMotion: `s{char}` finds matches above and below the cursor. `t{char}` finds matches forward only and stops one character before the match. Labels come from `asdghklqwertyuiopzxcvbnmfj;`, nearest match first. Labels become two keys long when necessary. Only visible rows are targets. | ✅ |
| FR-EDIT-1 | Keys: `i I a A o O`, `x`, `r{char}`, `p`, `u`, `U`/`Ctrl-R`, and `Ctrl-O`. `r` accepts any UTF-8 character, and `Enter` splits the line. `Ctrl-O` opens a line below and stays in Normal mode. As in Vim, `x` and the Visual operators put exactly the affected text into the clipboard. | ✅ |
| FR-EDIT-2 | `Ctrl-A`/`Ctrl-X` work on decimal numbers and `0x` hexadecimal numbers. They keep zero padding and the hex width and letter case. A leading `-` is part of the number. The cursor moves to the last digit. | ✅ |
| FR-EDIT-3 | `Ctrl-G` shows `"file" [Modified] [RO] N lines --P%--`. `Ctrl-L` redraws the screen and clears the message line. | ✅ |
| FR-EDIT-4 | Space is the leader key: `i` = `*`, `o` = `:nohl`, `n` = file explorer, `r` = recent-files list, `l` = `:set list!`, `f` = `:autoformat`. In Visual mode, `e` toggles comments. | ✅ |

### 6.5 Operators, text objects, repeat and undo (`editor.rs`, `textobject.rs`, `app.rs`, `history.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-OP-1 | The operators `d`, `y` and `c` work with the motions `0 $ w b e g G`. A doubled operator (`dd yy cc`) acts on the whole line. `cw` on a non-blank character works as `ce`. | ✅ |
| FR-OP-2 | Operator counts multiply: `3dw` = `d3w`, `2d3w` deletes six words, and `3dd`/`3yy`/`3cc` act on three lines. `3d` alone waits for its motion. `d2aw` acts on two word objects. | ✅ |
| FR-OP-3 | Text objects: `i` or `a` with `w p ( ) b [ ] { } B < > " ' \``. Bracket matching ignores brackets inside quotes. If the object does not exist, nothing happens. | ✅ |
| FR-OP-4 | A change is one undo step and one redo step. The step contains the deletion from `c`, `o` or `O`, and all the text typed in the same Insert session. An arrow key or a mouse click ends the step. Visual `>`/`<` over many lines is one step. | ✅ |
| FR-OP-5 | `.` repeats the last change with its original count. `N.` replaces that count. Motions and changes that change nothing do not replace the stored change. Cano drops a change that a prompt interrupts. | ✅ |
| FR-OP-6 | A new edit clears the redo stack. The undo stack keeps at most `undo_size` steps and drops the oldest step first. `0` or less means no limit. After an undo, every record can be redone, including `r` and `x`. | ✅ |
| FR-OP-7 | Recursion limits: replay nesting stops at 64 levels, with the message `Recursive repeat` or `Recursive key map`. One keypress can expand to at most 10,000 keys. | ✅ |

### 6.6 Insert mode (`editor.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-INS-1 | Keys: `Backspace`/`Ctrl-H` delete one code point. `Ctrl-W` deletes a word. `Ctrl-U` deletes to the indent, then the indent. `Ctrl-T`/`Ctrl-D` shift the line. `Tab` inserts `indent` spaces, or a tab when `indent` is 0. | ✅ |
| FR-INS-2 | `Enter` indents the new line by the bracket depth. `(`, `[` and `{` insert their closing bracket. When you type a closing bracket in front of the same bracket, the cursor steps over it. `Enter` between a bracket pair splits it onto three lines. | ✅ |
| FR-INS-3 | `Ctrl-N`/`Ctrl-P` complete from the words in the buffer, nearest first, and wrap around. When you move past either end, the typed text comes back and Cano shows `Back at original`. The status line shows `match i of n`. | ✅ |
| FR-INS-4 | `Ctrl-O` runs one Normal-mode command and shows `-- (insert) --`. Then Cano goes back to Insert mode, unless the command selects a different mode. | ✅ |
| FR-INS-5 | `:imap lhs rhs`: when you type the full left-hand side without a break, the mapping runs at once. A new mapping of the same left-hand side replaces the old one. | ✅ |

### 6.7 Visual mode (`editor.rs`, `comment.rs`, `autoformat.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-VIS-1 | `v`/`V`/`Ctrl-V` start Visual mode. The motions (with counts) and `s{char}` extend the selection. `v`/`V` change the kind of selection. If you press the key of the current kind, Cano leaves Visual mode. | ✅ |
| FR-VIS-2 | `d`/`x` delete the selection, including the character under the cursor. `y` yanks the same text. `>`/`<` shift the lines. `=` formats the lines. `Ctrl-E`/`space e` toggles comments. | ✅ |
| FR-VIS-3 | `c` deletes the selection and starts Insert mode. The deletion and the typed text are one undo step. `p` puts the clipboard text over the selection and puts the replaced text into the clipboard. `o` moves the cursor to the other end. `r{char}` replaces every selected character except line breaks, as one undo step. | ✅ |
| FR-VIS-4 | In blockwise mode, `d`/`x` cut the rectangle as one undo step, and `y` copies it. Short lines count as blank. `p` pastes the rows as lines. | ⚠️ L-2 |
| FR-VIS-5 | The comment toggle uses the comment marker for the file type (`//`, `#`, `--`, `"`), in one aligned column. It skips blank lines and is one undo step. It refuses for an unknown file type, JSON, or a read-only buffer. | ✅ |

### 6.8 Search and substitute (`buffer.rs`, `substitute.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-SRCH-1 | `/text` searches for literal text. The search is case-sensitive and wraps around. `n`/`N` accept counts. Cano highlights the matches. | ✅ |
| FR-SRCH-2 | `*` searches for the word under the cursor as a whole word. `:nohl` hides the highlight and keeps the pattern. | ✅ |
| FR-SRCH-3 | The legacy `/s/old/new` replaces the next match after the cursor, as one undo step. It refuses on a read-only buffer. | ✅ |
| FR-SUB-1 | The syntax is `:[range]s/pat/rep/[gci]`. The range is `%`, `N`, `.`, `$` or `a,b`. The command name can be any prefix of `substitute`. The delimiter can be any punctuation except `\`. | ✅ |
| FR-SUB-2 | Patterns are literal text. They support `\<`/`\>` word boundaries. `\` escapes the delimiter and itself. | ✅ |
| FR-SUB-3 | With the `c` flag, Cano asks `Replace? (y/n/a/q)` before each match. The whole command is one undo step. Cano reports `N substitutions on M lines`. The command refuses on a read-only buffer. | ✅ |

### 6.9 Command line (`command.rs`, `app.rs`, `process.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-CMD-1 | Commands: `:w`, `:q`, `:q!`, `:wq`, and the legacy `:e` (a quit that checks for unsaved changes) and `:we`. | ✅ |
| FR-CMD-2 | `:set` accepts `name`, `noname`, `name!`, `name=N` and `name?`. One line can set many options. Vim aliases work (`rnu sw ts ai cul bk syn lcs …`). A value out of range gives `Invalid value for <name>: <v>`. | ✅ |
| FR-CMD-3 | Commands: `set-var`, `set-output "path"`, `set-map key "rhs"`, `let name [=] expr` and `echo`. Expressions use integers, evaluated from left to right. | ✅ |
| FR-CMD-4 | `:!cmd` runs `sh -c` on a worker thread, so the editor continues to respond. While the command runs, the message line shows `Running: <cmd> (Ns)`. The result is the last non-blank line of stdout. If stdout is empty, Cano uses stderr. On failure, the result starts with `[exit N]` or `[signal N]`. Only one command can run at a time. | ✅ |
| FR-CMD-5 | Commands: `:autoformat`/`:Autoformat` and `:nohl`/`:nohlsearch`. | ✅ |
| FR-CMD-6 | The `:` prompt and the `/` prompt each keep a history of 50 different entries for the session. `Up`/`Down` move through the history. `Ctrl-F` shows it as a list. | ✅ |
| FR-CMD-7 | `:help [page]` / `:h [page]` opens `general`, `keys` or `cmds` read-only in place of the buffer. It refuses if there are unsaved changes. `:help!` discards them. Cano puts the file that you leave at the top of the recent-files list. | ✅ |
| FR-CMD-8 | `:e <file>` and `:w <file>`. | ⛔ |

### 6.10 Display (`render.rs`, `display.rs`, `syntax.rs`, `markdown.rs`, `listchars.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-UI-1 | The screen has, in this order: a gutter (at least 4 digits wide, with optional relative numbers), the text, a scrollbar in the last column, the optional AI panel, the status line, and the message line, which also holds the prompts. Messages clear after about 1 second. | ✅ |
| FR-UI-2 | `cursorline` underlines the cursor line. The selection is in reverse video. Search matches have a highlight background. Jump labels have their own style. | ✅ |
| FR-UI-3 | Cano draws valid UTF-8 as characters at their display width. CJK, fullwidth and emoji characters take 2 cells. Combining marks take 0 cells and join the previous cell. Invalid bytes and control characters show as `�`. If the screen edge cuts a wide character, Cano draws that cell blank. | ✅ |
| FR-UI-4 | Each tab is `tabstop` columns wide (1–32, default 4). `tabstop` is separate from `indent`. | ⚠️ L-1 |
| FR-UI-5 | Syntax highlighting for C, C++, Rust, Python, shell, Vim script, Lua and JSON. Cano selects the language by extension or by known dotfile name. Each language has its own scanner. | ✅ |
| FR-UI-6 | A `<ext>.cyntax` palette has `k`/`t`/`w` groups with RGB colours and word lists. An empty `k` or `t` list uses the built-in list. Cano ignores a malformed palette. | ✅ |
| FR-UI-7 | Markdown display is on by default for `.md`-family files. `Ctrl-M`/`Enter` turns it on or off. It colours the text in place and does not move any character. | ✅ |
| FR-UI-8 | `:set list` shows invisible characters with the `listchars` items `tab trail eol nbsp space`. The default shows all five. The glyphs do not change the layout. | ✅ |
| FR-UI-9 | `wrap` (on by default) continues a long line on the next screen row at the right edge of the text area, as in vim. Only the first screen row of a line shows its line number. A wide character that does not fit moves to the next screen row. The view scrolls by screen rows, so the cursor is always visible, also on a line taller than the screen. `:set nowrap` scrolls the view sideways instead. | ✅ |

### 6.11 Formatting (`autoformat.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-FMT-1 | For a `.json` file, `:autoformat` validates the JSON and pretty-prints it at the `indent` width. It keeps key order, duplicate keys, number spellings, escapes and the final newline. For invalid JSON, Cano reports `Invalid JSON: …` and changes nothing. | ✅ |
| FR-FMT-2 | For all other files, `:autoformat` re-indents, retabs (it measures tabs with `tabstop`) and removes trailing spaces. An option controls each step. Brackets inside strings do not count. The number of lines never changes. | ✅ |
| FR-FMT-3 | Each format is one undo step. Cano reports `Formatted N lines` or `Already formatted`. The command refuses on a read-only buffer. | ✅ |

### 6.12 Files, panes and backups (`io.rs`, `explorer.rs`, `recent.rs`, `backup.rs`)

The two panes are the file explorer and the recent-files list.

| ID | Requirement | Status |
|----|-------------|--------|
| FR-FILE-1 | Load and save are byte-exact. Cano does no decoding and no line-ending conversion, and it does not add a final newline. | ✅ |
| FR-FILE-2 | Saves are atomic. Cano writes a temporary file next to the target, fsyncs it, and renames it over the target. The file keeps its permissions, and on Unix also its owner when the system allows it. Cano follows symlinks (up to 40 hops) and keeps them. Cano refuses to save a read-only file. Cano writes FIFOs, devices and hard-linked files in place. | ✅ |
| FR-FILE-3 | After a failed save, the buffer stays marked as modified, Cano shows `Could not write …`, and Cano cancels a pending quit. Read-only buffers (help pages) refuse to save with `Buffer is read-only`. | ✅ |
| FR-PANE-1 | The file explorer (`Ctrl-N`) lists the working directory, including hidden files. Directory names end in `/`, and there is a `..` entry. Symlinks are not listed. | ✅ |
| FR-PANE-2 | The recent-files list (`Ctrl-P`) shows up to 50 files, most recent first. Cano stores them as absolute paths. On Windows, the stored paths have no `\\?\` prefix. The list persists across restarts. It skips deleted files and leaves out help pages. | ✅ |
| FR-PANE-3 | If the buffer has unsaved changes, both panes first ask `Save changes? (y/n, Esc cancels)`. If the save fails, the pane does not open. | ✅ |
| FR-BAK-1 | Before each save, Cano copies the file on disk to `<dir>/.backups/<file>.<YYYYMMDD-HHMMSS>`. The time is UTC. If the name exists, Cano adds a `.N` suffix. The backup keeps the permissions of the original file. Cano keeps the 10 newest backups. | ✅ |
| FR-BAK-2 | If a backup fails, Cano reports `Could not back up …` and still saves. A file that does not exist yet gets no backup. The `backup` option turns backups off. | ✅ |

### 6.13 Mouse (`terminal.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-MOUSE-1 | A click moves the cursor. A click on either half of a wide character moves to the start of that character. A drag selects text in charwise Visual mode. The wheel scrolls 3 lines. The scrollbar responds to click and drag. | ✅ |
| FR-MOUSE-2 | In the panes and the history list, a click selects an entry. A second click on the selected entry opens it. The scrollbars of these lists move only the selection. | ✅ |
| FR-MOUSE-3 | The `mouse` option turns mouse capture on or off at once. A click never answers a prompt and never selects a jump label. | ✅ |

### 6.14 AI assistant (`assistant.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-AI-1 | The assistant is available only when `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` is set. Cano uses Anthropic unless `CANO_AI_PROVIDER=openai`. `CANO_AI_MODEL` sets the model (default `claude-opus-5-5`, or `gpt-5` for OpenAI). `CANO_AI_EFFORT` sets the effort (default `medium`). | ✅ |
| FR-AI-2 | `Ctrl-K` or the `AI` button in the status line opens or closes a docked panel. The panel starts at 20% of the screen height and is at least 3 rows high. A drag of the title bar resizes it. Without a key, `Ctrl-K` tells the user how to turn the assistant on. | ✅ |
| FR-AI-3 | Panel keys: `Enter` sends. `Esc` stops a request or moves focus back to the editor. `Ctrl-L`, or sending `/clear`, starts a new chat, also while a request runs. The arrows, PgUp/PgDn, the wheel and the scrollbar scroll the chat. The input line supports basic line editing. | ✅ |
| FR-AI-4 | The system prompt contains the `general`, `keys` and `cmds` help pages, so that the answers are based on the Cano reference. | ✅ |
| FR-AI-5 | Tools: `get_editor_state`, `read_buffer`, `edit_buffer` (1-based line numbers, one undo step per call), `run_editor_command` (cannot quit), `read_file`, `list_directory`, `run_shell`, and `write_file` (cannot write the open file). | ✅ |
| FR-AI-6 | Each `run_shell` and `write_file` call needs approval. `y`/`Enter` allows the call. `n` refuses it. `a` allows the tool for the rest of the session. `Esc` cancels the task. When a round is cancelled, Cano still answers every tool call, so the conversation stays valid. | ✅ |
| FR-AI-7 | Limits: 40 rounds per request, 30,000 characters per tool result, 2,000 lines per read, 400 chat entries, and a 600-second HTTP timeout. | ✅ |
| FR-AI-8 | `curl` sends the HTTP requests, on a worker thread. Cano gives the key to curl on stdin, never on its command line. Shell tools run on a worker thread, so they never block the UI. | ✅ |
| FR-AI-9 | After each completed round, Cano saves the conversation to `ai-history.json` (mode 0600, with an atomic rename). Cano loads it at startup. Cano reuses the saved API context only when the provider and model are the same. When the user clears the chat, Cano deletes the file. | ✅ |

---

## 7. Non-functional requirements

| ID | Requirement | Status |
|----|-------------|--------|
| NFR-1 Size | The release profile uses `opt-level="z"`, LTO, `codegen-units=1`, stripped symbols and `panic="abort"`. The binary is about 1.1 MB. Cano uses no HTTP, TLS or Unicode crate: the assistant uses `curl`, and character widths come from a small built-in table. | ✅ |
| NFR-2 Dependencies | crossterm 0.29, ratatui 0.30, mlua 0.12 (vendored Lua 5.4, which needs a C compiler), serde_json, and `libc` on Unix only. | ✅ |
| NFR-3 Platforms | Linux, macOS and Windows. On Windows, `:!` and `run_shell` need an `sh` on `PATH`, and suspend has no shell to go to. Prebuilt archives exist for 5 targets: Linux x86-64 and ARM64, macOS x86-64 and ARM64, and Windows x86-64. The Nix flake builds on x86_64-linux. | ✅ |
| NFR-4 Robustness | No input causes a panic in normal use. Debug builds check design-by-contract invariants with `debug_assert!`. Examples: an undo group is open only in Insert mode, and the cursor is on a character boundary after a motion. A panic hook restores the terminal before the process aborts. | ✅ |
| NFR-5 Responsiveness | The event loop waits for input. While a `:!` command or an assistant request runs, it also wakes every 200 ms to collect results. While it waits, it wakes at least every 100 ms to check the terminal size and for SIGTERM/SIGHUP. Messages clear after about 1 second. `:!` commands, assistant requests and shell tools run outside the UI thread. | ✅ |
| NFR-6 Security | The Lua config is trusted code with the full standard library. API keys never appear in argv or in `Debug` output. Only the user can read the chat history file. Shell commands and file writes from the assistant need approval. `:help` opens only the three shipped page names, never a path that the user gives. | ✅ |
| NFR-7 Testability | On Linux: 436 library unit tests, 8 binary unit tests, 6 `:help` integration tests and 5 lifecycle integration tests, plus a PTY smoke test (`make smoke`, Python 3). `cargo fmt` and `clippy -D warnings` must pass. GitHub Actions builds each push for all five targets. | ✅ |
| NFR-8 Data integrity | Saves are atomic, and Cano makes a backup first. A failed save never discards the buffer. Pane switches, `:help` and quits check for unsaved changes. After SIGTERM or SIGHUP, Cano restores the terminal and exits with 128 + signal. It does not save. | ✅ |

## 8. Architecture

```
            ┌──────────┐   args    ┌──────────┐  Lua   ┌──────────┐
  argv ───▶ │  cli.rs  │ ────────▶ │ main.rs  │ ─────▶ │config.rs │ (mlua)
            └──────────┘           │ startup, │        └──────────┘
                                   │ event    │  Input  ┌──────────────┐
  terminal ◀── terminal.rs ◀──────▶│ loop,    │ ──────▶ │    app.rs    │
  (crossterm)  render.rs  ◀─frame──│ effects, │ ◀────── │ routing,     │
  (ratatui)    display.rs          │ shell job│ AppEffect panes, repeat│
                                   └──────────┘         └──┬────┬────┬─┘
                     io.rs (load/atomic save, help) │      │    │    │
                     backup.rs   process.rs (sh -c, worker)│    │ assistant.rs
                                                   editor.rs    │  (curl worker)
                                                   buffer.rs    │
                                                   history.rs   │
                                     command.rs substitute.rs textobject.rs
                                     jump.rs comment.rs autoformat.rs
                                     syntax.rs markdown.rs listchars.rs
                                     explorer.rs recent.rs
```

**State.**

- The buffer is a flat `Vec<u8>`. A row index is rebuilt after each edit.
- The cursor is a byte offset. In Normal mode, it is always on a character
  boundary.
- `display.rs` maps bytes to screen cells.
- Undo and redo are stacks of inverse records. Records can be joined into
  groups. The undo stack has a size limit.
- There is one unnamed clipboard.

**Effects.** `App::handle(Input) -> Vec<AppEffect>` does no I/O. `main.rs`
carries out the effects `Save`, `Quit`, `Shell`, `Suspend` and `Redraw`. This
lets the tests check the logic without a terminal. A shell command becomes a
background job, and the main loop polls it.

**Assistant.** The assistant is a state machine on `App`:

1. A worker thread runs `curl`.
2. `assistant_poll()` collects the replies on each frame.
3. Editor tools run on the main thread between frames.
4. Shell tools run on a separate worker thread.
5. An approval prompt pauses the tool queue.

## 9. User-visible messages

This section lists the messages that Cano shows, grouped by where they
appear. Each message was checked against the source.

**Startup (stderr or stdout, before the editor opens)**
- `Unexpected flag`
- `usage: <argv0> --config <init.lua> <filename>`
- `No help page named <x>; pages: general, keys, cmds`
- `could not determine the home directory (HOME/USERPROFILE unset)`
- `Failed to apply configuration: <reason>`
- `Exiting as specified in the configuration, message: <msg>`
- `Could not open <path>: <error>`

**Editing**
- `No previous change`, `Recursive repeat`, `Recursive key map`
- `No previous search`, `Pattern not found`, `Pattern not found: <pat>`
- `No word under the cursor`, `No word before the cursor`, `No number under the cursor`
- `match N of M`, `Back at original`, `The only match`
- `No jump targets`, `No such jump target`
- `Not enough characters to replace`
- `Commented`, `Uncommented`, `Nothing to comment`, `No comment syntax for this file type`
- `Markdown display on`, `Markdown display off`
- `Cano has one window; splits are not supported`
- `-- (insert) --`
- `Undo failed: …`, `Redo failed: …`

**Files and panes**
- `No write since last change (add ! to override)`
- `Save changes? (y/n, Esc cancels)`
- `Buffer is read-only`
- `Could not write <path>: <error>`, `Could not back up <path>: <error>`
- `No recent files`
- `"<file>" [Modified] [RO] N lines --P%--` (`Ctrl-G`)
- `No help page named <x>`

**Commands, options and the shell**
- `Empty command`, `Unknown command: <x>`, `Unterminated string`
- `Not enough args`, `Too many args`, `Invalid special key`
- `Invalid arg, expected <kind> but found <token>`, `Invalid expression`, `Integer overflow`
- `Unknown option: <x>`, `Invalid value for <name>: <v>`
- `Malformed listchars item: …`, `Unknown listchars item: …`
- `Unknown substitute flag: <f>`, `No previous substitute pattern`, `No substitutions`
- `Replace? (y/n/a/q)`, `N substitution(s) on M line(s)`
- `Invalid JSON: …`, `Already formatted`, `Formatted N line(s)`
- `No command history`, `No search history`
- `Running: <cmd> (Ns)`, `A shell command is already running: <cmd>`
- `[exit N] <last line>`, `[signal N] <last line>`, `Shell command failed: <error>`

**AI assistant**
- `AI assistant is off: set ANTHROPIC_API_KEY or OPENAI_API_KEY and restart`
- `The assistant is still working; Esc stops it`
- `(The model changed since this chat, so it starts without the earlier context. Ctrl-L clears.)`

## 10. Acceptance criteria

You can run each check by hand on a release build. The unit tests,
integration tests or PTY smoke test also cover most of them.

1. `cano --version` prints exactly `cano 0.26.1029` and exits with status 0.
2. `cano -x` and `cano --helpful` print `Unexpected flag` and exit with
   status 1.
3. `cano --help=cmds` opens the command reference read-only, also when no
   help directory is installed. `cano --help=bogus` lists the pages and exits
   with status 1.
4. On a line that contains `foo foo`, `:s/foo/bar/g` replaces both and reports
   `2 substitutions on 1 line`.
5. `:%s/a/b/gc` asks `Replace? (y/n/a/q)` for each match. `a` replaces all
   remaining matches. One `u` undoes all the replacements.
6. On a modified buffer, `:q` shows `No write since last change (add ! to
   override)` and does not exit. `:q!` exits.
7. `:set rnu`, `:set nornu`, `:set rnu!` and `:set rnu?` turn the option on,
   turn it off, toggle it and show it. `:set ts=8` changes the tab width but
   not `sw`. Cano refuses `:set ts=0`.
8. On a `.json` buffer, `space f` pretty-prints valid JSON. For invalid JSON,
   it reports `Invalid JSON: …`.
9. `s` followed by a character shows labels from `asdghkl…`. When you type a
   label, the cursor jumps to it.
10. `Ctrl-A` on `0x0f` gives `0x10`. On `007`, it gives `008`.
11. After you open 51 different files, `recent` has exactly 50 entries, newest
    first.
12. With `backup` on, `:w` of an existing file makes
    `.backups/<name>.<YYYYMMDD-HHMMSS>` next to it, with the same permissions.
13. `ciwfoo<Esc>` and then `u` restore the original word in one step.
14. `3dd` deletes three lines and leaves no operator pending. `d3w` deletes
    three words, and `2d3w` deletes six. `3x` and then `.` delete three more
    characters.
15. In Visual mode, `c`, `p`, `o`, `r` and `v`/`V` work as FR-VIS-1 and
    FR-VIS-3 state.
16. For a file that contains `héllo 日本`, Cano draws those glyphs, and `l`
    moves one character at a time.
17. `:w` on a file with mode 0640, or on a symlink, keeps the mode and the
    link.
18. During `:!sleep 2; false`, the editor continues to respond. Then it shows
    `[exit 1]`.
19. `kill -TERM <pid>` restores the terminal, and Cano exits with status 143.
20. Sending `/clear` in the AI panel empties the chat and deletes
    `ai-history.json`.

## 11. Changes in 0.26.1016

These defects were found during the analysis of 0.26.921. Version 0.26.1016
fixes all of them, and each fix has regression tests.

| ID | Area | Before | After |
|----|------|--------|-------|
| G-1 | Undo | `c` needed two `u` presses. | A change and the text typed after it are one undo step and one redo step. `o`, `O` and Visual `>`/`<` are also one step. |
| G-2 | Search | `/s/old/new` could not be undone and ignored read-only buffers. | It is one undo step and refuses on a read-only buffer. |
| G-3 | Counts | Counts before `.`, `r` or `*` had no useful effect. `3dd` left `d` pending. `d3w` moved instead of deleting. | Counts work as in Vim everywhere (FR-NAV-2, FR-OP-2, FR-OP-5). `3d` alone now waits for its motion. |
| G-4 | Undo | New edits did not clear redo. `undo_size` had no effect. `r` could not be redone. The undo of an insert was off by one. | All four are fixed (FR-OP-6). |
| G-5 | Display | Tabs were always 4 columns, and `ts` was an alias of `indent`. UTF-8 showed one cell per byte. | `tabstop` is a separate option. Characters are drawn at their display width, and motions move by character (FR-UI-3, FR-UI-4). |
| G-6 | Clipboard | `x` copied an extra byte (a NUL at the end of the file). Visual `d` left out the cursor character. | Both work as in Vim (FR-EDIT-1, FR-VIS-2). |
| G-7 | Visual | `c`, `p`, `o`, `r`, `v`/`V` and counts did nothing. | All are implemented (FR-VIS-1, FR-VIS-3). |
| G-8 | AI | The help described `/clear`, but nothing handled it. | `/clear` starts a new chat. |
| G-9 | Shell | `:!` blocked the UI and lost stderr and the exit status. | It runs in the background and reports output and status (FR-CMD-4). |
| G-10 | Save | Saves were not atomic and lost permissions and symlinks. Backups lost permissions. SIGTERM/SIGHUP left the terminal in raw mode. | All are fixed (FR-FILE-2, FR-BAK-1, NFR-8). |
| G-11 | Packaging | The flake version and the `listchars` escape in the example were wrong. | Both are fixed. A GitHub Actions workflow now builds all five targets. |
| G-12 | Help | Only the `general` page was available. `--help*` and `--config*` matched by prefix. | `--help=<page>` and `:help` open every page, and flags must match exactly (FR-CLI-3, FR-CLI-4, FR-CMD-7). |

## 12. Known limitations

| ID | Limitation | Possible follow-up |
|----|------------|--------------------|
| L-1 | Each tab is `tabstop` columns wide wherever it is. Vim aligns tabs to the next tab stop. | Align tabs to tab stops. The retab calculation must change to match. |
| L-2 | Blockwise Visual mode counts columns in characters, not screen cells. A block next to wide characters can look uneven. | Lay out blocks in display columns. |
| L-3 | If the user quits while a `:!` command runs, the command keeps running. | Stop the child process, or ask before quitting. |
| L-4 | `setup()` cannot set `undo_size` to a number, because `setup()` uses only booleans. Use `cano.command("set undo_size=N")`. | Accept integers in `setup()`. This breaks compatibility with the `setup()` semantics of the C editor. |
| L-5 | The assistant's `run_editor_command` can save, but it can never quit. | None. This is by design. |

## 13. Success metrics

| Metric | Target |
|--------|--------|
| Test suite | 100% pass on `cargo test --locked --all-targets` (now 455 of 455 on Linux) and `make smoke` |
| Lint | `cargo clippy -D warnings` and `cargo fmt --check` report no problems |
| CI | The release workflow passes on all five targets for each push |
| Binary size | 1.2 MB or less for the stripped release build (now about 1.1 MB) |
| Cold start | Less than 50 ms to the first frame on a typical machine, without the assistant |
| Crash rate | Zero panics in a random key-sequence stress run (the 30,000-seed run in the DbC report) |
| Compatibility | An unchanged `init.lua` and `.cyntax` from the C editor load without errors |
| Assistant quality | The assistant answers questions about keys and commands from the built-in reference. The user can undo each buffer edit that it makes with `u`. |

## 14. Open questions

1. Do we move the remaining C compatibility behaviours behind a `compat`
   option, so that the defaults can follow Vim completely? These behaviours
   are `/s/` at the search prompt, `:e` as a quit, and boolean-only `setup()`.
2. Do we add a `:` command for the assistant (for example `:ai <prompt>`), so
   that mappings and `cano.command` can use it?
3. Do we tell the assistant where `init.lua` and the `.cyntax` files are, so
   that it can edit the configuration when asked?
4. An independent analysis of the 0.26.918 Windows binary found two aborts:
   one from `cano.exit()` and one from a bad `setup()` argument. Neither abort
   occurs on Linux in 0.26.1016. There, `cano.exit()` gives a clean exit code,
   and a bad argument gives a configuration error. Somebody must confirm this
   on Windows with the new release build.
