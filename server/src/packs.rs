//! Pack recipes: everything about a pack lives in a data file (data/packs/<id>.json), built by tools/build_packs.py.
//! Adding a pack means adding a recipe, not code.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Mythic,
    Legendary,
    Rare,
    Uncommon,
    Common,
}

impl Tier {
    /// Best first.
    pub const ORDER: [Tier; 5] = [Tier::Mythic, Tier::Legendary, Tier::Rare, Tier::Uncommon, Tier::Common];

    /// 0 for Mythic up to 4 for Common.
    pub fn rank(self) -> usize {
        Tier::ORDER.iter().position(|t| *t == self).unwrap()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Mythic => "mythic",
            Tier::Legendary => "legendary",
            Tier::Rare => "rare",
            Tier::Uncommon => "uncommon",
            Tier::Common => "common",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Tier::ORDER.into_iter().find(|t| t.as_str() == s)
    }

    /// Parts for scrapping one extra copy. A boosted pack costs about five packs' worth of scrapped duplicates.
    pub fn scrap_parts(self) -> i32 {
        match self {
            Tier::Common => 5,
            Tier::Uncommon => 12,
            Tier::Rare => 40,
            Tier::Legendary => 150,
            Tier::Mythic => 600,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Odds {
    /// Per card in slots 1 to 4.
    pub slots: HashMap<Tier, f64>,
    /// The last slot when it isn't a Mythic or a guaranteed Legendary.
    pub last: HashMap<Tier, f64>,
    pub mythic_base: f64,
    pub demo_mythic: f64,
    /// The Mythic chance climbs after `soft` packs without one and reaches 100% at `hard`.
    pub soft: u32,
    pub hard: u32,
    /// Legendary or better at least every `leg_every` packs.
    pub leg_every: u32,
    /// A boosted pack (crafted from parts). Recipes can set their own; otherwise `Boost::default()`.
    #[serde(default)]
    pub boosted: Boost,
}

/// Odds for a boosted pack: better slots, a better last card, and a multiplied Mythic chance. The pity meters still
/// count and still guarantee as usual.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Boost {
    pub slots: HashMap<Tier, f64>,
    pub last: HashMap<Tier, f64>,
    pub mythic_mult: f64,
}

impl Default for Boost {
    fn default() -> Boost {
        Boost {
            slots: HashMap::from([(Tier::Common, 0.40), (Tier::Uncommon, 0.35), (Tier::Rare, 0.20), (Tier::Legendary, 0.05)]),
            last: HashMap::from([(Tier::Rare, 0.60), (Tier::Legendary, 0.40)]),
            mythic_mult: 4.0,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Team {
    pub num: i32,
    pub name: String,
    pub div: String,
    pub tier: Tier,
}

#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub claimable: bool,
    pub odds: Odds,
    pub teams: Vec<Team>,
}

#[derive(Debug)]
pub struct Pack {
    pub recipe: Recipe,
    pub pools: HashMap<Tier, Vec<i32>>,
    pub divisions: BTreeMap<String, Vec<i32>>,
    pub team_div: HashMap<i32, String>,
    pub team_tier: HashMap<i32, Tier>,
}

impl Pack {
    pub fn new(recipe: Recipe) -> anyhow::Result<Pack> {
        let mut pools: HashMap<Tier, Vec<i32>> = HashMap::new();
        let mut divisions: BTreeMap<String, Vec<i32>> = BTreeMap::new();
        let mut team_div = HashMap::new();
        let mut team_tier = HashMap::new();
        for t in &recipe.teams {
            if team_tier.insert(t.num, t.tier).is_some() {
                bail!("pack {}: team {} is listed twice", recipe.id, t.num);
            }
            pools.entry(t.tier).or_default().push(t.num);
            divisions.entry(t.div.clone()).or_default().push(t.num);
            team_div.insert(t.num, t.div.clone());
        }
        for tier in Tier::ORDER {
            if pools.get(&tier).is_none_or(|p| p.len() < 5) {
                bail!("pack {}: needs at least 5 {} teams", recipe.id, tier.as_str());
            }
        }
        let o = &recipe.odds;
        for (name, w) in [("slots", &o.slots), ("last", &o.last), ("boosted slots", &o.boosted.slots), ("boosted last", &o.boosted.last)] {
            let sum: f64 = w.values().sum();
            if (sum - 1.0).abs() > 1e-9 {
                bail!("pack {}: {name} odds add up to {sum}, not 1", recipe.id);
            }
        }
        if o.soft >= o.hard || o.leg_every == 0 {
            bail!("pack {}: bad pity settings", recipe.id);
        }
        Ok(Pack { recipe, pools, divisions, team_div, team_tier })
    }

    pub fn id(&self) -> &str {
        &self.recipe.id
    }
}

#[derive(Debug)]
pub struct Catalog {
    pub packs: Vec<Pack>,
}

impl Catalog {
    pub fn load(dir: &Path) -> anyhow::Result<Catalog> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        let mut packs = Vec::new();
        for f in files {
            let text = std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
            let recipe: Recipe = serde_json::from_str(&text).with_context(|| format!("parsing {}", f.display()))?;
            packs.push(Pack::new(recipe)?);
        }
        if packs.is_empty() {
            bail!("no pack recipes in {}", dir.display());
        }
        if packs.iter().filter(|p| p.recipe.claimable).count() != 1 {
            bail!("exactly one pack must be claimable (the free pack)");
        }
        Ok(Catalog { packs })
    }

    pub fn get(&self, id: &str) -> Option<&Pack> {
        self.packs.iter().find(|p| p.id() == id)
    }

    /// The pack that free claims and new accounts receive.
    pub fn claimable(&self) -> &Pack {
        self.packs.iter().find(|p| p.recipe.claimable).unwrap()
    }
}
