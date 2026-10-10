-- Favorite cards: a team you've marked is never scrapped, however many copies you hold.
create table favorites (
  user_id uuid not null references users on delete cascade,
  pack_id text not null,
  team int not null,
  created_at timestamptz not null default now(),
  primary key (user_id, pack_id, team)
);
