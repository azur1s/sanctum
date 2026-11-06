//! Floating damage numbers. Shown when a player takes damage or deals it
//! (melee or skill); mob-vs-mob damage is skipped.
//!
//! The damage events report damage before armor, so the number shown is the
//! health the target really lost, measured one tick after the hit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pumpkin_plugin_api::{
    BillboardMode, DamageType, DisplayEntityExt, Entity, EntityDisplayExt, EntityType, Server,
    events::{EntityDamageEvent, EventData, EventHandler},
    persistent_data::PersistentDataHolder,
    scheduler::SchedulerExt,
};

use crate::config;
use crate::engine::text;
use crate::game::NS;
use crate::texts::{indicator, indicators as look};

/// Custom data key on indicator entities: when they were made (unix seconds).
const TAG: &str = "indicator";
/// Only damage within this range of some player is shown. Also how far `find`
/// looks, which combat depends on, so this is not a setting.
const RANGE: f64 = 32.0;

struct Pending {
    id: u32,
    health_before: f32,
    damage: f32,
    kind: DamageType,
    target_is_player: bool,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// Entities a player damaged this tick -> the damage kind, if a skill set one.
static BY_PLAYER: Mutex<Option<HashMap<u32, Option<String>>>> = Mutex::new(None);
/// Entities hit this tick -> the elements of those hits, for their icons.
static HIT_ELEMENTS: Mutex<Option<HashMap<u32, Vec<String>>>> = Mutex::new(None);
/// Entities that took a crit this tick, for their numbers' look.
static CRITS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static FLUSH_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Marks that a player damaged `entity` (with a skill's damage `kind`, if
/// any), so its number is shown in that kind's style.
pub fn note_player_damage(entity: u32, kind: Option<&str>) {
    let mut map = BY_PLAYER.lock().unwrap();
    let entry = map.get_or_insert_with(HashMap::new).entry(entity).or_insert(None);
    if kind.is_some() {
        *entry = kind.map(str::to_string);
    }
}

/// Marks the elements of a hit on `entity`, so its number shows their icons.
pub fn note_elements(entity: u32, elements: Vec<String>) {
    let mut map = HIT_ELEMENTS.lock().unwrap();
    let entry = map.get_or_insert_with(HashMap::new).entry(entity).or_default();
    for element in elements {
        if !entry.contains(&element) {
            entry.push(element);
        }
    }
}

/// Marks a hit on `entity` as a crit, so its number stands out.
pub fn note_crit(entity: u32) {
    CRITS.lock().unwrap().push(entity);
}

/// The damage type of the hit `entity` is taking right now, as a name like
/// "player_attack". `EntityDamageByEntityEvent` runs right after the
/// `EntityDamageEvent` recorded here, and only has an unparseable cause itself.
pub fn current_source(entity: u32) -> Option<String> {
    PENDING.lock().unwrap().iter().rev().find(|hit| hit.id == entity).map(|hit| source_name(hit.kind))
}

/// An entity within `RANGE` of some player, and whether it is a player.
pub fn find(server: &Server, id: u32) -> Option<(Entity, bool)> {
    for player in server.get_all_players() {
        let me = player.as_entity();
        if me.get_id() == id {
            return Some((me, true));
        }
        if let Some(e) = me.get_nearby_entities(RANGE, RANGE, RANGE).into_iter().find(|e| e.get_id() == id) {
            return Some((e, false));
        }
    }
    None
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A small pseudo-random offset so numbers from quick hits do not overlap.
fn jitter(seed: u32) -> f64 {
    let x = seed.wrapping_mul(2_654_435_761) >> 16;
    (f64::from(x % 1000) / 1000.0 - 0.5) * 0.6
}

/// "DamageType::PlayerAttack" -> "player_attack".
fn source_name(kind: DamageType) -> String {
    let debug = format!("{kind:?}");
    let name = debug.rsplit("::").next().unwrap_or(&debug);
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// The number's prefix and color: from the skill's kind, else the kind whose
/// `source` matches the damage type, else plain gray.
fn style(skill_kind: Option<&str>, source: DamageType) -> (String, String) {
    let kind = skill_kind
        .and_then(crate::game::kind)
        .or_else(|| crate::game::kind_for_source(&source_name(source)));
    match kind {
        Some(k) => {
            let color = if k.display.color.is_empty() { indicator(&look::KIND_COLOR, &[]) } else { k.display.color };
            (k.display.prefix, color)
        }
        None => (String::new(), indicator(&look::NO_KIND_COLOR, &[])),
    }
}

pub struct OnDamage;
impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, server: Server, event: EventData<EntityDamageEvent>) -> EventData<EntityDamageEvent> {
        let id = event.entity_id as u32;
        let Some((entity, target_is_player)) = find(&server, id) else { return event };
        let Some(living) = entity.as_living() else { return event };
        PENDING.lock().unwrap().push(Pending {
            id,
            health_before: living.get_health(),
            damage: event.damage,
            kind: event.damage_type,
            target_is_player,
        });
        // Measure all of this tick's hits together, one tick later.
        if !FLUSH_SCHEDULED.swap(true, Ordering::Relaxed) {
            server.schedule_delayed_task(1, |server: Server| flush(&server));
        }
        event
    }
}

fn flush(server: &Server) {
    FLUSH_SCHEDULED.store(false, Ordering::Relaxed);
    // Always drained, also with numbers off: combat reads `PENDING` through
    // `current_source`, so `OnDamage` keeps recording either way.
    let pending = std::mem::take(&mut *PENDING.lock().unwrap());
    let by_player = BY_PLAYER.lock().unwrap().take().unwrap_or_default();
    let hit_elements = HIT_ELEMENTS.lock().unwrap().take().unwrap_or_default();
    let crits = std::mem::take(&mut *CRITS.lock().unwrap());
    if !config::get().indicators {
        return;
    }
    let elements = crate::game::elements();
    for hit in pending {
        let by = by_player.get(&hit.id);
        if !hit.target_is_player && by.is_none() {
            continue;
        }
        let (prefix, color) = style(by.and_then(|k| k.as_deref()), hit.kind);
        let icons = hit_elements.get(&hit.id).map_or_else(String::new, |ids| elements.icons(ids));
        let number_color = if crits.contains(&hit.id) { indicator(&look::CRIT_COLOR, &[]) } else { color.clone() };
        let style = Style { icons, color, prefix, number_color };
        let Some((entity, _)) = find(server, hit.id) else { continue };
        let lost = match entity.as_living() {
            Some(l) if !l.is_dead() => hit.health_before - l.get_health(),
            // Dead (or gone): the event's damage is the best we have.
            _ => hit.damage,
        };
        // Blocked by armor, invulnerability frames, a shield, ...
        if lost < 0.05 {
            continue;
        }
        spawn(server, &entity, lost, &style);
    }
}

/// What one number looks like, for `number` in `indicators.kdl`.
struct Style {
    icons: String,
    color: String,
    prefix: String,
    number_color: String,
}

fn spawn(server: &Server, target: &Entity, amount: f32, style: &Style) {
    let (x, y, z) = target.get_position();
    let seed = target.get_id() ^ (now_secs() as u32) ^ amount.to_bits();
    let pos = (x + jitter(seed), y + target.get_height() as f64 + 0.2, z + jitter(seed.rotate_left(11)));
    let display = target.get_world().spawn_entity(EntityType::TextDisplay, pos);
    let Some(label) = display.as_text_display() else {
        display.remove();
        return;
    };
    let number = crate::items::expr::format_number(f64::from(amount));
    label.set_text(text(&indicator(&look::NUMBER, &[
        ("number_color", &style.number_color),
        ("icons", &style.icons),
        ("color", &style.color),
        ("prefix", &style.prefix),
        ("number", &number),
    ])));
    label.set_background(0);
    let config = config::get();
    label.set_shadow(config.indicator_shadow);
    label.get_display().set_billboard(BillboardMode::Center);
    display.set_string(NS, TAG, &now_secs().to_string());

    // Float up (the client animates it), then remove.
    let (lifetime, rise) = (config.indicator_lifetime, config.indicator_rise);
    let id = display.get_id();
    server.schedule_delayed_task(1, move |server: Server| {
        if let Some(d) = find(&server, id).and_then(|(e, _)| e.as_display()) {
            d.set_interpolation_start(0);
            d.set_interpolation_duration(lifetime as i32);
            d.set_translation(0.0, rise as f32, 0.0);
        }
    });
    server.schedule_delayed_task(u64::from(lifetime), move |server: Server| {
        if let Some((e, _)) = find(&server, id) {
            e.remove();
        }
    });
}

/// Removes old indicators near players: ones a restart or chunk unload left
/// behind. Call once per second.
pub fn sweep(server: &Server) {
    let now = now_secs();
    // A little past a number's lifetime, so only leftovers are caught.
    let max_age = u64::from(config::get().indicator_lifetime).div_ceil(20) + 2;
    for player in server.get_all_players() {
        for e in player.as_entity().get_nearby_entities(RANGE, RANGE, RANGE) {
            if let Some(made) = e.get_string(NS, TAG).and_then(|s| s.parse::<u64>().ok())
                && now.saturating_sub(made) > max_age
            {
                e.remove();
            }
        }
    }
}
