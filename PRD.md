# Cano — Product Requirements Document

| | |
|---|---|
| **Product** | Cano, a modal terminal text editor (Rust port) |
| **Package / binary** | `cano-fresh` 0.26.1016 / `cano` |
| **Status** | Implemented. Reverse-engineered from the source, then updated with the 0.26.1016 fixes |
| **Date** | 2026-10-16 |
| **License** | Apache-2.0 |
| **Related** | [README.md](README.md) (user guide), [MIGRATION_NOTES.md](MIGRATION_NOTES.md) (compatibility choices), [docs/dbc-ponytail-report.md](docs/dbc-ponytail-report.md) (contracts audit) |

Requirement IDs (`FR-…`, `NFR-…`) cite the module that implements each
requirement. Every requirement is marked with one of these statuses:

| Status | Meaning |
|--------|---------|
| ✅ | Implemented as stated |
| ⚠️ | Implemented with a known limitation, listed in [§12](#12-known-limitations) |
| ⛔ | Not implemented (by decision; see the non-goals) |

---

## 1. Overview

Cano is a small, fast, Vim-inspired editor for the terminal. It started as a C
project ([Cano-Projects/Cano](https://github.com/Cano-Projects/Cano)). This
repository is an independent Rust re-implementation that has three aims:

- keep the C editor's keys, commands and configuration;
- replace its unsafe memory behaviour with bounded results;
- add the Vim conveniences that users expect.

The editor is built on Ratatui and Crossterm, configured in Lua 5.4, and ships
as a single binary of about 1 MB for Linux, macOS and Windows. It includes an
optional AI assistant panel. The panel teaches users how Cano works and can
carry out editing tasks through a permissioned tool loop.

## 2. Problem statement

Developers who live in the terminal want a modal editor that:

1. starts instantly and works over SSH with no setup;
2. offers the core Vim muscle memory (motions, operators, text objects, `.`,
   counts, `:s`) without Vim's size and configuration surface;
3. is understandable end to end: small codebase, plain-text help, no plugin
   ecosystem to learn;
4. helps a newcomer learn modal editing, which is the steepest part of
   adopting any vi-like editor.

The C original met the first and third needs. It had memory-safety hazards
and lacked operators, text objects and repeat, so it fell short on the second
and fourth.

## 3. Goals and non-goals

### Goals

- **G1 — Compatibility.** Keep the C editor's keys, commands, Lua `setup()`
  semantics and `.cyntax` format, so existing configs keep working. Where the
  C behaviour was a defect (the clipboard taking an extra byte, an exclusive
  Visual delete, an empty `r` inverse), follow Vim instead and record the
  change in `MIGRATION_NOTES.md`.
- **G2 — Safety.** Never crash or corrupt data on any input.
- **G3 — Vim fluency.** Provide the commonly used Vim subset: operators, text
  objects, dot-repeat with counts, `:s`, `:set`, `*`, the full set of Visual
  operators, Insert-mode completion, scrolling keys, and `Ctrl-A`/`Ctrl-X`.
- **G4 — Small footprint.** A single size-optimized binary, with no runtime
  dependencies except optional `sh` and `curl`.
- **G5 — Learnability.** Help pages built into the binary and reachable with
  `:help`, plus an AI assistant grounded in those pages.
- **G6 — Data safety.** Saves are atomic and keep permissions. The previous
  version is backed up. Unsaved work is never lost to a pane switch, `:help`,
  a quit or a signal.

### Non-goals

- Multiple windows, splits, tabs or buffers.
- A regex engine (search and substitute are literal).
- A plugin system beyond Lua `setup()` and `cano.command()`.
- LSP, tree-sitter, or external formatter integration.
- Named registers or system-clipboard integration.
- These Vim keys and features: `f` `F` `;` `,` (character search; `s`/`t`
  are jump labels in Cano), `P`, `J`, `D`, `C`, `S`, `Y`, `~`, `^`,
  `H` `M` `L`, `{` `}`, `#`, Normal-mode `>>`/`<<`, macros (`q`, `@`), marks
  (`m`, `` ` ``), and `:e file`/`:w file`.
- Aligning tabs to tab stops: each tab is drawn `tabstop` columns wide.

## 4. Users and personas

| Persona | Needs | Key features |
|---------|-------|--------------|
| **Existing C-Cano user** | Their config and habits still work | `setup()` booleans, `.cyntax`, legacy `:e`/`:we`, `set-map`, `/s/old/new` |
| **Vim user wanting something lighter** | Muscle memory transfers | Operators with counts, text objects, `.`, `:s`, `:set` with Vim names, `*`, Visual `c`/`p`/`o`/`r`, `Ctrl-V`, `Ctrl-A` |
| **Newcomer to modal editing** | Learn without leaving the editor | `:help`, the AI assistant explaining keys, mouse support, explicit status messages |
| **Ops / SSH user** | Quick config-file edits on a remote box | Fast start, small prebuilt binary, syntax for shell/JSON/Lua/Vim, atomic saves with backups, `:autoformat` for JSON |

## 5. User stories

1. As a user I can open a file, edit it modally and save it, and I'm prevented
   from quitting with unsaved changes unless I say so.
2. As a Vim user I can type `ciw`, `3dd`, `d3w`, `dap`, `3.`, `:%s/a/b/gc`,
   `*` and `Ctrl-V` and get the results I expect. One `u` undoes one change.
3. As a user I can browse the current directory or my recent files and open
   one without losing unsaved work.
4. As a user I can recover the version I overwrote from `.backups/`. A crash
   during a save never leaves a half-written file.
5. As a user I can configure options and mappings in `init.lua`, using Vim
   spellings where I know them.
6. As a newcomer I can type `:help keys` or press `Ctrl-K` and ask "how do I
   delete a word?", and get the exact keys.
7. As a user I can ask the assistant to make an edit and undo it with `u`. I
   can also ask it to run a build, which needs my explicit approval.
8. As a user I can edit text containing accents, CJK characters or emoji and
   see them drawn correctly.

---

## 6. Functional requirements

### 6.1 Startup and CLI (`cli.rs`, `main.rs`, `io.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-CLI-1 | `cano [--config <path>] [file]`. The first positional argument is the file; any others are ignored. | ✅ |
| FR-CLI-2 | `-v`/`--version` prints `cano <version>` and exits 0 before any config is read. It takes precedence over help. | ✅ |
| FR-CLI-3 | `-h`/`--help` opens `general`, and `--help=<page>` opens `general`, `keys` or `cmds`, as a read-only buffer that is not added to the recent list. An unknown page prints `No help page named <x>; pages: general, keys, cmds` and exits 1. | ✅ |
| FR-CLI-4 | Flags must be spelled exactly. `--config X` and `--config=X` work; `--helpful` and `--configuration` are `Unexpected flag`. | ✅ |
| FR-CLI-5 | Help pages are looked up in this order: `$CANO_HELP_DIR`, then the build-time `CANO_HELP_DIR`, then paths relative to the executable, then `./docs/help`, and finally the copies of all three pages built into the binary. | ✅ |
| FR-CLI-6 | A nonexistent file opens as an empty buffer and is created on the first save. Other read errors exit 1 with `Could not open …`. | ✅ |
| FR-CLI-7 | With no file argument, the buffer is named `out.txt`. | ✅ |
| FR-CLI-8 | An unknown flag, or `--config` without a value, prints usage to stderr and exits 1. | ✅ |
| FR-CLI-9 | Exit codes: 0 for normal exit; 1 for argument, config, open or terminal errors; 128 + signal after SIGTERM/SIGHUP; otherwise the code from `cano.exit`. A negative code keeps its low byte, and a code above 255 becomes 255. | ✅ |

### 6.2 Configuration (`config.rs`, `command.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-CFG-1 | Load `~/.config/cano/init.lua` if it exists, or the `--config` path, which must exist. The home directory is `HOME`, falling back to `USERPROFILE`. | ✅ |
| FR-CFG-2 | `setup(table)` accepts the slots `syntax relative auto_indent indent undo_size cursorline mouse backup list`. Only Lua booleans are honoured (`true`=1, `false`=0). Repeated calls merge. A non-table argument is a reported configuration error, not a crash. | ✅ |
| FR-CFG-3 | `setup` returns `{command, exit}`. `command(line)` queues a `:` line (a leading `:` is optional). The queued lines run in order before the terminal opens, and a failing line is reported. | ✅ |
| FR-CFG-4 | `exit(code[, msg])` stops startup, prints `Exiting as specified in the configuration, message: <msg>` and exits with the code. The first call wins. | ✅ |
| FR-CFG-5 | Defaults: syntax on, relative off, indent 4, tabstop 4, undo_size 32, cursorline off, mouse on, backup on, list off, all three autoformat steps on. | ✅ |
| FR-CFG-6 | `auto_indent` is accepted for compatibility but has no effect: `Enter`, `o` and `O` always auto-indent. | ✅ |
| FR-CFG-7 | Files that live next to the effective config: `<ext>.cyntax`, `recent` and `ai-history.json`. | ✅ |

### 6.3 Modes and input routing (`app.rs`, `editor.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-MODE-1 | Five modes: Normal, Insert, Visual (charwise, linewise, blockwise), Search and Command. `Esc`/`Ctrl-C` returns to Normal from any mode and clears any pending count or operator. | ✅ |
| FR-MODE-2 | Input is handled in this priority order: the assistant (`Ctrl-K` or a focused panel), then a pending `r`, `Ctrl-Q`, the mouse, the `:s` confirm prompt, the save prompt, the history list, a pending jump, `set-map` expansion, and finally the current mode's handler. | ✅ |
| FR-MODE-3 | `Ctrl-Q` quits, but refuses while there are unsaved changes. `Ctrl-S` saves and then quits. | ✅ |
| FR-MODE-4 | `Ctrl-Z` in Normal mode suspends the process (Unix SIGTSTP) and restores the screen on `fg`. | ✅ |
| FR-MODE-5 | `Ctrl-W` acts as a window prefix: it swallows the next key and shows `Cano has one window; splits are not supported`. | ✅ |
| FR-MODE-6 | The status line reads `MODE file [MD] (col,row) Saved/Modified`, with an AI button at the right end. The column is the 1-based display column. Cursor shape: a bar in Insert, hidden in Visual, the terminal default otherwise. | ✅ |

### 6.4 Normal-mode motions and editing (`buffer.rs`, `editor.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-NAV-1 | Motions `h j k l 0 $ w b e g G %` and the arrow keys. `%` skips brackets inside quotes. `h`/`l` move over whole UTF-8 characters, and `j`/`k` keep the display column. | ✅ |
| FR-NAV-2 | Counts work the Vim way. `N`+motion repeats the motion, and `Ng`/`NG` go to line N. `3x`, `3p`, `3rX`, `3*` and `3.` repeat. A count before a Ctrl key multiplies it. A count before `s`, `t`, `:`, `/` or space is dropped. | ✅ |
| FR-NAV-3 | Scrolling: `Ctrl-F/B` move a full page keeping 2 lines of overlap, `Ctrl-D/U` move half a page, and `Ctrl-E/Y` move one line. The cursor moves only when it must. | ✅ |
| FR-NAV-4 | EasyMotion `s{char}` (both directions) and `t{char}` (forward, stopping one character before). Labels come from `asdghklqwertyuiopzxcvbnmfj;`, nearest first, growing to two keys when needed. Only visible rows are targets. | ✅ |
| FR-EDIT-1 | `i I a A o O`, `x`, `r{char}` (any UTF-8 character; `Enter` splits the line), `p`, `u`, `U`/`Ctrl-R`, and `Ctrl-O` (open a line below without leaving Normal mode). `x` and Visual operators move exactly the affected text into the clipboard, as in Vim. | ✅ |
| FR-EDIT-2 | `Ctrl-A`/`Ctrl-X` on decimal and `0x` hex numbers. They keep zero padding and hex width/case, treat a leading `-` as part of the number, and move the cursor to the last digit. | ✅ |
| FR-EDIT-3 | `Ctrl-G` shows `"file" [Modified] [RO] N lines --P%--`. `Ctrl-L` redraws and clears the message line. | ✅ |
| FR-EDIT-4 | Space is the leader key: `i`=`*`, `o`=`:nohl`, `n`=explorer, `r`=recent, `l`=`:set list!`, `f`=`:autoformat`. In Visual mode, `e` toggles comments. | ✅ |

### 6.5 Operators, text objects, repeat, undo (`editor.rs`, `textobject.rs`, `app.rs`, `history.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-OP-1 | Operators `d`, `y` and `c` work with the motions `0 $ w b e g G`. Doubled (`dd yy cc`) they act on the whole line. `cw` on a non-blank behaves like `ce`. | ✅ |
| FR-OP-2 | Operator counts multiply: `3dw` = `d3w`, `2d3w` deletes six words, and `3dd`/`3yy`/`3cc` take three lines. A bare `3d` waits for its motion. `d2aw` takes two word objects. | ✅ |
| FR-OP-3 | Text objects `i`/`a` combined with `w p ( ) b [ ] { } B < > " ' \``. Bracket matching is quote-aware. A missing object does nothing. | ✅ |
| FR-OP-4 | A change is one undo step and one redo step: the deletion from `c`, `o` or `O`, plus everything typed in the same Insert session. An arrow key or a click ends the step. Visual `>`/`<` over several lines is one step. | ✅ |
| FR-OP-5 | `.` replays the last change with its original count. `N.` replaces that count. Motions and no-op changes don't replace the stored change, and a change interrupted by a prompt is dropped. | ✅ |
| FR-OP-6 | A new edit clears the redo stack. The undo stack keeps at most `undo_size` steps, dropping the oldest (`0` or less means unlimited). Every record, including `r` and `x`, can be redone after an undo. | ✅ |
| FR-OP-7 | Recursion guards: replay nesting is capped at 64 levels (`Recursive repeat`, `Recursive key map`), and one keypress can expand to at most 10,000 keys. | ✅ |

### 6.6 Insert mode (`editor.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-INS-1 | Keys: `Backspace`/`Ctrl-H` (delete one code point), `Ctrl-W` (delete word), `Ctrl-U` (to the indent, then the indent itself), `Ctrl-T`/`Ctrl-D` (shift the line), and `Tab`, which inserts `indent` spaces or a tab when `indent` is 0. | ✅ |
| FR-INS-2 | `Enter` auto-indents by brace depth. `(`, `[` and `{` auto-close, typing an existing closer steps over it, and `Enter` between a pair splits it onto three lines. | ✅ |
| FR-INS-3 | `Ctrl-N`/`Ctrl-P` complete from the words in the buffer, nearest first, wrapping around. Walking off either end restores what was typed (`Back at original`), and the status shows `match i of n`. | ✅ |
| FR-INS-4 | `Ctrl-O` runs one Normal command, showing `-- (insert) --`, and then returns to Insert, unless that command chooses a mode of its own. | ✅ |
| FR-INS-5 | `:imap lhs rhs`: an uninterrupted run of typed keys fires as soon as it is complete. Mapping the same left-hand side again replaces the old mapping. | ✅ |

### 6.7 Visual mode (`editor.rs`, `comment.rs`, `autoformat.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-VIS-1 | `v`/`V`/`Ctrl-V` enter Visual mode, and the motions (with counts) plus `s{char}` extend the selection. `v`/`V` switch kind, or leave when pressed in their own kind. | ✅ |
| FR-VIS-2 | `d`/`x` delete, including the character under the cursor; `y` yanks the same span. `>`/`<` shift, `=` formats the lines, and `Ctrl-E`/`space e` toggles comments. | ✅ |
| FR-VIS-3 | `c` deletes and enters Insert as one undo step with what is typed. `p` puts the clipboard over the selection and keeps the replaced text. `o` swaps the ends. `r{char}` overwrites every selected character except line breaks, as one step. | ✅ |
| FR-VIS-4 | In blockwise mode, `d`/`x` cut the rectangle as one undo step and `y` copies it. Short lines count as blank, and `p` pastes the rows as lines. | ⚠️ L-2 |
| FR-VIS-5 | The comment toggle uses the file type's marker (`//`, `#`, `--`, `"`) in an aligned column, skips blank lines, and is one undo step. It refuses on an unknown file type, JSON, or a read-only buffer. | ✅ |

### 6.8 Search and substitute (`buffer.rs`, `substitute.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-SRCH-1 | `/text` does a literal, case-sensitive search that wraps around. `n`/`N` take counts. Matches are highlighted. | ✅ |
| FR-SRCH-2 | `*` does a whole-word search for the word under the cursor. `:nohl` hides the highlight but keeps the pattern. | ✅ |
| FR-SRCH-3 | Legacy `/s/old/new` replaces the next occurrence after the cursor as one undo step, and refuses on a read-only buffer. | ✅ |
| FR-SUB-1 | `:[range]s/pat/rep/[gci]`. The range is `%`, `N`, `.`, `$` or `a,b`. The command name can be any prefix of `substitute`, and any punctuation except `\` can be the delimiter. | ✅ |
| FR-SUB-2 | Patterns are literal and support `\<`/`\>` word boundaries. `\` escapes the delimiter and itself. | ✅ |
| FR-SUB-3 | The `c` flag asks `Replace? (y/n/a/q)` before each match. The whole command is one undo step and reports `N substitutions on M lines`. It refuses on a read-only buffer. | ✅ |

### 6.9 Command line (`command.rs`, `app.rs`, `process.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-CMD-1 | `:w`, `:q`, `:q!`, `:wq`, and the legacy `:e` (a guarded quit) and `:we`. | ✅ |
| FR-CMD-2 | `:set` accepts `name`, `noname`, `name!`, `name=N` and `name?`, several per line, and Vim aliases (`rnu sw ts ai cul bk syn lcs …`). Out-of-range values give `Invalid value for <name>: <v>`. | ✅ |
| FR-CMD-3 | `set-var`, `set-output "path"`, `set-map key "rhs"`, `let name [=] expr`, `echo`. Expressions are integers evaluated left to right. | ✅ |
| FR-CMD-4 | `:!cmd` runs `sh -c` on a worker thread, and the editor stays responsive. While it runs, the message line shows `Running: <cmd> (Ns)`. The result is the last non-blank stdout line, falling back to stderr, prefixed with `[exit N]` or `[signal N]` on failure. Only one command runs at a time. | ✅ |
| FR-CMD-5 | `:autoformat`/`:Autoformat`, and `:nohl`/`:nohlsearch`. | ✅ |
| FR-CMD-6 | The `:` and `/` prompts each keep a history of 50 distinct entries for the session. `Up`/`Down` walk it, and `Ctrl-F` opens it as a list. | ✅ |
| FR-CMD-7 | `:help [page]` / `:h [page]` opens `general`, `keys` or `cmds` read-only in place of the buffer. It refuses unsaved changes; `:help!` discards them. The file being left goes to the front of the recent list. | ✅ |
| FR-CMD-8 | `:e <file>` and `:w <file>`. | ⛔ |

### 6.10 Display (`render.rs`, `display.rs`, `syntax.rs`, `markdown.rs`, `listchars.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-UI-1 | The layout is: a gutter (at least 4 digits, relative numbering available), the text, a scrollbar in the last column, an optional AI panel, the status line, and a prompt/message line. Messages auto-clear after about 1 s. | ✅ |
| FR-UI-2 | `cursorline` underlines the cursor's line. The selection is shown in reverse video, search matches get a highlight background, and jump labels get a distinct style. | ✅ |
| FR-UI-3 | Valid UTF-8 is drawn as characters at their display width: 2 cells for CJK, fullwidth and emoji, and 0 for combining marks, which join the previous cell. Invalid bytes and control characters show as `�`. A wide character cut by a screen edge is drawn blank. | ✅ |
| FR-UI-4 | Each tab is drawn `tabstop` columns wide (1–32, default 4), separate from `indent`. | ⚠️ L-1 |
| FR-UI-5 | Syntax highlighting for C, C++, Rust, Python, shell, Vim script, Lua and JSON, chosen by extension or by known dotfile name. Each language has its own scanner. | ✅ |
| FR-UI-6 | `<ext>.cyntax` palettes contain `k`/`t`/`w` groups with RGB colours and word lists. An empty `k`/`t` list falls back to the built-in list. A malformed palette is ignored. | ✅ |
| FR-UI-7 | Markdown display, on by default for `.md`-family files and toggled with `Ctrl-M`/`Enter`, colours the text in place without moving any character. | ✅ |
| FR-UI-8 | `:set list` with `listchars` (`tab trail eol nbsp space`). The default draws all five, and the glyphs don't shift the layout. | ✅ |

### 6.11 Formatting (`autoformat.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-FMT-1 | On a `.json` file, `:autoformat` validates and pretty-prints at the `indent` width. It keeps key order, duplicate keys, number spellings, escapes and the final newline. On invalid JSON it reports `Invalid JSON: …` and changes nothing. | ✅ |
| FR-FMT-2 | On any other file it re-indents, retabs (measuring tabs with `tabstop`) and strips trailing spaces, each step behind its own option. Brackets inside strings are ignored, and the line count never changes. | ✅ |
| FR-FMT-3 | Each format is one undo step. It reports `Formatted N lines` or `Already formatted`, and refuses on a read-only buffer. | ✅ |

### 6.12 Files, panes, backups (`io.rs`, `explorer.rs`, `recent.rs`, `backup.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-FILE-1 | Load and save are byte-exact: no decoding, no line-ending conversion, no added final newline. | ✅ |
| FR-FILE-2 | Saves are atomic: a temporary file next to the target is fsynced, then renamed over it. The file keeps its permissions (on Unix, also its owner when allowed). Symlinks are followed, up to 40 hops, and kept. Read-only files are refused. FIFOs, devices and hard-linked files are written in place. | ✅ |
| FR-FILE-3 | A failed save keeps the buffer marked modified, shows `Could not write …`, and cancels any pending quit. Read-only (help) buffers refuse to save with `Buffer is read-only`. | ✅ |
| FR-PANE-1 | The explorer (`Ctrl-N`) lists the working directory, including hidden files, with a trailing `/` on directories and a `..` entry. Symlinks are not listed. | ✅ |
| FR-PANE-2 | The recent list (`Ctrl-P`) shows up to 50 files, most recent first, stored as absolute paths. It survives restarts, skips deleted files, and leaves out help pages. | ✅ |
| FR-PANE-3 | When the buffer has unsaved changes, opening either pane first asks `Save changes? (y/n, Esc cancels)`. A failed save keeps the pane shut. | ✅ |
| FR-BAK-1 | Before each save, the file on disk is copied to `<dir>/.backups/<file>.<YYYYMMDD-HHMMSS>` (UTC, with a `.N` suffix on a collision), keeping the original's permissions. The 10 newest are kept. | ✅ |
| FR-BAK-2 | A failed backup is reported (`Could not back up …`) but does not block the save. A file that doesn't exist yet gets no backup. The `backup` option turns the feature off. | ✅ |

### 6.13 Mouse (`terminal.rs`, `app.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-MOUSE-1 | Click moves the cursor (a click on either half of a wide character goes to its start), drag selects (charwise Visual), the wheel scrolls 3 lines, and the scrollbar responds to click and drag. | ✅ |
| FR-MOUSE-2 | In the lists, a click selects an entry and a second click on the selected entry opens it. Their scrollbars only move the selection. | ✅ |
| FR-MOUSE-3 | The `mouse` option toggles mouse capture immediately. A click never answers a prompt or picks a jump label. | ✅ |

### 6.14 AI assistant (`assistant.rs`)

| ID | Requirement | Status |
|----|-------------|--------|
| FR-AI-1 | The assistant is available only when `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` is set. Anthropic is preferred unless `CANO_AI_PROVIDER=openai`. `CANO_AI_MODEL` and `CANO_AI_EFFORT` override the model (default `claude-opus-5-5`, or `gpt-5` for OpenAI) and the effort (default `medium`). | ✅ |
| FR-AI-2 | `Ctrl-K` or the status-bar `AI` button toggles a docked panel that starts at 20% of the screen height and is at least 3 rows. Dragging the title bar resizes it. Without a key, `Ctrl-K` explains how to turn the assistant on. | ✅ |
| FR-AI-3 | Panel keys: `Enter` sends; `Esc` stops a request or returns focus to the editor; `Ctrl-L` or sending `/clear` starts a new chat, even while a request is running; the arrows, PgUp/PgDn, the wheel and the scrollbar scroll; and the input line supports basic line editing. | ✅ |
| FR-AI-4 | The system prompt embeds the `general`, `keys` and `cmds` help pages, so answers stay grounded in Cano's own reference. | ✅ |
| FR-AI-5 | Tools: `get_editor_state`, `read_buffer`, `edit_buffer` (1-based lines, one undo step per call), `run_editor_command` (cannot quit), `read_file`, `list_directory`, `run_shell` and `write_file` (cannot target the open file). | ✅ |
| FR-AI-6 | `run_shell` and `write_file` need per-call approval: `y`/`Enter` allows, `n` refuses, `a` allows the tool for the rest of the session, and `Esc` cancels the task. A cancelled round still answers every tool call so the conversation stays valid. | ✅ |
| FR-AI-7 | Limits: 40 rounds per request, 30,000 characters per tool result, 2,000 lines per read, 400 transcript entries, and a 600 s HTTP timeout. | ✅ |
| FR-AI-8 | HTTP goes through `curl` on a worker thread. The key is sent on curl's stdin, never on its command line. Shell tools run on a worker thread so the UI never blocks. | ✅ |
| FR-AI-9 | The conversation is saved to `ai-history.json` (mode 0600, atomic rename) after each completed round and restored at startup. The saved API context is reused only when the provider and model match. Clearing the chat deletes the file. | ✅ |

---

## 7. Non-functional requirements

| ID | Requirement | Status |
|----|-------------|--------|
| NFR-1 Size | The release profile is `opt-level="z"`, LTO, `codegen-units=1`, stripped, with `panic="abort"`. The binary is about 1.1 MB. No HTTP/TLS or Unicode crate is used; the assistant shells out to `curl`, and character widths come from a small built-in table. | ✅ |
| NFR-2 Dependencies | crossterm 0.29, ratatui 0.30, mlua 0.12 (vendored Lua 5.4, so a C compiler is needed), serde_json, and `libc` on Unix only. | ✅ |
| NFR-3 Platforms | Linux, macOS and Windows. On Windows, `:!` and `run_shell` need an `sh` on `PATH`, and suspend has nothing to suspend into. Prebuilt archives are produced for 5 targets: Linux x86-64/ARM64, macOS x86-64/ARM64 and Windows x86-64. The Nix flake builds on x86_64-linux. | ✅ |
| NFR-4 Robustness | No input panics in normal use. Debug builds check design-by-contract `debug_assert!` invariants, including that a group stays open only in Insert mode and that the cursor is on a character boundary after a motion. A panic hook restores the terminal before aborting. | ✅ |
| NFR-5 Responsiveness | The event loop polls every 100 ms, and messages time out after about 1 s. `:!` commands, assistant requests and shell tools run off the UI thread. | ✅ |
| NFR-6 Security | The Lua config is trusted code with the full standard library. API keys never appear in argv or `Debug` output. The chat history file is private to the user. Shell commands and file writes from the AI need approval. `:help` only opens the three shipped page names, never an arbitrary path. | ✅ |
| NFR-7 Testability | About 440 unit tests, 8 binary tests, 6 `:help` tests and 5 lifecycle integration tests, plus a PTY smoke test (`make smoke`, Python 3). `cargo fmt` and `clippy -D warnings` must pass. GitHub Actions builds every push for all five targets. | ✅ |
| NFR-8 Data integrity | Saves are atomic and backed up first, a failed save never discards the buffer, and pane switches, `:help` and quits are guarded. SIGTERM/SIGHUP restore the terminal and exit with 128 + signal, without saving. | ✅ |

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

- **State.** The buffer is a flat `Vec<u8>`, plus a row index that is rebuilt
  after each edit and a byte cursor that always sits on a character boundary
  in Normal mode. `display.rs` maps bytes to screen cells. Undo and redo are
  stacks of inverse records; records can be joined into groups, and the undo
  stack is capped. There is one unnamed clipboard.
- **Effects.** `App::handle(Input) -> Vec<AppEffect>` stays pure with respect
  to I/O. `main.rs` carries out `Save`, `Quit`, `Shell`, `Suspend` and
  `Redraw`, so the logic can be tested without a terminal. A shell command
  becomes a background job that the main loop polls.
- **Assistant.** It runs as a state machine on `App`:
  1. A worker thread runs `curl`.
  2. `assistant_poll()` collects the replies each frame.
  3. Editor tools run on the main thread between frames.
  4. Shell tools run on another worker thread.
  5. Approval pauses the tool queue.

## 9. User-visible messages

These are the messages Cano shows, grouped by where they appear. Each has
been checked against the source.

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

Each check can be run by hand against a release build. Most are also covered
by the unit, integration or PTY smoke tests.

1. `cano --version` prints exactly `cano 0.26.1016` and exits 0.
2. `cano -x` and `cano --helpful` print `Unexpected flag` and exit 1.
3. `cano --help=cmds` opens the command reference read-only, even with no help
   directory installed. `cano --help=bogus` exits 1 and lists the pages.
4. `:s/foo/bar/g` on a line containing `foo foo` replaces both and reports
   `2 substitutions on 1 line`.
5. `:%s/a/b/gc` prompts `Replace? (y/n/a/q)` for each match, and `a` replaces
   all remaining matches. One `u` undoes the lot.
6. `:q` on a modified buffer shows `No write since last change (add ! to
   override)` and does not exit; `:q!` exits.
7. `:set rnu`, `:set nornu`, `:set rnu!` and `:set rnu?` turn the option on,
   off, toggle it and report it. `:set ts=8` changes tab width but not `sw`.
   `:set ts=0` is refused.
8. On a `.json` buffer, `space f` pretty-prints valid JSON and reports
   `Invalid JSON: …` for invalid input.
9. `s` followed by a character shows labels drawn from `asdghkl…` and jumps
   when a label is typed.
10. `Ctrl-A` on `0x0f` produces `0x10`; `007` becomes `008`.
11. Opening 51 different files leaves exactly 50 entries in `recent`, newest
    first.
12. With `backup` on, `:w` of an existing file creates
    `.backups/<name>.<YYYYMMDD-HHMMSS>` next to it, with the same permissions.
13. `ciwfoo<Esc>` then `u` restores the original word in one step.
14. `3dd` deletes three lines and leaves nothing pending. `d3w` and `2d3w`
    delete three and six words. `3x` then `.` deletes three more.
15. In Visual mode, `c`, `p`, `o`, `r` and `v`/`V` behave as in FR-VIS-3.
16. A file containing `héllo 日本` draws those glyphs, and `l` moves one
    character at a time.
17. `:w` on a file with mode 0640 or a symlink keeps the mode and the link.
18. `:!sleep 2; false` keeps the editor responsive and then shows `[exit 1]`.
19. `kill -TERM <pid>` restores the terminal and exits 143.
20. Sending `/clear` in the AI panel empties the chat and deletes
    `ai-history.json`.

## 11. Changes in 0.26.1016

These defects were found while reverse-engineering 0.26.921, and all are
fixed in 0.26.1016. Each fix has regression tests.

| ID | Area | Was | Now |
|----|------|-----|-----|
| G-1 | Undo | `c` took two `u` presses. | A change, plus what is typed after it, is one undo and one redo step. `o`, `O` and Visual `>`/`<` are one step too. |
| G-2 | Search | `/s/old/new` couldn't be undone and ignored read-only buffers. | It is one undo step and refuses on a read-only buffer. |
| G-3 | Counts | Counts did nothing useful before `.`, `r` or `*`; `3dd` left `d` pending; `d3w` moved instead of deleting. | Vim count semantics everywhere (FR-NAV-2, FR-OP-2, FR-OP-5). A bare `3d` now waits for its motion. |
| G-4 | Undo | Redo wasn't cleared by new edits, `undo_size` did nothing, `r` couldn't be redone, and an insert's undo was off by one. | All four fixed (FR-OP-6). |
| G-5 | Display | Tabs were always 4 columns and `ts` was an alias of `indent`; UTF-8 showed one cell per byte. | There is a separate `tabstop` option, and characters are drawn at their display width with character-wise motions (FR-UI-3, FR-UI-4). |
| G-6 | Clipboard | `x` copied an extra byte (a NUL at the end of the file); Visual `d` left out the cursor character. | Both follow Vim (FR-EDIT-1, FR-VIS-2). |
| G-7 | Visual | `c`, `p`, `o`, `r`, `v`/`V` and counts did nothing. | All implemented (FR-VIS-1, FR-VIS-3). |
| G-8 | AI | The help promised `/clear`, but nothing handled it. | `/clear` starts a new chat. |
| G-9 | Shell | `:!` blocked the UI and dropped stderr and the exit status. | It runs in the background and reports output and status (FR-CMD-4). |
| G-10 | Save | Saves weren't atomic, and permissions and symlinks were lost. Backups lost permissions. SIGTERM/SIGHUP left the terminal in raw mode. | All fixed (FR-FILE-2, FR-BAK-1, NFR-8). |
| G-11 | Packaging | The flake version and the example `listchars` escape were wrong. | Both fixed. A GitHub Actions workflow now builds all five targets. |
| G-12 | Help | Only `general` was reachable, and `--help*` and `--config*` were prefix matches. | `--help=<page>` and `:help` reach every page, and flags are matched exactly (FR-CLI-3, FR-CLI-4, FR-CMD-7). |

## 12. Known limitations

| ID | Limitation | Possible follow-up |
|----|------------|--------------------|
| L-1 | Each tab is `tabstop` columns wide wherever it sits, rather than aligned to the next tab stop as in Vim. | Align to tab stops. Retab's arithmetic would have to follow. |
| L-2 | Blockwise Visual counts its columns in characters, not screen cells, so a block can look ragged next to wide characters. | Lay blocks out in display columns. |
| L-3 | Quitting while a `:!` command runs leaves the command running. | Kill the child process, or ask before quitting. |
| L-4 | `undo_size` can't be set to a number through `setup()`, because only booleans count there. Use `cano.command("set undo_size=N")`. | Accept integers in `setup()`. Would break compatibility with C-Cano's semantics. |
| L-5 | `run_editor_command` in the assistant can save but never quit. | By design. |

## 13. Success metrics

| Metric | Target |
|--------|--------|
| Test suite | 100% passing on `cargo test --locked --all-targets` (currently 455/455) and `make smoke` |
| Lint | `cargo clippy -D warnings` and `cargo fmt --check` are clean |
| CI | The release workflow is green on all five targets for every push |
| Binary size | ≤ 1.2 MB stripped release build (currently about 1.1 MB) |
| Cold start | Under 50 ms to the first frame on a typical machine, without the assistant |
| Crash rate | Zero panics under a randomized key-sequence stress run (the DbC report's 30k-seed run) |
| Compatibility | An unmodified C-Cano `init.lua` and `.cyntax` load without errors |
| Assistant quality | It answers key and command questions from the bundled reference; every buffer edit it makes is undoable with `u` |

## 14. Open questions

1. Should the remaining C compatibility behaviours (`/s/` at the search
   prompt, `:e` meaning exit, boolean-only `setup()`) move behind a `compat`
   option, so the defaults can follow Vim entirely?
2. Should the assistant gain a `:` command (for example `:ai <prompt>`) so it
   can be driven from mappings and `cano.command`?
3. Should the assistant know the `init.lua` and `.cyntax` locations, so it can
   edit configuration on request?
4. An independent analysis of the 0.26.918 Windows binary found that
   `cano.exit()` and a bad `setup()` argument aborted the process. Neither
   reproduces on Linux in 0.26.1016 (a clean exit code, or a reported
   configuration error). It should be confirmed on Windows using the new
   release build.
