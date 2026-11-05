//! Debug toggles for `/relic admin`. Server-wide and not saved: a restart
//! turns them all off.

use std::sync::atomic::{AtomicBool, Ordering};

static NO_COOLDOWN: AtomicBool = AtomicBool::new(false);
static INFINITE_MANA: AtomicBool = AtomicBool::new(false);

pub fn no_cooldown() -> bool {
    NO_COOLDOWN.load(Ordering::Relaxed)
}

pub fn infinite_mana() -> bool {
    INFINITE_MANA.load(Ordering::Relaxed)
}

/// Flips no-cooldown mode. Returns the new state.
pub fn toggle_no_cooldown() -> bool {
    !NO_COOLDOWN.fetch_not(Ordering::Relaxed)
}

/// Flips infinite mana. Returns the new state.
pub fn toggle_infinite_mana() -> bool {
    !INFINITE_MANA.fetch_not(Ordering::Relaxed)
}
