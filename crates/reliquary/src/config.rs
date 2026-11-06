//! `configs/config.kdl`: how the plugin behaves. Read once per use, so
//! `/relic reload` applies right away.

use std::sync::{Arc, RwLock};

use crate::items::TooltipConfig;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Mana of a player seen for the first time.
    pub start_mana: u32,
    pub mana_bar: bool,
    /// What players have before items, snake_case.
    pub player_stats: Vec<(String, f64)>,
    /// What mobs have before their gear. Crit power, so that crit chance on
    /// their gear does something.
    pub mob_stats: Vec<(String, f64)>,
    /// A kill counts for the last player who hit within this many seconds.
    pub kill_credit: f64,
    /// How far `/relic equip` reaches, in blocks.
    pub equip_range: f64,
    /// Seconds between two identical denial messages to one player.
    pub notice_delay: f64,
    pub tooltip: TooltipConfig,
    pub indicators: bool,
    /// How long a damage number stays, in ticks.
    pub indicator_lifetime: u32,
    /// How far a damage number floats up, in blocks.
    pub indicator_rise: f64,
    pub indicator_shadow: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start_mana: 100,
            mana_bar: true,
            player_stats: vec![("max_mana".into(), 100.0), ("mana_regen".into(), 5.0), ("crit_power".into(), 50.0)],
            mob_stats: vec![("crit_power".into(), 50.0)],
            kill_credit: 5.0,
            equip_range: 20.0,
            notice_delay: 1.0,
            tooltip: TooltipConfig::default(),
            indicators: true,
            indicator_lifetime: 20,
            indicator_rise: 0.8,
            indicator_shadow: true,
        }
    }
}

/// None until the first successful load.
static CONFIG: RwLock<Option<Arc<Config>>> = RwLock::new(None);

pub fn get() -> Arc<Config> {
    CONFIG.read().unwrap().clone().unwrap_or_default()
}

pub fn install(config: Config) {
    *CONFIG.write().unwrap() = Some(Arc::new(config));
}
