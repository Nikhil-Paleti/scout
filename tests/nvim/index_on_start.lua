-- Headless test: the indexer starts on VimEnter and on :cd into a repo,
-- and skips folders outside git and $HOME. Run after `cargo build --release`:
--   nvim --headless -u NONE -c "luafile tests/nvim/index_on_start.lua"
-- and as a late (VeryLazy) load, when VimEnter has already fired:
--   nvim --headless -u NONE -c "au VimEnter * ++once ++nested luafile tests/nvim/index_on_start.lua"
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h:h")
vim.opt.rtp:prepend(here)
vim.cmd("runtime plugin/scout.lua")

local uv = vim.uv or vim.loop
local tmp = vim.fn.tempname()
vim.env.SCOUT_CACHE_DIR = tmp .. "/cache"
local function repo(name, git)
  local d = tmp .. "/" .. name
  vim.fn.mkdir(d .. "/src", "p")
  vim.fn.writefile({ "int marker_" .. name .. "(void) { return 1; }" }, d .. "/src/a.c")
  if git then
    vim.fn.system({ "git", "init", "-q", d })
  end
  return uv.fs_realpath(d)
end
local a, b, plain, home = repo("a", true), repo("b", true), repo("plain", false), repo("home", true)
vim.env.HOME = home

local failures = 0
local function check(cond, msg)
  if not cond then
    failures = failures + 1
  end
  io.stdout:write((cond and "ok   " or "FAIL ") .. msg .. "\n")
end

local util = require("scout.util")
local bin
local function indexed(root)
  local out = vim.fn.system({ bin, "status", "--json", "--root", root })
  local ok, v = pcall(vim.json.decode, out)
  return ok and type(v) == "table" and v.indexed == true
end

vim.cmd("cd " .. a)
local t0 = uv.hrtime()
require("scout").setup({ route_lazyvim_keys = false })
bin = util.bin()
check(bin ~= nil, "binary found")
check(not indexed(a), "no index before VimEnter")
local late = vim.v.vim_did_enter == 1
if not late then
  vim.api.nvim_exec_autocmds("VimEnter", { group = "scout" })
end
check((uv.hrtime() - t0) / 1e6 < 50, "setup and VimEnter return at once")
check(vim.wait(10000, function() return indexed(a) end, 100), (late and "a late setup" or "VimEnter") .. " indexes the repo")

vim.cmd("cd " .. b)
check(vim.wait(10000, function() return indexed(b) end, 100), ":cd into another repo indexes it")

vim.cmd("cd " .. plain)
vim.wait(1500)
check(not indexed(plain), "a folder outside git is skipped")
vim.cmd("cd " .. home)
vim.wait(1500)
check(not indexed(home), "$HOME is skipped")

-- index_on_start = false starts nothing.
local c = repo("c", true)
require("scout").setup({ route_lazyvim_keys = false, index_on_start = false })
vim.cmd("cd " .. c)
vim.api.nvim_exec_autocmds("VimEnter", { group = "scout" })
vim.wait(1500)
check(not indexed(c), "index_on_start = false leaves it alone")

for _, r in ipairs({ a, b }) do
  vim.fn.system({ bin, "stop", "--root", r })
end
print(failures == 0 and "ALL PASSED" or (failures .. " FAILED"))
vim.cmd((failures == 0 and "qa!" or "cq!"))
