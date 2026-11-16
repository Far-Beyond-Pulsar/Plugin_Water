//! The water surface's planar reflection: a mirrored capture view of the
//! frame (`helio_core::view`), clipped at the water's rest plane. The frame
//! composes the view from its own members and retains it; this owns the
//! capture's target, its mip chain and the reflected camera the surface
//! samples it through.

use bytemuck::Zeroable;
use glam::{Mat4, Vec3, Vec4};
use helio_core::GpuCameraUniforms;
use std::sync::Arc;

pub(super) const REFLECTION_RESOURCE: helio_core::ResourceKey<&'static wgpu::TextureView> =
    helio_core::ResourceKey::new("water_planar_reflection");

pub(super) struct PlanarReflectionCapture {
    device: Arc<wgpu::Device>,
    camera_buffer: wgpu::Buffer,
    hdr: wgpu::Texture,
    hdr_full_view: wgpu::TextureView,
    mip_views: Vec<wgpu::TextureView>,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    mip_sampler: wgpu::Sampler,
    mip_pipeline: wgpu::RenderPipeline,
    mip_layout: wgpu::BindGroupLayout,
    mip_bind_groups: Vec<wgpu::BindGroup>,
    width: u32,
    height: u32,
    reflected_camera: GpuCameraUniforms,
}

impl PlanarReflectionCapture {
    pub(super) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Water Reflected Camera"),
            size: 2 * std::mem::size_of::<GpuCameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mip_count = mip_count(width, height);
        let hdr = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water Planar Reflection HDR Mips"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let hdr_full_view = hdr.create_view(&wgpu::TextureViewDescriptor::default());
        let mip_views: Vec<wgpu::TextureView> = (0..mip_count)
            .map(|level| {
                hdr.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Water Planar Reflection Mip"),
                    format: Some(wgpu::TextureFormat::Rgba16Float),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    base_array_layer: 0,
                    array_layer_count: Some(1),
                    aspect: wgpu::TextureAspect::All,
                    usage: None,
                })
            })
            .collect();
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water Planar Reflection Depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            // Copied by the view's Hi-Z pyramid.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let mip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Water Planar Reflection Mip Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let mip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Reflection Mip BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mip_bind_groups = make_mip_bind_groups(device, &mip_layout, &mip_views, &mip_sampler);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Water Reflection Mip Shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
struct VertexOut { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) index: u32) -> VertexOut {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(p * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = p;
    return out;
}
@fragment fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(source_tex, source_sampler, in.uv);
}
"#
                .into(),
            ),
        });
        let mip_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Reflection Mip Pipeline Layout"),
            bind_group_layouts: &[Some(&mip_layout)],
            immediate_size: 0,
        });
        let mip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Water Reflection Mip Pipeline"),
            layout: Some(&mip_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            device: Arc::new(device.clone()),
            camera_buffer,
            hdr,
            hdr_full_view,
            mip_views,
            depth,
            depth_view,
            mip_sampler,
            mip_pipeline,
            mip_layout,
            mip_bind_groups,
            width,
            height,
            reflected_camera: GpuCameraUniforms::zeroed(),
        }
    }

    pub(super) fn reflection_view(&self) -> &wgpu::TextureView {
        &self.hdr_full_view
    }

    pub(super) fn camera_buffer(&self) -> &wgpu::Buffer {
        &self.camera_buffer
    }

    pub(super) fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.hdr = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water Planar Reflection HDR Mips"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count(width, height),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.hdr_full_view = self
            .hdr
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.mip_views = (0..mip_count(width, height))
            .map(|level| {
                self.hdr.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Water Planar Reflection Mip"),
                    format: Some(wgpu::TextureFormat::Rgba16Float),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    base_array_layer: 0,
                    array_layer_count: Some(1),
                    aspect: wgpu::TextureAspect::All,
                    usage: None,
                })
            })
            .collect();
        self.mip_bind_groups = make_mip_bind_groups(
            &self.device,
            &self.mip_layout,
            &self.mip_views,
            &self.mip_sampler,
        );
        self.depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water Planar Reflection Depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            // Copied by the view's Hi-Z pyramid.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.depth_view = self
            .depth
            .create_view(&wgpu::TextureViewDescriptor::default());
    }

    /// Renders the reflection: a mirrored capture view of the frame
    /// (`helio_core::view`) across the water's rest plane, clipped at it, into
    /// the mip chain's top level, then its mips. Every feature the frame
    /// places at the capture's slots and phases draws in it (meshes,
    /// foliage, decals, virtual geometry, the sky; voxel terrain builds
    /// nothing in a secondary view yet); none is named here.
    pub(super) fn render(
        &mut self,
        views: &mut helio_core::ViewContext<'_>,
        rest_y: f64,
    ) -> helio_core::Result<()> {
        let source = *views.camera_data;
        let plane_y = (rest_y - views.world_origin.y) as f32;
        // When the eye is below the authored rest plane the above-water
        // mirror is not a meaningful view; the surface falls back to its sky
        // and screen-space trace (`view_info` left clear marks it invalid).
        let above = source.position_near[1] > plane_y;
        let rendered = above && views.can_render() && {
            let source_view = Mat4::from_cols_array(&source.view);
            let reflection = Mat4::from_translation(Vec3::new(0.0, 2.0 * plane_y, 0.0))
                * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
            let view = source_view * reflection;
            let projection = Mat4::from_cols_array(&source.proj);
            let reflected_position =
                reflection.transform_point3(Vec3::from_slice(&source.position_near[..3]));
            let prev = Mat4::from_cols_array(&source.prev_view_proj) * reflection;
            let mut camera = GpuCameraUniforms::new(
                view,
                projection,
                reflected_position,
                source.position_near[3],
                source.forward_far[3],
                source.jitter_frame[2] as u32,
                [source.jitter_frame[0], source.jitter_frame[1]],
                prev,
            )
            // What lies under the water is not reflected in it.
            .with_clip_plane(Vec4::new(0.0, 1.0, 0.0, -plane_y));
            // The host's view identity, so per-view rows (fog, post) follow
            // the camera into its reflection.
            camera.jitter_frame[3] = source.jitter_frame[3];
            let target = &self.mip_views[0];
            let request = helio_core::ViewRequest {
                key: 0,
                kind: helio_core::ViewKind::Capture,
                mirrored: true,
                camera,
                content_to_drawn: Mat4::IDENTITY,
                width: self.width,
                height: self.height,
                target,
                depth: &self.depth_view,
                depth_texture: &self.depth,
                imports: &[],
                seeds: &[(helio_frame_api::color::SCENE_COLOR, target)],
            };
            let rendered = views.render_view(&request)?;
            if rendered {
                camera.view_info[1] = helio_core::view::VIEW_MIRRORED;
                self.reflected_camera = camera;
            }
            rendered
        };
        if !rendered {
            self.reflected_camera.view_info = [0; 4];
        }
        views.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[self.reflected_camera, self.reflected_camera]),
        );
        if rendered {
            self.generate_mips(views.encoder());
        }
        Ok(())
    }

    fn generate_mips(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.mip_views.len() <= 1 {
            return;
        }
        for (index, group) in self.mip_bind_groups.iter().enumerate() {
            let level = index + 1;
            let attachments = [Some(wgpu::RenderPassColorAttachment {
                view: &self.mip_views[level],
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Water Reflection Mip"),
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

fn make_mip_bind_groups(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    views: &[wgpu::TextureView],
    sampler: &wgpu::Sampler,
) -> Vec<wgpu::BindGroup> {
    (1..views.len())
        .map(|level| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Water Reflection Mip BG"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&views[level - 1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        })
        .collect()
}

fn mip_count(width: u32, height: u32) -> u32 {
    u32::BITS - width.max(height).leading_zeros()
}
