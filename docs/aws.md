# Moving FRC Packs to AWS

Today the site runs on one machine: the Rust server serves the site and the API, and Postgres holds the data, behind Cloudflare Tunnel. This is how it moves to AWS when it outgrows that, and what's already in place for it.

## The shape

```
             CloudFront (packs.<domain>)
              ├── /api/*  ──►  Lambda (the same Rust server, built with --features lambda)  ──►  RDS Proxy  ──►  Postgres (Aurora Serverless v2 or RDS)
              └── everything else  ──►  S3 (the built site, robot photos, pack recipes)
```

- **S3 + CloudFront** serve everything that isn't the API. Nothing about the site changes: it already asks for `/photos/…`, `/packs/…` and `/api/…` on its own domain, so one CloudFront distribution with two behaviors covers it.
- **Lambda** runs the unchanged server. `cargo build --release --features lambda` adds the Lambda runtime; when Lambda starts the binary it answers Lambda's HTTP events instead of opening a port. Locally and on the laptop it still runs as a normal server.
- **Postgres stays.** See "Why not DynamoDB" below.

## What's already done

- `--features lambda` (server/Cargo.toml, server/src/main.rs): the same binary runs on Lambda.
- Sign-in rate limiting reads the visitor's IP from CloudFront (`CloudFront-Viewer-Address`) as well as Cloudflare.
- `deploy/aws/sync-static.sh <bucket>` uploads the site, photos and recipes to S3 with the same cache rules the server uses.
- The server doesn't need to serve the site (it already skips it when `WEB_DIR` has no build), so on Lambda it's API only.

## Steps

1. **Database.** Create Aurora Serverless v2 (Postgres) or a small RDS Postgres, and an RDS Proxy in front of it (Lambda opens many short connections; the proxy pools them). Move the data with `pg_dump`/`pg_restore` from the laptop's nightly backup (see deploy/README.md, Backups). The server runs its own migrations on start.
2. **Lambda.** Build for Lambda's Linux:
   ```sh
   cargo install cargo-lambda
   cd server && cargo lambda build --release --features lambda --arm64
   ```
   Make a function from `target/lambda/frc-packs-server/bootstrap` (runtime `provided.al2023`, arm64, 512 MB is plenty), in the database's VPC. Environment: `DATABASE_URL` (through the proxy), `DATA_DIR=/var/task/data` with `data/packs` bundled into the zip (the server reads the recipes; it doesn't need the photos), `COOKIE_SECURE=1`. Give it a function URL, or an API Gateway HTTP API.
3. **S3.** Make a private bucket; run `deploy/aws/sync-static.sh <bucket>` after each site build.
4. **CloudFront.** One distribution for `packs.<domain>`: default behavior → the S3 bucket (origin access control), default root object `index.html`; behavior `/api/*` → the Lambda URL with caching off and all headers, cookies and query strings forwarded. Point the domain at CloudFront (if DNS stays on Cloudflare, a CNAME with the proxy off).
5. **Cut over** the same way as moving machines in deploy/README.md: take the old site offline, take a final backup, restore it into AWS, then switch DNS. Same hostname, so everyone stays signed in and keeps their cards.

Things to know on Lambda:
- Failed sign-in counting lives in memory, so each warm Lambda counts separately. It still slows guessing; for a hard limit, add AWS WAF rate rules on `/api/login`.
- A cold start is roughly a quarter second (Rust is quick; most of it is the database connection).
- Nightly backups become RDS automated backups; the systemd backup timer doesn't come along.

## Why not DynamoDB

DynamoDB would mean rewriting the server's data layer, not configuring it. The game leans on things Postgres does in one transaction:

- **Opening a pack** mints five serial numbers (a counter per team), writes five cards, updates pity, checks division sets and can award bonus packs, all or nothing.
- **Trades** check that both players still hold every card, swap owners and call off other offers that promised those cards, all or nothing.
- **Scrapping** picks "every extra copy except the first" with a window query.
- **The binder, admin panel and stats** are joins and counts.

DynamoDB can do some of this (`TransactWriteItems` handles up to 100 items, atomic counters work for serials), but each feature needs its own key design and access patterns, the admin queries become scans or extra indexes, and a schema change later is a migration script, not a SQL file. Estimate two to four weeks of rewrite and retesting for no gain at this size.

Aurora Serverless v2 gives the AWS-managed, scale-to-demand part without the rewrite: it scales with load, and the code stays as it is. DynamoDB starts to pay off at millions of players or when you want per-request pricing near zero at idle; neither is the case for a team's card game.

## Rough cost (us-east-1, small team)

- Lambda + API Gateway/function URL: likely free tier, a few dollars at most.
- S3 + CloudFront: under a dollar a month for photos and the site.
- The database is the main cost. Aurora Serverless v2 held at 0.5 ACU is roughly $45 a month; it can also pause to zero when idle, but the first request after a pause waits several seconds. A `db.t4g.micro` RDS Postgres is about $12 a month plus storage. RDS Proxy bills for at least 2 database vCPUs, about $22 a month. For a small community, plain RDS without the proxy, with the Lambda's concurrency capped, is the cheap option.

These are list prices as of 2026; check the AWS pricing pages before deciding.
