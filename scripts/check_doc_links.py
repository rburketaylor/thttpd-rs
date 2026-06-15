#!/usr/bin/env python3
"""Validate relative Markdown links in docs/ resolve to real files."""
import os
import re
import sys

LINK_RE = re.compile(r'\[([^\]]+)\]\(([^)]+)\)')
DOCS_DIR = os.path.join(os.path.dirname(__file__), "..", "docs")
REPO_ROOT = os.path.abspath(os.path.join(DOCS_DIR, ".."))

# Scan docs/ + the top-level README.
MD_FILES = [os.path.join(DOCS_DIR, n) for n in sorted(os.listdir(DOCS_DIR)) if n.endswith(".md")]
MD_FILES.append(os.path.join(REPO_ROOT, "README.md"))

errors = []
checked = 0
for path in MD_FILES:
    name = os.path.relpath(path, REPO_ROOT)
    text = open(path).read()
    for label, target in LINK_RE.findall(text):
        # Skip external links and anchors
        if target.startswith(("http://", "https://", "#")):
            continue
        # Strip any anchor
        rel = target.split("#")[0]
        if not rel:
            continue
        # Resolve relative to repo root (links use docs/... from README) or docs dir
        for base in (REPO_ROOT, DOCS_DIR):
            candidate = os.path.normpath(os.path.join(base, rel))
            if os.path.exists(candidate):
                break
        else:
            errors.append(f"{name}: broken link [{label}]({target})")
        checked += 1

print(f"checked {checked} relative links across docs/")
if errors:
    for e in errors:
        print("  BROKEN:", e)
    sys.exit(1)
print("all doc links resolve")
