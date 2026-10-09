-- Boosted packs can be traded like standard ones.
alter table trades
  add column give_boosted int not null default 0,
  add column want_boosted int not null default 0;
