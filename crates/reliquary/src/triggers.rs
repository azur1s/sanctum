//! Triggers that need bookkeeping: `damaged`, `kill`, `sneak`, `timer`,
//! `equip` and `unequip`. Clicks and melee hits are handled in `game.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pumpkin_plugin_api::{
    Entity, Player, Server,
    events::{EntityDamageEvent, EntityDeathEvent, EventData, EventHandler, PlayerToggleSneakEvent},
    scheduler::SchedulerExt,
};

use crate::game::{self, player_key};
use crate::items::{ItemDef, Trigger};
use crate::stats;

/// How long after a hit a kill still counts for that player.
fn kill_credit() -> Duration {
    Duration::from_secs_f64(crate::config::get().kill_credit)
}

/// Entity id -> (player UUID, when) of the last player hit, for `kill`.
static LAST_HIT: Mutex<Option<HashMap<u32, (String, Instant)>>> = Mutex::new(None);
/// Damaged entity id -> attacker entity id, for `damaged` skills' `attacker`.
static ATTACKERS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
/// Player UUID -> "slot:item" and the item, of every relic it had in its slot
/// last second. Keeps the item as it was then, with its gems, for `unequip`.
static EQUIPPED: Mutex<Option<HashMap<String, Vec<(String, Arc<ItemDef>)>>>> = Mutex::new(None);
/// Seconds since the plugin loaded, for `timer`.
static SECONDS: AtomicU64 = AtomicU64::new(0);

/// Remembers that `player` hit `entity` (melee or a skill), for `kill`.
pub fn note_hit(entity: u32, player: &Player) {
    let mut map = LAST_HIT.lock().unwrap();
    map.get_or_insert_with(HashMap::new).insert(entity, (player_key(player), Instant::now()));
}

/// Remembers who damaged `entity`, for `damaged` skills.
pub fn note_attacker(entity: u32, attacker: u32) {
    ATTACKERS.lock().unwrap().get_or_insert_with(HashMap::new).insert(entity, attacker);
}

/// A fresh handle to the online player with this UUID.
fn online(server: &Server, key: &str) -> Option<Player> {
    server.get_all_players().into_iter().find(|p| player_key(p) == key)
}

/// An entity near the player (or the player itself) by id.
fn near(player: &Player, id: u32) -> Option<Entity> {
    player
        .as_entity()
        .get_nearby_entities(32.0, 32.0, 32.0)
        .into_iter()
        .find(|e| e.get_id() == id)
}

/// Fires `trigger` on every relic the player has in its slot.
fn fire_equipped(server: &Server, player: Player, other: Option<Entity>, trigger: Trigger) {
    let items: Vec<_> = stats::equipped(&player).into_iter().map(|s| s.item).collect();
    game::fire(server, player, other, &items, |t| t == trigger);
}

pub struct OnDamaged;
impl EventHandler<EntityDamageEvent> for OnDamaged {
    fn handle(&self, server: Server, event: EventData<EntityDamageEvent>) -> EventData<EntityDamageEvent> {
        let id = event.entity_id as u32;
        let Some(key) = server
            .get_all_players()
            .into_iter()
            .find(|p| p.as_entity().get_id() == id)
            .map(|p| player_key(&p))
        else {
            return event;
        };
        // Run one tick later: by then the attacker (if any) is known, and the
        // skill does not run inside the server's damage code.
        server.schedule_delayed_task(1, move |server: Server| {
            let attacker = ATTACKERS.lock().unwrap().as_mut().and_then(|m| m.remove(&id));
            let Some(player) = online(&server, &key) else { return };
            let other = attacker.and_then(|a| near(&player, a));
            fire_equipped(&server, player, other, Trigger::Damaged);
        });
        event
    }
}

pub struct OnDeath;
impl EventHandler<EntityDeathEvent> for OnDeath {
    fn handle(&self, server: Server, event: EventData<EntityDeathEvent>) -> EventData<EntityDeathEvent> {
        let id = event.entity_id as u32;
        let credit = LAST_HIT.lock().unwrap().as_mut().and_then(|m| m.remove(&id));
        if let Some((key, when)) = credit
            && when.elapsed() <= kill_credit()
            && let Some(player) = online(&server, &key)
        {
            let victim = near(&player, id);
            fire_equipped(&server, player, victim, Trigger::Kill);
        }
        event
    }
}

pub struct OnSneak;
impl EventHandler<PlayerToggleSneakEvent> for OnSneak {
    fn handle(&self, server: Server, event: EventData<PlayerToggleSneakEvent>) -> EventData<PlayerToggleSneakEvent> {
        if event.is_sneaking
            && let Some(player) = online(&server, &player_key(&event.player))
        {
            fire_equipped(&server, player, None, Trigger::Sneak);
        }
        event
    }
}

/// Call once per second, before the per-player work.
pub fn next_second() -> u64 {
    // Forget kill credit and attackers nobody used.
    let credit = kill_credit();
    if let Some(map) = LAST_HIT.lock().unwrap().as_mut() {
        map.retain(|_, (_, when)| when.elapsed() <= credit);
    }
    if let Some(map) = ATTACKERS.lock().unwrap().as_mut() {
        map.clear();
    }
    SECONDS.fetch_add(1, Ordering::Relaxed) + 1
}

/// Once per second per player: `equip` / `unequip` for relics that entered or
/// left their slot since last second, then `timer` skills that are due.
pub fn second(server: &Server, key: &str, seconds: u64) {
    let Some(player) = online(server, key) else { return };
    let now: Vec<(String, Arc<ItemDef>)> = stats::equipped(&player)
        .into_iter()
        .map(|s| (format!("{:?}:{}", s.slot, s.item.id), s.item))
        .collect();
    let before = {
        let mut map = EQUIPPED.lock().unwrap();
        let map = map.get_or_insert_with(HashMap::new);
        map.insert(key.to_string(), now.clone()).unwrap_or_default()
    };

    let added: Vec<_> = now.iter().filter(|(k, _)| !before.iter().any(|(b, _)| b == k)).map(|(_, d)| d.clone()).collect();
    let removed: Vec<_> = before.into_iter().filter(|(k, _)| !now.iter().any(|(n, _)| n == k)).map(|(_, d)| d).collect();
    let timed: Vec<_> = now.into_iter().map(|(_, d)| d).collect();

    for (items, trigger) in [(removed, Trigger::Unequip), (added, Trigger::Equip)] {
        if !items.is_empty()
            && let Some(player) = online(server, key)
        {
            game::fire(server, player, None, &items, |t| t == trigger);
        }
    }
    if let Some(player) = online(server, key) {
        game::fire(server, player, None, &timed, |t| matches!(t, Trigger::Timer(n) if seconds % u64::from(n) == 0));
    }
}

pub fn forget(key: &str) {
    if let Some(map) = EQUIPPED.lock().unwrap().as_mut() {
        map.remove(key);
    }
}
