//! Water (`pulsar.water`): the plugin's root crate, the one place the plugin
//! registers everything it contributes.
//!
//! The plugin's other crates hold the code: `helio-water-api` (the contracts
//! other kinds read), `helio-pass-water-sim` (the Helio feature: the
//! simulation, lighting, caustics and draw), `water_component` (the water
//! volume component, its rows and its row join) and `water_editor` (the
//! level editor's water entries). None of them registers a contribution;
//! this crate imports them and submits their registrations, so this file and
//! the two modules below are the whole list of what the plugin brings:
//!
//! - [`runtime`]: what a game links. The component class, the Helio feature,
//!   the scene rows and the animated content (water simulates on the frame
//!   clock).
//! - `editor` (with the `editor` feature): what the editor adds. The
//!   environment mixer's water volume (its create entry and listing).
//!
//! Every registration names this crate as its origin
//! (`pulsar_plugins::origin!()`), so the plugin gate skips all of them while
//! a project disables the plugin, and the plugin catalog lists them under
//! `pulsar.water`. Other plugins never depend on this crate (only on
//! `helio-water-api`); hosts reach it through the installed set or a
//! generated game, a game without the `editor` feature.

// The plugin's descriptor (`pulsar-plugin.toml`): linking this crate installs
// the plugin; a project enables it or not (`pulsar_plugins`).
pulsar_plugins::installed!("pulsar-plugin.toml");

pub use water_component;
#[cfg(feature = "editor")]
pub use water_editor;

pub mod runtime;

#[cfg(feature = "editor")]
pub mod editor;
