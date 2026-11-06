//! Where the plugin's files live, and moving them there from older versions.

use std::path::Path;

/// Written when missing, so every setting has a file to edit.
pub const DEFAULTS: [(&str, &str); 11] = [
    ("configs/config.kdl", include_str!("../examples/configs/config.kdl")),
    (crate::texts::messages::PATH, crate::texts::messages::DEFAULTS),
    ("configs/kinds.kdl", include_str!("../examples/configs/kinds.kdl")),
    ("configs/elements.kdl", include_str!("../examples/configs/elements.kdl")),
    ("configs/item_rarity.kdl", include_str!("../examples/configs/item_rarity.kdl")),
    ("configs/reforges.kdl", include_str!("../examples/configs/reforges.kdl")),
    ("configs/upgrades.kdl", include_str!("../examples/configs/upgrades.kdl")),
    ("formats/stats_format.kdl", include_str!("../examples/formats/stats_format.kdl")),
    ("formats/stats_layout.kdl", include_str!("../examples/formats/stats_layout.kdl")),
    (crate::texts::lore::PATH, crate::texts::lore::DEFAULTS),
    (crate::texts::indicators::PATH, crate::texts::indicators::DEFAULTS),
];

/// Player data. Not a setting, so it stays apart from the files people edit.
pub const MANA: &str = "data/mana.toml";

/// Files of older versions, which all sat next to `items/` and `skills/`.
const MOVED: [(&str, &str); 8] = [
    ("kinds.kdl", "configs/kinds.kdl"),
    ("elements.kdl", "configs/elements.kdl"),
    ("item_rarity.kdl", "configs/item_rarity.kdl"),
    ("reforges.kdl", "configs/reforges.kdl"),
    ("upgrades.kdl", "configs/upgrades.kdl"),
    ("stats_format.kdl", "formats/stats_format.kdl"),
    ("stats_layout.kdl", "formats/stats_layout.kdl"),
    ("mana.toml", MANA),
];

/// Moves files from where older versions kept them, and says what it did. Runs
/// before defaults are written: otherwise a fresh default would take the new path
/// and an edited file would be silently ignored. Never overwrites.
pub fn migrate(root: &Path) -> Result<Vec<String>, String> {
    let mut log = Vec::new();
    for (old, new) in MOVED {
        let (from, to) = (root.join(old), root.join(new));
        if !from.exists() {
            continue;
        }
        if to.exists() {
            log.push(format!("both {old} and {new} exist: using {new}, {old} is ignored"));
            continue;
        }
        move_file(&from, &to).map_err(|e| format!("could not move {old} to {new}: {e}"))?;
        log.push(format!("moved {old} to {new}"));
    }
    Ok(log)
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // A failed copy must not leave half a file for the next load to trust.
    if let Err(e) = std::fs::copy(from, to) {
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    std::fs::remove_file(from)
}

/// Writes every default file that does not exist yet.
pub fn write_defaults(root: &Path) -> Result<(), String> {
    for (path, contents) in DEFAULTS {
        let file = root.join(path);
        if file.exists() {
            continue;
        }
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&file, contents).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// An empty folder of its own, for one test.
    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("reliquary-{}-{name}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_old_files_and_keeps_edits() {
        let root = temp_dir("move");
        std::fs::write(root.join("stats_layout.kdl"), "// edited").unwrap();
        std::fs::write(root.join("mana.toml"), "x = 1").unwrap();

        let log = migrate(&root).unwrap();
        assert_eq!(log.len(), 2, "{log:?}");
        write_defaults(&root).unwrap();

        assert!(!root.join("stats_layout.kdl").exists());
        assert_eq!(std::fs::read_to_string(root.join("formats/stats_layout.kdl")).unwrap(), "// edited");
        assert_eq!(std::fs::read_to_string(root.join(MANA)).unwrap(), "x = 1");
        assert!(root.join("configs/config.kdl").exists());
        // Nothing left to do the second time.
        assert!(migrate(&root).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn never_overwrites_the_new_file() {
        let root = temp_dir("keep");
        std::fs::create_dir_all(root.join("configs")).unwrap();
        std::fs::write(root.join("kinds.kdl"), "// old").unwrap();
        std::fs::write(root.join("configs/kinds.kdl"), "// new").unwrap();

        let log = migrate(&root).unwrap();
        assert!(log[0].contains("both"), "{log:?}");
        assert_eq!(std::fs::read_to_string(root.join("configs/kinds.kdl")).unwrap(), "// new");
        assert_eq!(std::fs::read_to_string(root.join("kinds.kdl")).unwrap(), "// old");
        std::fs::remove_dir_all(root).unwrap();
    }
}
