-- Sample Cano configuration.
-- Copy this file to ~/.config/cano/init.lua or pass it with --config.
-- Options omitted from setup() keep the editor's built-in defaults.

local cano = setup({
    syntax = true,       -- Enable syntax highlighting.
    relative = true,     -- Show relative line numbers.
    cursorline = true,   -- Underline the line the cursor is on.
    mouse = true,        -- Let Cano handle the mouse; false gives it back.
    backup = true,       -- Copy what each save overwrites into .backups/.
    list = false,        -- Show invisible characters (:set listchars? for the set).
    indent = false,      -- false uses tabs; true uses one-space indentation.
    auto_indent = false, -- Retained for compatibility; currently a no-op.
    undo_size = false,   -- Retained for compatibility; currently a no-op.
})

-- The value returned by setup exposes Cano runtime functions.
--
-- cano.command runs any line you could type at the `:` prompt, so options
-- without a slot above -- and mappings -- are configured with it. A leading
-- `:` is optional. Use a [[long string]] so backslashes survive: `:set`
-- splits its arguments on spaces, and the one in `tab:>\ ` has to be escaped
-- to stay part of the value, exactly as it would in a vimrc.
--
-- cano.command([[set listchars=tab:> ,trail:.,eol:$]])
-- cano.command("imap ;; <Esc>")
-- cano.command("set cursorline nomouse")
-- cano.command("set noautoformat_retab")   -- a Makefile needs its tabs
--
-- cano.exit(0, "Configuration loaded")

