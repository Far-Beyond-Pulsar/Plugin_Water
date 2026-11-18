//!use water_volume

// Packed scene rows can be sparse for direct hosts. Classify slot indices and
// retain last frame's active mask so a retired heightfield is cleared once.
struct Slots {
    previous: u32,
    // x = live slot, y = retired slot.
    indices: array<vec2u>,
}

struct SimUniform {
    spring: f32,
    damping: f32,
    hitbox_count: u32,
    _pad: u32,
    previous_shift: vec4f,
    drop_center: vec2f,
    drop_radius: f32,
    drop_strength: f32,
}

override CASCADES: u32;
override SIM_SIZE: u32;
const GROUP_SIZE: u32 = 8u;
override PATCH_SIZE: f32;

@group(0) @binding(0) var<storage, read_write> args: array<u32, 6>;
@group(0) @binding(1) var<storage, read_write> slots: Slots;
@group(0) @binding(2) var<storage, read> volumes: array<WaterVolume>;
@group(0) @binding(3) var source: texture_2d_array<f32>;
@group(0) @binding(4) var destination: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(5) var<uniform> u: SimUniform;
@group(0) @binding(6) var<storage, read> hitboxes: array<GpuWaterHitbox>;
@group(0) @binding(7) var<storage, read> previous: array<GpuWaterHitbox>;

@compute @workgroup_size(1)
fn classify() {
    var mask = 0u;
    var live_count = 0u;
    var retired_count = 0u;
    let capacity = arrayLength(&slots.indices);
    for (var slot = 0u; slot < capacity; slot++) {
        let bit = 1u << slot;
        var is_live = false;
        if slot < arrayLength(&volumes) {
            let volume = volumes[slot];
            is_live = any(volume.bounds_max.xyz - volume.bounds_min.xyz > vec3f(1e-4));
        }
        if is_live {
            mask |= bit;
            slots.indices[live_count].x = slot;
            live_count++;
        } else if (slots.previous & bit) != 0u {
            slots.indices[retired_count].y = slot;
            retired_count++;
        }
    }
    slots.previous = mask;
    let groups = SIM_SIZE / GROUP_SIZE;
    args = array<u32, 6>(groups, groups, live_count,
                          groups, groups, retired_count);
}

/// One AABB hitbox (80 bytes = 5 × vec4<f32>)
struct GpuWaterHitbox {
    old_min:  vec4<f32>,   // xyz = old AABB min
    old_max:  vec4<f32>,   // xyz = old AABB max
    new_min:  vec4<f32>,   // xyz = new AABB min
    new_max:  vec4<f32>,   // xyz = new AABB max
    params:   vec4<f32>,   // x = edge_softness, y = strength, z = identity
}

/// Smooth 3-D Gaussian falloff inside an AABB, times the depth of the box
/// below the surface.
///
/// The weight is 1.0 inside the box's footprint, smoothly tapering to 0
/// beyond its edges; `softness` (metres) scales how far the falloff extends.
/// The texel stands for the point of its tile nearest the box, at the rest
/// height; outside the volume's footprint it is 0.
fn volume_in_box(box_min: vec3<f32>, box_max: vec3<f32>, uv: vec2<f32>, softness: f32, vol: WaterVolume) -> f32 {
    let box_center  = (box_min + box_max) * 0.5;
    let box_half    = (box_max - box_min) * 0.5;
    let rest_y      = vol.bounds_max.w;

    // The world XZ this texel stands for, in the tile nearest the box. The
    // waves tile the world, so in a camera-relative frame the texel's
    // tiling is offset by the volume's world phase (as in `surface.wgsl`).
    let tile = uv * PATCH_SIZE - vol.world_phase.xy;
    let world_xz = tile + PATCH_SIZE * round((box_center.xz - tile) / PATCH_SIZE);
    if any(world_xz < vol.bounds_min.xz) || any(world_xz > vol.bounds_max.xz) {
        return 0.0;
    }

    // Per-axis distance from the box's footprint (negative inside, positive outside)
    let d = abs(world_xz - box_center.xz) - box_half.xz;

    // Smooth falloff: exp(-clamp(d/softness, 0, 4)^2) per axis, multiplied together
    let soft = max(d, vec2<f32>(0.0)) / max(softness, 0.001);
    let weight = exp(-dot(soft * soft, vec2<f32>(1.0)));

    // Only the part of the box below the surface displaces water.
    let submerged_depth = clamp(rest_y - box_min.y, 0.0, max(box_max.y - box_min.y, 0.0));

    return weight * submerged_depth * 0.1;
}

// The texture contains height, velocity, and the X/Z normal. Integer loads at
// texel centers reproduce the old linear clamp-to-edge sampler stencil.
fn load_state(pixel: vec2i, layer: i32) -> vec4f {
    let limit = vec2i(textureDimensions(source)) - vec2i(1);
    return textureLoad(source, clamp(pixel, vec2i(0), limit), layer, 0);
}

fn live_layer(job: u32) -> i32 {
    return i32(slots.indices[job].x * CASCADES);
}

@compute @workgroup_size(GROUP_SIZE, GROUP_SIZE)
fn clear_retired(@builtin(global_invocation_id) id: vec3u) {
    let layer = slots.indices[id.z].y * CASCADES;
    textureStore(destination, vec2i(id.xy), i32(layer), vec4f(0.0));
}

@compute @workgroup_size(GROUP_SIZE, GROUP_SIZE)
fn displace(@builtin(global_invocation_id) id: vec3u) {
    let layer = live_layer(id.z);
    let pixel = vec2i(id.xy);
    var info = load_state(pixel, layer);
    let vol = volumes[slots.indices[id.z].x];
    if all(vol.bounds_max.xz > vol.bounds_min.xz) {
        let uv = (vec2f(id.xy) + 0.5) / vec2f(textureDimensions(source));
        let count = min(u.hitbox_count, min(arrayLength(&hitboxes), arrayLength(&previous)));
        for (var i = 0u; i < count; i++) {
            let hb = hitboxes[i];
            let strength = hb.params.y;
            if strength == 0.0 {
                continue;
            }
            var old_min = hb.old_min.xyz;
            var old_max = hb.old_max.xyz;
            let last = previous[i];
            if last.params.z == hb.params.z && last.params.y != 0.0 {
                old_min = last.new_min.xyz - u.previous_shift.xyz;
                old_max = last.new_max.xyz - u.previous_shift.xyz;
            }
            info.r += volume_in_box(old_min, old_max, uv, hb.params.x, vol) * strength;
            info.r -= volume_in_box(hb.new_min.xyz, hb.new_max.xyz, uv, hb.params.x, vol) * strength;
        }
    }
    textureStore(destination, pixel, layer, info);
}

@compute @workgroup_size(GROUP_SIZE, GROUP_SIZE)
fn add_drop(@builtin(global_invocation_id) id: vec3u) {
    let layer = live_layer(id.z);
    let pixel = vec2i(id.xy);
    var info = load_state(pixel, layer);
    let uv = (vec2f(id.xy) + 0.5) / vec2f(textureDimensions(source));
    let ripple = max(0.0, 1.0 - length(u.drop_center * 0.5 + 0.5 - uv) / u.drop_radius);
    info.r += (0.5 - cos(ripple * 3.14159265) * 0.5) * u.drop_strength;
    textureStore(destination, pixel, layer, info);
}

@compute @workgroup_size(GROUP_SIZE, GROUP_SIZE)
fn propagate(@builtin(global_invocation_id) id: vec3u) {
    let layer = live_layer(id.z);
    let pixel = vec2i(id.xy);
    var info = load_state(pixel, layer);
    let dynamics = volumes[slots.indices[id.z].x].sim_dynamics;
    let spring = select(u.spring, clamp(dynamics.x, 0.1, 2.0), dynamics.x > 0.0);
    let damping = select(u.damping, clamp(dynamics.y, 0.0, 1.0), dynamics.x > 0.0);
    let dx = vec2i(1, 0);
    let dy = vec2i(0, 1);
    let avg = (load_state(pixel - dx, layer).r + load_state(pixel - dy, layer).r
             + load_state(pixel + dx, layer).r + load_state(pixel + dy, layer).r) * 0.25;
    info.g = (info.g + (avg - info.r) * spring) * damping;
    info.r += info.g;
    textureStore(destination, pixel, layer, info);
}

@compute @workgroup_size(GROUP_SIZE, GROUP_SIZE)
fn normals(@builtin(global_invocation_id) id: vec3u) {
    let layer = live_layer(id.z);
    let pixel = vec2i(id.xy);
    var info = load_state(pixel, layer);
    let right = load_state(pixel + vec2i(1, 0), layer).r;
    let up = load_state(pixel + vec2i(0, 1), layer).r;
    let delta = 1.0 / vec2f(textureDimensions(source));
    let tangent_x = vec3f(delta.x, right - info.r, 0.0);
    let tangent_z = vec3f(0.0, up - info.r, delta.y);
    let normal = normalize(cross(tangent_z, tangent_x));
    info.b = normal.x;
    info.a = normal.z;
    textureStore(destination, pixel, layer, info);
}

@compute @workgroup_size(GROUP_SIZE, GROUP_SIZE)
fn publish_state(@builtin(global_invocation_id) id: vec3u) {
    let layer = live_layer(id.z);
    let pixel = vec2i(id.xy);
    textureStore(destination, pixel, layer, load_state(pixel, layer));
}
