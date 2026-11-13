//! Water's contribution to TSR's `transparency_reactivity` mask.
//!
//! The water surface is composited into `pre_aa` by [`crate::WaterSimPass`]
//! with animated shading and no motion vectors of its own, so the temporal
//! resolve cannot reproject it: it would blend last frame's waves (carried
//! along the velocity of the opaque floor beneath) into this frame's. This
//! pass tells the resolve where the water is so it drops that history, the
//! same hint transparent meshes give for their coverage.
//!
//! Reads the surface depth `WaterSimPass` publishes as
//! [`WATER_SURFACE_DEPTH`]; a pixel the surface draw wrote is water.
//!
//! A member only while the scene holds water, so a frame without water has
//! no mask from it (the resolve then does no coverage work). Where the
//! transparent drawers write the mask too (`color::ReactiveMask`) they clear
//! it first and this pass adds to it; otherwise this pass allocates and
//! clears it.

use helio_core::graph::{ResourceBuilder, ResourceSize};
use helio_core::{PassContext, RenderPass, ResourceKey, Result as HelioResult};

/// Graph key of the water surface's own depth ([`crate::WaterSimPass`] writes
/// it): non-zero (reverse-Z) where the surface drew this frame.
pub const WATER_SURFACE_DEPTH: &str = "water_surface_depth";

/// The frame's reactive mask, which the temporal resolve reads.
const MASK: &str = helio_frame_api::color::REACTIVE_MASK;

/// Raises `transparency_reactivity` where the water surface is.
pub struct WaterReactivityPass {
    /// The frame's first writer of the mask: allocates and clears it.
    first: bool,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    group: Option<(wgpu::TextureView, wgpu::BindGroup)>,
}

impl WaterReactivityPass {
    /// `first`: no earlier drawer of the frame writes the mask.
    pub fn new(device: &wgpu::Device, first: bool) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water reactivity"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = helio_core::shader::module_with(
            device,
            "Water reactivity",
            helio_core::include_wgsl!("../shaders/reactivity.wgsl"),
            &[],
        );
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water reactivity"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // Max, so a transparent mesh's higher coverage is never lowered.
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let pipeline = helio_core::shader::create_render_pipeline(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water reactivity"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::R8Unorm,
                        blend: Some(wgpu::BlendState {
                            color: max,
                            alpha: max,
                        }),
                        write_mask: wgpu::ColorWrites::RED,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            },
        );
        Self {
            first,
            pipeline,
            layout,
            group: None,
        }
    }
}

impl RenderPass for WaterReactivityPass {
    fn name(&self) -> &'static str {
        "WaterReactivity"
    }

    /// A member while the scene has water, like the simulation.
    fn is_member(&mut self, scene: &mut helio_core::SceneContent<'_>) -> bool {
        scene.holds_rows(crate::WATER_VOLUMES.name())
    }

    fn reads(&self) -> &'static [&'static str] {
        if self.first {
            &[]
        } else {
            &[MASK]
        }
    }

    fn writes(&self) -> &'static [&'static str] {
        &[MASK]
    }

    fn declare_resources(&self, builder: &mut ResourceBuilder) {
        if self.first {
            builder.write_color_raw(
                MASK,
                wgpu::TextureFormat::R8Unorm,
                ResourceSize::MatchSurface,
            );
        }
    }

    fn render_pass_descriptor_with_storage<'a>(
        &'a self,
        _target: &'a wgpu::TextureView,
        _depth: &'a wgpu::TextureView,
        resources: &'a helio_core::ResourceRegistry<'a>,
        storage: &'a mut helio_core::RenderFrameStorage,
    ) -> Option<wgpu::RenderPassDescriptor<'a>> {
        let mask = resources.texture_view(ResourceKey::new(MASK))?;
        let color_attachments: &'a [Option<wgpu::RenderPassColorAttachment<'a>>] = storage
            .retain_boxed_slice(Box::new([Some(wgpu::RenderPassColorAttachment {
                view: mask,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: if self.first {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })]));
        Some(wgpu::RenderPassDescriptor {
            label: Some("WaterReactivity"),
            color_attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    fn execute(&mut self, ctx: &mut PassContext) -> HelioResult<()> {
        let Some(depth) = ctx.registry.get(ResourceKey::new(WATER_SURFACE_DEPTH)) else {
            return Ok(());
        };
        let depth: &wgpu::TextureView = depth;
        if self.group.as_ref().is_none_or(|(key, _)| key != depth) {
            let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Water reactivity"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(depth),
                }],
            });
            self.group = Some((depth.clone(), group));
        }
        let Some((_, group)) = &self.group else {
            return Ok(());
        };
        let mut pass = ctx
            .render_cmds()
            .expect("the graph opened this pass's render pass");
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}
