//! Arrows from ranged relics: drawn vanilla bows and `arrow` skills.
//!
//! Pumpkin reports an arrow hit with the arrow itself as the damager, never
//! its shooter, and the plugin API can't set an arrow's owner. So every arrow
//! from a ranged relic is remembered here by entity id, with its shooter and
//! their stats from the moment it left the bow, and `OnHit` swaps the vanilla
//! damage for the relic's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pumpkin_plugin_api::{
    Entity, Player, Server,
    events::{EntityDamageByEntityEvent, EventData, EventHandler, ProjectileHitEvent, ProjectileLaunchEvent},
    scheduler::SchedulerExt,
    wit::pumpkin::plugin::entity_types::EntityType,
};

use crate::engine::{self, Cast};
use crate::game::{self, player_key};
use crate::items::{self, PlayerStats};
use crate::stats;

/// How long an arrow is remembered. Long enough for any real shot; also when
/// skill arrows that never landed are cleared away.
const LIFETIME: Duration = Duration::from_secs(10);
/// Skill arrows that hit a block stay this long, so the shot reads, then vanish.
const STUCK_TICKS: u64 = 10;
/// A fully drawn bow launches its arrow at this many blocks per tick.
const FULL_DRAW_SPEED: f64 = 3.0;
/// How far ahead of the eyes a skill arrow appears, to start clear of the
/// shooter's own hitbox (the arrow has no owner, so it could hit them).
const MUZZLE: f64 = 1.0;

#[derive(Clone)]
pub struct Shot {
    /// The shooter's entity id.
    pub shooter: u32,
    pub shooter_key: String,
    /// The shooter's stats when the arrow left: swapping weapons mid-flight
    /// changes nothing.
    pub stats: Arc<PlayerStats>,
    /// From a drawn bow: damage follows how far it was drawn, like vanilla.
    pub drawn: bool,
    shot_at: Instant,
}

static ARROWS: Mutex<Option<HashMap<u32, Shot>>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut HashMap<u32, Shot>) -> T) -> T {
    let mut guard = ARROWS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    // Shots that never hit anything: forget them as they go.
    let now = Instant::now();
    map.retain(|_, s| now.duration_since(s.shot_at) < LIFETIME);
    f(map)
}

fn track(arrow: u32, player: &Player, stats: Arc<PlayerStats>, drawn: bool) {
    let shot = Shot { shooter: player.as_entity().get_id(), shooter_key: player_key(player), stats, drawn, shot_at: Instant::now() };
    with(|map| map.insert(arrow, shot));
}

/// Forgets a player's arrows, when they leave.
pub fn forget(key: &str) {
    with(|map| map.retain(|_, s| s.shooter_key != key));
}

/// An arrow from the shooter's world, by id.
fn find_arrow(player: &Player, id: u32) -> Option<Entity> {
    player.get_world().get_entities().into_iter().find(|e| e.get_id() == id)
}

/// `arrow speed=3 arc=0`: a real arrow from the caster's eyes along where they
/// look, turned `arc` degrees to the right (negative: left).
pub fn shoot(cast: &Cast, speed: f64, arc: f64) {
    let caster = cast.caster.as_entity();
    let dir = engine::look_turned(&caster, arc);
    let pos = engine::add(caster.get_eye_position(), engine::scale(dir, MUZZLE));
    let world = cast.caster.get_world();
    let arrow = world.spawn_entity(EntityType::Arrow, pos);
    arrow.set_rotation(caster.get_yaw() + arc as f32, caster.get_pitch());
    arrow.set_velocity(engine::scale(dir, speed));
    let id = arrow.get_id();
    track(id, &cast.caster, cast.stats.clone(), false);
    // Arrows that fly off without landing: clear them when they are forgotten.
    let key = player_key(&cast.caster);
    cast.server.schedule_delayed_task(LIFETIME.as_secs() * 20, move |server: Server| {
        if let Some(player) = server.get_all_players().into_iter().find(|p| player_key(p) == key)
            && let Some(arrow) = find_arrow(&player, id)
        {
            arrow.remove();
        }
    });
}

/// Remembers arrows loosed from a ranged relic held in the main hand.
pub struct OnLaunch;
impl EventHandler<ProjectileLaunchEvent> for OnLaunch {
    fn handle(&self, server: Server, event: EventData<ProjectileLaunchEvent>) -> EventData<ProjectileLaunchEvent> {
        let Some(shooter) = event.shooter_id else { return event };
        let Some(player) = server.get_all_players().into_iter().find(|p| p.as_entity().get_id() as i32 == shooter) else {
            return event;
        };
        if game::held_relic(&player).is_some_and(|def| def.ranged) {
            track(event.entity_id as u32, &player, Arc::new(stats::for_cast(&player)), true);
        }
        event
    }
}

/// A tracked arrow that hit a block is done. Skill arrows go away shortly
/// after; drawn ones stay to be picked up, like any arrow.
pub struct OnLand;
impl EventHandler<ProjectileHitEvent> for OnLand {
    fn handle(&self, server: Server, event: EventData<ProjectileHitEvent>) -> EventData<ProjectileHitEvent> {
        // Entity hits are handled (and forgotten) in `OnHit`.
        if event.hit_entity_id.is_some() {
            return event;
        }
        let id = event.entity_id as u32;
        let Some(shot) = with(|map| map.remove(&id)) else { return event };
        if !shot.drawn {
            let key = shot.shooter_key;
            server.schedule_delayed_task(STUCK_TICKS, move |server: Server| {
                if let Some(player) = server.get_all_players().into_iter().find(|p| player_key(p) == key)
                    && let Some(arrow) = find_arrow(&player, id)
                {
                    arrow.remove();
                }
            });
        }
        event
    }
}

/// How far a drawn bow was pulled, 0..=1, from the arrow's speed (vanilla
/// damage also follows the speed). Undrawn shots always count in full.
fn draw(shot: &Shot, near: &Entity, arrow: u32) -> f64 {
    if !shot.drawn {
        return 1.0;
    }
    let Some(arrow) = near.get_nearby_entities(6.0, 6.0, 6.0).into_iter().find(|e| e.get_id() == arrow) else { return 1.0 };
    let speed = engine::length(arrow.get_velocity());
    (speed / FULL_DRAW_SPEED).clamp(0.0, 1.0)
}

/// The arrow `shot` hit `event.entity_id`: the relic's damage instead of the
/// vanilla arrow's, and the shooter's `hit` skills. Returns false if the
/// arrow isn't one of ours.
pub fn on_hit(server: &Server, event: &mut EventData<EntityDamageByEntityEvent>) -> bool {
    let arrow = event.damager_id as u32;
    let victim = event.entity_id as u32;
    let Some(shot) = with(|map| map.get(&arrow).cloned()) else { return false };
    // Ownerless skill arrows can come back down on whoever fired them. A
    // cancelled hit bounces the arrow off, so it stays tracked.
    if victim == shot.shooter {
        event.cancelled = true;
        return true;
    }
    with(|map| map.remove(&arrow));
    crate::triggers::note_attacker(victim, shot.shooter);
    let Some((target, _)) = crate::indicators::find(server, victim) else { return true };

    let base = items::stat_total(&shot.stats, "damage");
    if base > 0.0 {
        let source = crate::indicators::current_source(victim);
        let defense = match source.as_deref().and_then(game::kind_for_source) {
            Some(kind) => kind.defense,
            None => Some("defense".to_string()),
        };
        crate::indicators::note_elements(victim, items::damage_elements(&shot.stats));
        let mut crit = 1.0;
        if let Some(multiplier) = stats::crit_multiplier(&shot.stats, engine::random()) {
            crit = multiplier;
            crate::indicators::note_crit(victim);
        }
        let multiplier = game::hit_multiplier(defense.as_deref(), None, Some(&shot.stats), &stats::of_entity(server, &target));
        event.damage = (base * draw(&shot, &target, arrow) * multiplier * crit) as f32;
    }

    if let Some(player) = server.get_all_players().into_iter().find(|p| player_key(p) == shot.shooter_key) {
        crate::triggers::note_hit(victim, &player);
        crate::indicators::note_player_damage(victim, None);
        engine::begin_hit_fold(victim);
        game::fire_hit(server, player, Some(target), false);
        event.damage += engine::end_hit_fold() as f32;
    }
    true
}
