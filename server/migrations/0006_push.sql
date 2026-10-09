-- Push notifications. Each browser or installed app that turns them on gets a subscription (its push endpoint and
-- keys, from the browser). The server's own signing key (VAPID) is made on first start and kept here.
create table push_subs (
  endpoint text primary key,
  user_id uuid not null references users on delete cascade,
  p256dh text not null,
  auth text not null,
  created_at timestamptz not null default now()
);
create index push_subs_user on push_subs (user_id);
alter table settings add column vapid_private text;
-- The free-pack time we last sent a "pack ready" notification for, so each timer notifies once.
alter table users add column pack_ping_at timestamptz;
