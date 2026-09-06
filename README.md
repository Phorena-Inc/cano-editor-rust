# Cano

Cano (kay-no) is a Vim-inspired modal terminal editor written in Rust using
Ratatui and Crossterm. This implementation was created independently from Code
Atlas migration specifications and does not depend on the original C
implementation or an earlier Rust port. The original C implementation lives at
[Cano-Projects/Cano](https://github.com/Cano-Projects/Cano).

## Demo
[![asciicast](https://asciinema.org/a/655184.svg)](https://asciinema.org/a/655184)

## Quick start

Cano requires a current Rust toolchain with Cargo. Lua 5.4 is built from vendored sources by the `mlua` dependency.

1. Navigate to the Cano directory
```sh
cd path/to/cano
```

2. Build Cano
```sh
cargo build --release --locked
```

3. Run Cano
```sh
./target/release/cano path/to/file
```

Opening a path that does not exist starts a new empty buffer; the file is
created on the first save. To use a configuration file outside the default
location, pass it explicitly:

```sh
./target/release/cano --config path/to/init.lua path/to/file
```

`make` remains available as a compatibility wrapper and copies the release binary to `build/cano`.

Run the full validation suite with:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

## Modes
Normal - For motions and deletion \
Insert - For inserting text \
Visual - For selecting text and performing actions on them \
Search - For searching of text in the current buffer \
Command - For executing commands

## Keybinds
|Mode  | Keybind        | Action                                          |
|------|----------------|-------------------------------------------------|
|Global| Ctrl + Q       | Quit from any mode (refuses if there are unsaved changes) |
|Global| Esc / Ctrl + C | Enter Normal Mode                               |
|Normal| h              | Move cursor left                                |
|Normal| j              | Move cursor down                                |
|Normal| k              | Move cursor up                                  |
|Normal| l              | Move cursor right                               |
|Normal| x              | Delete character                                |
|Normal| g              | Go to first line                                |
|Normal| G              | Go to last line                                 |
|Normal| 0              | Go to beginning of line                         |
|Normal| $              | Go to end of line                               |
|Normal| w              | Go to next word                                 |
|Normal| b              | Go to last word                                 |
|Normal| e              | Go to end of next word                          |
|Normal| o              | Create line below current                       |
|Normal| O              | Create line above current                       |
|Normal| Ctrl + o       | Create line below current without changing mode |
|Normal| %              | Go to corresponding brace                       |
|Normal| i              | Enter insert mode                               |
|Normal| I              | Enter insert mode at the first non-blank character |
|Normal| a              | Insert mode on next char                        |
|Normal| A              | Insert mode at end of line                      |
|Normal| v              | Enter visual mode                               |
|Normal| V              | Enter visual mode by line                       |
|Normal| dd             | Delete current line                             |
|Normal| yy             | Yank current line                               |
|Normal| p              | Paste                                           |
|Normal| u              | Undo                                            |
|Normal| U              | Redo                                            |
|Normal| /              | Enter Search mode                               |
|Normal| n              | Jump to next search                             |
|Normal| N              | Jump to previous search                         |
|Normal/Insert| Ctrl + S| Save and exit                                   |
|Normal| r              | Replace current char with the next typed key (Esc cancels) |
|Normal| d + motion     | Delete over the next motion                     |
|Normal| (n) + motion   | Repeat next motion n times (also `n`, `N`, `u`, `U`) |
|Normal| (n) + d        | Delete n lines                                  |
|Normal| (n) + g / G    | Go to line n                                    |
|Normal| Ctrl + n       | Toggle file explorer (Esc closes it)            |
|Normal| Ctrl + r       | Toggle the recent-file list (Esc closes it)     |
|Normal| [space] n      | Leader mapping for Ctrl + n                     |
|Normal| [space] r      | Leader mapping for Ctrl + r                     |
|Normal| Ctrl + m       | Toggle markdown display (Enter is the same key) |
|Normal/Visual| s{char} | EasyMotion: jump to any {char} on screen, forward or back |
|Normal| t{char}        | EasyMotion: jump just before the next {char} (forward only) |
|Normal| *              | Search the word under the cursor and highlight every match |
|Normal| [space] i      | Leader mapping for `*`                          |
|Normal| [space] o      | Leader mapping for `:nohl`                      |
|Normal/Visual| Arrow keys | Move like h/j/k/l                            |
|Insert| Tab            | Insert the configured indentation               |

## Mouse
The mouse is on by default and can be turned off with `:set-var mouse 0`, or
from `init.lua` with `setup({ mouse = false })`. The setting takes effect
immediately, without a restart.

| Gesture | Action |
|---------|--------|
| Click | Move the cursor there. A click in the gutter goes to the start of that line, and a click past the end of a line goes to its end. |
| Click and drag | Select from where the button went down, in charwise Visual mode. |
| Wheel | Scroll three lines, bringing the cursor along only as far as it must. |
| Click or drag the scrollbar | Scroll to that position. The thumb lands on the row you clicked and follows a drag, which keeps working even if the pointer strays off the column. |
| Click in the explorer or recent list | Select that entry; clicking the entry already selected opens it, so a double click opens. Its scrollbar moves the selection instead, and never opens anything. |

While Cano has the mouse the terminal does not, so its own click-to-select stops
working. Most terminals still offer a native selection with Shift held; turning
`mouse` off hands it back entirely.

A click is not a key: it cannot answer the save prompt, complete an `:imap`, or
pick an `s`/`t` jump label.

## Recent files
`Ctrl + r` opens a list of the files you have opened before, most recent first.
`j`/`k` and the arrow keys move through it, Enter opens the selection, and Esc
or a second `Ctrl + r` closes it. It shares the pane with the file explorer, so
opening one closes the other.

If the buffer has unsaved changes, both keys ask `Save changes? (y/n, Esc
cancels)` before leaving it: `y` writes the file and then opens the pane, `n`
opens it and keeps the changes in the buffer, and Esc stays put. A failed write
leaves the pane shut, so unsaved work is never one keystroke from being
replaced.

The list is stored beside the effective configuration file — `~/.config/cano/recent`
by default — as one absolute path per line, and it survives across sessions. It
holds up to 50 entries, records a file each time one is opened, and never offers
an entry whose file has since been deleted. Built-in help pages are left out of
it. Long paths are shown with their beginning elided, because the file name is
the part that tells two entries apart.

## Jump motions
`s` and `t` are the two find motions from
[vim-easymotion](https://github.com/easymotion/vim-easymotion). Both ask for one
character, label every occurrence you can see, and jump to whichever label you
type next. They differ the way the plugin's do:

- `s{char}` finds the character **in both directions**.
- `t{char}` is **forward only** and stops one byte **before** the match, like
  vim's own `t`. (Upstream's bidirectional till is a separate `bd-t` mapping.)

Labels come from EasyMotion's default key order, `asdghklqwertyuiopzxcvbnmfj;`,
and are handed out nearest-first, so the closest match is always one keystroke.
When there are more matches than keys the trailing keys become prefixes for
two-key labels, and typing the first key narrows the labels still on screen. A
single match skips the label and jumps straight there. Escape, or any key that
matches no label, cancels.

`s` also works in Visual mode, where it is a motion like any other: the labels
appear over the text and picking one extends the selection to it, keeping the
anchor where you started. A linewise selection (`V`) still grows by whole rows.
Escape cancels the jump and leaves the selection alone, so it takes a second
Escape to leave Visual mode.

Targets are limited to the rows currently on screen, because a label you cannot
read is not a label you can reach. While a jump is collecting keys it owns the
keyboard, so a `set-map` mapping cannot fire out from under the labels.

## Search and replace
`:s` is vim's substitute command:

```vim
:%s/foo/bar/g
```

| Command | Action |
|---------|--------|
| `:%s/foo/bar/g` | Replace every `foo` with `bar` in the whole file |
| `:%s/foo/bar/gc` | Confirm each change (`y` yes, `n` no, `a` all the rest, `q` stop) |
| `:%s/foo/bar/gi` | Case-insensitive: also matches `Foo`, `FOO` |
| `:%s/\<foo\>/bar/g` | Whole word only, so `foobar` and `seafood` are left alone |
| `:5,12s/foo/bar/g` | Only lines 5 through 12 |
| `:s/foo/bar/g` | Only the line the cursor is on |
| `:%s#http://#https://#g` | Any punctuation can be the delimiter, so slashes need no escaping |

The range may be `%` for the whole file, a line number, `.` for the current
line, `$` for the last, or a `from,to` pair of any of those; without one the
command acts on the cursor's line. Without `g` only the first match on each line
is replaced. Flags may be combined, as in `gc` or `gi`. `substitute` may be
spelled out in place of `s`.

One `:s` command is **one undo step**, however many lines it changed — including
a confirmed run, where `u` takes back everything you said yes to.

Patterns are literal text rather than regular expressions, with the two vim
assertions `\<` and `\>` for word boundaries. Inside the pattern or the
replacement, `\` escapes the delimiter and itself.

## Highlighting the word under the cursor
`*` takes the word under the cursor (or the next one on the line), moves to its
next **whole-word** occurrence, and highlights every match — so `*` on `the`
leaves `then` and `other` alone. `/` highlights its matches too, as a substring
search, and `n` repeats whichever kind was last used.

`n` repeats the search forward and `N` repeats it backward; both wrap around
the end of the buffer and take a count, so `3N` goes back three matches.

`:nohl` (or `:nohlsearch`) stops showing the highlight without forgetting the
pattern, so `n` and `N` keep working afterwards.

The leader is space, giving the two mappings:

```vim
let mapleader = " "
map <leader>i *          " [space] [i] to highlight a word
map <leader>o :nohl<cr>  " [space] [o] to un-highlight all words
```

Both are built in, so no configuration is needed. Space followed by any other
key does nothing.

## Markdown
Markdown display formats the buffer in place: headings, emphasis, code, links,
lists, quotes, tables and rules are colored, and the syntax punctuation is
dimmed rather than hidden. Every byte keeps the cell it occupies in the normal
display, so the cursor, selections, search and every motion behave identically
and the file stays fully editable while it is on.

It is on by default for `.md`, `.markdown`, `.mdown`, `.mkd`, `.mkdn` and
`.mdx` files, replaces source-code highlighting while active, and is shown as
`[MD]` in the status line. `Ctrl + m` toggles it; most terminals send the same
byte for `Ctrl + m` and `Enter`, so `Enter` toggles it in Normal mode too.

## Visual
Visual mode works the same as Normal mode, except it works on the entire selection, instead of character by character.
The motions `h j k l 0 $ w b e g G %` and the arrow keys extend the selection.
| Keybind        | Action                                          |
|----------------|-------------------------------------------------|
| s{char}        | Extend the selection to any {char} on screen    |
| d / x          | Delete the selection                            |
| y              | Yank the selection                              |
| >              | Indent current selection                        |
| <              | Unindent current selection                      |

## Search
Search mode takes a string and finds it in the file.
if prepended with 's/' then it will replace the first substring with the second.

Example: Using the following command
```sh
s/hello/goodbye
```
Will replace hello with goodbye.

## Commands 

Press `:` in Normal mode, type a command, and press Enter. Cano supports these
Vim-compatible file commands:

| Command | Action |
|---------|--------|
| `:w`    | Write the current buffer without exiting |
| `:q`    | Quit if the buffer has no unsaved changes |
| `:q!`   | Quit and discard unsaved changes |
| `:wq`   | Write the current buffer and quit |
| `:e`    | Exit without writing (legacy alias for `:q`) |
| `:we`   | Write the current buffer and exit (legacy alias for `:wq`) |

`:q`, `:e`, and Ctrl+Q refuse to close a modified buffer. Use `:wq` to save
it or `:q!` to discard the changes.

Additional commands:

| Command               | Action                                                    |
|-----------------------|-----------------------------------------------------------|
| set-output "(file)"   | change output file (the quotes are required)              |
| echo (v)              | echo value (v) where v is either an ident or a literal    |
| set-var (var) (value) | Change a config variable                                  |
| set-map (a) "(b)"     | Map key a to any combination of keys b                    |
| let (n) (v)           | Create variable (n) with value (v)                        |
| !(command)            | Execute a shell command                                   |

Mappings expand in Normal mode. An expansion can run a colon command by
including the newline that submits it, for example
`set-map <c-p> ":wq\n"` typed with a literal Enter inside the quotes.

### Special Keys
These special key spellings are accepted by `set-map`:
| Key |
|-----|
| `<space>` `<esc>`/`<escape>` `<tab>` `<nul>` |
| `<enter>`/`<cr>`/`<return>` `<bs>`/`<backspace>` |
| `<up>` `<down>` `<left>` `<right>` |
| `<home>` `<end>` `<delete>`/`<del>` `<insert>`/`<ins>` |
| `<pageup>`/`<page-up>` `<pagedown>`/`<page-down>` |
| `<c-a>` through `<c-z>` (also spelled `<ctrl-a>` … `<ctrl-z>`), `<c-?>` |

## Config file
The config file is stored in ~/.config/cano/init.lua by default (on Windows
the home directory comes from `USERPROFILE`, e.g.
`C:\Users\you\.config\cano\init.lua`), or can be set at runtime like so:
```sh
./cano --config <config_file>
```

The `init.lua` is the entrypoint of your cano configuration.
Call the setup() function to make changes to the default configuration.
Cano preserves the legacy option semantics: every option is read as a Lua
Boolean, and non-Boolean values are ignored. Options you do not set keep the
editor's built-in defaults (syntax highlighting on, `indent` of four spaces,
relative numbers off). In particular, numeric `indent` and `undo_size` values
are ignored by the compatibility API; use `false` for tab indentation or
`true` for one-space indentation. `auto_indent` and `undo_size` are retained
compatibility settings but do not currently change editor behavior.

A ready-to-copy configuration is available at [`examples/init.lua`](examples/init.lua).

```lua
setup({
	syntax = true, -- toggle syntax highlighting on-off
	auto_indent = true, -- retained compatibility no-op
	relative = true, -- toggle relative line numbers
	indent = false, -- false: tabs, true: one space
	undo_size = false, -- retained compatibility no-op
})
```

The first parameter of the setup() function must be a table.
You are free to decide which of the settings above you want to change.
Settings you never set keep the editor defaults. Repeated `setup` calls retain values from earlier calls.

Therefore, it is also perfectly valid to call setup() with an empty table:
```lua
setup({})
```

There is also a way to interact with cano from your configuration.
The setup() function returns a table of functions you can use to do so.
```lua
local cano = setup({
	syntax = true, -- toggle syntax highlighting on-off
	auto_indent = true, -- retained compatibility no-op
	relative = true, -- toggle relative line numbers
	indent = false,
	undo_size = false,
})

cano.exit(42, "Yoo")
```

The following functions are available:
```lua
---@param code The return code of cano on exit (clamped to 0–255)
---@param message Optional; printed on exit when provided
function exit(code, message) end
```

## Syntax highlighting
Cano highlights C, C++, Rust and Python out of the box, with no configuration.
The language is chosen from the file's extension:

| Language | Extensions |
|----------|------------|
| C        | `c`, `h` |
| C++      | `cc`, `cpp`, `cxx`, `c++`, `hh`, `hpp`, `hxx`, `h++`, `ipp`, `tpp` |
| Rust     | `rs` |
| Python   | `py`, `pyi`, `pyw` |
| Bash     | `sh`, `bash`, `zsh`, `ksh`, `ash`, `dash`, and the names `.bashrc`, `.bash_profile`, `.bash_aliases`, `.bash_logout`, `.profile`, `.zshrc`, `.zprofile`, `.zshenv`, `.zlogin`, `.zlogout`, `.kshrc` |
| Vimscript | `vim`, `vimrc`, and the names `.vimrc`, `_vimrc`, `.gvimrc`, `.exrc` |
| Lua      | `lua` |

The language picks the scanner as well as the word lists, because the languages
disagree about what the same characters mean: `//` opens a comment in C, C++ and
Rust but is floor division in Python; `'a` is a lifetime in Rust and a string
delimiter elsewhere; `#` is a directive in C and C++, an attribute in Rust, a
comment in Python, and in shell a comment only at the start of a word so that
`${name#prefix}` survives; `"` is a string everywhere except vimscript, where it
is also the comment marker; `--` is a comment in Lua and a minus sign elsewhere.
Rust raw strings (`r#"…"#`) and nested block comments, Python triple-quoted and
prefixed (`f"…"`, `rb'…'`) strings, shell parameter expansion, vim key notation
(`<leader>`, `<C-x>`) and Lua long brackets (`[[…]]`, `[=[…]=]`) are all
understood.

Because `.vimrc` and `.bashrc` have no extension at all, the whole file name is
consulted before falling back to one. A `.cyntax` palette is still keyed by
extension, so a file named rather than extended gets the built-in colors but
cannot be given a custom palette.

Any other extension is left uncolored unless you supply a `.cyntax` palette for
it, and `:set-var syntax 0` turns highlighting off entirely.

There is a secondary config file, which is for custom syntax highlighting. It is stored in the same folder as the regular config, but uses a different naming format.
An example is ~/.config/cano/c.cyntax (spelled cyntax, with a c). The c can be replaced with whatever the file extension of your language is, such as go.cyntax for Golang.
Here is an example of a cyntax file:
```sh
k,170,68,68,
auto,struct,break,else,switch,case,
enum,register,typedef,extern,return,
union,continue,for,signed,void,do,
if,static,while,default,goto,sizeof,
volatile,const,unsigned.
t,255,165,0,
double,size_t,int,
long,char,float,short.
w,128,160,255.
```
There's a bit to unpack here, basically the single characters represent the type of the keywords:
k - Keyword
t - Type
w - Word
The type is then followed by the RGB values, all comma separated <b>without</b> spaces. After the RGB values, there is the actual keywords. End each type with a dot '.' as seen above, to indicate to Cano that the list is finished. The words are meant to be left blank, as it will highlight any words not found in the keywords above with the chosen RGB color.
If you wish to only set the color, you can provide no keywords to any, and it will fill in the keywords with the built-in list for that file's language (C for an extension Cano does not recognize). A `.cyntax` file supplies colors and words only; it never changes how the file is scanned, so `rs.cyntax` still gets Rust's lexer.

## Insert-mode mappings
`:imap {keys} {rhs}` maps a sequence of keys typed in Insert mode:

```vim
imap ;; <Esc>
imap jk <Esc>
imap ,d hello
```

The left-hand side may be more than one key. The right-hand side is a key
sequence: `<...>` spellings become their byte and everything else is taken
literally, so `imap ;; <Esc> :w <CR>` works too. Whitespace separates tokens, so
quote a right-hand side that needs spaces of its own.

Only an uninterrupted run of typed keys completes a mapping — moving the cursor
or leaving Insert mode between them breaks the run. Because there is no input
timeout to wait on, a mapping fires as soon as its keys are all in, which means
a longer mapping sharing a shorter one's prefix is unreachable. Re-running
`:imap` with the same left-hand side replaces the earlier binding. Keys with no
single-byte spelling (the arrows, say) cannot be a right-hand side, because
replay feeds it back one byte at a time.

## Config Variables
Config variables can also be modified at runtime by using `:set-var ...`.
Both the `init.lua` spellings (`auto_indent`, `undo_size`) and the dashed
spellings (`auto-indent`, `undo-size`) are accepted.

```sh
relative # toggle relative line numbers
auto-indent # retained compatibility no-op
syntax # toggle syntax highlighting on-off
indent # set indent
undo-size # retained compatibility no-op
cursorline # mark the line the cursor is on (also spelled cursor-line)
mouse # let Cano handle the mouse
```

`cursorline` is vim's option of the same name, off by default as it is in vim.
It underlines the cursor's line rather than tinting its background, which is
what vim itself does in a terminal (`CursorLine` defaults to `cterm=underline`)
and the only thing that stays readable without knowing whether the terminal's
background is light or dark. It can also be set from `init.lua`:

```lua
setup({ cursorline = true })
```

## Installation

Cano builds from source on Windows, Linux, and macOS. All three platforms
need a current Rust toolchain (installed with [rustup](https://rustup.rs))
and a C compiler, because the vendored Lua 5.4 is compiled by the `mlua`
dependency during the build.

### Windows

1. Install rustup, either with winget or from <https://rustup.rs>:
   ```powershell
   winget install Rustlang.Rustup
   ```
2. When prompted, install the Visual Studio C++ Build Tools (rustup offers
   this automatically); the default `stable-msvc` toolchain is what you want.
3. Build and run from a fresh terminal:
   ```powershell
   cd path\to\cano
   cargo build --release --locked
   .\target\release\cano.exe .\file.txt
   ```
   Copy `target\release\cano.exe` somewhere on your `PATH` to install it.
   Note that `:!command` shell execution requires `sh` and is not available
   on Windows.

### Linux

1. Install a C toolchain if you do not have one (`sudo apt install
   build-essential` on Debian/Ubuntu, `sudo dnf group install
   development-tools` on Fedora, `sudo pacman -S base-devel` on Arch).
2. Install rustup:
   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
3. Build and install:
   ```sh
   cd path/to/cano
   cargo build --release --locked
   sudo make install PREFIX=/usr/local
   ```
   `make install` places the binary at `/usr/local/bin/cano` and the help
   pages under `/usr/local/share/cano/help`.

   On Nix or NixOS you can instead build the `.#cano` package from the
   included flake.

### macOS

1. Install the Xcode command-line tools for the C compiler:
   ```sh
   xcode-select --install
   ```
2. Install rustup, either with Homebrew (`brew install rustup-init &&
   rustup-init`) or the script above.
3. Build and install:
   ```sh
   cd path/to/cano
   cargo build --release --locked
   sudo make install PREFIX=/usr/local
   ```

Prebuilt distribution packages (such as the AUR package) target the original
C implementation; see [Cano-Projects/Cano](https://github.com/Cano-Projects/Cano)
for those.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
