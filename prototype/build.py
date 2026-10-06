#!/usr/bin/env python3
"""Inline the team data into src/app.html and write index.html (self-contained)."""
import pathlib
root = pathlib.Path(__file__).parent
data = (root / "data" / "cmp-2026.txt").read_text(encoding="utf-8")
assert "`" not in data and "${" not in data and "\\" not in data, "data contains characters that break the template literal"
page = (root / "src" / "app.html").read_text(encoding="utf-8")
assert "/*TEAMS*/" in page
(root / "index.html").write_text(page.replace("/*TEAMS*/", data.strip()), encoding="utf-8")
print("wrote index.html", len(page) + len(data))
