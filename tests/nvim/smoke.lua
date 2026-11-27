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

-- File-name hits come first, marked [file]; a file:-only query lists files.
vim.fn.setqflist({}, "r")
vim.cmd([[Scout file:a\.c]])
check(wait(5000, function() return #vim.fn.getqflist() > 0 end), ":Scout file: fills quickfix")
local fq = vim.fn.getqflist()
check(#fq == 1 and fq[1].lnum == 1 and fq[1].text == "[file]", "one file hit tagged [file]")
vim.fn.setqflist({}, "r")
vim.cmd("Scout hello_world OR nomatch_zz")
check(wait(5000, function() return #vim.fn.getqflist() > 0 end), "OR query works")
check(#vim.fn.getqflist() == 2, "OR query: two hits")

-- :ScoutFiles without snacks: prompt result goes to vim.ui.select.
local picked
local orig_select = vim.ui.select
vim.ui.select = function(items, _, cb)
  picked = items
  cb(nil)
end
vim.cmd("ScoutFiles sac")
check(wait(5000, function() return picked ~= nil end), ":ScoutFiles lists matches")
check(picked and #picked == 1 and picked[1] == "src/a.c", "fuzzy file match src/a.c")
-- :ScoutFolders without snacks: pick a folder, it opens with :edit.
picked = nil
vim.ui.select = function(items, _, cb)
  picked = items
  cb(items[1])
end
vim.cmd("ScoutFolders sr")
check(wait(5000, function() return picked ~= nil end), ":ScoutFolders lists matches")
check(picked and picked[1] == "src", "folder match src: " .. vim.inspect(picked))
check(wait(2000, function() return vim.api.nvim_buf_get_name(0):match("/src$") ~= nil end), "picked folder is opened")
check(vim.fn.maparg("<leader>fd", "n", false, true).desc == "Find folder (scout)", "<leader>fd mapped without snacks")
vim.cmd("enew")
vim.ui.select = orig_select
check(require("scout.search").dir_filter("src/x y.d") == [[file:^src/x\ y\.d/ ]], "dir filter escapes")

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
