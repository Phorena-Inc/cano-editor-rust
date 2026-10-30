---
title: "Per-frame render cost: scan module, token lookup, binary overlays"
author: Phorena
date: 2026-10-30
branch: AI-editor
cost: { minutes: 12, tokens: 70263 }
touches:
  - src/buffer.rs
  - src/lib.rs
  - src/render.rs
  - src/syntax.rs
  - ".archify/architecture-cano-editor-20260928-160729/candidate.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.browser-check.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.delivery.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.finalize-summary.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.finalize.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.html"
---

## Decided
- The SIMD block scans live in `src/scan.rs` (`byte_mask`, `find_byte`, `find`/`rfind`). SSE2 is used on x86_64 and SWAR elsewhere. This supersedes the `src/buffer/newline.rs` path named in the line-index learning. The row-table fill is now `rows_into` in buffer.rs.
- `Highlight::find`/`rfind` delegate to `scan::find`/`rfind`, which filter candidates on the needle's first and last bytes.
- render.rs: `CellStyles::Syntax` keeps the token list and finds a cell's token by `partition_point`. The per-byte `Vec<Option<Color>>` (4 bytes per buffer byte per frame) is gone. The hlsearch overlay test per cell also uses `partition_point`.
- syntax.rs: `Vocabulary` (built once per `tokens()` call) replaces linear `in_group` (now test-only). It buckets words by length, packs words of 8 bytes or fewer into u64, and keeps group priority keyword > type > word.
- On 10 MiB of Rust, one frame went from about 130 ms to about 31 ms: tokens 72 -> 29 ms, matches 32 -> 1.5 ms, overlay 26 -> 0.06 ms.
## Tried and ruled out
- Limiting `Highlight::matches` to the visible range: overlapping-needle parity (e.g. "aa" in "aaaa") depends on where the scan starts, so the results would change.
## Watch out
- `tokens()` still scans the whole buffer every frame (about 27 ms on 10 MiB). The next win is caching tokens between frames when the buffer has not changed.
- `cargo +nightly miri test --lib scan::` takes about 9 min, mostly the find/rfind test.
