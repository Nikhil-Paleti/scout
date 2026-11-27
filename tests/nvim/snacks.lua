-- Headless test of the snacks pickers. Needs a snacks.nvim checkout:
--   SNACKS_DIR=/path/to/snacks.nvim nvim --headless -u NONE -c "luafile tests/nvim/snacks.lua"
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h:h")
assert(vim.env.SNACKS_DIR, "set SNACKS_DIR to a snacks.nvim checkout")
vim.opt.rtp:prepend(vim.env.SNACKS_DIR)
vim.opt.rtp:prepend(here)
vim.cmd("runtime plugin/scout.lua")

local uv = vim.uv or vim.loop
local tmp = vim.fn.tempname()
local repo = tmp .. "/repo"
vim.fn.mkdir(repo .. "/src/widget", "p")
vim.fn.writefile({ "int hello_world(void) { return 1; }", "int main() { return hello_world(); }" }, repo .. "/src/a.c")
vim.fn.writefile({ "void draw_widget(void) {}", "// hello_world lives in a.c" }, repo .. "/src/widget/draw.c")
vim.fn.writefile({ "hello_world notes" }, repo .. "/README.md")
vim.fn.system({ "git", "init", "-q", repo })
repo = uv.fs_realpath(repo)
vim.env.SCOUT_CACHE_DIR = tmp .. "/cache"
vim.cmd("cd " .. repo)

local failures = 0
local function check(cond, msg)
  if not cond then
    failures = failures + 1
  end
  io.stdout:write((cond and "ok   " or "FAIL ") .. msg .. "\n")
end

require("snacks").setup({ picker = { enabled = true } })
require("scout").setup({ route_lazyvim_keys = false, index_on_start = false })
local util = require("scout.util")
vim.fn.system({ util.bin(), "build", "--quiet" })
check(vim.v.shell_error == 0, "scout build")
local pickers = require("scout.pickers.snacks")
check(pickers.available(), "snacks picker available")

-- Waits for the picker's finder to finish; returns its (filtered) items.
local function settle(p)
  vim.wait(5000, function()
    return not p:is_active() and #p:items() > 0
  end, 20)
  vim.wait(200)
  return p:items()
end
local function files_of(items)
  local out = {}
  for _, it in ipairs(items) do
    out[#out + 1] = it.file .. (it.pos and (":" .. it.pos[1]) or " [file]")
  end
  return out
end
local function has(list, v)
  return vim.tbl_contains(list, v)
end

-- :ScoutFiles: every path, fuzzy matched by snacks.
local p = pickers.files()
local items = settle(p)
local paths = files_of(items)
check(#items == 3, "files picker lists 3 paths, got " .. #items)
check(has(paths, "src/widget/draw.c [file]"), "files picker has src/widget/draw.c")
p:close()
p = pickers.files({ search = "wdgdraw" })
items = settle(p)
check(#items == 1 and items[1].file == "src/widget/draw.c", "pattern wdgdraw keeps draw.c, got " .. table.concat(files_of(items), ", "))
p:action("confirm")
vim.wait(1000, function() return vim.api.nvim_buf_get_name(0):find("draw%.c$") ~= nil end, 20)
check(vim.api.nvim_buf_get_name(0) == repo .. "/src/widget/draw.c", "confirm opens draw.c")

-- Live search: file-name hits first, then lines with positions.
vim.cmd("edit " .. repo .. "/src/a.c")
p = pickers.live({ search = "widget" })
items = settle(p)
paths = files_of(items)
check(paths[1] == "src/widget/draw.c [file]", "live: file-name hit first, got " .. tostring(paths[1]))
check(has(paths, "src/widget/draw.c:1"), "live: line hit with a position")
p:close()

p = pickers.live({ search = "hello_world" })
items = settle(p)
paths = files_of(items)
check(has(paths, "src/a.c:1") and has(paths, "src/a.c:2") and has(paths, "README.md:1"), "live: hello_world lines, got " .. table.concat(paths, ", "))
local hit
for _, it in ipairs(items) do
  if it.file == "src/a.c" and it.pos[1] == 2 then
    hit = it
  end
end
check(hit and hit.pos[2] == 20 and hit.positions and #hit.positions == 11, "live: column and match highlight")
p:close()

-- :ScoutDir limits to the current file's directory.
vim.cmd("edit " .. repo .. "/src/widget/draw.c")
p = pickers.live({ dir = "src/widget", search = "hello_world" })
items = settle(p)
paths = files_of(items)
check(#paths == 1 and paths[1] == "src/widget/draw.c:2", "dir search: only src/widget, got " .. table.concat(paths, ", "))
p:close()

-- One fixed query (definitions), and jumping to a hit.
p = pickers.query("sym:hello_world")
items = settle(p)
paths = files_of(items)
check(#paths == 1 and paths[1] == "src/a.c:1", "sym: one definition, got " .. table.concat(paths, ", "))
p:action("confirm")
vim.wait(1000, function() return vim.api.nvim_buf_get_name(0):find("a%.c$") ~= nil end, 20)
check(vim.api.nvim_buf_get_name(0) == repo .. "/src/a.c" and vim.api.nvim_win_get_cursor(0)[1] == 1, "confirm jumps to src/a.c:1")

-- Nothing typed: changed files (newest first), untracked, recent, then the
-- current file's directory, with the file previewed and a syntax hint.
local r2 = tmp .. "/repo2"
local function put(rel, lines)
  vim.fn.mkdir(vim.fn.fnamemodify(r2 .. "/" .. rel, ":h"), "p")
  vim.fn.writefile(lines, r2 .. "/" .. rel)
end
for _, f in ipairs({ "lib/core.c", "lib/util.c", "lib/extra.c", "lib/zeta.c", "docs/guide.md", "main.c" }) do
  put(f, { "// " .. f })
end
vim.fn.system({ "git", "init", "-q", r2 })
vim.fn.system({ "git", "-C", r2, "add", "." })
vim.fn.system({ "git", "-C", r2, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init" })
put("lib/util.c", { "// util changed" })
put("docs/guide.md", { "# Guide, changed" })
put("lib/fresh.c", { "// untracked" })
local now = os.time()
uv.fs_utime(r2 .. "/docs/guide.md", now, now)
uv.fs_utime(r2 .. "/lib/util.c", now - 60, now - 60)
uv.fs_utime(r2 .. "/lib/fresh.c", now - 120, now - 120)
r2 = uv.fs_realpath(r2)
vim.cmd("cd " .. r2)
vim.fn.system({ util.bin(), "build", "--quiet" })
vim.cmd("%bwipeout!")
vim.cmd("edit " .. r2 .. "/lib/extra.c")
vim.cmd("edit " .. r2 .. "/lib/core.c")
vim.v.oldfiles = { r2 .. "/main.c" }

local function labels(items)
  local out = {}
  for _, it in ipairs(items) do
    out[#out + 1] = it.file .. (it.line and (" " .. it.line) or "")
  end
  return out
end
local expect = {
  "docs/guide.md [changed]",
  "lib/util.c [changed]",
  "lib/fresh.c [new]",
  "lib/extra.c [recent]",
  "main.c [recent]",
  "lib/zeta.c",
}
local t0 = uv.hrtime()
p = pickers.live()
check((uv.hrtime() - t0) / 1e6 < 100, "empty live picker opens at once")
local ok = vim.wait(5000, function()
  return #p:items() > 0 and p:items()[1].file == "docs/guide.md"
end, 20)
items = p:items()
check(ok and vim.deep_equal(labels(items), expect), "empty query lists changed, new, recent, nearby: " .. table.concat(labels(items), ", "))
local pbuf = p.preview and p.preview.win and p.preview.win.buf
check(vim.wait(2000, function()
  return pbuf and vim.api.nvim_buf_is_valid(pbuf) and (vim.api.nvim_buf_get_lines(pbuf, 0, 1, false)[1] or "") == "# Guide, changed"
end, 20), "preview shows the highlighted file")
local ibuf = p.input.win.buf
local function hint_shown()
  vim.api.nvim_exec_autocmds("TextChanged", { buffer = ibuf })
  local marks = vim.api.nvim_buf_get_extmarks(ibuf, vim.api.nvim_create_namespace("scout.hint"), 0, -1, { details = true })
  return #marks == 1 and marks[1][4].virt_text[1][1]:find("sym:", 1, true) ~= nil
end
check(hint_shown(), "syntax hint shows in the empty prompt")
p.input:set(nil, "util changed")
p:find()
check(vim.wait(5000, function()
  local it = p:items()[1]
  return it and it.pos ~= nil
end, 20), "typing switches to search")
items = p:items()
check(#items == 1 and items[1].file == "lib/util.c" and items[1].pos[1] == 1, "search hit for the typed query")
check(not hint_shown(), "hint hides while typing")
p.input:set(nil, "")
p:find()
check(vim.wait(5000, function()
  return vim.deep_equal(labels(p:items()), expect)
end, 20), "clearing the query brings the list back")
p:action("confirm")
vim.wait(1000, function() return vim.api.nvim_buf_get_name(0):find("guide%.md$") ~= nil end, 20)
check(vim.api.nvim_buf_get_name(0) == r2 .. "/docs/guide.md", "Enter opens the first file")

-- :ScoutDir keeps to its directory.
vim.cmd("edit " .. r2 .. "/lib/core.c")
p = pickers.live({ dir = "lib" })
vim.wait(5000, function() return #p:items() >= 4 end, 20)
check(vim.deep_equal(labels(p:items()), { "lib/util.c [changed]", "lib/fresh.c [new]", "lib/extra.c [recent]", "lib/zeta.c" }), "dir scope: " .. table.concat(labels(p:items()), ", "))
p:close()

-- :ScoutFiles: the same list first, then every other path once.
p = pickers.files()
vim.wait(5000, function() return not p:is_active() and #p:items() >= 7 end, 20)
items = p:items()
local first = vim.list_slice(labels(items), 1, #expect)
check(vim.deep_equal(first, expect), "files picker starts with the list: " .. table.concat(first, ", "))
local seen_files, dupes = {}, 0
for _, it in ipairs(items) do
  dupes = dupes + (seen_files[it.file] and 1 or 0)
  seen_files[it.file] = true
end
check(#items == 7 and dupes == 0, "files picker has every path once, got " .. #items)
p:close()

-- With the hint turned off, nothing is drawn.
require("scout.config").options.syntax_hint = false
p = pickers.live()
vim.wait(3000, function()
  local b = p.input.win.buf
  return #p:items() > 0 and b and vim.api.nvim_buf_is_valid(b)
end, 20)
ibuf = p.input.win.buf
check(ibuf and #vim.api.nvim_buf_get_extmarks(ibuf, vim.api.nvim_create_namespace("scout.hint"), 0, -1, {}) == 0, "syntax_hint = false hides the hint")
p:close()
vim.fn.system({ util.bin(), "stop", "--root", r2 })
vim.fn.system({ util.bin(), "stop", "--root", repo })
print(failures == 0 and "ALL PASSED" or (failures .. " FAILED"))
vim.cmd((failures == 0 and "qa!" or "cq!"))
