-- snacks.picker sources for scout: live search, a fixed query, symbols
-- and callers.
local search = require("scout.search")
local util = require("scout.util")

local M = {}

function M.available()
  return pcall(require, "snacks.picker")
end

local function finder(root, get_query, flags)
  return function(opts, ctx)
    local cfg = require("scout.config").options
    local query = get_query(ctx)
    if not query or query == "" or (opts.live and #query < cfg.min_chars) then
      return function() end
    end
    flags = flags or {}
    local cmd = search.cmd(query, { root = root, fixed = flags.fixed, word = flags.word, near = opts.near })
    if not cmd then
      return function() end
    end
    local proc = {
      cmd = cmd[1],
      args = vim.list_slice(cmd, 2),
      cwd = root,
      notify = false,
      transform = function(item)
        local h = search.decode(item.text)
        if not h then
          return false
        end
        item.cwd = root
        item.file = h.path
        item.pos = { h.line, h.col - 1 }
        local note = search.note(h)
        item.line = note and (h.text .. "  " .. note) or h.text
        item.text = h.path .. ":" .. h.line .. ":" .. h.text
        -- Match highlights: 1-based byte positions within the line.
        local positions = {}
        for _, r in ipairs(h.ranges or {}) do
          for i = r[1] + 1, r[2] do
            positions[#positions + 1] = i
          end
        end
        if #positions > 0 then
          item.positions = positions
          item.end_pos = { h.line, h.ranges[1][2] }
        end
      end,
    }
    -- snacks merges picker options into the process config with ctx:opts();
    -- older releases took a list of tables instead.
    local cfg_proc = ctx.opts and ctx:opts(proc) or { opts, proc }
    return require("snacks.picker.source.proc").proc(cfg_proc, ctx)
  end
end

local function near(root)
  local name = vim.api.nvim_buf_get_name(0)
  if name ~= "" and name:sub(1, #root + 1) == root .. "/" then
    return name:sub(#root + 2)
  end
end

--- Live search: results update as you type. opts.search pre-fills it.
function M.live(opts)
  opts = opts or {}
  local root = opts.root or util.root()
  return require("snacks").picker.pick({
    source = "scout",
    title = "Scout",
    live = true,
    supports_live = true,
    search = opts.search,
    near = near(root),
    format = "file",
    show_empty = true,
    finder = finder(root, function(ctx)
      return ctx.filter.search
    end),
  })
end

--- One query, results filtered by fuzzy matching as you type.
function M.query(query, opts)
  opts = opts or {}
  local root = opts.root or util.root()
  return require("snacks").picker.pick({
    source = "scout",
    title = opts.title or ("Scout " .. query),
    near = near(root),
    format = "file",
    finder = finder(root, function()
      return query
    end, opts),
  })
end

return M
