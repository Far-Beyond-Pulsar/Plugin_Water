# Plugin_Water

Water volumes: their component, rows and the renderer feature that simulates, lights and draws them, with their caustics as the frame's transmission.

`pulsar.water` ("Water") is a plugin of
[Pulsar-Native](https://github.com/Far-Beyond-Pulsar/Pulsar-Native), the engine and editor that renders with
[Helio](https://github.com/Far-Beyond-Pulsar/Helio). Pulsar-Native vendors this repository as a git submodule
at `plugins/vendor/water`.

## What it owns

Every object kind is a plugin. A plugin owns its SceneDB components (their
data and scriptable methods; the editor calls the same methods scripts do),
the contracts other kinds read (its `api` crate: graph keys, row ABIs, WGSL
snippets), the Helio feature that renders it (passes, scene derivations,
settings), its file formats and its editor UI. Helio knows no object kind,
and other kinds meet this one only through frame contracts, extension points,
draw phases and api crates.

| Crate | Path | What |
|---|---|---|
| `water_plugin` | `./` | Water (`pulsar.water`): the plugin's root crate, the one place it registers everything it contributes (runtime registrations always, the editor's with `editor`) |
| `helio-water-api` | `api/` | The water feature's public contract: the terrain elevation seam its shoreline bakes from. |
| `water_component` | `component/` | Water (pulsar.water): the water volume component, its rows, its row join and the plugin's Helio feature set (the water simulation and draw); the plugin's root crate registers them |
| `water_editor` | `editor/` | Water (pulsar.water): the editor crate -- the level editor's water entries, over the plugin's component crate |
| `water-examples` | `examples/` | The water plugin's standalone hosts on Helio: the cathedral water and the oil rig at night |
| `helio-pass-water-sim` | `feature/` | Helio feature: water simulation, lighting, caustics (the frame's transmission) and drawing |

- **Root crate**: `water_plugin`. It is the one place the plugin submits
  its contributions (component classes, features, scene seams, settings,
  editor tools), each naming it as origin, so the plugin gate and the
  plugin catalog see all of them. The other crates submit none (only type
  attachments, such as a component's GPU mirror, stay with their types). A
  game links the root without its `editor` feature; the editor links it with.
- **Requires**: nothing. A project enables or disables the plugin in its
  `Pulsar.toml` (`[plugins]`); disabled, the kind is absent and the frame
  draws without it.
- **Reads other kinds through**: `helio-foliage-api` (foliage); `helio-light-api, helio-pass-deferred-light` (lights); `helio-mats` (materials); `helio-pass-gbuffer, helio-pass-indirect-dispatch, helio-pass-object-batch, helio-pass-occlusion-cull` (meshes).
- **Its standalone hosts also compose**: `helio-pass-volumetric-fog` (fog); `helio-pass-gi` (gi); `helio-pass-flare, helio-pass-lens` (lens); `helio-light-api, helio-light-host` (lights); `helio-mats` (materials); `helio-asset-compat, helio-mesh-api, helio-mesh-host, helio-pass-forward-lit, helio-pass-vg, helio-vg-build, helio-vg-format, helio-vg-stream` (meshes); `helio-pass-sky` (sky) (demos and GPU tests
  draw a whole scene; the plugin itself does not depend on these).

## Render graph nodes

Every pass the plugin's feature adds to the default pipeline is a node of `.rendergraph` assets, registered from its feature (`registrar.node`, so only while the plugin is enabled) with generated pins, an `Enabled` parameter and the default placement below. A graph naming one while `pulsar.water` is disabled fails validation with the plugin named. Test: `tests/graph_node.rs`; see Pulsar-Native's `.agents/RENDER_GRAPH.md` ("Plugin nodes").

| Node kind | Title | Default placement |
|---|---|---|
| `helio.pass.water_transmission` | Water: Caustics | Lighting-inputs slot, order 20 (builds nothing where lighting runs before the inputs) |
| `helio.pass.water_sim` | Water: Simulate + Draw | Overlay phase drawer, order 30 |
| `helio.pass.water_reactivity` | Water: TSR Reactivity | Transparent phase drawer, order 50 (builds nothing without a reactive mask) |

## Building and testing

Like [Plugin_Physics](https://github.com/Far-Beyond-Pulsar/Plugin_Physics), this plugin's crates are members
of Pulsar-Native's workspace: their dependencies are `workspace = true`, and
Helio comes from Pulsar-Native's `crates/renderer/helio` submodule. It builds
only inside a Pulsar-Native checkout:

```bash
git clone --recursive https://github.com/Far-Beyond-Pulsar/Pulsar-Native
cd Pulsar-Native
cargo check -p helio-water-api -p water_component -p water_plugin -p water_editor -p water-examples -p helio-pass-water-sim --tests
cargo test -p helio-water-api -p water_plugin -p water_component -p helio-pass-water-sim
```

Suites that render real frames need a GPU; CI leaves them out by name
(Pulsar-Native's `.config/nextest.toml`, `ci` profile). This repository's CI
checks the plugin inside a Pulsar-Native checkout: rustfmt, clippy and the
`ci` profile's tests.

A change that spans this plugin and Pulsar-Native (or Helio) merges
dependency-first: merge here, re-pin `plugins/vendor/water` in Pulsar-Native to
this repository's `main`, then merge the Pulsar-Native PR (Pulsar-Native's
`.agents/CRATES.md`, "Submodule pins").

## Architecture

- [Every object kind is a plugin](https://github.com/Far-Beyond-Pulsar/Helio/blob/main/docs/architecture/plugin-migration.md): the
  rule, what Helio is, the plugin table and the "As built" notes of each
  phase.
- [The layered rewrite](https://github.com/Far-Beyond-Pulsar/Helio/blob/main/docs/architecture/layered-rewrite.md): layers, buffer
  contracts, extension points and why the renderer has this shape.
- [The `helio-architecture` skill](https://github.com/Far-Beyond-Pulsar/Helio/blob/main/.agents/skills/helio-architecture/SKILL.md): required reading for any change
  here (see `AGENTS.md`).
- Pulsar-Native's `.agents/PLUGIN_SYSTEM.md` and `.agents/CRATES.md`: plugin
  descriptors, the installed set and the submodule pins.

History before 2026-11-02 (when the plugin moved into Pulsar-Native) is in
Helio's repository; this repository's history starts with that move
(`git subtree split` of `plugins/vendor/water`).

## License

MIT, as the rest of Pulsar (`LICENSE.md`).
