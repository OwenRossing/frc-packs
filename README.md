# frc-packs

Pokémon-style packs, but every card is an FRC team's robot.

- `web/` and `server/`: the real site. A Rust server decides what's in every pack and keeps everyone's cards; the site shows it.
- `prototype/`: the original single-file prototype (everything in the browser). Still playable: open `prototype/index.html`. See `prototype/README.md`.
- `data/`: shared data. Rosters, robot photos, and the pack recipes the server and site both read.

## How it fits together

```
browser (web/, Vite + TypeScript)
   │  JSON over /api, photos from /photos, pack recipes from /packs
   ▼
server/ (Rust: Axum + sqlx)  ──►  Postgres
   reads data/packs/*.json (what's in each pack, odds, pity rules)
```

- **Packs are data.** `data/packs/cmp26.json` holds the 2026 Championship pack: its 515 teams with their rarity, the odds and the Mythic meter rules. `tools/build_packs.py` builds it from `data/cmp-2026.txt`. Adding a pack means adding a recipe there, not code.
- **The server owns everything that matters.** It rolls packs, mints serial numbers (No. 1, No. 2, ... per card), runs the free-pack timer and the Mythic meter, and stores every card. The browser can't change any of it.
- **Guest accounts.** The first visit creates an account and keeps it in a cookie (the database stores only a hash of it). Sign-in comes later and will attach to this account.

## Run it locally

You need Rust (rustup.rs), Node 20 or newer, and Postgres (Docker is easiest).

```sh
docker compose up -d                       # Postgres on localhost:5432 (user, password and database in docker-compose.yml)

cd server
DATABASE_URL=postgres://frc:frc@localhost:5432/frcpacks DEV_TOOLS=1 cargo run
                                           # API on http://127.0.0.1:3000; creates its tables on start

cd web                                     # in another terminal
npm install
npm run dev                                # open http://localhost:5173 (proxies /api to the server)
```

To try it the way it will run in production, `npm run build` in `web/`, then open http://127.0.0.1:3000: the server serves the built site itself.

### Server settings

| Variable | Default | What it does |
| --- | --- | --- |
| `DATABASE_URL` | required | Postgres connection string |
| `BIND` | `127.0.0.1:3000` | Address to listen on |
| `DATA_DIR` | `../data` | Where `packs/` and `photos/` are |
| `WEB_DIR` | `../web/dist` | The built site; skipped if it isn't there |
| `DEV_TOOLS` | off | Testing helpers in Settings: demo luck, +1 pack, skip timer, reset. **Never turn on in production.** |
| `COOKIE_SECURE` | off | Mark the session cookie HTTPS-only. Turn on when hosted over HTTPS. |

## Host it

`deploy/install.sh` installs and runs the site on an Ubuntu machine, and Cloudflare Tunnel puts it on your domain without opening any ports. Steps, backups, and moving to another machine: [deploy/README.md](deploy/README.md).

## Tests

```sh
cd server && DATABASE_URL=postgres://frc:frc@localhost:5432/frcpacks cargo test
```
Odds over 200,000 simulated packs, the pity guarantees, and API tests against a real database (claims, packs in hand, opening, serial numbers, division sets, two tabs opening at once, resuming a pack mid-reveal). The API tests are skipped without `DATABASE_URL`.

```sh
cd web && npx playwright install chromium   # once
npm run build                               # then start the server with DEV_TOOLS=1 (it serves web/dist)
npm test                                    # a browser bot plays packs against the server
```

## Not done yet

- Sign-in (accounts are guest-only and live in one browser's cookie).
- Rate limiting on account creation and pack opening.
- Trading, crafting with parts, and more packs.
