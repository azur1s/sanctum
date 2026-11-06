//! Per-player mana. Plain data keyed by player UUID string, saved as TOML.
//! The maximum and regen come from player stats.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::config;

static MANA: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());

pub fn get(player: &str) -> u32 {
    MANA.lock().unwrap().get(player).copied().unwrap_or_else(|| config::get().start_mana)
}

pub fn set(player: &str, value: u32) {
    MANA.lock().unwrap().insert(player.to_string(), value);
}

/// Takes `cost` mana if the player has enough. Returns the new amount, or None.
pub fn try_spend(player: &str, cost: u32) -> Option<u32> {
    let mut map = MANA.lock().unwrap();
    let mana = map.entry(player.to_string()).or_insert_with(|| config::get().start_mana);
    let left = mana.checked_sub(cost)?;
    *mana = left;
    Some(left)
}

/// Adds `amount` mana, capped at `max` (also when max went down).
/// Returns the new amount.
pub fn regen_by(player: &str, amount: u32, max: u32) -> u32 {
    let mut map = MANA.lock().unwrap();
    let mana = map.entry(player.to_string()).or_insert_with(|| config::get().start_mana);
    *mana = (*mana + amount).min(max);
    *mana
}

pub fn load(text: &str) -> Result<usize, String> {
    let map: BTreeMap<String, u32> = toml::from_str(text).map_err(|e| e.to_string())?;
    let count = map.len();
    *MANA.lock().unwrap() = map;
    Ok(count)
}

pub fn save() -> Result<String, String> {
    let map = MANA.lock().unwrap().clone();
    toml::to_string(&map).map_err(|e| e.to_string())
}
