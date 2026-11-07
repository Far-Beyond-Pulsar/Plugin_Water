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
| `helio-water-api` | `api/` | The water feature's public contract: the terrain elevation seam its shoreline bakes from. |
| `water_component` | `component/` | Water (pulsar.water): the water volume component, its rows and the registrations of the plugin's Helio feature (the water simulation and draw) |
| `water_plugin` | `editor/` | Water (pulsar.water): the editor plugin -- its metadata, over the plugin's component crate |
| `water-examples` | `examples/` | The water plugin's standalone hosts on Helio: the cathedral water and the oil rig at night |
| `helio-pass-water-sim` | `feature/` | Helio feature: water simulation, lighting, caustics (the frame's transmission) and drawing |

- **Runtime module** (what a game built for a project links): `water_component`.
- **Editor module** (what the editor links in addition): `water_plugin`.
- **Requires**: nothing. A project enables or disables the plugin in its
  `Pulsar.toml` (`[plugins]`); disabled, the kind is absent and the frame
  draws without it.
- **Reads other kinds through**: `helio-foliage-api` (foliage); `helio-light-api, helio-pass-deferred-light` (lights); `helio-mats` (materials); `helio-pass-gbuffer, helio-pass-indirect-dispatch, helio-pass-object-batch, helio-pass-occlusion-cull` (meshes).
- **Its standalone hosts also compose**: `helio-pass-volumetric-fog` (fog); `helio-pass-gi` (gi); `helio-pass-flare, helio-pass-lens` (lens); `helio-light-api, helio-light-host` (lights); `helio-mats` (materials); `helio-asset-compat, helio-mesh-api, helio-mesh-host, helio-pass-forward-lit, helio-pass-vg, helio-vg-build, helio-vg-format, helio-vg-stream` (meshes); `helio-pass-sky` (sky) (demos and GPU tests
  draw a whole scene; the plugin itself does not depend on these).

## Building and testing

Like [Plugin_Physics](https://github.com/Far-Beyond-Pulsar/Plugin_Physics), this plugin's crates are members
of Pulsar-Native's workspace: their dependencies are `workspace = true`, and
Helio comes from Pulsar-Native's `crates/renderer/helio` submodule. It builds
only inside a Pulsar-Native checkout:

```bash
git clone --recursive https://github.com/Far-Beyond-Pulsar/Pulsar-Native
cd Pulsar-Native
cargo check -p helio-water-api -p water_component -p water_plugin -p water-examples -p helio-pass-water-sim --tests
cargo test -p helio-water-api -p water_component -p helio-pass-water-sim
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
