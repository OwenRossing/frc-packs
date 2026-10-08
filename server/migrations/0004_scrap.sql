-- Scrapping extra copies for parts, and crafting boosted packs from parts.
alter table users add column parts int not null default 0 check (parts >= 0);
-- How many of the sealed packs are boosted. Boosted packs open first.
alter table user_packs add column boosted int not null default 0 check (boosted >= 0 and boosted <= sealed);
alter table hands add column boosted boolean not null default false;
alter table openings add column boosted boolean not null default false;
-- Parts a boosted pack costs. The admin sets it with the other free-pack rules.
alter table settings add column boost_cost int not null default 250 check (boost_cost between 1 and 100000);
