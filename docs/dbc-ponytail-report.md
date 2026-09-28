# Design-by-Contract + Ponytail refactor — report

Date: 2026-09-19 · Base commit: `ceac76a` (0.26.917) · Scope: whole crate (`src/`, `tests/`, `Cargo.toml`)

The refactor (passes 1 and 2) was committed as `e6fcafc`, with a version bump to 0.26.918 that followed from
tooling in this workspace rather than from the work itself. The fixes in pass 3 are still uncommitted.

## Summary

Three passes over the codebase. The first two are behaviour-preserving; the third fixes bugs they found:

1. **Design by Contract.** 39 contracts added as `debug_assert!`, covering type invariants, preconditions and
   postconditions. They are checked in every debug build and every test run, and compile out of release builds
   entirely. No crate was added.
2. **Ponytail refactor.** A repo-wide ponytail audit (`ponytail-audit`) was run by four parallel read-only
   reviewers. Each finding was then checked against the code before it was applied. The contracts from pass 1
   served as the safety net for this pass.
3. **Bug fixes.** Thirteen real bugs: a crash, three hangs and two ways to lose text. Eleven came from the
   audit and two from the randomized stress run. All are fixed in section 3, each with a regression test that
   was checked to fail without its fix. This pass does change behaviour, deliberately.

| Metric | Before | After |
|---|---|---|
| Lines in `src/` | 18,623 | 18,022 (**−601**, −3.2%; the refactor cut 975, the fixes and their tests added 374) |
| Diff | | refactor +835 / −1,815 (22 files), fixes +483 / −95 (9 files, tests included) |
| Tests passing | 353 / 353 | 363 / 363 (353 unchanged, +10 regression tests) |
| `cargo clippy --all-targets` warnings | 2 | **0** |
| `cargo fmt --check` | clean | clean |
| Release binary (`opt-level="z"`, LTO) | 967,136 B | 951,072 B (−16 KB) |
| Runtime contracts | 0 | 39 (debug builds only) |
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

### Contract inventory (39)

| Module | Kind | Contract | What relies on it |
|---|---|---|---|
| `buffer` | invariant (documented on `Buffer`, checked ×5) | `cursor <= data.len()` and `rows == derived_rows(data)`, so there is always ≥1 row. Checked after `insert_byte`, `delete_byte`, `delete_selection`, `replace_region`, `insert_selection` | Everything that indexes `rows[...]` |
| `buffer` | post ×2 | `move_up`/`move_down` land on exactly the adjacent row | `j`/`k`, visual motions |
| `buffer` | post | `matching_brace_index` returns the opposite bracket, outside any quote | `%` |
| `history` | post ×2 | `apply_record` leaves the buffer valid; the inverse starts where the record did and is a forward range | Every later undo/redo replay |
| `editor` | post | `motion_range` returns a forward, in-bounds range | `d`/`c`/`y` + motion |
| `editor` | pre | a recorded insertion never inverts (`push_active_insert`) | Undo of typed text (added with fix 2) |
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
| `config` `Arc<Mutex>` → `Rc<RefCell>` | Was unsafe until bug 7 below; now that the lock is not held across Lua calls it is an option, but it buys nothing on its own |
| API used only by its own tests (`command::Span`, `History::new`, `Cli::help_page` shape, …) | Cutting it means editing test expectations |
| Recent/Explorer/comment contracts | Each held trivially by construction; the pane state is already checked in `App::handle` |

## 3. Bugs found, and fixed

The refactor itself changed no behaviour. These fixes are a separate, third pass, each with a regression test.
**Every test below was checked to fail without its fix**: the fix was reverted, that one test was run, and the
fix put back. Bug 8 cannot be reverted in place, so it was confirmed against the committed pre-fix state in a
scratch worktree, where the mapping returns `[Quit]` with unsaved changes; bugs 12 and 13 were confirmed there
too, hanging before the fixes and finishing instantly after them.

Neither hang needs an exotic file or a corrupted buffer: a one-line mapping, or a dot-repeat of an ordinary
insertion, is enough to wedge the editor with no way out but to kill it.

| # | Severity | Bug and fix | Regression test |
|---|---|---|---|
| 1 | **Crash** | Prompt history recall indexed past the end: `history_browse` survived Escape and was shared by `:` and `/`, so `:a⏎ :b⏎ :c⏎ /x⏎` then `:` `↑` `Esc` `/` `↑` panicked, and release builds abort. **Fix:** `clear_prompt` drops the walk, which also makes `↑` start from the newest entry again | `app`: `walking_the_history_starts_again_for_each_prompt` |
| 2 | **Data loss** | A mouse click in Insert mode moved the cursor without closing the record in flight, so `i` `ab`, click, `Esc`, `u` deleted `abhello ` from `hello world`. **Fix:** `Editor::place_cursor` closes the record and starts a new one, as the arrow keys already did; the click and the scroll wheel both go through it | `app`: `a_click_in_insert_mode_closes_the_insertion_it_leaves` |
| 3 | Medium | `delete_rows` cleared the operator but not an armed text object, so after `d` `i` `3` `d` the next key was swallowed. **Fix:** `cancel_pending()` | `editor`: `a_counted_delete_disarms_the_object_it_never_used` |
| 4 | Low | `:set sw=2 bogus` applied `sw=2` but returned before syncing `editor.indent`, so the editor kept indenting by the old width. **Fix:** sync after the loop either way | `app`: `a_failed_set_keeps_the_options_that_came_before_it` |
| 5 | Medium | `cano.exit(-1)` reported success, because the code was clamped to `0..=255`. **Fix:** a negative code is taken as its low byte the way C takes it (`-1` → 255); wide codes still saturate | `lifecycle`: `a_negative_lua_exit_code_still_reports_failure` |
| 6 | Medium | Backup pruning sorted counters as text, so `.10` sorted before `.2` and saves within one second could prune the newest copies. **Fix:** sort by stamp, then by counter as a number | `backup`: `counters_are_pruned_in_numeric_order` |
| 7 | **Hang** | The configuration lock was held while `table.get` ran. A Lua `__index` metamethod calling `setup`, `cano.command` or `cano.exit` took the same lock on the same thread and hung forever. **Fix:** read all nine slots first, then merge them under the lock | `config`: `a_metamethod_that_calls_back_into_the_api_does_not_hang` (bounded wait, so a regression fails instead of hanging) |
| 8 | **Data loss** | `saved` was a cache refreshed only at the end of a keypress, so a mapping such as `x:q⏎` edited and then quit without the "No write since last change" refusal. **Fix (root cause):** `saved()` compares the buffer with the last written copy on every read, so there is no stale copy to read. The `buffer_replaced` flag it needed is gone | `app`: `a_mapping_that_edits_before_quitting_is_still_refused` |
| 9 | Low | The Vim scanner treated `\` inside `'…'` as an escape, so `'C:\'` looked unterminated and lost its colour. Vim single quotes are literal and `''` is one quote. **Fix:** `vim_literal` | `syntax`: `vim_tells_a_comment_quote_from_a_string_quote` |
| 10 | Cosmetic | Two doc comments sat on the wrong item: the panic-hook explanation on `enter_terminal`, and the extension mapping on `for_path`. **Fix:** moved to `install_panic_hook` and `for_extension` | — |
| 11 | Low | `autoformat::format` dragged a region past EOF onto the last line, so a region that named no line still reformatted one. Not reachable from the editor, which never passes one. **Fix:** a region starting past the end stays past the end | `autoformat`: `a_region_takes_whole_lines_and_leaves_the_rest_alone` |
| 12 | **Hang** | `.` could repeat itself. A replay runs against the state it finds, not the one it was recorded in, so a `.` typed into Insert mode can come back as a Normal-mode repeat of the change it is inside. Each nesting multiplies the keys replayed, and the depth cap of 64 bounds nesting, not work. **Fix:** a `.` met while replaying is not a repeat | `app`: `a_repeat_met_while_repeating_is_not_another_repeat` |
| 13 | **Hang** | A key mapping that expands to itself (`:set-map 'Q' 'QQ'`) doubles at every level, so 64 levels never finish. **Fix:** one press may expand into at most `MAX_EXPANSION` (10,000) keys, then it stops with the existing "Recursive key map" message | `app`: `a_mapping_that_expands_to_itself_stops_instead_of_spinning` |

One contract became true only after fix 2 and was added with it: a recorded insertion never inverts
(`push_active_insert`). Before the fix, a click to the left of the insertion produced exactly that.

## 4. Verification

- `cargo test --locked --all-targets`: **363 passed** (356 lib, 4 bin, 3 integration).
  - The refactor left all 353 original tests untouched: no `assert` line and no `#[test]` was added, removed or
    changed by it. Its only test-code edits were moving the duplicated `Fixture` into `lib::test_support` and
    dropping two `.unwrap()`s after `apply_effects` stopped returning `Result`.
  - The fixes added 10 regression tests, and one existing test (`opening_an_explorer_file_resets_the_saved_baseline`)
    now dirties the buffer by typing instead of writing to the `saved` field the fix removed. The two tests for
    the hangs run their work on a thread and wait at most ten seconds, so a regression fails the suite instead
    of wedging it.
- **Each fix was checked to be the thing its test catches.** For every fix, the source change was reverted, that
  one test was run, and the fix was restored: all eleven reverted fixes failed their test. Bug 8 cannot be reverted
  in place, so it was confirmed against the committed pre-fix state in a scratch worktree, where the mapping
  `x:q⏎` returns `[Quit]` on a dirty buffer.
- `cargo clippy --locked --all-targets`: 0 warnings, down from 2. `cargo fmt --check` is clean.
- `cargo build --release --locked`: clean, 951,072 B.
- `tests/pty_smoke.py` drives a real PTY through resize, `:q` refusal, `:w`, `:wq`, `:q!`, `-h`/`--help`,
  comment toggle and the Control keys. It passed against the **release** binary, and against the **debug**
  binary with all contracts live.
- **Randomized stress (throwaway, not committed).** Seeds 1–30,000, each a session of up to 400 keys over 6
  buffer shapes and 5 file types, driven through `App::handle` in a debug build, weighted toward motions,
  operators, text objects, `.`, `:s///gc`, `:set`, `:imap`, mouse and scrolling. It ran as three 10k shards of
  200–380 s. **No contract fired and nothing panicked.** Each session also presses `u` 2,000 times at the end;
  about half of all sessions undo exactly back to the text they started from. Why the others do not was not
  audited — a session may have opened another file into the buffer, and the characterized legacy records
  (`DeleteChar` keeps no redo bytes, `InsertChars` is a byte short) do not all round-trip. What the run checks
  is that undoing everything never panics and never breaks a contract.
  - **This run is what found bug 12.** One shard never finished, and the seed behind it hung inside a single
    `.`; probing the same shape by hand then turned up bug 13. Before the fixes that shard could not complete;
    after them all three finish.
  - The driver runs in an empty temporary directory. In the repo the explorer opens whatever it finds, and a
    session that loaded a multi-megabyte build artifact out of `target/` looked like a hang while it was only
    doing O(file size) work per key with contracts on — which also made runs irreproducible, since `target/`
    changes between them.
  - About 1 session in 600 is stopped at a 256 KiB buffer cap, because repeated `yG`/`p` doubles the buffer.
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
