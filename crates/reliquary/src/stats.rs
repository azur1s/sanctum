//! Player stats: base values plus the stats of every relic worn or held in
//! its own slot.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use pumpkin_plugin_api::{Entity, ItemStack, Player, Server, common::Hand, world::EquipmentSlot};

use crate::config;
use crate::game;
use crate::items::{ItemDef, PlayerStats, Slot};
use crate::mana;

/// Stats from the last regen tick, to notice changes.
static LAST: Mutex<Option<HashMap<String, PlayerStats>>> = Mutex::new(None);

/// One item's contribution, for `/relic stats`.
pub struct Source {
    pub slot: Slot,
    pub item: Arc<ItemDef>,
}

fn relic(stack: Option<ItemStack>) -> Option<Arc<ItemDef>> {
    game::relic_of(&stack?)
}

/// Relics that count: each must be in the slot it is made for.
pub fn equipped(player: &Player) -> Vec<Source> {
    let inv = player.get_inventory();
    let slots = [
        (Slot::MainHand, player.get_item_in_hand(Hand::Right)),
        (Slot::OffHand, inv.get_off_hand()),
        (Slot::Head, inv.get_helmet()),
        (Slot::Chest, inv.get_chestplate()),
        (Slot::Legs, inv.get_leggings()),
        (Slot::Feet, inv.get_boots()),
    ];
    slots
        .into_iter()
        .filter_map(|(slot, stack)| relic(stack).filter(|def| def.slot == slot).map(|item| Source { slot, item }))
        .collect()
}

/// Base stats plus everything equipped.
pub fn current(player: &Player) -> PlayerStats {
    let mut total: PlayerStats = config::get().player_stats.iter().cloned().collect();
    for source in equipped(player) {
        for (stat, value) in &source.item.stats {
            *total.entry(stat.clone()).or_insert(0.0) += value;
        }
    }
    total
}

/// What a mob's gear adds up to: each relic must be in the slot it is made for,
/// like on a player, on top of the mob stats from `config.kdl`.
fn of_mob(entity: &Entity) -> PlayerStats {
    let mut total: PlayerStats = config::get().mob_stats.iter().cloned().collect();
    let Some(living) = entity.as_living() else { return total };
    let slots = [
        (Slot::MainHand, EquipmentSlot::MainHand),
        (Slot::OffHand, EquipmentSlot::OffHand),
        (Slot::Head, EquipmentSlot::Head),
        (Slot::Chest, EquipmentSlot::Chest),
        (Slot::Legs, EquipmentSlot::Legs),
        (Slot::Feet, EquipmentSlot::Feet),
    ];
    for (slot, equipment) in slots {
        if let Some(def) = relic(living.get_equipment(equipment)).filter(|def| def.slot == slot) {
            for (stat, value) in &def.stats {
                *total.entry(stat.clone()).or_insert(0.0) += value;
            }
        }
    }
    total
}

/// The stats of anything that can be hit. All damage looks stats up here.
pub fn of_entity(server: &Server, entity: &Entity) -> PlayerStats {
    let id = entity.get_id();
    match server.get_all_players().into_iter().find(|p| p.as_entity().get_id() == id) {
        Some(player) => current(&player),
        None => of_mob(entity),
    }
}

/// What a hit is multiplied by if `roll` (0.0..1.0) makes it a crit. `crit_chance`
/// and `crit_power` are percents. A crit never does less than a normal hit.
pub fn crit_multiplier(stats: &PlayerStats, roll: f64) -> Option<f64> {
    let chance = stats.get("crit_chance").copied().unwrap_or(0.0) / 100.0;
    (roll < chance).then(|| 1.0 + stats.get("crit_power").copied().unwrap_or(0.0).max(0.0) / 100.0)
}

/// Stats plus live values for skills: `self.mana`, `self.hp`, `self.max_hp`.
pub fn for_cast(player: &Player) -> PlayerStats {
    let mut stats = current(player);
    stats.insert("mana".into(), mana::get(&player.get_id().to_string()) as f64);
    if let Some(living) = player.as_entity().as_living() {
        stats.insert("hp".into(), living.get_health() as f64);
        stats.insert("max_hp".into(), living.get_max_health() as f64);
    }
    stats
}

/// Stores `stats` as the player's latest; returns true if they changed.
pub fn update(player: &Player, stats: &PlayerStats) -> bool {
    let mut last = LAST.lock().unwrap();
    let map = last.get_or_insert_with(HashMap::new);
    map.insert(player.get_id().to_string(), stats.clone()).as_ref() != Some(stats)
}

pub fn forget(player: &Player) {
    if let Some(map) = LAST.lock().unwrap().as_mut() {
        map.remove(&player.get_id().to_string());
    }
}

/// Names `self.<stat>` may use: built-ins, live values and declared stats.
pub fn known_names(declared: &BTreeSet<String>) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = crate::items::BUILTIN_STATS.iter().map(|s| s.name.to_string()).collect();
    names.extend(["mana", "hp", "max_hp"].map(String::from));
    names.extend(declared.iter().cloned());
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(chance: f64, power: f64) -> PlayerStats {
        PlayerStats::from([("crit_chance".to_string(), chance), ("crit_power".to_string(), power)])
    }

    #[test]
    fn crit_chance_is_a_percent() {
        assert_eq!(crit_multiplier(&stats(25.0, 50.0), 0.24), Some(1.5));
        assert_eq!(crit_multiplier(&stats(25.0, 50.0), 0.25), None);
        assert_eq!(crit_multiplier(&stats(0.0, 50.0), 0.0), None);
        assert_eq!(crit_multiplier(&stats(100.0, 50.0), 0.999), Some(1.5));
        assert_eq!(crit_multiplier(&PlayerStats::new(), 0.0), None);
    }

    #[test]
    fn crit_power_adds_up_and_never_weakens_a_hit() {
        assert_eq!(crit_multiplier(&stats(100.0, 150.0), 0.5), Some(2.5));
        assert_eq!(crit_multiplier(&stats(100.0, -80.0), 0.5), Some(1.0));
    }

    #[test]
    fn crit_stats_are_builtin_on_any_slot() {
        for stat in ["crit_chance", "crit_power"] {
            assert!(!crate::items::stat_info(stat).unwrap().main_hand_only);
        }
    }
}
