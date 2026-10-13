//! Source-backed quest content and persistent player quest state.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::progression::SourceCondition;

const QUEST_PACK: &str = include_str!("../content/tbc-quests.toml");
const STARTING_GEAR_PACK: &str = include_str!("../content/tbc-starting-gear.toml");
pub const QUEST_SCHEMA_VERSION: u32 = 2;
pub const STARTING_GEAR_SCHEMA_VERSION: u32 = 1;
/// TBC 2.4.3 `MAX_QUEST_LOG_SIZE` (CMaNGOS `Player.h`).
pub const QUEST_LOG_SLOTS: usize = 25;
pub const BACKPACK_SLOTS: u8 = 16;
/// Worn equipment, `INV_SLOT` 0..18 (head through tabard).
pub const EQUIPMENT_SLOTS: u8 = 19;
pub const MAX_REWARD_CHOICES: usize = 6;
pub const MAX_FIXED_REWARDS: usize = 4;
pub const MAX_REWARD_REPS: usize = 5;
/// The project calendar year, in days. Game time counts minutes from a project-owned epoch of
/// 2007-01-01 00:00; the epoch and the fixed 365-day year are project rules (RULES.md).
pub const CALENDAR_YEAR_DAYS: u64 = 365;
pub const MINUTES_PER_DAY: u64 = 1440;

#[derive(Debug, Deserialize)]
pub struct QuestPack {
    pub schema_version: u32,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub conversion_version: u32,
    pub client_build: u32,
    pub extraction_command: String,
    pub quest: Vec<QuestDefinition>,
    #[serde(default)]
    pub gossip: Vec<QuestGossip>,
    #[serde(default)]
    pub greeting: Vec<QuestGreeting>,
    #[serde(default)]
    pub condition: Vec<SourceCondition>,
    #[serde(default)]
    pub game_event: Vec<GameEventDefinition>,
    #[serde(default)]
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
    #[serde(default)]
    pub special_flags: u32,
    pub required_race_mask: u32,
    pub required_class_mask: u32,
    #[serde(default)]
    pub required_skill: u32,
    #[serde(default)]
    pub required_skill_value: u32,
    #[serde(default)]
    pub required_condition: u32,
    #[serde(default)]
    pub required_min_rep_faction: u32,
    #[serde(default)]
    pub required_min_rep_value: i32,
    #[serde(default)]
    pub required_max_rep_faction: u32,
    #[serde(default)]
    pub required_max_rep_value: i32,
    #[serde(default)]
    pub suggested_players: u8,
    #[serde(default)]
    pub limit_time_secs: u32,
    #[serde(default)]
    pub char_title_id: u32,
    #[serde(default)]
    pub prev_quest_id: i32,
    #[serde(default)]
    pub next_quest_id: i32,
    #[serde(default)]
    pub exclusive_group: i32,
    #[serde(default)]
    pub breadcrumb_for_quest_id: u32,
    #[serde(default)]
    pub next_quest_in_chain: u32,
    #[serde(default)]
    pub src_item_id: u32,
    #[serde(default)]
    pub src_item_count: u32,
    #[serde(default)]
    pub src_spell: u32,
    pub details: String,
    pub objectives_text: String,
    pub request_items_text: String,
    pub offer_reward_text: String,
    #[serde(default)]
    pub end_text: String,
    #[serde(default)]
    pub objective_texts: Vec<String>,
    pub reward_money_copper: i32,
    #[serde(default)]
    pub reward_money_max_level: u32,
    #[serde(default)]
    pub reward_spell: u32,
    #[serde(default)]
    pub reward_spell_cast: u32,
    #[serde(default)]
    pub reward_mail_template_id: u32,
    #[serde(default)]
    pub reward_mail_delay_secs: u32,
    #[serde(default)]
    pub reward_honorable_kills: u32,
    #[serde(default)]
    pub point_map_id: u32,
    #[serde(default)]
    pub point_x: f32,
    #[serde(default)]
    pub point_y: f32,
    #[serde(default)]
    pub point_opt: u32,
    #[serde(default)]
    pub rep_objective_faction: u32,
    #[serde(default)]
    pub rep_objective_value: i32,
    #[serde(default)]
    pub giver_creature_entries: Vec<u32>,
    #[serde(default)]
    pub turnin_creature_entries: Vec<u32>,
    #[serde(default)]
    pub giver_gameobject_entries: Vec<u32>,
    #[serde(default)]
    pub turnin_gameobject_entries: Vec<u32>,
    #[serde(default)]
    pub event_ids: Vec<u32>,
    #[serde(default)]
    pub objective: Vec<QuestObjective>,
    #[serde(default)]
    pub reward_choice: Vec<QuestRewardChoice>,
    #[serde(default)]
    pub reward_item: Vec<QuestRewardChoice>,
    #[serde(default)]
    pub reward_rep: Vec<QuestRewardRep>,
}

impl QuestDefinition {
    /// `QUEST_SPECIAL_FLAG_REPEATABLE` (source `SpecialFlags` bit 0).
    pub fn is_repeatable(&self) -> bool {
        self.special_flags & 0x1 != 0
    }

    /// `QUEST_FLAGS_DAILY`.
    pub fn is_daily(&self) -> bool {
        self.quest_flags & 0x1000 != 0
    }

    /// `QUEST_FLAGS_WEEKLY`.
    pub fn is_weekly(&self) -> bool {
        self.quest_flags & 0x8000 != 0
    }

    /// `QUEST_SPECIAL_FLAG_MONTHLY`.
    pub fn is_monthly(&self) -> bool {
        self.special_flags & 0x4 != 0
    }

    /// `QUEST_FLAGS_AUTO_REWARDED`.
    pub fn is_auto_rewarded(&self) -> bool {
        self.quest_flags & 0x400 != 0
    }

    /// `Quest::IsAutoComplete`: `Method` 0.
    pub fn is_auto_complete(&self) -> bool {
        self.method == 0
    }

    /// `QUEST_SPECIAL_FLAG_TIMED`.
    pub fn is_timed(&self) -> bool {
        self.limit_time_secs > 0
    }

    /// The objective text for slot `index`, from the shared `ObjectiveText1..4` columns.
    pub fn objective_text(&self, index: usize) -> &str {
        self.objective_texts
            .get(index)
            .map(String::as_str)
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestObjectiveKind {
    /// A positive `ReqCreatureOrGOId` with no `ReqSpellCast`: kill or speak to the creature.
    CreatureKill,
    /// A positive `ReqCreatureOrGOId` with a `ReqSpellCast`: cast that spell on the creature.
    CreatureSpellcast,
    /// A negative `ReqCreatureOrGOId` with no `ReqSpellCast`: use/activate the gameobject.
    GameobjectUse,
    /// A negative `ReqCreatureOrGOId` with a `ReqSpellCast`: cast that spell on the gameobject.
    GameobjectSpellcast,
    /// A `ReqItemId` count in the backpack or equipment.
    Item,
    /// A `ReqSpellCast` with no target (source `SPELL_EFFECT_QUEST_COMPLETE`/`SEND_EVENT`).
    Spell,
}

#[derive(Debug, Deserialize)]
pub struct QuestObjective {
    pub kind: QuestObjectiveKind,
    /// The source objective slot 0..3. The client's quest packets carry four slots, each holding at
    /// most one creature/gameobject objective and one item objective.
    #[serde(default)]
    pub slot: u32,
    pub entry: u32,
    pub count: u32,
    #[serde(default)]
    pub spell_id: u32,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct QuestRewardChoice {
    pub item_entry: u32,
    pub count: u32,
}

#[derive(Debug, Deserialize)]
pub struct QuestRewardRep {
    pub faction: u32,
    pub value: i32,
    #[serde(default)]
    pub max_value: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuestGossip {
    pub creature_entry: u32,
    pub menu_id: u32,
    pub text_id: u32,
    #[serde(default)]
    pub male: String,
    #[serde(default)]
    pub female: String,
    #[serde(default)]
    pub probability: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuestGreeting {
    pub creature_entry: u32,
    pub kind: u32,
    pub text: String,
    #[serde(default)]
    pub emote_id: u32,
    #[serde(default)]
    pub emote_delay: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameEventDefinition {
    pub entry: u32,
    pub schedule_type: u32,
    pub occurrence_minutes: u32,
    pub length_minutes: u32,
    #[serde(default)]
    pub holiday: u32,
    #[serde(default)]
    pub start_month: u32,
    #[serde(default)]
    pub start_day: u32,
    #[serde(default)]
    pub start_minute: u32,
    #[serde(default)]
    pub description: String,
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
    pub material: i32,
    pub max_durability: u32,
    /// Converted weapon swing numbers (`item_template` `dmg_min1`/`dmg_max1`, rounded to whole
    /// points, and `delay` in ms). Absent or zero on non-weapons and on packs converted before
    /// weapon damage was part of the schema.
    #[serde(default)]
    pub dmg_min: u32,
    #[serde(default)]
    pub dmg_max: u32,
    #[serde(default)]
    pub delay: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuestJournal {
    /// Active quests only: a turn-in removes the entry, as `Player::RewardQuest` clears the log
    /// slot. Rewarded quests live in `rewarded` so a non-repeatable quest cannot be taken twice.
    pub entries: Vec<QuestProgress>,
    /// Rewarded non-repeatable quest ids, sorted. A repeatable quest records only its cooldown.
    #[serde(default)]
    pub rewarded: Vec<u32>,
    /// Last reward time for repeatable daily/weekly/monthly quests, keyed by quest id.
    #[serde(default)]
    pub cooldowns: Vec<QuestCooldown>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestCooldown {
    pub quest_id: u32,
    pub last_rewarded_minutes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestProgress {
    pub quest_id: u32,
    pub objective_counts: Vec<u32>,
    /// Choice index selected at turn-in. `Some` also marks a non-repeatable quest as rewarded.
    pub reward_choice: Option<u32>,
    /// Game-time minute the timed quest fails, for `LimitTime` quests.
    #[serde(default)]
    pub deadline_minutes: Option<u64>,
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
    pub health_gained: u32,
    pub mana_gained: u32,
    pub experience: u32,
    /// Net copper from the turn-in: the positive reward, the max-level money, or the negative
    /// required money.
    pub money_copper: i64,
    pub choice_index: u32,
    pub items: Vec<InventoryItem>,
    /// Applied reputation changes `(faction, delta)`.
    pub reputation: Vec<(u32, i32)>,
    /// Spells the turn-in grants directly (`RewSpell`/`RewSpellCast`).
    pub learned_spells: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestOfferState {
    Available,
    Incomplete,
    Rewardable,
}

/// Where a quest interaction happens: a creature entry or a gameobject entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestSource {
    Creature(u32),
    GameObject(u32),
}

impl QuestSource {
    pub fn entry(self) -> u32 {
        match self {
            Self::Creature(entry) | Self::GameObject(entry) => entry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestGossipRow {
    pub quest_id: u32,
    pub icon: QuestOfferState,
    pub level: u32,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReputationStanding {
    pub faction: u32,
    pub value: i32,
}

/// The character state an availability check reads. Everything is borrowed so the game core can
/// evaluate without cloning the snapshot.
pub struct QuestEvaluation<'a> {
    pub race: u8,
    pub class: u8,
    pub gender: u8,
    pub level: u8,
    pub journal: &'a QuestJournal,
    pub inventory: &'a [InventoryItem],
    pub money_copper: u32,
    pub known_spell_ids: &'a [u32],
    pub aura_spell_ids: &'a [u32],
    pub reputation: &'a [ReputationStanding],
    pub game_time_minutes: u64,
}

/// Standing toward one faction, or zero when no standing is recorded.
pub fn reputation_standing(reputation: &[ReputationStanding], faction: u32) -> i32 {
    reputation
        .iter()
        .find(|standing| standing.faction == faction)
        .map(|standing| standing.value)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestAvailability {
    Available,
    Incomplete,
    Rewardable,
    Unavailable(QuestError),
}

impl QuestAvailability {
    pub fn offer_state(self) -> Option<QuestOfferState> {
        match self {
            Self::Available => Some(QuestOfferState::Available),
            Self::Incomplete => Some(QuestOfferState::Incomplete),
            Self::Rewardable => Some(QuestOfferState::Rewardable),
            Self::Unavailable(_) => None,
        }
    }
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
    /// A source requirement the offline simulation does not model (skill, instance script, world
    /// script, area, or an unknown condition kind).
    UnsupportedRequirement,
    /// A converted prerequisite condition, reputation, previous quest, breadcrumb, exclusive
    /// group, or chain check failed.
    PrerequisiteNotMet,
    /// The quest's seasonal event is not active at the current game time.
    EventInactive,
    /// A repeatable daily/weekly/monthly quest is inside its reset window.
    RepeatableCooldown,
    /// Another timed quest is already active (`QUEST_SPECIAL_FLAG_TIMED`).
    TimedQuestActive,
    /// The character lacks the required money or items for the turn-in.
    TurnInRequirement,
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
            Self::UnsupportedRequirement => {
                "the quest has a source requirement the offline simulation does not model"
            }
            Self::PrerequisiteNotMet => "quest prerequisites are not met",
            Self::EventInactive => "the quest's seasonal event is not active",
            Self::RepeatableCooldown => "the quest's daily, weekly or monthly reset has not passed",
            Self::TimedQuestActive => "another timed quest is already active",
            Self::TurnInRequirement => "the turn-in money or items are missing",
        })
    }
}

impl std::error::Error for QuestError {}

struct QuestIndex {
    quests: HashMap<u32, &'static QuestDefinition>,
    giver_creatures: HashMap<u32, Vec<u32>>,
    turnin_creatures: HashMap<u32, Vec<u32>>,
    giver_gameobjects: HashMap<u32, Vec<u32>>,
    turnin_gameobjects: HashMap<u32, Vec<u32>>,
    gossip: HashMap<u32, &'static QuestGossip>,
    greetings: HashMap<u32, Vec<&'static QuestGreeting>>,
    conditions: HashMap<u32, &'static SourceCondition>,
    events: HashMap<u32, &'static GameEventDefinition>,
    /// Signed previous-quest ids per quest: positive requires the quest rewarded, negative
    /// requires it active. Built the way CMaNGOS `ObjectMgr::LoadQuests` builds `prevQuests`.
    prev_quests: HashMap<u32, Vec<i32>>,
    /// Quests whose `NextQuestInChain` names this quest; any of them active blocks acceptance.
    prev_chain_quests: HashMap<u32, Vec<u32>>,
    /// Breadcrumb quests whose `BreadcrumbForQuestId` names this quest.
    dependent_breadcrumbs: HashMap<u32, Vec<u32>>,
    /// Positive `ExclusiveGroup` → member quest ids.
    exclusive_groups: HashMap<i32, Vec<u32>>,
    items: HashMap<u32, &'static QuestItemTemplate>,
}

fn quest_index() -> &'static QuestIndex {
    static INDEX: OnceLock<QuestIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let pack = quest_pack();
        let mut quests = HashMap::with_capacity(pack.quest.len());
        let mut giver_creatures: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut turnin_creatures: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut giver_gameobjects: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut turnin_gameobjects: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut prev_quests: HashMap<u32, Vec<i32>> = HashMap::new();
        let mut prev_chain_quests: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut dependent_breadcrumbs: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut exclusive_groups: HashMap<i32, Vec<u32>> = HashMap::new();
        for quest in &pack.quest {
            quests.insert(quest.id, quest);
            for &entry in &quest.giver_creature_entries {
                giver_creatures.entry(entry).or_default().push(quest.id);
            }
            for &entry in &quest.turnin_creature_entries {
                turnin_creatures.entry(entry).or_default().push(quest.id);
            }
            for &entry in &quest.giver_gameobject_entries {
                giver_gameobjects.entry(entry).or_default().push(quest.id);
            }
            for &entry in &quest.turnin_gameobject_entries {
                turnin_gameobjects.entry(entry).or_default().push(quest.id);
            }
            if quest.exclusive_group > 0 {
                exclusive_groups
                    .entry(quest.exclusive_group)
                    .or_default()
                    .push(quest.id);
            }
        }
        for quest in &pack.quest {
            if quest.prev_quest_id != 0 {
                prev_quests
                    .entry(quest.id)
                    .or_default()
                    .push(quest.prev_quest_id);
            }
            if quest.next_quest_id != 0 {
                let signed = if quest.next_quest_id < 0 {
                    -(quest.id as i32)
                } else {
                    quest.id as i32
                };
                prev_quests
                    .entry(quest.next_quest_id.unsigned_abs())
                    .or_default()
                    .push(signed);
            }
            if quest.next_quest_in_chain != 0 {
                prev_chain_quests
                    .entry(quest.next_quest_in_chain)
                    .or_default()
                    .push(quest.id);
            }
            if quest.breadcrumb_for_quest_id != 0 {
                dependent_breadcrumbs
                    .entry(quest.breadcrumb_for_quest_id)
                    .or_default()
                    .push(quest.id);
            }
        }
        let mut gossip = HashMap::with_capacity(pack.gossip.len());
        for row in &pack.gossip {
            gossip.insert(row.creature_entry, row);
        }
        let mut greetings: HashMap<u32, Vec<&QuestGreeting>> = HashMap::new();
        for row in &pack.greeting {
            greetings.entry(row.creature_entry).or_default().push(row);
        }
        let mut conditions = HashMap::with_capacity(pack.condition.len());
        for condition in &pack.condition {
            conditions.insert(condition.id, condition);
        }
        let mut events = HashMap::with_capacity(pack.game_event.len());
        for event in &pack.game_event {
            events.insert(event.entry, event);
        }
        let mut items: HashMap<u32, &QuestItemTemplate> = HashMap::new();
        for item in pack
            .item_template
            .iter()
            .chain(starting_gear_pack().item_template.iter())
            .chain(crate::loot::loot_data().item_template.iter())
            .chain(crate::vendor::vendor_pack().item_template.iter())
        {
            items.entry(item.entry).or_insert(item);
        }
        QuestIndex {
            quests,
            giver_creatures,
            turnin_creatures,
            giver_gameobjects,
            turnin_gameobjects,
            gossip,
            greetings,
            conditions,
            events,
            prev_quests,
            prev_chain_quests,
            dependent_breadcrumbs,
            exclusive_groups,
            items,
        }
    })
}

pub fn race_allowed(definition: &QuestDefinition, race: u8) -> bool {
    definition.required_race_mask == 0
        || ((1..=32).contains(&race) && definition.required_race_mask & (1u32 << (race - 1)) != 0)
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
            toml::from_str(QUEST_PACK).expect("checked-in TBC quest content must parse");
        assert_eq!(pack.schema_version, QUEST_SCHEMA_VERSION);
        assert_eq!(pack.client_build, 8606);
        pack
    })
}

pub fn quest_definition(quest_id: u32) -> Option<&'static QuestDefinition> {
    quest_index().quests.get(&quest_id).copied()
}

pub fn item_template(entry: u32) -> Option<&'static QuestItemTemplate> {
    quest_index().items.get(&entry).copied()
}

pub fn gossip_for(creature_entry: u32) -> Option<&'static QuestGossip> {
    quest_index().gossip.get(&creature_entry).copied()
}

pub fn greetings_for(creature_entry: u32) -> &'static [&'static QuestGreeting] {
    quest_index()
        .greetings
        .get(&creature_entry)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn game_event(entry: u32) -> Option<&'static GameEventDefinition> {
    quest_index().events.get(&entry).copied()
}

/// The quest ids a creature offers, in content order.
pub fn creature_giver_quests(entry: u32) -> &'static [u32] {
    quest_index()
        .giver_creatures
        .get(&entry)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn creature_turnin_quests(entry: u32) -> &'static [u32] {
    quest_index()
        .turnin_creatures
        .get(&entry)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn gameobject_giver_quests(entry: u32) -> &'static [u32] {
    quest_index()
        .giver_gameobjects
        .get(&entry)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn gameobject_turnin_quests(entry: u32) -> &'static [u32] {
    quest_index()
        .turnin_gameobjects
        .get(&entry)
        .map(Vec::as_slice)
        .unwrap_or(&[])
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
pub fn starting_items(
    race: u8,
    class: u8,
    gender: u8,
) -> impl Iterator<Item = &'static StartingItem> {
    starting_gear_pack()
        .starting_item
        .iter()
        .filter(move |item| item.race == race && item.class == class && item.gender == gender)
}

/// Day serial of the project calendar: game time counts minutes from 2007-01-01 00:00.
pub fn calendar_day(game_time_minutes: u64) -> u64 {
    game_time_minutes / MINUTES_PER_DAY
}

/// Week serial of the project calendar: seven-day weeks from the epoch day.
pub fn calendar_week(game_time_minutes: u64) -> u64 {
    calendar_day(game_time_minutes) / 7
}

/// Month (0-based) of the project calendar: a fixed 365-day year with Gregorian month lengths.
pub fn calendar_month(game_time_minutes: u64) -> u64 {
    let day = calendar_day(game_time_minutes) % CALENDAR_YEAR_DAYS;
    let mut remaining = day;
    for (index, length) in MONTH_LENGTHS.iter().enumerate() {
        if remaining < *length {
            return index as u64;
        }
        remaining -= length;
    }
    MONTH_LENGTHS.len() as u64 - 1
}

const MONTH_LENGTHS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Zero-based day of the project calendar year for a source month/day (1-based).
fn calendar_day_of_year(month: u32, day: u32) -> u64 {
    if !(1..=12).contains(&month) || day == 0 {
        return 0;
    }
    let mut offset = 0;
    for length in MONTH_LENGTHS.iter().take(month as usize - 1) {
        offset += length;
    }
    offset + (day as u64 - 1).min(MONTH_LENGTHS[month as usize - 1] - 1)
}

/// Whether a converted game event is inside its window at the given game time.
///
/// Project rule (RULES.md): server-side events (`schedule_type` 0) are active whenever they have a
/// nonzero length — the scripts that would start them are out of scope, and a zero-length event is
/// inactive. Date and yearly events (`schedule_type` 1 and 11) recur from their converted
/// `game_event_time` month/day/minute in the fixed 365-day project year; an occurrence of a year or
/// more is treated as annual. Other schedule types (Darkmoon Faire, lunar new year, Easter) are
/// not used by any quest and read inactive.
pub fn game_event_active(event: &GameEventDefinition, game_time_minutes: u64) -> bool {
    if event.length_minutes == 0 {
        return false;
    }
    match event.schedule_type {
        0 => true,
        1 | 11 => {
            let day = calendar_day(game_time_minutes) % CALENDAR_YEAR_DAYS;
            let minute_of_day = game_time_minutes % MINUTES_PER_DAY;
            let now = day * MINUTES_PER_DAY + minute_of_day;
            let anchor = calendar_day_of_year(event.start_month, event.start_day) * MINUTES_PER_DAY
                + u64::from(event.start_minute);
            let span = CALENDAR_YEAR_DAYS * MINUTES_PER_DAY;
            let length = u64::from(event.length_minutes);
            let occurrence = u64::from(event.occurrence_minutes.max(1));
            if occurrence >= span {
                if anchor + length <= span {
                    now >= anchor && now < anchor + length
                } else {
                    now >= anchor || now < anchor + length - span
                }
            } else {
                (now + span * occurrence - anchor % occurrence) % occurrence < length
            }
        }
        _ => false,
    }
}

/// Whether every event a quest is attached to is active. A quest with no event association is
/// always active, as in CMaNGOS `ObjectMgr::LoadGameEvents`.
pub fn quest_event_active(definition: &QuestDefinition, game_time_minutes: u64) -> bool {
    definition.event_ids.is_empty()
        || definition.event_ids.iter().any(|entry| {
            game_event(*entry).is_some_and(|event| game_event_active(event, game_time_minutes))
        })
}

/// `Quest::XPValue` from the pinned CMaNGOS TBC core, in project terms: the source stores the
/// max-level money in `RewMoneyMaxLevel`, and the quest XP is derived from it by quest level with
/// a falloff once the player is more than five levels above the quest.
pub fn quest_reward_xp(definition: &QuestDefinition, player_level: u8) -> u32 {
    if definition.reward_money_max_level == 0 {
        return 0;
    }
    let quest_level = definition.quest_level.max(0) as u32;
    let full_xp = match quest_level {
        65.. => definition.reward_money_max_level as f64 / 6.0,
        64 => definition.reward_money_max_level as f64 / 4.8,
        63 => definition.reward_money_max_level as f64 / 3.6,
        62 => definition.reward_money_max_level as f64 / 2.4,
        61 => definition.reward_money_max_level as f64 / 1.2,
        1..=60 => definition.reward_money_max_level as f64 / 0.6,
        0 => 0.0,
    };
    let level = player_level as u32;
    let scale = if level <= quest_level + 5 {
        1.0
    } else if level == quest_level + 6 {
        0.8
    } else if level == quest_level + 7 {
        0.6
    } else if level == quest_level + 8 {
        0.4
    } else if level == quest_level + 9 {
        0.2
    } else {
        0.1
    };
    (full_xp * scale).ceil() as u32
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
        progress
            .counts(definition)
            .iter()
            .zip(&definition.objective)
            .all(|(&count, objective)| count >= objective.count)
    }

    /// Completeness including item objectives read from the current inventory.
    pub fn is_complete_with(
        &self,
        definition: &QuestDefinition,
        inventory: &[InventoryItem],
    ) -> bool {
        let Some(progress) = self.progress(definition.id) else {
            return false;
        };
        progress
            .effective_counts(definition, inventory)
            .iter()
            .zip(&definition.objective)
            .all(|(&count, objective)| count >= objective.count)
    }

    pub fn cooldown(&self, quest_id: u32) -> Option<u64> {
        self.cooldowns
            .iter()
            .find(|cooldown| cooldown.quest_id == quest_id)
            .map(|cooldown| cooldown.last_rewarded_minutes)
    }

    pub fn is_rewarded(&self, quest_id: u32) -> bool {
        self.rewarded.binary_search(&quest_id).is_ok()
    }

    /// Whether a repeatable daily/weekly/monthly quest's reset has passed.
    pub fn cooldown_ok(&self, definition: &QuestDefinition, now_minutes: u64) -> bool {
        let Some(last) = self.cooldown(definition.id) else {
            return true;
        };
        if definition.is_daily() && calendar_day(last) == calendar_day(now_minutes) {
            return false;
        }
        if definition.is_weekly() && calendar_week(last) == calendar_week(now_minutes) {
            return false;
        }
        if definition.is_monthly() && calendar_month(last) == calendar_month(now_minutes) {
            return false;
        }
        true
    }

    /// Record a turn-in: a repeatable quest leaves no entry so it can be taken again, a
    /// non-repeatable one joins the rewarded set, and a daily/weekly/monthly cycle stamps its
    /// reset clock.
    pub fn record_reward(&mut self, definition: &QuestDefinition, now_minutes: u64) {
        let cycles = definition.is_repeatable()
            || definition.is_daily()
            || definition.is_weekly()
            || definition.is_monthly();
        self.entries
            .retain(|progress| progress.quest_id != definition.id);
        if cycles {
            if definition.is_daily() || definition.is_weekly() || definition.is_monthly() {
                self.cooldowns
                    .retain(|cooldown| cooldown.quest_id != definition.id);
                let index = self
                    .cooldowns
                    .binary_search_by_key(&definition.id, |cooldown| cooldown.quest_id)
                    .unwrap_or_else(|index| index);
                self.cooldowns.insert(
                    index,
                    QuestCooldown {
                        quest_id: definition.id,
                        last_rewarded_minutes: now_minutes,
                    },
                );
            }
        } else if !self.is_rewarded(definition.id) {
            let index = self
                .rewarded
                .binary_search(&definition.id)
                .unwrap_or_else(|index| index);
            self.rewarded.insert(index, definition.id);
        }
    }
}

impl QuestProgress {
    /// Stored counts for non-item objectives; item objectives are read from the inventory.
    pub fn counts(&self, definition: &QuestDefinition) -> Vec<u32> {
        definition
            .objective
            .iter()
            .enumerate()
            .map(|(index, objective)| match objective.kind {
                QuestObjectiveKind::Item => 0,
                _ => self.objective_counts.get(index).copied().unwrap_or(0),
            })
            .collect()
    }

    pub fn effective_counts(
        &self,
        definition: &QuestDefinition,
        inventory: &[InventoryItem],
    ) -> Vec<u32> {
        definition
            .objective
            .iter()
            .enumerate()
            .map(|(index, objective)| match objective.kind {
                QuestObjectiveKind::Item => {
                    inventory_count(inventory, objective.entry).min(objective.count)
                }
                _ => self.objective_counts.get(index).copied().unwrap_or(0),
            })
            .collect()
    }

    /// The four per-slot counts the client's quest log packs: for each slot, the creature/gameobject
    /// count or, when the slot has only an item objective, the item count.
    pub fn slot_counts(
        &self,
        definition: &QuestDefinition,
        inventory: &[InventoryItem],
    ) -> [u32; 4] {
        let mut counts = [0u32; 4];
        for (index, objective) in definition.objective.iter().enumerate() {
            let slot = objective.slot.min(3) as usize;
            let count = match objective.kind {
                QuestObjectiveKind::Item => {
                    inventory_count(inventory, objective.entry).min(objective.count)
                }
                _ => self.objective_counts.get(index).copied().unwrap_or(0),
            };
            if count > counts[slot] {
                counts[slot] = count;
            }
        }
        counts
    }
}

/// Total count of an item across the backpack and equipment.
pub fn inventory_count(inventory: &[InventoryItem], entry: u32) -> u32 {
    inventory
        .iter()
        .filter(|item| item.entry == entry)
        .map(|item| item.count)
        .sum()
}

/// A race's team mask, as the source `CONDITION_TEAM` values (469 Alliance, 67 Horde).
fn team_mask(race: u8) -> u32 {
    match race {
        1 | 3 | 4 | 7 | 11 => 469,
        2 | 5 | 6 | 8 | 10 => 67,
        _ => 0,
    }
}

fn condition_met(condition_id: u32, evaluation: &QuestEvaluation, depth: u32) -> bool {
    if condition_id == 0 {
        return true;
    }
    if depth > 8 {
        return false;
    }
    let Some(condition) = quest_index().conditions.get(&condition_id).copied() else {
        return false;
    };
    let value = |index: usize| -> u32 {
        [
            condition.value1,
            condition.value2,
            condition.value3,
            condition.value4,
        ][index]
    };
    let mut result = match condition.kind {
        -1 => {
            condition_met(value(0), evaluation, depth + 1)
                && condition_met(value(1), evaluation, depth + 1)
        }
        -2 => {
            condition_met(value(0), evaluation, depth + 1)
                || condition_met(value(1), evaluation, depth + 1)
        }
        -3 => !condition_met(value(0), evaluation, depth + 1),
        1 => evaluation.aura_spell_ids.contains(&value(0)),
        2 | 23 => inventory_count(evaluation.inventory, value(0)) >= value(1),
        6 => team_mask(evaluation.race) & value(0) != 0,
        8 => evaluation.journal.is_rewarded(value(0)),
        9 => evaluation
            .journal
            .progress(value(0))
            .is_some_and(|_| match value(1) {
                1 => !quest_definition(value(0)).is_some_and(|definition| {
                    evaluation
                        .journal
                        .is_complete_with(definition, evaluation.inventory)
                }),
                2 => quest_definition(value(0)).is_some_and(|definition| {
                    evaluation
                        .journal
                        .is_complete_with(definition, evaluation.inventory)
                }),
                _ => true,
            }),
        12 => quest_index()
            .events
            .get(&value(0))
            .is_some_and(|event| game_event_active(event, evaluation.game_time_minutes)),
        14 => {
            let race_mask = value(0);
            let class_mask = value(1);
            (race_mask == 0 || race_allowed_mask(race_mask, evaluation.race))
                && (class_mask == 0 || class_allowed_mask(class_mask, evaluation.class))
        }
        15 => {
            let level = evaluation.level as i32;
            match value(1) {
                1 => level >= value(0) as i32,
                2 => level <= value(0) as i32,
                _ => level == value(0) as i32,
            }
        }
        17 => evaluation.known_spell_ids.contains(&value(0)) == (value(1) == 0),
        19 => quest_definition(value(0))
            .is_some_and(|definition| quest_acceptance(definition, evaluation, depth + 1).is_ok()),
        22 => evaluation.journal.progress(value(0)).is_none(),
        26 => quest_index().events.values().any(|event| {
            event.holiday == value(0) && game_event_active(event, evaluation.game_time_minutes)
        }),
        35 => u32::from(evaluation.gender) == value(0),
        5 => {
            reputation_standing(evaluation.reputation, value(0))
                >= reputation_rank_threshold(value(1))
        }
        30 => {
            reputation_standing(evaluation.reputation, value(0))
                < reputation_rank_threshold(value(1))
        }
        _ => false,
    };
    if condition.flags & 0x1 != 0 {
        result = !result;
    }
    result
}

fn race_allowed_mask(mask: u32, race: u8) -> bool {
    (1..=32).contains(&race) && mask & (1u32 << (race - 1)) != 0
}

fn class_allowed_mask(mask: u32, class: u8) -> bool {
    (1..=32).contains(&class) && mask & (1u32 << (class - 1)) != 0
}

/// The source reputation ranks (`REPUTATION_RANK_*`) as standing thresholds.
fn reputation_rank_threshold(rank: u32) -> i32 {
    match rank {
        0 => -42_000,
        1 => -6_000,
        2 => -3_000,
        3 => 0,
        4 => 3_000,
        5 => 9_000,
        6 => 21_000,
        7 => 42_000,
        _ => i32::MIN,
    }
}

fn quest_rewarded(journal: &QuestJournal, quest_id: u32) -> bool {
    journal.is_rewarded(quest_id)
}

fn quest_active(journal: &QuestJournal, quest_id: u32) -> bool {
    journal.progress(quest_id).is_some()
}

fn previous_quests_met(definition: &QuestDefinition, evaluation: &QuestEvaluation) -> bool {
    let Some(entries) = quest_index().prev_quests.get(&definition.id) else {
        return true;
    };
    entries.iter().any(|&signed| {
        let quest_id = signed.unsigned_abs();
        if signed > 0 {
            quest_rewarded(evaluation.journal, quest_id)
        } else {
            quest_active(evaluation.journal, quest_id)
        }
    })
}

fn prev_chain_met(definition: &QuestDefinition, evaluation: &QuestEvaluation) -> bool {
    let Some(entries) = quest_index().prev_chain_quests.get(&definition.id) else {
        return true;
    };
    !entries
        .iter()
        .any(|quest_id| quest_active(evaluation.journal, *quest_id))
}

fn next_chain_met(definition: &QuestDefinition, evaluation: &QuestEvaluation) -> bool {
    definition.next_quest_in_chain == 0
        || !quest_active(evaluation.journal, definition.next_quest_in_chain)
}

fn breadcrumb_met(definition: &QuestDefinition, evaluation: &QuestEvaluation, depth: u32) -> bool {
    if definition.breadcrumb_for_quest_id == 0 {
        return true;
    }
    quest_definition(definition.breadcrumb_for_quest_id)
        .is_some_and(|target| quest_acceptance(target, evaluation, depth + 1).is_ok())
}

fn dependent_breadcrumbs_met(definition: &QuestDefinition, evaluation: &QuestEvaluation) -> bool {
    let Some(entries) = quest_index().dependent_breadcrumbs.get(&definition.id) else {
        return true;
    };
    !entries
        .iter()
        .any(|quest_id| evaluation.journal.progress(*quest_id).is_some())
}

fn exclusive_group_met(definition: &QuestDefinition, evaluation: &QuestEvaluation) -> bool {
    if definition.exclusive_group <= 0 {
        return true;
    }
    let Some(members) = quest_index()
        .exclusive_groups
        .get(&definition.exclusive_group)
    else {
        return true;
    };
    !members
        .iter()
        .filter(|quest_id| **quest_id != definition.id)
        .any(|quest_id| quest_active(evaluation.journal, *quest_id))
}

/// The acceptance prerequisites CMaNGOS `Player::CanTakeQuest` checks, in its order.
fn quest_acceptance(
    definition: &QuestDefinition,
    evaluation: &QuestEvaluation,
    depth: u32,
) -> Result<(), QuestError> {
    if evaluation.journal.progress(definition.id).is_some() {
        return Err(QuestError::AlreadyAccepted);
    }
    if evaluation.journal.is_rewarded(definition.id) {
        return Err(QuestError::AlreadyRewarded);
    }
    if !race_allowed(definition, evaluation.race) {
        return Err(QuestError::WrongRace);
    }
    if !class_allowed(definition, evaluation.class) {
        return Err(QuestError::WrongClass);
    }
    if evaluation.level < definition.min_level {
        return Err(QuestError::LevelTooLow);
    }
    if definition.max_level != u8::MAX && evaluation.level > definition.max_level {
        return Err(QuestError::LevelTooHigh);
    }
    if definition.required_skill != 0 {
        return Err(QuestError::UnsupportedRequirement);
    }
    if definition.required_condition != 0
        && !condition_met(definition.required_condition, evaluation, depth)
    {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if definition.required_min_rep_faction != 0 {
        let reputation =
            reputation_standing(evaluation.reputation, definition.required_min_rep_faction);
        if reputation < definition.required_min_rep_value {
            return Err(QuestError::PrerequisiteNotMet);
        }
    }
    if definition.required_max_rep_faction != 0 {
        let reputation =
            reputation_standing(evaluation.reputation, definition.required_max_rep_faction);
        if reputation >= definition.required_max_rep_value {
            return Err(QuestError::PrerequisiteNotMet);
        }
    }
    if !previous_quests_met(definition, evaluation) {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if !prev_chain_met(definition, evaluation) {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if !next_chain_met(definition, evaluation) {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if !breadcrumb_met(definition, evaluation, depth) {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if !dependent_breadcrumbs_met(definition, evaluation) {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if !exclusive_group_met(definition, evaluation) {
        return Err(QuestError::PrerequisiteNotMet);
    }
    if !quest_event_active(definition, evaluation.game_time_minutes) {
        return Err(QuestError::EventInactive);
    }
    if !evaluation
        .journal
        .cooldown_ok(definition, evaluation.game_time_minutes)
    {
        return Err(QuestError::RepeatableCooldown);
    }
    if definition.is_timed()
        && evaluation
            .journal
            .entries
            .iter()
            .any(|progress| progress.deadline_minutes.is_some())
    {
        return Err(QuestError::TimedQuestActive);
    }
    Ok(())
}

/// Evaluate a quest against one NPC's role and the character state. `role` is how the NPC is
/// related to the quest: only a giver can start it, while either side can show an active quest.
pub fn quest_availability(
    definition: &QuestDefinition,
    role: QuestNpcRole,
    evaluation: &QuestEvaluation,
) -> QuestAvailability {
    if evaluation.journal.is_rewarded(definition.id) {
        if definition.is_repeatable()
            && evaluation
                .journal
                .cooldown_ok(definition, evaluation.game_time_minutes)
        {
            return QuestAvailability::Available;
        }
        return QuestAvailability::Unavailable(QuestError::AlreadyRewarded);
    }
    if let Some(_progress) = evaluation.journal.progress(definition.id) {
        return if evaluation
            .journal
            .is_complete_with(definition, evaluation.inventory)
        {
            QuestAvailability::Rewardable
        } else {
            QuestAvailability::Incomplete
        };
    }
    if role == QuestNpcRole::TurnIn {
        return QuestAvailability::Unavailable(QuestError::WrongNpc);
    }
    match quest_acceptance(definition, evaluation, 0) {
        Ok(()) => QuestAvailability::Available,
        Err(error) => QuestAvailability::Unavailable(error),
    }
}

/// Whether the character may accept the quest right now (CMaNGOS `Player::CanTakeQuest`).
pub fn can_accept(
    definition: &QuestDefinition,
    evaluation: &QuestEvaluation,
) -> Result<(), QuestError> {
    quest_acceptance(definition, evaluation, 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestNpcRole {
    Giver,
    TurnIn,
}

fn push_gossip_row(rows: &mut Vec<QuestGossipRow>, quest_id: u32, icon: QuestOfferState) {
    if rows.iter().any(|row| row.quest_id == quest_id) {
        return;
    }
    let Some(definition) = quest_definition(quest_id) else {
        return;
    };
    rows.push(QuestGossipRow {
        quest_id,
        icon,
        level: definition.quest_level.max(0) as u32,
        title: definition.title.clone(),
    });
}

/// The gossip rows one creature or gameobject shows for the active character, sorted by quest id.
///
/// This mirrors CMaNGOS `Player::PrepareQuestMenu`: the *involved* (turn-in) relations contribute
/// only a quest already in the log, while the quest-giver relations contribute only a quest the
/// character does not yet have. A giver who merely started a quest that is turned in elsewhere
/// therefore shows no row for it, while the turn-in shows the active row.
pub fn gossip_rows(source: QuestSource, evaluation: &QuestEvaluation) -> Vec<QuestGossipRow> {
    let (givers, turnins) = match source {
        QuestSource::Creature(entry) => {
            (creature_giver_quests(entry), creature_turnin_quests(entry))
        }
        QuestSource::GameObject(entry) => (
            gameobject_giver_quests(entry),
            gameobject_turnin_quests(entry),
        ),
    };
    let mut rows = Vec::new();
    // The turn-in relations first: an active quest the character can finish at this NPC.
    for &quest_id in turnins {
        let Some(definition) = quest_definition(quest_id) else {
            continue;
        };
        if let Some(icon) =
            quest_availability(definition, QuestNpcRole::TurnIn, evaluation).offer_state()
        {
            push_gossip_row(&mut rows, quest_id, icon);
        }
    }
    // The giver relations: only a quest not yet taken, as the reference's `QUEST_STATUS_NONE`.
    for &quest_id in givers {
        if evaluation.journal.progress(quest_id).is_some() {
            continue;
        }
        let Some(definition) = quest_definition(quest_id) else {
            continue;
        };
        if let Some(icon) =
            quest_availability(definition, QuestNpcRole::Giver, evaluation).offer_state()
        {
            push_gossip_row(&mut rows, quest_id, icon);
        }
    }
    rows.sort_by_key(|row| row.quest_id);
    rows
}

/// Whether the saved journal is internally consistent. Active entries are never rewarded; a
/// turn-in moves a non-repeatable quest to the rewarded set and clears a repeatable one.
pub fn valid_journal(journal: &QuestJournal) -> bool {
    if journal.entries.len() > QUEST_LOG_SLOTS
        || !journal
            .entries
            .windows(2)
            .all(|pair| pair[0].quest_id < pair[1].quest_id)
        || !journal
            .cooldowns
            .windows(2)
            .all(|pair| pair[0].quest_id < pair[1].quest_id)
        || !journal.rewarded.windows(2).all(|pair| pair[0] < pair[1])
    {
        return false;
    }
    journal.entries.iter().all(|progress| {
        let Some(definition) = quest_definition(progress.quest_id) else {
            return false;
        };
        if progress.objective_counts.len() != definition.objective.len()
            || progress.reward_choice.is_some()
        {
            return false;
        }
        for (objective, &count) in definition.objective.iter().zip(&progress.objective_counts) {
            if count > objective.count {
                return false;
            }
        }
        if progress.deadline_minutes.is_some() && !definition.is_timed() {
            return false;
        }
        true
    }) && journal
        .rewarded
        .iter()
        .all(|quest_id| quest_definition(*quest_id).is_some())
        && journal
            .cooldowns
            .iter()
            .all(|cooldown| quest_definition(cooldown.quest_id).is_some())
}

/// Migrate a journal parsed from a pre-reward-set save (schema 9 and older), where a rewarded quest
/// stayed in `entries` with `reward_choice` set.
pub fn normalize_journal(journal: &mut QuestJournal) {
    let mut moved: Vec<u32> = journal
        .entries
        .iter()
        .filter(|progress| progress.reward_choice.is_some())
        .map(|progress| progress.quest_id)
        .collect();
    journal
        .entries
        .retain(|progress| progress.reward_choice.is_none());
    if !moved.is_empty() {
        journal.rewarded.append(&mut moved);
        journal.rewarded.sort_unstable();
        journal.rewarded.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_evaluation(journal: &QuestJournal) -> QuestEvaluation<'_> {
        static EMPTY_REPUTATION: [ReputationStanding; 0] = [];
        QuestEvaluation {
            race: 10,
            class: 2,
            gender: 1,
            level: 1,
            journal,
            inventory: &[],
            money_copper: 0,
            known_spell_ids: &[],
            aura_spell_ids: &[],
            reputation: &EMPTY_REPUTATION,
            game_time_minutes: 0,
        }
    }

    #[test]
    fn checked_in_quest_pack_is_source_backed_and_indexed() {
        let pack = quest_pack();
        assert_eq!(pack.source, "https://github.com/cmangos/tbc-db");
        assert_eq!(pack.revision, "866723612d93791e442fb1b18d642118fa682edd");
        assert_eq!(pack.license, "GPL-3.0");
        assert_eq!(pack.quest.len(), 6599);
        assert_eq!(pack.game_event.len(), 153);
        assert!(pack.condition.len() >= 250);
        assert!(pack.item_template.len() >= 6_800);

        let quest = quest_definition(8325).expect("Reclaiming Sunstrider Isle");
        assert_eq!(quest.title, "Reclaiming Sunstrider Isle");
        assert_eq!(quest.giver_creature_entries, [15278]);
        assert_eq!(quest.turnin_creature_entries, [15278]);
        assert_eq!(quest.objective.len(), 1);
        assert_eq!(quest.objective[0].kind, QuestObjectiveKind::CreatureKill);
        assert_eq!(quest.objective[0].entry, 15274);
        assert_eq!(quest.objective[0].count, 8);
        assert_eq!(quest.reward_choice.len(), 2);
        assert_eq!(quest.reward_money_copper, 30);
        assert_eq!(quest_reward_xp(quest, 1), 100);
        assert_eq!(quest.reward_rep.len(), 1);
        assert_eq!(quest.reward_rep[0].faction, 911);
        for choice in &quest.reward_choice {
            let item = item_template(choice.item_entry).expect("reward item template");
            assert_eq!(item.entry, choice.item_entry);
        }

        // The chain and the gossip table resolve through the index.
        let second = quest_definition(8326).expect("Unfortunate Measures");
        assert_eq!(second.prev_quest_id, 8325);
        assert_eq!(second.next_quest_in_chain, 8327);
        let gossip = gossip_for(15278).expect("Erona's gossip menu");
        assert_eq!(gossip.text_id, 7781);
        assert!(gossip.female.contains("Sunstrider Isle"));

        assert!(creature_giver_quests(15278).contains(&8325));
        assert!(creature_turnin_quests(15278).contains(&8325));
        assert!(valid_journal(&QuestJournal::default()));
    }

    #[test]
    fn chain_availability_requires_the_previous_quest_rewarded() {
        let first = quest_definition(8325).expect("first quest");
        let second = quest_definition(8326).expect("second quest");
        let mut journal = QuestJournal::default();
        let evaluation = empty_evaluation(&journal);
        assert_eq!(
            quest_availability(first, QuestNpcRole::Giver, &evaluation),
            QuestAvailability::Available
        );
        assert_eq!(
            quest_availability(second, QuestNpcRole::Giver, &evaluation),
            QuestAvailability::Unavailable(QuestError::PrerequisiteNotMet)
        );

        journal.entries.push(QuestProgress {
            quest_id: 8325,
            objective_counts: vec![8],
            reward_choice: None,
            deadline_minutes: None,
        });
        journal.record_reward(first, 0);
        let evaluation = empty_evaluation(&journal);
        assert_eq!(
            quest_availability(second, QuestNpcRole::Giver, &evaluation),
            QuestAvailability::Available
        );
        assert_eq!(
            quest_availability(first, QuestNpcRole::Giver, &evaluation),
            QuestAvailability::Unavailable(QuestError::AlreadyRewarded)
        );
    }

    #[test]
    fn a_giver_that_only_starts_the_quest_shows_no_row_for_it() {
        // Quest 4641 "Your Place In The World": Kaltunk (10176) gives it and Gornek (3143) turns
        // it in. Accepting must drop Kaltunk's marker and raise Gornek's, as CMaNGOS
        // `Player::PrepareQuestMenu` reads the giver relations only for a quest not yet taken.
        let definition = quest_definition(4641).expect("Your Place In The World");
        assert_eq!(definition.giver_creature_entries, [10176]);
        assert_eq!(definition.turnin_creature_entries, [3143]);

        let mut journal = QuestJournal::default();
        journal.entries.push(QuestProgress {
            quest_id: 4641,
            objective_counts: vec![0; definition.objective.len()],
            reward_choice: None,
            deadline_minutes: None,
        });
        let evaluation = empty_evaluation(&journal);

        let giver = gossip_rows(QuestSource::Creature(10176), &evaluation);
        assert!(
            giver.iter().all(|row| row.quest_id != 4641),
            "the giver must not mark a quest it only started"
        );
        let turnin = gossip_rows(QuestSource::Creature(3143), &evaluation);
        assert!(
            turnin
                .iter()
                .any(|row| row.quest_id == 4641 && row.icon != QuestOfferState::Available),
            "the turn-in shows the active row"
        );
    }

    #[test]
    fn rewarding_the_first_chain_quest_offers_only_the_next_link() {
        // Quests 456 and 457 are both "The Balance of Nature", both given and turned in by
        // Conservator Ilthalaine (2079), with 456 chaining to 457. Rewarding 456 must stop offering
        // 456 and start offering 457 — the shared title is not a retake.
        let first = quest_definition(456).expect("The Balance of Nature (first)");
        assert_eq!(first.title, "The Balance of Nature");
        assert!(!first.is_repeatable());
        let mut journal = QuestJournal::default();
        // Before either is taken, only the first links is gated in: 457 requires 456 rewarded.
        let before = gossip_rows(QuestSource::Creature(2079), &empty_evaluation(&journal));
        assert!(before.iter().any(|row| row.quest_id == 456));
        assert!(
            !before.iter().any(|row| row.quest_id == 457),
            "the follow-up is not offered before its prerequisite: {before:?}"
        );

        journal.record_reward(first, 0);
        let evaluation = empty_evaluation(&journal);
        let rows = gossip_rows(QuestSource::Creature(2079), &evaluation);
        assert!(
            !rows.iter().any(|row| row.quest_id == 456),
            "the rewarded quest must not be offered again: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.quest_id == 457 && row.icon == QuestOfferState::Available),
            "the next link must be offered: {rows:?}"
        );

        // The last link is not repeatable either: rewarding it empties the list.
        let second = quest_definition(457).expect("The Balance of Nature (second)");
        journal.record_reward(second, 0);
        let after = gossip_rows(QuestSource::Creature(2079), &empty_evaluation(&journal));
        assert!(
            !after.iter().any(|row| row.quest_id == 457),
            "the last link must not be offered again: {after:?}"
        );
    }

    #[test]
    fn item_objectives_read_the_live_inventory() {
        // Quest 8330 asks for Solanian's three belongings; every objective is an item count.
        let definition = quest_definition(8330).expect("Solanian's Belongings");
        let objectives: Vec<_> = definition
            .objective
            .iter()
            .filter(|objective| objective.kind == QuestObjectiveKind::Item)
            .collect();
        assert_eq!(objectives.len(), 3);
        let mut journal = QuestJournal::default();
        journal.entries.push(QuestProgress {
            quest_id: definition.id,
            objective_counts: vec![0; definition.objective.len()],
            reward_choice: None,
            deadline_minutes: None,
        });
        assert!(!journal.is_complete_with(definition, &[]));
        let inventory: Vec<_> = objectives
            .iter()
            .enumerate()
            .map(|(index, objective)| InventoryItem {
                instance_id: index as u64 + 1,
                entry: objective.entry,
                count: objective.count,
                slot: index as u8,
            })
            .collect();
        assert!(journal.is_complete_with(definition, &inventory));
    }

    #[test]
    fn seasonal_event_windows_follow_the_project_calendar() {
        let midsummer = game_event(1).expect("Midsummer Fire Festival");
        assert_eq!(midsummer.start_month, 6);
        assert_eq!(midsummer.start_day, 21);
        // 2007-01-01 noon: inactive; June 21 02:00: active; July 10: inactive again.
        assert!(!game_event_active(midsummer, 12 * 60));
        let june_21 = calendar_day_of_year(6, 21) * MINUTES_PER_DAY + 120;
        assert!(game_event_active(midsummer, june_21));
        assert!(game_event_active(
            midsummer,
            june_21 + u64::from(midsummer.length_minutes) - 1
        ));
        assert!(!game_event_active(
            midsummer,
            june_21 + u64::from(midsummer.length_minutes)
        ));
        // The window recurs the next project year.
        assert!(game_event_active(
            midsummer,
            june_21 + CALENDAR_YEAR_DAYS * MINUTES_PER_DAY
        ));
    }

    #[test]
    fn daily_cooldowns_reset_with_the_project_calendar() {
        let definition = quest_pack()
            .quest
            .iter()
            .find(|quest| quest.is_daily())
            .expect("the pack has daily quests");
        let mut journal = QuestJournal::default();
        journal.record_reward(definition, 12 * 60);
        assert!(!journal.cooldown_ok(definition, 13 * 60));
        assert!(journal.cooldown_ok(definition, 12 * 60 + MINUTES_PER_DAY));
    }
}
