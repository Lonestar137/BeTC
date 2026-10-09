//! Converted TBC starter and trainer progression data.

use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

const PROGRESSION_PACK: &str = include_str!("../content/tbc-progression.toml");
const CONDITIONS_PACK: &str = include_str!("../content/tbc-spell-conditions.toml");
const LEVEL_XP_PACK: &str = include_str!("../content/tbc-player-levels.toml");
pub const PROGRESSION_SCHEMA_VERSION: u32 = 1;
pub const CONDITIONS_SCHEMA_VERSION: u32 = 1;
pub const LEVEL_XP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct ProgressionData {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub extraction_command: String,
    pub starter_spell: Vec<StarterSpell>,
    pub action_bar: Vec<StartingAction>,
    pub trainer_spell: Vec<TrainerSpell>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StarterSpell {
    pub race: u8,
    pub class: u8,
    pub spell_id: u32,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct StartingAction {
    pub race: u8,
    pub class: u8,
    pub button: u8,
    pub action: u32,
    #[serde(rename = "type")]
    pub action_type: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrainerSpell {
    pub class: u8,
    pub spell_id: u32,
    pub required_level: u8,
    /// Source SkillLine gate; offers with a nonzero id or value remain unavailable until character
    /// skill ranks are modeled.
    pub required_skill_id: u32,
    pub required_skill_value: u32,
    pub cost_copper: u32,
    pub prerequisite_spell_ids: Vec<u32>,
    pub trainer_entries: Vec<u32>,
    pub trainer_template_ids: Vec<u32>,
    /// Source condition id, resolved through the separate converted condition pack.
    pub condition_id: u32,
}

#[derive(Debug, Deserialize)]
pub struct SpellConditionData {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub extraction_command: String,
    pub condition: Vec<SourceCondition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceCondition {
    pub id: u32,
    pub kind: i8,
    pub value1: u32,
    pub value2: u32,
    pub value3: u32,
    pub value4: u32,
    pub flags: u8,
    pub comment: String,
}

#[derive(Debug, Deserialize)]
pub struct LevelXpData {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub extraction_command: String,
    pub level_xp: Vec<LevelXp>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct LevelXp {
    pub level: u8,
    pub xp_to_next: u32,
}

pub fn progression_data() -> &'static ProgressionData {
    static DATA: OnceLock<ProgressionData> = OnceLock::new();
    DATA.get_or_init(|| {
        let data: ProgressionData =
            toml::from_str(PROGRESSION_PACK).expect("checked-in progression content must parse");
        assert_eq!(
            data.schema_version, PROGRESSION_SCHEMA_VERSION,
            "checked-in progression content schema must match the loader"
        );
        data
    })
}

pub fn spell_condition_data() -> &'static SpellConditionData {
    static DATA: OnceLock<SpellConditionData> = OnceLock::new();
    DATA.get_or_init(|| {
        let data: SpellConditionData =
            toml::from_str(CONDITIONS_PACK).expect("checked-in spell condition content parses");
        assert_eq!(
            data.schema_version, CONDITIONS_SCHEMA_VERSION,
            "checked-in spell condition schema must match the loader"
        );
        data
    })
}

pub fn spell_condition(id: u32) -> Option<&'static SourceCondition> {
    spell_condition_data()
        .condition
        .iter()
        .find(|condition| condition.id == id)
}

pub fn level_xp_data() -> &'static LevelXpData {
    static DATA: OnceLock<LevelXpData> = OnceLock::new();
    DATA.get_or_init(|| {
        let data: LevelXpData =
            toml::from_str(LEVEL_XP_PACK).expect("checked-in level XP content parses");
        assert_eq!(
            data.schema_version, LEVEL_XP_SCHEMA_VERSION,
            "checked-in level XP schema must match the loader"
        );
        data
    })
}

pub fn xp_to_next_level(level: u8) -> Option<u32> {
    level_xp_data()
        .level_xp
        .iter()
        .find(|entry| entry.level == level)
        .map(|entry| entry.xp_to_next)
}

pub fn starter_spell_ids(race: u8, class: u8) -> Vec<u32> {
    progression_data()
        .starter_spell
        .iter()
        .filter(|spell| spell.race == race && spell.class == class)
        .map(|spell| spell.spell_id)
        .collect()
}

pub fn starting_actions(race: u8, class: u8) -> impl Iterator<Item = StartingAction> {
    progression_data()
        .action_bar
        .iter()
        .filter(move |action| action.race == race && action.class == class)
        .copied()
}

pub fn trainer_offers(trainer_entry: u32) -> impl Iterator<Item = &'static TrainerSpell> {
    progression_data()
        .trainer_spell
        .iter()
        .filter(move |offer| offer.trainer_entries.contains(&trainer_entry))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainerError {
    NoActiveCharacter,
    TrainerUnavailable,
    WrongClass,
    AlreadyKnown,
    LevelTooLow,
    MissingPrerequisite,
    ConditionNotMet,
    UnsupportedCondition,
    UnsupportedSkill,
    InsufficientFunds,
}

impl fmt::Display for TrainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoActiveCharacter => "no active character",
            Self::TrainerUnavailable => "trainer does not offer that spell",
            Self::WrongClass => "spell is not available to this class",
            Self::AlreadyKnown => "character already knows that spell",
            Self::LevelTooLow => "character does not meet the required level",
            Self::MissingPrerequisite => "character is missing a prerequisite spell",
            Self::ConditionNotMet => "character does not meet the trainer source condition",
            Self::UnsupportedCondition => "trainer spell has an unsupported source condition",
            Self::UnsupportedSkill => "trainer spell requires an unsupported skill",
            Self::InsufficientFunds => "character does not have enough copper",
        })
    }
}

impl std::error::Error for TrainerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_trainer_condition_and_boolean_child_is_converted() {
        let data = spell_condition_data();
        let mut referenced: Vec<u32> = progression_data()
            .trainer_spell
            .iter()
            .filter_map(|offer| (offer.condition_id != 0).then_some(offer.condition_id))
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = referenced.pop() {
            if !seen.insert(id) {
                continue;
            }
            let condition = spell_condition(id).unwrap_or_else(|| {
                panic!("trainer condition {id} and its child references must be imported")
            });
            if matches!(condition.kind, -1 | -2) {
                referenced.extend(
                    [
                        condition.value1,
                        condition.value2,
                        condition.value3,
                        condition.value4,
                    ]
                    .into_iter()
                    .filter(|child| *child != 0),
                );
            }
        }
        assert_eq!(data.condition.len(), 15);
    }

    #[test]
    fn level_xp_pack_has_the_pinned_tbc_thresholds_through_level_70() {
        let data = level_xp_data();
        assert_eq!(data.level_xp.len(), 69);
        assert_eq!(xp_to_next_level(1), Some(400));
        assert_eq!(xp_to_next_level(60), Some(494_000));
        assert_eq!(xp_to_next_level(69), Some(779_700));
        assert_eq!(xp_to_next_level(70), None);
    }
}
