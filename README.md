# Cano

Cano (kay-no) is a small modal text editor for the terminal, in the style of
Vim. It is written in Rust with [Ratatui](https://ratatui.rs) and
[Crossterm](https://github.com/crossterm-rs/crossterm). You configure it in
Lua 5.4. It has an optional built-in AI assistant.

The original editor is written in C and lives at
[Cano-Projects/Cano](https://github.com/Cano-Projects/Cano). This Rust version
was written from migration specifications of that editor. It does not reuse
the C source or an earlier Rust port. For the compatibility choices, see
[`MIGRATION_NOTES.md`](MIGRATION_NOTES.md). For the product requirements, see
[`PRD.md`](PRD.md).

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

- Five modes: Normal, Insert, Visual, Search and Command. Visual mode selects
  charwise, linewise or blockwise.
- The `d`/`y`/`c` operators, text objects (`iw aw ip ap i( a" …`), `.` repeat,
  Vim-style counts (`3dd`, `d3w`, `3.`), and EasyMotion-style `s`/`t` jumps.
- UTF-8 display and motions. Wide CJK and emoji characters take two cells, and
  `h`/`l`/`x`/`r` act on whole characters.
- Vim-style `:s` with ranges and the `g`/`c`/`i` flags, `*` word search with
  match highlighting, and `:set` with Vim option names.
- Built-in syntax highlighting for C, C++, Rust, Python, shell, Vim script,
  Lua and JSON, plus custom `.cyntax` palettes.
- Markdown display in place, `:set list`, `:autoformat` (JSON pretty-print or
  whitespace tidy) and line-comment toggle.
- A file explorer, a recent-files list that persists across sessions, prompt
  history for each session, and timestamped backups on each save.
- Mouse support: click, drag to select, wheel, and a scrollbar you can click.
- An optional AI assistant panel (Anthropic or OpenAI). It explains Cano,
  edits the buffer in steps you can undo, and runs commands with your
  approval.
- Atomic saves that keep permissions and symlinks. `:!` commands run in the
  background.
- A release binary of about 1 MB. Each GitHub release has prebuilt archives
  for Linux, macOS and Windows.

## Quick start

```sh
cargo build --release --locked
./target/release/cano path/to/file
```

If the file does not exist, Cano opens an empty buffer and creates the file
when you first save. If you give no file, the buffer is named `out.txt`.

1. Press `i` to type text. Press `Esc` to stop.
2. Type `:w` to save.
3. Type `:q` to quit. Type `:q!` to quit and discard changes.

If an API key is set, press `Ctrl-K` to open the AI assistant.

## Installation

### Prebuilt binaries

Each [GitHub release](https://github.com/Phorena-Inc/cano-editor-rust/releases)
has archives for these targets:

- Linux: x86-64 and ARM64
- macOS: Intel and Apple Silicon
- Windows: x86-64

Unpack an archive and put `bin/cano` on your `PATH`. The archive also contains
`docs/help`. You do not need it, because the help pages are also built into
the binary.

### From source

On every platform you need:

- a current stable Rust toolchain from [rustup](https://rustup.rs). The code
  uses let-chains, so use a recent stable release.
- a **C compiler**. The `mlua` crate compiles the vendored Lua 5.4 during the
  build.

These runtime tools are optional:

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

`make install` builds the binary with `CANO_HELP_DIR=$(PREFIX)/share/cano/help`
built in. Give the same `PREFIX` to `make` and `make install`, so that the
binary finds the help pages. `make uninstall` removes both.

On Nix (x86_64-linux), run `nix build .#cano`. `nix develop` opens a shell
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

Copy `cano.exe` to a directory on your `PATH`.

**Note:** Windows has no `sh`. `:!command` works only if an `sh` is on your
`PATH`. `Ctrl-Z` has no shell to suspend into.

Prebuilt packages such as the AUR package are for the original C editor, not
for this version.

## Command-line usage

```
cano [--config <init.lua>] [file]
cano -h | --help
cano -v | --version
```

| Argument | Effect |
|----------|--------|
| `file` | The file to edit. The default is `out.txt`. Cano ignores extra positional arguments. |
| `--config <path>` / `--config=<path>` | Use this Lua config instead of `~/.config/cano/init.lua`. The file must exist. If you give it twice, the last one wins. |
| `-h`, `--help` | Open the general help page, read-only, in the editor. |
| `--help=<page>` | Open the `general`, `keys` or `cmds` page. For an unknown page, Cano lists the pages and exits with status 1. |
| `-v`, `--version` | Print `cano <version>` and exit. This flag takes precedence over `--help`. |

Flags must match exactly. `--helpful`, `--configuration` and any other
argument that starts with `-` are errors.

**Exit status:**

| Status | When |
|--------|------|
| `0` | Normal quit, `--version`, or a startup command that quits. |
| `1` | An argument, configuration, file-open or terminal error. |
| `128 + signal` | After `SIGTERM` (143) or `SIGHUP` (129). Cano restores the terminal first. It does not save the buffer. |
| The `cano.exit()` code | When `init.lua` calls `cano.exit()`. A negative code keeps its low byte, so `-1` exits as 255. A code above 255 becomes 255. |

Cano looks for the help pages in this order:

1. `$CANO_HELP_DIR`;
2. the `CANO_HELP_DIR` set at build time;
3. `docs/help` or `share/cano/help` relative to the binary;
4. `./docs/help`;
5. the copies built into the binary.

All three pages are built in, so help always works. In the editor,
`:help [page]` opens the same pages.

## Modes

| Mode | Use | Enter with | Status line shows |
|------|-----|------------|-------------------|
| Normal | Motions, operators, commands | `Esc` / `Ctrl-C` from any mode | `NORMAL` |
| Insert | Type text | `i I a A o O`, `c…` | `INSERT` |
| Visual | Select text (charwise, linewise, blockwise) | `v`, `V`, `Ctrl-V` | `VISUAL` |
| Search | Type a `/` search | `/` | `SEARCH` |
| Command | Type a `:` command | `:` | `COMMAND` |

The status line reads `MODE file [MD] (col,row) Saved|Modified`. When the
assistant is available, an `AI` button follows at the right end. The column is
the display column, so a tab or a wide character counts as the cells it takes.

Messages on the bottom line (the message line) clear after about one second.

## Key reference

### All modes

| Key | Action |
|-----|--------|
| `Esc` / `Ctrl-C` | Go back to Normal mode. Cancel a pending count, operator or prompt. |
| `Ctrl-Q` | Quit. Cano refuses while there are unsaved changes. |
| `Ctrl-S` | Save, then quit (Normal and Insert mode). |
| `Ctrl-K` | Open or close the AI assistant panel. |

### Normal mode: movement

| Key | Action |
|-----|--------|
| `h j k l`, arrows | Left, down, up, right |
| `0` / `$` | Start / end of line |
| `w` / `b` / `e` | Next word / previous word / end of word |
| `g` / `G` | First line / last line |
| `N g`, `N G` | Go to line N |
| `%` | Matching bracket. Brackets inside quotes do not count. |
| `s{char}` | EasyMotion: label every visible `{char}` above and below the cursor. Type a label to jump to it. |
| `t{char}` | EasyMotion, forward only. The cursor stops one character before the match. |
| `Ctrl-F` / `Ctrl-B` | Forward / back one screen, with 2 lines of overlap |
| `Ctrl-D` / `Ctrl-U` | Forward / back half a screen |
| `Ctrl-E` / `Ctrl-Y` | Scroll one line down / up. The cursor moves only if it would leave the view. |
| `N` + motion | Do the motion N times. A count also multiplies the scroll keys. |

EasyMotion labels come from `asdghklqwertyuiopzxcvbnmfj;`. The nearest match
gets the first label. When there are more matches than keys, labels become two
keys long. If there is only one match, Cano jumps at once. `Esc`, or a key
that matches no label, cancels. Only rows on the screen are targets.

### Normal mode: editing

| Key | Action |
|-----|--------|
| `i` / `I` | Insert before the cursor / at the first non-blank |
| `a` / `A` | Insert after the cursor / at the end of the line |
| `o` / `O` | Open an auto-indented line below / above |
| `Ctrl-O` | Open a line below and stay in Normal mode |
| `x` | Delete the character (`3x` deletes three) |
| `r{char}` | Replace the character (`3rX` replaces three). `Enter` writes a line break. `Esc` cancels. |
| `dd` / `yy` / `cc` | Delete / yank / change the line (`3dd` acts on three lines) |
| `d`/`y`/`c` + motion or text object | See [Operators](#operators-text-objects-and-repeat) |
| `p` | Paste. Linewise clipboard text goes below the current line. `3p` pastes three times. |
| `u` | Undo |
| `U` / `Ctrl-R` | Redo |
| `.` | Repeat the last change (`3.` repeats it with a count of 3) |
| `Ctrl-A` / `Ctrl-X` | Add / subtract 1 (or the count) to the number at or after the cursor |

`Ctrl-A` and `Ctrl-X` read decimal numbers and `0x` hexadecimal numbers:

- A leading `-` is part of the number.
- Zero padding stays: `007` becomes `008`.
- Hex numbers keep their width and letter case.
- The cursor stops on the last digit.

### Normal mode: panes, search and other keys

| Key | Action |
|-----|--------|
| `/` · `n` · `N` | Search · next match · previous match (both wrap) |
| `*` | Search for the word under the cursor as a whole word, and highlight all matches (`3*` goes to the third) |
| `:` | Open the command prompt |
| `:help` | Open a help page (see [Commands](#commands)) |
| `v` / `V` / `Ctrl-V` | Charwise / linewise / blockwise Visual mode |
| `Ctrl-N` | Open or close the file explorer |
| `Ctrl-P` | Open or close the recent-files list |
| `Ctrl-M` / `Enter` | Turn Markdown display on or off. Most terminals send the same byte for both keys. |
| `Ctrl-G` | Show the file name, line count and position |
| `Ctrl-L` | Redraw the screen and clear the message |
| `Ctrl-W` | Vim's window prefix. Cano has no splits, so it ignores the next key and tells you so. |
| `Ctrl-Z` | Suspend to the shell. Type `fg` to resume (Unix only). |

### Leader key (space)

| Keys | Same as |
|------|---------|
| `space i` | `*` |
| `space o` | `:nohl` |
| `space n` | `Ctrl-N` (explorer) |
| `space r` | `Ctrl-P` (recent files) |
| `space l` | `:set list!` |
| `space f` | `:autoformat` |
| `space e` (Visual) | `Ctrl-E` (comment toggle) |

### Insert mode

| Key | Action |
|-----|--------|
| `Esc` / `Ctrl-C` | Go back to Normal mode |
| `Backspace` / `Ctrl-H` | Delete the character before the cursor |
| `Ctrl-W` | Delete the word before the cursor |
| `Ctrl-U` | Delete back to the first non-blank. Press again to delete the indent. |
| `Ctrl-T` / `Ctrl-D` | Indent / unindent the line by one `indent` width |
| `Tab` | Insert `indent` spaces, or a tab when `indent` is 0 |
| `Enter` | Start a new line, indented to the bracket depth |
| `Ctrl-N` / `Ctrl-P` | Complete the word before the cursor from words in the buffer, nearest first |
| `Ctrl-O` | Run one Normal-mode command, then go back to Insert mode |
| Arrows | Move the cursor. This ends the current undo step. |

Brackets:

- When you type `(`, `[` or `{`, Cano also inserts the closing bracket.
- When you type a closing bracket in front of the same bracket, the cursor
  steps over it.
- When you press `Enter` between a bracket pair, Cano opens an indented line
  between them.

Completion moves through the list each time you press the key. If you move
past either end, your original text comes back. The status line shows the
position, for example `match 2 of 5`.

### Command and Search prompts

| Key | Action |
|-----|--------|
| `Enter` | Run the line |
| `Esc` / `Ctrl-C` | Discard the line |
| `Left` / `Right`, `Backspace` | Edit the line |
| `Up` / `Down` | Move through this prompt's history |
| `Ctrl-F` | Show the history as a list. `j`/`k` move, `Enter` runs, `Esc` closes. |

Each prompt keeps its last 50 different lines for the current session.

## Operators, text objects and repeat

The operators `d` (delete), `y` (yank) and `c` (change) take one of these
targets:

- a motion: `0 $ w b e g G`;
- the same key again, for the whole line: `dd`, `yy`, `cc`;
- a text object.

Any other key cancels the operator.

`c` deletes the target and then starts Insert mode. The deletion and all the
text you type are **one undo step**. As in Vim, `cw` on a word changes only to
the end of the word.

**Counts** apply to the motion as in Vim:

- `3dw` and `d3w` both delete three words. `2d3w` deletes six.
- `3dd`, `3yy` and `3cc` act on three lines.
- `3d` alone waits for its motion.
- `d2aw` deletes two words and their blanks.
- Cano drops a count that comes before `s`, `t`, `:`, `/` or space.

| Text object (`i` inner / `a` around) | Selects |
|--------------------------------------|---------|
| `w` | The run of word characters, punctuation or blanks under the cursor |
| `p` | The paragraph: the run of blank or non-blank lines |
| `(` `)` `b` | The innermost parentheses |
| `[` `]` | The innermost square brackets |
| `{` `}` `B` | The innermost braces |
| `<` `>` | The innermost angle brackets |
| `"` `'` `` ` `` | The quoted string under the cursor, or the next one on the line |

- The `a` form includes the delimiters.
- `aw` and `a"` also take the blanks after the object. At the end of a line,
  they take the blanks before it.
- Bracket matching ignores brackets inside quotes.
- If the text object does not exist, nothing happens. An example is `di(`
  outside any parentheses.

### The `.` repeat

`.` repeats the last change. A change is one of these:

- `x`, `p` or `r`;
- an operator and its target;
- `Ctrl-A` / `Ctrl-X`;
- an Insert session and all the text typed before `Esc`.

`.` uses the count of the original change, so `3x` then `.` deletes three more
characters. A count before `.` replaces the stored count. The new count stays
for the next `.`.

These do not replace the stored change:

- motions, searches, undo and panes;
- a command that changed nothing;
- a command interrupted by a `:` or `/` prompt;
- Visual-mode edits, which are not recorded.

Cano replays the keys as you typed them, so a mapped key repeats its mapping.

## Visual mode

| Key | Action |
|-----|--------|
| `h j k l 0 $ w b e g G %`, arrows | Extend the selection. Counts work: `3j`. |
| `s{char}` | Extend the selection to an EasyMotion label |
| `d` / `x` | Delete the selection, including the character under the cursor |
| `y` | Yank the selection |
| `c` | Delete the selection and type the new text, as one undo step. In linewise mode, one empty line stays. |
| `p` | Put the clipboard text over the selection. The replaced text goes into the clipboard. |
| `o` | Move the cursor to the other end of the selection |
| `r{char}` | Replace every selected character. Line breaks stay. |
| `v` / `V` | Change to charwise / linewise. Press the key of the current kind to leave Visual mode. |
| `>` / `<` | Indent / unindent the selected lines |
| `=` | Format the selected lines (whitespace tidy; see [Autoformat](#autoformat)) |
| `Ctrl-E` / `space e` | Toggle line comments |
| `Ctrl-V` | Change to blockwise. If the selection is already blockwise, leave Visual mode. |

**Blockwise mode** (`Ctrl-V`) selects a rectangle:

- `d`/`x` cut the rectangle in one undo step. `y` copies it.
- Lines too short to reach the block count as blank.
- The clipboard does not store the shape, so `p` pastes the rows back as
  lines.
- `>`, `<` and `=` act on every row that the block touches.

**Comment toggle** uses the comment marker for the file type:

| Marker | File types |
|--------|------------|
| `//` | C, C++, Rust |
| `#` | Python, shell |
| `--` | Lua |
| `"` | Vim script |

- If all non-blank selected lines have comments, Cano removes them. If not,
  Cano adds comments to all of them, at the column of the smallest indent.
- Blank lines are skipped.
- The whole toggle is one undo step.
- For an unknown file type, JSON, or a read-only buffer, Cano shows an error.

## Search, highlight and replace

### Search

`/text` searches forward from the cursor for the literal text. The search is
case-sensitive and wraps at the end of the file. Matches stay highlighted.

- `n`/`N` repeat the search forward and back. They accept a count.
- `*` searches for the word under the cursor as a whole word.
- `:nohl` (or `:nohlsearch`) hides the highlight. `n` still uses the pattern.

### Legacy replace: `/s/old/new`

This shortcut comes from the C editor. It replaces the *next* `old` after the
cursor and moves the cursor there. It is one undo step. It is refused on a
read-only buffer. It takes no flags; use `:s` for flags.

### Substitute: `:s`

```
:[range]s/pattern/replacement/[flags]
```

| Part | Meaning |
|------|---------|
| range | `%` the whole file, `N` line N, `.` the current line, `$` the last line, `a,b` lines a to b. If you omit it, Cano uses the current line. |
| `s` | Any prefix of `substitute` also works. |
| delimiter | Any punctuation except `\`. For example, `:%s#http://#https://#g` works. |
| pattern | Literal text, not a regular expression. `\<` and `\>` mark word boundaries. `\` escapes the delimiter and itself. |
| flags | `g` replaces every match on a line. `c` asks before each match: `y` yes, `n` no, `a` all the rest, `q`/`Esc` stop. `i` ignores case. |

One `:s` command is one undo step, with or without `c`. It is refused on a
read-only buffer.

## Commands

To run a command, type `:`, then the command, then press `Enter`.

| Command | Action |
|---------|--------|
| `:w` | Save. If you set a path with `set-output`, Cano saves there. |
| `:q` | Quit. Cano refuses if there are unsaved changes. |
| `:q!` | Quit and discard changes |
| `:wq` | Save and quit |
| `:e` / `:we` | Legacy aliases for `:q` / `:wq`. `:e` does **not** open a file. |
| `:help [page]` / `:h [page]` | Open `general` (the default), `keys` or `cmds` read-only in place of the buffer. Cano refuses if there are unsaved changes; `:help!` discards them. `Ctrl-P` takes you back to your file. |
| `:[range]s/…/…/[gci]` | Substitute (see above) |
| `:set …` | Set options (see [Options](#options)) |
| `:set-var name expr` | Set an option to an integer expression |
| `:set-output "path"` | Change the path that `:w` saves to. The quotes are required. |
| `:set-map key "keys"` | Map a Normal-mode key to a key sequence |
| `:imap lhs rhs…` | Map a key sequence typed in Insert mode |
| `:let name [=] expr` | Define an integer variable |
| `:echo name` / `:echo "text"` | Show a variable or a literal |
| `:nohl` / `:nohlsearch` | Hide the search highlight |
| `:autoformat` / `:Autoformat` | Format the buffer |
| `:!command` | Run `sh -c command` in the background (see below) |

Expressions use integers and `+ - * /`. Cano evaluates them strictly from left
to right. There is no operator precedence.

### `:!command`

The command runs in the background, so you can keep editing.

- While it runs, the message line shows `Running: <command> (Ns)`.
- When it ends, the message line shows the last non-blank line of its
  standard output. If standard output is empty, it shows the last non-blank
  line of standard error.
- On failure, the status comes first, for example `[exit 2] error: foo`.
- Only one command can run at a time.

### `set-map`

`set-map` works in Normal mode. The key is one character or a special key
name. To submit a `:` command, put a literal line break in the right-hand
side:

```lua
cano.command([[set-map <c-f> ":autoformat
"]])
```

- If you map the same key twice, Cano keeps the *first* mapping.
- Expansion has limits: 64 levels deep and 10,000 keys. A map that refers to
  itself cannot hang the editor.

### `imap`

`imap` maps a sequence of keys typed in Insert mode, for example
`:imap jk <Esc>`. A `<…>` name becomes its byte. All other text is literal. If
the right-hand side contains spaces, put it in quotes.

- A mapping runs as soon as you type its last key. So if a longer mapping
  starts with a shorter one, the longer one can never run.
- If you move the cursor, a partly typed sequence is reset.
- If you map the same left-hand side again, the new mapping replaces the old
  one.

### Special key names

These names work in `set-map` and `imap`:

- `<space> <esc>/<escape> <tab> <nul>`
- `<cr>/<enter>/<return> <bs>/<backspace>`
- `<up> <down> <left> <right> <home> <end>`
- `<delete>/<del> <insert>/<ins>`
- `<pageup>/<page-up> <pagedown>/<page-down>`
- `<c-a>`…`<c-z>` (also `<ctrl-a>`…), `<c-?>`

## Options

`:set` works as in Vim:

| Form | Effect |
|------|--------|
| `:set name` / `:set noname` | Turn the option on / off |
| `:set name!` | Toggle the option |
| `:set name=N` | Set an integer (`listchars` takes a spec) |
| `:set name?` | Show the current value |

You can set several options in one line: `:set cursorline rnu sw=2`. A
backslash escapes a space inside a value. `:set-var name expr` sets an option
from an expression.

| Option | Aliases | Default | Meaning |
|--------|---------|---------|---------|
| `syntax` | `syn` | on | Syntax highlighting |
| `relative` | `relativenumber`, `rnu` | off | Relative line numbers. The cursor line shows its absolute number. |
| `indent` | `shiftwidth`, `sw` | 4 | Spaces per indent level. `0` means a tab. |
| `tabstop` | `ts` | 4 | Width of a tab in columns (1–32). Retab also uses it. |
| `cursorline` | `cursor-line`, `cul` | off | Underline the cursor line |
| `mouse` | | on | Cano handles the mouse. The change takes effect at once. |
| `backup` | `bk` | on | Back up the file on disk before each save |
| `list` | | off | Show invisible characters |
| `wrap` | | on | Continue long lines on the next screen row. `:set nowrap` scrolls sideways instead. |
| `listchars` | `lcs` | see below | Glyphs for `list` |
| `autoformat_autoindent` | | on | `:autoformat` re-indents |
| `autoformat_retab` | | on | `:autoformat` converts leading whitespace to match `indent` |
| `autoformat_remove_trailing_spaces` | | on | `:autoformat` removes trailing whitespace |
| `auto_indent` | `auto-indent`, `autoindent`, `ai` | on | Kept for compatibility. It has no effect: `Enter`, `o` and `O` always auto-indent. |
| `undo_size` | `undo-size` | 32 | Number of undo steps kept. `0` or less means no limit. |

**Note:** Each tab is `tabstop` columns wide wherever it is. Cano does not
align tabs to tab stops as Vim does.

## Configuration (`init.lua`)

Cano runs `~/.config/cano/init.lua` if it exists. With `--config`, it runs the
file you give instead. The home directory comes from `HOME`. If `HOME` is not
set, Cano uses `USERPROFILE`. Without `--config`, Cano creates
`~/.config/cano/` at startup.

```lua
local cano = setup({
    syntax = true,       -- syntax highlighting
    relative = true,     -- relative line numbers
    cursorline = true,   -- underline the cursor line
    mouse = true,        -- false gives the mouse back to the terminal
    backup = true,       -- copy what each save overwrites into .backups/
    list = false,        -- show invisible characters
    wrap = true,         -- wrap long lines at the screen edge
    indent = false,      -- false: tabs; true: ONE space (see below)
    auto_indent = false, -- compatibility no-op
})

cano.command([[set sw=2 listchars=tab:»\ ,trail:~,eol:$]])
cano.command("imap jk <Esc>")
-- cano.exit(0, "bye")
```

### `setup(table)`

- `setup` is optional. Its argument must be a table. A second call merges with
  the earlier ones.
- **Only Lua booleans count.** `true` means 1 and `false` means 0. Cano
  ignores any other value, such as `indent = 2`. This is why `indent = true`
  gives an indent of one space.
- For other widths, use `cano.command("set sw=2")`. For the undo limit, use
  `cano.command("set undo_size=100")`. A boolean `undo_size` in `setup` sets
  the limit to 1 or 0.

### `cano.command` and `cano.exit`

`setup` returns a table with two functions:

- `cano.command(line)` queues a `:` command line. The leading `:` is optional.
  - The lines run in order, after Cano applies the `setup` options and
    **before the terminal opens**.
  - If a line fails, Cano reports the error and does not apply that line.
  - This is the only way to set non-boolean options and the `autoformat_*`
    options from Lua.
  - For commands that contain backslashes, use a Lua `[[long string]]`.
- `cano.exit(code[, message])` stops Cano before the editor opens. It prints
  `Exiting as specified in the configuration, message: <message>` and exits
  with `code`.

**Warning:** The Lua state has the full standard library (`io`, `os`, …).
Treat `init.lua` as trusted code.

For a commented example, see [`examples/init.lua`](examples/init.lua).

## Syntax highlighting

| Language | Selected by |
|----------|-------------|
| C | `c`, `h` |
| C++ | `cc cpp cxx c++ hh hpp hxx h++ ipp tpp` |
| Rust | `rs` |
| Python | `py pyi pyw` |
| Shell | Extensions `sh bash zsh ksh ash dash`; file names `.bashrc .bash_profile .bash_aliases .bash_logout .profile .zshrc .zprofile .zshenv .zlogin .zlogout .kshrc bashrc profile` |
| Vim script | Extensions `vim vimrc`; file names `.vimrc _vimrc vimrc .gvimrc _gvimrc gvimrc .exrc` |
| Lua | `lua` |
| JSON | `json` |

Each language has its own scanner, not only its own keyword list. So each
token has the meaning that its language gives it:

- `//` is a comment in C, but floor division in Python.
- `'a` is a lifetime in Rust.
- `#` is a directive, an attribute or a comment, depending on the language. In
  shell, it starts a comment only at the start of a word.
- `"` starts a comment in Vim script.
- `--` starts a comment in Lua.
- The scanners also handle Rust raw strings and nested block comments, Python
  prefixed and triple-quoted strings, shell `${…}`, Vim `<key>` notation and
  Lua long brackets.

`:set nosyntax` turns highlighting off.

### Custom palettes (`.cyntax`)

Put `<extension>.cyntax` in the same directory as your config, for example
`~/.config/cano/go.cyntax`. The file is a list of groups. Each group ends with
`.` and has comma-separated fields: `tag,r,g,b,word,word,…`.

```text
k,170,68,68,
auto,struct,break,else,switch,case,return.
t,255,165,0,
int,char,float,double,size_t.
w,128,160,255,
NULL,EOF.
```

| Tag | Group | An empty word list means |
|-----|-------|--------------------------|
| `k` | Keywords | Use the language's built-in keywords (C keywords for an unknown extension) |
| `t` | Types | Use the language's built-in types |
| `w` | Extra words | Highlight nothing |

- Colour values are integers from 0 to 255. Cano ignores whitespace around
  fields.
- A palette changes colours and words. It never changes the scanner.
- Cano finds palettes by extension only. A file recognised by its name (such
  as `.vimrc`) cannot use a palette.
- Cano ignores a malformed palette without a message and uses the built-in
  colours.

## Markdown display

Markdown display is on by default for `.md .markdown .mdown .mkd .mkdn .mdx`
files. When it is on, the status line shows `[MD]`. `Ctrl-M` or `Enter`
turns it on or off.

It colours these elements in place:

- headings (ATX and setext), emphasis and strikethrough;
- code spans and code fences;
- links, autolinks and bare URLs;
- lists and task boxes, block quotes, tables, horizontal rules and front
  matter.

The Markdown punctuation is dimmed, not hidden. Every character stays in its
column, so editing and motions work the same as with the display off.

## Invisible characters

`:set list` (or `space l`) shows invisible characters. The glyphs come from
`listchars`:

```vim
:set listchars=tab:▸\ ,trail:·,eol:↲,nbsp:⎵,space:·    " the default
:set listchars?                                       " show the current set
```

| Item | Marks |
|------|-------|
| `tab:xy[z]` | A tab: `x` first, then `y` as fill (`z` is optional) |
| `trail:x` | Trailing whitespace |
| `eol:x` | End of line |
| `nbsp:x` | Non-breaking space |
| `space:x` | All other spaces |

- A new `listchars` value replaces the whole set. Cano does not show the items
  you leave out.
- A backslash escapes the next character. This is how a space can be a glyph.
- The glyphs are dimmed. Nothing moves: a tab keeps its width.

## Autoformat

`:autoformat` (`space f`) formats the whole buffer.

**`.json` files:** Cano validates the JSON and pretty-prints it at the
`indent` width. It keeps key order, duplicate keys, number spellings, string
escapes and the final newline. If the JSON is not valid, the buffer does not
change and Cano shows the parse error.

**All other files:** Cano uses the
[vim-autoformat](https://github.com/vim-autoformat/vim-autoformat) fallback.
It has up to three steps. An option controls each step:

1. Re-indent from the bracket nesting. Brackets in strings do not count
   (`autoformat_autoindent`).
2. Convert leading whitespace to match `indent` (`autoformat_retab`).
3. Remove trailing whitespace (`autoformat_remove_trailing_spaces`).

The number of lines never changes.

**Note:** In a Makefile, turn off the retab step with
`:set noautoformat_retab`, so that the tabs stay.

In Visual mode, `=` applies the whitespace tidy to the selected lines, also in
JSON files.

In both cases:

- The format is one undo step.
- Cano reports `Formatted N lines` or `Already formatted`.
- Cano refuses on a read-only buffer.

## File explorer and recent files

The file explorer and the recent-files list are the two panes. Each pane
fills the screen, so when you open one, the other closes.

**File explorer (`Ctrl-N` / `space n`)**

- Shows the current working directory, including hidden entries.
- Directory names end in `/`. Select `..` to go up. Symbolic links are not
  shown.
- `j`/`k` or the arrows move, `Enter` opens, `Esc` closes.

**Recent-files list (`Ctrl-P` / `space r`)**

- Shows the files you opened, newest first.
- Keeps up to 50 files. Files that no longer exist are skipped. Help pages
  are not added.
- Cano stores the list in a file named `recent` in the config directory, so
  it stays across sessions.
- Long paths are shortened from the left.

If the buffer has unsaved changes, both panes first ask
`Save changes? (y/n, Esc cancels)`:

| Key | Result |
|-----|--------|
| `y` | Save, then open the pane. If the save fails, the pane does not open. |
| `n` | Open the pane. The changes stay in memory. |
| `Esc` | Stay in the buffer. |

## Backups

Before each save, Cano copies the file **as it is on disk** to a `.backups/`
directory next to it. So you can always get back the version that the save
overwrote.

```
src/.backups/main.rs.20261016-142233      (UTC timestamp)
src/.backups/main.rs.20261016-142233.1    (a second save in the same second)
```

- Cano keeps the 10 newest backups of each file.
- A new file has no earlier version, so its first save makes no backup.
- The backup has the same permissions as the original file.
- If the backup fails, Cano shows the error and still saves the file.
- To turn backups off, use `:set nobackup` or `setup({ backup = false })`.
- You can browse the backups in the file explorer.

## Mouse

| Action | Result |
|--------|--------|
| Click | Move the cursor. A click in the gutter goes to the start of the line. A click past the end goes to the end of the line. |
| Drag | Select text in charwise Visual mode |
| Wheel | Scroll 3 lines |
| Scrollbar (rightmost column) | Click or drag to scroll |
| Click in a pane or the history list | Select an entry. Click the selected entry again to open it. |
| `AI` button / panel title bar | Click to open the assistant / drag to resize the panel |

While Cano has the mouse, you can usually still use the terminal's own
selection if you hold Shift. `:set nomouse` gives the mouse back to the
terminal completely.

Mouse clicks never answer prompts and never select jump labels.

## AI assistant

Cano can show an assistant panel above the status line. The assistant
**explains Cano** and **does tasks** in the editor.

### Turn it on

1. Make sure that `curl` is on your `PATH`.
2. Set an API key.
3. Start Cano.

```sh
export ANTHROPIC_API_KEY=sk-ant-...     # or OPENAI_API_KEY=sk-...
cano notes.md
```

| Variable | Effect |
|----------|--------|
| `ANTHROPIC_API_KEY` | Use Anthropic's Messages API. This is the default when both keys are set. |
| `OPENAI_API_KEY` | Use OpenAI's Chat Completions API |
| `CANO_AI_PROVIDER=openai` | Use OpenAI when both keys are set |
| `CANO_AI_MODEL` | Set the model. The default is `claude-opus-5-5`, or `gpt-5` for OpenAI. |
| `CANO_AI_EFFORT` | Set the effort level sent to Anthropic. The default is `medium`. |

Without a key, the assistant is off. `Ctrl-K` then tells you how to turn it
on.

### Panel keys

| Key | Action |
|-----|--------|
| `Ctrl-K` / click `AI` | Open or close the panel |
| `Enter` | Send the message |
| `Esc` | Stop the running request, or move focus back to the editor |
| `Ctrl-L`, or send `/clear` | Start a new chat |
| `Up`/`Down`/`PgUp`/`PgDn`, wheel, scrollbar | Scroll the chat |
| `Left`/`Right`, `Home`/`Ctrl-A`, `End`/`Ctrl-E`, `Ctrl-U`, `Backspace`, `Delete` | Edit the input line |
| Click the panel / drag its title bar | Move focus to the panel / resize it |

### Tools

The assistant uses these tools:

| Tool | Needs your approval |
|------|---------------------|
| `get_editor_state`: file, mode, cursor, line count, modified flag | no |
| `read_buffer` / `edit_buffer`: read and replace lines by number. Each edit is one undo step, so `u` reverts it. | no |
| `run_editor_command`: any `:` command. It can save, but it can never quit. | no |
| `read_file`, `list_directory` | no |
| `run_shell`: runs `sh -c` and returns the exit status and output | **yes** |
| `write_file`: writes any file except the one open in the editor | **yes** |

When the assistant asks for approval, press one of these keys:

| Key | Result |
|-----|--------|
| `y` / `Enter` | Allow this step |
| `n` | Refuse this step |
| `a` | Always allow this tool for the rest of the session |
| `Esc` | Stop the whole task |

Limits:

- 40 model round trips per request;
- 30,000 characters per tool result;
- 2,000 lines per read;
- a 600-second request timeout.

### Privacy

- Cano gives the API key to `curl` on its standard input, never on its command
  line.
- After each completed round, Cano saves the chat to `ai-history.json` in the
  config directory. On Unix, the file has mode `0600`. Cano loads the chat
  again at the next start.
- If the provider or model changed after the chat was saved, Cano shows the
  old chat but does not send its context to the model.
- `Ctrl-L` clears the chat.

**Caution:** Cano sends everything the assistant reads (buffer text, files,
command output) to the selected provider.

## Files Cano reads and writes

| Path | Use |
|------|-----|
| `~/.config/cano/init.lua` | Configuration (optional) |
| `~/.config/cano/<ext>.cyntax` | Custom syntax palettes |
| `~/.config/cano/recent` | Recent-files list |
| `~/.config/cano/ai-history.json` | AI chat history (only when a key is set) |
| `<dir>/.backups/<file>.<stamp>` | Backups made before each save |
| `$PREFIX/share/cano/help/{general,keys,cmds}` | Help pages (optional; also built in) |

The config directory is the directory of the config file. It is
`~/.config/cano` by default. With `--config`, it is the directory of the file
you give, and Cano reads and writes the files above there.

## Behaviour notes and limitations

- **Cano writes files exactly as they are in the buffer.** It does not convert
  line endings or add a final newline. CRLF files stay CRLF, and each `\r`
  shows as `�`.
- **Saves are atomic.** Cano writes a temporary file next to the target, then
  renames it into place.
  - The file keeps its permissions. On Unix, it also keeps its owner when the
    system allows it.
  - A symlink stays a symlink.
  - Cano refuses to save over a read-only file.
  - Cano writes FIFOs, devices and hard-linked files in place, so they keep
    their identity.
- **Display.** Cano draws valid UTF-8 as characters at their display width.
  Invalid UTF-8 bytes and control characters show as `�`. Each tab is
  `tabstop` columns wide. Blockwise Visual mode counts columns in characters,
  not cells, so a block next to wide characters can look uneven.
- **There is one window and one buffer.** When you open a file from a pane, or
  open `:help`, it replaces the current buffer. Cano does not support
  `:e file` or `:w file`.
- **There is one unnamed clipboard.** Cano has no registers and does not use
  the system clipboard.
- **The scroll keys work in Normal mode only.** In Visual mode, `Ctrl-E`
  toggles comments.
- **`Ctrl-Z` works in Normal mode only.** In Insert mode it does nothing.
- **If you quit while a `:!` command runs,** the command keeps running.
- **Cano does not have these Vim features:**
  - character search `f` `F` `;` `,` (in Cano, `s`/`t` are jump labels);
  - `P`, `J`, `D`, `C`, `S`, `Y`, `~`, `^`, `H` `M` `L`, `{` `}`, `#`;
  - `>>`/`<<` in Normal mode (use Visual `>`/`<`);
  - macros (`q`, `@`) and marks (`m`, `` ` ``);
  - named registers and regular expressions;
  - `:e`/`:w` with a file name.

  You can use `set-map` to bind most of these keys to key sequences that Cano
  has.

[`MIGRATION_NOTES.md`](MIGRATION_NOTES.md) lists more compatibility choices
from the C editor.

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

| Suite | What it tests |
|-------|---------------|
| Unit tests in `src/` (about 440) | The buffer, history, editor, app, commands, syntax, display, rendering, saves, the assistant and more |
| `tests/lifecycle.rs` | Startup errors, exact flag parsing, exit codes, and config runs without a terminal |
| `tests/help.rs` | `:help` and its check for unsaved changes |
| `tests/pty_smoke.py` | The real binary in a pseudo-terminal: resize, save and quit checks, `--help=` and `:help`, comment toggle, number increment, block delete, Insert-mode keys, background `:!`, and terminal restore after `SIGTERM`/`SIGHUP` |

GitHub Actions (`.github/workflows/release.yml`) builds release archives for
all five targets on each push to any branch. When you push a `v*` tag, it
also publishes them as a GitHub release.

Debug builds check the design-by-contract invariants with `debug_assert!`.
For details, see [`docs/dbc-ponytail-report.md`](docs/dbc-ponytail-report.md).

The release profile is built for size: `opt-level = "z"`, LTO, one codegen
unit, stripped symbols, and `panic = "abort"`. A panic hook restores the
terminal before the process stops.

## Project layout

| Module | Responsibility |
|--------|----------------|
| `main.rs` | Startup order, event loop, effects (save, quit, shell, suspend) |
| `cli.rs` | Argument parsing |
| `config.rs` | Lua runtime, `setup`, `cano.command`, `cano.exit` |
| `app.rs` | Input routing, panes, prompts, mouse, repeat, mappings |
| `editor.rs` | Modal editing state, operators, Insert and Visual behaviour |
| `buffer.rs` | Byte buffer, row index, cursor, motions |
| `history.rs` | Undo and redo records |
| `command.rs` | `:` command language, options, variables, mappings |
| `substitute.rs` | `:s` parsing and execution |
| `textobject.rs` | Text-object ranges |
| `jump.rs` | EasyMotion labels |
| `comment.rs` | Line-comment toggle |
| `autoformat.rs` | JSON and whitespace formatting |
| `syntax.rs` | Language scanners and `.cyntax` palettes |
| `markdown.rs` | Markdown display spans |
| `listchars.rs` | `listchars` parsing |
| `display.rs` | UTF-8 decoding, character widths, byte-to-cell layout |
| `render.rs` | Ratatui drawing: gutter, text, scrollbar, status line, panes, AI panel |
| `terminal.rs` | Crossterm session, input decoding, suspend, panic hook, SIGTERM/SIGHUP |
| `io.rs` | File load, atomic save, path canonicalization, help lookup, built-in help |
| `process.rs` | `sh -c` execution, background jobs, output summary |
| `explorer.rs` / `recent.rs` / `backup.rs` | File explorer, recent-files list, backups |
| `assistant.rs` | AI panel, provider backends, tool loop, chat history |

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
