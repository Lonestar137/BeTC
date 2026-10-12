//! Converted TBC server content consumed by the offline runtime, never at runtime from SQL.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const WORLD_SPAWNS: &str = include_str!("../content/tbc-world-spawns.toml");
const CONTENT_SCHEMA_VERSION: u32 = 3;
const CREATURE_EVENT_POLICY: &str = "base-world-only";

#[derive(Debug, Deserialize)]
pub struct CreatureContent {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub event_policy: String,
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

/// Lookup index over the parsed pack, built once on first use. Population and streaming query it
/// per travel step, so the linear scans over 100k spawns would otherwise dominate a session.
struct ContentIndex {
    templates: HashMap<u32, &'static CreatureTemplate>,
    spawns_by_map: HashMap<u32, Vec<&'static CreatureSpawn>>,
    waypoints: HashMap<u32, Vec<&'static CreatureWaypoint>>,
    addons: HashMap<u32, &'static CreatureAddon>,
}

fn content_index() -> &'static ContentIndex {
    static INDEX: OnceLock<ContentIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let content = creature_content();
        let mut templates = HashMap::with_capacity(content.template.len());
        for template in &content.template {
            templates.insert(template.entry, template);
        }
        let mut spawns_by_map: HashMap<u32, Vec<&CreatureSpawn>> = HashMap::new();
        for spawn in &content.spawn {
            spawns_by_map.entry(spawn.map_id).or_default().push(spawn);
        }
        let mut waypoints: HashMap<u32, Vec<&CreatureWaypoint>> = HashMap::new();
        for waypoint in &content.waypoint {
            waypoints
                .entry(waypoint.source_guid)
                .or_default()
                .push(waypoint);
        }
        let mut addons = HashMap::with_capacity(content.addon.len());
        for addon in &content.addon {
            addons.insert(addon.source_guid, addon);
        }
        ContentIndex {
            templates,
            spawns_by_map,
            waypoints,
            addons,
        }
    })
}

pub fn creature_content() -> &'static CreatureContent {
    static CONTENT: OnceLock<CreatureContent> = OnceLock::new();
    CONTENT.get_or_init(|| {
        let content: CreatureContent = toml::from_str(WORLD_SPAWNS)
            .expect("checked-in converted world content must parse");
        assert_eq!(
            content.schema_version, CONTENT_SCHEMA_VERSION,
            "checked-in converted world content schema must match the loader"
        );
        assert_eq!(
            content.event_policy, CREATURE_EVENT_POLICY,
            "the runtime expects the converted base-world event policy"
        );
        content
    })
}

impl CreatureContent {
    pub fn template(&self, entry: u32) -> Option<&CreatureTemplate> {
        content_index().templates.get(&entry).copied()
    }

    pub fn spawns_near(
        &self,
        map_id: u32,
        x: f32,
        y: f32,
        radius: f32,
    ) -> impl Iterator<Item = &'static CreatureSpawn> {
        let radius_squared = radius * radius;
        content_index()
            .spawns_by_map
            .get(&map_id)
            .into_iter()
            .flat_map(|spawns| spawns.iter().copied())
            .filter(move |spawn| {
                (spawn.x - x).mul_add(spawn.x - x, (spawn.y - y) * (spawn.y - y)) <= radius_squared
            })
    }

    pub fn waypoints_for(
        &self,
        source_guid: u32,
    ) -> impl Iterator<Item = &'static CreatureWaypoint> {
        content_index()
            .waypoints
            .get(&source_guid)
            .into_iter()
            .flat_map(|waypoints| waypoints.iter().copied())
    }

    pub fn addon_for(&self, source_guid: u32) -> Option<&CreatureAddon> {
        content_index().addons.get(&source_guid).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn converted_content_has_provenance_and_resolvable_stable_population() {
        let content = creature_content();
        assert_eq!(content.source, "https://github.com/cmangos/tbc-db");
        assert_eq!(content.license, "GPL-3.0");
        assert_eq!(content.revision.len(), 40);
        assert_eq!(content.event_policy, CREATURE_EVENT_POLICY);
        assert!(content.template.len() >= 11_000);
        assert!(content.spawn.len() >= 94_000);
        assert!(content.waypoint.len() >= 98_000);
        assert!(content.addon.len() >= 1_000);
        assert!(
            content
                .template
                .iter()
                .all(|template| !template.name.trim().is_empty())
        );

        let maps: HashSet<_> = content.spawn.iter().map(|spawn| spawn.map_id).collect();
        for map_id in [0, 1, 530] {
            assert!(maps.contains(&map_id), "the world pack covers map {map_id}");
        }
        assert!(maps.len() >= 60, "the world pack covers {} maps", maps.len());

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
            // Source movement types are carried verbatim; only 2 patrols, the rest stand still.
            // Type 3 appears across the world and is treated as idle.
            assert!(matches!(spawn.movement_type, 0 | 1 | 2 | 3 | 4));
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
        for event_spawn_guid in [94_529, 94_864, 86_849, 150_786] {
            assert!(
                !source_guids.contains(&event_spawn_guid),
                "event-only creature spawn {event_spawn_guid} is absent from the base world"
            );
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

    /// The lazy index answers exactly what the flat scan would: the Sunstrider anchor's nearby
    /// spawns in source order, the template for a converted entry, and the patrol rows for a
    /// waypoint spawn.
    #[test]
    fn spawn_index_agrees_with_the_flat_pack() {
        let content = creature_content();
        let (x, y, radius) = (10_349.6, -6_357.3, 180.0);
        let indexed: Vec<u32> = content
            .spawns_near(530, x, y, radius)
            .map(|spawn| spawn.source_guid)
            .collect();
        let scanned: Vec<u32> = content
            .spawn
            .iter()
            .filter(|spawn| {
                spawn.map_id == 530
                    && (spawn.x - x) * (spawn.x - x) + (spawn.y - y) * (spawn.y - y)
                        <= radius * radius
            })
            .map(|spawn| spawn.source_guid)
            .collect();
        assert!(!indexed.is_empty());
        assert_eq!(indexed, scanned);

        let template = content.template(6).expect("Kobold Vermin");
        assert_eq!(template.name, "Kobold Vermin");
        assert_eq!(
            content.template(6).map(|template| template.entry),
            Some(6),
            "the index resolves the same entry the scan would"
        );

        let waypoints: Vec<u32> = content
            .waypoints_for(80127)
            .map(|waypoint| waypoint.point)
            .collect();
        assert!(waypoints.len() > 1, "80127 is a converted patrol");
        let mut sorted = waypoints.clone();
        sorted.sort_unstable();
        assert_eq!(waypoints, sorted, "patrol points stay in source order");
    }
}
