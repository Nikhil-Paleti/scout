#!/usr/bin/env python3
"""Compare scout search against ripgrep on a repo: latency and result parity.

usage: compare.py REPO [scout-binary]   (SCOUT_CACHE_DIR is honoured)
"""
import os, statistics, subprocess, sys, time

repo = sys.argv[1]
scout = sys.argv[2] if len(sys.argv) > 2 else "scout"
QUERIES = [
    "kmalloc_array",
    r"EXPORT_SYMBOL_GPL\(dma_",
    r"struct\s+file_operations\s+\w+\s*=",
    r"spin_lock_irqsave.*flags",
    "TODO|FIXME",
    r"\bcopy_from_user\b",
    "MODULE_AUTHOR",
    r"a.b",  # no usable trigrams: full scan fallback
]

def run(cmd):
    t = time.perf_counter()
    out = subprocess.run(cmd, cwd=repo, capture_output=True, stdin=subprocess.DEVNULL).stdout
    return time.perf_counter() - t, out

def lines(out):
    # scout prints one line per matching line; normalise rg to the same
    s = set()
    for l in out.decode("utf-8", "replace").splitlines():
        parts = l.split(":", 3)
        if len(parts) == 4:
            s.add((parts[0].removeprefix("./"), parts[1]))
    return s

print(f"{'query':40} {'scout p50':>10} {'rg p50':>8} {'speedup':>8} {'scout':>7} {'rg':>7}")
for q in QUERIES:
    sc = [scout, "search", "--max", "1000000", q]
    rg = ["rg", "--no-heading", "-n", "--column", "-S", "--hidden", "--max-filesize", "1M", "-g", "!.git", q, "."]
    run(sc); run(rg)  # warm
    st, so = zip(*[run(sc) for _ in range(5)])
    rt, ro = zip(*[run(rg) for _ in range(3)])
    a, b = lines(so[0]), lines(ro[0])
    sp, rp = statistics.median(st), statistics.median(rt)
    print(f"{q:40} {sp*1000:8.0f}ms {rp*1000:6.0f}ms {rp/sp:7.1f}x {len(a):7} {len(b):7}"
          + ("" if a == b else f"  diff: only-scout {len(a-b)} only-rg {len(b-a)}"))
