# Cano

Cano (kay-no) is a small Vim-inspired modal terminal editor written in Rust on
[Ratatui](https://ratatui.rs) and [Crossterm](https://github.com/crossterm-rs/crossterm),
configured in Lua 5.4, with an optional built-in AI assistant.

This implementation was written independently from migration specifications of
the original C editor and does not reuse its source or an earlier Rust port.
The original C implementation lives at
[Cano-Projects/Cano](https://github.com/Cano-Projects/Cano). See
[`MIGRATION_NOTES.md`](MIGRATION_NOTES.md) for the compatibility choices this
port keeps, and [`PRD.md`](PRD.md) for the product requirements.

[![asciicast](https://asciinema.org/a/655184.svg)](https://asciinema.org/a/655184)

## Contents

- [Highlights](#highlights)
- [Quick start](#quick-start)
- [Installation](#installation)
- [Command-line usage](#command-line-usage)
- [Modes](#modes)
- [Key reference](#key-reference)
- [Operators, text objects and repeat](#operators-text-objects-and-repeat)
- [Visual mode](#visual-mode)
- [Search, highlight and replace](#search-highlight-and-replace)
- [Commands](#commands)
- [Options](#options)
- [Configuration (`init.lua`)](#configuration-initlua)
- [Syntax highlighting](#syntax-highlighting)
- [Markdown display](#markdown-display)
- [Invisible characters](#invisible-characters)
- [Autoformat](#autoformat)
- [File explorer and recent files](#file-explorer-and-recent-files)
- [Backups](#backups)
- [Mouse](#mouse)
- [AI assistant](#ai-assistant)
- [Files Cano reads and writes](#files-cano-reads-and-writes)
- [Behaviour notes and limitations](#behaviour-notes-and-limitations)
- [Development](#development)
- [Project layout](#project-layout)
- [License](#license)

## Highlights

- Five modes (Normal, Insert, Visual, Search, Command) with charwise, linewise
  and blockwise Visual selection.
- The `d`/`y`/`c` operators, the text objects `iw aw ip ap i( a" …`, the `.`
  repeat, Vim-style counts (`3dd`, `d3w`, `3.`), and EasyMotion-style `s`/`t`
  jumps.
- UTF-8 aware display and motions: wide CJK and emoji characters take two
  cells, and `h`/`l`/`x`/`r` move over whole characters.
- Vim-style `:s` substitute with ranges and the `g`/`c`/`i` flags, `*`
  word search with highlighting, and `:set` with vim option names.
- Built-in highlighting for C, C++, Rust, Python, shell, Vim script, Lua and
  JSON, plus custom `.cyntax` palettes.
- In-place Markdown display, `:set list`, `:autoformat` (JSON pretty-print or
  whitespace tidy), and line-comment toggling.
- A file explorer, a persistent recent-files picker, per-session prompt
  history, and automatic timestamped backups on save.
- Mouse support: click, drag-select, wheel, and a clickable scrollbar.
- An optional AI chat panel (Anthropic or OpenAI). It can explain Cano, edit
  the buffer as undoable steps, and run commands with your permission.
- Atomic saves that keep permissions and symlinks, and `:!` commands that run
  in the background.
- A release binary of about 1 MB, built for size, with prebuilt archives for
  Linux, macOS and Windows on each GitHub release.

## Quick start

```sh
cargo build --release --locked
./target/release/cano path/to/file
```

If the path does not exist, Cano opens an empty buffer and creates the file on
the first save. With no path at all, the buffer is named `out.txt`.

Press `i` to type and `Esc` to stop. Use `:w` to save and `:q` to quit
(`:q!` discards changes). Press `Ctrl-K` for the AI assistant if an API key is
set.

## Installation

### Prebuilt binaries

Each [GitHub release](https://github.com/Phorena-Inc/cano-editor-rust/releases)
has archives for Linux (x86-64, ARM64), macOS (Intel, Apple Silicon) and
Windows (x86-64). Unpack one and put `bin/cano` on your `PATH`. The archive
also contains `docs/help`, but the help pages are built into the binary as
well, so it works on its own.

### From source

Every platform needs a current stable Rust toolchain (installed with
[rustup](https://rustup.rs)). The code uses let-chains, so use a recent stable
release. You also need a **C compiler**, because `mlua` compiles the vendored
Lua 5.4 during the build. These runtime tools are optional:

- `sh`, for `:!command` and the assistant's `run_shell` tool;
- `curl`, for the AI assistant.

#### Linux

```sh
# Debian/Ubuntu: sudo apt install build-essential
# Fedora:        sudo dnf group install development-tools
# Arch:          sudo pacman -S base-devel
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
make                                  # release build, also copied to build/cano
sudo make install PREFIX=/usr/local   # /usr/local/bin/cano + /usr/local/share/cano/help
```

`make install` builds with `CANO_HELP_DIR=$(PREFIX)/share/cano/help` baked in.
Pass the same `PREFIX` to `make` and `make install` so that the binary and the
help pages agree. `make uninstall` removes both.

On Nix (x86_64-linux), run `nix build .#cano`. `nix develop` gives you a shell
with cargo, rustc, rustfmt and clippy.

#### macOS

```sh
xcode-select --install
brew install rustup-init && rustup-init   # or the rustup script above
make && sudo make install PREFIX=/usr/local
```

#### Windows

```powershell
winget install Rustlang.Rustup     # accept the Visual Studio C++ Build Tools
cargo build --release --locked
.\target\release\cano.exe .\file.txt
```

Copy `cano.exe` to a directory on your `PATH`. Windows has no `sh`, so
`:!command` does not work there unless an `sh` is on `PATH`. `Ctrl-Z` has
nothing to suspend into.

Prebuilt packages such as the AUR one are for the original C implementation.

## Command-line usage

```
cano [--config <init.lua>] [file]
cano -h | --help
cano -v | --version
```

| Argument | Effect |
|----------|--------|
| `file` | The file to edit. Extra positional arguments are ignored. Defaults to `out.txt`. |
| `--config <path>` / `--config=<path>` | Use this Lua config instead of `~/.config/cano/init.lua`. The file must exist. If given twice, the last one wins. |
| `-h`, `--help` | Open the general help page read-only in the editor. |
| `--help=<page>` | Open `general`, `keys` or `cmds`. An unknown page lists the pages and exits 1. |
| `-v`, `--version` | Print `cano <version>` and exit. This takes precedence over `--help`. |

Flags must be spelled exactly, so `--helpful` or `--configuration` is an
error, as is any other argument that starts with `-`.

**Exit status:**

- `0` after a normal quit, `--version`, or a startup command that quits.
- `1` for an argument, configuration, file-open or terminal error.
- `128 + signal` after `SIGTERM` (143) or `SIGHUP` (129). The terminal is
  restored first, and the buffer is not saved.
- Otherwise, the code passed to `cano.exit()`. A negative value keeps its low
  byte, so `-1` exits as 255. Values above 255 become 255.

Cano looks for help pages in these places, in order:

1. `$CANO_HELP_DIR`;
2. the `CANO_HELP_DIR` baked in at build time;
3. `docs/help` or `share/cano/help` relative to the binary;
4. `./docs/help`;
5. the copies compiled into the binary.

All three pages are compiled in, so help always works. Inside the editor,
`:help [page]` opens the same pages.

## Modes

| Mode | Purpose | Enter with | Status line shows |
|------|---------|------------|-------------------|
| Normal | Motions, operators, commands | `Esc` / `Ctrl-C` from any mode | `NORMAL` |
| Insert | Typing text | `i I a A o O`, `c…` | `INSERT` |
| Visual | Selecting text (charwise, linewise, blockwise) | `v`, `V`, `Ctrl-V` | `VISUAL` |
| Search | Typing a `/` search | `/` | `SEARCH` |
| Command | Typing a `:` command | `:` | `COMMAND` |

The status line reads `MODE file [MD] (col,row) Saved|Modified`. It ends with
an `AI` button when the assistant is available. The column is the display
column, so a tab or a wide character counts as the cells it takes.
Messages on the bottom line clear themselves after about a second.

## Key reference

### Anywhere

| Key | Action |
|-----|--------|
| `Esc` / `Ctrl-C` | Back to Normal mode. Cancels a pending count, operator or prompt. |
| `Ctrl-Q` | Quit. Refuses while there are unsaved changes. |
| `Ctrl-S` | Save, then quit (Normal and Insert). |
| `Ctrl-K` | Open or close the AI assistant panel. |

### Normal mode — movement

| Key | Action |
|-----|--------|
| `h j k l`, arrows | Left, down, up, right |
| `0` / `$` | Start / end of line |
| `w` / `b` / `e` | Next word / previous word / end of word |
| `g` / `G` | First line / last line |
| `N g`, `N G` | Go to line N |
| `%` | Matching bracket (ignores brackets inside quotes) |
| `s{char}` | EasyMotion: label every visible `{char}` in both directions and jump to the one whose label you type |
| `t{char}` | EasyMotion: forward only; lands one character before the match |
| `Ctrl-F` / `Ctrl-B` | Forward / back one screen (keeps 2 lines of context) |
| `Ctrl-D` / `Ctrl-U` | Forward / back half a screen |
| `Ctrl-E` / `Ctrl-Y` | Scroll one line down / up without moving the cursor unless it would leave the view |
| `N` + motion | Repeat the motion N times. A count multiplies the scroll keys too. |

EasyMotion labels come from `asdghklqwertyuiopzxcvbnmfj;` and are given out
nearest-first. When there are more matches than keys, the labels become two
keys long. A single match jumps immediately. `Esc`, or a key that matches no
label, cancels. Only rows on screen are targets.

### Normal mode — editing

| Key | Action |
|-----|--------|
| `i` / `I` | Insert before the cursor / at the first non-blank |
| `a` / `A` | Insert after the cursor / at the end of the line |
| `o` / `O` | Open an auto-indented line below / above |
| `Ctrl-O` | Open a line below and stay in Normal mode |
| `x` | Delete the character (`3x` deletes three) |
| `r{char}` | Replace the character (`3rX` replaces three; `Enter` writes a newline; `Esc` cancels) |
| `dd` / `yy` / `cc` | Delete / yank / change the line (`3dd` takes three lines) |
| `d`/`y`/`c` + motion or text object | See [Operators](#operators-text-objects-and-repeat) |
| `p` | Paste. A line-wise clipboard pastes below the current line. `3p` pastes three times. |
| `u` | Undo |
| `U` / `Ctrl-R` | Redo |
| `.` | Repeat the last change (`3.` repeats it with a count of 3) |
| `Ctrl-A` / `Ctrl-X` | Add / subtract 1 (or the count) from the number at or after the cursor |

`Ctrl-A`/`Ctrl-X` read decimal and `0x` hexadecimal numbers. A leading `-`
belongs to the number. Zero padding is kept (`007` becomes `008`), hex numbers
keep their width and letter case, and the cursor ends on the last digit.

### Normal mode — panes, search and misc

| Key | Action |
|-----|--------|
| `/` · `n` · `N` | Search · next match · previous match (both wrap) |
| `*` | Search the word under the cursor as a whole word and highlight every match (`3*` goes to the third) |
| `:` | Command prompt |
| `:help` | Open a help page (see [Commands](#commands)) |
| `v` / `V` / `Ctrl-V` | Charwise / linewise / blockwise Visual mode |
| `Ctrl-N` | Toggle the file explorer |
| `Ctrl-P` | Toggle the recent-files list |
| `Ctrl-M` / `Enter` | Toggle Markdown display (most terminals send the same byte for both) |
| `Ctrl-G` | Show the file name, line count and position |
| `Ctrl-L` | Redraw the screen and clear the message |
| `Ctrl-W` | Vim's window prefix. Cano has no splits, so it swallows the next key and says so. |
| `Ctrl-Z` | Suspend to the shell; `fg` resumes (Unix only) |

### Leader (space)

| Keys | Same as |
|------|---------|
| `space i` | `*` |
| `space o` | `:nohl` |
| `space n` | `Ctrl-N` (explorer) |
| `space r` | `Ctrl-P` (recent files) |
| `space l` | `:set list!` |
| `space f` | `:autoformat` |
| `space e` (Visual) | `Ctrl-E`, which toggles comments |

### Insert mode

| Key | Action |
|-----|--------|
| `Esc` / `Ctrl-C` | Back to Normal |
| `Backspace` / `Ctrl-H` | Delete the character before the cursor |
| `Ctrl-W` | Delete the word before the cursor |
| `Ctrl-U` | Delete back to the first non-blank; press again to delete the indent |
| `Ctrl-T` / `Ctrl-D` | Indent / unindent the line by one shiftwidth |
| `Tab` | Insert `indent` spaces, or a tab when `indent` is 0 |
| `Enter` | New line, auto-indented to the bracket depth |
| `Ctrl-N` / `Ctrl-P` | Complete the word before the cursor from words in the buffer, nearest first |
| `Ctrl-O` | Run one Normal-mode command, then return to Insert |
| Arrows | Move the cursor (this ends the current undo step) |

Typing `(`, `[` or `{` inserts the closing bracket too. Typing a closing
bracket over one that is already there steps over it. `Enter` between a bracket
pair opens an indented middle line.

Completion walks the list on each press. Walking off either end restores what
you typed. The status line shows `match 2 of 5`.

### Command and Search prompts

| Key | Action |
|-----|--------|
| `Enter` | Run |
| `Esc` / `Ctrl-C` | Abandon the line |
| `Left` / `Right`, `Backspace` | Edit the line |
| `Up` / `Down` | Walk this prompt's history |
| `Ctrl-F` | Open the history as a list (`j`/`k` move, `Enter` runs, `Esc` closes) |

Each prompt keeps its last 50 distinct lines for the current session.

## Operators, text objects and repeat

`d` (delete), `y` (yank) and `c` (change) take one of these targets:

- a motion: `0 $ w b e g G`;
- a doubled key, which acts on the whole line: `dd`, `yy`, `cc`;
- a text object.

Any other key cancels the operator. `c` deletes and then enters Insert mode,
and the deletion plus everything you type is **one undo step**. `cw` on a word
changes only to the end of the word, as in Vim.

**Counts** reach the motion the way they do in Vim. `3dw` and `d3w` both
delete three words, and `2d3w` deletes six. `3dd`, `3yy` and `3cc` take three
lines. A bare `3d` waits for its motion. `d2aw` takes two words with their
blanks. A count before `s`, `t`, `:`, `/` or space is dropped.

| Text object (`i` inner / `a` around) | Selects |
|--------------------------------------|---------|
| `w` | The run of word characters, punctuation or blanks under the cursor |
| `p` | The paragraph: the run of blank or non-blank lines |
| `(` `)` `b` | The innermost parentheses |
| `[` `]` | The innermost square brackets |
| `{` `}` `B` | The innermost braces |
| `<` `>` | The innermost angle brackets |
| `"` `'` `` ` `` | The quoted string under the cursor, or the next one on the line |

The `a` form includes the delimiters. `aw` and `a"` also take the following
blanks, or the preceding blanks at the end of a line. Bracket matching ignores
brackets inside quotes. A text object that doesn't exist, such as `di(` outside
any parentheses, does nothing.

**`.` repeat.** `.` replays the last change. A change is any of these:

- `x`, `p` or `r`;
- an operator together with its target;
- `Ctrl-A` / `Ctrl-X`;
- an Insert session together with everything typed before `Esc`.

`.` repeats a change with the count it was made with, so `3x` then `.`
deletes three more. A count in front of `.` replaces that count, and the new
count sticks for the next `.`.

Motions, searches, undo and panes are not changes. A command that changed
nothing does not replace the stored change, and neither does one interrupted
by a `:` or `/` prompt. Visual-mode edits are not recorded. The keys are
replayed as typed, so a mapped key repeats as the mapping.

## Visual mode

| Key | Action |
|-----|--------|
| `h j k l 0 $ w b e g G %`, arrows | Extend the selection (counts work: `3j`) |
| `s{char}` | Extend the selection to an EasyMotion label |
| `d` / `x` | Delete the selection, including the character under the cursor |
| `y` | Yank the selection |
| `c` | Delete the selection and type its replacement (one undo step). Linewise keeps one empty line. |
| `p` | Put the clipboard over the selection; the replaced text goes into the clipboard |
| `o` | Move the cursor to the other end of the selection |
| `r{char}` | Overwrite every selected character (line breaks stay) |
| `v` / `V` | Switch to charwise / linewise, or leave Visual mode when pressed in its own kind |
| `>` / `<` | Indent / unindent the selected lines |
| `=` | Format the selected lines (whitespace tidy; see [Autoformat](#autoformat)) |
| `Ctrl-E` / `space e` | Toggle line comments |
| `Ctrl-V` | Make the selection blockwise, or leave Visual mode if it already is |

**Blockwise** (`Ctrl-V`) selects a rectangle:

- `d`/`x` cut it in one undo step, and `y` copies it.
- Lines too short to reach the block count as blank.
- The clipboard has no shape, so `p` pastes the rows back as lines.
- `>`, `<` and `=` act on every row the block touches.

**Comment toggle** uses the marker for the file type: `//` for C, C++ and
Rust, `#` for Python and shell, `--` for Lua, `"` for Vim script.

- If every non-blank selected line is already commented, it uncomments them.
  Otherwise it comments them all, aligned to the shallowest indent.
- Blank lines are skipped.
- The whole toggle is one undo step.
- For an unknown file type, JSON, or a read-only buffer it reports an error
  instead.

## Search, highlight and replace

**`/text`** searches for literal, case-sensitive text after the cursor,
wrapping at the end. Matches stay highlighted. `n`/`N` repeat forward and back
and accept a count. `*` searches the word under the cursor as a whole word.
`:nohl` (or `:nohlsearch`) hides the highlight but keeps the pattern for `n`.

**`/s/old/new`** is a legacy shortcut from the C editor. It replaces the
*next* `old` after the cursor and jumps to it, as one undo step, and is
refused on a read-only buffer. It takes no flags; use `:s` for those.

**`:s` substitute:**

```
:[range]s/pattern/replacement/[flags]
```

| Part | Meaning |
|------|---------|
| range | `%` the whole file, `N` one line, `.` the current line, `$` the last line, `a,b` a span. Omitted means the current line. |
| `s` | Any prefix of `substitute` also works. |
| delimiter | Any punctuation except `\`, so `:%s#http://#https://#g` works. |
| pattern | Literal text, not a regex. `\<` and `\>` mark word boundaries. `\` escapes the delimiter and itself. |
| flags | `g` replaces every match on a line; `c` asks before each one (`y` yes, `n` no, `a` all the rest, `q`/`Esc` stop); `i` ignores case. |

One `:s` command, confirmed or not, is a single undo step. It is refused on
read-only buffers.

## Commands

Press `:`, type, press `Enter`.

| Command | Action |
|---------|--------|
| `:w` | Save (to the `set-output` path if one is set) |
| `:q` | Quit; refuses if there are unsaved changes |
| `:q!` | Quit and discard changes |
| `:wq` | Save and quit |
| `:e` / `:we` | Legacy aliases for `:q` / `:wq`. `:e` does **not** open a file. |
| `:help [page]` / `:h [page]` | Open `general` (default), `keys` or `cmds` read-only in place of the buffer. Refuses if there are unsaved changes; `:help!` discards them. `Ctrl-P` takes you back to your file. |
| `:[range]s/…/…/[gci]` | Substitute (see above) |
| `:set …` | Set options (see [Options](#options)) |
| `:set-var name expr` | Set an option to an integer expression |
| `:set-output "path"` | Change where the buffer is saved (the quotes are required) |
| `:set-map key "keys"` | Map a Normal-mode key to a key sequence |
| `:imap lhs rhs…` | Map a key sequence typed in Insert mode |
| `:let name [=] expr` | Define an integer variable |
| `:echo name` / `:echo "text"` | Show a variable or a literal |
| `:nohl` / `:nohlsearch` | Hide the search highlight |
| `:autoformat` / `:Autoformat` | Format the buffer |
| `:!command` | Run `sh -c command` in the background (see below) |

Expressions use integers with `+ - * /`, evaluated strictly left to right
with no precedence.

**`:!command`** runs in the background, so the editor stays usable. While it
runs, the message line shows `Running: <command> (Ns)`. When it ends you see
the last non-blank line of its output, or of its standard error if standard
output was silent. A failure is prefixed with its status, as in
`[exit 2] error: foo`. Only one command runs at a time.

**`set-map`** works in Normal mode. The key is one character or a special name.
The right-hand side can include a literal newline to submit a `:` command:

```lua
cano.command([[set-map <c-f> ":autoformat
"]])
```

If you map the same key twice, the *first* mapping is kept. Expansion is capped
(64 levels deep, 10,000 keys) so a self-referencing map cannot hang the editor.

**`imap`** maps a run of Insert-mode keys, for example `:imap jk <Esc>`.
`<…>` names become their byte and everything else is literal. Quote a
right-hand side that contains spaces.

- A mapping fires as soon as its last key is typed. As a result, a longer
  mapping that starts with a shorter one can never fire.
- Moving the cursor breaks a partly typed run.
- Re-mapping the same left-hand side replaces the old mapping.

**Special key names** for `set-map` and `imap`:

- `<space> <esc>/<escape> <tab> <nul>`
- `<cr>/<enter>/<return> <bs>/<backspace>`
- `<up> <down> <left> <right> <home> <end>`
- `<delete>/<del> <insert>/<ins>`
- `<pageup>/<page-up> <pagedown>/<page-down>`
- `<c-a>`…`<c-z>` (also spelled `<ctrl-a>`…), `<c-?>`

## Options

`:set` works the way it does in Vim:

| Form | Effect |
|------|--------|
| `:set name` / `:set noname` | Turn on / off |
| `:set name!` | Toggle |
| `:set name=N` | Assign an integer (`listchars` takes a spec) |
| `:set name?` | Show the current value |

You can set several at once, as in `:set cursorline rnu sw=2`. A backslash
escapes whitespace inside a value. `:set-var name expr` does the same with an
expression.

| Option | Aliases | Default | Meaning |
|--------|---------|---------|---------|
| `syntax` | `syn` | on | Syntax highlighting |
| `relative` | `relativenumber`, `rnu` | off | Relative line numbers (the cursor line shows its absolute number) |
| `indent` | `shiftwidth`, `sw` | 4 | Spaces per indent level; `0` means a tab |
| `tabstop` | `ts` | 4 | Columns a tab is drawn across (1–32); retab uses it too |
| `cursorline` | `cursor-line`, `cul` | off | Underline the cursor line |
| `mouse` | | on | Cano handles the mouse (takes effect immediately) |
| `backup` | `bk` | on | Back up the file on disk before each save |
| `list` | | off | Show invisible characters |
| `listchars` | `lcs` | see below | Glyphs used by `list` |
| `autoformat_autoindent` | | on | `:autoformat` re-indents |
| `autoformat_retab` | | on | `:autoformat` converts leading whitespace to match `indent` |
| `autoformat_remove_trailing_spaces` | | on | `:autoformat` strips trailing whitespace |
| `auto_indent` | `auto-indent`, `autoindent`, `ai` | on | Kept for compatibility; has no effect (`Enter`/`o`/`O` always auto-indent) |
| `undo_size` | `undo-size` | 32 | How many undo steps are kept; `0` or less means unlimited |

Each tab is drawn `tabstop` columns wide wherever it sits; Cano does not align
tabs to tab stops the way Vim does.

## Configuration (`init.lua`)

Cano runs `~/.config/cano/init.lua` if it exists, or the file you pass with
`--config`. The home directory comes from `HOME`, or from `USERPROFILE` when
`HOME` is unset. If you don't pass `--config`, Cano creates `~/.config/cano/`
on startup.

```lua
local cano = setup({
    syntax = true,       -- syntax highlighting
    relative = true,     -- relative line numbers
    cursorline = true,   -- underline the cursor line
    mouse = true,        -- false hands the mouse back to the terminal
    backup = true,       -- copy what each save overwrites into .backups/
    list = false,        -- show invisible characters
    indent = false,      -- false: tabs; true: ONE space (see below)
    auto_indent = false, -- compatibility no-op
})

cano.command([[set sw=2 listchars=tab:»\ ,trail:~,eol:$]])
cano.command("imap jk <Esc>")
-- cano.exit(0, "bye")
```

- `setup(table)` is optional. Its argument must be a table. Calling it again
  merges with earlier calls.
- **Only Lua booleans count.** `true` means 1 and `false` means 0. Any other
  value, such as `indent = 2`, is ignored. That is why `indent = true` gives
  one-space indents. Use `cano.command("set sw=2")` for any other width, and
  `cano.command("set undo_size=100")` for the undo limit (a boolean
  `undo_size` in `setup` would set it to 1 or 0).
- `setup` returns a table with two functions:
  - `cano.command(line)` queues any `:` command line (a leading `:` is
    optional). These run in order, after the `setup` options are applied and
    **before the terminal opens**. A failing line is reported, not applied.
    This is the only way to set non-boolean options and the `autoformat_*`
    switches from Lua.
  - `cano.exit(code[, message])` stops Cano before the editor opens. It prints
    `Exiting as specified in the configuration, message: <message>` and
    exits with `code`.
- The Lua state has the full standard library (`io`, `os`, …), so treat
  `init.lua` as trusted code.
- Use a Lua `[[long string]]` for commands that contain backslashes.

A commented example is in [`examples/init.lua`](examples/init.lua).

## Syntax highlighting

| Language | Chosen by |
|----------|-----------|
| C | `c`, `h` |
| C++ | `cc cpp cxx c++ hh hpp hxx h++ ipp tpp` |
| Rust | `rs` |
| Python | `py pyi pyw` |
| Shell | `sh bash zsh ksh ash dash`; names `.bashrc .bash_profile .bash_aliases .bash_logout .profile .zshrc .zprofile .zshenv .zlogin .zlogout .kshrc bashrc profile` |
| Vim script | `vim vimrc`; names `.vimrc _vimrc vimrc .gvimrc _gvimrc gvimrc .exrc` |
| Lua | `lua` |
| JSON | `json` |

Each language has its own scanner, not just its own keyword list, so tokens
mean what that language says they mean:

- `//` is a comment in C but floor division in Python.
- `'a` is a lifetime in Rust.
- `#` is a directive, an attribute or a comment depending on the language, and
  in shell only at the start of a word.
- `"` starts a comment in Vim script.
- `--` is a Lua comment.
- Rust raw strings and nested block comments, Python prefixed and
  triple-quoted strings, shell `${…}`, Vim `<key>` notation and Lua long
  brackets are all handled.

`:set nosyntax` turns highlighting off.

### Custom palettes (`.cyntax`)

Put `<extension>.cyntax` next to your config, for example
`~/.config/cano/go.cyntax`. It is a list of groups. Each group ends with `.`
and has comma-separated fields, `tag,r,g,b,word,word,…`:

```text
k,170,68,68,
auto,struct,break,else,switch,case,return.
t,255,165,0,
int,char,float,double,size_t.
w,128,160,255,
NULL,EOF.
```

| Tag | Group | Empty word list means |
|-----|-------|-----------------------|
| `k` | Keywords | Use the language's built-in keywords (C's for an unknown extension) |
| `t` | Types | Use the language's built-in types |
| `w` | Extra words | Highlight nothing |

The colour values are integers from 0 to 255, and whitespace around fields is
ignored. A palette changes colours and words but never the scanner. Palettes
are matched by extension only, so a file that is recognised by its name (like
`.vimrc`) cannot get one. A malformed palette is silently ignored and the
built-in colours are used.

## Markdown display

Markdown display is on by default for `.md .markdown .mdown .mkd .mkdn .mdx`
files and shows `[MD]` in the status line. `Ctrl-M` or `Enter` toggles it.

It colours these elements in place:

- headings (ATX and setext), emphasis, strikethrough;
- code spans and fences;
- links, autolinks and bare URLs;
- lists and task boxes, quotes, tables, rules and front matter.

The Markdown punctuation is dimmed rather than hidden. Every character keeps its
column, so editing and motions work exactly as they do with the display off.

## Invisible characters

`:set list` (or `space l`) draws invisible characters. The glyphs come from
`listchars`:

```vim
:set listchars=tab:▸\ ,trail:·,eol:↲,nbsp:⎵,space:·    " the default
:set listchars?                                       " show the current set
```

| Item | Marks |
|------|-------|
| `tab:xy[z]` | A tab: `x` first, `y` as fill (`z` optional) |
| `trail:x` | Trailing whitespace |
| `eol:x` | End of line |
| `nbsp:x` | Non-breaking space |
| `space:x` | Any other space |

Setting `listchars` replaces the whole set, so items you leave out are not
drawn. A backslash escapes the next character, which is how a space can be a
glyph. The glyphs are dimmed, and nothing shifts: a tab keeps its width.

## Autoformat

`:autoformat` (`space f`) works on the whole buffer:

- **`.json` files** are validated and pretty-printed at the `indent` width.
  Key order, duplicate keys, number spellings, string escapes and the final
  newline are all kept. If the JSON is invalid, the buffer is left unchanged
  and the parse error is shown.
- **Any other file** gets the
  [vim-autoformat](https://github.com/vim-autoformat/vim-autoformat) fallback.
  It runs up to three steps, each controlled by its own option:
  1. re-indent from bracket nesting, ignoring brackets in strings
     (`autoformat_autoindent`);
  2. convert leading whitespace to match `indent` (`autoformat_retab`);
  3. strip trailing whitespace (`autoformat_remove_trailing_spaces`).

  The line count never changes. In a Makefile, turn off `autoformat_retab`
  with `:set noautoformat_retab` so the tabs survive.

Visual `=` applies the whitespace tidy to the selected lines, even in JSON.
Either way the rewrite is one undo step. It reports `Formatted N lines` or
`Already formatted`, and it refuses on a read-only buffer.

## File explorer and recent files

**Explorer (`Ctrl-N` / `space n`)**

- Shows the current working directory full-screen, including hidden entries.
  Directories end in `/`, `..` goes up, and symbolic links are not shown.
- `j`/`k` or the arrows move, `Enter` opens, `Esc` closes.

**Recent files (`Ctrl-P` / `space r`)**

- Lists the files you have opened, newest first. It keeps up to 50, skips
  files that no longer exist, and leaves out help pages.
- It is stored as `recent` beside the config file and persists across
  sessions.
- Long paths are shortened from the left.

Both panes share the screen, so opening one closes the other.

If the buffer has unsaved changes, both panes first ask `Save changes? (y/n,
Esc cancels)`:

- `y` saves and then opens the pane. If the save fails, the pane stays shut.
- `n` opens the pane and keeps the changes in memory.
- `Esc` stays in the buffer.

## Backups

Before each save, Cano copies the file **as it is on disk** into a `.backups/`
directory next to it, so you can always get back the version you overwrote:

```
src/.backups/main.rs.20261016-142233      (UTC timestamp)
src/.backups/main.rs.20261016-142233.1    (a second save in the same second)
```

- The 10 newest backups per file are kept.
- A new file has nothing to back up, so the first save creates no backup.
- The backup keeps the original file's permissions.
- If the backup fails, the error is shown but the save still happens.
- `:set nobackup` or `setup({ backup = false })` turns backups off.
- You can browse backups with the explorer.

## Mouse

| Gesture | Action |
|---------|--------|
| Click | Move the cursor (a click in the gutter goes to the line start; a click past the end goes to the line end) |
| Drag | Select in charwise Visual mode |
| Wheel | Scroll 3 lines |
| Scrollbar (rightmost column) | Click or drag to scroll |
| Click in explorer, recent list or history | Select an entry; click the selected entry again to open it |
| `AI` button / panel title bar | Open the assistant / drag to resize it |

While Cano has the mouse, the terminal's own selection usually still works
with Shift held. `:set nomouse` gives the mouse back to the terminal entirely.
Mouse clicks never answer prompts or pick jump labels.

## AI assistant

Cano can dock a chat panel above the status line that both **teaches Cano**
and **does tasks** in the editor.

### Turning it on

To turn it on, set an API key before starting Cano. The `curl` executable must
be on your `PATH`.

```sh
export ANTHROPIC_API_KEY=sk-ant-...     # or OPENAI_API_KEY=sk-...
cano notes.md
```

| Variable | Effect |
|----------|--------|
| `ANTHROPIC_API_KEY` | Use Anthropic's Messages API (preferred when both keys are set) |
| `OPENAI_API_KEY` | Use OpenAI's Chat Completions API |
| `CANO_AI_PROVIDER=openai` | Prefer OpenAI when both keys are set |
| `CANO_AI_MODEL` | Override the model (default `claude-opus-5-5`, or `gpt-5` for OpenAI) |
| `CANO_AI_EFFORT` | Effort level sent to Anthropic (default `medium`) |

Without a key the assistant is off, and `Ctrl-K` explains how to enable it.

### Panel keys

| Key | Action |
|-----|--------|
| `Ctrl-K` / click `AI` | Open or close the panel |
| `Enter` | Send |
| `Esc` | Stop the running request, or return focus to the editor |
| `Ctrl-L`, or send `/clear` | Start a new chat |
| `Up`/`Down`/`PgUp`/`PgDn`, wheel, scrollbar | Scroll the transcript |
| `Left`/`Right`, `Home`/`Ctrl-A`, `End`/`Ctrl-E`, `Ctrl-U`, `Backspace`, `Delete` | Edit the input line |
| Click the panel / drag its top bar | Focus the panel / resize it |

### What it can do

| Tool | Needs your approval |
|------|---------------------|
| `get_editor_state`: file, mode, cursor, line count, modified flag | no |
| `read_buffer` / `edit_buffer`: line-numbered read and replace. Each edit is one undo step, so `u` reverts it. | no |
| `run_editor_command`: any `:` command. It can save, but it never quits. | no |
| `read_file`, `list_directory` | no |
| `run_shell`: runs `sh -c`; exit status and output are returned | **yes** |
| `write_file`: any file except the one open in the editor | **yes** |

When asked for approval, press `y`/`Enter` to allow, `n` to refuse that step,
`a` to always allow that tool for this session, or `Esc` to stop the whole
task.

Safety limits:

- 40 model round trips per request;
- 30,000 characters per tool result;
- 2,000 lines per read;
- a 600-second request timeout.

### Privacy

- The API key reaches `curl` on its standard input, never on its command line.
- The conversation is saved after each completed round to
  `ai-history.json` beside `init.lua`, with mode `0600` on Unix. It is restored
  on the next start.
- If the provider or model has changed since the chat was saved, the
  transcript is shown but the earlier context is not sent.
- `Ctrl-L` clears the chat.
- Everything the assistant reads (buffer text, files, command output) is sent
  to the chosen provider.

## Files Cano reads and writes

| Path | Purpose |
|------|---------|
| `~/.config/cano/init.lua` | Configuration (optional) |
| `~/.config/cano/<ext>.cyntax` | Custom syntax palettes |
| `~/.config/cano/recent` | Recent-files list |
| `~/.config/cano/ai-history.json` | AI chat history (only when a key is set) |
| `<dir>/.backups/<file>.<stamp>` | Save backups |
| `$PREFIX/share/cano/help/{general,keys,cmds}` | Help pages (optional; also built in) |

When you use `--config`, the files in `~/.config/cano` above are read and
written in the directory of that config file instead.

## Behaviour notes and limitations

- **Files are written exactly as they are in the buffer.** Cano does not
  convert line endings or add a final newline. CRLF files stay CRLF, and the
  `\r` shows as `�`.
- **Saves are atomic.** Cano writes a temporary file beside the target, then
  renames it into place. The file keeps its permissions (and, on Unix, its
  owner when allowed), a symlink stays a symlink, and a read-only file is
  refused. FIFOs, devices and hard-linked files are written in place instead,
  so they keep their identity.
- **Display.** Valid UTF-8 is drawn as characters at their display width.
  Bytes that aren't valid UTF-8, and control characters, show as `�`. Each tab
  is `tabstop` columns wide. Blockwise Visual selections still count columns
  in characters, not cells, so a block next to wide text can look ragged.
- **There is one window and one buffer.** Opening a file from a pane, or
  `:help`, replaces the current buffer. `:e file` and `:w file` are not
  supported.
- **There is one unnamed clipboard.** Cano has no registers and does not use
  the system clipboard.
- **The scroll keys are Normal-mode only.** In Visual mode `Ctrl-E` toggles
  comments instead.
- **`Ctrl-Z` only works in Normal mode.** In Insert mode it does nothing.
- **Quitting while a `:!` command runs** leaves that command running on its
  own.
- **Vim keys Cano does not have:** `f` `F` `;` `,` (character search; `s`/`t`
  are jump labels), `P`, `J`, `D`, `C`, `S`, `Y`, `~`, `^`, `H` `M` `L`,
  `{` `}`, `#`, `>>`/`<<` in Normal mode (use Visual `>`/`<`), macros (`q`,
  `@`), marks (`m`, `` ` ``), named registers, regular expressions, and
  `:e`/`:w` with a file name. `set-map` can bind most of these to sequences
  Cano does have.

[`MIGRATION_NOTES.md`](MIGRATION_NOTES.md) lists further compatibility
choices carried over from the C editor.

## Development

```sh
make             # release build -> build/cano
make debug       # debug build   -> build/cano-debug
make test        # cargo test --locked --all-targets
make smoke       # release build + PTY smoke test (needs Python 3)

cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
```

Test suites:

- **Unit tests** in `src/` (about 440), covering the buffer, history, editor,
  app, commands, syntax, display, rendering, saving, the assistant and more.
- **`tests/lifecycle.rs`**: startup errors, exact flag parsing, exit codes,
  and running config without a terminal.
- **`tests/help.rs`**: `:help` and its unsaved-changes guard.
- **`tests/pty_smoke.py`**: drives the real binary in a pseudo-terminal
  (resize, save/quit guards, `--help=`/`:help`, comment toggle, number
  increment, block delete, Insert-mode keys, background `:!`, and
  `SIGTERM`/`SIGHUP` terminal restore).

Pushing to any branch makes GitHub Actions build release archives for all
five targets (`.github/workflows/release.yml`). Pushing a `v*` tag also
publishes them as a GitHub release.

Debug builds check the design-by-contract invariants with `debug_assert!`. See
[`docs/dbc-ponytail-report.md`](docs/dbc-ponytail-report.md).

The release profile is built for size: `opt-level = "z"`, LTO, one codegen
unit, stripped, and `panic = "abort"`. A panic hook restores the terminal
first.

## Project layout

| Module | Responsibility |
|--------|----------------|
| `main.rs` | Startup order, event loop, applying effects (save, quit, shell, suspend) |
| `cli.rs` | Argument parsing |
| `config.rs` | Lua runtime, `setup`, `cano.command`, `cano.exit` |
| `app.rs` | Top-level input routing, panes, prompts, mouse, repeat, mappings |
| `editor.rs` | Modal editing state, operators, Insert/Visual behaviour |
| `buffer.rs` | Byte buffer, row index, cursor, motions |
| `history.rs` | Undo/redo records |
| `command.rs` | `:` command language, options, variables, mappings |
| `substitute.rs` | `:s` parsing and execution |
| `textobject.rs` | Text-object ranges |
| `jump.rs` | EasyMotion labels |
| `comment.rs` | Line-comment toggling |
| `autoformat.rs` | JSON and whitespace formatting |
| `syntax.rs` | Per-language scanners and `.cyntax` palettes |
| `markdown.rs` | Markdown display spans |
| `listchars.rs` | `listchars` parsing |
| `display.rs` | UTF-8 decoding, character widths, and byte-to-cell layout |
| `render.rs` | Ratatui drawing: gutter, text, scrollbar, status, panes, AI panel |
| `terminal.rs` | Crossterm session, input decoding, suspend, panic hook, SIGTERM/SIGHUP |
| `io.rs` | File load, atomic save, help lookup, embedded help |
| `process.rs` | `sh -c` execution, background jobs, output summary |
| `explorer.rs` / `recent.rs` / `backup.rs` | Explorer listing, recent list, backups |
| `assistant.rs` | AI panel, provider backends, tool loop, persistence |

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
