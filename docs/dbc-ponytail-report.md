# Design-by-Contract + Ponytail refactor — report

Date: 2026-09-17 · Base commit: `ceac76a` (0.26.917) · Scope: whole crate (`src/`, `tests/`, `Cargo.toml`)

## Summary

Two passes over the codebase, both behaviour-preserving:

1. **Design by Contract.** 38 contracts added as `debug_assert!`, covering type invariants, preconditions and
   postconditions. They are checked in every debug build and every test run, and compile out of release builds
   entirely. No crate was added.
2. **Ponytail refactor.** A repo-wide ponytail audit (`ponytail-audit`) was run by four parallel read-only
   reviewers. Each finding was then checked against the code before it was applied. The contracts from pass 1
   served as the safety net for this pass.

| Metric | Before | After |
|---|---|---|
| Lines in `src/` | 18,623 | 17,648 (**−975**, −5.2%) |
| Diff | | +835 / −1,815 across 22 files |
| Tests passing | 353 / 353 | 353 / 353 (no test expectation changed) |
| `cargo clippy --all-targets` warnings | 2 | **0** |
| `cargo fmt --check` | clean | clean |
| Release binary (`opt-level="z"`, LTO) | 967,136 B | 948,928 B (−18 KB) |
| Runtime contracts | 0 | 38 (debug builds only) |
| Dependencies | 5 | 5 |

## 1. Design by Contract

### Approach

- **No framework.** Rust has no built-in contracts, and the `contracts` crate is a proc-macro dependency. Plain
  `debug_assert!` does the same job at zero release cost. This follows ponytail's "stdlib before
  dependency" rule.
- **Only conditions that always hold.** A function that deliberately tolerates bad input was left without a
  precondition. `insert_byte` clamps an out-of-range cursor, `delete_selection` returns `None` for a reversed
  range, and `autoformat::format` accepts a region past EOF; tests exercise all three on purpose. Each contract
  below was traced to the code that guarantees it.
- **Contracts at convergence points.** Most contracts sit where many code paths meet (`App::handle`,
  `apply_record`, `tokens`), not on every small function. This gives broad coverage with few lines.
- **The legacy quirks are pinned, not "fixed".** Examples: the `InsertChars` off-by-one, the inclusive clipboard
  with its synthetic NUL, and a failed undo record being dropped. The contracts describe the characterized
  behaviour from `MIGRATION_NOTES.md`.

### Contract inventory (38)

| Module | Kind | Contract | What relies on it |
|---|---|---|---|
| `buffer` | invariant (documented on `Buffer`, checked ×5) | `cursor <= data.len()` and `rows == derived_rows(data)`, so there is always ≥1 row. Checked after `insert_byte`, `delete_byte`, `delete_selection`, `replace_region`, `insert_selection` | Everything that indexes `rows[...]` |
| `buffer` | post ×2 | `move_up`/`move_down` land on exactly the adjacent row | `j`/`k`, visual motions |
| `buffer` | post | `matching_brace_index` returns the opposite bracket, outside any quote | `%` |
| `history` | post ×2 | `apply_record` leaves the buffer valid; the inverse starts where the record did and is a forward range | Every later undo/redo replay |
| `editor` | post | `motion_range` returns a forward, in-bounds range | `d`/`c`/`y` + motion |
| `textobject` | post | `range` returns a forward, in-bounds range | `diw`, `ci(`, … |
| `command` | post | after `SetVar`, `variable(v) == value` | Keeps the two hand-written 12-arm tables in step |
| `substitute` | post | `matches` are ordered, non-overlapping, inside the range | App applies them back to front |
| `syntax` | post (scanner contract) | every scanner returns `at < end <= len` | The stall-free tokenizer loop |
| `syntax` | post | `tokens` spans are non-empty, ordered, in bounds | Renderer's `colors[start..end].fill(..)` |
| `render` | invariant | gutter + text + scrollbar = frame width exactly | Layout arithmetic |
| `render` | post | `Viewport::follow` keeps the cursor inside the new window | Scrolling |
| `render` | invariant ×2 | `Track`: thumb fits the track; `scrollable_track <= scrollable_items` | Makes the next row's inverse exact |
| `render` | post | `item_from_track(row)` is the exact inverse of thumb placement | Scrollbar dragging |
| `render` | post | the terminal cursor is inside the frame | `set_cursor_position` |
| `render` | post | an elided label is exactly `width` chars | List panes |
| `app` | post of `handle` ×7 | buffer valid · at most one pane open · recent/history picker cursor names an entry · prompt cursor inside prompt (in prompt modes) · Insert-only state gone outside Insert · a jump only waits in Normal/Visual | Every key path, including mappings and `.` replay |
| `app` | pre | `dispatch` depth ≤ `MAX_DEPTH` | Recursion bound for mappings/replays |
| `app` | post | `rewrite_buffer`: splicing only the changed span reproduces the whole rewrite | Comment toggle / autoformat undo record |
| `app` | post | `place_cursor_on_row` lands on the requested (clamped) row | Paging keys |
| `autoformat` | post | `format` never adds or drops a line | `moved()` carries regions between steps |
| `autoformat` | post | `format_json` output still parses as JSON | JSON pretty-printer |
| `jump` | post | `labels` returns `count` labels, none a prefix of another | EasyMotion label entry |
| `markdown` | invariant | each inline helper consumes ≥1 byte within the line or declines | Stall-free inline scan |
| `backup` | post | every name `destination` writes is recognised by `prune` | No orphaned backups |

A contract also paid for itself in code. Because the `Buffer` invariant ("always ≥1 row") is now checked, six
defensive `if rows == 0 { return }` guards in `app.rs` became provably dead and were removed. The rest of the
codebase already indexes `rows[...]` on that assumption.

## 2. Ponytail refactor

The audit tags follow ponytail's scheme: `delete` (dead code), `stdlib`/`native` (already in std or a
dependency), `yagni` (one-caller layer), `shrink` (same logic, fewer lines).

### Applied

**Duplicated logic collapsed to one place**

- The quote/escape/bracket state machine existed three times (`buffer::quoted_bytes`, `editor::brace_depth`,
  `autoformat::Nesting`). It is now one `buffer::Nesting` used by all three.
- The depth-counted bracket scan existed four times (`%` forward and back, `di(` back and forward). It is now
  one `buffer::balanced`.
- The ASCII word predicate existed three times (`buffer`, `substitute`, `syntax`). It is now one
  `buffer::is_word`.
- The tab-aware column count existed three times (two in `render`, one in `autoformat`). It is now one
  `render::display_columns`.
- These existed twice each and are now shared: `bytes_to_path` (`app`/`recent`), `leading_end`
  (`comment`/`autoformat`), and the test `Fixture` (`io`/`explorer`, now `lib::test_support`).
- `editor`:
  - `cut` replaces three copies of "delete → clipboard → undo record".
  - `motion` replaces the Normal/Visual motion tables.
  - `>`/`<` are merged, and `insert_tab`/`add_indent` now reuse `indentation()`.
- `app`:
  - `prompt_input` replaces the near-identical `command_input` and `search_input`.
  - `rewrite_buffer` replaces the identical tails of comment-toggle and autoformat.
  - One tail replaces the four copies of the `execute_command` result handling.
- `history`: `undo`/`redo` are one `step`, and `apply_record` builds its range error through one closure
  instead of nine literals.
- `command`: eight argument-free commands share one arity check.

**Stdlib / platform instead of hand-rolled**

- `<[u8]>::trim_ascii` replaces a hand-written `trim`.
- `windows().position()` finds raw-string and Lua long-bracket closers, replacing two loops.
- `slice::fill` colours token spans.
- `saturating_add_signed` appears in three places.
- `as_chunks` and `take_while().count()` replace hand-written equivalents.
- ratatui's `Clear` widget and `Buffer::set_style` replace hand-written per-cell loops.
- `search_wrapped` is a single iterator.
- `replace_first_after_cursor` uses one `replace_region` instead of byte-by-byte delete/insert, going from
  O(n·m) to O(n).

**Delete (dead or speculative)**

- Unused pub API: `Row::len`, `Buffer::is_closing_brace`, `History::clear`, `SubstituteError::EmptyRange`, and
  `Recent::display_name` (a one-caller wrapper).
- Error impls nothing reads: `Display`/`Error` on `SyntaxError` and `CliError` (their messages are built
  elsewhere), and `ConfigError::source` chaining.
- `main::apply_effects` returned `Result` but had no error path; it now returns `bool`.
- Dead code inside functions: a write to a by-value parameter in `apply_record`, impossible `checked_add` error
  arms, and a redundant Escape check in `jump_input`.
- Cargo.toml `[lib]` and `default-run`, which only restated cargo's defaults.

**Shrink**

- The 14 syntax keyword tables were written one `b"word",` per line; they are now packed word lines. This
  saved 435 lines, and a script verified all 14 tables yield identical words in identical order.
- `main` applies the Lua config with `unwrap_or` instead of nine `if let`s, and `config` reads its slots in a
  loop.
- `app`:
  - `last_search` is stored as the `Highlight` it always was rebuilt into.
  - The completion index is one match.
  - `split_options` is one match.
  - The substitution end is computed from the untouched tail.
  - `window_rows` and `named` are inlined.
  - `MAX_DEPTH` names the literal `64` that appeared four times.
- `render`: 13 sites of `saturating_add(u16::try_from(n).unwrap_or(u16::MAX))` now go through one `shifted`.
- The two pre-existing clippy warnings are fixed.

### Audit findings deliberately not applied

| Finding | Why not |
|---|---|
| Drop the negative-era branch of `backup::civil_from_days` | It is a general algorithm documented as exact for the whole calendar; trimming it saves 4 lines and makes that doc false |
| `main::run` → `Box<dyn Error>` | Reworks error plumbing to save about 6 lines |
| Remove the empty `impl Error for X {}` markers where `Display` is used | Conventional std interop; the cut is 1 line each |
| `Substitute::resolve` empty-rows guard | Removing it changes the `Option` signature and ripples to callers |
| `config` `Arc<Mutex>` → `Rc<RefCell>` | Only safe together with the re-entrancy bug fix below |
| API used only by its own tests (`command::Span`, `History::new`, `Cli::help_page` shape, …) | Cutting it means editing test expectations |
| Recent/Explorer/comment contracts | Each held trivially by construction; the pane state is already checked in `App::handle` |

## 3. Bugs found along the way (not fixed: out of scope for a behaviour-preserving refactor)

**Reproduced** (throwaway tests driving `App::handle`, since deleted):

| # | Severity | Bug | Repro | Suggested fix |
|---|---|---|---|---|
| 1 | **High: crash** | Prompt history recall indexes past the end. `history_browse` survives Escape and is shared by `:` and `/` | `:a⏎ :b⏎ :c⏎ /x⏎` then `:` `↑` `Esc` `/` `↑`. Panics with index out of bounds in `browse_history`. The release profile uses `panic = "abort"`, so unsaved work is lost | Reset `history_browse` in `clear_prompt` |
| 2 | **High: data loss** | A mouse click in Insert mode moves the cursor without closing the active insert record, so undo deletes text that was there before | Buffer `hello world`: `i` `ab`, click column 8, `Esc`, `u`. The buffer becomes `world` | Route the click through the same close/restart path as `insert_move` |
| 3 | Medium | `delete_rows` clears the operator but not an armed text object, so the next key is swallowed | `one/two/three/four`: `d` `i` `3` `d` `x`. The `x` does nothing | Call `cancel_pending()` in `delete_rows` |
| 4 | Low | `:set sw=2 bogus` updates `commands.indent` but returns before syncing `editor.indent` | After it, `commands.indent == 2` but `editor.indent` still holds the old value | Sync the indent before the early return |

**By inspection** (not executed):

- `cano.exit(-1)` exits with status 0, because the code clamps to `0..=255`; C would give 255.
- Backups: counter suffixes sort as text (`.10` < `.2`). With many saves in one second, `prune` can delete a
  newer copy.
- `config`: the mutex is held while `table.get` runs. A Lua `__index` metamethod that calls `setup`,
  `cano.command` or `cano.exit` re-locks it on the same thread and hangs.
- `saved` is only refreshed at the end of `handle`. A mapping such as `x:q⏎` can edit and then quit without the
  "No write since last change" refusal.
- The Vim scanner treats a backslash inside `'…'` as an escape; Vim single quotes are literal, so this is minor
  mis-colouring.
- Two misplaced doc comments: the panic-hook doc sits on `terminal::enter_terminal`, and the `for_extension` doc
  sits on `syntax::for_path`.

Once bug 2 is fixed, a further contract becomes true and is worth adding: `mode != Insert ||
active_insert.start <= buffer.cursor`. Today that path breaks it legitimately.

## 4. Verification

- `cargo test --locked --all-targets`: 353 passed, the same count as before. No `assert` line and no `#[test]`
  was added, removed or changed. The only test-code edits are:
  - moving the duplicated `Fixture` into `lib::test_support`;
  - dropping two `.unwrap()`s after `apply_effects` stopped returning `Result`.
- `cargo clippy --locked --all-targets`: 0 warnings, down from 2. `cargo fmt --check` is clean.
- `cargo build --release --locked`: clean, 948,928 B.
- `tests/pty_smoke.py` drives a real PTY through resize, `:q` refusal, `:w`, `:wq`, `:q!`, `-h`/`--help`,
  comment toggle and the Control keys. It passed against the **release** binary, and against the **debug**
  binary with all contracts live.
- **Randomized stress (throwaway, not committed).** Seeds 1–30,000, each a session of up to 400 keys over 6 buffer
  shapes and 5 file types, driven through `App::handle` in a debug build. The keys were weighted toward motions,
  operators, text objects, `.`, `:s///gc`, `:set`, `:imap`, mouse and scrolling. It ran as three 10k shards of
  about 80–90 s each. **No contract fired and nothing panicked.**
  - Two sessions were stopped at a 256 KiB buffer cap, because repeated `yG`/`p` doubles the buffer.
  - An earlier attempt without that cap was terminated (SIGTERM) before reporting anything, so that run
    counts for nothing.
  - Random keys rarely hit the exact bug-1 sequence, which is why it was reproduced by hand.
- Line endings: the working tree was a Windows checkout with mixed CRLF/LF. Source files were normalized to LF
  to match the index and `.gitattributes`; git sees no line-ending changes.

## 5. How the contracts behave

- `cargo test` and `cargo build` (debug): every contract is checked, and a violation panics with the condition
  and the offending values.
- `cargo build --release` / `make`: `debug_assert!` compiles to nothing, so release behaviour and performance
  are unchanged. The release-path guards (for example the tokenizer's clamp and the markdown `max(at+1)`) are
  kept on purpose; the contracts only check that they never have to act.
- Some checks are O(n) per call in debug builds, such as re-deriving the rows or re-parsing JSON after a
  format. That is fine for tests and debugging; profile a release build.
