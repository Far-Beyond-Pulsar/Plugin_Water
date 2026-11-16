//! Water's host settings, described once (as `helio_settings` describes the
//! renderer's): engines build their settings UI from [`WATER`] and hand the
//! values to the feature as [`WaterSettings`] (a renderer config's
//! `features`).

use helio_feature_api::BuildEnv;
use helio_settings::{number, Group, Resolved};

/// The water's quality. A host lists it with its other settings groups.
pub static WATER: Group = Group {
    key: "water",
    title: "Water",
    description: "Water surfaces' quality",
    settings: &[number(
        "reflection_scale",
        "Reflection Resolution",
        "Water",
        (0.25, 1.0, 0.05),
        0.5,
        "The planar reflection's resolution, a fraction of the frame's internal resolution: \
         its capture renders the scene again at that size while water is in view",
    )],
};

/// Water's settings in a graph build ([`WATER`] describes them).
#[derive(Clone, Debug, PartialEq)]
pub struct WaterSettings {
    /// The reflection capture's size per axis, as a fraction of the frame's
    /// internal size.
    pub reflection_scale: f32,
}

impl Default for WaterSettings {
    fn default() -> Self {
        Self {
            reflection_scale: 0.5,
        }
    }
}

impl WaterSettings {
    /// The settings `settings` (of [`WATER`]) hold.
    pub fn from_settings(settings: &Resolved) -> Self {
        debug_assert!(std::ptr::eq(settings.group(), &WATER));
        Self {
            reflection_scale: (settings.number("reflection_scale") as f32).clamp(0.25, 1.0),
        }
    }
}

/// Water's settings in the graph being built.
pub fn water_settings(env: &BuildEnv<'_>) -> WaterSettings {
    env.setting::<WaterSettings>().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_defaults_are_the_default_settings() {
        assert_eq!(
            WaterSettings::from_settings(&WATER.defaults()),
            WaterSettings::default()
        );
    }
}
