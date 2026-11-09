//! What the water plugin registers for the editor (the `editor` feature):
//! the environment mixer's water volume.

use ui_level_editor::extend::{CreateEntry, EnvironmentListing};
use water_editor::level_editor;

// ── Level editor ────────────────────────────────────────────────────────────

// The environment mixer's "Water Volume": an object holding a default volume.
inventory::submit! {
    CreateEntry {
        origin: pulsar_plugins::origin!(),
        id: level_editor::WATER_VOLUME_ENTRY,
        menu_label: None,
        environment_label: Some("Water Volume"),
        quick_add: None,
        offered: None,
        order: 450,
        create: level_editor::create_water_volume,
    }
}

// The mixer lists the level's water volumes and adds one through the entry
// above.
inventory::submit! {
    EnvironmentListing {
        origin: pulsar_plugins::origin!(),
        class_name: water_component::WATER_VOLUME_CLASS_NAME,
        label: "Water Volume",
        order: 50,
        create_entry: Some(level_editor::WATER_VOLUME_ENTRY),
    }
}
