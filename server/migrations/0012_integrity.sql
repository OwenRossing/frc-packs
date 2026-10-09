-- Requests that change what a player owns can carry an Idempotency-Key. The same key sent again (a retry after the
-- connection dropped) gets the first answer back instead of opening, scrapping, crafting or offering twice.
create table requests (
  user_id uuid not null references users on delete cascade,
  key uuid not null,
  kind text not null,
  response jsonb,
  created_at timestamptz not null default now(),
  primary key (user_id, key)
);
create index requests_old on requests (created_at);

-- What each side of a trade was when it was offered, so the history still shows cards that were later scrapped.
alter table trades add column give_cards jsonb, add column want_cards jsonb;

-- The ledger: every change to a player's packs, parts and cards, written by triggers so no code path can skip it.
-- Each transaction says why (set_config('frc.reason', ...)); the sum of a player's rows always equals what they own.
create table ledger (
  id bigserial primary key,
  at timestamptz not null default now(),
  user_id uuid not null,
  item text not null check (item in ('packs', 'boosted', 'parts', 'card')),
  delta int not null,
  detail text,
  reason text not null
);
create index ledger_user on ledger (user_id, id desc);

create function ledger_reason() returns text language sql stable as
  $$ select coalesce(nullif(current_setting('frc.reason', true), ''), 'unlabeled') $$;

create function ledger_users() returns trigger language plpgsql as $$
begin
  if new.parts is distinct from old.parts then
    insert into ledger (user_id, item, delta, reason) values (new.id, 'parts', new.parts - old.parts, ledger_reason());
  end if;
  return null;
end $$;
create trigger ledger_users after update of parts on users for each row execute function ledger_users();

-- 'packs' counts standard sealed packs (sealed - boosted); 'boosted' counts boosted ones.
create function ledger_packs() returns trigger language plpgsql as $$
declare
  os int := 0; ob int := 0; ns int := 0; nb int := 0; who uuid; pk text;
begin
  if tg_op <> 'INSERT' then os := old.sealed; ob := old.boosted; who := old.user_id; pk := old.pack_id; end if;
  if tg_op <> 'DELETE' then ns := new.sealed; nb := new.boosted; who := new.user_id; pk := new.pack_id; end if;
  if (ns - nb) <> (os - ob) then
    insert into ledger (user_id, item, delta, detail, reason) values (who, 'packs', (ns - nb) - (os - ob), pk, ledger_reason());
  end if;
  if nb <> ob then
    insert into ledger (user_id, item, delta, detail, reason) values (who, 'boosted', nb - ob, pk, ledger_reason());
  end if;
  return null;
end $$;
create trigger ledger_packs after insert or delete or update of sealed, boosted on user_packs
  for each row execute function ledger_packs();

create function ledger_cards() returns trigger language plpgsql as $$
begin
  if tg_op = 'DELETE' or (tg_op = 'UPDATE' and new.user_id <> old.user_id) then
    insert into ledger (user_id, item, delta, detail, reason) values (old.user_id, 'card', -1, old.team || ' #' || old.serial, ledger_reason());
  end if;
  if tg_op = 'INSERT' or (tg_op = 'UPDATE' and new.user_id <> old.user_id) then
    insert into ledger (user_id, item, delta, detail, reason) values (new.user_id, 'card', 1, new.team || ' #' || new.serial, ledger_reason());
  end if;
  return null;
end $$;
create trigger ledger_cards after insert or delete or update of user_id on cards for each row execute function ledger_cards();

-- Opening balances, so every player's ledger adds up from today.
insert into ledger (user_id, item, delta, reason) select id, 'parts', parts, 'balance' from users where parts <> 0;
insert into ledger (user_id, item, delta, detail, reason)
  select user_id, 'packs', sealed - boosted, pack_id, 'balance' from user_packs where sealed - boosted <> 0;
insert into ledger (user_id, item, delta, detail, reason)
  select user_id, 'boosted', boosted, pack_id, 'balance' from user_packs where boosted <> 0;
insert into ledger (user_id, item, delta, detail, reason)
  select user_id, 'card', count(*)::int, 'all cards', 'balance' from cards group by user_id;
