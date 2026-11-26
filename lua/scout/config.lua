local M = {}

M.defaults = {
  -- Path to the scout binary. nil: the plugin's own target/release/scout,
  -- then `scout` on $PATH.
  bin = nil,
  -- Send LazyVim's grep keys (<leader>/, <leader>sg, <leader>sw) to scout
  -- when the project has an index. Without an index they stay on rg.
  route_lazyvim_keys = true,
  -- Live search waits for this many characters.
  min_chars = 3,
  -- Most results a picker asks for.
  max_results = 2000,
  -- Glob patterns to keep out of the index on this machine, in gitignore
  -- syntax. Used by :ScoutIndex.
  exclude = {},
  -- Tell the background updater about saves, and re-check git state when
  -- Neovim regains focus.
  notify_on_save = true,
  reconcile_on_focus = true,
}

M.options = vim.deepcopy(M.defaults)

function M.setup(opts)
  M.options = vim.tbl_deep_extend("force", vim.deepcopy(M.defaults), opts or {})
  return M.options
end

return M
