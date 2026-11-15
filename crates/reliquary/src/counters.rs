//! Skill counters (`counter "rage" max=8 expire="5s"`): numbers a skill keeps
//! between casts, per player and per item skill (like cooldowns). In memory
//! only: a restart or leaving the server clears them.
//!
//! Expiry is worked out when a counter is read, from how long ago it was last
//! added to, so nothing has to tick in the background.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::items::{CounterDef, Decay};

struct State {
    value: u32,
    /// Last `counter-add`, or the last decay step for `decay="one"`.
    since: Instant,
}

/// (player, cooldown group of the item skill, counter name)
type Key = (String, String, String);

static COUNTERS: Mutex<Option<HashMap<Key, State>>> = Mutex::new(None);

/// Applies expiry up to `now`.
fn settle(state: &mut State, def: &CounterDef, now: Instant) {
    let Some(expire) = def.expire else { return };
    let period = Duration::from_secs_f64(expire);
    let elapsed = now.saturating_duration_since(state.since);
    if elapsed < period {
        return;
    }
    match def.decay {
        Decay::All => state.value = 0,
        Decay::One => {
            let steps = (elapsed.as_secs_f64() / expire).floor();
            state.value = state.value.saturating_sub(steps.min(f64::from(u32::MAX)) as u32);
            state.since += period.mul_f64(steps);
        }
    }
}

fn key(player: &str, group: &str, def: &CounterDef) -> Key {
    (player.to_string(), group.to_string(), def.name.clone())
}

/// The counter's value right now.
pub fn get(player: &str, group: &str, def: &CounterDef, now: Instant) -> u32 {
    let mut guard = COUNTERS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    let k = key(player, group, def);
    let Some(state) = map.get_mut(&k) else { return 0 };
    settle(state, def, now);
    let value = state.value;
    if value == 0 {
        map.remove(&k);
    }
    value
}

/// Adds `amount` (rounded; negative takes away), kept within 0..=max, and
/// restarts the expiry timer. Returns the new value.
pub fn add(player: &str, group: &str, def: &CounterDef, amount: f64, now: Instant) -> u32 {
    let mut guard = COUNTERS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    let state = map.entry(key(player, group, def)).or_insert(State { value: 0, since: now });
    settle(state, def, now);
    let max = def.max.map_or(f64::from(u32::MAX), f64::from);
    state.value = (f64::from(state.value) + amount.round()).clamp(0.0, max) as u32;
    state.since = now;
    state.value
}

/// Drops everything a player had, when they leave.
pub fn forget(player: &str) {
    if let Some(map) = COUNTERS.lock().unwrap().as_mut() {
        map.retain(|(p, _, _), _| p != player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, max: Option<u32>, expire: Option<f64>, decay: Decay) -> CounterDef {
        CounterDef { name: name.into(), max, expire, decay }
    }

    #[test]
    fn caps_and_floors() {
        let rage = def("rage", Some(3), None, Decay::All);
        let t = Instant::now();
        assert_eq!(get("caps", "g", &rage, t), 0);
        assert_eq!(add("caps", "g", &rage, 2.0, t), 2);
        assert_eq!(add("caps", "g", &rage, 5.0, t), 3);
        assert_eq!(add("caps", "g", &rage, -10.0, t), 0);
        // Another item skill or player has its own.
        add("caps", "g", &rage, 1.0, t);
        assert_eq!(get("caps", "other", &rage, t), 0);
        assert_eq!(get("caps2", "g", &rage, t), 0);
        forget("caps");
        assert_eq!(get("caps", "g", &rage, t), 0);
    }

    #[test]
    fn expire_all_resets_and_adding_refreshes() {
        let rage = def("rage", Some(8), Some(5.0), Decay::All);
        let t = Instant::now();
        let s = |secs: f64| t + Duration::from_secs_f64(secs);
        add("all", "g", &rage, 1.0, t);
        add("all", "g", &rage, 1.0, s(4.0));
        // 4.9s after the last add: still there.
        assert_eq!(get("all", "g", &rage, s(8.9)), 2);
        assert_eq!(get("all", "g", &rage, s(9.0)), 0);
        // Adding after it expired starts from zero.
        add("all", "g", &rage, 1.0, s(20.0));
        assert_eq!(add("all", "g", &rage, 1.0, s(30.0)), 1);
    }

    #[test]
    fn decay_one_loses_a_stack_per_period() {
        let rage = def("rage", None, Some(2.0), Decay::One);
        let t = Instant::now();
        let s = |secs: f64| t + Duration::from_secs_f64(secs);
        add("one", "g", &rage, 5.0, t);
        assert_eq!(get("one", "g", &rage, s(1.9)), 5);
        assert_eq!(get("one", "g", &rage, s(4.5)), 3);
        // The partial period carries over: next step at 6s.
        assert_eq!(get("one", "g", &rage, s(5.9)), 3);
        assert_eq!(get("one", "g", &rage, s(6.0)), 2);
        assert_eq!(get("one", "g", &rage, s(60.0)), 0);
    }
}
