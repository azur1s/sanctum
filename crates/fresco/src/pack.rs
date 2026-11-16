//! Turns a folder of textures into a resource pack. No server calls in here.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;

use sha1::{Digest, Sha1};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::config::{Config, valid_part};

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

pub struct Pack {
    pub zip: Vec<u8>,
    /// Lowercase hex SHA-1 of `zip`.
    pub sha1: String,
    /// Sorted by id.
    pub models: Vec<Model>,
    /// Files that were skipped, and why.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// What goes into an item's `item_model`.
    pub id: String,
    /// The file it comes from, below the textures folder.
    pub file: String,
    pub pose: Pose,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pose {
    /// Held flat, like most items.
    Flat,
    /// Held like a sword or pickaxe.
    Handheld,
    /// Whatever the texture's own model file says.
    Own,
}

impl Pack {
    pub fn ids(&self) -> Vec<&str> {
        self.models.iter().map(|m| m.id.as_str()).collect()
    }

    /// The pack's id on the client. Follows the content, so a changed pack is a new pack.
    pub fn uuid(&self) -> (u64, u64) {
        let n = |at: usize| u64::from_str_radix(&self.sha1[at..at + 16], 16).unwrap_or(0);
        (n(0), n(16))
    }
}

/// Builds the pack from `textures`. The same files always give the same bytes,
/// so the hash only changes when the content does.
pub fn build(textures: &Path, config: &Config) -> Result<Pack, String> {
    let mut found = BTreeMap::new();
    let mut warnings = Vec::new();
    walk(textures, "", &mut found, &mut warnings)?;

    let ns = &config.namespace;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut models = Vec::new();

    for (path, bytes) in &found {
        if let Some(name) = path.strip_suffix(".png") {
            if !bytes.starts_with(PNG_MAGIC) {
                warnings.push(format!("{path}: not a PNG file, skipped"));
                continue;
            }
            files.insert(format!("assets/{ns}/textures/item/{path}"), bytes.clone());
            if !found.contains_key(&format!("{name}.json")) {
                let handheld = config.handheld.iter().any(|p| name.starts_with(p.as_str()));
                let (parent, pose) =
                    if handheld { ("minecraft:item/handheld", Pose::Handheld) } else { ("minecraft:item/generated", Pose::Flat) };
                let model = format!(r#"{{"parent":"{parent}","textures":{{"layer0":"{ns}:item/{name}"}}}}"#);
                files.insert(format!("assets/{ns}/models/item/{name}.json"), model.into_bytes());
                models.push((name.to_string(), path.clone(), pose));
            }
        } else if let Some(name) = path.strip_suffix(".json") {
            files.insert(format!("assets/{ns}/models/item/{path}"), bytes.clone());
            models.push((name.to_string(), path.clone(), Pose::Own));
        } else if path.ends_with(".png.mcmeta") {
            files.insert(format!("assets/{ns}/textures/item/{path}"), bytes.clone());
        } else {
            warnings.push(format!("{path}: not a .png, .png.mcmeta or .json file, skipped"));
        }
    }

    models.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, ..) in &models {
        // The dye tint lets an item's dyed_color recolor the texture; without
        // that component the default (white) leaves it untouched.
        let item = format!(
            r#"{{"model":{{"type":"minecraft:model","model":"{ns}:item/{name}","tints":[{{"type":"minecraft:dye","default":-1}}]}}}}"#
        );
        files.insert(format!("assets/{ns}/items/{name}.json"), item.into_bytes());
    }
    let mcmeta = format!(
        r#"{{"pack":{{"description":{},"min_format":{},"max_format":{}}}}}"#,
        json_string(&config.description),
        config.formats[0],
        config.formats[1]
    );
    files.insert("pack.mcmeta".into(), mcmeta.into_bytes());

    let zip = zip_up(&files)?;
    let sha1 = Sha1::digest(&zip).iter().map(|b| format!("{b:02x}")).collect();
    let models = models.into_iter().map(|(name, file, pose)| Model { id: format!("{ns}:{name}"), file, pose }).collect();
    Ok(Pack { zip, sha1, models, warnings })
}

/// Collects every file under `dir`, keyed by its `/`-separated path below the root.
fn walk(dir: &Path, prefix: &str, found: &mut BTreeMap<String, Vec<u8>>, warnings: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{prefix}{name}");
        if !valid_part(&name) {
            warnings.push(format!("{path}: names may only use a-z 0-9 _ - . (no capitals or spaces), skipped"));
            continue;
        }
        let full = entry.path();
        if full.is_dir() {
            walk(&full, &format!("{path}/"), found, warnings)?;
        } else {
            let bytes = std::fs::read(&full).map_err(|e| format!("{path}: {e}"))?;
            found.insert(path, bytes);
        }
    }
    Ok(())
}

fn zip_up(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in files {
        zip.start_file(path.as_str(), options).map_err(|e| format!("{path}: {e}"))?;
        zip.write_all(bytes).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(zip.finish().map_err(|e| e.to_string())?.into_inner())
}

fn json_string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn folder(name: &str, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("fresco-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, bytes) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, bytes).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn read(pack: &Pack, path: &str) -> Option<String> {
        let mut zip = zip::ZipArchive::new(Cursor::new(&pack.zip)).unwrap();
        let mut out = String::new();
        zip.by_name(path).ok()?.read_to_string(&mut out).ok()?;
        Some(out)
    }

    #[test]
    fn a_texture_becomes_a_model_id() {
        let root = folder("basic", &[("gem.png", PNG_MAGIC), ("weapons/blade.png", PNG_MAGIC)]);
        let config = Config { handheld: vec!["weapons/".into()], ..Config::default() };
        let pack = build(&root, &config).unwrap();

        assert_eq!(pack.ids(), ["sanctum:gem", "sanctum:weapons/blade"]);
        assert!(pack.warnings.is_empty());
        assert_eq!((pack.models[0].pose, pack.models[1].pose), (Pose::Flat, Pose::Handheld));
        assert_eq!(pack.models[1].file, "weapons/blade.png");
        assert!(read(&pack, "assets/sanctum/models/item/gem.json").unwrap().contains("item/generated"));
        let blade = read(&pack, "assets/sanctum/models/item/weapons/blade.json").unwrap();
        assert!(blade.contains("item/handheld") && blade.contains("sanctum:item/weapons/blade"));
        assert!(read(&pack, "assets/sanctum/items/weapons/blade.json").unwrap().contains("sanctum:item/weapons/blade"));
        assert!(read(&pack, "pack.mcmeta").unwrap().contains("\"min_format\":65"));
        assert_eq!(pack.sha1.len(), 40);
    }

    #[test]
    fn same_files_give_the_same_hash() {
        let root = folder("stable", &[("a.png", PNG_MAGIC), ("b.png", PNG_MAGIC)]);
        let (one, two) = (build(&root, &Config::default()).unwrap(), build(&root, &Config::default()).unwrap());
        assert_eq!(one.sha1, two.sha1);
        assert_eq!(one.uuid(), two.uuid());

        std::fs::write(root.join("c.png"), PNG_MAGIC).unwrap();
        assert_ne!(build(&root, &Config::default()).unwrap().sha1, one.sha1);
    }

    #[test]
    fn own_model_file_replaces_the_generated_one() {
        let own = br#"{"parent":"minecraft:item/handheld_rod","textures":{"layer0":"sanctum:item/staff"}}"#;
        let root = folder("own", &[("staff.png", PNG_MAGIC), ("staff.json", own), ("orb.json", b"{}")]);
        let pack = build(&root, &Config::default()).unwrap();

        assert_eq!(pack.ids(), ["sanctum:orb", "sanctum:staff"]);
        assert_eq!((pack.models[1].file.as_str(), pack.models[1].pose), ("staff.json", Pose::Own));
        assert!(read(&pack, "assets/sanctum/models/item/staff.json").unwrap().contains("handheld_rod"));
    }

    #[test]
    fn bad_files_are_skipped_with_a_warning() {
        let root = folder("bad", &[("Flame Sword.png", PNG_MAGIC), ("fake.png", b"nope"), ("notes.txt", b"hi"), ("ok.png", PNG_MAGIC)]);
        let pack = build(&root, &Config::default()).unwrap();

        assert_eq!(pack.ids(), ["sanctum:ok"]);
        assert_eq!(pack.warnings.len(), 3);
    }

    #[test]
    fn animation_files_ride_along() {
        let root = folder("anim", &[("fire.png", PNG_MAGIC), ("fire.png.mcmeta", b"{\"animation\":{}}")]);
        let pack = build(&root, &Config::default()).unwrap();
        assert!(read(&pack, "assets/sanctum/textures/item/fire.png.mcmeta").is_some());
        assert_eq!(pack.ids(), ["sanctum:fire"]);
    }
}
