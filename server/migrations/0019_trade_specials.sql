-- Special edition cards can be part of a trade, like any card.
alter table trades
  add column give_specials bigint[] not null default '{}',
  add column want_specials bigint[] not null default '{}';
