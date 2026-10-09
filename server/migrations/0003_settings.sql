-- Free-pack rules the admin sets in the panel. Exactly one row. The defaults are what the server did before.
create table settings (
  id boolean primary key default true check (id),
  claim_minutes int not null default 300 check (claim_minutes between 5 and 10080), -- time between free packs
  claim_packs int not null default 1 check (claim_packs between 1 and 20),          -- packs each timer gives
  bank int not null default 2 check (bank between 1 and 10),                        -- missed timers that wait to be claimed
  start_packs int not null default 2 check (start_packs between 0 and 50)           -- packs a new account starts with
);
insert into settings default values;
