//! Rolling a pack. Pure functions over a random source, so the odds can be tested with a seeded generator.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::packs::{Odds, Pack, Tier};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rolled {
    pub num: i32,
    pub tier: Tier,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Pity {
    /// Packs since the last Mythic.
    pub m: u32,
    /// Packs since the last Legendary or better.
    pub l: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Opts {
    pub demo: bool,
    /// The scripted first pack: its last card is Legendary or better.
    pub first: bool,
    pub pity: Pity,
    /// A boosted pack, crafted from parts: the recipe's boosted odds.
    pub boosted: bool,
}

/// Chance the last card is a Mythic when this is the `n`th pack since the last one.
pub fn mythic_chance(odds: &Odds, demo: bool, n: u32) -> f64 {
    let mut p = if demo { odds.demo_mythic } else { odds.mythic_base };
    if n > odds.soft {
        p = p.max((f64::from(n - odds.soft) / f64::from(odds.hard - odds.soft)).min(1.0));
    }
    p
}

fn weighted<R: Rng + ?Sized>(rng: &mut R, w: &HashMap<Tier, f64>) -> Tier {
    let r: f64 = rng.random();
    let mut sum = 0.0;
    let mut last = Tier::Common;
    for tier in Tier::ORDER.iter().rev() {
        if let Some(p) = w.get(tier) {
            sum += p;
            last = *tier;
            if r < sum {
                return *tier;
            }
        }
    }
    last
}

/// Five cards, sorted with the best last. No team appears twice in one pack.
pub fn roll<R: Rng + ?Sized>(pack: &Pack, rng: &mut R, o: &Opts) -> Vec<Rolled> {
    let odds = &pack.recipe.odds;
    let (pm, pl) = (o.pity.m + 1, o.pity.l + 1);
    let (slots, last, mult) =
        if o.boosted { (&odds.boosted.slots, &odds.boosted.last, odds.boosted.mythic_mult) } else { (&odds.slots, &odds.last, 1.0) };
    let mut cards: Vec<Rolled> = Vec::with_capacity(5);
    for slot in 0..5 {
        let tier = if slot < 4 {
            weighted(rng, slots)
        } else if rng.random::<f64>() < (mythic_chance(odds, o.demo, pm) * mult).min(1.0) {
            Tier::Mythic
        } else if o.first || pl >= odds.leg_every {
            Tier::Legendary
        } else {
            weighted(rng, last) // demo luck only boosts Mythic
        };
        let pool = &pack.pools[&tier];
        let free: Vec<i32> = pool.iter().copied().filter(|n| !cards.iter().any(|c| c.num == *n)).collect();
        let from = if free.is_empty() { pool.as_slice() } else { free.as_slice() };
        let num = from[rng.random_range(0..from.len())];
        cards.push(Rolled { num, tier });
    }
    sort_best_last(&mut cards);
    cards
}

pub fn sort_best_last(cards: &mut [Rolled]) {
    cards.sort_by_key(|c| std::cmp::Reverse(c.tier.rank()));
}

pub fn best(cards: &[Rolled]) -> Tier {
    cards.iter().map(|c| c.tier).min_by_key(|t| t.rank()).unwrap_or(Tier::Common)
}

/// Pity counters after opening these cards.
pub fn next_pity(p: Pity, cards: &[Rolled]) -> Pity {
    let has_m = cards.iter().any(|c| c.tier == Tier::Mythic);
    let has_l = has_m || cards.iter().any(|c| c.tier == Tier::Legendary);
    Pity { m: if has_m { 0 } else { p.m + 1 }, l: if has_l { 0 } else { p.l + 1 } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::Catalog;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::collections::HashSet;

    fn cmp26() -> Pack {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/packs");
        let mut c = Catalog::load(&dir).unwrap();
        let i = c.packs.iter().position(|p| p.id() == "cmp26").unwrap();
        c.packs.remove(i)
    }

    #[test]
    fn odds_over_200k_packs() {
        let pack = cmp26();
        let mut rng = StdRng::seed_from_u64(7);
        let n = 200_000;
        let (mut myth, mut leg_plus, mut dupes, mut last_ok) = (0, 0, 0, 0);
        for _ in 0..n {
            let c = roll(&pack, &mut rng, &Opts::default());
            let set: HashSet<i32> = c.iter().map(|x| x.num).collect();
            if set.len() != 5 {
                dupes += 1;
            }
            if c.iter().any(|x| x.tier == Tier::Mythic) {
                myth += 1;
            }
            if c.iter().any(|x| x.tier == Tier::Mythic || x.tier == Tier::Legendary) {
                leg_plus += 1;
            }
            if c[4].tier.rank() <= Tier::Rare.rank() {
                last_ok += 1;
            }
            for x in &c {
                assert_eq!(pack.team_tier[&x.num], x.tier, "card tier must match the team's tier");
            }
        }
        assert_eq!(dupes, 0, "no team twice in one pack");
        assert_eq!(last_ok, n, "last card is always Rare or better");
        // 1 in 400 without pity: 500 expected in 200k.
        assert!((380..=640).contains(&myth), "mythic packs {myth}");
        // Without the pity meter, Legendary+ is about 8.9% of packs (0.25% + 7% last slot + 2% from slots 1-4).
        let rate = f64::from(leg_plus) / f64::from(n);
        assert!((0.08..0.10).contains(&rate), "legendary+ rate {rate}");
    }

    #[test]
    fn pity_guarantees() {
        let pack = cmp26();
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..2000 {
            let c = roll(&pack, &mut rng, &Opts { pity: Pity { m: 199, l: 0 }, ..Default::default() });
            assert_eq!(c[4].tier, Tier::Mythic, "pack 200 is always a Mythic");
            let c = roll(&pack, &mut rng, &Opts { pity: Pity { m: 0, l: 19 }, ..Default::default() });
            assert!(c[4].tier.rank() <= Tier::Legendary.rank(), "20th dry pack is Legendary or better");
            let c = roll(&pack, &mut rng, &Opts { first: true, ..Default::default() });
            assert!(c[4].tier.rank() <= Tier::Legendary.rank(), "first pack ends in Legendary or better");
        }
        let o = &pack.recipe.odds;
        assert_eq!(mythic_chance(o, false, 10), 1.0 / 400.0);
        assert!((mythic_chance(o, false, 175) - 0.5).abs() < 1e-9);
        assert_eq!(mythic_chance(o, true, 10), 0.2);
    }

    #[test]
    fn boosted_packs_are_better() {
        let pack = cmp26();
        let mut rng = StdRng::seed_from_u64(11);
        let n = 100_000;
        let (mut myth, mut leg_plus, mut rare_plus_slots) = (0, 0, 0);
        for _ in 0..n {
            let c = roll(&pack, &mut rng, &Opts { boosted: true, ..Default::default() });
            assert!(c[4].tier.rank() <= Tier::Rare.rank(), "last card Rare or better");
            myth += i32::from(c.iter().any(|x| x.tier == Tier::Mythic));
            leg_plus += i32::from(c.iter().any(|x| x.tier.rank() <= Tier::Legendary.rank()));
            rare_plus_slots += c[..4].iter().filter(|x| x.tier.rank() <= Tier::Rare.rank()).count();
        }
        // Mythic 4x: 1 in 100, so about 1000 in 100k.
        assert!((850..=1150).contains(&myth), "boosted mythic packs {myth}");
        let rate = f64::from(leg_plus) / f64::from(n);
        assert!((0.50..0.60).contains(&rate), "boosted legendary+ rate {rate}");
        assert!(rare_plus_slots > n as usize, "more than one Rare+ in slots 1-4 per pack on average");
    }

    #[test]
    fn simulated_player_hits_pity_targets() {
        // A player opening packs with the meter: Legendary+ about 1 pack in 10, and never more than 20 apart.
        let pack = cmp26();
        let mut rng = StdRng::seed_from_u64(3);
        let mut p = Pity::default();
        let (mut leg, mut worst_gap, mut gap, mut worst_m) = (0, 0, 0, 0);
        for _ in 0..100_000 {
            let c = roll(&pack, &mut rng, &Opts { pity: p, ..Default::default() });
            p = next_pity(p, &c);
            gap = if p.l == 0 { 0 } else { gap + 1 };
            worst_gap = worst_gap.max(gap);
            worst_m = worst_m.max(p.m);
            if p.l == 0 {
                leg += 1;
            }
        }
        assert!(worst_gap < 20, "longest dry run {worst_gap}");
        assert!(worst_m < 200, "longest run without a Mythic {worst_m}");
        let rate = f64::from(leg) / 100_000.0;
        assert!((0.09..0.12).contains(&rate), "legendary+ rate with the meter {rate}");
    }
}
