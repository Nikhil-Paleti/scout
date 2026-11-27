#!/bin/sh
# Runs every query in docs/examples.md and docs/syntax.md against the demo
# repo here and fails if any finds nothing. Run after `cargo build --release`:
#   sh tests/docs/check.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
bin=${SCOUT_BIN:-$top/target/release/scout}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp -r "$here/demo" "$tmp/demo"
cd "$tmp/demo"
git init -q . && git add . && git -c user.name=t -c user.email=t@t commit -qm demo
export SCOUT_CACHE_DIR="$tmp/cache" NO_COLOR=1
"$bin" build --quiet || exit 2
python3 - "$top" > "$tmp/queries" <<'PY'
import re, sys
top = sys.argv[1]
for f, col in (("docs/examples.md", 0), ("docs/syntax.md", 2)):
    for line in open(f"{top}/{f}"):
        if line.startswith("| `"):
            cell = [c.strip() for c in line.strip().strip("|").split("|")][col]
            for q in re.findall(r"`([^`]+)`", cell):
                print(q)
PY
fail=0
n=0
while IFS= read -r q; do
  n=$((n + 1))
  case "$q" in
    "scout files "*) "$bin" files ${q#scout files } > /dev/null 2>&1 ;;
    *) "$bin" -- "$q" > /dev/null 2>&1 ;;
  esac
  e=$?
  if [ $e -ne 0 ]; then
    echo "FAIL (exit $e): $q"
    fail=$((fail + 1))
  fi
done < "$tmp/queries"

# Queries typed straight on the command line, unquoted: a `-term` after the
# query is an exclusion, not a flag. Each line is the args, then a path the
# results must not contain.
while IFS='|' read -r args absent; do
  n=$((n + 1))
  # shellcheck disable=SC2086
  out=$("$bin" $args 2>&1)
  e=$?
  if [ $e -ne 0 ]; then
    echo "FAIL (exit $e): scout $args: $out"
    fail=$((fail + 1))
  elif printf '%s\n' "$out" | grep -q "$absent"; then
    echo "FAIL (has $absent): scout $args"
    fail=$((fail + 1))
  fi
done <<'CLI'
calls launch -file:test|trainer_test.py
search Handle -file:_test|handler_test.go
launch -lang:python|\.py:
sym Trainer -file:test|trainer_test.py
files handler -file:_test|handler_test.go
CLI
echo "$n queries, $fail failed"
[ $fail -eq 0 ]
