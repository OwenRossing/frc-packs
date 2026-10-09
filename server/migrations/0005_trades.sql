-- Trades between players: one offers some of their cards for some of the other player's. Each side names specific
-- copies (card ids), so the serial numbers travel with the cards. Accepting swaps the owners in one transaction.
create table trades (
  id bigserial primary key,
  from_user uuid not null references users on delete cascade,
  to_user uuid not null references users on delete cascade,
  pack_id text not null,
  give bigint[] not null, -- cards from_user gives
  want bigint[] not null, -- cards from_user gets from to_user
  status text not null default 'open' check (status in ('open', 'accepted', 'declined', 'cancelled', 'failed')),
  created_at timestamptz not null default now(),
  decided_at timestamptz
);
create index trades_to_open on trades (to_user) where status = 'open';
create index trades_from_open on trades (from_user) where status = 'open';
