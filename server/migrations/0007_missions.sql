-- Daily missions: what each player did today (in the game's time zone), and which mission rewards they've taken.
create table daily (
  user_id uuid not null references users on delete cascade,
  day date not null,
  opened int not null default 0,
  scrapped int not null default 0,
  traded int not null default 0,
  claimed text[] not null default '{}',
  primary key (user_id, day)
);
-- The pack-opening streak: days in a row with at least one pack opened, and the last such day.
alter table users add column streak int not null default 0, add column streak_day date;
