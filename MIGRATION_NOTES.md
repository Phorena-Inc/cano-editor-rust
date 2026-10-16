# Fresh Rust migration

This package was implemented independently from the generated Rust migration documents. No Cano source tree or earlier Rust port was used as implementation input.

The design follows the documented boundaries:

- `buffer` and `history` own byte-oriented editing state.
- `editor`, `command`, and `app` own modal interaction and typed effects.
- `config`, `syntax`, `io`, `process`, and `explorer` isolate external adapters.
- `render`, `terminal`, and `main` own Ratatui/Crossterm presentation and lifecycle.

Characterized compatibility choices include operation-specific undo records, Boolean-only Lua options, iterative left-to-right expression evaluation, trailing-NUL mapping storage (the NUL is stripped when a mapping is replayed), and `sh -c` command execution. Unsafe C memory behavior is represented by bounded results or explicit errors.

Where the characterization recorded a C defect, the port now follows Vim instead:

- Selections and the clipboard: `x` and every cut put exactly the deleted bytes in the clipboard (no extra byte, no synthetic NUL at EOF), and a charwise Visual `d`/`x` includes the byte under the cursor, matching what Visual `y` copies. Line deletions (`dd`, `{count}dd`, linewise Visual `d`) store their lines linewise.
- Undo history: a fresh edit clears redo; the undo stack holds at most `undo_size` steps (oldest dropped; `undo_size` of 0 or less means no cap); redo of `r` works; the multi-byte insertion inverse no longer stops one byte short. A change (`c` plus the text typed before Esc or a cursor move), `o`/`O` plus its text, a counted edit, and a Visual `>`/`<` are each one undo step.
- Saving: the C editor truncated the file in place. Saves now write a temporary file beside the target and rename it over the original, keeping its permissions and any symlink; `:!` commands run in the background and report their exit status.
- Display: the C editor drew one cell per byte with a fixed tab width. Valid UTF-8 is now drawn at its display width, motions step over whole characters, and `tabstop` is an option of its own rather than an alias of `indent`. The buffer itself is still bytes, and saves are byte-exact.
