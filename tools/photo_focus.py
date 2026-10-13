"""Finds where the robot is in each team photo, so a card's photo window can be centered on it instead of on the middle
of the picture. Writes data/photos/focus.json: { "<team>": [x, y] } with x and y from 0 to 1 (left to right, top to bottom).

The robot is the busiest part of a photo (most edges and detail); floors, walls and sky are flat. This takes the
detail in the photo, ignores the outer 6% (borders and watermarks), and returns the middle of where most of it is.
Run: python tools/photo_focus.py
"""
import json
import pathlib

import numpy as np
from PIL import Image, ImageFilter

PHOTOS = pathlib.Path(__file__).resolve().parent.parent / "data" / "photos"


def focus(path):
    im = Image.open(path).convert("L")
    im.thumbnail((160, 160))
    a = np.asarray(im, dtype=np.float32)
    gx = np.abs(np.diff(a, axis=1))[:-1, :]
    gy = np.abs(np.diff(a, axis=0))[:, :-1]
    e = Image.fromarray(np.clip(gx + gy, 0, 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(3))
    e = np.asarray(e, dtype=np.float32)
    h, w = e.shape
    mx, my = int(w * 0.06), int(h * 0.06)
    mask = np.zeros_like(e)
    mask[my : h - my, mx : w - mx] = 1
    e = e * mask
    # Keep only the busiest 35% of the picture, so a flat background doesn't pull the middle away from the robot.
    cut = np.quantile(e[mask > 0], 0.65)
    e = np.where(e >= cut, e, 0)
    total = e.sum()
    if total <= 0:
        return [0.5, 0.5]
    ys, xs = np.mgrid[0:h, 0:w]
    return [round(float((xs * e).sum() / total) / w, 3), round(float((ys * e).sum() / total) / h, 3)]


def main():
    out = {}
    for p in sorted(PHOTOS.glob("*.webp")):
        try:
            out[p.stem] = focus(p)
        except Exception as ex:  # a bad photo just falls back to the middle
            print("skipped", p.name, ex)
    (PHOTOS / "focus.json").write_text(json.dumps(out, separators=(",", ":")))
    print(f"{len(out)} photos")


if __name__ == "__main__":
    main()
