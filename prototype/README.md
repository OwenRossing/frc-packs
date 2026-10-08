# FRC Packs prototype

A self-contained, playable prototype of the pack-opening game. Open `index.html` in a browser; there is nothing to install.

What it does:

- Claim a free pack every 5 hours, plus 2 welcome packs. Unclaimed free packs bank up to 2, so missing a timer overnight doesn't cost a pack.
- On first visit you can pick your team. Its card gets a gold MY TEAM border, pulling it gets its own moment, it's pinned first in the binder, and if it was at Champs your first pack includes it. Change it in Settings.
- The first pack always ends in Legendary or better.
- Pick one of 10 identical 2026 Championship packs from a shelf (they are the same pack; the choice is for fun), turn it over to read the odds, and swipe across the top to cut it open (inspired by Pokémon TCG Pocket). Common and Uncommon cards arrive face up; Rare and better arrive face down and glowing, and you tap to flip them. Swipe each card away (or press Reveal all to skip ahead; a face-down best card still gets its reveal), then see all 5 on a summary screen with your collection progress. "Open another pack" puts the next pack straight in your hand.
- Each pack holds 5 cards from the 515 teams at the 2026 Championship in Houston. Mythic is the 44 teams from the season's top 50 by Statbotics EPA who were there. Everyone else is ranked by EPA at Champs: the next 50 are Legendary, then 120 Rare, 150 Uncommon and 151 Common.
- Every card copy gets a serial number, and copies are unlimited.
- Binder with a pack list (just the 2026 Championship pack for now) showing overall and per-tier progress, plus tier and division progress, NEW marks (and a count on the Binder tab) for teams you haven't looked at yet, "show cards I'm missing" chase view, and a card inspector.
- Progress is saved in `localStorage`. Closing the tab mid-pack resumes the same cards.

What it is not:

- Not cheat-proof. Rolls, timers and serials run in the browser, so the real build needs a server (see the plan doc).
- Data came from the Statbotics API through a summarizing fetch, so small transcription errors are possible. `data/cmp-2026.txt` is the Champs roster (division, Champs EPA and record); `data/teams-2026-sample.txt` holds season ranks for the top 613 teams and is used to find the top 50 and team locations.
- Robot photos come from The Blue Alliance (504 of 515 teams), embedded as 240x180 WebP thumbnails so the page stays self-contained (about 5 MB). Each team gets its newest season's photo, preferring TBA's "preferred" picks; 429 are from 2026. The 11 teams with no photo on TBA keep a drawn robot. Legendary and Mythic cards use a full-art holo layout with the photo in a framed window.
- Mythic meter: real Mythic odds are 1 in 400 packs. The meter under the pack counter counts packs since your last Mythic; odds climb after 150 and a Mythic is guaranteed at 200. Legendary or better is guaranteed at least every 10 packs.
- Opening: a pack's contents are fixed when you pick it up (putting it back doesn't reroll it). As you swipe the top, the pack glows brighter in the color of its best card, then the cards slide straight out of the pack with no wait.
- Reveal cues: the tear glows in the color of the best card inside (a Mythic shows gold, then shifts to a color-cycling glow), face-down cards chime and vibrate by tier when they come up, a face-down Mythic turns from gold to Mythic before you flip it, and the flip itself gets a short build-up.
- Division sets: the binder tracks each of the 8 Houston divisions. Owning every team in one gives a bonus pack, and the summary tells you when you're 3 or fewer away.
- "Demo luck" (on by default) boosts Mythic to about 1 pack in 5.

## Files

- `src/app.html`: the app (HTML, CSS and JS in one file, with a data placeholder)
- `data/cmp-2026.txt`: the pack's teams, one `team|name|champs EPA|W-L|division|season rank|location` line per team
- `data/teams-2026-sample.txt`: 2026 season data, one `rank|team|name|epa|W-L|place` line per team
- `data/photos/<team>.webp` and `data/photos.txt`: robot thumbnails and where each came from (`team|season|source image`)
- `fetch_photos.py`: downloads the photos (`TBA_KEY=... python3 fetch_photos.py`; skips teams it already has unless `--refresh`)
- `build.py`: inlines the data and photos into `src/app.html` and writes `index.html`
- `test/play.js`: bot that plays packs through the UI, simulates 200,000 packs for odds, and checks layouts
- `test/edge.js`: failure-mode checks (two tabs, double click, mis-swipes, face-down cards, corrupt saves, keyboard, touch, reduced motion)
- `test/lib.js`: shared helpers, including `openOne`, which plays a pack from shelf to summary

## Run the tests

```
python3 build.py
NODE_PATH=$(npm root -g) node test/play.js
NODE_PATH=$(npm root -g) node test/edge.js
```

They need Playwright and a Chromium build (edit the `executablePath` in the scripts if yours lives elsewhere).
