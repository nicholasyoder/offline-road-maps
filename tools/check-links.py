#!/usr/bin/env python3
"""Lists relative links in the repository's Markdown files whose target file is missing
(anchors are not checked). Exit 1 if there are any.   tools/check-links.py"""
import os, re, subprocess, sys
root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip()
files = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.md"], cwd=root, capture_output=True, text=True, check=True).stdout.split()
bad = 0
for f in files:
    p = os.path.join(root, f)
    if not os.path.exists(p):
        continue
    for n, line in enumerate(open(p, encoding="utf-8"), 1):
        for m in re.finditer(r"\]\(([^)\s#]+)(?:#[^)\s]*)?\)", line):
            t = m.group(1)
            if re.match(r"[a-z]+:", t) or t.startswith("/"):
                continue
            if not os.path.exists(os.path.normpath(os.path.join(os.path.dirname(p), t))):
                print(f"{f}:{n}: {t}")
                bad += 1
sys.exit(1 if bad else 0)
