#!/usr/bin/env python3
"""Check repository-local Markdown file links and heading anchors."""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = (
    list(ROOT.glob("*.md"))
    + list((ROOT / "docs").glob("*.md"))
    + list((ROOT / "adapters").glob("*/README.md"))
)


def slug(text):
    text = re.sub(r"[`*_]", "", text.strip().lower())
    text = re.sub(r"[^a-z0-9\s-]", "", text)
    return re.sub(r"[\s-]+", "-", text).strip("-")


anchors = {}
for path in FILES:
    seen = {}
    values = set()
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        match = re.match(r"^#{1,6}\s+(.+?)\s*$", line)
        if not match:
            continue
        base = slug(match.group(1))
        number = seen.get(base, 0)
        seen[base] = number + 1
        values.add(base if number == 0 else f"{base}-{number}")
    anchors[path.resolve()] = values

problems = []
pattern = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+['\"][^'\"]*['\"])?\)")
for source in FILES:
    for line_number, line in enumerate(source.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        for match in pattern.finditer(line):
            link = match.group(1)
            if re.match(r"^[a-z]+://", link) or link.startswith("mailto:"):
                continue
            target_text, _, fragment = link.partition("#")
            target = (source.parent / target_text).resolve() if target_text else source.resolve()
            if not target.exists():
                problems.append(f"{source.relative_to(ROOT)}:{line_number}: missing file {link}")
            elif fragment and target.suffix.lower() == ".md" and fragment not in anchors.get(target, set()):
                problems.append(f"{source.relative_to(ROOT)}:{line_number}: missing anchor {link}")

print("\n".join(problems) if problems else "all local Markdown links valid")
sys.exit(1 if problems else 0)
