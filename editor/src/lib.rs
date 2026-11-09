//! Water (`pulsar.water`): the editor crate -- the level editor's water
//! entries ([`level_editor`]), over the plugin's component crate. It
//! registers nothing itself: the plugin's root crate (`water_plugin`, with
//! its `editor` feature) submits these through the level editor's extension
//! points. The inspector edits the components through their reflected
//! properties.

pub mod level_editor;
