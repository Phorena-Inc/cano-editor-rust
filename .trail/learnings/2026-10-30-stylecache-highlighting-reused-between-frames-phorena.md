---
title: "StyleCache: highlighting reused between frames"
author: Phorena
date: 2026-10-30
branch: AI-editor
cost: { minutes: 3, tokens: 32783 }
touches:
  - src/buffer.rs
  - src/lib.rs
  - src/main.rs
  - src/render.rs
  - src/syntax.rs
  - ".archify/architecture-cano-editor-20260928-160729/candidate.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.browser-check.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.delivery.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.finalize-summary.json"
  - ".archify/architecture-cano-editor-20260928-160729/cano-editor.finalize.json"
---

## Decided
- `render::StyleCache` (owned by the main loop, passed as `RenderOptions::styles`) keeps the last `CellStyles` with a copy of the buffer bytes and its source (Markdown, or the SyntaxConfig). It reuses the table while the bytes and source are equal. Tests pass `styles: None` to build fresh.
- Validity is a byte compare against a private copy, not an edit counter. `Buffer::data` is pub and written from many modules, so a missed counter bump would show stale colors. Cost: one extra buffer copy while highlighting is on, plus about 1 ms of memcmp per frame on 10 MiB.
- Plain text (no syntax, no markdown) drops the cached copy.
- Measured on 10 MiB Rust, 120x50, hlsearch on: frame 35 ms -> 3.9 ms when the text is unchanged; edit frames are unchanged at about 34 ms.
## Watch out
- Any new input to `CellStyles::build` must also go into `StyleSource`, or the cache will serve stale styles.
- Edits still re-tokenize the whole buffer. Incremental re-tokenizing needs care: the `tokens()` loop has no state, but scanners look ahead past a token's end, and `only_blanks_before` looks backward.
