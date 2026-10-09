//! Converted TBC server content consumed by the offline runtime, never at runtime from SQL.

use std::sync::OnceLock;

use serde::Deserialize;

const STARTING_ZONES: &str = include_str!("../content/tbc-starting-zones.toml");
const CONTENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Deserialize)]
pub struct StartingZoneContent {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub template: Vec<CreatureTemplate>,
    pub spawn: Vec<CreatureSpawn>,
    #[serde(default)]
    pub waypoint: Vec<CreatureWaypoint>,
    #[serde(default)]
    pub addon: Vec<CreatureAddon>,
}

#[derive(Debug, Deserialize)]
pub struct CreatureTemplate {
    pub entry: u32,
    pub name: String,
    pub subname: String,
    pub level: u8,
    pub display_id: u32,
    pub faction: u32,
    pub scale: f32,
    pub creature_type: u32,
    pub npc_flags: u32,
}

#[derive(Debug, Deserialize)]
pub struct CreatureSpawn {
    pub source_guid: u32,
    pub entry: u32,
    pub map_id: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub orientation: f32,
    pub respawn_min_secs: u32,
    pub respawn_max_secs: u32,
    pub wander_radius: f32,
    pub movement_type: u8,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CreatureWaypoint {
    pub source_guid: u32,
    pub point: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub orientation: f32,
    pub wait_ms: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CreatureAddon {
    pub source_guid: u32,
    pub stand_state: u8,
    pub sheath_state: u8,
    pub emote: u32,
}

pub fn starting_zone_content() -> &'static StartingZoneContent {
    static CONTENT: OnceLock<StartingZoneContent> = OnceLock::new();
    CONTENT.get_or_init(|| {
        let content: StartingZoneContent =
            toml::from_str(STARTING_ZONES).expect("checked-in starting-zone content must parse");
        assert_eq!(
            content.schema_version, CONTENT_SCHEMA_VERSION,
            "checked-in starting-zone content schema must match the loader"
        );
        content
    })
}

impl StartingZoneContent {
    pub fn template(&self, entry: u32) -> Option<&CreatureTemplate> {
        self.template
            .iter()
            .find(|template| template.entry == entry)
    }

    pub fn spawns_near(
        &self,
        map_id: u32,
        x: f32,
        y: f32,
        radius: f32,
    ) -> impl Iterator<Item = &CreatureSpawn> {
        let radius_squared = radius * radius;
        self.spawn.iter().filter(move |spawn| {
            spawn.map_id == map_id
                && (spawn.x - x).mul_add(spawn.x - x, (spawn.y - y) * (spawn.y - y))
                    <= radius_squared
        })
    }

    pub fn waypoints_for(&self, source_guid: u32) -> impl Iterator<Item = &CreatureWaypoint> {
        self.waypoint
            .iter()
            .filter(move |waypoint| waypoint.source_guid == source_guid)
    }

    pub fn addon_for(&self, source_guid: u32) -> Option<&CreatureAddon> {
        self.addon
            .iter()
            .find(|addon| addon.source_guid == source_guid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn converted_content_has_provenance_and_resolvable_stable_population() {
        let content = starting_zone_content();
        assert_eq!(content.source, "https://github.com/cmangos/tbc-db");
        assert_eq!(content.license, "GPL-3.0");
        assert_eq!(content.revision.len(), 40);
        assert!(content.template.len() >= 300);
        assert!(content.spawn.len() >= 1_000);
        assert!(content.waypoint.len() >= 1_000);
        assert!(!content.addon.is_empty());

        let template_entries: HashSet<_> = content.template.iter().map(|item| item.entry).collect();
        assert_eq!(template_entries.len(), content.template.len());
        assert!(content
            .template
            .windows(2)
            .all(|pair| pair[0].entry < pair[1].entry));
        let mut source_guids = HashSet::new();
        let mut movement_types = HashSet::new();
        assert!(content
            .spawn
            .windows(2)
            .all(|pair| pair[0].source_guid < pair[1].source_guid));
        for spawn in &content.spawn {
            assert!(
                source_guids.insert(spawn.source_guid),
                "duplicate spawn GUID"
            );
            assert!(
                spawn.source_guid <= 0x00ff_ffff,
                "spawn GUID does not fit imported identity"
            );
            assert!(
                spawn.entry <= 0x00ff_ffff,
                "template entry does not fit imported identity"
            );
            assert!(
                template_entries.contains(&spawn.entry),
                "spawn template is missing"
            );
            assert!(matches!(spawn.movement_type, 0 | 1 | 2 | 4));
            movement_types.insert(spawn.movement_type);
            assert!([
                spawn.x,
                spawn.y,
                spawn.z,
                spawn.orientation,
                spawn.wander_radius
            ]
            .into_iter()
            .all(f32::is_finite));
        }
        assert!(
            movement_types.contains(&4),
            "source movement type 4 stays explicit"
        );

        let spawn_by_guid: std::collections::HashMap<_, _> = content
            .spawn
            .iter()
            .map(|spawn| (spawn.source_guid, spawn.movement_type))
            .collect();
        assert!(content.waypoint.windows(2).all(|pair| {
            pair[0].source_guid < pair[1].source_guid
                || (pair[0].source_guid == pair[1].source_guid && pair[0].point < pair[1].point)
        }));
        for waypoint in &content.waypoint {
            assert_eq!(spawn_by_guid.get(&waypoint.source_guid), Some(&2));
            assert!([waypoint.x, waypoint.y, waypoint.z, waypoint.orientation]
                .into_iter()
                .all(f32::is_finite));
        }
        let mut addon_guids = HashSet::new();
        assert!(content
            .addon
            .windows(2)
            .all(|pair| pair[0].source_guid < pair[1].source_guid));
        for addon in &content.addon {
            assert!(spawn_by_guid.contains_key(&addon.source_guid));
            assert!(
                addon_guids.insert(addon.source_guid),
                "duplicate creature addon"
            );
        }

        assert!(content
            .template
            .iter()
            .any(|template| template.name == "Mana Wyrm"));
        assert!(content
            .template
            .iter()
            .any(|template| template.name == "Springpaw Lynx"));
        assert!(content
            .waypoint
            .iter()
            .any(|waypoint| waypoint.source_guid == 80127));
    }
}
