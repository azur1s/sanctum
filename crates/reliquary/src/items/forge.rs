//! Reforges, sockets and upgrades: what one copy of an item has on top of its
//! definition.
//!
//! A stack stores only ids and upgrade counts (see [`Instance`]), never stat
//! amounts, so edits to `reforges.kdl`, `upgrades.kdl` and gem definitions reach
//! items that already exist. An id that no
//! longer resolves does nothing but stays on the stack, and works again if the
//! definition comes back.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::texts::{lore, messages, msg};

use super::tooltip::{self, FILL};
use super::{ItemDef, Slot, StatFormats, Stats, split_stat, stat_info, stat_key};

/// A socket of this type takes every gem; a gem of this type fits every socket.
pub const ANY_SOCKET: &str = "any";

/// `defense 1 element="inherit"` in an upgrade: follow the host item's elements.
pub const INHERIT: &str = "inherit";

/// What a stack remembers about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instance {
    pub reforge: Option<String>,
    /// By socket position. Empty sockets are None.
    pub gems: Vec<Option<String>>,
    /// Upgrade id -> how many were applied.
    pub upgrades: BTreeMap<String, u32>,
}

impl Instance {
    /// From the stack's `reforge`, `gems` and `upgrades` strings.
    pub fn decode(reforge: Option<String>, gems: Option<String>, upgrades: Option<String>) -> Self {
        let gems = gems
            .filter(|s| !s.is_empty())
            .map(|s| s.split(',').map(|id| (!id.is_empty()).then(|| id.to_string())).collect())
            .unwrap_or_default();
        let upgrades = upgrades
            .iter()
            .flat_map(|s| s.split(','))
            .filter_map(|entry| {
                let (id, count) = entry.split_once(':')?;
                Some((id.to_string(), count.parse().ok().filter(|&c| c > 0)?))
            })
            .collect();
        Instance { reforge: reforge.filter(|s| !s.is_empty()), gems, upgrades }
    }

    /// The `gems` string: ids in socket order, comma separated, blank when
    /// empty ("ruby,,opal"). None when no socket is filled.
    pub fn encode_gems(&self) -> Option<String> {
        let used = self.gems.iter().rposition(Option::is_some)? + 1;
        Some(self.gems[..used].iter().map(|g| g.as_deref().unwrap_or("")).collect::<Vec<_>>().join(","))
    }

    /// The `upgrades` string: "polish:7,shine:2". None when there are none.
    pub fn encode_upgrades(&self) -> Option<String> {
        (!self.upgrades.is_empty()).then(|| self.upgrades.iter().map(|(id, count)| format!("{id}:{count}")).collect::<Vec<_>>().join(","))
    }

    pub fn upgrade_count(&self, id: &str) -> u32 {
        self.upgrades.get(id).copied().unwrap_or(0)
    }

    pub fn gem(&self, socket: usize) -> Option<&str> {
        self.gems.get(socket)?.as_deref()
    }

    pub fn set_gem(&mut self, socket: usize, gem: Option<String>) {
        if self.gems.len() <= socket {
            self.gems.resize(socket + 1, None);
        }
        self.gems[socket] = gem;
    }
}

/// A reforge from `reforges.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReforgeDef {
    pub id: String,
    /// Put before the item's name: "Sharp" -> "Sharp Moonlit Edge".
    pub name: String,
    /// How likely a roll is to land here, relative to the other reforges.
    pub weight: f64,
    /// Only items in these slots. Empty means any.
    pub slots: Vec<Slot>,
    /// Only items of these kinds, ignoring case. Empty means any.
    pub kinds: Vec<String>,
    pub stats: Stats,
}

/// `damage` and `attack_speed` are the item's totals, so they can only be
/// added to a main-hand item that sets them itself. Anything else adds up.
fn stat_fits(host: &ItemDef, stat: &str) -> bool {
    !stat_info(stat).is_some_and(|i| i.main_hand_only) || (host.slot == Slot::MainHand && host.stats.keys().any(|k| split_stat(k).0 == split_stat(stat).0))
}

/// `slots` and `kinds` filters; empty means any.
fn filters_fit(slots: &[Slot], kinds: &[String], host: &ItemDef) -> bool {
    (slots.is_empty() || slots.contains(&host.slot))
        && (kinds.is_empty() || host.kind.as_ref().is_some_and(|k| kinds.iter().any(|f| f.eq_ignore_ascii_case(k))))
}

impl ReforgeDef {
    pub fn fits(&self, host: &ItemDef) -> bool {
        host.is_relic()
            && host.reforgeable
            && filters_fit(&self.slots, &self.kinds, host)
            && self.stats.keys().all(|s| stat_fits(host, s))
    }
}

/// An upgrade from `upgrades.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct UpgradeDef {
    pub id: String,
    pub name: String,
    /// Color codes for the name: the rarity of the item that applies this.
    /// Filled in by `link_upgrades`.
    pub color: String,
    /// How many one item can take.
    pub max: u32,
    /// The first one that fits an item is the one it gets.
    pub bonuses: Vec<UpgradeBonus>,
}

/// What one application of an upgrade adds to the items it fits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpgradeBonus {
    pub slots: Vec<Slot>,
    pub kinds: Vec<String>,
    pub stats: Stats,
    /// Plain stat name -> amount, split over the host's elements for that stat.
    pub inherited: Stats,
}

impl UpgradeDef {
    pub fn bonus_for(&self, host: &ItemDef) -> Option<&UpgradeBonus> {
        self.bonuses.iter().find(|b| b.fits(host))
    }
}

impl UpgradeBonus {
    fn fits(&self, host: &ItemDef) -> bool {
        host.is_relic()
            && filters_fit(&self.slots, &self.kinds, host)
            && self.stats.keys().chain(self.inherited.keys()).all(|s| stat_fits(host, s))
    }

    /// Adds `count` applications to `total`. `base` is the item's own stats, not
    /// what gems and reforges add, so socketing a fire gem can't change this.
    fn add_to(&self, total: &mut Stats, base: &Stats, count: u32) {
        let count = f64::from(count);
        for (stat, amount) in &self.stats {
            *total.entry(stat.clone()).or_insert(0.0) += amount * count;
        }
        for (stat, amount) in &self.inherited {
            for (key, share) in element_shares(base, stat) {
                *total.entry(key).or_insert(0.0) += amount * count * share;
            }
        }
    }
}

/// How `base` divides `stat` between its elements, as stat keys and fractions
/// adding up to 1. All of it is untagged if `base` has none of that stat.
fn element_shares(base: &Stats, stat: &str) -> Vec<(String, f64)> {
    let parts: Vec<(&String, f64)> =
        base.iter().filter(|(key, value)| split_stat(key).0 == stat && **value > 0.0).map(|(key, value)| (key, *value)).collect();
    let total: f64 = parts.iter().map(|(_, value)| value).sum();
    if parts.is_empty() {
        return vec![(stat_key(stat, None), 1.0)];
    }
    parts.into_iter().map(|(key, value)| (key.clone(), value / total)).collect()
}

/// Picks among `options` by weight; `roll` is a random number in 0.0..1.0.
/// Never lands on `current` if there is anything else to land on.
pub fn roll_reforge(options: &[Arc<ReforgeDef>], current: Option<&str>, roll: f64) -> Option<Arc<ReforgeDef>> {
    let others: Vec<&Arc<ReforgeDef>> = options.iter().filter(|r| Some(r.id.as_str()) != current).collect();
    let pool = if others.is_empty() { options.iter().collect() } else { others };
    let total: f64 = pool.iter().map(|r| r.weight).sum();
    let mut left = roll * total;
    for reforge in &pool {
        left -= reforge.weight;
        if left < 0.0 {
            return Some((*reforge).clone());
        }
    }
    pool.last().map(|r| (*r).clone())
}

/// Why `gem` can't go in socket `socket` of `host`, if it can't.
pub fn gem_problem(host: &ItemDef, socket: usize, gem: &ItemDef) -> Option<String> {
    let Some(gem_type) = &gem.gem else {
        return Some(msg(&messages::NOT_A_GEM, &[("gem", &gem.name)]));
    };
    let Some(socket_type) = host.sockets.get(socket) else {
        return Some(msg(&messages::NO_SUCH_SOCKET, &[("item", &host.name), ("socket", &(socket + 1))]));
    };
    if socket_type != ANY_SOCKET && gem_type != ANY_SOCKET && socket_type != gem_type {
        return Some(msg(&messages::WRONG_SOCKET, &[("gem", &gem.name), ("type", gem_type), ("socket", &(socket + 1)), ("takes", socket_type)]));
    }
    if let Some(stat) = gem.stats.keys().find(|s| !stat_fits(host, s)) {
        return Some(msg(&messages::GEM_STAT_MAIN_HAND, &[("gem", &gem.name), ("stat", &stat.replace('_', "-"))]));
    }
    if host.slot != Slot::MainHand
        && let Some(skill) = gem.skills.iter().find(|s| s.skill.trigger.main_hand_only())
    {
        return Some(msg(&messages::GEM_SKILL_MAIN_HAND, &[("gem", &gem.name), ("skill", &skill.name)]));
    }
    None
}

/// How one socket shows in the lore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketView {
    Empty(String),
    /// The gem's name with its color codes.
    Filled(String),
    /// A gem that does nothing: deleted, or no longer fits after an edit.
    Inactive(String),
}

/// How one upgrade shows in the lore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeView {
    Active { name: String, color: String, count: u32, max: u32 },
    /// An upgrade that does nothing: deleted, or no longer fits after an edit.
    Inactive(String),
}

/// Upgrades by id, with their names colored by the rarity of the item that
/// applies them. Several items can apply one: the first by id decides.
pub fn link_upgrades(upgrades: Vec<UpgradeDef>, items: &BTreeMap<String, Arc<ItemDef>>) -> Upgrades {
    upgrades
        .into_iter()
        .map(|mut upgrade| {
            if let Some(item) = items.values().find(|d| d.upgrade.as_deref() == Some(upgrade.id.as_str())) {
                upgrade.color = item.rarity.color.clone();
            }
            (upgrade.id.clone(), Arc::new(upgrade))
        })
        .collect()
}


/// Whether `max` pips fit on the line without widening the smallest tooltip.
/// Shorter names leave room for more. It goes by `max`, not the count, so the
/// pips don't vanish as an item fills up.
fn pips_fit(name: &str, color: &str, max: u32, formats: &StatFormats) -> bool {
    let without_pips = formats.lore.fill(&lore::UPGRADE_BAR, &[("color", &color), ("name", &name), ("full", &""), ("empty", &""), ("fill", &FILL), ("count", &max), ("max", &max)]);
    let pip = u64::from(tooltip::text_width(formats.lore.get(&lore::UPGRADE_PIP)));
    u64::from(max) * pip <= u64::from(tooltip::room_left(&without_pips, formats.tooltip.min_width))
}

impl ItemDef {
    /// This item as one copy of it is: reforge prefix and stats added, gem
    /// stats and skills added, upgrades added. Everything that reads an item's
    /// stats or skills works on the result.
    pub fn with_instance(
        &self,
        instance: &Instance,
        gem: impl Fn(&str) -> Option<Arc<ItemDef>>,
        reforge: impl Fn(&str) -> Option<Arc<ReforgeDef>>,
        upgrade: impl Fn(&str) -> Option<Arc<UpgradeDef>>,
    ) -> ItemDef {
        let mut out = self.clone();
        if let Some(r) = instance.reforge.as_deref().and_then(reforge).filter(|r| r.fits(self)) {
            out.name = format!("{} {}", r.name, self.name);
            add_stats(&mut out.stats, &r.stats);
        }
        for socket in 0..self.sockets.len() {
            let Some(id) = instance.gem(socket) else {
                out.socket_views.push(SocketView::Empty(self.sockets[socket].clone()));
                continue;
            };
            let Some(g) = gem(id).filter(|g| gem_problem(self, socket, g).is_none()) else {
                out.socket_views.push(SocketView::Inactive(id.to_string()));
                continue;
            };
            add_stats(&mut out.stats, &g.stats);
            for (index, skill) in g.skills.iter().enumerate() {
                let mut skill = skill.clone();
                skill.cooldown_group = Some(format!("reliquary:{}.{index}", g.id));
                out.skills.push(skill);
            }
            out.socket_views.push(SocketView::Filled(g.display_name()));
        }
        for (id, &count) in &instance.upgrades {
            let found = upgrade(id);
            let Some((def, bonus)) = found.as_ref().and_then(|u| Some((u, u.bonus_for(self)?))) else {
                out.upgrade_views.push(UpgradeView::Inactive(id.clone()));
                continue;
            };
            // Lowering `max` caps old items without forgetting what they had.
            let count = count.min(def.max);
            bonus.add_to(&mut out.stats, &self.stats, count);
            out.upgrade_views.push(UpgradeView::Active { name: def.name.clone(), color: def.color.clone(), count, max: def.max });
        }
        out
    }

    /// Lore lines for the upgrades, or how to use an upgrade item.
    pub(super) fn upgrade_lines(&self, formats: &StatFormats) -> Vec<String> {
        let texts = &formats.lore;
        if self.upgrade.is_some() {
            return vec![texts.fill(&lore::UPGRADE_ITEM_HINT, &[])];
        }
        self.upgrade_views
            .iter()
            .map(|view| match view {
                UpgradeView::Active { name, color, count, max } if pips_fit(name, color, *max, formats) => {
                    let pip = texts.get(&lore::UPGRADE_PIP);
                    let (full, empty) = (pip.repeat(*count as usize), pip.repeat((max - count) as usize));
                    texts.fill(&lore::UPGRADE_BAR, &[("color", color), ("name", name), ("full", &full), ("empty", &empty), ("fill", &FILL), ("count", count), ("max", max)])
                }
                UpgradeView::Active { name, color, count, max } => {
                    texts.fill(&lore::UPGRADE_SHORT, &[("color", color), ("name", name), ("fill", &FILL), ("count", count), ("max", max)])
                }
                UpgradeView::Inactive(id) => texts.fill(&lore::UPGRADE_INACTIVE, &[("id", id)]),
            })
            .collect()
    }

    /// Lore lines for the sockets, or for what kind of socket a gem fits.
    pub(super) fn socket_lines(&self, formats: &StatFormats) -> Vec<String> {
        let texts = &formats.lore;
        if let Some(gem) = &self.gem {
            return vec![if gem == ANY_SOCKET {
                texts.fill(&lore::GEM_FITS_ANY, &[])
            } else {
                texts.fill(&lore::GEM_FITS, &[("type", gem)])
            }];
        }
        self.socket_views
            .iter()
            .map(|view| match view {
                SocketView::Empty(t) if t == ANY_SOCKET => texts.fill(&lore::SOCKET_EMPTY_ANY, &[]),
                SocketView::Empty(t) => texts.fill(&lore::SOCKET_EMPTY, &[("type", t)]),
                SocketView::Filled(name) => texts.fill(&lore::SOCKET_FILLED, &[("gem", name)]),
                SocketView::Inactive(id) => texts.fill(&lore::SOCKET_INACTIVE, &[("id", id)]),
            })
            .collect()
    }
}

fn add_stats(total: &mut Stats, more: &Stats) {
    for (stat, value) in more {
        *total.entry(stat.clone()).or_insert(0.0) += value;
    }
}

/// Reforges by id, in the form the game keeps them.
pub type Reforges = BTreeMap<String, Arc<ReforgeDef>>;

/// Upgrades by id, in the form the game keeps them.
pub type Upgrades = BTreeMap<String, Arc<UpgradeDef>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::{LoreEntry, PlayerStats, StatFormats, link, parse_file, parse_reforges, parse_upgrades};

    fn items(src: &str) -> BTreeMap<String, Arc<ItemDef>> {
        let parsed = parse_file("t.kdl", src).unwrap();
        let rarities = vec![Arc::new(crate::items::RarityDef::default())];
        parsed
            .items
            .into_iter()
            .map(|f| link(f, &BTreeMap::new(), &rarities).unwrap())
            .map(|d| (d.id.clone(), Arc::new(d)))
            .collect()
    }

    const SRC: &str = r#"
        item "sword" {
            name "Sword"
            kind "Sword"
            base "minecraft:iron_sword"
            sockets "red" "any"
            stats { damage 10; health 2; }
        }
        item "hat" {
            name "Hat"
            kind "Helmet"
            base "minecraft:iron_helmet"
            sockets 1
        }
        item "plain" {
            name "Plain"
            base "minecraft:stick"
            reforgeable #false
        }
        item "ruby" {
            name "Ruby"
            base "minecraft:redstone"
            gem "red"
            stats { damage 3; health 1; }
        }
        item "opal" {
            name "Opal"
            base "minecraft:quartz"
            gem "blue"
            stats { health 4; }
        }
        item "spark" {
            name "Spark"
            base "minecraft:glowstone_dust"
            gem "any"
            skill "Zap" {
                trigger "right_click"
                cooldown 2.0
                message "zap"
            }
        }
    "#;

    fn reforges() -> Reforges {
        let src = r#"
            reforge "sharp" {
                name "Sharp"
                weight 3
                kinds "sword"
                stats { damage 2; }
            }
            reforge "sturdy" {
                name "Sturdy"
                stats { health 1; }
            }
            reforge "war" {
                name "War"
                stats { damage 1; }
            }
        "#;
        parse_reforges("r.kdl", src).unwrap().0.into_iter().map(|r| (r.id.clone(), Arc::new(r))).collect()
    }

    fn apply(def: &ItemDef, instance: &Instance, all: &BTreeMap<String, Arc<ItemDef>>) -> ItemDef {
        let r = reforges();
        def.with_instance(instance, |id| all.get(id).cloned(), |id| r.get(id).cloned(), |_| None)
    }

    #[test]
    fn instance_round_trips() {
        for gems in ["ruby", "ruby,,opal", ",opal"] {
            let i = Instance::decode(None, Some(gems.to_string()), None);
            assert_eq!(i.encode_gems().as_deref(), Some(gems));
        }
        let i = Instance::decode(Some("sharp".into()), Some("ruby,,opal".into()), None);
        assert_eq!(i.gem(0), Some("ruby"));
        assert_eq!(i.gem(1), None);
        assert_eq!(i.gem(2), Some("opal"));
        assert_eq!(i.gem(9), None);

        // Nothing left in the sockets means nothing to store.
        let mut i = Instance::decode(None, Some("ruby".into()), None);
        i.set_gem(0, None);
        assert_eq!(i.encode_gems(), None);
        assert_eq!(Instance::decode(Some(String::new()), Some(String::new()), Some(String::new())), Instance::default());
    }

    #[test]
    fn stats_add_up() {
        let all = items(SRC);
        let instance = Instance { reforge: Some("sharp".into()), gems: vec![Some("ruby".into()), Some("opal".into())], ..Default::default() };
        let sword = apply(&all["sword"], &instance, &all);
        assert_eq!(sword.name, "Sharp Sword");
        assert_eq!(sword.stats["damage"], 15.0);
        assert_eq!(sword.stats["health"], 7.0);
        // The definition is untouched.
        assert_eq!(all["sword"].stats["damage"], 10.0);
    }

    #[test]
    fn socket_types() {
        let all = items(SRC);
        let sword = &all["sword"];
        assert!(gem_problem(sword, 0, &all["ruby"]).is_none());
        assert!(gem_problem(sword, 0, &all["opal"]).unwrap().contains("takes red"));
        // An `any` socket or an `any` gem goes with everything.
        assert!(gem_problem(sword, 1, &all["opal"]).is_none());
        assert!(gem_problem(sword, 0, &all["spark"]).is_none());
        assert!(gem_problem(sword, 2, &all["ruby"]).unwrap().contains("no socket 3"));
        assert!(gem_problem(sword, 0, sword).unwrap().contains("not a gem"));
    }

    #[test]
    fn hand_stats_and_skills_stay_off_armor() {
        let all = items(SRC);
        let hat = &all["hat"];
        assert!(gem_problem(hat, 0, &all["ruby"]).unwrap().contains("damage"));
        assert!(gem_problem(hat, 0, &all["spark"]).unwrap().contains("Zap"));
        assert!(gem_problem(hat, 0, &all["opal"]).is_none());

        let r = reforges();
        assert!(!r["war"].fits(hat), "`damage` is the item's total, a helmet has none");
        assert!(r["war"].fits(&all["sword"]));
        assert!(r["sturdy"].fits(hat));
        assert!(!r["sharp"].fits(hat), "kinds filter");
        assert!(!r["sturdy"].fits(&all["plain"]), "reforgeable #false");
        assert!(!r["sturdy"].fits(&all["ruby"]), "gems are not reforged");
    }

    #[test]
    fn gem_skills_join_the_item() {
        let all = items(SRC);
        let instance = Instance { reforge: None, gems: vec![None, Some("spark".into())], ..Default::default() };
        let sword = apply(&all["sword"], &instance, &all);
        assert_eq!(sword.skills.len(), 1);
        assert_eq!(sword.skills[0].name, "Zap");
        // The cooldown follows the gem, so it stays put whatever else is socketed.
        assert_eq!(sword.skills[0].cooldown_group.as_deref(), Some("reliquary:spark.0"));
        assert_eq!(crate::items::cooldown_group(&sword, 0), "reliquary:spark.0");
    }

    #[test]
    fn stale_ids_do_nothing_but_survive() {
        let all = items(SRC);
        let instance = Instance { reforge: Some("gone".into()), gems: vec![Some("gone".into()), Some("ruby".into())], ..Default::default() };
        let sword = apply(&all["sword"], &instance, &all);
        assert_eq!(sword.name, "Sword");
        assert_eq!(sword.stats["damage"], 13.0);
        assert_eq!(sword.socket_views[0], SocketView::Inactive("gone".into()));
        assert_eq!(instance.encode_gems().as_deref(), Some("gone,ruby"));

        // So does a gem that stopped fitting, and one past the last socket.
        let instance = Instance { reforge: None, gems: vec![Some("opal".into()), None, Some("ruby".into())], ..Default::default() };
        let sword = apply(&all["sword"], &instance, &all);
        assert_eq!(sword.stats["health"], 2.0);
        assert_eq!(sword.socket_views.len(), 2);
    }

    #[test]
    fn rolls_follow_weight_and_change_the_reforge() {
        let r = reforges();
        let options: Vec<_> = r.values().cloned().collect(); // sharp 3, sturdy 1, war 1
        let id = |roll, current| roll_reforge(&options, current, roll).unwrap().id.clone();
        assert_eq!(id(0.0, None), "sharp");
        assert_eq!(id(0.59, None), "sharp");
        assert_eq!(id(0.61, None), "sturdy");
        assert_eq!(id(0.99, None), "war");
        for roll in [0.0, 0.3, 0.6, 0.99] {
            assert_ne!(id(roll, Some("sharp")), "sharp");
        }
        // Only one option: stay on it rather than fail.
        let one = [r["sharp"].clone()];
        assert_eq!(roll_reforge(&one, Some("sharp"), 0.5).unwrap().id.as_str(), "sharp");
        assert!(roll_reforge(&[], None, 0.5).is_none());
    }

    #[test]
    fn lore_shows_sockets() {
        let all = items(SRC);
        let formats = StatFormats { layout: vec![LoreEntry::Sockets], ..Default::default() };
        let instance = Instance { reforge: None, gems: vec![Some("ruby".into())], ..Default::default() };
        let lore = apply(&all["sword"], &instance, &all).lore_lines(&PlayerStats::new(), &formats);
        assert_eq!(lore, ["&f◆ Ruby", "&8◇ Empty socket"]);
        let lore = apply(&all["ruby"], &Instance::default(), &all).lore_lines(&PlayerStats::new(), &formats);
        assert_eq!(lore, ["&8Fits red sockets"]);
    }

    #[test]
    fn parser_rejects_bad_files() {
        let e = parse_reforges("r", "reforge \"a\" {\n  name \"A\"\n  weight 0\n}").unwrap_err().to_string();
        assert!(e.contains("weight"), "{e}");
        let e = parse_reforges("r", "reforge \"a\" {\n  name \"A\"\n  slots \"head\"\n  stats { damage 1; }\n}")
            .unwrap_err()
            .to_string();
        assert!(e.contains("main-hand"), "{e}");
        let e = parse_file("t", "item \"a\" {\n  name \"A\"\n  base \"x\"\n  gem \"red\"\n  sockets 1\n}").err().unwrap().to_string();
        assert!(e.contains("gem"), "{e}");
    }

    fn upgrades() -> Upgrades {
        let src = r#"
            upgrade "polish" {
                name "Polish"
                max 3
                bonus {
                    kinds "Sword"
                    stats { damage 1 element="inherit"; health 1; }
                }
                bonus {
                    slots "head" "chest" "legs" "feet"
                    stats { defense 2 element="inherit"; }
                }
            }
        "#;
        link_upgrades(parse_upgrades("u.kdl", src).unwrap().0, &BTreeMap::new())
    }

    fn with_upgrades(def: &ItemDef, instance: &Instance) -> ItemDef {
        let u = upgrades();
        def.with_instance(instance, |_| None, |_| None, |id| u.get(id).cloned())
    }

    fn polished(count: u32) -> Instance {
        Instance { upgrades: [("polish".to_string(), count)].into(), ..Default::default() }
    }

    const UPGRADE_SRC: &str = r#"
        item "sword" {
            name "Sword"
            kind "Sword"
            base "minecraft:iron_sword"
            stats { damage 5; damage 2 element="fire"; }
        }
        item "fire_helm" {
            name "Fire Helm"
            kind "Helmet"
            base "minecraft:iron_helmet"
            stats { defense 100 element="fire"; }
        }
        item "cap" {
            name "Cap"
            kind "Helmet"
            base "minecraft:leather_helmet"
        }
        item "stick" {
            name "Stick"
            base "minecraft:stick"
        }
        item "polish" {
            name "Polish"
            base "minecraft:iron_ingot"
            upgrade "polish"
        }
    "#;

    #[test]
    fn upgrade_counts_round_trip() {
        let i = Instance::decode(None, None, Some("polish:7,shine:2".into()));
        assert_eq!(i.upgrade_count("polish"), 7);
        assert_eq!(i.upgrade_count("shine"), 2);
        assert_eq!(i.upgrade_count("gone"), 0);
        assert_eq!(i.encode_upgrades().as_deref(), Some("polish:7,shine:2"));
        // Junk reads as nothing rather than breaking the item.
        assert_eq!(Instance::decode(None, None, Some("polish,shine:x,glow:0".into())), Instance::default());
        assert_eq!(Instance::default().encode_upgrades(), None);
    }

    #[test]
    fn upgrades_stack_and_inherit_in_proportion() {
        let all = items(UPGRADE_SRC);
        let sword = with_upgrades(&all["sword"], &polished(2));
        // 2 damage split 5:2 between neutral and fire.
        assert!((sword.stats["damage"] - (5.0 + 2.0 * 5.0 / 7.0)).abs() < 1e-9);
        assert!((sword.stats["damage.fire"] - (2.0 + 2.0 * 2.0 / 7.0)).abs() < 1e-9);
        assert_eq!(sword.stats["health"], 2.0);

        let helm = with_upgrades(&all["fire_helm"], &polished(3));
        assert_eq!(helm.stats["defense.fire"], 106.0);
        assert!(!helm.stats.contains_key("defense"));
        // The definition is untouched.
        assert_eq!(all["fire_helm"].stats["defense.fire"], 100.0);
    }

    #[test]
    fn inherit_without_that_stat_is_untagged() {
        let all = items(UPGRADE_SRC);
        let cap = with_upgrades(&all["cap"], &polished(1));
        assert_eq!(cap.stats["defense"], 2.0);
    }

    #[test]
    fn inherit_reads_the_definition_not_the_gems() {
        let all = items(&format!("{UPGRADE_SRC}\nitem \"coal\" {{\n name \"Coal\"\n base \"minecraft:coal\"\n gem \"any\"\n stats {{ defense 100 element=\"water\"; }}\n}}"));
        let u = upgrades();
        let mut instance = polished(1);
        instance.gems = vec![Some("coal".into())];
        let mut cap = (*all["cap"]).clone();
        cap.sockets = vec![ANY_SOCKET.to_string()];
        let got = cap.with_instance(&instance, |id| all.get(id).cloned(), |_| None, |id| u.get(id).cloned());
        assert_eq!(got.stats["defense.water"], 100.0);
        assert_eq!(got.stats["defense"], 2.0, "the gem's water must not pull the upgrade along");
    }

    #[test]
    fn first_fitting_bonus_wins_and_some_items_get_none() {
        let all = items(UPGRADE_SRC);
        let u = upgrades();
        assert_eq!(u["polish"].bonus_for(&all["sword"]), Some(&u["polish"].bonuses[0]));
        assert_eq!(u["polish"].bonus_for(&all["cap"]), Some(&u["polish"].bonuses[1]));
        assert!(u["polish"].bonus_for(&all["stick"]).is_none());
        assert!(u["polish"].bonus_for(&all["polish"]).is_none(), "upgrades don't take upgrades");
    }

    #[test]
    fn lowered_max_caps_and_stale_ids_survive() {
        let all = items(UPGRADE_SRC);
        let helm = with_upgrades(&all["fire_helm"], &polished(9));
        assert_eq!(helm.stats["defense.fire"], 106.0, "capped at max 3");
        assert_eq!(helm.upgrade_views[0], UpgradeView::Active { name: "Polish".into(), color: "&7".into(), count: 3, max: 3 });
        // The count itself stays, so raising `max` again gives it back.
        assert_eq!(polished(9).encode_upgrades().as_deref(), Some("polish:9"));

        let gone = Instance { upgrades: [("gone".to_string(), 2)].into(), ..Default::default() };
        let helm = with_upgrades(&all["fire_helm"], &gone);
        assert_eq!(helm.stats["defense.fire"], 100.0);
        assert_eq!(helm.upgrade_views[0], UpgradeView::Inactive("gone".into()));
    }

    #[test]
    fn upgrade_lore() {
        let all = items(UPGRADE_SRC);
        let formats = StatFormats { layout: vec![LoreEntry::Upgrades], ..Default::default() };
        let lore = with_upgrades(&all["cap"], &polished(2)).lore_lines(&PlayerStats::new(), &formats);
        assert!(lore[0].starts_with("&7Polish &7●●&8●") && lore[0].ends_with("&72/3"), "{lore:?}");
        let lore = all["polish"].lore_lines(&PlayerStats::new(), &formats);
        assert_eq!(lore, ["&8Hold in offhand: /relic upgrade"]);
        assert!(all["cap"].lore_lines(&PlayerStats::new(), &formats).is_empty());
    }

    #[test]
    fn upgrade_progress_is_right_aligned() {
        let all = items(UPGRADE_SRC);
        let formats = StatFormats { layout: vec![LoreEntry::Upgrades], ..Default::default() };
        let line = |max: u32, count: u32| {
            let mut cap = (*all["cap"]).clone();
            cap.upgrade_views.push(UpgradeView::Active { name: "Polish".into(), color: "&9".into(), count, max });
            cap.lore_lines(&PlayerStats::new(), &formats).remove(0)
        };
        // Same width whatever the numbers, so they end at the same pixel.
        let width = super::super::tooltip::text_width(&line(10, 1));
        assert_eq!(super::super::tooltip::text_width(&line(10, 10)), width);
        assert_eq!(super::super::tooltip::text_width(&line(3, 2)), width);
        assert_eq!(super::super::tooltip::text_width(&line(500, 123)), width);
        // Too many to fit: no pips.
        assert!(!line(500, 123).contains('●') && line(500, 123).ends_with("&7123/500"));
        // Filled pips take the name's color, empty ones stay dim.
        assert!(line(10, 2).starts_with("&9Polish &9●●&8●●●●●●●●"), "{}", line(10, 2));
        // Too big for the tooltip: it grows instead of cutting anything off.
        let huge = line(4_000_000_000, 3_999_999_999);
        assert!(!huge.contains('●') && huge.ends_with("&73999999999/4000000000"));
        assert!(super::super::tooltip::text_width(&huge) >= width);
    }

    #[test]
    fn shorter_names_fit_more_pips() {
        let formats = StatFormats::default();
        let most = |name: &str| (1..=200).rev().find(|&max| pips_fit(name, "&7", max, &formats)).unwrap_or(0);
        assert!(most("Polish") >= 10, "{}", most("Polish"));
        assert!(most("Oil") > most("Polish"));
        assert!(most("Polish") > most("Whetstone of the Long Night"));
        assert_eq!(most("A very long upgrade name that fills the whole tooltip up"), 0);
        println!("pips: Oil {} Polish {} long {}", most("Oil"), most("Polish"), most("Whetstone of the Long Night"));
    }

    #[test]
    fn upgrade_names_use_the_rarity_of_their_item() {
        let rare = Arc::new(crate::items::RarityDef { color: "&9".into(), ..Default::default() });
        let mut item = (*items(UPGRADE_SRC)["polish"]).clone();
        item.rarity = rare;
        let all = BTreeMap::from([("polish".to_string(), Arc::new(item))]);
        let src = "upgrade \"polish\" {\n  name \"Polish\"\n  max 1\n  bonus {\n    stats { health 1; }\n  }\n}\nupgrade \"orphan\" {\n  name \"Orphan\"\n  max 1\n  bonus {\n    stats { health 1; }\n  }\n}";
        let linked = link_upgrades(parse_upgrades("u", src).unwrap().0, &all);
        assert_eq!(linked["polish"].color, "&9");
        assert_eq!(linked["orphan"].color, "&7", "no item applies it");
    }

    #[test]
    fn upgrade_items_are_not_relics() {
        let all = items(UPGRADE_SRC);
        assert!(!all["polish"].is_relic());
        assert!(all["sword"].is_relic());
        let r = reforges();
        assert!(!r["sturdy"].fits(&all["polish"]), "upgrade items are not reforged");
    }

    #[test]
    fn upgrade_parser_rejects_bad_files() {
        let bad = |src: &str| parse_upgrades("u", src).unwrap_err().to_string();
        assert!(bad("upgrade \"a\" {\n  max 1\n  bonus {\n    stats { health 1; }\n  }\n}").contains("needs a `name`"));
        assert!(bad("upgrade \"a\" {\n  name \"A\"\n  bonus {\n    stats { health 1; }\n  }\n}").contains("needs a `max`"));
        assert!(bad("upgrade \"a\" {\n  name \"A\"\n  max 0\n  bonus {\n    stats { health 1; }\n  }\n}").contains("at least 1"));
        assert!(bad("upgrade \"a\" {\n  name \"A\"\n  max 1\n}").contains("needs a `bonus`"));
        assert!(bad("upgrade \"a\" {\n  name \"A\"\n  max 1\n  bonus {\n    kinds \"Sword\"\n  }\n}").contains("needs `stats`"));
        assert!(bad("upgrade \"a\" {\n  name \"A\"\n  max 1\n  bonus {\n    slots \"head\"\n    stats { damage 1 element=\"inherit\"; }\n  }\n}").contains("main-hand"));
        let one = "upgrade \"a\" {\n  name \"A\"\n  max 1\n  bonus {\n    stats { health 1; }\n  }\n}";
        assert!(bad(&format!("{one}\n{one}")).contains("defined twice"));
        assert!(bad("nope \"a\"").contains("only holds `upgrade`"));

        let bad_item = |extra: &str| parse_file("t", &format!("item \"a\" {{\n  name \"A\"\n  base \"x\"\n  upgrade \"polish\"\n  {extra}\n}}")).err().unwrap().to_string();
        assert!(bad_item("gem \"red\"").contains("upgrade item"));
        assert!(bad_item("sockets 1").contains("upgrade item"));
        assert!(bad_item("stats { health 1; }").contains("upgrade item"));
        assert!(bad_item("skill \"Zap\" {\n trigger \"right_click\"\n message \"zap\"\n}").contains("upgrade item"));

        // Plain stats and inherited ones come back apart, for the loader's checks.
        let (_, stats, inherited) = parse_upgrades("u", "upgrade \"a\" {\n  name \"A\"\n  max 1\n  bonus {\n    stats {\n      health 1\n      defense 1 element=\"inherit\"\n    }\n  }\n}").unwrap();
        assert_eq!(stats[0].0, "health");
        assert_eq!(inherited[0].0, "defense");
    }

    #[test]
    fn examples_parse() {
        parse_upgrades("u", include_str!("../../examples/configs/upgrades.kdl")).unwrap();
        let upgrade_items = parse_file("i", include_str!("../../examples/items/upgrades.kdl")).unwrap();
        assert!(upgrade_items.items.iter().all(|f| f.item.upgrade.is_some()));
        parse_reforges("r", include_str!("../../examples/configs/reforges.kdl")).unwrap();
        let gems = parse_file("g", include_str!("../../examples/items/gems.kdl")).unwrap();
        assert!(gems.items.iter().all(|f| f.item.gem.is_some()));
    }
}
