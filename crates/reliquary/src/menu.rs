//! `/relic` item browser: a chest menu with every item; click one to get it.
//!
//! Every button carries `reliquary:menu` custom data saying what it does, so
//! clicks are only handled (and cancelled) on our buttons, never in a normal chest.

use pumpkin_plugin_api::{
    ItemStack, ItemStackExt, Player, Screen, Server,
    events::{EventData, EventHandler, InventoryClickEvent},
    gui::Gui,
    item::Item,
    persistent_data::PersistentDataHolder,
};

use crate::engine::text;
use crate::game::{self, NS};
use crate::items::Instance;
use crate::texts::{messages, msg};

/// Item slots per page; the bottom row holds the buttons.
const PER_PAGE: usize = 45;
const KEY: &str = "menu";

fn button(item: Item, name: &str, action: &str) -> ItemStack {
    let stack = ItemStack::of(item, 1);
    stack.set_custom_name(Some(text(name).italic(false)));
    stack.set_string(NS, KEY, action);
    stack
}

pub fn open(player: &Player, page: usize) {
    let items = game::all_items();
    let pages = items.len().div_ceil(PER_PAGE).max(1);
    let page = page.min(pages - 1);

    let gui = Gui::new(Screen::Generic9x6, text(&msg(&messages::MENU_TITLE, &[("page", &(page + 1)), ("pages", &pages)])));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);

    let stats = crate::stats::current(player);
    for (slot, def) in items.iter().skip(page * PER_PAGE).take(PER_PAGE).enumerate() {
        let stack = game::build_stack(def, &Instance::default(), 1, &stats);
        stack.add_lore(text(""));
        stack.add_lore(text(&msg(&messages::MENU_CLICK_HINT, &[])).italic(false));
        stack.set_string(NS, KEY, &format!("give:{}", def.id));
        gui.set_item(slot as u32, stack);
    }
    for slot in 45..54 {
        gui.set_item(slot, button(Item::GrayStainedGlassPane, " ", "none"));
    }
    if page > 0 {
        gui.set_item(45, button(Item::Arrow, &msg(&messages::MENU_PREVIOUS, &[]), &format!("page:{}", page - 1)));
    }
    if page + 1 < pages {
        gui.set_item(53, button(Item::Arrow, &msg(&messages::MENU_NEXT, &[]), &format!("page:{}", page + 1)));
    }
    gui.set_item(49, button(Item::Book, &msg(&messages::MENU_COUNT, &[("count", &items.len())]), "none"));
    player.open_gui(gui);
}

pub struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _: Server, mut event: EventData<InventoryClickEvent>) -> EventData<InventoryClickEvent> {
        let Some(action) = event.clicked_item.as_ref().and_then(|s| s.get_string(NS, KEY)) else {
            return event;
        };
        event.cancelled = true;

        if let Some(id) = action.strip_prefix("give:") {
            match game::give(&event.player, id) {
                Ok(name) => event.player.send_system_message(text(&msg(&messages::MENU_GOT, &[("item", &name)])), false),
                Err(e) => event.player.send_system_message(text(&e), false),
            }
        } else if let Some(page) = action.strip_prefix("page:").and_then(|p| p.parse().ok()) {
            open(&event.player, page);
        }
        event
    }
}
