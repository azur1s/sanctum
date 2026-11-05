//! Pumpkin glue: loads item files, turns definitions into item stacks, and
//! wires up commands, events and the mana regen task.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use pumpkin_plugin_api::{
    Attribute, AttributeModifier, AttributeModifierSlot, Context, Entity, ItemAttributeModifier,
    ItemStack, ModifierOperation, Player, Server,
    command::{
        ArgumentType, Command, CommandError, CommandNode, CommandSender, CommandSuggestion,
        CommandSuggestions, ConsumedArgs, StringType, SuggestionRequest,
    },
    command_wit::Arg,
    commands::{CommandHandler, CommandSuggestionHandler},
    common::Hand,
    world::EquipmentSlot,
    data_components::DataComponent,
    events::{
        EntityDamageByEntityEvent, EventData, EventHandler, EventPriority, InteractAction,
        EntityDamageEvent, EntityDeathEvent, InventoryClickEvent, PlayerInteractEvent, PlayerJoinEvent,
        PlayerLeaveEvent, PlayerToggleSneakEvent,
    },
    permission::{Permission, PermissionDefault, PermissionLevel},
    persistent_data::PersistentDataHolder,
    scheduler::SchedulerExt,
};
use tracing::{info, warn};

use crate::engine::{self, Cast, text};
use crate::items::{self, DamageKind, Elements, Instance, ItemDef, Params, PlayerStats, ReforgeDef, Reforges, Scope, Slot, StatFormats, Trigger, UpgradeDef, Upgrades, forge};
use crate::{mana, stats};

pub(crate) const NS: &str = "reliquary";
const PERMISSION: &str = "reliquary:command.relic";
/// How far `/relic equip` reaches, in blocks.
const EQUIP_RANGE: f64 = 20.0;

/// Written to `items/` and `skills/` when those folders do not exist yet.
const EXAMPLES: [(&str, &str); 8] = [
    ("items/moonlit_edge.kdl", include_str!("../examples/items/moonlit_edge.kdl")),
    ("items/stormcaller.kdl", include_str!("../examples/items/stormcaller.kdl")),
    ("items/starweave.kdl", include_str!("../examples/items/starweave.kdl")),
    ("items/gems.kdl", include_str!("../examples/items/gems.kdl")),
    ("items/upgrades.kdl", include_str!("../examples/items/upgrades.kdl")),
    ("skills/blades.kdl", include_str!("../examples/skills/blades.kdl")),
    ("skills/storm.kdl", include_str!("../examples/skills/storm.kdl")),
    ("skills/starweave.kdl", include_str!("../examples/skills/starweave.kdl")),
];

/// Written to `kinds.kdl` when it does not exist yet.
const KINDS_EXAMPLE: &str = include_str!("../examples/kinds.kdl");

/// Written to `elements.kdl` when it does not exist yet.
const ELEMENTS_EXAMPLE: &str = include_str!("../examples/elements.kdl");

/// Written to `item_rarity.kdl` when it does not exist yet.
const RARITY_EXAMPLE: &str = include_str!("../examples/item_rarity.kdl");

/// Written to `stats_format.kdl` when it does not exist yet.
const STATS_FORMAT_EXAMPLE: &str = include_str!("../examples/stats_format.kdl");

/// Written to `stats_layout.kdl` when it does not exist yet.
const STATS_LAYOUT_EXAMPLE: &str = include_str!("../examples/stats_layout.kdl");

/// Written to `reforges.kdl` when it does not exist yet.
const REFORGES_EXAMPLE: &str = include_str!("../examples/reforges.kdl");

/// Written to `upgrades.kdl` when it does not exist yet.
const UPGRADES_EXAMPLE: &str = include_str!("../examples/upgrades.kdl");

static ITEMS: RwLock<BTreeMap<String, Arc<ItemDef>>> = RwLock::new(BTreeMap::new());
static REFORGES: RwLock<Reforges> = RwLock::new(BTreeMap::new());
static UPGRADES: RwLock<Upgrades> = RwLock::new(BTreeMap::new());
static KINDS: RwLock<BTreeMap<String, DamageKind>> = RwLock::new(BTreeMap::new());
/// None until the first successful load.
static ELEMENTS: RwLock<Option<Arc<Elements>>> = RwLock::new(None);
/// None until the first successful load.
static STAT_FORMATS: RwLock<Option<Arc<StatFormats>>> = RwLock::new(None);
static DATA_FOLDER: OnceLock<String> = OnceLock::new();

fn data_path(file: &str) -> String {
    format!("{}/{file}", DATA_FOLDER.get().map_or("data", |s| s.as_str()))
}

pub(crate) fn player_key(player: &Player) -> String {
    player.get_id().to_string()
}

pub(crate) fn item(id: &str) -> Option<Arc<ItemDef>> {
    ITEMS.read().unwrap().get(id).cloned()
}

// ---- Files --------------------------------------------------------------------

fn kdl_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            kdl_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kdl") {
            out.push(path);
        }
    }
    Ok(())
}

/// Loads every `.kdl` file under `skills/` and `items/` (either folder may
/// hold items and skills), then links items to their skills. All or nothing:
/// if anything has a problem, the current items stay and the problems are returned.
pub fn load_items() -> Result<usize, Vec<String>> {
    let root = data_path("");
    let root = Path::new(&root);
    for folder in ["items", "skills"] {
        let dir = root.join(folder);
        if dir.exists() {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(|e| vec![format!("{}: {e}", dir.display())])?;
        for (name, contents) in EXAMPLES.iter().filter(|(n, _)| n.starts_with(folder)) {
            std::fs::write(root.join(name), contents).map_err(|e| vec![format!("{name}: {e}")])?;
        }
        info!("wrote example {folder} to {}", dir.display());
    }

    let mut files = Vec::new();
    for folder in ["skills", "items"] {
        kdl_files(&root.join(folder), &mut files).map_err(|e| vec![format!("{folder}: {e}")])?;
    }
    files.sort();

    let mut item_files = Vec::new();
    let mut library = BTreeMap::new();
    let mut stat_refs = Vec::new();
    let mut kind_refs = Vec::new();
    let mut element_refs = Vec::new();
    let mut declared = std::collections::BTreeSet::new();
    let mut problems = Vec::new();

    // Damage kinds live in one file, written with the defaults if missing.
    let kinds_path = root.join("kinds.kdl");
    if !kinds_path.exists() {
        std::fs::write(&kinds_path, KINDS_EXAMPLE).map_err(|e| vec![format!("kinds.kdl: {e}")])?;
    }
    let kinds: BTreeMap<String, DamageKind> = match std::fs::read_to_string(&kinds_path)
        .map_err(|e| format!("kinds.kdl: {e}"))
        .and_then(|src| items::parse_kinds("kinds.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok(list) => list.into_iter().map(|k| (k.id.clone(), k)).collect(),
        Err(e) => {
            problems.push(e);
            BTreeMap::new()
        }
    };
    for kind in kinds.values() {
        // A kind's power stat counts as declared.
        declared.extend(kind.power.iter().chain(&kind.defense).filter(|s| items::stat_info(s).is_none()).cloned());
        if crate::names::damage_type(&kind.source).is_none() {
            problems.push(format!("kinds.kdl: kind `{}` has unknown source `{}`", kind.id, kind.source));
        }
    }
    // Elements, and the stats that can be tagged with one.
    let elements_path = root.join("elements.kdl");
    if !elements_path.exists() {
        std::fs::write(&elements_path, ELEMENTS_EXAMPLE).map_err(|e| vec![format!("elements.kdl: {e}")])?;
    }
    let elements = match std::fs::read_to_string(&elements_path)
        .map_err(|e| format!("elements.kdl: {e}"))
        .and_then(|src| items::parse_elements("elements.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok(elements) => elements,
        Err(e) => {
            problems.push(e);
            Elements::default()
        }
    };
    let mut taggable: std::collections::BTreeSet<String> = ["damage", "defense"].map(String::from).into();
    taggable.extend(kinds.values().flat_map(|k| k.power.iter().chain(&k.defense)).cloned());
    // Rarities too, in file order (the first is the default).
    let rarity_path = root.join("item_rarity.kdl");
    if !rarity_path.exists() {
        std::fs::write(&rarity_path, RARITY_EXAMPLE).map_err(|e| vec![format!("item_rarity.kdl: {e}")])?;
    }
    let rarities: Vec<Arc<items::RarityDef>> = match std::fs::read_to_string(&rarity_path)
        .map_err(|e| format!("item_rarity.kdl: {e}"))
        .and_then(|src| items::parse_rarities("item_rarity.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok(list) => list.into_iter().map(Arc::new).collect(),
        Err(e) => {
            problems.push(e);
            Vec::new()
        }
    };
    // How stats look in lore.
    let formats_path = root.join("stats_format.kdl");
    if !formats_path.exists() {
        std::fs::write(&formats_path, STATS_FORMAT_EXAMPLE).map_err(|e| vec![format!("stats_format.kdl: {e}")])?;
    }
    let (mut formats, mut format_refs) = match std::fs::read_to_string(&formats_path)
        .map_err(|e| format!("stats_format.kdl: {e}"))
        .and_then(|src| items::parse_stat_formats("stats_format.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok(parsed) => parsed,
        Err(e) => {
            problems.push(e);
            Default::default()
        }
    };
    // What the lore shows, in order.
    let layout_path = root.join("stats_layout.kdl");
    if !layout_path.exists() {
        std::fs::write(&layout_path, STATS_LAYOUT_EXAMPLE).map_err(|e| vec![format!("stats_layout.kdl: {e}")])?;
    }
    match std::fs::read_to_string(&layout_path)
        .map_err(|e| format!("stats_layout.kdl: {e}"))
        .and_then(|src| items::parse_stats_layout("stats_layout.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok((layout, refs)) => {
            formats.layout = layout;
            format_refs.extend(refs);
        }
        Err(e) => problems.push(e),
    }
    // Reforges.
    let reforges_path = root.join("reforges.kdl");
    if !reforges_path.exists() {
        std::fs::write(&reforges_path, REFORGES_EXAMPLE).map_err(|e| vec![format!("reforges.kdl: {e}")])?;
    }
    let (reforges, reforge_stats) = match std::fs::read_to_string(&reforges_path)
        .map_err(|e| format!("reforges.kdl: {e}"))
        .and_then(|src| items::parse_reforges("reforges.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok(parsed) => parsed,
        Err(e) => {
            problems.push(e);
            Default::default()
        }
    };
    let reforges: Reforges = reforges.into_iter().map(|r| (r.id.clone(), Arc::new(r))).collect();
    // Upgrades.
    let upgrades_path = root.join("upgrades.kdl");
    if !upgrades_path.exists() {
        std::fs::write(&upgrades_path, UPGRADES_EXAMPLE).map_err(|e| vec![format!("upgrades.kdl: {e}")])?;
    }
    let (upgrades, upgrade_stats, upgrade_inherited) = match std::fs::read_to_string(&upgrades_path)
        .map_err(|e| format!("upgrades.kdl: {e}"))
        .and_then(|src| items::parse_upgrades("upgrades.kdl", &src).map_err(|e| e.to_string()))
    {
        Ok(parsed) => parsed,
        Err(e) => {
            problems.push(e);
            Default::default()
        }
    };
    for path in files {
        let shown = path.strip_prefix(root).unwrap_or(&path).display().to_string().replace('\\', "/");
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| format!("{shown}: {e}"))
            .and_then(|src| items::parse_file(&shown, &src).map_err(|e| e.to_string()));
        match parsed {
            Ok(parsed) => {
                for skill in parsed.skills {
                    problems.extend(engine::validate(&skill));
                    if library.contains_key(&skill.id) {
                        problems.push(format!("{shown}: skill `{}` is defined twice", skill.id));
                    }
                    library.insert(skill.id.clone(), Arc::new(skill));
                }
                item_files.extend(parsed.items);
                stat_refs.extend(parsed.stat_refs);
                kind_refs.extend(parsed.kind_refs);
                element_refs.extend(parsed.element_refs);
                declared.extend(parsed.declared_stats);
            }
            Err(e) => problems.push(e),
        }
    }

    problems.extend(items::check_stat_names(&reforge_stats, &declared));
    problems.extend(items::check_stat_elements(&reforge_stats, &taggable, &elements));
    problems.extend(items::check_stat_names(&upgrade_stats, &declared));
    problems.extend(items::check_stat_names(&upgrade_inherited, &declared));
    problems.extend(items::check_stat_elements(&upgrade_stats, &taggable, &elements));
    problems.extend(items::check_inherited_stats(&upgrade_inherited, &taggable));

    let mut set = BTreeMap::new();
    for file in item_files {
        problems.extend(items::check_stats(&file, &declared));
        problems.extend(items::check_stat_elements(&file.stat_at, &taggable, &elements));
        // Inline skills are not in the library, so check them here.
        for u in &file.uses {
            if let items::SkillSource::Inline(skill) = &u.skill {
                problems.extend(engine::validate(skill));
            }
        }
        match items::link(file, &library, &rarities) {
            Ok(def) => {
                if set.contains_key(&def.id) {
                    problems.push(format!("item `{}` is defined twice", def.id));
                }
                set.insert(def.id.clone(), Arc::new(def));
            }
            Err(p) => problems.extend(p),
        }
    }
    // A socket type no gem has would stay empty forever: probably a typo.
    let gem_types: std::collections::BTreeSet<&str> = set.values().filter_map(|d: &Arc<ItemDef>| d.gem.as_deref()).collect();
    for def in set.values() {
        for socket in &def.sockets {
            if socket != forge::ANY_SOCKET && !gem_types.contains(socket.as_str()) && !gem_types.contains(forge::ANY_SOCKET) {
                problems.push(format!("item `{}`: no gem fits a `{socket}` socket", def.id));
            }
        }
    }
    let upgrades: Upgrades = items::link_upgrades(upgrades, &set);
    for def in set.values() {
        if let Some(id) = &def.upgrade
            && !upgrades.contains_key(id)
        {
            let list: Vec<&str> = upgrades.keys().map(String::as_str).collect();
            problems.push(format!("item `{}`: unknown upgrade `{id}` (upgrades.kdl has: {})", def.id, list.join(", ")));
        }
    }
    // `self.<stat>` must be a stat that exists (a typo otherwise reads 0).
    let known = stats::known_names(&declared);
    for (stat, at) in &stat_refs {
        if !known.contains(items::split_stat(stat).0) {
            let list: Vec<&str> = known.iter().map(String::as_str).collect();
            problems.push(format!("{at}: unknown stat `self.{stat}` (known: {})", list.join(", ")));
        }
    }
    for (stat, at) in &format_refs {
        if !known.contains(items::split_stat(stat).0) {
            let list: Vec<&str> = known.iter().map(String::as_str).collect();
            problems.push(format!("{at}: unknown stat `{stat}` (known: {})", list.join(", ")));
        }
    }
    problems.extend(items::check_stat_elements(&stat_refs, &taggable, &elements));
    problems.extend(items::check_stat_elements(&format_refs, &taggable, &elements));
    for (element, at) in element_refs {
        if !elements.elements.contains_key(&element) {
            let list: Vec<&str> = elements.elements.keys().map(String::as_str).collect();
            problems.push(format!("{at}: unknown element `{element}` (elements.kdl has: {})", list.join(", ")));
        }
    }
    for (kind, at) in kind_refs {
        if !kinds.contains_key(&kind) {
            let list: Vec<&str> = kinds.keys().map(String::as_str).collect();
            problems.push(format!("{at}: unknown damage kind `{kind}` (kinds.kdl has: {})", list.join(", ")));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let count = set.len();
    *ITEMS.write().unwrap() = set;
    *REFORGES.write().unwrap() = reforges;
    *UPGRADES.write().unwrap() = upgrades;
    *KINDS.write().unwrap() = kinds;
    formats.element_names = elements.elements.values().map(|e| (e.id.clone(), format!("{}{}", e.color, e.name))).collect();
    formats.element_colors = elements.elements.values().map(|e| (e.id.clone(), e.color.clone())).collect();
    *ELEMENTS.write().unwrap() = Some(Arc::new(elements));
    *STAT_FORMATS.write().unwrap() = Some(Arc::new(formats));
    Ok(count)
}

/// How stats look in lore, from stats_format.kdl.
fn stat_formats() -> Arc<StatFormats> {
    STAT_FORMATS.read().unwrap().clone().unwrap_or_default()
}

/// A damage kind from kinds.kdl.
pub(crate) fn kind(id: &str) -> Option<DamageKind> {
    KINDS.read().unwrap().get(id).cloned()
}

/// `elements.kdl`.
pub(crate) fn elements() -> Arc<Elements> {
    ELEMENTS.read().unwrap().clone().unwrap_or_default()
}

/// What a hit on a player is multiplied by: their `defense_stat` (including
/// what is tagged with an element) and the attack's element against their mix
/// of defense stats. `weapon` is the attacker's stats for a melee hit, which
/// takes its elements from their `damage` instead of `attack`.
pub(crate) fn hit_multiplier(defense_stat: Option<&str>, attack: Option<&str>, weapon: Option<&PlayerStats>, target: &PlayerStats) -> f64 {
    let defense = defense_stat.map_or(0.0, |stat| items::stat_total(target, stat)).max(0.0);
    let kinds = KINDS.read().unwrap();
    let is_defense = |stat: &str| stat == "defense" || kinds.values().any(|k| k.defense.as_deref() == Some(stat));
    let elements = elements();
    let element = match weapon {
        Some(weapon) => elements.melee(weapon, target, &is_defense),
        None => elements.against(attack, target, &is_defense),
    };
    100.0 / (100.0 + defense) * element
}

/// The first kind (by id) whose `source` is this Minecraft damage type.
pub(crate) fn kind_for_source(source: &str) -> Option<DamageKind> {
    KINDS.read().unwrap().values().find(|k| k.source == source).cloned()
}

fn load_mana() {
    let path = data_path("mana.toml");
    match std::fs::read_to_string(&path) {
        Ok(s) => match mana::load(&s) {
            Ok(n) => info!("loaded mana for {n} player(s)"),
            Err(e) => warn!("could not read {path}: {e}"),
        },
        Err(_) => info!("no {path} yet, everyone starts with full mana"),
    }
}

pub fn save_mana() {
    let path = data_path("mana.toml");
    let result = mana::save().and_then(|s| std::fs::write(&path, s).map_err(|e| e.to_string()));
    if let Err(e) = result {
        warn!("could not save {path}: {e}");
    }
}

// ---- Item stacks ----------------------------------------------------------------

/// Minecraft network encoding: VarInt length, then UTF-8 bytes.
fn write_string(out: &mut Vec<u8>, s: &str) {
    let mut len = s.len() as u32;
    loop {
        let byte = (len & 0x7f) as u8;
        len >>= 7;
        if len == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend_from_slice(s.as_bytes());
}

/// Network NBT string tag: type id 8, u16 length, then (modified) UTF-8.
/// Matches plain UTF-8 for anything but NUL and characters outside the BMP.
fn write_nbt_string(out: &mut Vec<u8>, s: &str) {
    out.push(8);
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// `&5Name` -> `§5Name`, for text the client formats itself.
fn section_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let is_code = chars.peek().is_some_and(|n| n.is_ascii_hexdigit() || matches!(n.to_ascii_lowercase(), 'k'..='o' | 'r'));
        out.push(if c == '&' && is_code { '§' } else { c });
    }
    out
}

fn attribute_slot(slot: Slot) -> AttributeModifierSlot {
    match slot {
        Slot::MainHand => AttributeModifierSlot::MainHand,
        Slot::OffHand => AttributeModifierSlot::OffHand,
        Slot::Head => AttributeModifierSlot::Head,
        Slot::Chest => AttributeModifierSlot::Chest,
        Slot::Legs => AttributeModifierSlot::Legs,
        Slot::Feet => AttributeModifierSlot::Feet,
    }
}

/// Adds an attribute modifier. Using the same id and slot as one of the base
/// item's modifiers replaces it.
fn set_attribute(stack: &ItemStack, slot: Slot, attribute: Attribute, id: &str, amount: f64, operation: ModifierOperation) {
    stack.add_attribute_modifier(&ItemAttributeModifier {
        attribute,
        modifier: AttributeModifier { id: id.into(), amount, operation },
        slot: attribute_slot(slot),
    });
}

/// The id vanilla armor pieces use for their armor modifier in this slot.
fn base_armor_id(slot: Slot) -> Option<&'static str> {
    match slot {
        Slot::Head => Some("minecraft:armor.helmet"),
        Slot::Chest => Some("minecraft:armor.chestplate"),
        Slot::Legs => Some("minecraft:armor.leggings"),
        Slot::Feet => Some("minecraft:armor.boots"),
        Slot::MainHand | Slot::OffHand => None,
    }
}

/// What this stack has on top of its definition.
pub(crate) fn instance_of(stack: &ItemStack) -> Instance {
    Instance::decode(stack.get_string(NS, "reforge"), stack.get_string(NS, "gems"), stack.get_string(NS, "upgrades"))
}

fn reforge(id: &str) -> Option<Arc<ReforgeDef>> {
    REFORGES.read().unwrap().get(id).cloned()
}

fn upgrade(id: &str) -> Option<Arc<UpgradeDef>> {
    UPGRADES.read().unwrap().get(id).cloned()
}

fn apply_instance(def: &ItemDef, instance: &Instance) -> ItemDef {
    def.with_instance(instance, item, reforge, upgrade)
}

/// The relic this stack is, with its reforge, gems and upgrades. Gems give
/// nothing until socketed and upgrade items nothing until used, so loose ones
/// are not relics here.
pub(crate) fn relic_of(stack: &ItemStack) -> Option<Arc<ItemDef>> {
    let def = item(&stack.get_string(NS, "id")?).filter(|d| d.is_relic())?;
    let instance = instance_of(stack);
    if instance == Instance::default() {
        return Some(def);
    }
    Some(Arc::new(apply_instance(&def, &instance)))
}

/// Builds the item stack. `stats` (the holder's) fill in lore placeholders.
pub(crate) fn build_stack(base: &ItemDef, instance: &Instance, count: u8, stats: &PlayerStats) -> ItemStack {
    let def = &apply_instance(base, instance);
    let stack = ItemStack::new(&def.base, count);
    // Workaround: Pumpkin sends `custom_name` as plain text, dropping colors
    // and `italic(false)`. `item_name` is not italic and is sent as a
    // translate key, which the client shows as-is with `§` codes applied.
    // Go back to `set_custom_name` once the server has the CustomName fix.
    let mut name = Vec::new();
    write_nbt_string(&mut name, &section_codes(&def.display_name()));
    stack.set_component(DataComponent::ItemName, &name);
    stack.set_lore(def.lore_lines(stats, &stat_formats()).iter().map(|l| text(l).italic(false)).collect());

    if let Some(model) = &def.model {
        let mut bytes = Vec::new();
        write_string(&mut bytes, model);
        stack.set_component(DataComponent::ItemModel, &bytes);
    }
    // Stats backed by vanilla attributes. The player has 1 base damage and
    // 4 base attack speed; the tooltip shows totals, so store the difference
    // under the vanilla ids to replace the base item's own values.
    let (slot, add) = (def.slot, ModifierOperation::Add);
    let attributes = if def.is_relic() { Some(&def.stats) } else { None };
    // Protection comes from the `defense` stat, so zero the base item's vanilla
    // armor (under its own id, or it would add up). `remove_attribute_modifiers`
    // can't do this: emptying the list falls back to the base item's values.
    if let Some(id) = base_armor_id(slot) {
        set_attribute(&stack, slot, Attribute::Armor, id, 0.0, add);
    }
    // Every part of `damage` is in the vanilla hit; `OnHit` sorts out elements.
    if let Some(stats) = attributes
        && stats.keys().any(|k| items::split_stat(k).0 == "damage")
    {
        set_attribute(&stack, slot, Attribute::AttackDamage, "minecraft:base_attack_damage", items::stat_total(stats, "damage") - 1.0, add);
    }
    for (stat, &value) in attributes.into_iter().flatten() {
        match stat.as_str() {
            "attack_speed" => set_attribute(&stack, slot, Attribute::AttackSpeed, "minecraft:base_attack_speed", value - 4.0, add),
            "health" => set_attribute(&stack, slot, Attribute::MaxHealth, "reliquary:health", value, add),
            "armor_toughness" => set_attribute(&stack, slot, Attribute::ArmorToughness, "reliquary:armor_toughness", value, add),
            // Percent: `speed 10` = 10% faster.
            "speed" => set_attribute(&stack, slot, Attribute::MovementSpeed, "reliquary:speed", value / 100.0, ModifierOperation::MultiplyBase),
            _ => {}
        }
    }
    // The lore shows the stats, so hide the client's "When in Main Hand" lines.
    // tooltip_display: hide_tooltip, then a VarInt count and component ids
    // (each below 128, so one VarInt byte). The modifiers still apply.
    stack.set_component(DataComponent::TooltipDisplay, &[0, 1, DataComponent::AttributeModifiers as u8]);
    // The cooldown overlay follows one group per item: use the first
    // right-click skill that has a cooldown.
    let overlay = def
        .skills
        .iter()
        .enumerate()
        .find(|(_, s)| engine::shows_overlay(s));
    if let Some((index, skill)) = overlay {
        // use_cooldown: f32 seconds (big endian), then an optional group string.
        let mut bytes = skill.cooldown.to_be_bytes().to_vec();
        bytes.push(1);
        write_string(&mut bytes, &engine::cooldown_group(def, index));
        stack.set_component(DataComponent::UseCooldown, &bytes);
    }
    stack.set_string(NS, "id", &def.id);
    if let Some(reforge) = &instance.reforge {
        stack.set_string(NS, "reforge", reforge);
    }
    if let Some(gems) = instance.encode_gems() {
        stack.set_string(NS, "gems", &gems);
    }
    if let Some(upgrades) = instance.encode_upgrades() {
        stack.set_string(NS, "upgrades", &upgrades);
    }
    stack
}

/// The relic in the player's main hand.
fn held_relic(player: &Player) -> Option<Arc<ItemDef>> {
    relic_of(&player.get_item_in_hand(Hand::Right)?)
}

/// Puts a stack into an inventory slot. The selected hotbar slot goes through
/// `set_item_in_hand`: only that path makes the server re-apply the held
/// item's attack damage and speed (plain `set_item` leaves the old values).
fn put_item(player: &Player, slot: u32, stack: ItemStack) {
    if slot == u32::from(player.get_selected_slot()) {
        player.set_item_in_hand(Hand::Right, Some(stack));
    } else {
        player.get_inventory().as_inventory().set_item(slot, Some(stack));
    }
}

/// Rebuilds every relic in the player's inventory from the current files,
/// so name, lore and stats follow edits.
fn refresh_inventory(player: &Player) {
    let stats = stats::current(player);
    let rebuild = |stack: Option<ItemStack>| {
        let stack = stack?;
        let def = item(&stack.get_string(NS, "id")?)?;
        Some(build_stack(&def, &instance_of(&stack), stack.get_count(), &stats))
    };
    let inv = player.get_inventory();
    // The plugin API's generic `set_item` only writes slots 0-35 of a player
    // inventory, so armor and offhand go through their own setters.
    for slot in 0..36 {
        if let Some(stack) = rebuild(inv.as_inventory().get_item(slot)) {
            put_item(player, slot, stack);
        }
    }
    if let Some(stack) = rebuild(inv.get_helmet()) {
        inv.set_helmet(Some(stack));
    }
    if let Some(stack) = rebuild(inv.get_chestplate()) {
        inv.set_chestplate(Some(stack));
    }
    if let Some(stack) = rebuild(inv.get_leggings()) {
        inv.set_leggings(Some(stack));
    }
    if let Some(stack) = rebuild(inv.get_boots()) {
        inv.set_boots(Some(stack));
    }
    if let Some(stack) = rebuild(inv.get_off_hand()) {
        player.set_item_in_hand(Hand::Left, Some(stack));
    }
}

fn max_mana(stats: &PlayerStats) -> u32 {
    stats.get("max_mana").copied().unwrap_or(0.0).max(0.0) as u32
}

fn show_mana(player: &Player, mana: u32, max: u32) {
    player.show_actionbar(text(&format!("&b✦ Mana {mana}/{max}")));
}

/// Runs every skill of `items` whose trigger passes `when`. `other` is the
/// `victim` / `attacker` target.
pub(crate) fn fire(
    server: &Server,
    player: Player,
    other: Option<Entity>,
    items: &[Arc<ItemDef>],
    when: impl Fn(Trigger) -> bool,
) {
    fire_from(server, player, other, items, when, false);
}

/// `fire`, marking the casts as started by `triggers-hit` when `from_proc`.
fn fire_from(
    server: &Server,
    player: Player,
    other: Option<Entity>,
    items: &[Arc<ItemDef>],
    when: impl Fn(Trigger) -> bool,
    from_proc: bool,
) {
    if !items.iter().any(|d| d.skills.iter().any(|s| when(s.skill.trigger))) {
        return;
    }
    let key = player_key(&player);
    let stats = Arc::new(stats::for_cast(&player));
    let cast = Cast { server, caster: player, victim: other, params: Default::default(), stats: stats.clone(), from_proc };
    let mut used = false;
    for def in items {
        for (index, skill) in def.skills.iter().enumerate() {
            if when(skill.skill.trigger) {
                cast.params.replace(skill.params.clone());
                used |= engine::try_cast(&cast, def, index);
            }
        }
    }
    if used {
        show_mana(&cast.caster, mana::get(&key), max_mana(&stats));
    }
}

/// Runs the caster's `hit` skills on `target`, for `damage ... triggers-hit=#true`.
pub(crate) fn proc_hit(server: &Server, caster: &Player, target: &Entity) {
    let key = player_key(caster);
    let Some(player) = server.get_all_players().into_iter().find(|p| player_key(p) == key) else { return };
    let Some(def) = held_relic(&player) else { return };
    let id = target.get_id();
    let victim = player.as_entity().get_nearby_entities(32.0, 32.0, 32.0).into_iter().find(|e| e.get_id() == id);
    if victim.is_some() {
        fire_from(server, player, victim, &[def], |t| t == Trigger::Hit, true);
    }
}

/// Fires `trigger` on the relic in the main hand (clicks and melee hits).
fn fire_held(server: &Server, player: Player, other: Option<Entity>, trigger: Trigger) {
    let Some(def) = held_relic(&player) else { return };
    // A skill started by a hit never causes another hit (`triggers-hit`).
    fire_from(server, player, other, &[def], |t| t == trigger, trigger == Trigger::Hit);
}

// ---- Commands -------------------------------------------------------------------

fn fail(msg: &str) -> CommandError {
    CommandError::CommandFailed(text(&format!("&c{msg}")))
}

/// All items, sorted by id.
pub(crate) fn all_items() -> Vec<Arc<ItemDef>> {
    ITEMS.read().unwrap().values().cloned().collect()
}

/// Puts one of item `id` into the first free main inventory slot.
/// Returns the item's display name.
pub(crate) fn give(player: &Player, id: &str) -> Result<String, String> {
    let def = item(id).ok_or_else(|| format!("Unknown item '{id}'. Try /relic list."))?;
    let inv = player.get_inventory().as_inventory();
    let slot = (0..36).find(|&s| inv.get_item(s).is_none()).ok_or("Inventory full.")?;
    put_item(player, slot, build_stack(&def, &Instance::default(), 1, &stats::current(player)));
    Ok(def.display_name())
}

/// The player named in the `player` argument, or the sender.
fn target_player(sender: &CommandSender, args: &ConsumedArgs) -> Result<Player, CommandError> {
    if let Arg::Players(mut players) = args.get_value("player") {
        return players.pop().ok_or_else(|| fail("No player found."));
    }
    sender.as_player().ok_or_else(|| fail("Name a player when using this from the console."))
}

/// `/relic stats [player]`: totals and which item gives what.
struct StatsCmd;
impl CommandHandler for StatsCmd {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = target_player(&sender, &args)?;
        let total = stats::current(&player);
        let key = player_key(&player);
        sender.send_message(text(&format!(
            "&e{}'s stats &7(mana {}/{})",
            player.get_name(),
            mana::get(&key),
            max_mana(&total)
        )));
        for (stat, value) in &total {
            sender.send_message(text(&format!("&7- {stat}: &f{}", items::expr::format_number(*value))));
        }
        sender.send_message(text("&eFrom:"));
        sender.send_message(text(&format!(
            "&7- base: {}",
            stats::BASE.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
        )));
        for source in stats::equipped(&player) {
            let list: Vec<String> = source
                .item
                .stats
                .iter()
                .map(|(k, v)| format!("{k} {}", items::expr::format_number(*v)))
                .collect();
            let list = if list.is_empty() { "no stats".to_string() } else { list.join(", ") };
            sender.send_message(text(&format!("&7- {:?}: {} &7{list}", source.slot, source.item.display_name())));
        }
        Ok(1)
    }
}

/// `/relic eval <formula>`: evaluates a formula with your stats.
struct Eval;
impl CommandHandler for Eval {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let src = match args.get_value("formula") {
            Arg::Simple(s) | Arg::Msg(s) => s,
            _ => return Err(fail("Missing formula.")),
        };
        let e = items::expr::parse(&src).map_err(|e| fail(&format!("at {}: {}", e.at + 1, e.message)))?;
        let (params, stats) = (Params::new(), stats::for_cast(&player));
        let value = e.eval(Scope { params: &params, stats: &stats });
        sender.send_message(text(&format!("&7{src} &f= &b{value}")));
        Ok(1)
    }
}

/// `/relic mana <amount>`: sets your mana (capped at max mana).
struct SetMana;
impl CommandHandler for SetMana {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let Arg::Num(Ok(n)) = args.get_value("amount") else {
            return Err(fail("Expected a whole number."));
        };
        let amount = match n {
            pumpkin_plugin_api::command_wit::Number::Int32(i) => i.max(0) as u32,
            _ => return Err(fail("Expected a whole number.")),
        };
        let max = max_mana(&stats::current(&player));
        mana::set(&player_key(&player), amount.min(max));
        show_mana(&player, amount.min(max), max);
        Ok(1)
    }
}

struct Give;
impl CommandHandler for Give {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let Arg::Simple(id) = args.get_value("id") else {
            return Err(fail("Missing item id."));
        };
        let name = give(&player, &id).map_err(|e| fail(&e))?;
        sender.send_message(text(&format!("&aGave {name}")));
        Ok(1)
    }
}

/// `/relic equip <id>`: puts a relic on the mob you are looking at, in the slot
/// the relic is made for.
struct Equip;
impl CommandHandler for Equip {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let Arg::Simple(id) = args.get_value("id") else {
            return Err(fail("Missing item id."));
        };
        let def = item(&id).filter(|d| d.is_relic()).ok_or_else(|| fail(&format!("No relic `{id}`.")))?;
        let hit = player.ray_trace_entity(EQUIP_RANGE).ok_or_else(|| fail("Look at a mob first."))?;
        let living = hit.entity.as_living().filter(|l| l.is_mob()).ok_or_else(|| fail("Look at a mob first."))?;
        let slot = match def.slot {
            Slot::MainHand => EquipmentSlot::MainHand,
            Slot::OffHand => EquipmentSlot::OffHand,
            Slot::Head => EquipmentSlot::Head,
            Slot::Chest => EquipmentSlot::Chest,
            Slot::Legs => EquipmentSlot::Legs,
            Slot::Feet => EquipmentSlot::Feet,
        };
        living.set_equipment(slot, Some(build_stack(&def, &Instance::default(), 1, &PlayerStats::new())));
        sender.send_message(text(&format!("&aPut {} on the mob ({:?})", def.display_name(), def.slot)));
        Ok(1)
    }
}

struct Menu;
impl CommandHandler for Menu {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        crate::menu::open(&player, 0);
        Ok(1)
    }
}

struct List;
impl CommandHandler for List {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let lines: Vec<String> = ITEMS
            .read()
            .unwrap()
            .values()
            .map(|def| format!("&7- &f{} &7({})", def.id, def.display_name()))
            .collect();
        sender.send_message(text(&format!("&eItems ({}):", lines.len())));
        for line in lines {
            sender.send_message(text(&line));
        }
        Ok(1)
    }
}

struct Reload;
impl CommandHandler for Reload {
    fn handle(&self, sender: CommandSender, server: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        match load_items() {
            Ok(n) => {
                for player in server.get_all_players() {
                    refresh_inventory(&player);
                }
                info!("reloaded {n} item(s)");
                sender.send_message(text(&format!("&aReloaded {n} item(s).")));
                Ok(1)
            }
            Err(problems) => {
                sender.send_message(text(&format!("&cReload failed, kept the old items ({} problem(s)):", problems.len())));
                for problem in &problems {
                    warn!("{problem}");
                    sender.send_message(text(&format!("&7- {problem}")));
                }
                Ok(0)
            }
        }
    }
}

/// The relic in the main hand, to change: its definition and what it has on it.
fn held_for_forge(player: &Player) -> Result<(Arc<ItemDef>, Instance), CommandError> {
    let stack = player.get_item_in_hand(Hand::Right).ok_or_else(|| fail("Hold an item in your main hand."))?;
    let def = stack
        .get_string(NS, "id")
        .and_then(|id| item(&id))
        .ok_or_else(|| fail("That isn't a Reliquary item."))?;
    if !def.is_relic() {
        return Err(fail("Hold the item you want to change, not a gem or upgrade."));
    }
    if stack.get_count() != 1 {
        return Err(fail("Hold just one of it."));
    }
    Ok((def, instance_of(&stack)))
}

/// Puts the changed relic back in the main hand.
fn forge_result(player: &Player, def: &ItemDef, instance: &Instance) {
    player.set_item_in_hand(Hand::Right, Some(build_stack(def, instance, 1, &stats::current(player))));
}

/// The number in an optional 1-based `socket` argument, as an index.
fn socket_arg(args: &ConsumedArgs) -> Option<usize> {
    match args.get_value("socket") {
        Arg::Num(Ok(pumpkin_plugin_api::command_wit::Number::Int32(n))) if n >= 1 => Some(n as usize - 1),
        _ => None,
    }
}

/// `/relic reforge [id]`: rolls a new reforge, or sets the one named.
struct Reforge;
impl CommandHandler for Reforge {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let (def, mut instance) = held_for_forge(&player)?;
        if !def.reforgeable {
            return Err(fail(&format!("{} can't be reforged.", def.name)));
        }
        let options: Vec<Arc<ReforgeDef>> = REFORGES.read().unwrap().values().filter(|r| r.fits(&def)).cloned().collect();
        let chosen = match args.get_value("id") {
            Arg::Simple(id) if !id.is_empty() => options.iter().find(|r| r.id == id).cloned().ok_or_else(|| {
                let list: Vec<&str> = options.iter().map(|r| r.id.as_str()).collect();
                fail(&format!("'{id}' doesn't fit {}. Fits: {}", def.name, list.join(", ")))
            })?,
            _ => forge::roll_reforge(&options, instance.reforge.as_deref(), engine::random())
                .ok_or_else(|| fail(&format!("No reforge fits {}.", def.name)))?,
        };
        instance.reforge = Some(chosen.id.clone());
        forge_result(&player, &def, &instance);
        sender.send_message(text(&format!("&aReforged into {}{} {}", def.rarity.color, chosen.name, def.name)));
        Ok(1)
    }
}

/// `/relic socket [socket]`: moves one gem from the offhand into the main-hand relic.
struct Socket;
impl CommandHandler for Socket {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let (def, mut instance) = held_for_forge(&player)?;
        if def.sockets.is_empty() {
            return Err(fail(&format!("{} has no sockets.", def.name)));
        }
        let off_hand = player.get_inventory().get_off_hand().ok_or_else(|| fail("Hold a gem in your offhand."))?;
        let gem = off_hand
            .get_string(NS, "id")
            .and_then(|id| item(&id))
            .filter(|d| d.gem.is_some())
            .ok_or_else(|| fail("Hold a gem in your offhand."))?;

        let socket = match socket_arg(&args) {
            Some(socket) => {
                if instance.gem(socket).is_some() {
                    return Err(fail(&format!("Socket {} is taken. Use /relic unsocket {} first.", socket + 1, socket + 1)));
                }
                if let Some(problem) = forge::gem_problem(&def, socket, &gem) {
                    return Err(fail(&problem));
                }
                socket
            }
            None => {
                let mut free = (0..def.sockets.len()).filter(|&s| instance.gem(s).is_none()).peekable();
                let first = *free.peek().ok_or_else(|| fail(&format!("Every socket on {} is taken.", def.name)))?;
                free.find(|&s| forge::gem_problem(&def, s, &gem).is_none()).ok_or_else(|| {
                    fail(&forge::gem_problem(&def, first, &gem).unwrap_or_default())
                })?
            }
        };

        instance.set_gem(socket, Some(gem.id.clone()));
        forge_result(&player, &def, &instance);
        let left = off_hand.get_count().saturating_sub(1);
        let rest = (left > 0).then(|| build_stack(&gem, &Instance::default(), left, &stats::current(&player)));
        player.set_item_in_hand(Hand::Left, rest);
        sender.send_message(text(&format!("&aSocketed {} &ainto socket {}", gem.display_name(), socket + 1)));
        Ok(1)
    }
}

/// `/relic unsocket [socket]`: takes a gem out, the last one if none is named.
/// Also reaches gems past the last socket, left there by an edit to the item.
struct Unsocket;
impl CommandHandler for Unsocket {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let (def, mut instance) = held_for_forge(&player)?;
        let socket = match socket_arg(&args) {
            Some(socket) => socket,
            None => instance.gems.iter().rposition(Option::is_some).ok_or_else(|| fail("No gems to take out."))?,
        };
        let id = instance.gem(socket).ok_or_else(|| fail(&format!("Socket {} is empty.", socket + 1)))?.to_string();
        let gem = item(&id).ok_or_else(|| fail(&format!("Gem '{id}' no longer exists, so it can't come out. Restore its definition.")))?;

        let inv = player.get_inventory().as_inventory();
        let slot = (0..36).find(|&s| inv.get_item(s).is_none()).ok_or_else(|| fail("Inventory full."))?;
        instance.set_gem(socket, None);
        forge_result(&player, &def, &instance);
        put_item(&player, slot, build_stack(&gem, &Instance::default(), 1, &stats::current(&player)));
        sender.send_message(text(&format!("&aTook {} &aout of socket {}", gem.display_name(), socket + 1)));
        Ok(1)
    }
}

/// `/relic upgrade`: uses one upgrade item from the offhand on the main-hand relic.
struct Upgrade;
impl CommandHandler for Upgrade {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail("Only players can use this."))?;
        let (def, mut instance) = held_for_forge(&player)?;
        let off_hand = player.get_inventory().get_off_hand().ok_or_else(|| fail("Hold an upgrade in your offhand."))?;
        let upgrade_item = off_hand
            .get_string(NS, "id")
            .and_then(|id| item(&id))
            .filter(|d| d.upgrade.is_some())
            .ok_or_else(|| fail("Hold an upgrade in your offhand."))?;
        let chosen = upgrade_item.upgrade.as_deref().and_then(upgrade).ok_or_else(|| fail("That upgrade no longer exists."))?;
        if chosen.bonus_for(&def).is_none() {
            return Err(fail(&format!("{} doesn't work on {}.", upgrade_item.name, def.name)));
        }
        let count = instance.upgrade_count(&chosen.id);
        if count >= chosen.max {
            return Err(fail(&format!("{} already has the most {} it can take ({}).", def.name, chosen.name, chosen.max)));
        }

        instance.upgrades.insert(chosen.id.clone(), count + 1);
        forge_result(&player, &def, &instance);
        let left = off_hand.get_count().saturating_sub(1);
        let rest = (left > 0).then(|| build_stack(&upgrade_item, &Instance::default(), left, &stats::current(&player)));
        player.set_item_in_hand(Hand::Left, rest);
        sender.send_message(text(&format!("&aApplied {} &ato {} ({}/{})", upgrade_item.display_name(), def.display_name(), count + 1, chosen.max)));
        Ok(1)
    }
}

/// `/relic reforges`: every reforge, and whether it fits the held item.
struct ListReforges;
impl CommandHandler for ListReforges {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let held = sender.as_player().and_then(|p| relic_of(&p.get_item_in_hand(Hand::Right)?));
        let reforges = REFORGES.read().unwrap();
        sender.send_message(text(&format!("&eReforges ({}):", reforges.len())));
        for r in reforges.values() {
            let stats: Vec<String> = r.stats.iter().map(|(k, v)| format!("{k} {}", items::expr::format_number(*v))).collect();
            let mark = if held.as_ref().is_some_and(|d| r.fits(d)) { "&a" } else { "&7" };
            sender.send_message(text(&format!("{mark}- {} &7({}, weight {}): {}", r.id, r.name, r.weight, stats.join(", "))));
        }
        Ok(1)
    }
}

/// Unused until Pumpkin's suggestion crash is fixed (see `register`).
#[allow(dead_code)]
struct ItemIds;
impl CommandSuggestionHandler for ItemIds {
    fn suggest(&self, _: CommandSender, _: Server, request: SuggestionRequest) -> CommandSuggestions {
        let values = ITEMS
            .read()
            .unwrap()
            .keys()
            .filter(|id| id.starts_with(&request.remaining))
            .map(|id| CommandSuggestion { value: id.clone(), tooltip: None })
            .collect();
        CommandSuggestions {
            start: request.start,
            length: request.remaining.len() as u32,
            values,
        }
    }
}

// ---- Events ----------------------------------------------------------------------

struct OnInteract;
impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(&self, server: Server, event: EventData<PlayerInteractEvent>) -> EventData<PlayerInteractEvent> {
        let trigger = match event.action {
            InteractAction::RightClickAir | InteractAction::RightClickBlock => Trigger::RightClick,
            InteractAction::LeftClickAir | InteractAction::LeftClickBlock => Trigger::LeftClick,
        };
        let player = event.player.as_entity();
        if let Some(player) = server.get_all_players().into_iter().find(|p| p.as_entity().get_id() == player.get_id()) {
            fire_held(&server, player, None, trigger);
        }
        event
    }
}

struct OnHit;
impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(&self, server: Server, mut event: EventData<EntityDamageByEntityEvent>) -> EventData<EntityDamageByEntityEvent> {
        // The event only has entity ids, so find the attacker and victim by id.
        crate::triggers::note_attacker(event.entity_id as u32, event.damager_id as u32);
        // Skill damage has no attacker: it never gets here, and applies defense
        // and elements in the `damage` mechanic.
        if let Some((target, _)) = crate::indicators::find(&server, event.entity_id as u32) {
            let source = crate::indicators::current_source(event.entity_id as u32);
            // A kind with this source decides the defense; anything else from
            // another entity (mobs, arrows, ...) is plain `defense`.
            let defense = match source.as_deref().and_then(kind_for_source) {
                Some(kind) => kind.defense,
                None => Some("defense".to_string()),
            };
            // Only melee uses the attacker's weapon: not thorns, arrows and so on.
            let melee = matches!(source.as_deref(), Some("player_attack" | "mob_attack" | "mob_attack_no_aggro"));
            let weapon = crate::indicators::find(&server, event.damager_id as u32)
                .filter(|_| melee)
                .map(|(attacker, _)| stats::of_entity(&server, &attacker));
            if let Some(weapon) = &weapon {
                crate::indicators::note_elements(event.entity_id as u32, items::damage_elements(weapon));
            }
            let multiplier = hit_multiplier(defense.as_deref(), None, weapon.as_ref(), &stats::of_entity(&server, &target));
            event.damage = (f64::from(event.damage) * multiplier) as f32;
        }
        let attacker = server
            .get_all_players()
            .into_iter()
            .find(|p| p.as_entity().get_id() as i32 == event.damager_id);
        if let Some(player) = attacker {
            crate::triggers::note_hit(event.entity_id as u32, &player);
            crate::indicators::note_player_damage(event.entity_id as u32, None);
            let victim = player
                .as_entity()
                .get_nearby_entities(8.0, 8.0, 8.0)
                .into_iter()
                .find(|e| e.get_id() as i32 == event.entity_id);
            if victim.is_some() {
                fire_held(&server, player, victim, Trigger::Hit);
            }
        }
        event
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _: Server, event: EventData<PlayerJoinEvent>) -> EventData<PlayerJoinEvent> {
        refresh_inventory(&event.player);
        event
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _: Server, event: EventData<PlayerLeaveEvent>) -> EventData<PlayerLeaveEvent> {
        stats::forget(&event.player);
        crate::triggers::forget(&player_key(&event.player));
        save_mana();
        event
    }
}

// ---- Setup -----------------------------------------------------------------------

pub fn register(context: &Context) -> Result<(), String> {
    let folder = context.get_data_folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{folder}: {e}"))?;
    let _ = DATA_FOLDER.set(folder);

    // Broken item files should not stop the plugin; /relic reload can fix them.
    match load_items() {
        Ok(n) => info!("loaded {n} item(s)"),
        Err(problems) => {
            for problem in problems {
                warn!("{problem}");
            }
            warn!("no items loaded; fix the files above and run /relic reload");
        }
    }
    load_mana();

    context.register_permission(&Permission {
        node: PERMISSION.into(),
        description: "Use /relic".into(),
        default: PermissionDefault::Op(PermissionLevel::Two),
        children: vec![],
    })?;
    let cmd = Command::new(&["relic".to_string()], "Reliquary items")
        .execute(Menu)
        .then(CommandNode::literal("menu").execute(Menu))
        .then(
            CommandNode::literal("give").then(
                // No .suggest(ItemIds) here: in Pumpkin 90ac8d8 plugin suggestions
                // panic the server ("no reactor running", wasm-host executor.rs:149).
                CommandNode::argument("id", &ArgumentType::String(StringType::SingleWord))
                    .execute(Give),
            ),
        )
        .then(
            CommandNode::literal("equip").then(
                CommandNode::argument("id", &ArgumentType::String(StringType::SingleWord)).execute(Equip),
            ),
        )
        .then(CommandNode::literal("list").execute(List))
        .then(CommandNode::literal("reforges").execute(ListReforges))
        .then(
            CommandNode::literal("reforge")
                .execute(Reforge)
                .then(CommandNode::argument("id", &ArgumentType::String(StringType::SingleWord)).execute(Reforge)),
        )
        .then(
            CommandNode::literal("socket")
                .execute(Socket)
                .then(CommandNode::argument("socket", &ArgumentType::Integer((Some(1), None))).execute(Socket)),
        )
        .then(
            CommandNode::literal("unsocket")
                .execute(Unsocket)
                .then(CommandNode::argument("socket", &ArgumentType::Integer((Some(1), None))).execute(Unsocket)),
        )
        .then(CommandNode::literal("upgrade").execute(Upgrade))
        .then(CommandNode::literal("reload").execute(Reload))
        .then(
            CommandNode::literal("stats")
                .execute(StatsCmd)
                .then(CommandNode::argument("player", &ArgumentType::Players).execute(StatsCmd)),
        )
        .then(
            CommandNode::literal("eval").then(
                CommandNode::argument("formula", &ArgumentType::String(StringType::Greedy)).execute(Eval),
            ),
        )
        .then(
            CommandNode::literal("mana").then(
                CommandNode::argument("amount", &ArgumentType::Integer((Some(0), None))).execute(SetMana),
            ),
        );
    context.register_command(cmd, PERMISSION);

    context.register_event_handler::<PlayerInteractEvent, _>(OnInteract, EventPriority::Normal, true)?;
    context.register_event_handler::<EntityDamageByEntityEvent, _>(OnHit, EventPriority::Normal, true)?;
    context.register_event_handler::<PlayerJoinEvent, _>(OnJoin, EventPriority::Normal, false)?;
    context.register_event_handler::<PlayerLeaveEvent, _>(OnLeave, EventPriority::Normal, false)?;
    context.register_event_handler::<InventoryClickEvent, _>(crate::menu::OnClick, EventPriority::Normal, true)?;
    context.register_event_handler::<EntityDamageEvent, _>(crate::triggers::OnDamaged, EventPriority::Normal, false)?;
    // Blocking: it must read the target's health before the damage applies.
    context.register_event_handler::<EntityDamageEvent, _>(crate::indicators::OnDamage, EventPriority::Normal, true)?;
    context.register_event_handler::<EntityDeathEvent, _>(crate::triggers::OnDeath, EventPriority::Normal, false)?;
    context.register_event_handler::<PlayerToggleSneakEvent, _>(crate::triggers::OnSneak, EventPriority::Normal, false)?;

    // Once per second: regen mana from stats, refresh the bar, rebuild relics
    // when stats changed (their lore placeholders use them), and run
    // equip/unequip/timer skills.
    context.schedule_repeating_task(20, 20, |server: Server| {
        let seconds = crate::triggers::next_second();
        crate::indicators::sweep(&server);
        for player in server.get_all_players() {
            let key = player_key(&player);
            let stats = stats::current(&player);
            let (max, regen) = (max_mana(&stats), stats.get("mana_regen").copied().unwrap_or(0.0).max(0.0) as u32);
            let mana = mana::regen_by(&key, regen, max);
            show_mana(&player, mana, max);
            if stats::update(&player, &stats) {
                refresh_inventory(&player);
            }
            crate::triggers::second(&server, &key, seconds);
        }
    });
    Ok(())
}
