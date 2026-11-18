use helio_core::{CommandRecorder, PrepareContext};

use crate::{CASCADE_COUNT, INTERACTION_PATCH_SIZE, MAX_SIM_VOLUMES, MAX_WATER_HITBOXES, SIM_SIZE};

#[repr(C)]
#[derive(Default, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DropUniform {
    pub center: [f32; 2],
    pub radius: f32,
    pub strength: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SimUniform {
    pub spring: f32,
    pub damping: f32,
    pub hitbox_count: u32,
    pub _pad: u32,
    pub previous_shift: [f32; 4],
    pub drop: DropUniform,
}

// Two indirect dispatch records, for the active and retired slot lists.
const DISPATCH_BYTES: u64 = 3 * std::mem::size_of::<u32>() as u64;

struct InputBindings {
    volumes: wgpu::Buffer,
    hitboxes: wgpu::Buffer,
    classify: wgpu::BindGroup,
    // Source A -> destination B, then source B -> destination A.
    solve: [wgpu::BindGroup; 2],
}

pub(crate) struct InteractionInputs<'a> {
    pub textures: [&'a wgpu::TextureView; 2],
    pub volumes: &'a wgpu::Buffer,
    pub hitboxes: Option<&'a wgpu::Buffer>,
}

pub(crate) struct Interaction {
    classify: wgpu::ComputePipeline,
    clear_retired: wgpu::ComputePipeline,
    displace: wgpu::ComputePipeline,
    add_drop: wgpu::ComputePipeline,
    propagate: wgpu::ComputePipeline,
    normals: wgpu::ComputePipeline,
    publish: wgpu::ComputePipeline,
    solve_layout: wgpu::BindGroupLayout,
    args: wgpu::Buffer,
    slots: wgpu::Buffer,
    uniform: wgpu::Buffer,
    previous_hitboxes: wgpu::Buffer,
    bindings: Option<InputBindings>,
}

impl Interaction {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = helio_core::shader::module_with(
            device,
            "Water Interaction",
            helio_core::include_wgsl!("../shaders/interaction.wgsl"),
            &[crate::WATER_VOLUME_SNIPPET],
        );
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = |read_only| buffer(wgpu::BufferBindingType::Storage { read_only });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            ty,
            visibility: wgpu::ShaderStages::COMPUTE,
            count: None,
        };
        let solve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Interaction Layout"),
            entries: &[
                entry(1, storage(false)),
                entry(2, storage(true)),
                entry(
                    3,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                entry(
                    4,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
                entry(5, buffer(wgpu::BufferBindingType::Uniform)),
                entry(6, storage(true)),
                entry(7, storage(true)),
            ],
        });
        let solve_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Water Interaction Pipeline Layout"),
                bind_group_layouts: &[Some(&solve_layout)],
                immediate_size: 0,
            });
        let make_pipeline = |label, entry_point, layout, constants| {
            helio_core::shader::create_compute_pipeline(
                device,
                wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout,
                    module: &shader,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants,
                        ..Default::default()
                    },
                    cache: None,
                },
            )
        };
        let cascades = ("CASCADES", CASCADE_COUNT as f64);
        let size = ("SIM_SIZE", f64::from(SIM_SIZE));
        let patch_size = ("PATCH_SIZE", f64::from(INTERACTION_PATCH_SIZE));
        let make_buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let storage_usage = wgpu::BufferUsages::STORAGE;
        let copy_dst = wgpu::BufferUsages::COPY_DST;
        Self {
            classify: make_pipeline("Water Interaction Classify", "classify", None, &[size]),
            clear_retired: make_pipeline(
                "Water Interaction Clear Retired",
                "clear_retired",
                Some(&solve_pipeline_layout),
                &[cascades],
            ),
            displace: make_pipeline(
                "Water Interaction Hitboxes",
                "displace",
                Some(&solve_pipeline_layout),
                &[cascades, patch_size],
            ),
            add_drop: make_pipeline(
                "Water Interaction Drop",
                "add_drop",
                Some(&solve_pipeline_layout),
                &[cascades],
            ),
            propagate: make_pipeline(
                "Water Interaction Propagate",
                "propagate",
                Some(&solve_pipeline_layout),
                &[cascades],
            ),
            normals: make_pipeline(
                "Water Interaction Normals",
                "normals",
                Some(&solve_pipeline_layout),
                &[cascades],
            ),
            publish: make_pipeline(
                "Water Interaction Publish",
                "publish_state",
                Some(&solve_pipeline_layout),
                &[cascades],
            ),
            solve_layout,
            args: make_buffer(
                "Water Interaction Args",
                2 * DISPATCH_BYTES,
                storage_usage | wgpu::BufferUsages::INDIRECT,
            ),
            // The previous mask, padding for vec2u alignment, then slot pairs.
            slots: make_buffer(
                "Water Interaction Slots",
                8 * (1 + u64::from(MAX_SIM_VOLUMES)),
                storage_usage | copy_dst,
            ),
            uniform: make_buffer(
                "Water Interaction Uniform",
                std::mem::size_of::<SimUniform>() as u64,
                wgpu::BufferUsages::UNIFORM | copy_dst,
            ),
            previous_hitboxes: make_buffer(
                "Water Interaction Previous Hitboxes",
                u64::from(MAX_WATER_HITBOXES) * std::mem::size_of::<crate::GpuWaterHitbox>() as u64,
                storage_usage | copy_dst,
            ),
            bindings: None,
        }
    }

    pub(crate) fn prepare(&self, ctx: &PrepareContext, params: &SimUniform) {
        ctx.write_buffer(&self.uniform, 0, bytemuck::bytes_of(params));
    }

    pub(crate) fn invalidate_bindings(&mut self) {
        self.bindings = None;
    }

    pub(crate) fn reset(&self, encoder: &mut CommandRecorder<'_>) {
        encoder.clear_buffer(&self.slots, 0, None);
        encoder.clear_buffer(&self.previous_hitboxes, 0, None);
    }

    pub(crate) fn execute(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut CommandRecorder<'_>,
        inputs: InteractionInputs<'_>,
        drop_staged: bool,
        steps: u32,
    ) -> bool {
        let InteractionInputs {
            textures,
            volumes,
            hitboxes,
        } = inputs;
        let displace = hitboxes.is_some() && steps > 0;
        let hitboxes = hitboxes.unwrap_or(&self.previous_hitboxes);
        let changed = self
            .bindings
            .as_ref()
            .is_none_or(|cached| &cached.volumes != volumes || &cached.hitboxes != hitboxes);
        if changed {
            let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
            let classify = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Water Interaction Classify Bindings"),
                layout: &self.classify.get_bind_group_layout(0),
                entries: &[
                    entry(0, self.args.as_entire_binding()),
                    entry(1, self.slots.as_entire_binding()),
                    entry(2, volumes.as_entire_binding()),
                ],
            });
            let solve = std::array::from_fn(|source| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Water Interaction Solve Bindings"),
                    layout: &self.solve_layout,
                    entries: &[
                        entry(1, self.slots.as_entire_binding()),
                        entry(2, volumes.as_entire_binding()),
                        entry(3, wgpu::BindingResource::TextureView(textures[source])),
                        entry(4, wgpu::BindingResource::TextureView(textures[1 - source])),
                        entry(5, self.uniform.as_entire_binding()),
                        entry(6, hitboxes.as_entire_binding()),
                        entry(7, self.previous_hitboxes.as_entire_binding()),
                    ],
                })
            });
            self.bindings = Some(InputBindings {
                volumes: volumes.clone(),
                hitboxes: hitboxes.clone(),
                classify,
                solve,
            });
        }

        let bindings = self.bindings.as_ref().unwrap();
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Water Interaction"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.classify);
            pass.set_bind_group(0, &bindings.classify, &[]);
            pass.dispatch_workgroups(1, 1, 1);

            pass.set_pipeline(&self.clear_retired);
            pass.set_bind_group(0, &bindings.solve[1], &[]);
            pass.dispatch_workgroups_indirect(&self.args, DISPATCH_BYTES);

            let mut source = 0;
            let mut run = |pipeline: &wgpu::ComputePipeline| {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bindings.solve[source], &[]);
                pass.dispatch_workgroups_indirect(&self.args, 0);
                source = 1 - source;
            };
            if displace {
                run(&self.displace);
            }
            if drop_staged {
                run(&self.add_drop);
            }
            if steps > 0 {
                for _ in 0..steps {
                    run(&self.propagate);
                }
                run(&self.normals);
            }

            // Public diagnostics and downstream passes always read texture A.
            if source != 0 {
                pass.set_pipeline(&self.publish);
                pass.set_bind_group(0, &bindings.solve[source], &[]);
                pass.dispatch_workgroups_indirect(&self.args, 0);
            }
        }

        let copied_hitboxes = displace && hitboxes.usage().contains(wgpu::BufferUsages::COPY_SRC);
        if copied_hitboxes {
            let size = hitboxes.size().min(self.previous_hitboxes.size());
            encoder.copy_buffer_to_buffer(hitboxes, 0, &self.previous_hitboxes, 0, size);
        }
        copied_hitboxes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    #[test]
    fn gpu_interaction_updates_sparse_slots_and_clears_retired_layers() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(helio_core::wgpu_instance_descriptor());
            let Ok(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
            else {
                eprintln!("GPU_VALIDATION_SKIPPED_NO_ADAPTER: sparse water interaction");
                return;
            };
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    required_limits: adapter.limits(),
                    ..Default::default()
                })
                .await
                .expect("create a water interaction test device");

            let mut interaction = Interaction::new(&device);
            let textures = crate::pipeline::SimTextures::new(&device, SIM_SIZE);
            let mut rows = [crate::GpuWaterVolume::zeroed(); MAX_SIM_VOLUMES as usize];
            rows[1].bounds_max = [1.0; 4];
            rows[6].bounds_max = [1.0; 4];
            let volumes = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: std::mem::size_of_val(&rows) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uniform = SimUniform {
                spring: 1.0,
                damping: 0.95,
                hitbox_count: 0,
                _pad: 0,
                previous_shift: [0.0; 4],
                drop: DropUniform {
                    center: [0.0; 2],
                    radius: 0.2,
                    strength: 0.25,
                },
            };
            queue.write_buffer(&interaction.uniform, 0, bytemuck::bytes_of(&uniform));

            let mut sample = |rows: &[crate::GpuWaterVolume], drop, steps| {
                queue.write_buffer(&volumes, 0, bytemuck::cast_slice(rows));
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 24,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                interaction.execute(
                    &device,
                    &mut CommandRecorder::from_encoder(&mut encoder),
                    InteractionInputs {
                        textures: [&textures.array_a, &textures.array_b],
                        volumes: &volumes,
                        hitboxes: None,
                    },
                    drop,
                    steps,
                );
                for (index, slot) in [0, 1, 6].into_iter().enumerate() {
                    encoder.copy_texture_to_buffer(
                        wgpu::TexelCopyTextureInfo {
                            texture: &textures.tex_a,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: SIM_SIZE / 2,
                                y: SIM_SIZE / 2,
                                z: slot * CASCADE_COUNT as u32,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyBufferInfo {
                            buffer: &readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: (index * 8) as u64,
                                bytes_per_row: None,
                                rows_per_image: None,
                            },
                        },
                        wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                let submission = queue.submit([encoder.finish()]);
                let slice = readback.slice(..);
                let (send, receive) = std::sync::mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |result| {
                    send.send(result).unwrap()
                });
                device
                    .poll(wgpu::PollType::Wait {
                        submission_index: Some(submission),
                        timeout: None,
                    })
                    .unwrap();
                receive.recv().unwrap().unwrap();
                let bytes = slice.get_mapped_range().unwrap();
                std::array::from_fn::<_, 3, _>(|index| {
                    u16::from_le_bytes([bytes[index * 8], bytes[index * 8 + 1]])
                })
            };

            let launched = sample(&rows, true, 2);
            assert_eq!(launched[0], 0, "empty slot received a drop");
            assert_ne!(launched[1], 0, "live slot missed the drop");
            assert_eq!(launched[1], launched[2]);

            rows[1] = crate::GpuWaterVolume::zeroed();
            assert_eq!(sample(&rows, false, 0), [0, 0, launched[2]]);

            rows[1].bounds_max = [1.0; 4];
            assert_eq!(sample(&rows, false, 0), [0, 0, launched[2]]);
        });
    }
}
