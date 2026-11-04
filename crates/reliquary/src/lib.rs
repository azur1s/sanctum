use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, permissions};
use tracing::*;

mod engine;
mod game;
mod indicators;
mod items;
mod mana;
mod menu;
mod stats;
mod triggers;
mod names;

struct ReliquaryPlugin;
impl Plugin for ReliquaryPlugin {
    fn new() -> Self {
        ReliquaryPlugin
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "reliquary".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Azur1s".into()],
            description: "A simple MMORPG (items) plugin".into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::FS_WRITE_DATA.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
        info!("Loaded Reliquary. Hello world!");
        game::register(&context)
    }

    fn on_unload(&self, _context: Context) -> pumpkin_plugin_api::Result<()> {
        game::save_mana();
        info!("Reliquary unloaded. Bye world!");
        Ok(())
    }
}

pumpkin_plugin_api::register_plugin!(ReliquaryPlugin);
