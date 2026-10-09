-- Serial numbers back in circulation: when the admin deletes an account, its cards' serial numbers go here, and the
-- next pull of that team takes the lowest one instead of printing a new number. (Scrapped copies stay retired.)
create table free_serials (
  pack_id text not null,
  team int not null,
  serial int not null,
  freed_at timestamptz not null default now(),
  primary key (pack_id, team, serial)
);
