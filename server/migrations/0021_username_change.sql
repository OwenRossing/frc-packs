-- Players can change their username, at most once every 14 days.
alter table users add column username_changed_at timestamptz;
