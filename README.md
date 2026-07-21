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
|Normal/Insert| Ctrl + S| Save and exit                                   |
|Normal| r              | Replace current char with the next typed key (Esc cancels) |
|Normal| d + motion     | Delete over the next motion                     |
|Normal| (n) + motion   | Repeat next motion n times (also `n`, `u`, `U`) |
|Normal| (n) + d        | Delete n lines                                  |
|Normal| (n) + g / G    | Go to line n                                    |
|Normal| Ctrl + n       | Toggle file explorer (Esc closes it)            |
|Normal/Visual| Arrow keys | Move like h/j/k/l                            |
|Insert| Tab            | Insert the configured indentation               |

## Visual
Visual mode works the same as Normal mode, except it works on the entire selection, instead of character by character.
The motions `h j k l 0 $ w b e g G %` and the arrow keys extend the selection.
| Keybind        | Action                                          |
|----------------|-------------------------------------------------|
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
If you wish to only set the color, you can provide no keywords to any, and it will fill in the keywords with C keywords by default.

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
