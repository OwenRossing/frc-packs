-- Standard and boosted packs are picked up separately, so each kind keeps its own pack in hand.
alter table hands drop constraint hands_pkey;
alter table hands add primary key (user_id, pack_id, boosted);
-- Trades can include sealed (standard) packs and parts on either side, besides cards.
alter table trades
  add column give_packs int not null default 0 check (give_packs between 0 and 20),
  add column want_packs int not null default 0 check (want_packs between 0 and 20),
  add column give_parts int not null default 0 check (give_parts between 0 and 100000),
  add column want_parts int not null default 0 check (want_parts between 0 and 100000);
