//! Water (`pulsar.water`): the component crate of the plugin that owns
//! simulated water.
//!
//! - The component ([`component`]): [`WaterVolumeComponent`], a water body
//!   centred on its owner.
//! - Its row ([`WaterVolumeSourceRow`]): the water volume row SceneDB
//!   derives from the authored value; the plugin's row join places it at
//!   the owner, writes the lights' directional light as its sun and the
//!   level's wind over it, and bounds the bodies physics marks as pushing
//!   water (`helio_pass_water_sim::scene_rows`).
//! - What the engine runs for them, each through a generic registration
//!   (the engine names no water type): the plugin's Helio feature
//!   ([`render_features`]: `helio_pass_water_sim::WaterFeature`, the
//!   simulation, the surface, and the caustics published as the frame's
//!   transmission), its scene rows ([`register_columns`], [`derivation`])
//!   and its animated content ([`watch_water`], [`water_live`]: water
//!   simulates on the frame clock, so a realtime viewport keeps drawing
//!   while a volume is enabled).
//!
//! This crate registers nothing itself: the plugin's root crate
//! (`water_plugin`) submits every registration above, in one place.

pub mod component;

pub use component::*;

use pulsar_scenedb::gpu::{GpuMirrorHandle, SceneGpuStore};
use pulsar_scenedb_derive::SceneStore;

/// Where the volumes' source rows live.
pub const WATER_VOLUME_SOURCES_BUFFER: &str = "water_volume_sources";

/// A water volume: its local size (`xyz`, full extent before the owner's
/// scale; `w`, the surface height above the owner's position), then the
/// water volume row after its bounds: waves (wave params, direction,
/// colour, extinction), optics (reflection and refraction, caustics, fog,
/// surface params), shading (shadow, sun direction, SSR, simulation
/// dynamics), wind and a reserved vec4. A disabled component has a zero
/// size, which the join skips.
#[derive(SceneStore, bytemuck::Pod, bytemuck::Zeroable, Clone, Copy, Debug, PartialEq)]
#[repr(C)]
#[gpu(layout = packed, buffer = "water_volume_sources")]
pub struct WaterVolumeSourceRow {
    #[gpu]
    pub size: [f32; 4],
    #[gpu]
    pub waves: [f32; 16],
    #[gpu]
    pub optics: [f32; 16],
    #[gpu]
    pub shading: [f32; 16],
    #[gpu]
    pub wind: [f32; 4],
    #[gpu]
    pub reserved: [f32; 4],
}

impl WaterVolumeSourceRow {
    pub fn of(water: &WaterVolumeComponent) -> Self {
        if !water.enabled {
            return bytemuck::Zeroable::zeroed();
        }
        let size = [
            water.size[0],
            water.size[1],
            water.size[2],
            water.surface_height_offset,
        ];
        let gpu = water.local_gpu();
        // The pass row after its two bound vec4s.
        let after_bounds = &bytemuck::bytes_of(&gpu)[32..];
        bytemuck::pod_read_unaligned(&[bytemuck::bytes_of(&size), after_bounds].concat())
    }
}

fn water_volume_dispatch(mirror: &GpuMirrorHandle, row: u32, data: *const (), is_new_insert: bool) {
    // SAFETY: SceneDB reaches this only through `WaterVolumeComponent`'s own
    // `ComponentId`, with a pointer to a live value.
    let water = unsafe { &*(data as *const WaterVolumeComponent) };
    pulsar_scenedb::gpu::write_derived_row(
        mirror,
        row,
        &WaterVolumeSourceRow::of(water),
        is_new_insert,
    );
}

fn water_volume_clear(mirror: &GpuMirrorHandle, row: u32) {
    pulsar_scenedb::gpu::clear_derived_row::<WaterVolumeSourceRow>(mirror, row);
}

pulsar_scenedb::pulsar_reflection::inventory::submit! {
    pulsar_scenedb::gpu::GpuMirrorRegistration {
        component_id: pulsar_scenedb::component_id::<WaterVolumeComponent>,
        dispatch: water_volume_dispatch,
    }
}

pulsar_scenedb::pulsar_reflection::inventory::submit! {
    pulsar_scenedb::gpu::GpuClearRegistration {
        component_id: pulsar_scenedb::component_id::<WaterVolumeComponent>,
        clear: water_volume_clear,
    }
}

/// Registers the volumes' source rows (the scene rows' columns).
pub fn register_columns(store: &mut SceneGpuStore, device: &std::sync::Arc<wgpu::Device>) {
    WaterVolumeSourceRow::register_gpu_columns_growable(store, 8, device);
}

/// The plugin's row join: places the volumes, writes their sun and wind
/// and bounds the water hitboxes (`helio_pass_water_sim::scene_rows`).
pub fn derivation(
    device: &wgpu::Device,
    keys: helio_scene_join::RowJoinKeys,
) -> Box<dyn helio_scene_join::SceneDerivation> {
    let key = helio_scene_join::BufferKey::of;
    Box::new(helio_pass_water_sim::scene_rows::scene_rows(
        device,
        keys,
        helio_pass_water_sim::WaterRowSources {
            water_volumes: key(WATER_VOLUME_SOURCES_BUFFER),
            wind: key(helio_foliage_api::WIND_SOURCES_BUFFER),
            water_hitboxes: key(helio_water_api::WATER_HITBOX_SOURCES_BUFFER),
        },
    ))
}

/// Water simulates on the frame clock: its waves travel in a wind, and
/// ripples from bodies moving through it (#1080) spread and settle over many
/// frames after the edit that made them. Any enabled volume keeps a
/// realtime viewport drawing.
pub fn water_live(world: &pulsar_scenedb::World) -> bool {
    world
        .query::<&WaterVolumeComponent>()
        .any(|(instance, water)| {
            water.enabled && pulsar_scene_model::attachments::is_enabled(world, instance)
        })
}

/// The changes that can change [`water_live`]'s answer.
pub fn watch_water(world: &pulsar_scenedb::World) -> pulsar_scenedb::ChangeCursor {
    world.open_change_cursor::<WaterVolumeComponent>()
}

/// Water's feature: the simulation, its draw and its transmission.
pub fn render_features() -> helio_feature_api::FeatureSet {
    helio_feature_api::FeatureSet::new().with(helio_pass_water_sim::WaterFeature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_have_the_documented_word_counts() {
        assert_eq!(std::mem::size_of::<WaterVolumeSourceRow>(), 60 * 4);
        assert_eq!(
            std::mem::size_of::<helio_pass_water_sim::WaterVolumeRow>(),
            64 * 4
        );
        assert_eq!(
            std::mem::size_of::<WaterVolumeSourceRow>() as u64,
            helio_pass_water_sim::scene_rows::WATER_VOLUME_SOURCE_ROW_BYTES
        );
    }

    #[test]
    fn a_disabled_volume_derives_an_inert_row() {
        let water = WaterVolumeComponent {
            enabled: false,
            ..Default::default()
        };
        let row = WaterVolumeSourceRow::of(&water);
        assert!(bytemuck::bytes_of(&row).iter().all(|byte| *byte == 0));
    }

    #[test]
    fn a_water_row_is_the_pass_row_after_its_bounds() {
        let water = WaterVolumeComponent {
            surface_height_offset: 2.5,
            ..Default::default()
        };
        let row = WaterVolumeSourceRow::of(&water);
        assert_eq!(
            &bytemuck::bytes_of(&row)[16..],
            &bytemuck::bytes_of(&water.local_gpu())[32..]
        );
        assert_eq!(row.size, [200.0, 60.0, 200.0, 2.5]);
    }
}
