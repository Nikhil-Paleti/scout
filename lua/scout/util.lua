-- Small helpers that work on Neovim 0.9 and later.
local M = {}

M.uv = vim.uv or vim.loop

local plugin_root = (function()
  local src = debug.getinfo(1, "S").source:sub(2)
  return vim.fn.fnamemodify(src, ":p:h:h:h")
end)()

function M.plugin_root()
  return plugin_root
end

--- The scout binary, or nil if none is installed.
function M.bin()
  local cfg = require("scout.config").options
  if cfg.bin then
    return vim.fn.executable(cfg.bin) == 1 and cfg.bin or nil
  end
  local local_bin = plugin_root .. "/target/release/scout"
  if M.uv.fs_stat(local_bin) then
    return local_bin
  end
  if vim.fn.executable("scout") == 1 then
    return "scout"
  end
  return nil
end

--- Repository root for a buffer: the nearest directory with .git, or cwd.
function M.root(buf)
  buf = buf or 0
  local name = vim.api.nvim_buf_get_name(buf)
  local start = name ~= "" and vim.fs.dirname(name) or M.uv.cwd()
  local found = vim.fs.find(".git", { path = start, upward = true })[1]
  local root = found and vim.fs.dirname(found) or M.uv.cwd()
  return M.uv.fs_realpath(root) or root
end

--- Runs a command without blocking. on_exit(code, stdout, stderr) runs on
--- the main loop.
function M.run(cmd, opts, on_exit)
  opts = opts or {}
  if vim.system then
    return vim.system(cmd, { cwd = opts.cwd, text = true }, function(r)
      vim.schedule(function()
        on_exit(r.code, r.stdout or "", r.stderr or "")
      end)
    end)
  end
  local out, err = {}, {}
  local id = vim.fn.jobstart(cmd, {
    cwd = opts.cwd,
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = function(_, d)
      out = d
    end,
    on_stderr = function(_, d)
      err = d
    end,
    on_exit = function(_, code)
      on_exit(code, table.concat(out, "\n"), table.concat(err, "\n"))
    end,
  })
  if id <= 0 then
    on_exit(127, "", "could not start " .. cmd[1])
  end
  return id
end

--- Word under the cursor, or the visual selection.
function M.word_or_selection()
  local mode = vim.fn.mode()
  if mode == "v" or mode == "V" or mode == "\22" then
    vim.cmd('noautocmd normal! "vy')
    local s = vim.fn.getreg("v")
    return (s:gsub("\n.*", "")), false
  end
  return vim.fn.expand("<cword>"), true
end

--- Escapes a string for a regex query.
function M.escape(s)
  return (s:gsub("[%^%$%(%)%.%[%]%*%+%-%?%{%}|\\#&~]", "\\%0"))
end

return M
