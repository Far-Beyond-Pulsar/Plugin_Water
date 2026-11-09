//! What the water plugin registers for a game (and the editor): every
//! runtime contribution, grouped by the engine seam it plugs into.

use engine_backend::scene::helio_bridge::SceneRowsRegistration;
use engine_backend::subsystems::render::{AnimatedContentRegistration, RenderFeatureRegistration};
use water_component::WaterVolumeComponent;

// ── Component classes ───────────────────────────────────────────────────────

pulsar_world_registry::submit_world_component!(WaterVolumeComponent);

// ── Rendering ───────────────────────────────────────────────────────────────

// The water feature: the simulation, its draw and the caustics published as
// the frame's transmission.
inventory::submit! {
    RenderFeatureRegistration {
        origin: pulsar_plugins::origin!(),
        features: water_component::render_features,
        configure: |config| config,
    }
}

// The volumes' source rows and the row join that places them, writes their
// sun and wind and bounds the water hitboxes.
inventory::submit! {
    SceneRowsRegistration {
        origin: pulsar_plugins::origin!(),
        name: "water.volumes",
        register_columns: water_component::register_columns,
        derivation: water_component::derivation,
    }
}

// Water simulates on the frame clock: any enabled volume keeps a realtime
// viewport drawing.
inventory::submit! {
    AnimatedContentRegistration {
        origin: pulsar_plugins::origin!(),
        name: "water.volumes",
        watch: water_component::watch_water,
        animated: water_component::water_live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_backend::subsystems::render::helio_renderer::render_features;

    #[test]
    fn registers_its_feature() {
        let features = render_features::plugin_features();
        assert!(
            features.ids().contains(&"helio.water"),
            "features: {:?}",
            features.ids()
        );
    }

    #[test]
    fn registers_under_its_class_name() {
        assert_eq!(
            pulsar_world_registry::component_id_for_class(water_component::WATER_VOLUME_CLASS_NAME),
            Some(pulsar_scenedb::component_id::<WaterVolumeComponent>())
        );
    }

    #[test]
    fn an_enabled_volume_animates() {
        use pulsar_scene_model::{SpawnObject, world_ext::SceneWorldExt};
        let mut world = pulsar_scenedb::World::new();
        let object = world.spawn_object(SpawnObject::new("lake")).unwrap();
        assert!(!water_component::water_live(&world));
        let water = pulsar_world_registry::attach_value(
            &mut world,
            object,
            WaterVolumeComponent::default(),
        )
        .unwrap();
        assert!(
            water_component::water_live(&world),
            "an enabled water volume animates"
        );
        world
            .get_mut::<WaterVolumeComponent>(water)
            .unwrap()
            .enabled = false;
        assert!(
            !water_component::water_live(&world),
            "a disabled one does not"
        );
    }

    /// Every registration of the plugin names this root, so it resolves to
    /// the plugin's id.
    #[test]
    fn every_registration_resolves_to_the_plugin() {
        let ours: Vec<_> = pulsar_plugins::contributions()
            .into_iter()
            .filter(|summary| summary.origin == pulsar_plugins::origin!())
            .collect();
        assert!(
            ours.iter()
                .any(|summary| summary.category == "component.class")
        );
        for summary in &ours {
            assert_eq!(
                pulsar_plugins::plugin_of(summary.origin).map(|plugin| plugin.id.as_str()),
                Some("pulsar.water"),
                "{summary:?}"
            );
        }
    }
}
