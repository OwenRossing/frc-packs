-- Real accounts: a username and password, made with an invite code. Sessions get their own table so one account can
-- be signed in on several devices at once.
create table sessions (
  token_hash bytea primary key, -- SHA-256 of the cookie token
  user_id uuid not null references users on delete cascade,
  created_at timestamptz not null default now()
);
create index sessions_user on sessions (user_id);
insert into sessions (token_hash, user_id, created_at) select token_hash, id, created_at from users;
alter table users drop column token_hash;

-- Invite codes. A code can be used max_uses times; each use creates one account.
create table invites (
  code text primary key, -- stored without dashes, upper case
  note text not null default '',
  max_uses int not null default 1 check (max_uses > 0),
  uses int not null default 0,
  created_at timestamptz not null default now(),
  created_by uuid references users on delete set null
);

-- Accounts made before sign-in existed are guests (no username). Signing up from that browser turns the guest into a
-- real account and keeps its cards.
alter table users
  add column username text,
  add column password_hash text, -- argon2id, PHC string
  add column is_admin boolean not null default false,
  add column disabled boolean not null default false,
  add column invite_code text references invites on delete set null;
create unique index users_username on users (lower(username));
