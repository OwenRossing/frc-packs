-- More daily missions: counts for the new kinds live in one JSON object per day.
alter table daily add column extra jsonb not null default '{}';
