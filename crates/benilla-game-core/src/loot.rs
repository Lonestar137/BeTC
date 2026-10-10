//! Converted build-8606-zone creature loot tables and deterministic table rolls.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::quest::QuestItemTemplate;

const LOOT_PACK: &str = include_str!("../content/tbc-creature-loot.toml");
pub const LOOT_SCHEMA_VERSION: u32 = 1;
const MAX_REFERENCE_DEPTH: usize = 8;
pub const MAX_CORPSE_ITEMS: usize = 16;

#[derive(Debug, Deserialize)]
pub struct LootPack {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub creature: Vec<CreatureLoot>,
    pub creature_loot: Vec<LootEntry>,
    pub reference_loot: Vec<LootEntry>,
    pub item_template: Vec<QuestItemTemplate>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CreatureLoot {
    pub entry: u32,
    pub loot_id: u32,
    pub min_gold: u32,
    pub max_gold: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LootEntry {
    pub table_id: u32,
    pub item: u32,
    pub chance: f64,
    pub group_id: u32,
    pub min_count_or_ref: i32,
    pub max_count: u32,
    pub condition_id: u32,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LootItem {
    pub entry: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LootRoll {
    pub gold: u32,
    pub items: Vec<LootItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpseLoot {
    pub spawn_id: u32,
    pub respawn_remaining_ms: u32,
    pub loot: LootRoll,
    pub looted_items: Vec<bool>,
    pub money_looted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LootWindow {
    pub gold: u32,
    pub items: Vec<(u8, LootItem)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LootError {
    NoActiveCharacter,
    NotLootable,
    InvalidSlot,
    BackpackFull,
    MoneyOverflow,
}

pub fn loot_data() -> &'static LootPack {
    static DATA: OnceLock<LootPack> = OnceLock::new();
    DATA.get_or_init(|| {
        let data: LootPack = toml::from_str(LOOT_PACK).expect("checked-in TBC loot pack parses");
        assert_eq!(data.schema_version, LOOT_SCHEMA_VERSION);
        data
    })
}

pub fn creature_loot(creature_entry: u32) -> Option<&'static CreatureLoot> {
    loot_data()
        .creature
        .iter()
        .find(|creature| creature.entry == creature_entry)
}

pub fn item_template(entry: u32) -> Option<&'static QuestItemTemplate> {
    loot_data()
        .item_template
        .iter()
        .find(|item| item.entry == entry)
}

/// Roll direct, grouped, and nested reference entries from the creature's converted loot id.
/// Quest-only rows are included only when the caller confirms the character needs that item.
pub fn roll(
    creature_entry: u32,
    mut random: impl FnMut() -> u64,
    quest_item_needed: impl Fn(u32) -> bool,
) -> Option<LootRoll> {
    let data = loot_data();
    let creature = creature_loot(creature_entry)?;
    let mut items = Vec::new();
    if creature.loot_id != 0 {
        roll_table(
            creature.loot_id,
            false,
            data,
            &mut random,
            &quest_item_needed,
            0,
            &mut items,
        );
    }
    let gold = if creature.min_gold <= creature.max_gold {
        roll_inclusive(creature.min_gold, creature.max_gold, &mut random)
    } else {
        0
    };
    Some(LootRoll { gold, items })
}

fn roll_table(
    table_id: u32,
    reference: bool,
    data: &LootPack,
    random: &mut impl FnMut() -> u64,
    quest_item_needed: &impl Fn(u32) -> bool,
    depth: usize,
    drops: &mut Vec<LootItem>,
) {
    if depth >= MAX_REFERENCE_DEPTH {
        return;
    }
    let rows = if reference {
        &data.reference_loot
    } else {
        &data.creature_loot
    };
    let mut groups = std::collections::BTreeMap::<u32, Vec<&LootEntry>>::new();
    for row in rows.iter().filter(|row| row.table_id == table_id) {
        if row.condition_id != 0 || (row.chance < 0.0 && !quest_item_needed(row.item)) {
            continue;
        }
        groups.entry(row.group_id).or_default().push(row);
    }
    if let Some(independent) = groups.remove(&0) {
        for row in independent {
            let chance = row.chance.abs();
            if chance >= 100.0 || (chance > 0.0 && random_percent(random) < chance) {
                roll_entry(row, data, random, quest_item_needed, depth, drops);
            }
        }
    }
    for group in groups.into_values() {
        let selected = select_group_entry(&group, random);
        if let Some(row) = selected {
            roll_entry(row, data, random, quest_item_needed, depth, drops);
        }
    }
}

fn select_group_entry<'a>(
    rows: &'a [&'a LootEntry],
    random: &mut impl FnMut() -> u64,
) -> Option<&'a LootEntry> {
    let positive_weight: f64 = rows.iter().map(|row| row.chance.abs()).sum();
    let scale = if positive_weight > 100.0 {
        100.0 / positive_weight
    } else {
        1.0
    };
    let zero_rows: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| row.chance == 0.0)
        .collect();
    let zero_weight = if zero_rows.is_empty() {
        0.0
    } else {
        (100.0 - positive_weight.min(100.0)) / zero_rows.len() as f64
    };
    let total = (positive_weight * scale) + zero_weight * zero_rows.len() as f64;
    if total == 0.0 {
        return None;
    }
    let roll = random_percent(random);
    let mut cumulative = 0.0;
    for row in rows {
        let weight = if row.chance == 0.0 {
            zero_weight
        } else {
            row.chance.abs() * scale
        };
        cumulative += weight;
        if roll < cumulative {
            return Some(row);
        }
    }
    None
}

fn roll_entry(
    row: &LootEntry,
    data: &LootPack,
    random: &mut impl FnMut() -> u64,
    quest_item_needed: &impl Fn(u32) -> bool,
    depth: usize,
    drops: &mut Vec<LootItem>,
) {
    if row.min_count_or_ref < 0 {
        roll_table(
            row.min_count_or_ref.unsigned_abs(),
            true,
            data,
            random,
            quest_item_needed,
            depth + 1,
            drops,
        );
    } else if row.item != 0 {
        if drops.len() < MAX_CORPSE_ITEMS {
            drops.push(LootItem {
                entry: row.item,
                count: roll_inclusive(row.min_count_or_ref as u32, row.max_count, random),
            });
        }
    }
}

fn random_percent(random: &mut impl FnMut() -> u64) -> f64 {
    (random() % 1_000_000) as f64 / 10_000.0
}

fn roll_inclusive(min: u32, max: u32, random: &mut impl FnMut() -> u64) -> u32 {
    if max <= min {
        return min;
    }
    min + (random() % (u64::from(max - min) + 1)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_starting_zone_loot_has_provenance_and_resolved_references() {
        let data = loot_data();
        assert_eq!(data.source, "https://github.com/cmangos/tbc-db");
        assert_eq!(data.revision, "866723612d93791e442fb1b18d642118fa682edd");
        assert_eq!(data.license, "GPL-3.0");
        assert!(data.creature.len() >= 300);
        assert!(data.reference_loot.len() >= 800);
        assert!(data.item_template.len() >= 400);
        for row in data
            .creature_loot
            .iter()
            .chain(&data.reference_loot)
            .filter(|row| row.min_count_or_ref < 0)
        {
            assert!(
                data.reference_loot
                    .iter()
                    .any(|entry| entry.table_id == row.min_count_or_ref.unsigned_abs())
            );
        }
        assert!(data.creature_loot.iter().any(|row| row.condition_id != 0));
        assert!(data.creature_loot.iter().any(|row| row.chance < 0.0));
    }

    #[test]
    fn mana_wyrm_loot_roll_is_seeded_and_resolves_reference_tables() {
        let mut state = 7;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let first = roll(15_274, &mut rng, |_| false).unwrap();
        let first_state = state;
        state = 7;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let second = roll(15_274, &mut rng, |_| false).unwrap();
        assert_eq!(first, second);
        assert_ne!(first_state, 7);
        assert_eq!(first.gold, 0);
        assert!(first.items.iter().all(|item| item.count > 0));
    }
}
