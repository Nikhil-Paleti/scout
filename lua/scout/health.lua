local M = {}

function M.check()
  local h = vim.health
  local util = require("scout.util")
  h.start("scout")
  local bin = util.bin()
  if not bin then
    h.error("scout binary not found", {
      "Run `cargo build --release` in " .. util.plugin_root(),
      "or put `scout` on $PATH, or set `bin` in setup()",
    })
    return
  end
  local v = vim.fn.system({ bin, "version" })
  if vim.v.shell_error ~= 0 then
    h.error("`" .. bin .. " version` failed: " .. vim.trim(v))
    return
  end
  h.ok("binary: " .. bin .. " (" .. vim.trim(v) .. ")")
  local root = util.root()
  local out = vim.fn.system({ bin, "status", "--json", "--root", root })
  local ok, st = pcall(vim.json.decode, out)
  if not ok or type(st) ~= "table" then
    h.warn("could not read status for " .. root)
    return
  end
  if not st.indexed then
    if st.building then
      h.info(root .. " is being indexed")
    else
      h.warn(root .. " has no index", { "Run :ScoutIndex; until then LazyVim's grep keys stay on rg" })
    end
    return
  end
  h.ok(("%s: %d files, %d segments, symbols %s"):format(root, st.files or 0, st.segments or 0, st.symbols or "?"))
  if st.stale_commit then
    h.warn("index is from an older commit; the background updater catches up when it runs")
  end
  if st.daemon and st.daemon ~= vim.NIL then
    h.ok("background updater running")
  else
    h.info("background updater not running; it starts on the next save")
  end
  if not pcall(require, "snacks.picker") then
    h.info("snacks.nvim not found; results go to the quickfix list")
  end
end

return M
