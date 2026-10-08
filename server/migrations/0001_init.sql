-- Accounts. A guest account is created on first visit and identified by a random token in a cookie;
-- only a SHA-256 hash of the token is stored. Real sign-in attaches to this row later.
create table users (
  id uuid primary key,
  token_hash bytea not null unique,
  created_at timestamptz not null default now(),
  -- When the next free pack can be claimed. Free packs bank up to 2.
  next_claim_at timestamptz not null,
  -- Testing only: boosts Mythic odds. Can only be turned on when the server runs with DEV_TOOLS=1.
  demo boolean not null default false
);

-- One row per account per pack type: sealed packs on hand, packs opened, and the pity counters.
create table user_packs (
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  sealed int not null default 0 check (sealed >= 0),
  opened int not null default 0,
  pity_m int not null default 0, -- packs since the last Mythic
  pity_l int not null default 0, -- packs since the last Legendary or better
  primary key (user_id, pack_id)
);

-- The pack you picked up but haven't opened. Its cards are rolled when you pick it up and stay fixed if you put it
-- back, so the glow while you swipe tells the truth. Never sent to the browser except for the best rarity.
create table hands (
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  cards jsonb not null,
  created_at timestamptz not null default now(),
  primary key (user_id, pack_id)
);

-- Each opened pack. `revealed` counts cards dealt off the stack, so a pack interrupted mid-reveal picks up where it left off.
create table openings (
  id bigserial primary key,
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  opened_at timestamptz not null default now(),
  revealed int not null default 0,
  sets text[] not null default '{}'
);
create index openings_pending on openings (user_id) where revealed < 5;

-- How many copies of each card have been minted, for sequential serial numbers (No. 1, No. 2, ...).
create table printings (
  pack_id text not null,
  team int not null,
  minted int not null default 0,
  primary key (pack_id, team)
);

-- Every copy of every card. The server is the only thing that writes here.
create table cards (
  id bigserial primary key,
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  team int not null,
  tier text not null,
  serial int not null,
  opening_id bigint references openings on delete set null,
  slot smallint,
  obtained text not null default 'pack',
  created_at timestamptz not null default now(),
  unique (pack_id, team, serial)
);
create index cards_owner on cards (user_id, pack_id, team);

-- Division sets: own every team from one division for a bonus pack.
create table sets_done (
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  division text not null,
  completed_at timestamptz not null default now(),
  primary key (user_id, pack_id, division)
);
