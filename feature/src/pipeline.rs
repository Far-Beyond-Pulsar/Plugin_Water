use crate::{
    make_caustics_grid, make_top_grid, vec4_vbl, WaterSimPass, BLIT_WGSL, CASCADE_COUNT,
    MAX_SIM_VOLUMES,
};
use bytemuck::Zeroable;
use wgpu::util::DeviceExt;

pub const WATER_SHORE_SNIPPET: helio_core::shader::ShaderSnippet = helio_core::wgsl_snippet!(
    "//!use water_shore_surface",
    "../shaders/water_surface_shared.wgsl"
);

/// The sky as the water sees it (`water_sky.wgsl`): one colour for the surface
/// underside and the underwater medium.
pub const WATER_SKY_SNIPPET: helio_core::shader::ShaderSnippet =
    helio_core::wgsl_snippet!("//!use water_sky", "../shaders/water_sky.wgsl");

pub const WATER_LIGHTS_SNIPPET: helio_core::shader::ShaderSnippet =
    helio_core::wgsl_snippet!("//!use water_lights", "../shaders/water_lights.wgsl");

fn storage_read_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_buffer_entry(binding: u32, min_size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(min_size),
        },
        count: None,
    }
}

fn texture_entry(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
    dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32, kind: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(kind),
        count: None,
    }
}

/// The simulation's height fields: two ping-pong arrays of one layer per
/// volume row and cascade, `size` square.
pub(crate) struct SimTextures {
    pub(crate) tex_a: wgpu::Texture,
    pub(crate) array_a: wgpu::TextureView,
    pub(crate) array_b: wgpu::TextureView,
    pub(crate) layers_a: Vec<wgpu::TextureView>,
}

impl SimTextures {
    pub(crate) fn new(device: &wgpu::Device, size: u32) -> Self {
        let total_layers = CASCADE_COUNT as u32 * MAX_SIM_VOLUMES;
        let make_array_tex = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: total_layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let tex_a = make_array_tex("WaterSim Tex A");
        let tex_b = make_array_tex("WaterSim Tex B");
        let layers = |tex: &wgpu::Texture, side: &str| -> Vec<wgpu::TextureView> {
            (0..total_layers)
                .map(|layer| {
                    tex.create_view(&wgpu::TextureViewDescriptor {
                        label: Some(&format!("WaterSim Layer {side} {layer}")),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_array_layer: layer,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect()
        };
        let array = |tex: &wgpu::Texture, label| {
            tex.create_view(&wgpu::TextureViewDescriptor {
                label: Some(label),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                array_layer_count: Some(total_layers),
                ..Default::default()
            })
        };
        Self {
            layers_a: layers(&tex_a, "A"),
            array_a: array(&tex_a, "WaterSim Array View A"),
            array_b: array(&tex_b, "WaterSim Array View B"),
            tex_a,
        }
    }
}

/// The underwater tint's scratch target at the internal resolution.
pub(crate) fn tint_scratch(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Water Tint Scratch"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// The water surface's own depth buffer. The scene's depth holds only opaque
/// geometry, and the surface is drawn without writing it, so overlapping
/// swells (a near crest over a far one at a grazing view) painted in draw
/// order. Water tests and writes this one; occlusion by the opaque scene is
/// the shader's own depth-copy test.
pub(crate) fn water_depth_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Water Surface Depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        // Sampled by `WaterReactivityPass` to find where the surface drew.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

pub(crate) fn shafts_scratch(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Half Resolution Water Shafts"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// The caustics projection: the refracted-sun grid that lands the ocean
/// slopes' light on the caustics slices (`shaders/caustics.wgsl`): its
/// bind group layout (water volumes, ocean slopes, sampler, slice
/// parameters) and its additive, 4x MSAA `Rgba16Float` pipeline. The pass
/// draws with it; the caustics test drives the same one.
pub struct CausticsProjection {
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub pipeline: helio_core::shader::DeferredRender,
}

impl CausticsProjection {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("WaterCaustics Render BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let caustics_pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Caustics PL"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let caustics_shader = helio_core::shader::module_with(
            device,
            "Water Caustics Shader",
            helio_core::include_wgsl!("../shaders/caustics.wgsl"),
            &[crate::WATER_VOLUME_SNIPPET],
        );
        let vbl = vec4_vbl();
        let caustics_pipeline = helio_core::shader::create_render_pipeline_deferred(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Caustics Pipeline"),
                layout: Some(&caustics_pl_layout),
                vertex: wgpu::VertexState {
                    module: &caustics_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(vbl.clone())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &caustics_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::OVER,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 4,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            },
        );
        Self {
            bind_group_layout,
            pipeline: caustics_pipeline,
        }
    }
}

impl WaterSimPass {
    #[allow(clippy::too_many_arguments)]
    /// The pass with the host's water settings ([`crate::WaterSettings`]).
    pub fn with_settings(mut self, settings: &crate::WaterSettings) -> Self {
        self.reflection_scale = settings.reflection_scale.clamp(0.25, 1.0);
        self
    }

    pub fn new(
        device: &wgpu::Device,
        _camera_buf: &wgpu::Buffer,
        internal_width: u32,
        internal_height: u32,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        // Placeholders until water exists (see `allocate_content`).
        let SimTextures {
            tex_a: sim_tex_a,
            array_a: sim_array_view_a,
            array_b: sim_array_view_b,
            layers_a: sim_layer_views_a,
        } = SimTextures::new(device, 1);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("WaterSim Internal Sampler"),
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let output_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("WaterSim Output Sampler"),
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });
        let depth_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Water Depth Sampler"),
            min_filter: wgpu::FilterMode::Nearest,
            mag_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let render_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Render BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // hiz_nearest — R32Float with a mip chain, read via textureLoad, so
                // it is unfilterable and needs no sampler.
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 13,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 14,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 15,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 16,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 17,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 18,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 19,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(
                    20,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                texture_entry(
                    23,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 24,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // The resolved atmosphere and its sky table: the sea reflects
                // and is lit by the real sky, as Ultra-Water does with its cube.
                wgpu::BindGroupLayoutEntry {
                    binding: 25,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            helio_frame_api::lighting::sky::SKY_FRAME_BYTES,
                        ),
                    },
                    count: None,
                },
                // The sky-view LUT: the sky's radiance by direction, the same
                // table the sky is drawn from.
                texture_entry(
                    27,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                storage_read_entry(28),
            ],
        });
        let fallback_sky_view_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water No Sky View"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let fallback_sky_view =
            fallback_sky_view_tex.create_view(&wgpu::TextureViewDescriptor::default());
        // Bound until an atmosphere publishes (planet.w = 0: no sky).
        let fallback_lights = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Water No Local Lights"),
            size: std::mem::size_of::<helio_light_api::GpuLight>() as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let fallback_atmosphere = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Water No Atmosphere"),
            size: helio_frame_api::lighting::sky::SKY_FRAME_BYTES,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });

        let caustics_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Water Caustics Sampler"),
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        let foam_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Baked Water Foam Sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let foam_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Baked Water Foam Material"),
            size: wgpu::Extent3d {
                width: crate::FOAM_TEXTURE_SIZE,
                height: crate::FOAM_TEXTURE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: crate::FOAM_MIP_LEVELS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let foam_view = foam_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Water Foam Full Mip Chain"),
            ..Default::default()
        });
        let foam_mip_views = (0..crate::FOAM_MIP_LEVELS)
            .map(|mip| {
                foam_texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Water Foam Mip"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_mip_level: mip,
                    mip_level_count: Some(1),
                    base_array_layer: 0,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();

        let pre_aa_fallback_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water PreAA Fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let pre_aa_fallback_view =
            pre_aa_fallback_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let gbuffer_fallback_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water GBuffer Fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let gbuffer_fallback_view =
            gbuffer_fallback_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Blit BGL"),
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
        let blit_shader = helio_core::shader::module(device, "Water Blit Shader", BLIT_WGSL);
        let blit_pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Blit PL"),
            bind_group_layouts: &[Some(&blit_bgl)],
            immediate_size: 0,
        });
        let blit_pipeline = helio_core::shader::create_render_pipeline_deferred(
            &device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Blit Pipeline"),
                layout: Some(&blit_pl_layout),
                vertex: wgpu::VertexState {
                    module: &blit_shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &blit_shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        let (tint_scratch_tex, tint_scratch_view) = tint_scratch(device, 1, 1, surface_format);
        let (water_depth_tex, water_depth_view) = water_depth_target(device, 1, 1);
        let (shafts_scratch_tex, shafts_scratch_view) = shafts_scratch(device, 1, 1);

        let underwater_tint_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Water Underwater Tint BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Scene depth — the underwater medium is integrated per
                    // pixel against the distance the view ray travels.
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                    // Heightfield — the containment test runs against the
                    // displaced surface, not the flat plane.
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 7,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Global FFT displacement and per-cascade f64-reduced
                    // phase, matching the surface pass's sampling contract.
                    wgpu::BindGroupLayoutEntry {
                        binding: 9,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 10,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    texture_entry(
                        11,
                        wgpu::TextureSampleType::Float { filterable: true },
                        wgpu::TextureViewDimension::D2,
                    ),
                    texture_entry(
                        15,
                        wgpu::TextureSampleType::Float { filterable: false },
                        wgpu::TextureViewDimension::D2Array,
                    ),
                    uniform_buffer_entry(16, 32),
                    // The atmosphere frame and sky-view LUT: the medium is
                    // lit by the same sky as the surface underside.
                    uniform_buffer_entry(20, helio_frame_api::lighting::sky::SKY_FRAME_BYTES),
                    texture_entry(
                        21,
                        wgpu::TextureSampleType::Float { filterable: true },
                        wgpu::TextureViewDimension::D2,
                    ),
                ],
            });
        let underwater_tint_shader = helio_core::shader::module_with(
            device,
            "Water Underwater Tint Shader",
            helio_core::include_wgsl!("../shaders/underwater_fog.wgsl"),
            &[
                crate::WATER_VOLUME_SNIPPET,
                WATER_SHORE_SNIPPET,
                helio_frame_api::lighting::sky::SKY_SNIPPET,
                WATER_SKY_SNIPPET,
            ],
        );
        let underwater_tint_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Underwater Tint PL"),
            bind_group_layouts: &[Some(&underwater_tint_bgl)],
            immediate_size: 0,
        });
        let underwater_tint_pipeline = helio_core::shader::create_render_pipeline_deferred(
            &device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Underwater Tint Pipeline"),
                layout: Some(&underwater_tint_pl),
                vertex: wgpu::VertexState {
                    module: &underwater_tint_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &underwater_tint_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        let shafts_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Half-Resolution Shafts BGL"),
            entries: &[
                storage_read_entry(0),
                storage_read_entry(1),
                texture_entry(
                    2,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2,
                ),
                texture_entry(
                    4,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                sampler_entry(5, wgpu::SamplerBindingType::Filtering),
                texture_entry(
                    6,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2Array,
                ),
                sampler_entry(7, wgpu::SamplerBindingType::Filtering),
                uniform_buffer_entry(8, 64),
                texture_entry(
                    9,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2,
                ),
                texture_entry(
                    10,
                    wgpu::TextureSampleType::Float { filterable: true },
                    wgpu::TextureViewDimension::D2Array,
                ),
                texture_entry(
                    15,
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::TextureViewDimension::D2Array,
                ),
                uniform_buffer_entry(16, 32),
            ],
        });
        let shafts_shader = helio_core::shader::module_with(
            device,
            "Water Half-Resolution Shafts Shader",
            helio_core::include_wgsl!("../shaders/underwater_shafts.wgsl"),
            &[crate::WATER_VOLUME_SNIPPET, WATER_SHORE_SNIPPET],
        );
        let shafts_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Half-Resolution Shafts PL"),
            bind_group_layouts: &[Some(&shafts_bgl)],
            immediate_size: 0,
        });
        let shafts_pipeline = helio_core::shader::create_render_pipeline_deferred(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Half-Resolution Shafts Pipeline"),
                layout: Some(&shafts_pl),
                vertex: wgpu::VertexState {
                    module: &shafts_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shafts_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        let render_pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Render PL"),
            bind_group_layouts: &[Some(&render_bgl)],
            immediate_size: 0,
        });

        // One module, two fragment entry points: `fs_above` and `fs_under` share
        // the vertex stage and every helper, so the surface can only be shaded
        // one way. They used to be separate files with the whole coordinate and
        // SSR apparatus duplicated between them.
        let surface_shader = helio_core::shader::module_with(
            device,
            "Water Surface Shader",
            helio_core::include_wgsl!("../shaders/surface.wgsl"),
            &[
                crate::WATER_VOLUME_SNIPPET,
                helio_pass_hiz::HIZ_SNIPPET,
                WATER_SHORE_SNIPPET,
                helio_frame_api::lighting::sky::SKY_SNIPPET,
                WATER_SKY_SNIPPET,
                WATER_LIGHTS_SNIPPET,
                helio_light_api::LIGHT_LIST_SNIPPET,
            ],
        );

        let vbl = vec4_vbl();

        let caustics = CausticsProjection::new(device);

        // Bake Ultra-Water's tileable lace/billow foam material once per pass
        // lifetime, then build its mip chain with ordinary render passes.
        let foam_bake_shader = helio_core::shader::module(
            device,
            "Water Foam Bake Shader",
            helio_core::include_wgsl!("../shaders/foam_bake.wgsl"),
        );
        let foam_bake_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Foam Bake PL"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let foam_bake_pipeline = helio_core::shader::create_render_pipeline_deferred(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Foam Bake Pipeline"),
                layout: Some(&foam_bake_pl),
                vertex: wgpu::VertexState {
                    module: &foam_bake_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &foam_bake_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );
        let foam_mip_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Foam Mip BGL"),
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
        let foam_mip_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Foam Mip PL"),
            bind_group_layouts: &[Some(&foam_mip_bgl)],
            immediate_size: 0,
        });
        let foam_mip_shader = helio_core::shader::module(
            device,
            "Water Foam Mip Shader",
            helio_core::include_wgsl!("../shaders/foam_mip.wgsl"),
        );
        let foam_mip_pipeline = helio_core::shader::create_render_pipeline_deferred(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Foam Mip Pipeline"),
                layout: Some(&foam_mip_pl),
                vertex: wgpu::VertexState {
                    module: &foam_mip_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &foam_mip_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        // The same explicit box-filter pass builds independent mip chains for
        // near and deep caustics, without atlas-slice bleed at repeat seams.
        let caustics_mip_pipeline = helio_core::shader::create_render_pipeline_deferred(
            device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Caustics Mip Pipeline"),
                layout: Some(&foam_mip_pl),
                vertex: wgpu::VertexState {
                    module: &foam_mip_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &foam_mip_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        let surface_pipeline = helio_core::shader::create_render_pipeline_deferred(
            &device,
            wgpu::RenderPipelineDescriptor {
                label: Some("Water Surface Pipeline"),
                layout: Some(&render_pl_layout),
                vertex: wgpu::VertexState {
                    module: &surface_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(vbl.clone())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &surface_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                // Writes the surface's own depth so near swells hide far ones.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(helio_core::depth::NEARER),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );

        let spray_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Water Spray BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let spray_shader = helio_core::shader::module_with(
            device,
            "Water Spray Shader",
            helio_core::include_wgsl!("../shaders/spray.wgsl"),
            &[crate::WATER_VOLUME_SNIPPET],
        );
        let spray_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Water Spray PL"),
            bind_group_layouts: &[Some(&spray_bgl)],
            immediate_size: 0,
        });
        let spray_pipeline = |label: &'static str, entry_point: &'static str| {
            helio_core::shader::create_render_pipeline_deferred(
                device,
                wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&spray_pl),
                    vertex: wgpu::VertexState {
                        module: &spray_shader,
                        entry_point: Some(entry_point),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &spray_shader,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: surface_format,
                            blend: Some(wgpu::BlendState {
                                color: wgpu::BlendComponent {
                                    src_factor: wgpu::BlendFactor::One,
                                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                    operation: wgpu::BlendOperation::Add,
                                },
                                alpha: wgpu::BlendComponent {
                                    src_factor: wgpu::BlendFactor::Zero,
                                    dst_factor: wgpu::BlendFactor::One,
                                    operation: wgpu::BlendOperation::Add,
                                },
                            }),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth32Float,
                        depth_write_enabled: Some(false),
                        depth_compare: Some(helio_core::depth::NEARER_EQUAL),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                },
            )
        };
        let spray_breaker_pipeline = spray_pipeline("Water Breaker Spray Pipeline", "vs_breaker");
        let spray_impact_pipeline = spray_pipeline("Water Impact Spray Pipeline", "vs_impact");
        let spray_impacts_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Water Spray Impact Events"),
            contents: bytemuck::cast_slice(&vec![
                crate::GpuWaterSprayImpact::zeroed();
                crate::MAX_WATER_SPRAY_IMPACTS
            ]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let spray_params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Water Spray Parameters"),
            contents: bytemuck::bytes_of(&crate::GpuWaterSprayParams {
                amount: 1.0,
                size: 1.0,
                max_distance: 350.0,
                emitter_count: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let spray_emitters_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Packed Water Breaker Emitters Placeholder"),
            contents: bytemuck::cast_slice(&[[0.0f32; 4]]),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let (top_vbuf, top_ibuf, top_index_count) = make_top_grid(device);
        let caustics_meshes = [
            make_caustics_grid(device, 128, 6),
            make_caustics_grid(device, 64, 7),
            make_caustics_grid(device, 32, 8),
        ];
        let [(caustics_vbuf_near, caustics_ibuf_near, caustics_index_near), (caustics_vbuf_mid, caustics_ibuf_mid, caustics_index_mid), (caustics_vbuf_deep, caustics_ibuf_deep, caustics_index_deep)] =
            caustics_meshes;
        let caustics_vbufs = [caustics_vbuf_near, caustics_vbuf_mid, caustics_vbuf_deep];
        let caustics_ibufs = [caustics_ibuf_near, caustics_ibuf_mid, caustics_ibuf_deep];
        let caustics_index_counts = [caustics_index_near, caustics_index_mid, caustics_index_deep];
        let caustics_slice_params = [
            [1.5f32, 128.0, 1.0, 5.0 / 128.0, 512.0, 0.0, 0.0, 0.0],
            [4.0f32, 64.0, 2.0, 6.0 / 64.0, 256.0, 0.0, 0.0, 0.0],
            [10.0f32, 32.0, 3.0, 7.0 / 32.0, 256.0, 1.0, 0.0, 0.0],
        ]
        .map(|values| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Water Caustics Slice Parameters"),
                contents: bytemuck::cast_slice(&values),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        });
        let make_caustics_texture = |label: &'static str, size: u32, mip_count: u32| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: mip_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let full_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mip_views = (0..mip_count)
                .map(|mip| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some(label),
                        format: Some(wgpu::TextureFormat::Rgba16Float),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_mip_level: mip,
                        mip_level_count: Some(1),
                        base_array_layer: 0,
                        array_layer_count: Some(1),
                        aspect: wgpu::TextureAspect::All,
                        usage: None,
                    })
                })
                .collect();
            (texture, full_view, mip_views)
        };
        let (caustics_near_tex, caustics_near_view, caustics_near_mip_views) =
            make_caustics_texture(
                "Water Near Caustics 512 Mip Chain",
                crate::CAUSTICS_SIZE,
                crate::CAUSTICS_NEAR_MIPS,
            );
        let (caustics_deep_tex, caustics_deep_view, caustics_deep_mip_views) =
            make_caustics_texture(
                "Water Deep Caustics 256 Mip Chain",
                crate::CAUSTICS_DEEP_SIZE,
                crate::CAUSTICS_DEEP_MIPS,
            );
        let make_msaa_target = |label: &'static str, size: u32| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 4,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        };
        let (caustics_near_msaa_tex, caustics_near_msaa_view) =
            make_msaa_target("Water Near Caustics 4x Splat Target", crate::CAUSTICS_SIZE);
        let (caustics_deep_msaa_tex, caustics_deep_msaa_view) = make_msaa_target(
            "Water Deep Caustics 4x Splat Target",
            crate::CAUSTICS_DEEP_SIZE,
        );

        let shore_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Water Shoreline Input Fields"),
            size: wgpu::Extent3d {
                width: crate::SHORE_MAP_SIZE,
                height: crate::SHORE_MAP_SIZE,
                depth_or_array_layers: 2,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let shore_view = shore_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Water Shoreline Input Array"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(2),
            ..Default::default()
        });
        let shore_params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Water Shoreline Map Parameters"),
            contents: bytemuck::cast_slice(&[0.0f32; 8]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Self {
            ocean_fft: crate::ocean_fft::OceanFft::new(device),
            surface_queries: crate::surface_query::WaterSurfaceQueries::new(device),
            reflection_capture: None,
            reflection_scale: crate::WaterSettings::default().reflection_scale,
            water_in_view: crate::capture_schedule::WaterInView::new(),
            ocean_rest_height: 0.0,
            shore_swell: 1.0,
            shore_map: None,
            shore_map_dirty: false,
            _shore_texture: shore_texture,
            shore_view,
            shore_params,
            interaction: crate::simulation::Interaction::new(device),
            sim_tex_a,
            sim_array_view_a,
            sim_array_view_b,
            sim_layer_views_a,
            sampler,
            output_sampler,
            depth_sampler,
            hitbox_origin: [0.0; 3],
            hitbox_prev_shift: [0.0; 3],
            pending_drops: std::collections::VecDeque::new(),
            drop_staged: false,
            top_vbuf,
            top_ibuf,
            top_index_count,
            caustics_vbufs,
            caustics_ibufs,
            caustics_index_counts,
            caustics_slice_params,
            _caustics_near_msaa_tex: caustics_near_msaa_tex,
            caustics_near_msaa_view,
            _caustics_deep_msaa_tex: caustics_deep_msaa_tex,
            caustics_deep_msaa_view,
            _caustics_near_tex: caustics_near_tex,
            caustics_near_view,
            caustics_near_mip_views,
            _caustics_deep_tex: caustics_deep_tex,
            caustics_deep_view,
            caustics_deep_mip_views,
            caustics_mip_pipeline,
            caustics_sampler,
            fallback_atmosphere,
            fallback_lights,
            _fallback_sky_view_tex: fallback_sky_view_tex,
            fallback_sky_view,
            _foam_texture: foam_texture,
            foam_view,
            foam_mip_views,
            foam_sampler,
            foam_bake_pipeline,
            foam_mip_bgl,
            foam_mip_pipeline,
            foam_baked: false,
            caustics_render_bgl: caustics.bind_group_layout,
            render_bgl,
            caustics_pipeline: caustics.pipeline,
            surface_pipeline,
            _pre_aa_fallback_tex: pre_aa_fallback_tex,
            pre_aa_fallback_view,
            _gbuffer_fallback_tex: gbuffer_fallback_tex,
            gbuffer_fallback_view,
            internal_width,
            internal_height,
            surface_format,
            viewport_buf: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Water Viewport"),
                contents: bytemuck::cast_slice(&[
                    internal_width as f32,
                    internal_height as f32,
                    1.0 / internal_width as f32,
                    1.0 / internal_height as f32,
                ]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            }),
            blit_bgl,
            blit_pipeline,
            blit_bg: None,
            blit_bg_key: None,
            water_output_view: None,
            content_allocated: false,
            screen_active: false,
            sim_live: false,
            caustics_bg_keys: [None, None, None],
            caustics_bgs: [None, None, None],
            render_bg: Default::default(),
            underwater_tint_bg: Default::default(),
            _tint_scratch_tex: tint_scratch_tex,
            tint_scratch_view,
            _water_depth_tex: water_depth_tex,
            water_depth_view,
            tint_blit_bg: None,
            underwater_tint_bgl,
            underwater_tint_pipeline,
            _shafts_scratch_tex: shafts_scratch_tex,
            shafts_view: shafts_scratch_view,
            shafts_bgl,
            shafts_pipeline,
            shafts_bg: Default::default(),
            spray_bgl,
            spray_breaker_pipeline,
            spray_impact_pipeline,
            spray_bg: Default::default(),
            spray_impacts_buf,
            spray_params_buf,
            spray_emitters_buf,
            spray_emitters_pending: Vec::new(),
            spray_emitters_dirty: false,
            spray_live_emitter_count: 0,
            spray_settings: crate::WaterSpraySettings::default(),
            spray_impacts: std::collections::VecDeque::new(),
            spray_live_impact_count: 0,
            wave_spring: 1.2,
            wave_damping: 0.985,
            wind_direction: [0.0, 0.0],
            wind_strength: 0.0,
            wave_scale: 1.0,
            wave_speed: 1.0,
            step_clock: 0.0,
            steps_this_frame: 0,
        }
    }
}
