-- Players can report another player (an offensive username, cheating, harassment). The admin panel lists open reports.
create table reports (
  id bigserial primary key,
  reporter uuid not null references users on delete cascade,
  target uuid not null references users on delete cascade,
  reason text not null,
  details text not null default '',
  created_at timestamptz not null default now(),
  resolved_at timestamptz
);
create index reports_open on reports (created_at) where resolved_at is null;
