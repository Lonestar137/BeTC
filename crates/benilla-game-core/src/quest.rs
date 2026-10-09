//! Source-backed quest content and persistent player quest state.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const QUEST_PACK: &str = include_str!("../content/tbc-sunstrider-quests.toml");
const STARTING_GEAR_PACK: &str = include_str!("../content/tbc-starting-gear.toml");
pub const QUEST_SCHEMA_VERSION: u32 = 1;
pub const STARTING_GEAR_SCHEMA_VERSION: u32 = 1;
pub const QUEST_LOG_SLOTS: usize = 20;
pub const BACKPACK_SLOTS: u8 = 16;
/// Worn equipment, `INV_SLOT` 0..18 (head through tabard).
pub const EQUIPMENT_SLOTS: u8 = 19;

#[derive(Debug, Deserialize)]
pub struct QuestPack {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub conversion_version: u32,
    pub client_build: u32,
    pub client_gender_source: String,
    pub extraction_command: String,
    pub quest: Vec<QuestDefinition>,
    pub item_template: Vec<QuestItemTemplate>,
}

#[derive(Debug, Deserialize)]
pub struct QuestDefinition {
    pub id: u32,
    pub title: String,
    pub method: u32,
    pub zone_or_sort: i32,
    pub min_level: u8,
    pub max_level: u8,
    pub quest_level: i16,
    pub quest_type: u32,
    pub quest_flags: u32,
    pub required_race_mask: u32,
    pub required_class_mask: u32,
    pub next_quest_in_chain: u32,
    pub source_item_entry: u32,
    pub giver_creature_entries: Vec<u32>,
    pub turnin_creature_entries: Vec<u32>,
    pub giver_npc_flags: u32,
    pub gossip_menu_id: u32,
    pub gossip_text_id: u32,
    pub gossip_text_male: String,
    pub gossip_text_female: String,
    pub gossip_text_probability: f32,
    pub gossip_npc_gender: u8,
    pub details: String,
    pub objectives_text: String,
    pub request_items_text: String,
    pub offer_reward_text: String,
    pub end_text: String,
    pub reward_money_copper: u32,
    pub reward_money_max_level: u32,
    pub project_reward_xp: u32,
    pub objective: Vec<QuestObjective>,
    pub reward_choice: Vec<QuestRewardChoice>,
}

#[derive(Debug, Deserialize)]
pub struct QuestObjective {
    pub kind: String,
    pub creature_entry: u32,
    pub count: u32,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct QuestRewardChoice {
    pub item_entry: u32,
    pub item_name: String,
    pub display_id: u32,
    pub count: u32,
}

/// The source fields needed by the stock item tooltip, inventory feed and reward presentation.
#[derive(Debug, Deserialize)]
pub struct QuestItemTemplate {
    pub entry: u32,
    pub name: String,
    pub class: u32,
    pub subclass: u32,
    pub display_id: u32,
    pub quality: u32,
    pub buy_price: u32,
    pub sell_price: u32,
    pub inventory_type: u32,
    pub allowable_class: i32,
    pub allowable_race: i32,
    pub item_level: u32,
    pub stackable: u32,
    pub armor: u32,
    pub bonding: u32,
    pub material: u32,
    pub max_durability: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuestJournal {
    pub entries: Vec<QuestProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestProgress {
    pub quest_id: u32,
    pub objective_counts: Vec<u32>,
    /// Choice index selected at turn-in. `Some` also marks the quest as rewarded.
    pub reward_choice: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryItem {
    /// Stable within the character save; the presentation adapter maps it to an item GUID.
    pub instance_id: u64,
    pub entry: u32,
    pub count: u32,
    pub slot: u8,
}

/// One worn starting item: its equip slot (`0..18`) is resolved from `InventoryType` at import.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct StartingItem {
    pub race: u8,
    pub class: u8,
    pub gender: u8,
    pub entry: u32,
    pub slot: u8,
    pub amount: u32,
}

/// The starting-outfit content pack: item stats for worn starting gear plus the per
/// race/class/gender rows that say who wears what.
#[derive(Debug, Deserialize)]
pub struct StartingGearPack {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub client_build: u32,
    pub extraction_command: String,
    pub item_template: Vec<QuestItemTemplate>,
    pub starting_item: Vec<StartingItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestObjectiveUpdate {
    pub quest_id: u32,
    pub creature_entry: u32,
    pub count: u32,
    pub required: u32,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestReward {
    pub quest_id: u32,
    /// XP threshold crossings applied by this turn-in.
    pub level: u8,
    pub levels_gained: u8,
    /// Remaining XP within the resulting level.
    pub experience_in_level: u32,
    pub experience: u32,
    pub money_copper: u32,
    pub choice_index: u32,
    pub item: InventoryItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestOfferState {
    Available,
    Incomplete,
    Rewardable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestGossipRow {
    pub quest_id: u32,
    pub icon: QuestOfferState,
    pub level: u32,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestError {
    NoActiveCharacter,
    UnknownQuest,
    WrongNpc,
    WrongRace,
    WrongClass,
    LevelTooLow,
    LevelTooHigh,
    AlreadyAccepted,
    AlreadyRewarded,
    QuestLogFull,
    Incomplete,
    InvalidRewardChoice,
    InventoryFull,
    MoneyOverflow,
    ExperienceOverflow,
}

impl std::fmt::Display for QuestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoActiveCharacter => "no active character",
            Self::UnknownQuest => "quest is not in the local content pack",
            Self::WrongNpc => "NPC does not offer or complete this quest",
            Self::WrongRace => "character race does not meet the quest requirement",
            Self::WrongClass => "character class does not meet the quest requirement",
            Self::LevelTooLow => "character does not meet the quest level requirement",
            Self::LevelTooHigh => "character exceeds the quest level requirement",
            Self::AlreadyAccepted => "quest is already active",
            Self::AlreadyRewarded => "quest was already rewarded",
            Self::QuestLogFull => "the quest log is full",
            Self::Incomplete => "quest objectives are incomplete",
            Self::InvalidRewardChoice => "reward choice is not available for this quest",
            Self::InventoryFull => "no free inventory slot for the quest reward",
            Self::MoneyOverflow => "quest reward would overflow the copper balance",
            Self::ExperienceOverflow => "quest reward would overflow experience",
        })
    }
}

impl std::error::Error for QuestError {}

pub fn race_allowed(definition: &QuestDefinition, race: u8) -> bool {
    (1..=32).contains(&race) && definition.required_race_mask & (1u32 << (race - 1)) != 0
}

pub fn class_allowed(definition: &QuestDefinition, class: u8) -> bool {
    definition.required_class_mask == 0
        || ((1..=32).contains(&class)
            && definition.required_class_mask & (1u32 << (class - 1)) != 0)
}

pub fn level_allowed(definition: &QuestDefinition, level: u8) -> bool {
    level >= definition.min_level
        && (definition.max_level == u8::MAX || level <= definition.max_level)
}

pub fn quest_pack() -> &'static QuestPack {
    static PACK: OnceLock<QuestPack> = OnceLock::new();
    PACK.get_or_init(|| {
        let pack: QuestPack =
            toml::from_str(QUEST_PACK).expect("checked-in Sunstrider quest content must parse");
        assert_eq!(pack.schema_version, QUEST_SCHEMA_VERSION);
        assert_eq!(pack.client_build, 8606);
        assert_eq!(pack.quest.len(), 1);
        pack
    })
}

pub fn quest_definition(quest_id: u32) -> Option<&'static QuestDefinition> {
    quest_pack().quest.iter().find(|quest| quest.id == quest_id)
}

pub fn item_template(entry: u32) -> Option<&'static QuestItemTemplate> {
    quest_pack()
        .item_template
        .iter()
        .chain(starting_gear_pack().item_template.iter())
        .find(|item| item.entry == entry)
}

pub fn starting_gear_pack() -> &'static StartingGearPack {
    static PACK: OnceLock<StartingGearPack> = OnceLock::new();
    PACK.get_or_init(|| {
        let pack: StartingGearPack = toml::from_str(STARTING_GEAR_PACK)
            .expect("checked-in starting gear content must parse");
        assert_eq!(pack.schema_version, STARTING_GEAR_SCHEMA_VERSION);
        assert_eq!(pack.client_build, 8606);
        pack
    })
}

/// The worn starting items for a race, class and gender, in file order.
pub fn starting_items(race: u8, class: u8, gender: u8) -> impl Iterator<Item = &'static StartingItem> {
    starting_gear_pack()
        .starting_item
        .iter()
        .filter(move |item| item.race == race && item.class == class && item.gender == gender)
}

impl QuestJournal {
    pub fn progress(&self, quest_id: u32) -> Option<&QuestProgress> {
        self.entries
            .iter()
            .find(|progress| progress.quest_id == quest_id)
    }

    pub fn slot(&self, quest_id: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|progress| progress.quest_id == quest_id)
    }

    pub fn is_complete(&self, quest_id: u32) -> bool {
        let Some(progress) = self.progress(quest_id) else {
            return false;
        };
        let Some(definition) = quest_definition(quest_id) else {
            return false;
        };
        definition
            .objective
            .iter()
            .zip(&progress.objective_counts)
            .all(|(objective, &count)| count >= objective.count)
    }
}

pub fn valid_journal(journal: &QuestJournal) -> bool {
    if journal.entries.len() > QUEST_LOG_SLOTS
        || !journal
            .entries
            .windows(2)
            .all(|pair| pair[0].quest_id < pair[1].quest_id)
    {
        return false;
    }
    journal.entries.iter().all(|progress| {
        let Some(definition) = quest_definition(progress.quest_id) else {
            return false;
        };
        if progress.objective_counts.len() != definition.objective.len() {
            return false;
        }
        for (objective, &count) in definition.objective.iter().zip(&progress.objective_counts) {
            if count > objective.count {
                return false;
            }
        }
        progress.reward_choice.is_none_or(|choice| {
            choice < definition.reward_choice.len() as u32
                && progress
                    .objective_counts
                    .iter()
                    .zip(&definition.objective)
                    .all(|(&count, objective)| count >= objective.count)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_reclaiming_quest_and_item_references_are_source_backed() {
        let pack = quest_pack();
        let quest = quest_definition(8325).expect("Reclaiming Sunstrider Isle");

        assert_eq!(pack.revision, "866723612d93791e442fb1b18d642118fa682edd");
        assert_eq!(quest.giver_creature_entries, [15278]);
        assert_eq!(quest.turnin_creature_entries, [15278]);
        assert_eq!(quest.objective.len(), 1);
        assert_eq!(quest.objective[0].creature_entry, 15274);
        assert_eq!(quest.objective[0].count, 8);
        assert_eq!(quest.reward_choice.len(), 2);
        assert_eq!(quest.gossip_menu_id, 6566);
        assert_eq!(quest.gossip_text_id, 7781);
        assert_eq!(quest.gossip_npc_gender, 1);
        assert_eq!(quest.project_reward_xp, 50);
        assert!(valid_journal(&QuestJournal::default()));
        for choice in &quest.reward_choice {
            let item = item_template(choice.item_entry).expect("reward item template");
            assert_eq!(item.display_id, choice.display_id);
        }
    }
}
