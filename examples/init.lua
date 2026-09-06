-- Sample Cano configuration.
-- Copy this file to ~/.config/cano/init.lua or pass it with --config.
-- Options omitted from setup() keep the editor's built-in defaults.

local cano = setup({
    syntax = true,       -- Enable syntax highlighting.
    relative = true,     -- Show relative line numbers.
    cursorline = true,   -- Underline the line the cursor is on.
    mouse = true,        -- Let Cano handle the mouse; false gives it back.
    indent = false,      -- false uses tabs; true uses one-space indentation.
    auto_indent = false, -- Retained for compatibility; currently a no-op.
    undo_size = false,   -- Retained for compatibility; currently a no-op.
})

-- The value returned by setup exposes Cano runtime functions. For example:
-- cano.exit(0, "Configuration loaded")

