# Fresh Rust migration

This package was implemented independently from the generated Rust migration documents. No Cano source tree or earlier Rust port was used as implementation input.

The design follows the documented boundaries:

- `buffer` and `history` own byte-oriented editing state.
- `editor`, `command`, and `app` own modal interaction and typed effects.
- `config`, `syntax`, `io`, `process`, and `explorer` isolate external adapters.
- `render`, `terminal`, and `main` own Ratatui/Crossterm presentation and lifecycle.

Characterized compatibility choices include mixed selection endpoints, operation-specific undo records, Boolean-only Lua options, iterative left-to-right expression evaluation, trailing-NUL mapping storage (the NUL is stripped when a mapping is replayed), truncating saves, and `sh -c` command execution. Unsafe C memory behavior is represented by bounded results or explicit errors.

