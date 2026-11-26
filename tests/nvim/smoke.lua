-- Headless smoke test of the Neovim plugin without snacks (quickfix path).
-- Run from the repo after `cargo build --release`:
--   nvim --headless -u NONE -c "luafile tests/nvim/smoke.lua"
-- It makes a throwaway repo and index cache under a temp dir.
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h:h")
vim.opt.rtp:prepend(here)
vim.cmd("runtime plugin/scout.lua")

local tmp = vim.fn.tempname()
local repo = tmp .. "/repo"
vim.fn.mkdir(repo .. "/src", "p")
vim.fn.writefile({ "int hello_world(void) { return 1; }", "int main() { return hello_world(); }" }, repo .. "/src/a.c")
vim.fn.system({ "git", "init", "-q", repo })
vim.env.SCOUT_CACHE_DIR = tmp .. "/cache"
vim.cmd("cd " .. repo)
local failures = 0
local function check(cond, msg)
  if cond then
    io.stdout:write("ok   " .. msg .. "\n")
  else
    failures = failures + 1
    io.stdout:write("FAIL " .. msg .. "\n")
  end
end
local function wait(ms, pred)
  return vim.wait(ms, pred, 20)
end

require("scout").setup({ route_lazyvim_keys = false })
local util = require("scout.util")
check(util.bin() ~= nil, "binary found: " .. tostring(util.bin()))
vim.fn.system({ util.bin(), "build", "--quiet" })
check(vim.v.shell_error == 0, "scout build")

-- :Scout fills the quickfix list.
vim.cmd("Scout hello_world")
check(wait(5000, function() return #vim.fn.getqflist() > 0 end), ":Scout fills quickfix")
local qf = vim.fn.getqflist()
check(#qf == 2, "two hits, got " .. #qf)
check(qf[1] and qf[1].lnum >= 1 and qf[1].col >= 1, "positions set")

-- :ScoutSym on the cursor word.
vim.cmd("cclose")
vim.cmd("edit src/a.c")
vim.api.nvim_win_set_cursor(0, { 2, 25 })
vim.fn.setqflist({}, "r")
vim.cmd("ScoutSym")
check(wait(5000, function() return #vim.fn.getqflist() > 0 end), ":ScoutSym finds the definition")
local s = vim.fn.getqflist()[1]
check(s and s.lnum == 1 and s.text:find("%[function%]"), "definition on line 1, tagged [function]")

vim.fn.setqflist({}, "r")
vim.cmd("ScoutCallers hello_world")
check(wait(5000, function() return #vim.fn.getqflist() > 0 end), ":ScoutCallers finds the call")
local c = vim.fn.getqflist()[1]
check(c and c.lnum == 2 and c.text:find("call in main"), "call on line 2, in main")

-- A save reaches the daemon and shows up in search.
local daemon = require("scout.daemon")
local root = util.root()
check(wait(5000, function() return daemon.indexed(root) end), "plugin sees the index")
vim.cmd("cclose")
vim.cmd("edit src/a.c")
vim.api.nvim_buf_set_lines(0, -1, -1, false, { "int saved_marker_qq(void) { return 2; }" })
vim.cmd("write")
local found = wait(8000, function()
  vim.fn.setqflist({}, "r")
  vim.cmd("Scout saved_marker_qq")
  return wait(1500, function() return #vim.fn.getqflist() > 0 end)
end)
check(found, "saved text is searchable")

-- Health check runs without errors.
local ok, err = pcall(vim.cmd, "checkhealth scout")
check(ok, "checkhealth scout runs " .. tostring(err or ""))
local health = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n")
check(health:find("binary:") ~= nil, "health reports the binary")

vim.fn.system({ util.bin(), "stop" })
print(failures == 0 and "ALL PASSED" or (failures .. " FAILED"))
vim.cmd((failures == 0 and "qa!" or "cq!"))
