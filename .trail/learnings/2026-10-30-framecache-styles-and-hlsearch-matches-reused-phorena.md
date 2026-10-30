---
title: "FrameCache: styles and hlsearch matches reused between frames"
author: Phorena
date: 2026-10-30
branch: AI-editor
cost: { minutes: 2, tokens: 19042 }
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
- `render::StyleCache` was renamed `FrameCache`, and `RenderOptions::styles` became `RenderOptions::cache`. It caches the style table and the hlsearch matches over one shared byte copy.
- Each entry has its own key: styles by `StyleSource` (Markdown, or the SyntaxConfig), matches by the whole `Highlight` (needle + whole_word). When the bytes change, both entries are dropped. Changing one key leaves the other entry cached.
- The byte copy is kept while syntax, markdown or hlsearch is on, and dropped only when all three are off.
- Measured on 10 MiB Rust, 120x50, hlsearch on: unchanged-text frame 3.9 ms -> 0.79 ms; with syntax off, 2.2 ms -> 0.71 ms. What remains is mostly the memcmp.
## Watch out
- Any new input to `CellStyles::build` or `Highlight::matches` must go into that entry's key in `CachedFrame`.
