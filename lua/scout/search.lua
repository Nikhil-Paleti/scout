-- Builds `scout search` command lines and fills the quickfix list.
local util = require("scout.util")

local M = {}

--- Command for a query. opts: root, fixed (a literal string, no
--- filters), word (whole word only), near (current file), max.
function M.cmd(query, opts)
  opts = opts or {}
  local cfg = require("scout.config").options
  local bin = util.bin()
  if not bin then
    return nil
  end
  local cmd = { bin, "search", "--json", "--color", "never", "--root", opts.root or util.root() }
  vim.list_extend(cmd, { "--max", tostring(opts.max or cfg.max_results) })
  if opts.near and opts.near ~= "" then
    vim.list_extend(cmd, { "--near", opts.near })
  end
  if opts.fixed then
    vim.list_extend(cmd, { "-F", "--no-filters" })
  end
  if opts.word then
    table.insert(cmd, "-w")
  end
  vim.list_extend(cmd, { "--", query })
  return cmd
end

--- Decodes one `--json` line. Returns the hit table, or nil for the
--- summary and anything unparsable.
function M.decode(line)
  if line == nil or line == "" then
    return nil
  end
  local ok, v = pcall(vim.json.decode, line)
  if not ok or type(v) ~= "table" or v.type ~= "hit" then
    return nil, ok and v or nil
  end
  return v
end

--- A short label for symbol hits: "[function]", "[call in main]".
function M.note(h)
  if not h.kind or h.kind == "text" then
    return nil
  end
  if h.kind == "call" and h["in"] and h["in"] ~= vim.NIL then
    return "[call in " .. h["in"] .. "]"
  end
  return "[" .. h.kind .. "]"
end

--- Runs a query and puts the results in the quickfix list.
function M.quickfix(query, opts)
  opts = opts or {}
  local root = opts.root or util.root()
  opts.root = root
  local cmd = M.cmd(query, opts)
  if not cmd then
    vim.notify("scout: binary not found; build it with `cargo build --release`", vim.log.levels.ERROR)
    return
  end
  util.run(cmd, {}, function(code, out, err)
    if code == 2 then
      vim.notify("scout: " .. vim.trim(err), vim.log.levels.ERROR)
      return
    end
    local items, summary = {}, nil
    for _, line in ipairs(vim.split(out, "\n", { trimempty = true })) do
      local h, other = M.decode(line)
      if h then
        local note = M.note(h)
        items[#items + 1] = {
          filename = root .. "/" .. h.path,
          lnum = h.line,
          col = h.col,
          text = note and (h.text .. "  " .. note) or h.text,
        }
      elseif other then
        summary = other
      end
    end
    vim.fn.setqflist({}, " ", { title = "scout " .. query, items = items })
    if #items == 0 then
      vim.notify("scout: no results for " .. query)
      return
    end
    vim.cmd("botright copen")
    if summary and summary.truncated then
      vim.notify(("scout: showing the first %d results"):format(#items))
    end
    if summary and summary.index_status == "building" then
      vim.notify("scout: no index yet, so these came from a file scan; indexing in the background")
      require("scout.daemon").when_indexed(root, function() end)
    end
  end)
end

return M
