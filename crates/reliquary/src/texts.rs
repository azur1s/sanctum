//! Text that server owners can reword: chat messages, the lore's fixed parts and
//! damage numbers. Each file is a flat list of `key "text"`. The example file
//! embedded in the plugin is the baseline and the file on disk goes on top, so a
//! line missing from an older file keeps working.
//!
//! The keys below are the only place a key is spelled, so a typo in the code does
//! not compile and a typo in a file is a load error.

use std::collections::BTreeMap;
use std::fmt::{Display, Write};
use std::sync::{Arc, LazyLock, RwLock};

/// One line of a text file, and the `{placeholders}` it may use.
#[derive(Debug)]
pub struct Key {
    pub key: &'static str,
    pub placeholders: &'static [&'static str],
}

/// Values for a text's placeholders.
pub type Vars<'a> = &'a [(&'a str, &'a dyn Display)];

/// Fills in `{name}` parts. One pass: a value holding braces (an item name, a
/// formula) is not read again. Braces that are not in `vars` are left alone.
pub fn render(template: &str, vars: Vars) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let found = after.find('}').and_then(|close| {
            let value = vars.iter().find(|(name, _)| *name == &after[..close])?.1;
            Some((close, value))
        });
        match found {
            Some((close, value)) => {
                let _ = write!(out, "{value}");
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `{name}` parts of a template. Things like `{color:&a}` do not count.
pub fn placeholders(template: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        if let Some(close) = rest.find('}') {
            let name = &rest[..close];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
                names.push(name);
            }
        }
    }
    names
}

/// A loaded text file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sheet(Arc<BTreeMap<String, String>>);

impl Sheet {
    /// `Key` is only made below, so a missing text means the embedded file lost a
    /// line. The tests check for that; showing the key beats a blank message.
    pub fn get<'a>(&'a self, key: &'a Key) -> &'a str {
        self.0.get(key.key).map_or(key.key, String::as_str)
    }

    pub fn fill(&self, key: &Key, vars: Vars) -> String {
        render(self.get(key), vars)
    }
}

fn parse_sheet(path: &str, src: &str, keys: &[Key], base: Option<&Sheet>) -> Result<Sheet, String> {
    let lines = crate::items::parse_texts(path, src, keys).map_err(|e| e.to_string())?;
    let mut texts = base.map_or_else(BTreeMap::new, |b| (*b.0).clone());
    texts.extend(lines);
    Ok(Sheet(Arc::new(texts)))
}

macro_rules! sheet {
    ($(#[$doc:meta])* $module:ident, $path:literal, { $($name:ident = $key:literal $([$($var:literal),*])?),* $(,)? }) => {
        $(#[$doc])*
        pub mod $module {
            use super::*;

            pub const PATH: &str = $path;
            pub const DEFAULTS: &str = include_str!(concat!("../examples/", $path));
            $(pub const $name: Key = Key { key: $key, placeholders: &[$($($var),*)?] };)*
            pub const KEYS: &[Key] = &[$($name),*];

            pub fn defaults() -> Sheet {
                static SHEET: LazyLock<Sheet> =
                    LazyLock::new(|| parse_sheet(PATH, DEFAULTS, KEYS, None).expect("embedded text file is valid"));
                SHEET.clone()
            }

            /// The file's lines on top of the defaults.
            pub fn parse(src: &str) -> Result<Sheet, String> {
                parse_sheet(PATH, src, KEYS, Some(&defaults()))
            }
        }
    };
}

sheet! {
    /// Everything said to players.
    messages, "configs/messages.kdl", {
        MANA_BAR = "mana-bar" ["mana", "max"],
        MANA_BAR_INFINITE = "mana-bar-infinite",
        ON_COOLDOWN = "on-cooldown" ["seconds"],
        NOT_ENOUGH_MANA = "not-enough-mana",
        MENU_TITLE = "menu-title" ["page", "pages"],
        MENU_CLICK_HINT = "menu-click-hint",
        MENU_PREVIOUS = "menu-previous",
        MENU_NEXT = "menu-next",
        MENU_COUNT = "menu-count" ["count"],
        MENU_GOT = "menu-got" ["item"],
        ONLY_PLAYERS = "only-players",
        NO_PLAYER_FOUND = "no-player-found",
        NAME_A_PLAYER = "name-a-player",
        MISSING_ITEM_ID = "missing-item-id",
        UNKNOWN_ITEM = "unknown-item" ["id"],
        INVENTORY_FULL = "inventory-full",
        GAVE = "gave" ["item"],
        NO_RELIC = "no-relic" ["id"],
        LOOK_AT_MOB = "look-at-mob",
        EQUIPPED_MOB = "equipped-mob" ["item", "slot"],
        LIST_HEADER = "list-header" ["count"],
        LIST_LINE = "list-line" ["id", "item"],
        STATS_HEADER = "stats-header" ["player", "mana", "max"],
        STATS_LINE = "stats-line" ["stat", "value"],
        STATS_FROM = "stats-from",
        STATS_BASE = "stats-base" ["stats"],
        STATS_SOURCE = "stats-source" ["slot", "item", "stats"],
        STATS_NONE = "stats-none",
        MISSING_FORMULA = "missing-formula",
        FORMULA_ERROR = "formula-error" ["at", "error"],
        FORMULA_RESULT = "formula-result" ["formula", "value"],
        WHOLE_NUMBER = "whole-number",
        STATE_ON = "state-on",
        STATE_OFF = "state-off",
        ADMIN_NO_COOLDOWNS = "admin-no-cooldowns" ["state"],
        ADMIN_INFINITE_MANA = "admin-infinite-mana" ["state"],
        RELOADED = "reloaded" ["count"],
        RELOAD_FAILED = "reload-failed" ["count"],
        RELOAD_PROBLEM = "reload-problem" ["problem"],
        HOLD_ITEM = "hold-item",
        NOT_RELIQUARY_ITEM = "not-reliquary-item",
        HOLD_RELIC = "hold-relic",
        HOLD_ONE = "hold-one",
        CANT_REFORGE = "cant-reforge" ["item"],
        REFORGE_DOESNT_FIT = "reforge-doesnt-fit" ["id", "item", "fits"],
        NO_REFORGE_FITS = "no-reforge-fits" ["item"],
        REFORGED = "reforged" ["color", "reforge", "item"],
        REFORGES_HEADER = "reforges-header" ["count"],
        REFORGES_LINE = "reforges-line" ["mark", "id", "name", "weight", "stats"],
        REFORGES_FITS = "reforges-fits",
        REFORGES_OTHER = "reforges-other",
        NO_SOCKETS = "no-sockets" ["item"],
        HOLD_GEM = "hold-gem",
        SOCKET_TAKEN = "socket-taken" ["socket"],
        SOCKETS_FULL = "sockets-full" ["item"],
        SOCKETED = "socketed" ["gem", "socket"],
        NO_GEMS = "no-gems",
        SOCKET_EMPTY = "socket-empty" ["socket"],
        GEM_GONE = "gem-gone" ["id"],
        UNSOCKETED = "unsocketed" ["gem", "socket"],
        NOT_A_GEM = "not-a-gem" ["gem"],
        NO_SUCH_SOCKET = "no-such-socket" ["item", "socket"],
        WRONG_SOCKET = "wrong-socket" ["gem", "type", "socket", "takes"],
        GEM_STAT_MAIN_HAND = "gem-stat-main-hand" ["gem", "stat"],
        GEM_SKILL_MAIN_HAND = "gem-skill-main-hand" ["gem", "skill"],
        HOLD_UPGRADE = "hold-upgrade",
        UPGRADE_GONE = "upgrade-gone",
        UPGRADE_DOESNT_FIT = "upgrade-doesnt-fit" ["upgrade", "item"],
        UPGRADE_MAXED = "upgrade-maxed" ["item", "upgrade", "max"],
        UPGRADED = "upgraded" ["upgrade", "item", "count", "max"],
    }
}

sheet! {
    /// The parts of item lore that `stats_format.kdl` does not cover.
    lore, "formats/lore_format.kdl", {
        RARITY_KIND = "rarity-kind" ["color", "rarity", "kind"],
        RARITY = "rarity" ["color", "rarity"],
        LORE_STYLE = "lore-style",
        SKILL_HEADER = "skill-header" ["color", "name", "trigger"],
        SKILL_DESCRIPTION_STYLE = "skill-description-style",
        SKILL_DESCRIPTION_INDENT = "skill-description-indent",
        SKILL_COSTS = "skill-costs" ["costs"],
        COST_SEPARATOR = "cost-separator",
        COST_MANA = "cost-mana" ["amount"],
        COST_COOLDOWN = "cost-cooldown" ["seconds"],
        COST_CHANCE = "cost-chance" ["percent"],
        TRIGGER_RIGHT_CLICK = "trigger-right-click",
        TRIGGER_LEFT_CLICK = "trigger-left-click",
        TRIGGER_HIT = "trigger-hit",
        TRIGGER_DAMAGED = "trigger-damaged",
        TRIGGER_KILL = "trigger-kill",
        TRIGGER_SNEAK = "trigger-sneak",
        TRIGGER_TIMER = "trigger-timer" ["seconds"],
        TRIGGER_EQUIP = "trigger-equip",
        TRIGGER_UNEQUIP = "trigger-unequip",
        SOCKET_EMPTY_ANY = "socket-empty-any",
        SOCKET_EMPTY = "socket-empty" ["type"],
        SOCKET_FILLED = "socket-filled" ["gem"],
        SOCKET_INACTIVE = "socket-inactive" ["id"],
        GEM_FITS_ANY = "gem-fits-any",
        GEM_FITS = "gem-fits" ["type"],
        UPGRADE_PIP = "upgrade-pip",
        UPGRADE_BAR = "upgrade-bar" ["color", "name", "full", "empty", "fill", "count", "max"],
        UPGRADE_SHORT = "upgrade-short" ["color", "name", "fill", "count", "max"],
        UPGRADE_INACTIVE = "upgrade-inactive" ["id"],
        UPGRADE_ITEM_HINT = "upgrade-item-hint",
    }
}

sheet! {
    /// How floating damage numbers look.
    indicators, "formats/indicators.kdl", {
        NUMBER = "number" ["number_color", "icons", "color", "prefix", "number"],
        CRIT_COLOR = "crit-color",
        KIND_COLOR = "kind-color",
        NO_KIND_COLOR = "no-kind-color",
    }
}

/// None until the first successful load.
static MESSAGES: RwLock<Option<Sheet>> = RwLock::new(None);
static INDICATORS: RwLock<Option<Sheet>> = RwLock::new(None);

pub fn install(new_messages: Sheet, new_indicators: Sheet) {
    *MESSAGES.write().unwrap() = Some(new_messages);
    *INDICATORS.write().unwrap() = Some(new_indicators);
}

/// A message from `configs/messages.kdl`, filled in.
pub fn msg(key: &Key, vars: Vars) -> String {
    MESSAGES.read().unwrap().clone().unwrap_or_else(messages::defaults).fill(key, vars)
}

/// A line from `formats/indicators.kdl`, filled in.
pub fn indicator(key: &Key, vars: Vars) -> String {
    INDICATORS.read().unwrap().clone().unwrap_or_else(indicators::defaults).fill(key, vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_fills_once_and_leaves_the_rest() {
        let item = "{count} coins";
        assert_eq!(render("{item} x{count} {other} {", &[("item", &item), ("count", &3)]), "{count} coins x3 {other} {");
    }

    /// Every key has a default, and the defaults only use their own placeholders
    /// (parsing checks that), for all three embedded files.
    #[test]
    fn embedded_files_cover_every_key() {
        for (keys, sheet) in [
            (messages::KEYS, messages::defaults()),
            (lore::KEYS, lore::defaults()),
            (indicators::KEYS, indicators::defaults()),
        ] {
            for key in keys {
                assert!(sheet.0.contains_key(key.key), "no default for `{}`", key.key);
                for name in placeholders(sheet.get(key)) {
                    assert!(key.placeholders.contains(&name), "`{}` uses `{name}`", key.key);
                }
            }
        }
    }

    #[test]
    fn files_are_checked_and_fall_back_to_defaults() {
        let sheet = messages::parse("not-enough-mana \"&cOut of mana\"").unwrap();
        assert_eq!(sheet.get(&messages::NOT_ENOUGH_MANA), "&cOut of mana");
        assert_eq!(sheet.get(&messages::HOLD_GEM), "&cHold a gem in your offhand.");

        let e = messages::parse("not-enuf-mana \"x\"").unwrap_err();
        assert!(e.starts_with("configs/messages.kdl:1:1: unknown text `not-enuf-mana`"), "{e}");
        let e = messages::parse("on-cooldown \"{secs}\"").unwrap_err();
        assert!(e.contains("unknown placeholder `{secs}`") && e.contains("{seconds}"), "{e}");
        let e = messages::parse("gave \"a\"\ngave \"b\"").unwrap_err();
        assert!(e.contains("set twice"), "{e}");
    }
}
