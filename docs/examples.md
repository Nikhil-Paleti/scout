# Examples

Queries work the same in `scout`, `:ScoutLive` and the MCP `search` tool. Every operator: [syntax.md](syntax.md).

## Find a file

| Query | Finds |
|---|---|
| `trainer` | files named like trainer first, then lines that mention it |
| `file:_test\.go$` | Go test files |
| `file:^kernels/ lang:c++` | C++ files under kernels/ |
| `lang:python file:test` | Python files with test in the path |
| `scout files trnr` | trainer.py, trainer_test.py by fuzzy name (`:ScoutFiles`) |

## Find a folder

| Query | Finds |
|---|---|
| `scout dirs lidar trk` | perception/lidar/fusion/tracker from loose words (`<leader>fd`) |
| `scout dirs kern` | kernels |
| `scout dirs fusion -tracker` | perception/lidar/fusion, not the folder under it |

## Find a definition

| Query | Finds |
|---|---|
| `sym:Trainer` | where Trainer is defined |
| `function:^launch$` | the function named exactly launch |
| `class:Widget` | the Widget class |
| `sym:Draw kind:method` | methods named Draw |
| `sym:widget kind:target` | the Bazel target named widget |

## Find callers

| Query | Finds |
|---|---|
| `calls:launch` | calls to launch, with the function each is in |
| `calls:launch -file:test` | calls to launch, skipping test files |
| `calls:Draw lang:c++` | calls to Draw in C++ |

## Exact text

| Query | Finds |
|---|---|
| `"tid, _, _"` | that exact text |
| `"tid, _, _" lang:python` | that exact text in Python files |
| `"printf(\"draw"` | text with a quote in it |
| `hello\ from` | a space without quotes |
| `refs:Trainer` | Trainer as a whole word, case sensitive |

## Regular expressions

| Query | Finds |
|---|---|
| `def \w+\(self` | Python methods |
| `printf\("draw` | regex with escaped punctuation |
| `pcre:yes class Widget \{\s+public` | a match across lines |

## Combine terms

| Query | Finds |
|---|---|
| `Trainer launch` | files with both; lines with either |
| `TODO OR FIXME` | either one |
| `(TODO OR FIXME) lang:python` | either one, in Python |
| `Handle -file:_test` | handle, outside test files |
| `TODO OR FIXME -lang:python` | either one, outside Python |

## Narrow by path or language

| Query | Finds |
|---|---|
| `return file:^server/` | return under server/ |
| `render lang:ts` | render in TypeScript |
| `name lang:bazel` | name in BUILD and .bzl files |
| `content:trainer` | trainer in file contents only, not file names |

## Comments, strings, case

| Query | Finds |
|---|---|
| `comment:TODO` | TODO inside comments |
| `usage:launch lang:python` | launch in code, not in comments or strings |
| `case:yes Trainer` | Trainer, not trainer |
| `widget` | widget, Widget, WIDGET (case insensitive by default) |
