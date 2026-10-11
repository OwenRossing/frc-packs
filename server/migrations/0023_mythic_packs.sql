-- Mythic packs: a kind of boosted pack that holds three guaranteed Mythics. They count inside `boosted` (and `sealed`),
-- so `mythic` can never be more than `boosted`.
alter table user_packs add column mythic int not null default 0 check (mythic >= 0 and mythic <= boosted);
