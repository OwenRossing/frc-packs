-- Numbered limited editions (for example the 7028 Beta Edition). Each copy has a serial, one per edition and number.
create table special_cards (
  id bigserial primary key,
  edition text not null,
  serial int not null,
  user_id uuid not null references users on delete cascade,
  created_at timestamptz not null default now(),
  unique (edition, serial)
);
create index special_cards_user on special_cards (user_id);
