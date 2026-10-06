# FRC Packs prototype

A self-contained, playable prototype of the pack-opening game. Open `index.html` in a browser; there is nothing to install.

What it does:

- Claim a free pack every 5 hours (unclaimed packs never stack), plus 2 welcome packs.
- Pick one of 10 identical 2026 Championship packs from a shelf (they are the same pack; the choice is for fun), turn it over to read the odds, and swipe across the top to cut it open (inspired by Pokémon TCG Pocket). Common and Uncommon cards arrive face up; Rare and better arrive face down and glowing, and you tap to flip them. Swipe each card away, then see all 5 on a summary screen.
- Each pack holds 5 cards from the 515 teams at the 2026 Championship in Houston. Mythic is the 44 teams from the season's top 50 by Statbotics EPA who were there. Everyone else is ranked by EPA at Champs: the next 50 are Legendary, then 120 Rare, 150 Uncommon and 151 Common.
- Every card copy gets a serial number, and copies are unlimited.
- Binder with tier progress, "show cards I'm missing" chase view, and a card inspector.
- Progress is saved in `localStorage`. Closing the tab mid-pack resumes the same cards.

What it is not:

- Not cheat-proof. Rolls, timers and serials run in the browser, so the real build needs a server (see the plan doc).
- Data came from the Statbotics API through a summarizing fetch, so small transcription errors are possible. `data/cmp-2026.txt` is the Champs roster (division, Champs EPA and record); `data/teams-2026-sample.txt` holds season ranks for the top 613 teams and is used to find the top 50 and team locations.
- Robots are drawings, not Blue Alliance photos. Legendary and Mythic cards use a full-art holo layout; every robot gets alliance bumpers with its number and one of four mechanisms.
- "Demo luck" (on by default) boosts Mythic to about 1 pack in 5. Real odds are 1 in 20,000 packs.

## Files

- `src/app.html`: the app (HTML, CSS and JS in one file, with a data placeholder)
- `data/cmp-2026.txt`: the pack's teams, one `team|name|champs EPA|W-L|division|season rank|location` line per team
- `data/teams-2026-sample.txt`: 2026 season data, one `rank|team|name|epa|W-L|place` line per team
- `build.py`: inlines the data into `src/app.html` and writes `index.html`
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
