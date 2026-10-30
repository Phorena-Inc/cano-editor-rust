---
title: "Line index: SIMD newline scan, binary search, incremental typing"
author: Phorena
date: 2026-10-30
branch: AI-editor
cost: { minutes: 7, tokens: 93508 }
touches:
  - src/buffer.rs
  - ".archify/architecture-cano-editor-20260928-160729/candidate.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.browser-check.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.delivery.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.finalize-summary.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.finalize.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.html"
  - ".archify/architecture-cano-editor-20260928-160729/run-2/cano-editor.browser-check.json"
  - ".archify/architecture-cano-editor-20260928-160729/run-2/cano-editor.finalize-summary.json"
  - ".archify/architecture-cano-editor-20260928-160729/run-2/cano-editor.finalize.json"
---

## Decided
- `Buffer::rows` stays AoS `Vec<Row>` (pub field, used widely). SoA was ruled out: after the binary search, lookups touch only ~log2(n) cache lines.
- `row_for_index` uses `partition_point(|r| r.end < index)`. It is valid because row ends strictly increase and the last end is `data.len()`.
- `src/buffer/newline.rs` scans 64-byte blocks into a u64 newline mask (SSE2 on x86_64, SWAR elsewhere) and reuses the `rows` allocation.
- `insert_byte`/`delete_byte` of a non-newline byte shift the later rows (`shift_rows_after`) instead of rescanning.
- Measured on 10 MiB with the release profile: rescan 6.9 ms -> 1.6 ms, lookup 31 us -> 29 ns, typed char 12 ms -> 0.34 ms.
## Tried and ruled out
- Safe-call SSE2 intrinsics with only a `cfg(target_feature = "sse2")` fail with E0133 on rustc 1.98. They need `#[target_feature]` or an `unsafe` block.
## Watch out
- The release profile is `opt-level = "z"`, which turns off loop vectorization. Byte loops stay scalar, so the hot scans need explicit SIMD.
- Run `cargo +nightly miri test --lib buffer::newline` after changing the scanner (about 35 s).
