//! Runs skills on the server: finds targets, checks conditions and applies
//! mechanics. Delayed parts (`wait`, `repeat`) go through the scheduler and
//! look entities up again by id, since entities can die or leave meanwhile.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pumpkin_plugin_api::{
    Entity, Player, Server, scheduler::SchedulerExt,
    text::TextComponent,
    wit::pumpkin::plugin::{sounds::SoundCategory, status_effect::StatusEffectInstance},
};

use crate::items::{Action, Actions, Check, Condition, ItemDef, Mechanic, Num, Params, PlayerStats, Scope, Targeter, Trigger, stat_key};
use crate::{admin, mana, names};

type Vec3 = (f64, f64, f64);

/// Live handles for one run of a skill.
pub struct Cast<'a> {
    pub server: &'a Server,
    pub caster: Player,
    /// The hit entity, for `hit` skills.
    pub victim: Option<Entity>,
    /// Param values for this item's use of the skill, plus the `repeat` vars
    /// of whatever is running now.
    pub params: RefCell<Arc<Params>>,
    /// The caster's stats when the skill started (for `self.<stat>`).
    pub stats: Arc<PlayerStats>,
    /// Started by `triggers-hit`: its own damage does not trigger hits again.
    pub from_proc: bool,
}

impl Cast<'_> {
    fn num(&self, n: &Num) -> f64 {
        let params = self.params.borrow();
        n.get(Scope { params: &params, stats: &self.stats })
    }

    /// Runs `f` with `var` (if any) set to `i`. Anything `f` schedules keeps the value.
    fn with_var<T>(&self, var: Option<&str>, i: u32, f: impl FnOnce() -> T) -> T {
        let Some(var) = var else { return f() };
        let mut params = Params::clone(&self.params.borrow());
        params.insert(var.to_string(), f64::from(i));
        let outer = self.params.replace(Arc::new(params));
        let result = f();
        self.params.replace(outer);
        result
    }
}

/// What a mechanic acts on: an entity or a point in the caster's world.
pub enum Subject {
    Entity(Entity),
    Point(Vec3),
}

impl Subject {
    /// Center of an entity's body, or the point itself.
    fn pos(&self) -> Vec3 {
        match self {
            Subject::Entity(e) => center(e),
            Subject::Point(p) => *p,
        }
    }
}

/// Particles that need extra data (a color, block, item, ...). Pumpkin's
/// plugin `spawn_particle` always sends none, and the client disconnects on
/// such a packet ("Failed to decode packet 'level_particles'"), so these are
/// rejected on load. From vanilla's particle types; new 26.x particles are
/// not checked.
const NEEDS_DATA: &[&str] = &[
    "block", "block_marker", "block_crumble", "falling_dust", "dust_pillar", "dust",
    "dust_color_transition", "effect", "instant_effect", "entity_effect", "flash",
    "tinted_leaves", "dragon_breath", "item", "vibration", "trail", "sculk_charge", "shriek",
];

/// The particle for a name, unless it is unknown or needs extra data.
fn safe_particle(name: &str) -> Option<pumpkin_plugin_api::particles::Particle> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    if NEEDS_DATA.contains(&bare) { None } else { names::particle(name) }
}

/// Why a particle name can't be used, if it can't.
fn particle_problem(name: &str) -> Option<String> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    if NEEDS_DATA.contains(&bare) {
        Some(format!("particle `{bare}` needs extra data (color, block, ...) that Pumpkin's plugin API can't send yet; it would disconnect players"))
    } else if names::particle(name).is_none() {
        Some(format!("unknown particle `{name}`"))
    } else {
        None
    }
}

/// How far around the caster delayed actions look for their entities again.
const LOOKUP_RANGE: f64 = 64.0;

pub fn text(s: &str) -> TextComponent {
    TextComponent::from_legacy_string_with_code(&crate::items::expand_hex(s), '&')
}

pub use crate::items::cooldown_group;

/// Minimum time between two identical denial messages to one player.
const NOTICE_DELAY: Duration = Duration::from_secs(1);

static LAST_NOTICE: Mutex<Option<HashMap<(String, &'static str), Instant>>> = Mutex::new(None);

/// Tells `player` why a cast was denied, at most once per `NOTICE_DELAY` per
/// kind, so spamming or holding right click doesn't flood the chat.
fn notice(player: &Player, kind: &'static str, msg: &str) {
    let now = Instant::now();
    let mut guard = LAST_NOTICE.lock().unwrap();
    let last = guard.get_or_insert_with(HashMap::new);
    let key = (player.get_id().to_string(), kind);
    if last.get(&key).is_some_and(|t| now.duration_since(*t) < NOTICE_DELAY) {
        return;
    }
    last.insert(key, now);
    player.send_system_message(text(msg), false);
}

/// A random number in 0.0..1.0, from the server's UUID generator.
/// Uses the low 53 bits: the top bits of a v4 UUID are fixed variant bits.
pub(crate) fn random() -> f64 {
    let id = pumpkin_plugin_api::uuid::generate();
    (id.low & ((1u64 << 53) - 1)) as f64 / (1u64 << 53) as f64
}

/// Tries to run one skill of `item`. Returns false if it did not run.
pub fn try_cast(cast: &Cast, item: &ItemDef, skill_index: usize) -> bool {
    let use_ = &item.skills[skill_index];
    let skill = &use_.skill;
    let me = Subject::Entity(cast.caster.as_entity());

    if use_.chance < 1.0 && random() >= use_.chance {
        return false;
    }
    if let Subject::Entity(caster) = &me
        && !skill.conditions.iter().all(|c| check(c, caster))
    {
        return false;
    }
    let group = cooldown_group(item, skill_index);
    let cooldown = if admin::no_cooldown() { 0.0 } else { use_.cooldown };
    if cooldown > 0.0 && cast.caster.is_on_cooldown(&group) {
        if skill.trigger != Trigger::Hit {
            // get_cooldown is the fraction left, so scale by the full duration.
            let left = (cast.caster.get_cooldown(&group) * cooldown).ceil().max(1.0) as u32;
            notice(&cast.caster, "cooldown", &format!("&cThis ability is on cooldown for {left}s"));
        }
        return false;
    }
    // A required target block that finds nothing cancels the skill for free.
    for action in skill.actions.iter() {
        if let Action::Target { targeter, filter, required: true, .. } = action
            && targets(cast, targeter, filter, &me).is_empty()
        {
            run(cast, &skill.on_fail, 0, &me);
            return false;
        }
    }
    let key = cast.caster.get_id().to_string();
    if use_.mana_cost > 0 && !admin::infinite_mana() && mana::try_spend(&key, use_.mana_cost).is_none() {
        if skill.trigger != Trigger::Hit {
            notice(&cast.caster, "mana", "&cNot enough mana");
        }
        return false;
    }
    if cooldown > 0.0 {
        cast.caster.start_cooldown(&group, (cooldown * 20.0).round() as i32);
    }
    run(cast, &skill.actions, 0, &me);
    true
}

/// Runs `actions[from..]` with `subject` as "self".
fn run(cast: &Cast, actions: &Actions, from: usize, subject: &Subject) {
    for (i, action) in actions.iter().enumerate().skip(from) {
        match action {
            Action::Wait(ticks) => {
                let rest = actions.clone();
                let ticks = cast.num(ticks).max(0.0) as u32;
                later(cast, subject, ticks, move |cast, subject| run(cast, &rest, i + 1, subject));
                return;
            }
            Action::Repeat { times, every, var, actions: body } => {
                let times = cast.num(times).max(0.0) as u32;
                let looping = Looping { body: body.clone(), every: every.clone(), var: var.clone(), last: times.saturating_sub(1) };
                repeat(cast, subject, looping, 0);
            }
            Action::Target { targeter, filter, actions: body, .. } => {
                for target in targets(cast, targeter, filter, subject) {
                    run(cast, body, 0, &target);
                }
            }
            Action::Projectile { speed, range, radius, trail, hit, end } => {
                let caster = cast.caster.as_entity();
                let shot = Shot {
                    dir: look(&caster),
                    speed: cast.num(speed).max(0.05),
                    left: cast.num(range).max(0.0),
                    radius: cast.num(radius).max(0.1),
                    shooter: caster.get_id(),
                    trail: trail.clone(),
                    hit: hit.clone(),
                    end: end.clone(),
                };
                fly(cast, caster.get_eye_position(), shot);
            }
            Action::Chain { jumps, radius, delay, beam, actions: body } => {
                if let Subject::Entity(e) = subject {
                    let link = ChainLink {
                        left: cast.num(jumps).max(0.0) as u32,
                        radius: cast.num(radius).max(0.0),
                        delay: cast.num(delay).max(0.0) as u32,
                        beam: beam.clone(),
                        actions: body.clone(),
                        hit: Vec::new(),
                    };
                    chain_step(cast, e, link);
                }
            }
            Action::Mechanic(mechanic) => apply(cast, mechanic, subject),
        }
    }
}

/// A projectile in flight. Moves `speed` blocks per tick in small steps.
#[derive(Clone)]
struct Shot {
    dir: Vec3,
    speed: f64,
    /// Blocks it may still travel.
    left: f64,
    radius: f64,
    /// Never hits whoever fired it.
    shooter: u32,
    trail: Actions,
    hit: Actions,
    end: Actions,
}

/// Collision checks happen at least every this many blocks.
const SHOT_STEP: f64 = 0.5;

fn fly(cast: &Cast, mut pos: Vec3, mut shot: Shot) {
    let world = cast.caster.get_world();
    let mut moved = 0.0;
    while moved < shot.speed {
        let step = SHOT_STEP.min(shot.speed - moved).min(shot.left);
        if step <= 0.0 {
            run(cast, &shot.end, 0, &Subject::Point(pos));
            return;
        }
        let next = add(pos, scale(shot.dir, step));
        if let Some(wall) = world.ray_trace_blocks(pos, next) {
            run(cast, &shot.end, 0, &Subject::Point(wall));
            return;
        }
        let target = entities_near(cast, next, shot.radius)
            .into_iter()
            .filter(|e| e.get_id() != shot.shooter && e.is_living())
            .min_by(|a, b| length(sub(center(a), next)).total_cmp(&length(sub(center(b), next))));
        if let Some(target) = target {
            run(cast, &shot.hit, 0, &Subject::Entity(target));
            return;
        }
        run(cast, &shot.trail, 0, &Subject::Point(next));
        pos = next;
        moved += step;
        shot.left -= step;
    }
    later(cast, &Subject::Point(pos), 1, move |cast, _| fly(cast, pos, shot.clone()));
}

/// The state of a `chain` between jumps.
#[derive(Clone)]
struct ChainLink {
    /// Targets still to hit, including this one.
    left: u32,
    radius: f64,
    delay: u32,
    beam: Option<String>,
    actions: Actions,
    /// Entities already hit; the chain never returns to them.
    hit: Vec<u32>,
}

fn chain_step(cast: &Cast, target: &Entity, mut link: ChainLink) {
    if link.left == 0 {
        return;
    }
    let Some(here) = find_entity(&cast.caster, target.get_id()) else { return };
    let from = center(&here);
    link.hit.push(here.get_id());
    run(cast, &link.actions, 0, &Subject::Entity(here));
    link.left -= 1;
    if link.left == 0 {
        return;
    }
    let caster_id = cast.caster.as_entity().get_id();
    let next = entities_near(cast, from, link.radius)
        .into_iter()
        .filter(|e| e.is_living() && e.get_id() != caster_id && !link.hit.contains(&e.get_id()))
        .min_by(|a, b| length(sub(center(a), from)).total_cmp(&length(sub(center(b), from))));
    let Some(next) = next else { return };
    if let Some(kind) = link.beam.as_deref().and_then(safe_particle) {
        let to = center(&next);
        let steps = (length(sub(to, from)) / 0.3).ceil().max(1.0) as usize;
        let world = cast.caster.get_world();
        for i in 1..steps {
            let p = add(from, scale(sub(to, from), i as f64 / steps as f64));
            world.spawn_particle(kind, p, (0.0, 0.0, 0.0), 0.0, 1);
        }
    }
    let delay = link.delay;
    later(cast, &Subject::Entity(next), delay, move |cast, subject| {
        if let Subject::Entity(e) = subject {
            chain_step(cast, e, link.clone());
        }
    });
}

/// The state of a `repeat` between iterations.
#[derive(Clone)]
struct Looping {
    body: Actions,
    every: Num,
    var: Option<String>,
    /// The final iteration number.
    last: u32,
}

/// Runs iteration `i`, then schedules the next after the gap `i` asks for.
fn repeat(cast: &Cast, subject: &Subject, looping: Looping, i: u32) {
    let var = looping.var.as_deref();
    let ticks = cast.with_var(var, i, || {
        run(cast, &looping.body, 0, subject);
        cast.num(&looping.every).max(1.0) as u32
    });
    if i < looping.last {
        later(cast, subject, ticks, move |cast, subject| repeat(cast, subject, looping.clone(), i + 1));
    }
}

/// Calls `f` after `ticks`, with fresh handles for the caster and subject.
fn later(cast: &Cast, subject: &Subject, ticks: u32, f: impl Fn(&Cast, &Subject) + Send + Sync + 'static) {
    enum Saved {
        Entity(u32),
        Point(Vec3),
    }
    let caster_id = cast.caster.get_id().to_string();
    let victim_id = cast.victim.as_ref().map(Entity::get_id);
    let (params, stats, from_proc) = (cast.params.borrow().clone(), cast.stats.clone(), cast.from_proc);
    let saved = match subject {
        Subject::Entity(e) => Saved::Entity(e.get_id()),
        Subject::Point(p) => Saved::Point(*p),
    };
    cast.server.schedule_delayed_task(ticks.max(1) as u64, move |server: Server| {
        let Some(caster) = server.get_all_players().into_iter().find(|p| p.get_id().to_string() == caster_id) else {
            return;
        };
        let victim = victim_id.and_then(|id| find_entity(&caster, id));
        let subject = match saved {
            Saved::Entity(id) => match find_entity(&caster, id) {
                Some(e) => Subject::Entity(e),
                None => return,
            },
            Saved::Point(p) => Subject::Point(p),
        };
        f(&Cast { server: &server, caster, victim, params: RefCell::new(params.clone()), stats: stats.clone(), from_proc }, &subject);
    });
}

/// A fresh handle to an entity near the caster (or the caster itself).
fn find_entity(caster: &Player, id: u32) -> Option<Entity> {
    let me = caster.as_entity();
    if me.get_id() == id {
        return Some(me);
    }
    me.get_nearby_entities(LOOKUP_RANGE, LOOKUP_RANGE, LOOKUP_RANGE)
        .into_iter()
        .find(|e| e.get_id() == id)
}

// ---- Targeters -------------------------------------------------------------------

fn add(a: Vec3, b: Vec3) -> Vec3 {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    (a.0 - b.0, a.1 - b.1, a.2 - b.2)
}

fn scale(a: Vec3, s: f64) -> Vec3 {
    (a.0 * s, a.1 * s, a.2 * s)
}

fn length(v: Vec3) -> f64 {
    (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt()
}

/// Center of an entity's body.
fn center(e: &Entity) -> Vec3 {
    let (x, y, z) = e.get_position();
    (x, y + e.get_height() as f64 / 2.0, z)
}

/// Unit vector of where an entity is looking.
fn look(e: &Entity) -> Vec3 {
    let (yaw, pitch) = (e.get_yaw().to_radians() as f64, e.get_pitch().to_radians() as f64);
    (-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos())
}

/// Flat unit vector of where an entity faces, rotated by `degrees` (positive = right).
fn facing(e: &Entity, degrees: f64) -> Vec3 {
    let yaw = (e.get_yaw() as f64 + degrees).to_radians();
    (-yaw.sin(), 0.0, yaw.cos())
}

/// Living entities within `range` of `origin`.
fn entities_near(cast: &Cast, origin: Vec3, range: f64) -> Vec<Entity> {
    let me = cast.caster.as_entity();
    let reach = range + length(sub(origin, me.get_position()));
    let mut found: Vec<Entity> = me
        .get_nearby_entities(reach, reach, reach)
        .into_iter()
        .filter(|e| length(sub(center(e), origin)) <= range)
        .collect();
    if length(sub(center(&me), origin)) <= range {
        found.push(me);
    }
    found
}

fn targets(cast: &Cast, targeter: &Targeter, filter: &[Condition], subject: &Subject) -> Vec<Subject> {
    let caster = cast.caster.as_entity();
    let n = |v: &Num| cast.num(v);
    let feet = caster.get_position();

    let entities: Vec<Entity> = match targeter {
        Targeter::Myself => {
            return match subject {
                Subject::Entity(e) => find_entity(&cast.caster, e.get_id()).map(Subject::Entity).into_iter().collect(),
                Subject::Point(p) => vec![Subject::Point(*p)],
            };
        }
        Targeter::Victim | Targeter::Attacker => cast
            .victim
            .as_ref()
            .and_then(|v| find_entity(&cast.caster, v.get_id()))
            .into_iter()
            .collect(),
        Targeter::Ray { range } => caster.ray_trace_entity(n(range)).map(|hit| hit.entity).into_iter().collect(),
        Targeter::Radius { range } => {
            let origin = subject.pos();
            let id = match subject {
                Subject::Entity(e) => Some(e.get_id()),
                Subject::Point(_) => None,
            };
            entities_near(cast, origin, n(range))
                .into_iter()
                .filter(|e| Some(e.get_id()) != id)
                .collect()
        }
        Targeter::Cone { range, angle } => {
            let (eye, dir, range) = (caster.get_eye_position(), look(&caster), n(range));
            let min_cos = (n(angle) / 2.0).to_radians().cos();
            caster
                .get_nearby_entities(range, range, range)
                .into_iter()
                .filter(|e| {
                    let to = sub(center(e), eye);
                    let dist = length(to);
                    dist <= range && (dist < 0.5 || (to.0 * dir.0 + to.1 * dir.1 + to.2 * dir.2) / dist >= min_cos)
                })
                .collect()
        }
        Targeter::Forward { distance, height } => {
            let p = add(add(feet, scale(facing(&caster, 0.0), n(distance))), (0.0, n(height), 0.0));
            return vec![Subject::Point(p)];
        }
        Targeter::Arc { radius, angle, points, height } => {
            let (count, angle) = (n(points).max(1.0) as usize, n(angle));
            let base = add(feet, (0.0, n(height), 0.0));
            return (0..count)
                .map(|i| {
                    let t = if count == 1 { 0.5 } else { i as f64 / (count - 1) as f64 };
                    Subject::Point(add(base, scale(facing(&caster, (t - 0.5) * angle), n(radius))))
                })
                .collect();
        }
        Targeter::Line { range, step } => {
            let (eye, dir, step) = (caster.get_eye_position(), look(&caster), n(step).max(0.1));
            let count = (n(range) / step).floor() as usize;
            return (1..=count).map(|i| Subject::Point(add(eye, scale(dir, step * i as f64)))).collect();
        }
        Targeter::Ring { radius, points, height } => {
            let count = n(points).max(1.0) as usize;
            let origin = match subject {
                Subject::Entity(e) => e.get_position(),
                Subject::Point(p) => *p,
            };
            let base = add(origin, (0.0, n(height), 0.0));
            return (0..count)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / count as f64;
                    Subject::Point(add(base, (a.cos() * n(radius), 0.0, a.sin() * n(radius))))
                })
                .collect();
        }
    };
    entities
        .into_iter()
        .filter(|e| e.is_living())
        .filter(|e| filter.iter().all(|c| check(c, e)))
        .map(Subject::Entity)
        .collect()
}

// ---- Conditions ------------------------------------------------------------------

/// "EntityType::ZombieVillager" -> "zombie_villager".
fn type_id(e: &Entity) -> String {
    let debug = format!("{:?}", e.get_type());
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

fn check(condition: &Condition, e: &Entity) -> bool {
    let result = match &condition.check {
        Check::Sneaking => e.is_sneaking(),
        Check::Sprinting => e.is_sprinting(),
        Check::OnGround => e.is_on_ground(),
        Check::InWater => e.is_in_water(),
        Check::Burning => e.is_on_fire(),
        Check::IsPlayer => type_id(e) == "player",
        Check::Type(t) => type_id(e) == *t,
        Check::HealthBelow(fraction) => e
            .as_living()
            .is_some_and(|l| l.get_health() < l.get_max_health() * *fraction as f32),
    };
    result != condition.negate
}

// ---- Mechanics -------------------------------------------------------------------

fn player_of(cast: &Cast, e: &Entity) -> Option<Player> {
    let id = e.get_id();
    cast.server.get_all_players().into_iter().find(|p| p.as_entity().get_id() == id)
}

/// Sets an entity's velocity. Players go through their own handle: `Entity`'s
/// only stores the value on the server, and a player's client moves itself, so
/// it needs the velocity packet that `Player`'s sends.
fn push(cast: &Cast, e: &Entity, velocity: Vec3) {
    match player_of(cast, e) {
        Some(player) => player.set_velocity(velocity),
        None => e.set_velocity(velocity),
    }
}

fn apply(cast: &Cast, mechanic: &Mechanic, subject: &Subject) {
    let n = |v: &Num| cast.num(v);
    let world = cast.caster.get_world();

    // Mechanics that work on points as well as entities.
    match mechanic {
        Mechanic::Particle { particle, count, spread, speed } => {
            if let Some(kind) = safe_particle(particle) {
                let s = n(spread);
                world.spawn_particle(kind, subject.pos(), (s, s, s), n(speed) as f32, n(count) as i32);
            }
            return;
        }
        Mechanic::Sound { sound, volume, pitch } => {
            world.play_custom_sound(sound, SoundCategory::Players, subject.pos(), n(volume) as f32, n(pitch) as f32);
            return;
        }
        Mechanic::Lightning { effect_only } => {
            let pos = match subject {
                Subject::Entity(e) => e.get_position(),
                Subject::Point(p) => *p,
            };
            world.strike_lightning(pos, *effect_only);
            return;
        }
        Mechanic::Message { text: msg } => {
            cast.caster.send_system_message(text(msg), false);
            return;
        }
        _ => {}
    }

    // The loader rejects entity mechanics on points, so this is a no-op guard.
    let Subject::Entity(e) = subject else { return };
    match mechanic {
        Mechanic::Damage { amount, kind, element, scale, source, triggers_hit } => {
            let kind = kind.as_deref().and_then(crate::game::kind);
            // The kind's power stat, plus what is tagged with this hit's element.
            let power = kind.as_ref().and_then(|k| k.power.as_ref()).map_or(0.0, |stat| {
                let own = cast.stats.get(stat).copied().unwrap_or(0.0);
                let tagged = element.as_ref().and_then(|el| cast.stats.get(&stat_key(stat, Some(el)))).copied().unwrap_or(0.0);
                own + tagged
            });
            let source = source
                .as_deref()
                .or(kind.as_ref().map(|k| k.source.as_str()))
                .unwrap_or("generic");
            let mut total = n(amount) + power * n(scale);
            // The target's defense for this kind and its element mix.
            let stats = crate::stats::of_entity(cast.server, e);
            total *= crate::game::hit_multiplier(kind.as_ref().and_then(|k| k.defense.as_deref()), element.as_deref(), None, &stats);
            // A skill that costs its caster health should not crit them.
            let crit = if e.get_id() == cast.caster.as_entity().get_id() { None } else { crate::stats::crit_multiplier(&cast.stats, random()) };
            total *= crit.unwrap_or(1.0);
            if let (Some(living), Some(source)) = (e.as_living(), names::damage_type(source)) {
                living.damage(total as f32, source);
                // Skill damage has no attacker in the API, so remember it for
                // `kill` and for damage numbers.
                crate::triggers::note_hit(e.get_id(), &cast.caster);
                crate::indicators::note_player_damage(e.get_id(), kind.as_ref().map(|k| k.id.as_str()));
                crate::indicators::note_elements(e.get_id(), element.iter().cloned().collect());
                if crit.is_some() {
                    crate::indicators::note_crit(e.get_id());
                }
                if *triggers_hit && !cast.from_proc && e.get_id() != cast.caster.as_entity().get_id() {
                    crate::game::proc_hit(cast.server, &cast.caster, e);
                }
            }
        }
        Mechanic::Heal { amount } => {
            if let Some(living) = e.as_living() {
                let health = (living.get_health() + n(amount) as f32).min(living.get_max_health());
                living.set_health(health);
            }
        }
        Mechanic::Ignite { seconds } => e.set_fire_ticks((n(seconds) * 20.0) as i32),
        Mechanic::Knockback { strength, up } => {
            let caster = cast.caster.as_entity();
            let d = if caster.get_id() == e.get_id() { look(&caster) } else { sub(e.get_position(), caster.get_position()) };
            let len = (d.0 * d.0 + d.2 * d.2).sqrt().max(1e-6);
            let s = n(strength);
            push(cast, e, (d.0 / len * s, n(up), d.2 / len * s));
        }
        Mechanic::Effect { effect, seconds, level } => {
            if let (Some(player), Some(kind)) = (player_of(cast, e), names::status_effect(effect)) {
                player.add_effect(StatusEffectInstance {
                    effect_type: kind,
                    duration: (n(seconds) * 20.0) as u32,
                    amplifier: (n(level).clamp(1.0, 256.0) - 1.0) as u8,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                });
            }
        }
        Mechanic::Glow { seconds } => {
            e.set_glowing(true);
            later(cast, subject, (n(seconds) * 20.0) as u32, |_, s| {
                if let Subject::Entity(e) = s {
                    e.set_glowing(false);
                }
            });
        }
        Mechanic::Dash { strength, up } => {
            let dir = look(&cast.caster.as_entity());
            push(cast, e, add(scale(dir, n(strength)), (0.0, n(up), 0.0)));
        }
        Mechanic::Pull { strength } => {
            let d = sub(cast.caster.as_entity().get_position(), e.get_position());
            let len = (d.0 * d.0 + d.2 * d.2).sqrt().max(1e-6);
            let s = n(strength);
            push(cast, e, (d.0 / len * s, 0.25 * s, d.2 / len * s));
        }
        Mechanic::Blink { distance } => {
            let caster = cast.caster.as_entity();
            let (dir, eye) = (look(&caster), caster.get_eye_position());
            let mut end = add(eye, scale(dir, n(distance)));
            if let Some(wall) = world.ray_trace_blocks(eye, end) {
                // Stop a little before the wall.
                end = sub(wall, scale(dir, 0.6));
            }
            let feet = sub(end, (0.0, caster.get_eye_height() as f64, 0.0));
            match player_of(cast, e) {
                Some(player) => player.teleport(feet, None, None, world),
                None => e.teleport(feet, world),
            }
        }
        Mechanic::Particle { .. } | Mechanic::Sound { .. } | Mechanic::Lightning { .. } | Mechanic::Message { .. } => {}
    }
}

// ---- Validation ------------------------------------------------------------------

/// Checks names the loader cannot know about (particles, effects, ...).
pub fn validate(skill: &crate::items::SkillDef) -> Vec<String> {
    let mut problems = Vec::new();
    validate_actions(skill, &skill.actions, &mut problems);
    validate_actions(skill, &skill.on_fail, &mut problems);
    problems
}

fn validate_actions(skill: &crate::items::SkillDef, actions: &Actions, problems: &mut Vec<String>) {
    let at = format!("skill `{}`", skill.id);
    for action in actions.iter() {
        match action {
            Action::Target { targeter, actions, .. } => {
                if matches!(targeter, Targeter::Victim) && !matches!(skill.trigger, Trigger::Hit | Trigger::Kill) {
                    problems.push(format!("{at}: `target \"victim\"` only works in `hit` and `kill` skills"));
                }
                if matches!(targeter, Targeter::Attacker) && skill.trigger != Trigger::Damaged {
                    problems.push(format!("{at}: `target \"attacker\"` only works in `damaged` skills"));
                }
                validate_actions(skill, actions, problems);
            }
            Action::Repeat { actions, .. } => validate_actions(skill, actions, problems),
            Action::Projectile { trail, hit, end, .. } => {
                for actions in [trail, hit, end] {
                    validate_actions(skill, actions, problems);
                }
            }
            Action::Chain { beam, actions, .. } => {
                if let Some(problem) = beam.as_deref().and_then(particle_problem) {
                    problems.push(format!("{at}: chain beam: {problem}"));
                }
                validate_actions(skill, actions, problems);
            }
            Action::Wait(_) => {}
            Action::Mechanic(m) => {
                let bad = match m {
                    Mechanic::Damage { source: Some(source), .. } if names::damage_type(source).is_none() => {
                        Some(format!("unknown damage source `{source}`"))
                    }
                    Mechanic::Effect { effect, .. } if names::status_effect(effect).is_none() => {
                        Some(format!("unknown effect `{effect}`"))
                    }
                    Mechanic::Particle { particle, .. } => particle_problem(particle),
                    _ => None,
                };
                if let Some(bad) = bad {
                    problems.push(format!("{at}: {bad}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn particles_that_need_data_are_rejected() {
        assert!(particle_problem("flash").unwrap().contains("needs extra data"));
        assert!(particle_problem("minecraft:dust").unwrap().contains("needs extra data"));
        assert!(particle_problem("nope").unwrap().starts_with("unknown particle"));
        assert_eq!(particle_problem("end_rod"), None);
        assert!(safe_particle("flash").is_none());
        assert!(safe_particle("end_rod").is_some());
    }
}
