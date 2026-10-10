-- A short tag shown next to a player's name (for example "Beta tester"). The admin can set or clear it.
alter table users add column badge text;
update users set badge = 'Beta tester' where username is not null and not is_admin;
