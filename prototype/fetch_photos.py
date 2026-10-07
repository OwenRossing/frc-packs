#!/usr/bin/env python3
"""Download one robot photo per Champs team from The Blue Alliance and save a small WebP thumbnail.

Run: TBA_KEY=... python3 fetch_photos.py
Writes data/photos/<team>.webp and data/photos.txt (team|year|source url). Teams already in photos.txt are skipped
unless --refresh is passed. Picks the newest season with a photo, preferring TBA's "preferred" imgur photos.
"""
import concurrent.futures, io, json, os, pathlib, sys, urllib.request
from PIL import Image, ImageOps

root = pathlib.Path(__file__).parent
KEY = os.environ.get("TBA_KEY") or sys.exit("set TBA_KEY")
YEARS = range(2026, 2015, -1)
W, H, Q = 240, 180, 58  # 4:3 thumbnail; cards crop it with object-fit
out_dir = root / "data" / "photos"; out_dir.mkdir(parents=True, exist_ok=True)
index_path = root / "data" / "photos.txt"

def get(url, headers=None, timeout=30):
    req = urllib.request.Request(url, headers={"User-Agent": "frc-packs-prototype", **(headers or {})})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        if "removed" in r.geturl():  # imgur redirects deleted images to a "no longer available" placeholder
            raise ValueError("removed")
        return r.read()

def candidates(media):
    """Image URLs from one season's media, best first."""
    imgur = [m for m in media if m.get("type") == "imgur" and m.get("direct_url")]
    other = [m for m in media if m.get("type") in ("instagram-image", "cdphotothread") and m.get("direct_url")]
    ranked = sorted(imgur, key=lambda m: not m.get("preferred")) + sorted(other, key=lambda m: not m.get("preferred"))
    return [m["direct_url"] for m in ranked]

def thumb(raw):
    im = ImageOps.exif_transpose(Image.open(io.BytesIO(raw))).convert("RGB")
    im = ImageOps.fit(im, (W, H), Image.LANCZOS, centering=(0.5, 0.45))
    buf = io.BytesIO(); im.save(buf, "WEBP", quality=Q, method=6)
    return buf.getvalue()

def fetch(num):
    for y in YEARS:
        try:
            media = json.loads(get(f"https://www.thebluealliance.com/api/v3/team/frc{num}/media/{y}", {"X-TBA-Auth-Key": KEY}))
        except Exception as e:
            return num, None, f"api {y}: {e}"
        for url in candidates(media):
            try:
                (out_dir / f"{num}.webp").write_bytes(thumb(get(url)))
                return num, (y, url), None
            except Exception:
                continue
    return num, None, "no photo"

teams = [int(l.split("|")[0]) for l in (root / "data" / "cmp-2026.txt").read_text().splitlines() if l and l[0].isdigit()]
done = {}
if index_path.exists() and "--refresh" not in sys.argv:
    for l in index_path.read_text().splitlines():
        if l and l[0].isdigit():
            n, y, u = l.split("|"); done[int(n)] = (int(y), u)
todo = [n for n in teams if n not in done]
missing = []
with concurrent.futures.ThreadPoolExecutor(8) as ex:
    for num, hit, err in ex.map(fetch, todo):
        if hit: done[num] = hit
        else: missing.append((num, err))
lines = ["# Robot photos from The Blue Alliance (team media). team|season|source image", *(f"{n}|{y}|{u}" for n, (y, u) in sorted(done.items()))]
index_path.write_text("\n".join(lines) + "\n")
print(f"{len(done)} of {len(teams)} teams have photos")
for n, err in missing: print("missing", n, err)
