//! Item and skill definitions. Plain data with no Pumpkin types, so this
//! module (and the KDL loader next to it) can move to another server.
//!
//! Skills are built from small mechanics. Reusable skills live in a library
//! and declare `param`s; items use them and can override mana, cooldown,
//! chance and any param. Names such as particles and sounds are kept as
//! strings here; the server side checks them after loading.

use std::collections::BTreeMap;
use std::sync::Arc;

pub mod expr;
mod font_widths;
pub mod forge;
mod kdl;
mod tooltip;

pub use expr::{Expr, Params, PlayerStats, Scope};
pub use forge::{link_upgrades, Instance, ReforgeDef, Reforges, SocketView, UpgradeDef, UpgradeView, Upgrades};
pub use kdl::{parse_elements, parse_file, parse_kinds, parse_rarities, parse_reforges, parse_stat_formats, parse_stats_layout, parse_upgrades};
pub use tooltip::{expand_hex, Divider, Rgb};

// ---- Items ----------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ItemDef {
    pub id: String,
    pub name: String,
    /// Vanilla item this is built on, e.g. "minecraft:diamond_sword".
    pub base: String,
    /// Optional item model id (custom models need a resource pack).
    pub model: Option<String>,
    /// Filled in by `link`.
    pub rarity: Arc<RarityDef>,
    /// Shown next to the rarity, e.g. "Sword" -> "Epic Sword".
    pub kind: Option<String>,
    pub lore: Vec<String>,
    /// Where the item must be to give its stats.
    pub slot: Slot,
    /// Stat name (snake_case) -> amount.
    pub stats: Stats,
    pub skills: Vec<ItemSkill>,
    /// The type of each socket; `sockets 2` is two "any".
    pub sockets: Vec<String>,
    /// Set on gems: the type of socket it goes in. A gem only does anything socketed.
    pub gem: Option<String>,
    /// `reforgeable #false` keeps reforges off this item.
    pub reforgeable: bool,
    /// `unbreakable #true` stops the item losing durability.
    pub unbreakable: bool,
    /// `unbreakable #true hidden=#true` leaves out the client's "Unbreakable" line.
    pub hide_unbreakable: bool,
    /// `fake-enchant #true` gives the item the enchantment glint, with no
    /// enchantment behind it.
    pub fake_enchant: bool,
    /// Set on upgrade items: the upgrade in `upgrades.kdl` it applies.
    pub upgrade: Option<String>,
    /// Filled in by `with_instance`, for the lore.
    pub socket_views: Vec<SocketView>,
    /// Filled in by `with_instance`, for the lore.
    pub upgrade_views: Vec<UpgradeView>,
}

pub type Stats = BTreeMap<String, f64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    MainHand,
    OffHand,
    Head,
    Chest,
    Legs,
    Feet,
}

impl Slot {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.replace('-', "_").as_str() {
            "mainhand" | "main_hand" => Slot::MainHand,
            "offhand" | "off_hand" => Slot::OffHand,
            "head" => Slot::Head,
            "chest" => Slot::Chest,
            "legs" => Slot::Legs,
            "feet" => Slot::Feet,
            _ => return None,
        })
    }

    /// Guesses the slot from a vanilla item id, like "minecraft:iron_helmet".
    pub fn guess(base: &str) -> Self {
        let id = base.rsplit(':').next().unwrap_or(base);
        if id.ends_with("_helmet") || id.ends_with("_skull") || id.ends_with("_head") || id == "carved_pumpkin" {
            Slot::Head
        } else if id.ends_with("_chestplate") || id == "elytra" {
            Slot::Chest
        } else if id.ends_with("_leggings") {
            Slot::Legs
        } else if id.ends_with("_boots") {
            Slot::Feet
        } else if id == "shield" {
            Slot::OffHand
        } else {
            Slot::MainHand
        }
    }
}

/// Stats the plugin knows. Any other name is a custom stat: it only adds up,
/// for formulas like `self.magic_power`.
pub struct StatInfo {
    pub name: &'static str,
    /// Only allowed on main-hand items.
    pub main_hand_only: bool,
}

pub const BUILTIN_STATS: &[StatInfo] = &[
    StatInfo { name: "damage", main_hand_only: true },
    StatInfo { name: "attack_speed", main_hand_only: true },
    StatInfo { name: "health", main_hand_only: false },
    StatInfo { name: "defense", main_hand_only: false },
    StatInfo { name: "armor_toughness", main_hand_only: false },
    StatInfo { name: "speed", main_hand_only: false },
    StatInfo { name: "max_mana", main_hand_only: false },
    StatInfo { name: "mana_regen", main_hand_only: false },
    StatInfo { name: "crit_chance", main_hand_only: false },
    StatInfo { name: "crit_power", main_hand_only: false },
];

pub fn stat_info(name: &str) -> Option<&'static StatInfo> {
    let (base, _) = split_stat(name);
    BUILTIN_STATS.iter().find(|s| s.name == base)
}

/// How a stat is stored: "damage", or "damage.fire" for `damage 2 element="fire"`.
/// Keeping the element in the key means reforges, gems and equipment add up
/// tagged stats like any other, and formulas read them as `self.damage.fire`.
pub fn stat_key(stat: &str, element: Option<&str>) -> String {
    match element {
        Some(element) => format!("{stat}.{element}"),
        None => stat.to_string(),
    }
}

/// "damage.fire" -> ("damage", Some("fire")).
pub fn split_stat(key: &str) -> (&str, Option<&str>) {
    match key.split_once('.') {
        Some((stat, element)) => (stat, Some(element)),
        None => (key, None),
    }
}

/// A stat's amount with and without elements, like `damage` + `damage.fire`.
pub fn stat_total(stats: &Stats, stat: &str) -> f64 {
    stats.iter().filter(|(key, _)| split_stat(key).0 == stat).map(|(_, value)| value).sum()
}

/// "magic_power" -> "Magic Power".
pub fn stat_label(name: &str) -> String {
    name.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How stats look in lore, from `stats_format.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatFormats {
    /// Line for stats without their own, with `{name}`, `{sign}` and `{value}`.
    pub format: String,
    /// In lore order. Stats not listed come after, by name.
    pub stats: Vec<StatFormat>,
    /// What the lore shows in order, from `stats_layout.kdl`.
    pub layout: Vec<LoreEntry>,
    /// Element id -> name with its color codes, for stat names like "&cFire
    /// Damage". From `elements.kdl`.
    pub element_names: BTreeMap<String, String>,
    /// Element id -> color codes, for `{color:...}`. From `elements.kdl`.
    pub element_colors: BTreeMap<String, String>,
}

/// A line or section of item lore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoreEntry {
    Empty,
    Rarity,
    Lore,
    /// One stat, snake_case.
    Stat(String),
    /// Every stat without its own `Stat` entry.
    Stats,
    Skills,
    /// The item's sockets and what is in them.
    Sockets,
    /// The item's upgrades and how far along they are.
    Upgrades,
    /// A line across the tooltip, if `exists` has something to show (or always
    /// without one).
    Divider { divider: Divider, exists: Option<Box<LoreEntry>> },
}

impl LoreEntry {
    fn default_layout() -> Vec<Self> {
        vec![Self::Rarity, Self::Empty, Self::Lore, Self::Empty, Self::Stats, Self::Empty, Self::Upgrades, Self::Empty, Self::Sockets, Self::Empty, Self::Skills]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatFormat {
    /// snake_case.
    pub stat: String,
    /// Shown in lore; defaults to the title-cased stat, like "Magic Power".
    pub name: Option<String>,
    pub format: Option<String>,
    /// Left out of lore.
    pub hidden: bool,
}

/// Placeholders a stat format may use.
pub const STAT_PLACEHOLDERS: &[&str] = &["name", "sign", "value", "fill"];

/// Fills in `{color:&a}`: the element's color for a tagged stat, else `&a`.
fn fill_color(format: &str, element_color: Option<&str>) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(open) = rest.find("{color:") {
        let Some(close) = rest[open..].find('}') else { break };
        let default = &rest[open + "{color:".len()..open + close];
        out.push_str(&rest[..open]);
        out.push_str(element_color.unwrap_or(default));
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

impl Default for StatFormats {
    fn default() -> Self {
        StatFormats { format: "&7{name} &b{sign}{value}".into(), stats: Vec::new(), layout: LoreEntry::default_layout(), element_names: BTreeMap::new(), element_colors: BTreeMap::new() }
    }
}

impl StatFormats {
    /// Lore lines for the stats without their own layout entry, in order;
    /// hidden ones are left out.
    pub fn lines(&self, stats: &Stats) -> Vec<String> {
        let mut shown: Vec<(&String, f64)> = stats
            .iter()
            .filter(|(stat, _)| !self.layout.iter().any(|e| matches!(e, LoreEntry::Stat(s) if s == *stat)))
            .map(|(stat, &value)| (stat, value))
            .collect();
        // Stable, so unlisted stats keep their by-name order.
        // A tagged stat without its own entry goes with its stat ("damage.fire"
        // sorts right after "damage").
        let position = |stat: &str| self.stats.iter().position(|f| f.stat == stat);
        shown.sort_by_key(|(stat, _)| position(stat).or_else(|| position(split_stat(stat).0)).unwrap_or(usize::MAX));
        shown.into_iter().filter_map(|(stat, value)| self.line(stat, value)).collect()
    }

    /// The lore line for one stat; None if it is hidden.
    pub fn line(&self, stat: &str, value: f64) -> Option<String> {
        let (base, element) = split_stat(stat);
        let own = self.stats.iter().find(|f| f.stat == stat);
        // "damage.fire" looks like "damage" unless it has its own entry.
        let parent = self.stats.iter().find(|f| f.stat == base);
        if own.or(parent).is_some_and(|f| f.hidden) {
            return None;
        }
        let base_name = || parent.and_then(|f| f.name.clone()).unwrap_or_else(|| stat_label(base));
        let name = own.and_then(|f| f.name.clone()).unwrap_or_else(|| match element {
            Some(id) => format!("{} {}", self.element_names.get(id).cloned().unwrap_or_else(|| stat_label(id)), base_name()),
            None => base_name(),
        });
        let format = own.and_then(|f| f.format.as_deref())
            .or_else(|| parent.and_then(|f| f.format.as_deref()))
            .unwrap_or(&self.format);
        let color = element.and_then(|id| self.element_colors.get(id)).map(String::as_str).filter(|c| !c.is_empty());
        Some(
            fill_color(format, color)
                .replace("{name}", &name)
                .replace("{sign}", if value >= 0.0 { "+" } else { "" })
                .replace("{value}", &expr::format_number(value))
                .replace("{fill}", &tooltip::FILL.to_string()),
        )
    }
}

/// A rarity from `item_rarity.kdl`, e.g. common or epic.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RarityDef {
    pub id: String,
    /// Shown in the lore, e.g. "Epic" -> "Epic Sword".
    pub name: String,
    /// Legacy color codes before the item name, like "&5".
    pub color: String,
}

/// A skill as used by one item: the shared definition plus this item's
/// overrides, already merged.
#[derive(Debug, Clone)]
pub struct ItemSkill {
    pub skill: Arc<SkillDef>,
    pub name: String,
    pub mana_cost: u32,
    /// Seconds.
    pub cooldown: f32,
    /// 0.0..=1.0
    pub chance: f64,
    /// Defaults from the skill, with the item's overrides on top.
    pub params: Arc<Params>,
    /// Set on skills that came from a gem: the cooldown belongs to the gem.
    pub cooldown_group: Option<String>,
}

// ---- Skills ---------------------------------------------------------------------

/// A number in a skill: written out, or a formula like `"$damage * 2"`.
#[derive(Debug, Clone)]
pub enum Num {
    Lit(f64),
    Expr(Expr),
}

impl Num {
    pub fn get(&self, scope: Scope) -> f64 {
        match self {
            Num::Lit(v) => *v,
            Num::Expr(e) => e.eval(scope),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    RightClick,
    LeftClick,
    /// Melee hit on an entity. The hit entity is the `victim` target.
    Hit,
    /// The holder takes damage. The attacker (if any) is the `attacker` target.
    Damaged,
    /// The holder killed something. The dead entity is the `victim` target.
    Kill,
    /// The holder starts sneaking.
    Sneak,
    /// Every `n` seconds while the item is held or worn in its slot.
    Timer(u32),
    /// The item was put in its slot (held or worn).
    Equip,
    /// The item left its slot.
    Unequip,
}

impl Trigger {
    pub fn label(self) -> String {
        match self {
            Trigger::RightClick => "Right-click".into(),
            Trigger::LeftClick => "Left-click".into(),
            Trigger::Hit => "On hit".into(),
            Trigger::Damaged => "When hit".into(),
            Trigger::Kill => "On kill".into(),
            Trigger::Sneak => "Sneak".into(),
            Trigger::Timer(n) => format!("Every {n}s"),
            Trigger::Equip => "On equip".into(),
            Trigger::Unequip => "On unequip".into(),
        }
    }

    /// Clicks and melee hits only come from the main-hand item.
    pub fn main_hand_only(self) -> bool {
        matches!(self, Trigger::RightClick | Trigger::LeftClick | Trigger::Hit)
    }
}

#[derive(Debug, Clone)]
pub struct SkillDef {
    pub id: String,
    /// Shown in lore.
    pub name: String,
    pub description: Vec<String>,
    pub trigger: Trigger,
    /// Defaults; items may override them.
    pub chance: f64,
    pub mana_cost: u32,
    pub cooldown: f32,
    pub params: Params,
    /// Checked against the caster before anything runs.
    pub conditions: Vec<Condition>,
    pub actions: Actions,
    /// Runs instead of the skill when a `required` target block finds nothing.
    /// No mana is spent and no cooldown starts.
    pub on_fail: Actions,
}

/// Shared so delayed parts of a skill can keep running after the event ends.
pub type Actions = Arc<Vec<Action>>;

#[derive(Debug, Clone)]
pub enum Action {
    /// Runs `actions` once for each target the targeter finds.
    Target {
        targeter: Targeter,
        /// Each found entity must pass these.
        filter: Vec<Condition>,
        /// If nothing is found, the skill fails (see `SkillDef::on_fail`).
        /// Only checked for target blocks at the top of a skill.
        required: bool,
        actions: Actions,
    },
    /// Runs `actions` now, then again every `every` ticks, `times` in total.
    /// Does not block: the actions after it run right away.
    /// `var` names a param holding the iteration number, from 0, inside
    /// `actions` and `every`. `every` is worked out again for each gap, so the
    /// gap after iteration 2 sees `$i` = 2.
    Repeat { times: Num, every: Num, var: Option<String>, actions: Actions },
    /// Pauses this action list for some ticks.
    Wait(Num),
    /// A bolt from the caster's eyes along where it looks, moving `speed`
    /// blocks per tick. `trail` runs on each point it passes, `hit` on the
    /// first entity it touches, `end` where it stops without hitting one.
    Projectile { speed: Num, range: Num, radius: Num, trail: Actions, hit: Actions, end: Actions },
    /// Runs `actions` on the subject, then on the nearest new entity within
    /// `radius`, `jumps` times in total, `delay` ticks apart.
    Chain { jumps: Num, radius: Num, delay: Num, beam: Option<String>, actions: Actions },
    Mechanic(Mechanic),
}

#[derive(Debug, Clone)]
pub enum Targeter {
    /// The current subject (the caster at the top of a skill).
    Myself,
    /// The entity that was hit or killed (`hit` and `kill` skills).
    Victim,
    /// Whoever damaged the holder (`damaged` skills).
    Attacker,
    /// The first entity in the caster's line of sight.
    Ray { range: Num },
    /// All entities within `range` of the subject.
    Radius { range: Num },
    /// Entities within `range` and `angle` degrees of where the caster looks.
    Cone { range: Num, angle: Num },
    /// A point `distance` blocks in front of the caster, `height` above its feet.
    Forward { distance: Num, height: Num },
    /// Points on a flat arc in front of the caster.
    Arc { radius: Num, angle: Num, points: Num, height: Num },
    /// Points from the caster's eyes along where it looks, `step` apart.
    Line { range: Num, step: Num },
    /// Points on a flat circle around the subject.
    Ring { radius: Num, points: Num, height: Num },
}

impl Targeter {
    /// Location targeters give points, not entities.
    pub fn gives_points(&self) -> bool {
        matches!(
            self,
            Targeter::Forward { .. } | Targeter::Arc { .. } | Targeter::Line { .. } | Targeter::Ring { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub negate: bool,
    pub check: Check,
}

#[derive(Debug, Clone)]
pub enum Check {
    Sneaking,
    Sprinting,
    OnGround,
    InWater,
    Burning,
    IsPlayer,
    /// Entity type id, e.g. "zombie".
    Type(String),
    /// Health below this fraction of max health (0.0..=1.0).
    HealthBelow(f64),
}

#[derive(Debug, Clone)]
pub enum Mechanic {
    /// `amount + <kind's power stat> * scale`. `source` is the Minecraft
    /// damage type; when missing, the kind's source (or "generic") is used.
    /// `triggers_hit` also runs the holder's `hit` skills on the target, like a
    /// melee hit (but not from inside a skill that was itself started that way).
    /// `element` also adds `<power stat>.<element>` to the scaling and uses
    /// `elements.kdl` against the target.
    Damage { amount: Num, kind: Option<String>, element: Option<String>, scale: Num, source: Option<String>, triggers_hit: bool },
    Heal { amount: Num },
    Ignite { seconds: Num },
    /// Pushes the subject away from the caster.
    Knockback { strength: Num, up: Num },
    /// Status effect (players only for now).
    Effect { effect: String, seconds: Num, level: Num },
    Glow { seconds: Num },
    Particle { particle: String, count: Num, spread: Num, speed: Num },
    Sound { sound: String, volume: Num, pitch: Num },
    Lightning { effect_only: bool },
    /// Message to the caster. Supports `&` color codes.
    Message { text: String },
    /// Launches the subject where the caster looks.
    Dash { strength: Num, up: Num },
    /// Drags the subject toward the caster.
    Pull { strength: Num },
    /// Teleports the subject up to `distance` blocks where the caster looks,
    /// stopping before walls.
    Blink { distance: Num },
}

impl Mechanic {
    /// Mechanics that also work on a point (not only on an entity).
    pub fn works_on_points(&self) -> bool {
        matches!(
            self,
            Mechanic::Particle { .. } | Mechanic::Sound { .. } | Mechanic::Lightning { .. } | Mechanic::Message { .. }
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Mechanic::Damage { .. } => "damage",
            Mechanic::Heal { .. } => "heal",
            Mechanic::Ignite { .. } => "ignite",
            Mechanic::Knockback { .. } => "knockback",
            Mechanic::Effect { .. } => "effect",
            Mechanic::Glow { .. } => "glow",
            Mechanic::Particle { .. } => "particle",
            Mechanic::Sound { .. } => "sound",
            Mechanic::Lightning { .. } => "lightning",
            Mechanic::Message { .. } => "message",
            Mechanic::Dash { .. } => "dash",
            Mechanic::Pull { .. } => "pull",
            Mechanic::Blink { .. } => "blink",
        }
    }
}

// ---- Elements -----------------------------------------------------------------------

/// One element from `elements.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ElementDef {
    pub id: String,
    /// Shown in stat names, like "Fire" in "Fire Damage".
    pub name: String,
    /// Legacy color codes for the name and icon, like "&c". May be empty.
    pub color: String,
    /// Shown on damage numbers of this element. May be empty.
    pub icon: String,
    /// Target element -> damage multiplier, when the file sets one.
    pub multipliers: BTreeMap<String, f64>,
}

/// `elements.kdl`: how much damage each element deals to each other.
#[derive(Debug, Clone, PartialEq)]
pub struct Elements {
    /// Against a different element the attacker has no entry for.
    pub default: f64,
    /// Against the same element, when the attacker has no entry for it.
    pub same: f64,
    pub elements: BTreeMap<String, ElementDef>,
}

impl Default for Elements {
    fn default() -> Self {
        Elements { default: 1.0, same: 1.0, elements: BTreeMap::new() }
    }
}

impl Elements {
    /// One element against another. An element's own entry beats the defaults.
    pub fn multiplier(&self, attack: &str, target: &str) -> f64 {
        self.elements
            .get(attack)
            .and_then(|def| def.multipliers.get(target))
            .copied()
            .unwrap_or(if attack == target { self.same } else { self.default })
    }

    /// What goes before a damage number for these elements: each icon in its
    /// color, then a space. Elements without an icon are skipped.
    pub fn icons(&self, ids: &[String]) -> String {
        ids.iter()
            .filter_map(|id| self.elements.get(id))
            .filter(|def| !def.icon.is_empty())
            .map(|def| format!("{}{} ", def.color, def.icon))
            .collect()
    }

    /// An attack against a target's defense stats. The target's element is a
    /// mix weighted by amount, with untagged defense counting as neutral (1x).
    /// Neutral attacks and targets without defense are always 1x.
    pub fn against(&self, attack: Option<&str>, target: &Stats, is_defense: &dyn Fn(&str) -> bool) -> f64 {
        let Some(attack) = attack else { return 1.0 };
        let (mut weight, mut sum) = (0.0, 0.0);
        for (key, &value) in target {
            let (stat, element) = split_stat(key);
            if is_defense(stat) {
                let amount = value.max(0.0);
                weight += amount;
                sum += amount * element.map_or(1.0, |e| self.multiplier(attack, e));
            }
        }
        if weight > 0.0 { sum / weight } else { 1.0 }
    }

    /// A melee hit: each part of the weapon's `damage` against the target, by
    /// its share of the weapon's total. `damage 5` + `damage 2 element="fire"`
    /// against a target that takes 1.25x from fire is 5/7 + 2/7 × 1.25.
    pub fn melee(&self, attacker: &Stats, target: &Stats, is_defense: &dyn Fn(&str) -> bool) -> f64 {
        let (mut weight, mut sum) = (0.0, 0.0);
        for (key, &value) in attacker {
            let (stat, element) = split_stat(key);
            if stat == "damage" {
                let amount = value.max(0.0);
                weight += amount;
                sum += amount * self.against(element, target, is_defense);
            }
        }
        if weight > 0.0 { sum / weight } else { 1.0 }
    }
}

/// The elements in a weapon's `damage`, biggest part first.
pub fn damage_elements(stats: &Stats) -> Vec<String> {
    let mut parts: Vec<(&str, f64)> = stats
        .iter()
        .filter_map(|(key, &value)| match split_stat(key) {
            ("damage", Some(element)) if value > 0.0 => Some((element, value)),
            _ => None,
        })
        .collect();
    parts.sort_by(|a, b| b.1.total_cmp(&a.1));
    parts.into_iter().map(|(element, _)| element.to_string()).collect()
}

/// `defense 1 element="inherit"` needs a stat that means something per element.
/// `stat_at` holds plain stat names.
pub fn check_inherited_stats(stat_at: &[(String, String)], taggable: &std::collections::BTreeSet<String>) -> Vec<String> {
    stat_at
        .iter()
        .filter(|(stat, _)| !taggable.contains(stat))
        .map(|(stat, at)| {
            let list: Vec<&str> = taggable.iter().map(String::as_str).collect();
            format!("{at}: stat `{stat}` can't inherit an element (only: {})", list.join(", "))
        })
        .collect()
}

/// `damage 2 element="fire"` needs an element that exists, on a stat that means
/// something per element.
pub fn check_stat_elements(
    stat_at: &[(String, String)],
    taggable: &std::collections::BTreeSet<String>,
    elements: &Elements,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, at) in stat_at {
        let (stat, Some(element)) = split_stat(key) else { continue };
        if !taggable.contains(stat) {
            let list: Vec<&str> = taggable.iter().map(String::as_str).collect();
            problems.push(format!("{at}: stat `{stat}` can't have an element (only: {})", list.join(", ")));
        }
        if !elements.elements.contains_key(element) {
            let list: Vec<&str> = elements.elements.keys().map(String::as_str).collect();
            problems.push(format!("{at}: unknown element `{element}` (elements.kdl has: {})", list.join(", ")));
        }
    }
    problems
}

// ---- Damage kinds -------------------------------------------------------------------

/// A damage kind from `kinds.kdl`, e.g. magic or physical.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageKind {
    pub id: String,
    /// Stat (snake_case) that scales this damage, like "magic_power".
    pub power: Option<String>,
    /// Minecraft damage type, like "magic" (ignores armor) or "player_attack".
    pub source: String,
    /// Stat (snake_case) of the target that reduces this damage, like
    /// "magic_defense": damage × 100 / (100 + defense).
    pub defense: Option<String>,
    /// How damage numbers of this kind look.
    pub display: KindDisplay,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct KindDisplay {
    /// Text before the number, like an icon from a resource pack font.
    pub prefix: String,
    /// Legacy color codes for the number, like "&d".
    pub color: String,
}

// ---- Linking ----------------------------------------------------------------------

/// Which skill an item uses, before linking against the library.
#[derive(Debug, Clone)]
pub struct SkillUse {
    /// "file:line:col", for error messages.
    pub at: String,
    pub skill: SkillSource,
    pub name: Option<String>,
    pub mana_cost: Option<u32>,
    pub cooldown: Option<f32>,
    pub chance: Option<f64>,
    pub params: Params,
}

#[derive(Debug, Clone)]
pub enum SkillSource {
    /// `skill "id" ...` without a block: a skill from the library.
    Library(String),
    /// `skill "Name" { ... }`: a skill written inside the item.
    Inline(Arc<SkillDef>),
}

/// An item as read from a file, with its skills not yet linked.
#[derive(Debug, Clone)]
pub struct ItemFile {
    pub item: ItemDef,
    /// `rarity "id"` with its "file:line:col", if the item sets one.
    pub rarity: Option<(String, String)>,
    pub uses: Vec<SkillUse>,
    /// Each stat with its "file:line:col", to check custom stats are declared.
    pub stat_at: Vec<(String, String)>,
}

/// Custom stats must be declared (`stat "magic-power"`), so a typo like
/// `damge 5` is an error instead of a new stat nobody uses.
pub fn check_stats(file: &ItemFile, declared: &std::collections::BTreeSet<String>) -> Vec<String> {
    check_stat_names(&file.stat_at, declared)
}

/// `check_stats` for a list of stats with their "file:line:col".
pub fn check_stat_names(stat_at: &[(String, String)], declared: &std::collections::BTreeSet<String>) -> Vec<String> {
    stat_at
        .iter()
        .filter(|(stat, _)| stat_info(stat).is_none() && !declared.contains(split_stat(stat).0))
        .map(|(stat, at)| {
            let builtin: Vec<&str> = BUILTIN_STATS.iter().map(|s| s.name).collect();
            format!(
                "{at}: unknown stat `{stat}` (built-in: {}; declare custom ones with `stat \"{}\"`)",
                builtin.join(", "),
                stat.replace('_', "-")
            )
        })
        .collect()
}

/// Fills in each item's rarity and its skills from the library and its
/// overrides. `rarities` is in file order; items without one get the first.
pub fn link(
    file: ItemFile,
    library: &BTreeMap<String, Arc<SkillDef>>,
    rarities: &[Arc<RarityDef>],
) -> Result<ItemDef, Vec<String>> {
    let ItemFile { mut item, rarity, uses, .. } = file;
    let mut problems = Vec::new();
    let found = match &rarity {
        Some((id, _)) => rarities.iter().find(|r| &r.id == id),
        None => rarities.first(),
    };
    match (found, rarity) {
        (Some(r), _) => item.rarity = r.clone(),
        (None, Some((id, at))) => {
            let list: Vec<&str> = rarities.iter().map(|r| r.id.as_str()).collect();
            problems.push(format!("{at}: unknown rarity `{id}` (item_rarity.kdl has: {})", list.join(", ")));
        }
        (None, None) => problems.push(format!("item `{}`: item_rarity.kdl has no rarities", item.id)),
    }
    for u in uses {
        let skill = match u.skill {
            SkillSource::Inline(def) => def,
            SkillSource::Library(id) => match library.get(&id) {
                Some(def) => def.clone(),
                None => {
                    problems.push(format!("{}: unknown skill `{id}`", u.at));
                    continue;
                }
            },
        };
        if item.slot != Slot::MainHand && skill.trigger.main_hand_only() {
            problems.push(format!(
                "{}: skill `{}` triggers on {}, which only works for main-hand items (this item goes in {:?})",
                u.at,
                skill.id,
                skill.trigger.label().to_lowercase(),
                item.slot
            ));
            continue;
        }
        let mut params = skill.params.clone();
        for (key, value) in u.params {
            if !params.contains_key(&key) {
                let known: Vec<&str> = skill.params.keys().map(String::as_str).collect();
                let hint = if known.is_empty() { "it has no params".into() } else { format!("params: {}", known.join(", ")) };
                problems.push(format!("{}: skill `{}` has no param `{key}` ({hint})", u.at, skill.id));
                continue;
            }
            params.insert(key, value);
        }
        item.skills.push(ItemSkill {
            name: u.name.unwrap_or_else(|| skill.name.clone()),
            mana_cost: u.mana_cost.unwrap_or(skill.mana_cost),
            cooldown: u.cooldown.unwrap_or(skill.cooldown),
            chance: u.chance.unwrap_or(skill.chance),
            params: Arc::new(params),
            skill,
            cooldown_group: None,
        });
    }
    if problems.is_empty() { Ok(item) } else { Err(problems) }
}

// ---- Lore -------------------------------------------------------------------------

/// Lore and description lines wrap at this many visible characters.
pub const WRAP_WIDTH: usize = 30;

fn is_color(c: char) -> bool {
    c.is_ascii_hexdigit()
}

fn is_format(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'k'..='o')
}

/// Splits `text` on spaces into lines of at most `width` visible characters.
/// `&x` codes do not count. Minecraft resets formatting on every lore line,
/// so the codes active at the end of a line are repeated at the start of the
/// next. A single word longer than `width` gets a line of its own.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut visible = 0;
    // Active color and formats, as `&x` codes.
    let mut color = String::new();
    let mut formats = String::new();

    for word in expand_hex(text).split(' ') {
        let mut word_visible = 0;
        let mut chars = word.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '&' && chars.peek().is_some_and(|&n| is_color(n) || is_format(n) || n == 'r' || n == 'x') {
                chars.next();
            } else {
                word_visible += 1;
            }
        }
        if visible > 0 && visible + 1 + word_visible > width {
            lines.push(std::mem::take(&mut line));
            line = format!("{color}{formats}");
            visible = 0;
        } else if visible > 0 {
            line.push(' ');
            visible += 1;
        }
        line.push_str(word);
        visible += word_visible;

        // Track which codes are active after this word.
        let mut chars = word.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '&' {
                continue;
            }
            match chars.peek().copied() {
                // A hex color is one color, not seven codes. Unlike `&a`, the
                // parser leaves bold and the like on after it.
                Some('x') => {
                    chars.next();
                    let digits: String = chars.by_ref().take(12).collect();
                    color = format!("&x{digits}");
                    continue;
                }
                Some(n) if is_color(n) => {
                    color = format!("&{n}");
                    formats.clear();
                }
                Some(n) if is_format(n) => formats.push_str(&format!("&{n}")),
                Some('r') => {
                    color.clear();
                    formats.clear();
                }
                _ => continue,
            }
            chars.next();
        }
    }
    lines.push(line);
    lines
}

/// A line of lore, before its width is known.
enum Line<'a> {
    Text(String),
    Divider(&'a Divider),
}

impl Line<'_> {
    fn is_empty(&self) -> bool {
        matches!(self, Line::Text(text) if text.is_empty())
    }

    fn min_width(&self) -> u32 {
        match self {
            Line::Text(text) => tooltip::min_line_width(text),
            Line::Divider(divider) => divider.min_width(),
        }
    }

    fn render(self, width: u32) -> String {
        match self {
            Line::Text(text) if text.contains(tooltip::FILL) => tooltip::fill_line(&text, width),
            Line::Text(text) => text,
            Line::Divider(divider) => divider.render(width),
        }
    }
}

/// Cooldown group for one skill of one item. Shared by every copy of the item,
/// and for a gem's skills by every copy of the gem.
pub fn cooldown_group(item: &ItemDef, skill_index: usize) -> String {
    match &item.skills[skill_index].cooldown_group {
        Some(group) => group.clone(),
        None => format!("reliquary:{}.{skill_index}", item.id),
    }
}

impl ItemDef {
    /// Gems and upgrade items do nothing on their own: they go into other items.
    pub fn is_relic(&self) -> bool {
        self.gem.is_none() && self.upgrade.is_none()
    }

    /// Display name with legacy color codes.
    pub fn display_name(&self) -> String {
        format!("{}{}", self.rarity.color, self.name)
    }

    /// Lore lines with legacy color codes. `<expr>` placeholders are filled
    /// in with `stats` (the stats of whoever holds the item).
    pub fn lore_lines(&self, stats: &PlayerStats, formats: &StatFormats) -> Vec<String> {
        let mut lines: Vec<Line> = Vec::new();
        // An entry since the last line had nothing to show for this item.
        let mut missing = false;
        for entry in &formats.layout {
            if *entry == LoreEntry::Empty {
                // Do not leave a blank line around something that is not there.
                if !(missing && lines.last().is_none_or(Line::is_empty)) {
                    lines.push(Line::Text(String::new()));
                }
                missing = false;
                continue;
            }
            if let LoreEntry::Divider { divider, exists } = entry {
                missing = exists.as_ref().is_some_and(|e| self.lore_entry(e, stats, formats).is_empty());
                if !missing {
                    lines.push(Line::Divider(divider));
                }
                continue;
            }
            let block = self.lore_entry(entry, stats, formats);
            missing = block.is_empty();
            lines.extend(block.into_iter().map(Line::Text));
        }
        if missing && lines.last().is_some_and(Line::is_empty) {
            lines.pop();
        }

        // Tooltips are as wide as their widest line, the name included.
        let width = tooltip::tooltip_width(
            lines.iter().map(Line::min_width).chain([tooltip::text_width(&self.display_name())]),
        );
        lines.into_iter().map(|line| line.render(width)).collect()
    }

    fn lore_entry(&self, entry: &LoreEntry, stats: &PlayerStats, formats: &StatFormats) -> Vec<String> {
        match entry {
            LoreEntry::Empty => vec![String::new()],
            // Placed by `lore_lines`, which knows how wide they have to be.
            LoreEntry::Divider { .. } => Vec::new(),
            LoreEntry::Rarity => {
                let c = &self.rarity.color;
                vec![match &self.kind {
                    Some(kind) => format!("{c}{} {kind}", self.rarity.name),
                    None => format!("{c}{}", self.rarity.name),
                }]
            }
            LoreEntry::Lore => {
                let no_params = Params::new();
                let scope = Scope { params: &no_params, stats };
                self.lore.iter().flat_map(|l| wrap(&expr::render(&format!("&7&o{l}"), scope), WRAP_WIDTH)).collect()
            }
            LoreEntry::Stat(stat) => self.stats.get(stat).and_then(|&v| formats.line(stat, v)).into_iter().collect(),
            LoreEntry::Stats => formats.lines(&self.stats),
            LoreEntry::Skills => self.skill_lines(stats),
            LoreEntry::Sockets => self.socket_lines(),
            LoreEntry::Upgrades => self.upgrade_lines(),
        }
    }

    fn skill_lines(&self, stats: &PlayerStats) -> Vec<String> {
        let c = &self.rarity.color;
        let mut lines = Vec::new();
        for skill in &self.skills {
            let scope = Scope { params: &skill.params, stats };
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("{c}◆ &f{} &8· &7{}", skill.name, skill.skill.trigger.label()));
            for l in &skill.skill.description {
                let text = expr::render(&format!("&7{l}"), scope);
                lines.extend(wrap(&text, WRAP_WIDTH).into_iter().map(|w| format!("  {w}")));
            }
            let mut costs = Vec::new();
            if skill.mana_cost > 0 {
                costs.push(format!("{} mana", skill.mana_cost));
            }
            if skill.cooldown > 0.0 {
                costs.push(format!("{}s cooldown", skill.cooldown));
            }
            if skill.chance < 1.0 {
                costs.push(format!("{}% chance", (skill.chance * 100.0).round()));
            }
            if !costs.is_empty() {
                lines.push(format!("&8  {}", costs.join(" · ")));
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_counts_only_visible_chars() {
        let lines = wrap("1234567890 1234567890 1234567890 really long description", 30);
        assert_eq!(lines, ["1234567890 1234567890", "1234567890 really long", "description"]);
        // Color codes do not count towards the width.
        let lines = wrap("&a1234567890 &b1234567890 &c12345678", 30);
        assert_eq!(lines, ["&a1234567890 &b1234567890 &c12345678"]);
    }

    #[test]
    fn wrap_carries_formatting() {
        let lines = wrap("&7This ability deals &5&lvery high magic damage to everything", 20);
        assert_eq!(lines, ["&7This ability deals", "&7&5&lvery high magic", "&5&ldamage to everything"]);
        // A color code resets formats, &r resets everything.
        let lines = wrap("&l&cbold red then &rplain text here", 10);
        assert_eq!(lines, ["&l&cbold red", "&cthen &rplain", "text here"]);
    }

    #[test]
    fn wrap_carries_hex_colors() {
        // Both spellings come out the long way, and neither counts as width.
        let long = "&x&f&f&8&8&0&0";
        assert_eq!(wrap("&#ff8800one two three", 3), [format!("{long}one"), format!("{long}two"), format!("{long}three")]);
        assert_eq!(wrap(&format!("{long}one two"), 3), [format!("{long}one"), format!("{long}two")]);
        // Hex leaves formats on; a plain color after it takes over.
        assert_eq!(wrap("&l&#ff8800one two", 3), [format!("&l{long}one"), format!("{long}&ltwo")]);
        assert_eq!(wrap("&#ff8800one &ctwo three", 7), [format!("{long}one &ctwo"), "&cthree".to_string()]);
    }

    #[test]
    fn hex_names_look_right() {
        let mut parsed = parse_file("i.kdl", "item \"a\" {\n  name \"&#ff8800A\"\n  base \"x\"\n}").unwrap();
        assert_eq!(parsed.items.remove(0).item.name, "&#ff8800A");
        assert_eq!(tooltip::text_width("&#ff8800A"), tooltip::text_width("A"));
    }

    #[test]
    fn wrap_keeps_long_words_whole() {
        assert_eq!(wrap("a supercalifragilistic b", 10), ["a", "supercalifragilistic", "b"]);
        assert_eq!(wrap("", 10), [""]);
    }
}
