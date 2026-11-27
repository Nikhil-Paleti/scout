# Syntax

Google Code Search syntax. Terms are regexes, case insensitive, and all must match in the file.

| Operator | Meaning | Example |
|---|---|---|
| `term` | regex in file contents or path | `launch` |
| `a b` | both in the file (implicit AND) | `Trainer launch` |
| `a OR b` | either; binds looser than AND | `TODO OR FIXME` |
| `( )` | group | `(TODO OR FIXME) lang:python` |
| `-term` | exclude files that match | `Handle -file:_test` |
| `"text"` | literal text; `\"` for a quote | `"tid, _, _"` |
| `\` | escape one character | `hello\ from`, `main\.py` |
| `content:` | contents only, not paths | `content:trainer` |
| `file:` | path regex (`filepath:`, `path:`, `f:`) | `file:^kernels/` |
| `lang:` | language (`language:`) | `lang:c++`, `lang:python`, `lang:bazel` |
| `case:yes` | case sensitive (`no`, `auto`) for its group | `case:yes Trainer` |
| `comment:` | inside comments | `comment:TODO` |
| `usage:` | outside comments and strings | `usage:launch` |
| `pcre:yes` | regexes match across lines | `pcre:yes class Widget \{\s+public` |
| `sym:` | definitions by name (`symbol:`) | `sym:Trainer` |
| `function:` | function definitions (`func:`) | `function:^launch$` |
| `class:` | class definitions | `class:Widget` |
| `kind:` | symbol kind, with `sym:` or `calls:` | `sym:widget kind:target` |
| `calls:` | call sites, with the enclosing function | `calls:launch` |
| `refs:` | whole word, case sensitive | `refs:Trainer` |

A query of only `file:` or `lang:` terms lists files. Symbol names are matched by name, not type: same-named symbols mix. Kinds: function, method, class, struct, enum, type, interface, macro, constant, target, prototype.
