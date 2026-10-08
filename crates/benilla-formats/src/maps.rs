//! `Map.dbc`: a server `mapId` to its MPQ directory (`0` "Azeroth", `1` "Kalimdor", `36`
//! "DeadminesInstance") for `MapTiles::load`, plus the columns the client reads per map. Build
//! 5875 records have 42 fields; build 8606 records have 125, with localized strings inline.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};

const MAP: &str = "DBFilesClient\\Map.dbc";

#[derive(Clone, Copy)]
struct MapLayout {
    field_count: usize,
    locale_width: usize,
    localized_fields: &'static [(usize, &'static str)],
    loading_screen: usize,
    min_level: usize,
    max_level: usize,
    max_players: usize,
    field_16: usize,
    field_17: usize,
    field_18: usize,
    bracket_span: Option<usize>,
    group_queue: Option<usize>,
    minimap_icon_scale: usize,
}

const VANILLA_LAYOUT: MapLayout = MapLayout {
    field_count: 42,
    locale_width: 9,
    localized_fields: &[
        (4, "MapName"),
        (20, "MapDescription0"),
        (29, "MapDescription1"),
    ],
    loading_screen: 38,
    min_level: 13,
    max_level: 14,
    max_players: 15,
    field_16: 16,
    field_17: 17,
    field_18: 18,
    bracket_span: Some(39),
    group_queue: Some(40),
    minimap_icon_scale: 41,
};

// Build 8606 Map.dbc, measured from its WDBC header and row layout. Localized fields have 16
// locale offsets plus the flags word; the unused locale offsets stay integer-typed.
const TBC_LAYOUT: MapLayout = MapLayout {
    field_count: 125,
    locale_width: 17,
    localized_fields: &[
        (4, "MapName"),
        (28, "MapDescriptionAlliance"),
        (45, "MapDescriptionHorde"),
        (66, "Requirement"),
        (83, "HeroicRequirement"),
        (100, "UnknownRequirement"),
    ],
    loading_screen: 62,
    min_level: 21,
    max_level: 22,
    max_players: 23,
    field_16: 24,
    field_17: 25,
    field_18: 26,
    bracket_span: None,
    group_queue: None,
    minimap_icon_scale: 65,
};

/// `Map.dbc`'s per-map data, keyed by map id.
pub struct MapCatalog {
    dirs: HashMap<u32, String>,
    /// `MapName_Lang[enUS]` (field 4, `+0x10`, read by `0x4a65a0`). The continent dropdown shows
    /// this ("Eastern Kingdoms"), not the art folder ("Azeroth") (`GetMapContinents` `0x4a7ce0`).
    names: HashMap<u32, String>,
    /// `LoadingScreenID`, for maps with a non-zero one.
    loading_screens: HashMap<u32, u32>,
    /// `InstanceType` (field 2, `+0x8`, read at `0x48a772`, `0x495cb9`, `0x495d33`): 0 none,
    /// 1 party, 2 raid, 3 battleground, as `IsInInstance` spells them (`0x83de58`, guarded `< 4`).
    instance_types: HashMap<u32, u32>,
    /// The battleground columns of every row, since the client resolves map 0's too.
    battleground: HashMap<u32, MapBattlegroundColumns>,
}

/// The `Map.dbc` values used by battleground presentation. Build 8606 stores the first six at
/// fields 21-26 and descriptions at 28/45; Vanilla stores them at 13-18 and 20/29.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MapBattlegroundColumns {
    /// Minimum level: field 21 in build 8606, field 13 in Vanilla.
    pub min_level: u32,
    /// Maximum level: field 22 in build 8606, field 14 in Vanilla.
    pub max_level: u32,
    /// Maximum players: field 23 in build 8606, field 15 in Vanilla.
    pub max_players: u32,
    /// The next raw integer: field 24 in build 8606, field 16 in Vanilla.
    pub field_16: i32,
    /// The next two raw floats: fields 25/26 in build 8606, 17/18 in Vanilla.
    pub field_17: f32,
    pub field_18: f32,
    /// Localized descriptions in Alliance/Horde order: fields 28/45 in build 8606, 20/29 in
    /// Vanilla.
    pub descriptions: [String; 2],
    /// Bracket span in Vanilla. TBC does not store this in `Map.dbc`.
    pub bracket_span: u32,
    /// Group-queue flag in Vanilla. TBC does not store this in `Map.dbc`.
    pub group_queue: u32,
    /// `MinimapIconScale`: field 65 in build 8606, field 41 in Vanilla.
    pub minimap_icon_scale: f32,
}

impl MapBattlegroundColumns {
    /// A wire bracket's `(min, max)` levels (`0x4aa6c0`/`0x4aa850`): with a span,
    /// `min = bracket · span + min_level` and `max = min(span + min − 1, 60)`, else both zero.
    pub fn bracket_levels(&self, bracket: u8) -> (u32, u32) {
        if self.bracket_span == 0 {
            return (0, 0);
        }
        let min = u32::from(bracket) * self.bracket_span + self.min_level;
        (min, (self.bracket_span + min).saturating_sub(1).min(60))
    }
}

impl MapCatalog {
    /// The MPQ directory name (`Directory`) for `map_id`, as `MapTiles::load(chain, dir)` takes it.
    pub fn directory(&self, map_id: u32) -> Option<&str> {
        self.dirs.get(&map_id).map(String::as_str)
    }

    /// The enUS display name (`MapName_Lang`) for `map_id`, "Eastern Kingdoms".
    pub fn name(&self, map_id: u32) -> Option<&str> {
        self.names.get(&map_id).map(String::as_str)
    }

    /// `LoadingScreenID` for `map_id`, to resolve against [`crate::LoadingScreenCatalog`].
    pub fn loading_screen_id(&self, map_id: u32) -> Option<u32> {
        self.loading_screens.get(&map_id).copied()
    }

    /// `InstanceType` for `map_id`. The client treats a missing row as type 0 wherever it asks.
    pub fn instance_type(&self, map_id: u32) -> Option<u32> {
        self.instance_types.get(&map_id).copied()
    }

    /// Whether `map_id` is a party dungeon, the reference's lockout test rather than "is an
    /// instance": `cmp [rec+8],1` gates `SMSG_UPDATE_LAST_INSTANCE` and `CanShowResetInstances`.
    pub fn is_party_dungeon(&self, map_id: u32) -> bool {
        self.instance_type(map_id) == Some(1)
    }

    /// The battleground columns for `map_id`; the client reads map 0's when nothing was listed.
    pub fn battleground(&self, map_id: u32) -> Option<&MapBattlegroundColumns> {
        self.battleground.get(&map_id)
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

const INSTANCE_TYPE_FIELD: usize = 2;

/// The unread fields are `u32` placeholders: only the 42 × 4 = 168-byte record has to add up.
fn map_schema(layout: MapLayout) -> Schema {
    let mut s = Schema::new("Map");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Directory", FieldType::String));
    let mut i = 2;
    while i < layout.field_count {
        if let Some((_, name)) = layout
            .localized_fields
            .iter()
            .find(|(start, _)| *start == i)
        {
            s.add_field(SchemaField::new(*name, FieldType::String));
            for locale in 1..layout.locale_width {
                s.add_field(SchemaField::new(
                    format!("_{name}_{locale}"),
                    FieldType::UInt32,
                ));
            }
            i += layout.locale_width;
            continue;
        }
        let ty = if [layout.field_17, layout.field_18, layout.minimap_icon_scale].contains(&i) {
            FieldType::Float32
        } else {
            FieldType::UInt32
        };
        let name = if i == INSTANCE_TYPE_FIELD {
            "InstanceType".to_string()
        } else if i == layout.loading_screen {
            "LoadingScreenID".to_string()
        } else {
            format!("_field{i}")
        };
        s.add_field(SchemaField::new(name, ty));
        i += 1;
    }
    s
}

/// Read Map.dbc off the patch chain into a [`MapCatalog`].
pub fn load_map_catalog(chain: &mut Chain) -> Result<MapCatalog> {
    let bytes = chain
        .read_file(MAP)
        .with_context(|| format!("reading {MAP}"))?;
    let layout = if chain.client_build() == Some(8606) {
        TBC_LAYOUT
    } else {
        VANILLA_LAYOUT
    };
    let rs = parse(&bytes, map_schema(layout), "Map")?;
    let mut dirs = HashMap::with_capacity(rs.records().len());
    let mut names = HashMap::with_capacity(rs.records().len());
    let mut loading_screens = HashMap::new();
    let mut instance_types = HashMap::with_capacity(rs.records().len());
    let mut battleground = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        battleground.insert(
            id,
            MapBattlegroundColumns {
                min_level: u32_at(r, layout.min_level).unwrap_or(0),
                max_level: u32_at(r, layout.max_level).unwrap_or(0),
                max_players: u32_at(r, layout.max_players).unwrap_or(0),
                field_16: u32_at(r, layout.field_16).unwrap_or(0) as i32,
                field_17: f32_at(r, layout.field_17).unwrap_or(0.0),
                field_18: f32_at(r, layout.field_18).unwrap_or(0.0),
                descriptions: [
                    str_at(&rs, r, layout.localized_fields[1].0).unwrap_or_default(),
                    str_at(&rs, r, layout.localized_fields[2].0).unwrap_or_default(),
                ],
                bracket_span: layout
                    .bracket_span
                    .and_then(|field| u32_at(r, field))
                    .unwrap_or(0),
                group_queue: layout
                    .group_queue
                    .and_then(|field| u32_at(r, field))
                    .unwrap_or(0),
                minimap_icon_scale: f32_at(r, layout.minimap_icon_scale).unwrap_or(1.0),
            },
        );
        if let Some(dir) = str_at(&rs, r, 1) {
            dirs.insert(id, dir);
        }
        if let Some(name) = str_at(&rs, r, layout.localized_fields[0].0).filter(|n| !n.is_empty()) {
            names.insert(id, name);
        }
        // Type 0 is recorded too, so `None` means only that no such map exists.
        if let Some(ty) = u32_at(r, INSTANCE_TYPE_FIELD) {
            instance_types.insert(id, ty);
        }
        if let Some(ls) = u32_at(r, layout.loading_screen).filter(|&v| v != 0) {
            loading_screens.insert(id, ls);
        }
    }
    Ok(MapCatalog {
        dirs,
        names,
        loading_screens,
        instance_types,
        battleground,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_battleground_columns_read_the_shipped_rows() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let catalog = load_map_catalog(&mut chain).expect("Map.dbc");
        if chain.client_build() == Some(8606) {
            let wsg = catalog.battleground(489).expect("Warsong Gulch");
            assert_eq!(
                (wsg.min_level, wsg.max_level, wsg.max_players, wsg.field_16),
                (10, 70, 10, -1)
            );
            let ab = catalog.battleground(529).expect("Arathi Basin");
            assert_eq!((ab.min_level, ab.max_players, ab.field_16), (20, 15, -1));
            assert!((ab.minimap_icon_scale - 1.25).abs() < 1e-6);
            assert_eq!(wsg.bracket_span, 0, "TBC stores brackets outside Map.dbc");
            assert_eq!(wsg.group_queue, 0, "not a build-8606 Map.dbc column");
            return;
        }
        let wsg = catalog.battleground(489).expect("Warsong Gulch");
        assert_eq!(
            (wsg.min_level, wsg.max_level, wsg.max_players, wsg.field_16),
            (10, 60, 10, -1)
        );
        assert_eq!((wsg.bracket_span, wsg.group_queue), (10, 1));
        assert!(wsg.descriptions[0].starts_with("A valley bordering Ashenvale"));
        assert_eq!(
            wsg.descriptions[0], wsg.descriptions[1],
            "both faction descriptions carry the same text on the shipped rows"
        );
        assert_eq!(wsg.bracket_levels(0), (10, 19));
        assert_eq!(wsg.bracket_levels(5), (60, 60), "the max clamps at 60");
        let ab = catalog.battleground(529).expect("Arathi Basin");
        assert_eq!(
            (ab.min_level, ab.max_players, ab.bracket_span),
            (20, 15, 10)
        );
        assert!(
            (ab.minimap_icon_scale - 1.25).abs() < 1e-6,
            "Arathi Basin\'s MinimapIconScale"
        );
        assert!((wsg.minimap_icon_scale - 1.0).abs() < 1e-6);
        assert_eq!(ab.bracket_levels(1), (30, 39));
        let av = catalog.battleground(30).expect("Alterac Valley");
        assert_eq!((av.min_level, av.max_players, av.group_queue), (51, 40, 0));
        assert_eq!(
            av.bracket_levels(0),
            (0, 0),
            "a zero span zeroes both bounds"
        );
        assert!((av.field_17 - 0.74).abs() < 1e-3 && (av.field_18 - 0.34).abs() < 1e-3);
        assert!(
            catalog.battleground(0).is_some(),
            "every row carries the columns — the client resolves map 0's when nothing was listed"
        );
    }

    #[test]
    fn build_8606_map_layout_resolves_sunstrider_isle() {
        let data = crate::wow_data_or_skip!();
        if !crate::is_tbc_data_dir(&data) {
            crate::skipped("resolved install is not TBC", &[data]);
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open build-8606 chain");
        let catalog = load_map_catalog(&mut chain).expect("parse build-8606 Map.dbc");

        assert_eq!(chain.client_build(), Some(8606));
        assert_eq!(catalog.directory(530), Some("Expansion01"));
        assert_eq!(catalog.name(530), Some("Outland"));
        assert!(catalog.loading_screen_id(530).is_some());
    }
}
