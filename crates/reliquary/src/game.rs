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

use crate::config::{self, Config};
use crate::engine::{self, Cast, text};
use crate::files;
use crate::items::{self, DamageKind, Elements, Instance, ItemDef, Params, PlayerStats, ReforgeDef, Reforges, Scope, Slot, StatFormats, Trigger, UpgradeDef, Upgrades, forge};
use crate::texts::{self, Key, Sheet, Vars, messages as m, msg};
use crate::{mana, stats};

pub(crate) const NS: &str = "reliquary";
const PERMISSION: &str = "reliquary:command.relic";
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

/// Reads and parses one settings file. A problem is noted and the rest of the
/// load carries on with an empty value, so one reload reports every broken file.
fn section<T: Default, E: std::fmt::Display>(root: &Path, path: &str, problems: &mut Vec<String>, parse: impl FnOnce(&str, &str) -> Result<T, E>) -> T {
    let parsed = std::fs::read_to_string(root.join(path))
        .map_err(|e| format!("{path}: {e}"))
        .and_then(|src| parse(path, &src).map_err(|e| e.to_string()));
    parsed.unwrap_or_else(|e| {
        problems.push(e);
        T::default()
    })
}

/// Everything the plugin's files define. Nothing is in use until `install`.
struct Loaded {
    items: BTreeMap<String, Arc<ItemDef>>,
    reforges: Reforges,
    upgrades: Upgrades,
    kinds: BTreeMap<String, DamageKind>,
    elements: Elements,
    formats: StatFormats,
    config: Config,
    messages: Sheet,
    indicators: Sheet,
}

/// Loads the settings and every `.kdl` file under `skills/` and `items/` (either
/// folder may hold items and skills), then links items to their skills. Files
/// from older versions move to their new place first, and missing files are
/// written with their defaults.
fn load_all(root: &Path) -> Result<Loaded, Vec<String>> {
    for line in files::migrate(root).map_err(|e| vec![e])? {
        info!("{line}");
    }
    files::write_defaults(root).map_err(|e| vec![e])?;
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

    let (config, config_refs): (Config, _) = section(root, "configs/config.kdl", &mut problems, items::parse_config);
    let messages = section(root, texts::messages::PATH, &mut problems, |_, src| texts::messages::parse(src));
    let lore = section(root, texts::lore::PATH, &mut problems, |_, src| texts::lore::parse(src));
    let indicators = section(root, texts::indicators::PATH, &mut problems, |_, src| texts::indicators::parse(src));

    let kinds: BTreeMap<String, DamageKind> = section(root, "configs/kinds.kdl", &mut problems, items::parse_kinds)
        .into_iter()
        .map(|k| (k.id.clone(), k))
        .collect();
    for kind in kinds.values() {
        // A kind's power stat counts as declared.
        declared.extend(kind.power.iter().chain(&kind.defense).filter(|s| items::stat_info(s).is_none()).cloned());
        if crate::names::damage_type(&kind.source).is_none() {
            problems.push(format!("configs/kinds.kdl: kind `{}` has unknown source `{}`", kind.id, kind.source));
        }
    }
    // Elements, and the stats that can be tagged with one.
    let elements: Elements = section(root, "configs/elements.kdl", &mut problems, items::parse_elements);
    let mut taggable: std::collections::BTreeSet<String> = ["damage", "defense"].map(String::from).into();
    taggable.extend(kinds.values().flat_map(|k| k.power.iter().chain(&k.defense)).cloned());
    // Rarities too, in file order (the first is the default).
    let rarities: Vec<Arc<items::RarityDef>> = section(root, "configs/item_rarity.kdl", &mut problems, items::parse_rarities)
        .into_iter()
        .map(Arc::new)
        .collect();
    // How stats look in lore, and what the lore shows, in order.
    let (mut formats, mut format_refs): (StatFormats, Vec<_>) = section(root, "formats/stats_format.kdl", &mut problems, items::parse_stat_formats);
    let (layout, layout_refs): (Vec<_>, Vec<_>) = section(root, "formats/stats_layout.kdl", &mut problems, items::parse_stats_layout);
    formats.layout = layout;
    format_refs.extend(layout_refs);
    format_refs.extend(config_refs);
    formats.lore = lore;
    formats.tooltip = config.tooltip;
    let (reforges, reforge_stats): (Vec<_>, Vec<_>) = section(root, "configs/reforges.kdl", &mut problems, items::parse_reforges);
    let reforges: Reforges = reforges.into_iter().map(|r| (r.id.clone(), Arc::new(r))).collect();
    let (upgrades, upgrade_stats, upgrade_inherited): (Vec<_>, Vec<_>, Vec<_>) = section(root, "configs/upgrades.kdl", &mut problems, items::parse_upgrades);
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
            problems.push(format!("item `{}`: unknown upgrade `{id}` (configs/upgrades.kdl has: {})", def.id, list.join(", ")));
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
            problems.push(format!("{at}: unknown damage kind `{kind}` (configs/kinds.kdl has: {})", list.join(", ")));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Loaded { items: set, reforges, upgrades, kinds, elements, formats, config, messages, indicators })
}

/// Puts everything into use at once.
fn install(loaded: Loaded) {
    let Loaded { items, reforges, upgrades, kinds, elements, mut formats, config, messages, indicators } = loaded;
    *ITEMS.write().unwrap() = items;
    *REFORGES.write().unwrap() = reforges;
    *UPGRADES.write().unwrap() = upgrades;
    *KINDS.write().unwrap() = kinds;
    formats.element_names = elements.elements.values().map(|e| (e.id.clone(), format!("{}{}", e.color, e.name))).collect();
    formats.element_colors = elements.elements.values().map(|e| (e.id.clone(), e.color.clone())).collect();
    *ELEMENTS.write().unwrap() = Some(Arc::new(elements));
    *STAT_FORMATS.write().unwrap() = Some(Arc::new(formats));
    config::install(config);
    texts::install(messages, indicators);
}

/// Loads everything from the data folder. All or nothing: if anything has a
/// problem, what is in use stays and the problems are returned.
pub fn load_items() -> Result<usize, Vec<String>> {
    let root = data_path("");
    let loaded = load_all(Path::new(&root))?;
    let count = loaded.items.len();
    install(loaded);
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
    let path = data_path(files::MANA);
    match std::fs::read_to_string(&path) {
        Ok(s) => match mana::load(&s) {
            Ok(n) => info!("loaded mana for {n} player(s)"),
            Err(e) => warn!("could not read {path}: {e}"),
        },
        Err(_) => info!("no {path} yet, everyone starts with full mana"),
    }
}

pub fn save_mana() {
    let path = data_path(files::MANA);
    let result = mana::save().and_then(|s| {
        if let Some(parent) = Path::new(&path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, s).map_err(|e| e.to_string())
    });
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
    // Needs a server with Pumpkin #4334: without it, this name loses its colors.
    stack.set_custom_name(Some(text(&def.display_name()).italic(false)));
    stack.set_lore(def.lore_lines(stats, &stat_formats()).iter().map(|l| text(l).italic(false)).collect());

    if let Some(model) = &def.model {
        let mut bytes = Vec::new();
        write_string(&mut bytes, model);
        stack.set_component(DataComponent::ItemModel, &bytes);
    }
    // Raw components, like the rest of this function: the plugin API we build
    // against has no setters for these. Unbreakable has no value; the glint
    // override is one bool.
    if def.unbreakable {
        stack.set_component(DataComponent::Unbreakable, &[]);
    }
    if def.fake_enchant {
        stack.set_component(DataComponent::EnchantmentGlintOverride, &[1]);
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
    let mut display = vec![0, 1, DataComponent::AttributeModifiers as u8];
    if def.hide_unbreakable {
        display[1] += 1;
        display.push(DataComponent::Unbreakable as u8);
    }
    stack.set_component(DataComponent::TooltipDisplay, &display);
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

/// Carries durability damage over to a rebuilt stack: it only lives on the
/// stack, so `build_stack` can't know it. Skipped if the base item changed, as
/// a diamond sword's damage could break a golden one on its next hit.
fn keep_damage(old: &ItemStack, new: &ItemStack) {
    if old.get_registry_key() != new.get_registry_key() {
        return;
    }
    let damage = old.get_components().into_iter().find(|c| matches!(c.component, DataComponent::Damage));
    if let Some(damage) = damage {
        new.set_component(DataComponent::Damage, &damage.value);
    }
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
        let rebuilt = build_stack(&def, &instance_of(&stack), stack.get_count(), &stats);
        keep_damage(&stack, &rebuilt);
        Some(rebuilt)
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
    if !config::get().mana_bar {
        return;
    }
    if crate::admin::infinite_mana() {
        player.show_actionbar(text(&msg(&m::MANA_BAR_INFINITE, &[])));
    } else {
        player.show_actionbar(text(&msg(&m::MANA_BAR, &[("mana", &mana), ("max", &max)])));
    }
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

fn fail(key: &Key, vars: Vars) -> CommandError {
    fail_text(&msg(key, vars))
}

/// For messages that are filled in already.
fn fail_text(message: &str) -> CommandError {
    CommandError::CommandFailed(text(message))
}

fn say(sender: &CommandSender, key: &Key, vars: Vars) {
    sender.send_message(text(&msg(key, vars)));
}

/// All items, sorted by id.
pub(crate) fn all_items() -> Vec<Arc<ItemDef>> {
    ITEMS.read().unwrap().values().cloned().collect()
}

/// Puts one of item `id` into the first free main inventory slot.
/// Returns the item's display name.
pub(crate) fn give(player: &Player, id: &str) -> Result<String, String> {
    let def = item(id).ok_or_else(|| msg(&m::UNKNOWN_ITEM, &[("id", &id)]))?;
    let inv = player.get_inventory().as_inventory();
    let slot = (0..36).find(|&s| inv.get_item(s).is_none()).ok_or_else(|| msg(&m::INVENTORY_FULL, &[]))?;
    put_item(player, slot, build_stack(&def, &Instance::default(), 1, &stats::current(player)));
    Ok(def.display_name())
}

/// The player named in the `player` argument, or the sender.
fn target_player(sender: &CommandSender, args: &ConsumedArgs) -> Result<Player, CommandError> {
    if let Arg::Players(mut players) = args.get_value("player") {
        return players.pop().ok_or_else(|| fail(&m::NO_PLAYER_FOUND, &[]));
    }
    sender.as_player().ok_or_else(|| fail(&m::NAME_A_PLAYER, &[]))
}

/// `/relic stats [player]`: totals and which item gives what.
struct StatsCmd;
impl CommandHandler for StatsCmd {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = target_player(&sender, &args)?;
        let total = stats::current(&player);
        let key = player_key(&player);
        say(&sender, &m::STATS_HEADER, &[("player", &player.get_name()), ("mana", &mana::get(&key)), ("max", &max_mana(&total))]);
        for (stat, value) in &total {
            say(&sender, &m::STATS_LINE, &[("stat", stat), ("value", &items::expr::format_number(*value))]);
        }
        say(&sender, &m::STATS_FROM, &[]);
        let base = config::get().player_stats.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ");
        say(&sender, &m::STATS_BASE, &[("stats", &base)]);
        for source in stats::equipped(&player) {
            let list: Vec<String> = source
                .item
                .stats
                .iter()
                .map(|(k, v)| format!("{k} {}", items::expr::format_number(*v)))
                .collect();
            let list = if list.is_empty() { msg(&m::STATS_NONE, &[]) } else { list.join(", ") };
            say(&sender, &m::STATS_SOURCE, &[("slot", &format!("{:?}", source.slot)), ("item", &source.item.display_name()), ("stats", &list)]);
        }
        Ok(1)
    }
}

/// `/relic eval <formula>`: evaluates a formula with your stats.
struct Eval;
impl CommandHandler for Eval {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let src = match args.get_value("formula") {
            Arg::Simple(s) | Arg::Msg(s) => s,
            _ => return Err(fail(&m::MISSING_FORMULA, &[])),
        };
        let e = items::expr::parse(&src).map_err(|e| fail(&m::FORMULA_ERROR, &[("at", &(e.at + 1)), ("error", &e.message)]))?;
        let (params, stats) = (Params::new(), stats::for_cast(&player));
        let value = e.eval(Scope { params: &params, stats: &stats });
        say(&sender, &m::FORMULA_RESULT, &[("formula", &src), ("value", &value)]);
        Ok(1)
    }
}

/// `/relic mana <amount>`: sets your mana (capped at max mana).
struct SetMana;
impl CommandHandler for SetMana {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let Arg::Num(Ok(n)) = args.get_value("amount") else {
            return Err(fail(&m::WHOLE_NUMBER, &[]));
        };
        let amount = match n {
            pumpkin_plugin_api::command_wit::Number::Int32(i) => i.max(0) as u32,
            _ => return Err(fail(&m::WHOLE_NUMBER, &[])),
        };
        let max = max_mana(&stats::current(&player));
        mana::set(&player_key(&player), amount.min(max));
        show_mana(&player, amount.min(max), max);
        Ok(1)
    }
}

fn on_off(enabled: bool) -> String {
    msg(if enabled { &m::STATE_ON } else { &m::STATE_OFF }, &[])
}

/// `/relic admin nocd`: toggles ability cooldowns off for everyone.
struct AdminNoCooldown;
impl CommandHandler for AdminNoCooldown {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let enabled = crate::admin::toggle_no_cooldown();
        say(&sender, &m::ADMIN_NO_COOLDOWNS, &[("state", &on_off(enabled))]);
        Ok(1)
    }
}

/// `/relic admin infinite_mana`: toggles free skills for everyone.
struct AdminInfiniteMana;
impl CommandHandler for AdminInfiniteMana {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let enabled = crate::admin::toggle_infinite_mana();
        say(&sender, &m::ADMIN_INFINITE_MANA, &[("state", &on_off(enabled))]);
        Ok(1)
    }
}

struct Give;
impl CommandHandler for Give {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let Arg::Simple(id) = args.get_value("id") else {
            return Err(fail(&m::MISSING_ITEM_ID, &[]));
        };
        let name = give(&player, &id).map_err(|e| fail_text(&e))?;
        say(&sender, &m::GAVE, &[("item", &name)]);
        Ok(1)
    }
}

/// `/relic equip <id>`: puts a relic on the mob you are looking at, in the slot
/// the relic is made for.
struct Equip;
impl CommandHandler for Equip {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let Arg::Simple(id) = args.get_value("id") else {
            return Err(fail(&m::MISSING_ITEM_ID, &[]));
        };
        let def = item(&id).filter(|d| d.is_relic()).ok_or_else(|| fail(&m::NO_RELIC, &[("id", &id)]))?;
        let hit = player.ray_trace_entity(config::get().equip_range).ok_or_else(|| fail(&m::LOOK_AT_MOB, &[]))?;
        let living = hit.entity.as_living().filter(|l| l.is_mob()).ok_or_else(|| fail(&m::LOOK_AT_MOB, &[]))?;
        let slot = match def.slot {
            Slot::MainHand => EquipmentSlot::MainHand,
            Slot::OffHand => EquipmentSlot::OffHand,
            Slot::Head => EquipmentSlot::Head,
            Slot::Chest => EquipmentSlot::Chest,
            Slot::Legs => EquipmentSlot::Legs,
            Slot::Feet => EquipmentSlot::Feet,
        };
        living.set_equipment(slot, Some(build_stack(&def, &Instance::default(), 1, &PlayerStats::new())));
        say(&sender, &m::EQUIPPED_MOB, &[("item", &def.display_name()), ("slot", &format!("{:?}", def.slot))]);
        Ok(1)
    }
}

struct Menu;
impl CommandHandler for Menu {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
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
            .map(|def| msg(&m::LIST_LINE, &[("id", &def.id), ("item", &def.display_name())]))
            .collect();
        say(&sender, &m::LIST_HEADER, &[("count", &lines.len())]);
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
                say(&sender, &m::RELOADED, &[("count", &n)]);
                Ok(1)
            }
            Err(problems) => {
                say(&sender, &m::RELOAD_FAILED, &[("count", &problems.len())]);
                for problem in &problems {
                    warn!("{problem}");
                    say(&sender, &m::RELOAD_PROBLEM, &[("problem", problem)]);
                }
                Ok(0)
            }
        }
    }
}

/// The relic in the main hand, to change: its definition and what it has on it.
fn held_for_forge(player: &Player) -> Result<(Arc<ItemDef>, Instance), CommandError> {
    let stack = player.get_item_in_hand(Hand::Right).ok_or_else(|| fail(&m::HOLD_ITEM, &[]))?;
    let def = stack
        .get_string(NS, "id")
        .and_then(|id| item(&id))
        .ok_or_else(|| fail(&m::NOT_RELIQUARY_ITEM, &[]))?;
    if !def.is_relic() {
        return Err(fail(&m::HOLD_RELIC, &[]));
    }
    if stack.get_count() != 1 {
        return Err(fail(&m::HOLD_ONE, &[]));
    }
    Ok((def, instance_of(&stack)))
}

/// Puts the changed relic back in the main hand.
fn forge_result(player: &Player, def: &ItemDef, instance: &Instance) {
    let stack = build_stack(def, instance, 1, &stats::current(player));
    if let Some(old) = player.get_item_in_hand(Hand::Right) {
        keep_damage(&old, &stack);
    }
    player.set_item_in_hand(Hand::Right, Some(stack));
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
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let (def, mut instance) = held_for_forge(&player)?;
        if !def.reforgeable {
            return Err(fail(&m::CANT_REFORGE, &[("item", &def.name)]));
        }
        let options: Vec<Arc<ReforgeDef>> = REFORGES.read().unwrap().values().filter(|r| r.fits(&def)).cloned().collect();
        let chosen = match args.get_value("id") {
            Arg::Simple(id) if !id.is_empty() => options.iter().find(|r| r.id == id).cloned().ok_or_else(|| {
                let list: Vec<&str> = options.iter().map(|r| r.id.as_str()).collect();
                fail(&m::REFORGE_DOESNT_FIT, &[("id", &id), ("item", &def.name), ("fits", &list.join(", "))])
            })?,
            _ => forge::roll_reforge(&options, instance.reforge.as_deref(), engine::random())
                .ok_or_else(|| fail(&m::NO_REFORGE_FITS, &[("item", &def.name)]))?,
        };
        instance.reforge = Some(chosen.id.clone());
        forge_result(&player, &def, &instance);
        say(&sender, &m::REFORGED, &[("color", &def.rarity.color), ("reforge", &chosen.name), ("item", &def.name)]);
        Ok(1)
    }
}

/// `/relic socket [socket]`: moves one gem from the offhand into the main-hand relic.
struct Socket;
impl CommandHandler for Socket {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let (def, mut instance) = held_for_forge(&player)?;
        if def.sockets.is_empty() {
            return Err(fail(&m::NO_SOCKETS, &[("item", &def.name)]));
        }
        let off_hand = player.get_inventory().get_off_hand().ok_or_else(|| fail(&m::HOLD_GEM, &[]))?;
        let gem = off_hand
            .get_string(NS, "id")
            .and_then(|id| item(&id))
            .filter(|d| d.gem.is_some())
            .ok_or_else(|| fail(&m::HOLD_GEM, &[]))?;

        let socket = match socket_arg(&args) {
            Some(socket) => {
                if instance.gem(socket).is_some() {
                    return Err(fail(&m::SOCKET_TAKEN, &[("socket", &(socket + 1))]));
                }
                if let Some(problem) = forge::gem_problem(&def, socket, &gem) {
                    return Err(fail_text(&problem));
                }
                socket
            }
            None => {
                let mut free = (0..def.sockets.len()).filter(|&s| instance.gem(s).is_none()).peekable();
                let first = *free.peek().ok_or_else(|| fail(&m::SOCKETS_FULL, &[("item", &def.name)]))?;
                free.find(|&s| forge::gem_problem(&def, s, &gem).is_none()).ok_or_else(|| {
                    fail_text(&forge::gem_problem(&def, first, &gem).unwrap_or_default())
                })?
            }
        };

        instance.set_gem(socket, Some(gem.id.clone()));
        forge_result(&player, &def, &instance);
        let left = off_hand.get_count().saturating_sub(1);
        let rest = (left > 0).then(|| build_stack(&gem, &Instance::default(), left, &stats::current(&player)));
        player.set_item_in_hand(Hand::Left, rest);
        say(&sender, &m::SOCKETED, &[("gem", &gem.display_name()), ("socket", &(socket + 1))]);
        Ok(1)
    }
}

/// `/relic unsocket [socket]`: takes a gem out, the last one if none is named.
/// Also reaches gems past the last socket, left there by an edit to the item.
struct Unsocket;
impl CommandHandler for Unsocket {
    fn handle(&self, sender: CommandSender, _: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let (def, mut instance) = held_for_forge(&player)?;
        let socket = match socket_arg(&args) {
            Some(socket) => socket,
            None => instance.gems.iter().rposition(Option::is_some).ok_or_else(|| fail(&m::NO_GEMS, &[]))?,
        };
        let id = instance.gem(socket).ok_or_else(|| fail(&m::SOCKET_EMPTY, &[("socket", &(socket + 1))]))?.to_string();
        let gem = item(&id).ok_or_else(|| fail(&m::GEM_GONE, &[("id", &id)]))?;

        let inv = player.get_inventory().as_inventory();
        let slot = (0..36).find(|&s| inv.get_item(s).is_none()).ok_or_else(|| fail(&m::INVENTORY_FULL, &[]))?;
        instance.set_gem(socket, None);
        forge_result(&player, &def, &instance);
        put_item(&player, slot, build_stack(&gem, &Instance::default(), 1, &stats::current(&player)));
        say(&sender, &m::UNSOCKETED, &[("gem", &gem.display_name()), ("socket", &(socket + 1))]);
        Ok(1)
    }
}

/// `/relic upgrade`: uses one upgrade item from the offhand on the main-hand relic.
struct Upgrade;
impl CommandHandler for Upgrade {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let player = sender.as_player().ok_or_else(|| fail(&m::ONLY_PLAYERS, &[]))?;
        let (def, mut instance) = held_for_forge(&player)?;
        let off_hand = player.get_inventory().get_off_hand().ok_or_else(|| fail(&m::HOLD_UPGRADE, &[]))?;
        let upgrade_item = off_hand
            .get_string(NS, "id")
            .and_then(|id| item(&id))
            .filter(|d| d.upgrade.is_some())
            .ok_or_else(|| fail(&m::HOLD_UPGRADE, &[]))?;
        let chosen = upgrade_item.upgrade.as_deref().and_then(upgrade).ok_or_else(|| fail(&m::UPGRADE_GONE, &[]))?;
        if chosen.bonus_for(&def).is_none() {
            return Err(fail(&m::UPGRADE_DOESNT_FIT, &[("upgrade", &upgrade_item.name), ("item", &def.name)]));
        }
        let count = instance.upgrade_count(&chosen.id);
        if count >= chosen.max {
            return Err(fail(&m::UPGRADE_MAXED, &[("item", &def.name), ("upgrade", &chosen.name), ("max", &chosen.max)]));
        }

        instance.upgrades.insert(chosen.id.clone(), count + 1);
        forge_result(&player, &def, &instance);
        let left = off_hand.get_count().saturating_sub(1);
        let rest = (left > 0).then(|| build_stack(&upgrade_item, &Instance::default(), left, &stats::current(&player)));
        player.set_item_in_hand(Hand::Left, rest);
        say(&sender, &m::UPGRADED, &[("upgrade", &upgrade_item.display_name()), ("item", &def.display_name()), ("count", &(count + 1)), ("max", &chosen.max)]);
        Ok(1)
    }
}

/// `/relic reforges`: every reforge, and whether it fits the held item.
struct ListReforges;
impl CommandHandler for ListReforges {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let held = sender.as_player().and_then(|p| relic_of(&p.get_item_in_hand(Hand::Right)?));
        let reforges = REFORGES.read().unwrap();
        say(&sender, &m::REFORGES_HEADER, &[("count", &reforges.len())]);
        for r in reforges.values() {
            let stats: Vec<String> = r.stats.iter().map(|(k, v)| format!("{k} {}", items::expr::format_number(*v))).collect();
            let mark = msg(if held.as_ref().is_some_and(|d| r.fits(d)) { &m::REFORGES_FITS } else { &m::REFORGES_OTHER }, &[]);
            say(&sender, &m::REFORGES_LINE, &[("mark", &mark), ("id", &r.id), ("name", &r.name), ("weight", &r.weight), ("stats", &stats.join(", "))]);
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
            let mut crit = 1.0;
            if let Some(weapon) = &weapon {
                crate::indicators::note_elements(event.entity_id as u32, items::damage_elements(weapon));
                if let Some(multiplier) = stats::crit_multiplier(weapon, engine::random()) {
                    crit = multiplier;
                    crate::indicators::note_crit(event.entity_id as u32);
                }
            }
            let multiplier = hit_multiplier(defense.as_deref(), None, weapon.as_ref(), &stats::of_entity(&server, &target));
            event.damage = (f64::from(event.damage) * multiplier * crit) as f32;
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
        )
        .then(
            CommandNode::literal("admin")
                .then(CommandNode::literal("nocd").execute(AdminNoCooldown))
                .then(CommandNode::literal("infinite_mana").execute(AdminInfiniteMana)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::tests::temp_dir;

    const OLD_FILES: [(&str, &str); 7] = [
        ("kinds.kdl", "configs/kinds.kdl"),
        ("elements.kdl", "configs/elements.kdl"),
        ("item_rarity.kdl", "configs/item_rarity.kdl"),
        ("reforges.kdl", "configs/reforges.kdl"),
        ("upgrades.kdl", "configs/upgrades.kdl"),
        ("stats_format.kdl", "formats/stats_format.kdl"),
        ("stats_layout.kdl", "formats/stats_layout.kdl"),
    ];

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let target = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy_tree(&path, &target);
            } else {
                std::fs::copy(&path, &target).unwrap();
            }
        }
    }

    #[test]
    fn a_fresh_folder_loads() {
        let root = temp_dir("fresh");
        let loaded = load_all(&root).unwrap_or_else(|p| panic!("{p:#?}"));
        assert!(!loaded.items.is_empty());
        assert_eq!(loaded.config, Config::default(), "config.kdl shows the defaults");
        assert_eq!(loaded.formats.lore, texts::lore::defaults());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Loads an old-layout folder and checks every edit survived: files move byte
    /// for byte, and the defaults do not take their place.
    fn check_migration(root: &Path) {
        let before: Vec<Vec<u8>> = OLD_FILES.iter().map(|(old, _)| std::fs::read(root.join(old)).unwrap()).collect();
        load_all(root).unwrap_or_else(|p| panic!("{p:#?}"));
        for ((old, new), bytes) in OLD_FILES.iter().zip(before) {
            assert!(!root.join(old).exists(), "{old} should have moved");
            assert_eq!(std::fs::read(root.join(new)).unwrap(), bytes, "{new} changed");
        }
    }

    #[test]
    fn an_old_folder_moves_and_keeps_its_edits() {
        let root = temp_dir("old");
        for (path, contents) in files::DEFAULTS.iter().filter(|(p, _)| OLD_FILES.iter().any(|(_, new)| new == p)) {
            let name = path.rsplit('/').next().unwrap();
            std::fs::write(root.join(name), format!("// edited\n{contents}")).unwrap();
        }
        check_migration(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The same on a copy of a real server's files, when this checkout has one in
    /// the old layout. The server's own folder is only read.
    #[test]
    fn a_real_old_folder_moves_and_keeps_its_edits() {
        let live = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../server/plugins/data/reliquary");
        if !live.join("kinds.kdl").exists() {
            return;
        }
        let root = temp_dir("live");
        copy_tree(&live, &root);
        check_migration(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_and_format_files_take_effect_and_typos_are_reported() {
        let root = temp_dir("edited");
        load_all(&root).unwrap_or_else(|p| panic!("{p:#?}"));

        std::fs::write(root.join("configs/config.kdl"), "mana { start 7; }\ntooltip { wrap-width 12; }\nstats { player { max-mana 40; } }").unwrap();
        std::fs::write(root.join("formats/lore_format.kdl"), "trigger-right-click \"RMB\"\nskill-costs \"costs: {costs}\"\ncost-separator \" / \"").unwrap();
        let loaded = load_all(&root).unwrap_or_else(|p| panic!("{p:#?}"));
        assert_eq!(loaded.config.start_mana, 7);
        assert_eq!(loaded.config.player_stats, [("max_mana".to_string(), 40.0)]);
        assert_eq!(loaded.formats.tooltip.wrap_width, 12);
        let lore = loaded.items["moonlit_edge"].lore_lines(&PlayerStats::new(), &loaded.formats);
        assert!(lore.iter().any(|l| l == "&5◆ &fMoon Slash &8· &7RMB"), "{lore:?}");
        assert!(lore.iter().any(|l| l == "costs: 20 mana / 3s cooldown"), "{lore:?}");

        // All at once, and each with its file and line.
        std::fs::write(root.join("configs/config.kdl"), "mana {\n  strat 7\n}\n").unwrap();
        std::fs::write(root.join("configs/messages.kdl"), "gave \"{itme}\"").unwrap();
        std::fs::write(root.join("formats/indicators.kdl"), "nmber \"x\"").unwrap();
        let problems = load_all(&root).err().expect("these files are broken");
        assert!(problems.iter().any(|p| p.starts_with("configs/config.kdl:2:3: unknown setting `strat`")), "{problems:#?}");
        assert!(problems.iter().any(|p| p.starts_with("configs/messages.kdl:1:1") && p.contains("{itme}")), "{problems:#?}");
        assert!(problems.iter().any(|p| p.starts_with("formats/indicators.kdl:1:1")), "{problems:#?}");

        // A base stat nobody knows.
        std::fs::write(root.join("configs/config.kdl"), "stats {\n  player {\n    mxa-mana 40\n  }\n}").unwrap();
        std::fs::remove_file(root.join("configs/messages.kdl")).unwrap();
        std::fs::remove_file(root.join("formats/indicators.kdl")).unwrap();
        let problems = load_all(&root).err().expect("unknown stat");
        assert!(problems.iter().any(|p| p.contains("configs/config.kdl:3:5") && p.contains("mxa_mana")), "{problems:#?}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
