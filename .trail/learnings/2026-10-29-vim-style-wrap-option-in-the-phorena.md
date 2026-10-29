---
title: Vim-style wrap option in the renderer
author: Phorena
date: 2026-10-29
branch: AI-editor
cost: { minutes: 8, tokens: 141339 }
touches:
  - PRD.md
  - README.md
  - docs/help/general
  - examples/init.lua
  - src/app.rs
  - src/command.rs
  - src/config.rs
  - src/explorer.rs
  - src/io.rs
  - src/main.rs
---

## Decided
- `wrap` is a ConfigVariable (on by default, also a Lua setup slot). The renderer gets it through `RenderOptions.wrap`.
- All drawing goes through `ScreenLine` lists built by `screen_lines()` in src/render.rs, for both wrap and nowrap. Line numbers, the cursor, `byte_at`, jump labels and cursorline all read from that list, so they cannot drift apart.
- Rows break every content_width columns. A wide char cut by the edge moves to the next row whole; a tab is split. A row exactly screen-wide gets an extra screen row only while the cursor sits at its end (`wrap_starts`).
- `Viewport.skip` is the number of screen rows of the first buffer row hidden above the top. It is only nonzero when the cursor's row is taller than the screen.
- `visible_rows(&Editor)` counts only rows that are fully on screen. If it counted a row cut off at the bottom, Ctrl-Y would place the cursor there and the follow logic would scroll the view back down.

## Tried and ruled out
- Slicing each segment as first_column = k*width: a wide char straddling the edge would blank out on both rows.

## Watch out
- The Lua slot-count test in config.rs (`a_metamethod...`) counts the setup slots. Bump it when you add a slot.
- Render tests use `options()` with wrap: false so the old horizontal-scroll tests stay valid. Use the `wrapped()` helper for wrap tests.
- Anything that sets `viewport.row` directly should reset `viewport.skip`.
