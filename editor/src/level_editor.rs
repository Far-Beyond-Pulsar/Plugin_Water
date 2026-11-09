//! Water in the level editor's environment mixer: its water volumes, which
//! the plugin's root crate registers through the level editor's extension
//! points (`ui_level_editor::extend`). Each add is one undoable command; the
//! components are this plugin's.

use ui_level_editor::commands::{CommandResult, TypedComponent};
use ui_level_editor::extend::add_object;
use ui_level_editor::scene_edit::{ObjectType, Transform};
use ui_level_editor::state::LevelEditorState;

/// The create entry that adds a water volume (the environment mixer's
/// listing names it).
pub const WATER_VOLUME_ENTRY: &str = "environment_water_volume";

/// Adds an object holding a default water volume, as one undoable command.
pub fn create_water_volume(state: &mut LevelEditorState) -> CommandResult {
    add_object(
        state,
        "Water Volume",
        ObjectType::Empty,
        Transform::default(),
        vec![TypedComponent::new(
            water_component::WaterVolumeComponent::default(),
        )],
    )
}
