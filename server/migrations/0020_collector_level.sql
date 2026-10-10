-- Collector level: one level per 25 different teams owned; each level pays parts once.
alter table users add column level_claimed int not null default 0;
-- Levels already reached count as collected, so the update doesn't hand out a pile of parts at once; rewards start with new progress.
update users u set level_claimed = (select count(distinct team) from cards c where c.user_id = u.id and c.pack_id = 'cmp26') / 25;
