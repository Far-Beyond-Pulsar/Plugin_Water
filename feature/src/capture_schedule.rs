//! When the water's reflection capture renders: only while some placed water
//! volume can be seen, so a frame without water in view pays nothing for it.
//!
//! The volumes are placed on the GPU (their owners' transforms, `scene_rows`),
//! so their bounds come back to the CPU by a non-blocking readback, two
//! frames late. Volumes rarely move; the test uses this frame's camera, so a
//! still volume is judged exactly the frame it enters the view. Until the
//! first rows come back every frame captures.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use glam::{DVec3, Vec3};
use helio_core::gpu_cache::{PooledBuffer, ReadbackPool};

use crate::{GpuWaterVolume, MAX_SIM_VOLUMES, WATER_VOLUMES};

const ROW_BYTES: u64 = std::mem::size_of::<GpuWaterVolume>() as u64;

/// A placed volume's world bounds.
#[derive(Clone, Copy, Debug)]
struct PlacedVolume {
    min: DVec3,
    max: DVec3,
    /// A lake or pool (clipped to its box); an ocean reaches the horizon.
    bounded: bool,
}

impl PlacedVolume {
    /// Whether it may show inside `planes` (frame space, `origin` its world
    /// position).
    fn seen(&self, planes: &[[f32; 4]; 6], origin: DVec3) -> bool {
        if !self.bounded {
            return true;
        }
        let min = (self.min - origin).as_vec3();
        let max = (self.max - origin).as_vec3();
        planes.iter().all(|plane| {
            let normal = Vec3::new(plane[0], plane[1], plane[2]);
            let farthest = Vec3::select(normal.cmpge(Vec3::ZERO), max, min);
            normal.dot(farthest) + plane[3] >= 0.0
        })
    }
}

/// A readback in flight: the buffer, whether it is mapped yet, whether it
/// was asked to map, and the frame origin its rows were placed at.
struct Pending {
    buffer: PooledBuffer,
    ready: Arc<AtomicBool>,
    mapping: bool,
    origin: DVec3,
}

/// The placed volumes' bounds, kept current by readback.
pub(super) struct WaterInView {
    readbacks: ReadbackPool,
    pending: Option<Pending>,
    volumes: Option<Vec<PlacedVolume>>,
}

impl WaterInView {
    pub(super) fn new() -> Self {
        Self {
            readbacks: ReadbackPool::new("Water Volume Bounds Readback"),
            pending: None,
            volumes: None,
        }
    }

    /// Whether some water volume may be seen from the requesting view, and
    /// the next readback of their bounds recorded into `views`.
    pub(super) fn visible(&mut self, views: &mut helio_core::ViewContext<'_>) -> bool {
        self.receive();
        let visible = match &self.volumes {
            None => true,
            Some(volumes) => {
                let planes = helio_core::depth::frustum_planes(views.camera_data.view_proj);
                volumes
                    .iter()
                    .any(|volume| volume.seen(&planes, views.world_origin))
            }
        };
        self.request(views);
        visible
    }

    fn receive(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        if !pending.mapping {
            // Its copy was submitted with the frame that recorded it.
            pending.buffer.map_read(Arc::clone(&pending.ready));
            pending.mapping = true;
            return;
        }
        if !pending.ready.load(Ordering::Acquire) {
            return;
        }
        let origin = pending.origin;
        let volumes = match pending.buffer.slice(..).get_mapped_range() {
            Ok(bytes) => {
                let whole = (bytes.len() as u64 / ROW_BYTES * ROW_BYTES) as usize;
                bytemuck::pod_collect_to_vec::<u8, GpuWaterVolume>(&bytes[..whole])
                    .iter()
                    .take(MAX_SIM_VOLUMES as usize)
                    .filter_map(|row| placed(row, origin))
                    .collect()
            }
            // Unreadable: keep capturing every frame until a readback reads.
            Err(_) => {
                pending.buffer.unmap();
                self.pending = None;
                return;
            }
        };
        pending.buffer.unmap();
        self.pending = None;
        self.volumes = Some(volumes);
    }

    fn request(&mut self, views: &mut helio_core::ViewContext<'_>) {
        if self.pending.is_some() {
            return;
        }
        let Some(rows) = views.scene_buffers.get(WATER_VOLUMES.key()) else {
            self.volumes = Some(Vec::new());
            return;
        };
        let size = (ROW_BYTES * u64::from(MAX_SIM_VOLUMES)).min(rows.buffer.size());
        if size == 0 {
            return;
        }
        let buffer = self.readbacks.get(views.device, size);
        views
            .encoder()
            .copy_buffer_to_buffer(&rows.buffer, 0, &buffer, 0, size);
        self.pending = Some(Pending {
            buffer,
            ready: Arc::new(AtomicBool::new(false)),
            mapping: false,
            origin: views.world_origin,
        });
    }
}

/// A placed row's world bounds; `None` for an empty row (no extent).
fn placed(row: &GpuWaterVolume, origin: DVec3) -> Option<PlacedVolume> {
    let min = DVec3::new(
        f64::from(row.bounds_min[0]),
        f64::from(row.bounds_min[1]),
        f64::from(row.bounds_min[2]),
    );
    let max = DVec3::new(
        f64::from(row.bounds_max[0]),
        f64::from(row.bounds_max[1]),
        f64::from(row.bounds_max[2]),
    );
    let extent = max - min;
    if !(extent.x > 1e-4 || extent.y > 1e-4 || extent.z > 1e-4) {
        return None;
    }
    Some(PlacedVolume {
        min: min + origin,
        max: max + origin,
        bounded: row.world_phase[2] >= 0.5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planes_looking_down_negative_z() -> [[f32; 4]; 6] {
        let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
        let proj = helio_core::depth::perspective(1.0, 1.0, 0.1);
        helio_core::depth::frustum_planes((proj * view).to_cols_array())
    }

    fn pool(center: DVec3) -> PlacedVolume {
        PlacedVolume {
            min: center - DVec3::splat(5.0),
            max: center + DVec3::splat(5.0),
            bounded: true,
        }
    }

    #[test]
    fn a_pool_counts_only_inside_the_frustum_and_an_ocean_always() {
        let planes = planes_looking_down_negative_z();
        let origin = DVec3::new(1.0e7, 0.0, 0.0);
        let ahead = pool(origin + DVec3::new(0.0, 0.0, -50.0));
        let behind = pool(origin + DVec3::new(0.0, 0.0, 50.0));
        assert!(ahead.seen(&planes, origin));
        assert!(!behind.seen(&planes, origin));
        let ocean = PlacedVolume {
            bounded: false,
            ..behind
        };
        assert!(ocean.seen(&planes, origin));
    }

    #[test]
    fn empty_rows_are_no_volume() {
        assert!(placed(&GpuWaterVolume::zeroed(), DVec3::ZERO).is_none());
    }

    use bytemuck::Zeroable;
}
