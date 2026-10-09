-- Profiles: up to 3 cards a player pins to show off, and a wishlist of teams they want (shown to trade partners).
alter table users add column showcase int[] not null default '{}';
create table wishlist (
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  team int not null,
  created_at timestamptz not null default now(),
  primary key (user_id, pack_id, team)
);
