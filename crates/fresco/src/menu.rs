//! `/fresco` model browser: a chest menu with one item per model, drawn with
//! that model. Left click puts the id in chat, right click hands out a sample.
//!
//! Every button carries `fresco:menu` custom data saying what it does, so
//! clicks are only handled (and cancelled) on our buttons, never in a normal chest.

use pumpkin_plugin_api::{
    ItemStack, ItemStackExt, Player, Screen, Server,
    common::ClickType,
    data_components::DataComponent,
    events::{EventData, EventHandler, InventoryClickEvent},
    gui::Gui,
    item::Item,
    persistent_data::PersistentDataHolder,
    text::TextComponent,
};

use crate::pack::{Model, Pose};

/// Models per page; the bottom row holds the buttons.
const PER_PAGE: usize = 45;
const NS: &str = "fresco";
const KEY: &str = "menu";

/// Text with `&` color codes. Ids and file names cannot contain `&`.
fn text(s: &str) -> TextComponent {
    TextComponent::from_legacy_string_with_code(s, '&').italic(false)
}

/// A piece of paper that looks like the model.
fn sample(id: &str) -> ItemStack {
    let stack = ItemStack::of(Item::Paper, 1);
    stack.set_component(DataComponent::ItemModel, &protocol_string(id));
    stack.set_custom_name(Some(text(&format!("&f{id}"))));
    stack
}

/// A string as the game protocol writes it: length as a VarInt, then UTF-8.
fn protocol_string(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut len = s.len() as u32;
    loop {
        let byte = (len & 0x7f) as u8;
        len >>= 7;
        if len == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend_from_slice(s.as_bytes());
    out
}

fn entry(model: &Model) -> ItemStack {
    let pose = match model.pose {
        Pose::Flat => "flat item",
        Pose::Handheld => "held like a tool",
        Pose::Own => "own model file",
    };
    let stack = sample(&model.id);
    stack.add_lore(text(&format!("&7File: &ftextures/{}", model.file)));
    stack.add_lore(text(&format!("&7Model: &f{pose}")));
    stack.add_lore(text(""));
    stack.add_lore(text("&eLeft click &7id in chat, to copy"));
    stack.add_lore(text("&eRight click &7get a sample item"));
    stack.set_string(NS, KEY, &format!("model:{}", model.id));
    stack
}

fn button(item: Item, name: &str, lore: &[String], action: &str) -> ItemStack {
    let stack = ItemStack::of(item, 1);
    stack.set_custom_name(Some(text(name)));
    for line in lore {
        stack.add_lore(text(line));
    }
    stack.set_string(NS, KEY, action);
    stack
}

pub fn open(player: &Player, page: usize) {
    let live = crate::live();
    let models = live.as_ref().map_or(&[][..], |l| &l.models[..]);
    let pages = models.len().div_ceil(PER_PAGE).max(1);
    let page = page.min(pages - 1);

    let gui = Gui::new(Screen::Generic9x6, text(&format!("Fresco models ({}/{pages})", page + 1)));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);

    for (slot, model) in models.iter().skip(page * PER_PAGE).take(PER_PAGE).enumerate() {
        gui.set_item(slot as u32, entry(model));
    }
    for slot in 45..54 {
        gui.set_item(slot, button(Item::GrayStainedGlassPane, " ", &[], "none"));
    }
    if page > 0 {
        gui.set_item(45, button(Item::Arrow, "&fPrevious page", &[], &format!("page:{}", page - 1)));
    }
    if page + 1 < pages {
        gui.set_item(53, button(Item::Arrow, "&fNext page", &[], &format!("page:{}", page + 1)));
    }
    let info = match &live {
        Some(live) => vec![format!("&7Pack: &f{}", &live.sha1[..12]), format!("&7Size: &f{} KiB", live.bytes.len().div_ceil(1024))],
        None => vec!["&7Put .png files into the plugin's".into(), "&7textures/ folder.".into()],
    };
    gui.set_item(49, button(Item::Book, &format!("&f{} model(s)", models.len()), &info, "none"));
    player.open_gui(gui);
}

pub struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _: Server, mut event: EventData<InventoryClickEvent>) -> EventData<InventoryClickEvent> {
        let Some(action) = event.clicked_item.as_ref().and_then(|s| s.get_string(NS, KEY)) else {
            return event;
        };
        event.cancelled = true;
        let player = &event.player;

        if let Some(id) = action.strip_prefix("model:") {
            if matches!(event.click_type, ClickType::Right | ClickType::ShiftRight) {
                let inventory = player.get_inventory().as_inventory();
                match (0..36).find(|&slot| inventory.get_item(slot).is_none()) {
                    Some(slot) => inventory.set_item(slot, Some(sample(id))),
                    None => player.send_system_message(text("&cYour inventory is full."), false),
                }
            } else {
                let line = text(&format!("&f{id} &7(click to copy)"))
                    .click_copy_to_clipboard(id)
                    .hover_show_text(text("&7Copy the id"));
                player.send_system_message(line, false);
            }
        } else if let Some(page) = action.strip_prefix("page:").and_then(|p| p.parse().ok()) {
            open(player, page);
        }
        event
    }
}
