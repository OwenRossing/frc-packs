#!/usr/bin/env python3
"""Inline the team data and robot photos into src/app.html and write index.html (self-contained)."""
import base64, json, pathlib
root = pathlib.Path(__file__).parent
data = (root.parent / "data" / "cmp-2026.txt").read_text(encoding="utf-8")
assert "`" not in data and "${" not in data and "\\" not in data, "data contains characters that break the template literal"
page = (root / "src" / "app.html").read_text(encoding="utf-8")
assert "/*TEAMS*/" in page and "/*PHOTOS*/{}" in page
photos = {int(f.stem): base64.b64encode(f.read_bytes()).decode() for f in sorted((root.parent / "data" / "photos").glob("*.webp"))}
page = page.replace("/*TEAMS*/", data.strip()).replace("/*PHOTOS*/{}", json.dumps(photos, separators=(",", ":")))
(root / "index.html").write_text(page, encoding="utf-8")
print("wrote index.html", len(page), "bytes,", len(photos), "photos")
