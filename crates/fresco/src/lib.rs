//! Fresco: put a texture in `textures/`, get an item model id.
//!
//! The plugin builds a resource pack from the folder, serves it over HTTP and
//! sends it to players. It makes no items: the ids are for `item_model`.

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

use pumpkin_plugin_api::{
    Context, Player, Plugin, PluginMetadata, Server,
    command::{Command, CommandError, CommandNode, CommandSender, ConsumedArgs},
    commands::CommandHandler,
    events::{EventData, EventHandler, EventPriority, InventoryClickEvent, PlayerJoinEvent},
    permission::{Permission, PermissionDefault, PermissionLevel},
    permissions,
    player::JavaResourcePack,
    scheduler::SchedulerExt,
    text::TextComponent,
    uuid::Uuid,
};
use tracing::{info, warn};

mod config;
mod http;
mod menu;
mod pack;

use config::Config;

const PERMISSION: &str = "fresco:command.fresco";
const TEXTURES: &str = "textures";
/// Ticks between checks for changed files.
const WATCH_TICKS: u64 = 40;

static STATE: Mutex<Option<State>> = Mutex::new(None);

struct State {
    folder: String,
    config: Config,
    live: Option<Live>,
    /// What the files looked like at the last build attempt.
    fingerprint: u64,
    http: Option<(u16, http::Server)>,
    ticks: u64,
}

/// The pack players have right now.
#[derive(Clone)]
struct Live {
    bytes: Arc<Vec<u8>>,
    sha1: String,
    uuid: (u64, u64),
    models: Vec<pack::Model>,
}

/// What a rebuild changed, for telling players and whoever asked.
struct Rebuilt {
    config: Config,
    old: Option<Live>,
    new: Option<Live>,
    warnings: Vec<String>,
}

impl State {
    /// Reads the config and the textures again. On an error the old pack stays.
    fn rebuild(&mut self) -> Result<Rebuilt, String> {
        let root = Path::new(&self.folder);
        self.fingerprint = fingerprint(root);

        let text = std::fs::read_to_string(root.join(config::PATH)).map_err(|e| format!("{}: {e}", config::PATH))?;
        let config = Config::parse(&text).map_err(|e| format!("{}: {e}", config::PATH))?;
        let built = pack::build(&root.join(TEXTURES), &config)?;

        let new = if built.models.is_empty() {
            None
        } else {
            let write = |name: &str, bytes: &[u8]| std::fs::write(root.join(name), bytes).map_err(|e| format!("{name}: {e}"));
            write("pack.zip", &built.zip)?;
            write("pack.sha1", built.sha1.as_bytes())?;
            write("models.txt", (built.ids().join("\n") + "\n").as_bytes())?;
            Some(Live { uuid: built.uuid(), sha1: built.sha1, models: built.models, bytes: Arc::new(built.zip) })
        };

        let wanted = config.serve.enabled.then_some(config.serve.port);
        if self.http.as_ref().map(|(port, _)| *port) != wanted {
            self.http = None;
            if let Some(port) = wanted {
                match http::Server::bind(port) {
                    Ok(server) => self.http = Some((port, server)),
                    Err(e) => warn!("cannot serve the pack on port {port}: {e}"),
                }
            }
        }

        let old = std::mem::replace(&mut self.live, new.clone());
        self.config = config.clone();
        Ok(Rebuilt { config, old, new, warnings: built.warnings })
    }
}

/// Changes whenever a texture or the config is added, removed or edited.
fn fingerprint(root: &Path) -> u64 {
    fn visit(path: &Path, hasher: &mut impl Hasher) {
        let Ok(meta) = std::fs::metadata(path) else { return };
        path.hash(hasher);
        if meta.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path()).collect();
            entries.sort();
            for entry in entries {
                visit(&entry, hasher);
            }
        } else {
            meta.len().hash(hasher);
            meta.modified().ok().hash(hasher);
        }
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    visit(&root.join(config::PATH), &mut hasher);
    visit(&root.join(TEXTURES), &mut hasher);
    hasher.finish()
}

/// Rebuilds, then moves every online player to the new pack. The state lock is
/// released before any player is touched: host calls can re-enter the plugin.
fn rebuild_and_push(server: &Server) -> Result<Rebuilt, String> {
    let rebuilt = match STATE.lock().unwrap().as_mut() {
        Some(state) => state.rebuild()?,
        None => return Err("not loaded".into()),
    };
    for warning in &rebuilt.warnings {
        warn!("{warning}");
    }
    let (old, new) = (rebuilt.old.as_ref().map(|l| l.uuid), rebuilt.new.as_ref().map(|l| l.uuid));
    if old != new {
        match &rebuilt.new {
            Some(live) => info!("pack rebuilt: {} model(s), sha1 {}", live.models.len(), live.sha1),
            None => info!("no textures: players get no pack"),
        }
        for player in server.get_all_players() {
            push(&player, &rebuilt.config, rebuilt.new.as_ref(), rebuilt.old.as_ref());
        }
    }
    Ok(rebuilt)
}

/// Sends `new` to a player, taking `old` away first.
fn push(player: &Player, config: &Config, new: Option<&Live>, old: Option<&Live>) {
    let Some(java) = player.as_java() else { return };
    if let Some(old) = old {
        java.remove_resource_pack(Uuid { high: old.uuid.0, low: old.uuid.1 });
    }
    let Some(live) = new else { return };
    let url = if !config.serve.url.is_empty() {
        config.serve.url.clone()
    } else if config.serve.enabled {
        format!("http://{}:{}/{}.zip", host(&java.get_server_address()), config.serve.port, live.sha1)
    } else {
        return;
    };
    java.send_resource_pack(JavaResourcePack {
        id: Uuid { high: live.uuid.0, low: live.uuid.1 },
        url,
        hash: live.sha1.clone(),
        forced: config.force,
        prompt_message: (!config.prompt.is_empty()).then(|| TextComponent::text(&config.prompt)),
    });
}

/// The host part of the address a player typed to join, ready for a URL.
fn host(address: &str) -> String {
    // Modded clients append data after a NUL; DNS names may end in a dot.
    let address = address.split('\0').next().unwrap_or("").trim();
    let (name, port) = address.rsplit_once(':').unwrap_or((address, ""));
    let numeric = !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit());
    // `::1` has colons but no port; `[::1]:25565` and `host:25565` have one.
    let name = if numeric && (name.ends_with(']') || !name.contains(':')) { name } else { address };
    let name = name.trim_end_matches('.');
    match name {
        "" => "127.0.0.1".into(),
        name if name.contains(':') && !name.starts_with('[') => format!("[{name}]"),
        name => name.into(),
    }
}

fn say(sender: &CommandSender, line: &str) {
    sender.send_message(TextComponent::text(line));
}

struct List;
impl CommandHandler for List {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        let Some(live) = live() else {
            say(&sender, "No models yet: put .png files into the plugin's textures/ folder.");
            return Ok(0);
        };
        say(&sender, &format!("{} model(s), pack sha1 {}:", live.models.len(), live.sha1));
        for model in &live.models {
            say(&sender, &format!("  {}", model.id));
        }
        Ok(live.models.len() as i32)
    }
}

/// The pack players have right now, if there is one.
fn live() -> Option<Live> {
    STATE.lock().unwrap().as_ref().and_then(|s| s.live.clone())
}

/// `/fresco`: the picture menu for a player, the plain list for the console.
struct Browse;
impl CommandHandler for Browse {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        match sender.as_player() {
            Some(player) => {
                menu::open(&player, 0);
                Ok(1)
            }
            None => List.handle(sender, server, args),
        }
    }
}

struct Reload;
impl CommandHandler for Reload {
    fn handle(&self, sender: CommandSender, server: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        match rebuild_and_push(&server) {
            Ok(rebuilt) => {
                for warning in &rebuilt.warnings {
                    say(&sender, &format!("Skipped {warning}"));
                }
                let count = rebuilt.new.as_ref().map_or(0, |l| l.models.len());
                say(&sender, &format!("Fresco pack rebuilt: {count} model(s)."));
                Ok(1)
            }
            Err(problem) => Err(CommandError::CommandFailed(TextComponent::text(&format!("Fresco: {problem}")))),
        }
    }
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _: Server, event: EventData<PlayerJoinEvent>) -> EventData<PlayerJoinEvent> {
        let current = STATE.lock().unwrap().as_ref().map(|s| (s.config.clone(), s.live.clone()));
        if let Some((config, live)) = current {
            push(&event.player, &config, live.as_ref(), None);
        }
        event
    }
}

/// Every tick: move downloads along. Every `WATCH_TICKS`: rebuild if files changed.
fn tick(server: Server) {
    let changed = {
        let mut guard = STATE.lock().unwrap();
        let Some(state) = guard.as_mut() else { return };
        let bytes = state.live.as_ref().map(|l| Arc::clone(&l.bytes));
        if let Some((_, http)) = &mut state.http {
            http.poll(bytes.as_ref());
        }
        state.ticks += 1;
        state.config.watch && state.ticks % WATCH_TICKS == 0 && fingerprint(Path::new(&state.folder)) != state.fingerprint
    };
    if changed && let Err(problem) = rebuild_and_push(&server) {
        warn!("{problem} (keeping the previous pack)");
    }
}

struct FrescoPlugin;
impl Plugin for FrescoPlugin {
    fn new() -> Self {
        FrescoPlugin
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "fresco".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Azur1s".into()],
            description: "Textures in, item model ids out".into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::FS_WRITE_DATA.into(),
                permissions::NETWORK_TCP_BIND.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
        let folder = context.get_data_folder();
        let root = Path::new(&folder);
        std::fs::create_dir_all(root.join(TEXTURES)).map_err(|e| format!("{folder}: {e}"))?;
        if !root.join(config::PATH).exists() {
            std::fs::write(root.join(config::PATH), config::DEFAULT).map_err(|e| format!("{}: {e}", config::PATH))?;
        }

        let mut state = State { folder, config: Config::default(), live: None, fingerprint: 0, http: None, ticks: 0 };
        // A broken config or texture should not stop the plugin: fixing the file rebuilds.
        match state.rebuild() {
            Ok(rebuilt) => {
                for warning in &rebuilt.warnings {
                    warn!("{warning}");
                }
                match &rebuilt.new {
                    Some(live) => info!("{} model(s), pack sha1 {}", live.models.len(), live.sha1),
                    None => info!("no textures yet: put .png files into plugins/data/fresco/{TEXTURES}"),
                }
            }
            Err(problem) => warn!("{problem}"),
        }
        *STATE.lock().unwrap() = Some(state);

        // Not fatal: after a hot reload the permission is still registered.
        let _ = context.register_permission(&Permission {
            node: PERMISSION.into(),
            description: "Use /fresco".into(),
            default: PermissionDefault::Op(PermissionLevel::Two),
            children: vec![],
        });
        let command = Command::new(&["fresco".to_string()], "Fresco texture pack")
            .execute(Browse)
            .then(CommandNode::literal("menu").execute(Browse))
            .then(CommandNode::literal("list").execute(List))
            .then(CommandNode::literal("reload").execute(Reload));
        context.register_command(command, PERMISSION);
        context.register_event_handler::<PlayerJoinEvent, _>(OnJoin, EventPriority::Normal, false)?;
        context.register_event_handler::<InventoryClickEvent, _>(menu::OnClick, EventPriority::Normal, true)?;
        context.schedule_repeating_task(1, 1, tick);
        Ok(())
    }
}

pumpkin_plugin_api::register_plugin!(FrescoPlugin);

#[cfg(test)]
mod tests {
    use super::host;

    #[test]
    fn host_is_taken_from_the_join_address() {
        assert_eq!(host("play.example.com"), "play.example.com");
        assert_eq!(host("play.example.com:25565"), "play.example.com");
        assert_eq!(host("play.example.com.\0FML3\0"), "play.example.com");
        assert_eq!(host("192.168.1.5:25570"), "192.168.1.5");
        assert_eq!(host("::1"), "[::1]");
        assert_eq!(host("[::1]:25565"), "[::1]");
        assert_eq!(host(""), "127.0.0.1");
    }
}
