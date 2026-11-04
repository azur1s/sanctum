//! Reforges and sockets: what one copy of an item has on top of its definition.
//!
//! A stack stores only ids (see [`Instance`]), never amounts, so edits to
//! `reforges.kdl` and gem definitions reach items that already exist. An id that no
//! longer resolves does nothing but stays on the stack, and works again if the
//! definition comes back.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{ItemDef, Slot, Stats, stat_info};

/// A socket of this type takes every gem; a gem of this type fits every socket.
pub const ANY_SOCKET: &str = "any";

/// What a stack remembers about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instance {
    pub reforge: Option<String>,
    /// By socket position. Empty sockets are None.
    pub gems: Vec<Option<String>>,
}

impl Instance {
    /// From the stack's `reforge` and `gems` strings.
    pub fn decode(reforge: Option<String>, gems: Option<String>) -> Self {
        let gems = gems
            .filter(|s| !s.is_empty())
            .map(|s| s.split(',').map(|id| (!id.is_empty()).then(|| id.to_string())).collect())
            .unwrap_or_default();
        Instance { reforge: reforge.filter(|s| !s.is_empty()), gems }
    }

    /// The `gems` string: ids in socket order, comma separated, blank when
    /// empty ("ruby,,opal"). None when no socket is filled.
    pub fn encode_gems(&self) -> Option<String> {
        let used = self.gems.iter().rposition(Option::is_some)? + 1;
        Some(self.gems[..used].iter().map(|g| g.as_deref().unwrap_or("")).collect::<Vec<_>>().join(","))
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
    !stat_info(stat).is_some_and(|i| i.main_hand_only) || (host.slot == Slot::MainHand && host.stats.contains_key(stat))
}

impl ReforgeDef {
    pub fn fits(&self, host: &ItemDef) -> bool {
        host.gem.is_none()
            && host.reforgeable
            && (self.slots.is_empty() || self.slots.contains(&host.slot))
            && (self.kinds.is_empty()
                || host.kind.as_ref().is_some_and(|k| self.kinds.iter().any(|f| f.eq_ignore_ascii_case(k))))
            && self.stats.keys().all(|s| stat_fits(host, s))
    }
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
        return Some(format!("{} is not a gem.", gem.name));
    };
    let Some(socket_type) = host.sockets.get(socket) else {
        return Some(format!("{} has no socket {}.", host.name, socket + 1));
    };
    if socket_type != ANY_SOCKET && gem_type != ANY_SOCKET && socket_type != gem_type {
        return Some(format!("{} is a {gem_type} gem; socket {} takes {socket_type}.", gem.name, socket + 1));
    }
    if let Some(stat) = gem.stats.keys().find(|s| !stat_fits(host, s)) {
        return Some(format!("{}'s {} only works on main-hand items that have that stat.", gem.name, stat.replace('_', "-")));
    }
    if host.slot != Slot::MainHand
        && let Some(skill) = gem.skills.iter().find(|s| s.skill.trigger.main_hand_only())
    {
        return Some(format!("{}'s {} only works on main-hand items.", gem.name, skill.name));
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

impl ItemDef {
    /// This item as one copy of it is: reforge prefix and stats added, gem
    /// stats and skills added. Everything that reads an item's stats or
    /// skills works on the result.
    pub fn with_instance(
        &self,
        instance: &Instance,
        gem: impl Fn(&str) -> Option<Arc<ItemDef>>,
        reforge: impl Fn(&str) -> Option<Arc<ReforgeDef>>,
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
        out
    }

    /// Lore lines for the sockets, or for what kind of socket a gem fits.
    pub(super) fn socket_lines(&self) -> Vec<String> {
        if let Some(gem) = &self.gem {
            return vec![if gem == ANY_SOCKET {
                "&8Fits any socket".to_string()
            } else {
                format!("&8Fits {gem} sockets")
            }];
        }
        self.socket_views
            .iter()
            .map(|view| match view {
                SocketView::Empty(t) if t == ANY_SOCKET => "&8◇ Empty socket".to_string(),
                SocketView::Empty(t) => format!("&8◇ Empty {t} socket"),
                SocketView::Filled(name) => format!("&f◆ {name}"),
                SocketView::Inactive(id) => format!("&8✖ {id} (inactive)"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::{LoreEntry, PlayerStats, StatFormats, link, parse_file, parse_reforges};

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
        def.with_instance(instance, |id| all.get(id).cloned(), |id| r.get(id).cloned())
    }

    #[test]
    fn instance_round_trips() {
        for gems in ["ruby", "ruby,,opal", ",opal"] {
            let i = Instance::decode(None, Some(gems.to_string()));
            assert_eq!(i.encode_gems().as_deref(), Some(gems));
        }
        let i = Instance::decode(Some("sharp".into()), Some("ruby,,opal".into()));
        assert_eq!(i.gem(0), Some("ruby"));
        assert_eq!(i.gem(1), None);
        assert_eq!(i.gem(2), Some("opal"));
        assert_eq!(i.gem(9), None);

        // Nothing left in the sockets means nothing to store.
        let mut i = Instance::decode(None, Some("ruby".into()));
        i.set_gem(0, None);
        assert_eq!(i.encode_gems(), None);
        assert_eq!(Instance::decode(Some(String::new()), Some(String::new())), Instance::default());
    }

    #[test]
    fn stats_add_up() {
        let all = items(SRC);
        let instance = Instance { reforge: Some("sharp".into()), gems: vec![Some("ruby".into()), Some("opal".into())] };
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
        let instance = Instance { reforge: None, gems: vec![None, Some("spark".into())] };
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
        let instance = Instance { reforge: Some("gone".into()), gems: vec![Some("gone".into()), Some("ruby".into())] };
        let sword = apply(&all["sword"], &instance, &all);
        assert_eq!(sword.name, "Sword");
        assert_eq!(sword.stats["damage"], 13.0);
        assert_eq!(sword.socket_views[0], SocketView::Inactive("gone".into()));
        assert_eq!(instance.encode_gems().as_deref(), Some("gone,ruby"));

        // So does a gem that stopped fitting, and one past the last socket.
        let instance = Instance { reforge: None, gems: vec![Some("opal".into()), None, Some("ruby".into())] };
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
        let instance = Instance { reforge: None, gems: vec![Some("ruby".into())] };
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

    #[test]
    fn examples_parse() {
        parse_reforges("r", include_str!("../../examples/reforges.kdl")).unwrap();
        let gems = parse_file("g", include_str!("../../examples/items/gems.kdl")).unwrap();
        assert!(gems.items.iter().all(|f| f.item.gem.is_some()));
    }
}
