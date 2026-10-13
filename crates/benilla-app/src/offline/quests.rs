use super::*;

use benilla_game_core::quest::{self, QuestDefinition, QuestOfferState, QuestSource};

pub(super) fn quest_npc_entry(core: &GameCore, guid: u64) -> Option<u32> {
    let spawn = imported_spawn_for_guid(guid)?;
    let template = benilla_game_core::content::creature_content().template(spawn.entry)?;
    let interaction_flags = crate::target::cursor_mode::npc_flags::GOSSIP
        | crate::target::cursor_mode::npc_flags::QUESTGIVER;
    if template.npc_flags & interaction_flags == 0 {
        return None;
    }
    let player = core.snapshot(core.active()?)?.position;
    (player.map_id == spawn.map_id).then_some(spawn.entry)
}

/// The target dispatcher already gates an interaction click against the live ECS pose. State-changing
/// quest commands recheck reach against game-core's authoritative saved pose.
pub(super) fn quest_npc_in_range(core: &GameCore, guid: u64) -> Option<u32> {
    let entry = quest_npc_entry(core, guid)?;
    let spawn = imported_spawn_for_guid(guid)?;
    let player = core.snapshot(core.active()?)?.position;
    let dx = player.x - spawn.x;
    let dy = player.y - spawn.y;
    let dz = player.z - spawn.z;
    (dx * dx + dy * dy + dz * dz
        <= OFFLINE_INTERACTION_RANGE_YARDS * OFFLINE_INTERACTION_RANGE_YARDS)
        .then_some(entry)
}

pub(super) fn quest_giver_guid(core: &GameCore, npc_entry: u32) -> Option<u64> {
    let player = core.snapshot(core.active()?)?.position;
    benilla_game_core::content::creature_content()
        .spawns_near(
            player.map_id,
            player.x,
            player.y,
            OFFLINE_CONTENT_RADIUS_YARDS,
        )
        .find(|spawn| spawn.entry == npc_entry)
        .map(|spawn| imported_guid(spawn.entry, spawn.source_guid))
}

/// One nearby spawn of `entry` and the overhead marker it should show right now, or `None` when no
/// spawn is streamed. The offline session pushes this after a quest moves along, mirroring the
/// reference's per-NPC `getDialogStatus` refresh on accept, completion and turn-in.
fn questgiver_status_event(core: &GameCore, entry: u32) -> Option<SessionEvent> {
    let npc = quest_giver_guid(core, entry)?;
    let status = core
        .quest_dialog_status(QuestSource::Creature(entry))
        .ok()
        .flatten()
        .map(quest_status_icon)
        .unwrap_or(benilla_protocol::messages::dialog_status::NONE);
    Some(SessionEvent::QuestGiverStatus { npc, status })
}

/// The marker refresh for every creature on a quest's chain, givers and turn-ins alike, deduped.
/// A giver whose quest is finished elsewhere drops its marker; the turn-in picks it up.
fn quest_chain_status_events(core: &GameCore, definition: &QuestDefinition) -> Vec<SessionEvent> {
    let mut entries: Vec<u32> = definition
        .giver_creature_entries
        .iter()
        .chain(&definition.turnin_creature_entries)
        .copied()
        .collect();
    entries.sort_unstable();
    entries.dedup();
    entries
        .into_iter()
        .filter_map(|entry| questgiver_status_event(core, entry))
        .collect()
}

pub(super) fn quest_status_icon(status: QuestOfferState) -> u32 {
    match status {
        QuestOfferState::Available => benilla_protocol::messages::dialog_status::AVAILABLE,
        QuestOfferState::Incomplete => benilla_protocol::messages::dialog_status::INCOMPLETE,
        QuestOfferState::Rewardable => benilla_protocol::messages::dialog_status::REWARD2,
    }
}

/// The icon a quest *menu* row carries, which is not the overhead marker's icon. CMaNGOS
/// `Player::PrepareQuestMenu` marks a completed, unrewarded quest `DIALOG_STATUS_REWARD_REP` (4),
/// and the client's `0x4e2430`/`0x4e2580` test files a row as active when its icon is 3 or 4. A
/// completed turn-in row sent as the marker's `REWARD2` (7) therefore reads as an *available*
/// quest, so selecting it queries the detail panel the server refuses instead of turning it in.
fn quest_menu_icon(status: QuestOfferState) -> u32 {
    match status {
        QuestOfferState::Available => benilla_protocol::messages::dialog_status::AVAILABLE,
        QuestOfferState::Incomplete => benilla_protocol::messages::dialog_status::INCOMPLETE,
        QuestOfferState::Rewardable => benilla_protocol::messages::dialog_status::REWARD_REP,
    }
}

fn quest_source(core: &GameCore, guid: u64) -> Option<QuestSource> {
    quest_npc_entry(core, guid).map(QuestSource::Creature)
}

fn quest_gossip_rows(core: &GameCore, source: QuestSource) -> Vec<(u32, u32, u32, String)> {
    core.quest_gossip_rows(source)
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            (
                row.quest_id,
                quest_menu_icon(row.icon),
                row.level,
                row.title,
            )
        })
        .collect()
}

fn reward_item(item_entry: u32, count: u32) -> benilla_protocol::messages::QuestRewardItem {
    benilla_protocol::messages::QuestRewardItem {
        item_id: item_entry,
        count,
        display_id: benilla_game_core::quest::item_template(item_entry)
            .map(|item| item.display_id)
            .unwrap_or(0),
    }
}

fn reward_choices(
    definition: &QuestDefinition,
) -> Vec<benilla_protocol::messages::QuestRewardItem> {
    definition
        .reward_choice
        .iter()
        .map(|choice| reward_item(choice.item_entry, choice.count))
        .collect()
}

fn reward_fixed(definition: &QuestDefinition) -> Vec<benilla_protocol::messages::QuestRewardItem> {
    definition
        .reward_item
        .iter()
        .map(|reward| reward_item(reward.item_entry, reward.count))
        .collect()
}

fn required_items(
    definition: &QuestDefinition,
) -> Vec<benilla_protocol::messages::QuestRequiredItem> {
    definition
        .objective
        .iter()
        .filter(|objective| objective.kind == quest::QuestObjectiveKind::Item)
        .map(|objective| reward_item(objective.entry, objective.count))
        .collect()
}

fn reward_spell_id(definition: &QuestDefinition) -> u32 {
    if definition.reward_spell_cast != 0 {
        definition.reward_spell_cast
    } else {
        definition.reward_spell
    }
}

/// Turn objective-credit updates from an interaction into the events the stock quest log and
/// questgiver markers expect.
fn objective_events(
    core: &GameCore,
    updates: Vec<benilla_game_core::CombatUpdate>,
) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    for update in updates {
        let benilla_game_core::CombatUpdate::QuestObjective(update) = update else {
            continue;
        };
        let Some(id) = core.active().cloned() else {
            continue;
        };
        let Some(snapshot) = core.snapshot(&id) else {
            continue;
        };
        events.push(SessionEvent::QuestObjectiveKill {
            quest_id: update.quest_id,
            entry: update.creature_entry,
            count: update.count,
            required: update.required,
        });
        events.push(SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&quest_log_slot_fields_for(snapshot, update.quest_id)),
        });
        if update.complete {
            events.push(SessionEvent::QuestObjectivesComplete {
                quest_id: update.quest_id,
            });
            if let Some(definition) = benilla_game_core::quest::quest_definition(update.quest_id) {
                events.extend(quest_chain_status_events(core, definition));
            }
        }
    }
    events
}

pub(super) fn send_offline_gossip_menu(
    core: &mut GameCore,
    channels: &OfflineChannels,
    buyback: Option<&mut OfflineVendorBuyback>,
    guid: u64,
) {
    let Some(npc_entry) = quest_npc_entry(core, guid) else {
        return;
    };
    // Opening an NPC's gossip is the talk-to interaction the source credits for `SPEAKTO`
    // objectives; the kill path also credits those, as `KilledMonsterCredit` and
    // `TalkedToCreature` both do.
    let updates = core.record_creature_interaction(npc_entry);
    let events = objective_events(core, updates);
    // A trainer or vendor keeps its service panel: the stock gossip options that would offer both
    // services and quests are not converted yet, and the service frame is what the old slice
    // opened. Quest-only NPCs show the quest list directly.
    if let Some(template) = imported_template_for_guid(guid) {
        if template.npc_flags & crate::target::cursor_mode::npc_flags::TRAINER_MASK != 0 {
            send_offline_trainer_list(core, channels, guid, None, None);
            send_gameplay_events(channels, events);
            return;
        }
        if template.npc_flags & crate::target::cursor_mode::npc_flags::VENDOR_MASK != 0 {
            send_offline_vendor_inventory(core, channels, buyback, guid);
            send_gameplay_events(channels, events);
            return;
        }
    }
    let source = QuestSource::Creature(npc_entry);
    let rows = quest_gossip_rows(core, source);
    let gossip = benilla_game_core::quest::gossip_for(npc_entry);
    if gossip.is_none() && rows.is_empty() {
        if !events.is_empty() {
            send_gameplay_events(channels, events);
        }
        return;
    }
    let mut all = events;
    all.push(SessionEvent::GossipMenu {
        npc: guid,
        text_id: gossip.map(|gossip| gossip.text_id).unwrap_or(0),
        options: Vec::new(),
        quests: rows,
    });
    send_gameplay_events(channels, all);
}

pub(super) fn send_offline_npc_text(
    core: &GameCore,
    channels: &OfflineChannels,
    guid: u64,
    text_id: u32,
) {
    let Some(npc_entry) = quest_npc_entry(core, guid) else {
        return;
    };
    let Some(gossip) = benilla_game_core::quest::gossip_for(npc_entry) else {
        return;
    };
    if gossip.text_id != text_id {
        return;
    }
    let mut blocks = vec![
        benilla_protocol::messages::NpcTextBlock::default();
        benilla_protocol::messages::NPC_TEXT_BLOCKS
    ];
    blocks[0] = benilla_protocol::messages::NpcTextBlock {
        probability: gossip.probability,
        male: gossip.male.clone(),
        female: gossip.female.clone(),
    };
    send_gameplay_events(channels, [SessionEvent::NpcGreeting { text_id, blocks }]);
}

pub(super) fn send_offline_questgiver_status(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
) {
    let Some(source) = quest_source(core, npc) else {
        return;
    };
    let status = core
        .quest_dialog_status(source)
        .ok()
        .flatten()
        .map(quest_status_icon)
        .unwrap_or(benilla_protocol::messages::dialog_status::NONE);
    info!(
        "offline questgiver status: entry {} -> {status}",
        source.entry()
    );
    send_gameplay_events(channels, [SessionEvent::QuestGiverStatus { npc, status }]);
}

pub(super) fn send_offline_quest_greeting(core: &GameCore, channels: &OfflineChannels, npc: u64) {
    let Some(npc_entry) = quest_npc_entry(core, npc) else {
        return;
    };
    let quests = quest_gossip_rows(core, QuestSource::Creature(npc_entry))
        .into_iter()
        .map(
            |(quest_id, icon, level, title)| benilla_protocol::messages::QuestListEntry {
                quest_id,
                icon,
                level,
                title,
            },
        )
        .collect();
    let greeting = benilla_game_core::quest::greetings_for(npc_entry)
        .iter()
        .find(|greeting| greeting.kind == 0);
    send_gameplay_events(
        channels,
        [SessionEvent::QuestGreeting(
            benilla_protocol::messages::QuestGiverList {
                npc,
                greeting: greeting.map(|row| row.text.clone()).unwrap_or_default(),
                emote_delay: greeting.map(|row| row.emote_delay).unwrap_or(0),
                emote: greeting.map(|row| row.emote_id).unwrap_or(0),
                quests,
            },
        )],
    );
}

pub(super) fn send_offline_quest_details(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(source) = quest_source(core, npc) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    if !core.quest_gossip_rows(source).is_ok_and(|rows| {
        rows.iter()
            .any(|row| row.quest_id == quest_id && row.icon == QuestOfferState::Available)
    }) {
        return;
    }
    send_gameplay_events(
        channels,
        [SessionEvent::QuestDetail(
            benilla_protocol::messages::QuestDetails {
                npc,
                quest_id,
                title: definition.title.clone(),
                details: definition.details.clone(),
                objectives: definition.objectives_text.clone(),
                auto_finish: 0,
                choices: reward_choices(definition),
                rewards: reward_fixed(definition),
                money: definition.reward_money_copper.max(0),
                reward_spell: reward_spell_id(definition),
            },
        )],
    );
}

fn quest_progress_panel(npc: u64, definition: &QuestDefinition, is_complete: bool) -> SessionEvent {
    SessionEvent::QuestProgress(benilla_protocol::messages::QuestRequestItems {
        npc,
        quest_id: definition.id,
        title: definition.title.clone(),
        request_text: definition.request_items_text.clone(),
        emote: 0,
        close_on_cancel: 0,
        required_money: (-definition.reward_money_copper).max(0) as u32,
        required_items: required_items(definition),
        is_complete,
    })
}

fn quest_reward_panel(npc: u64, definition: &QuestDefinition) -> SessionEvent {
    SessionEvent::QuestOffer(benilla_protocol::messages::QuestOfferReward {
        npc,
        quest_id: definition.id,
        title: definition.title.clone(),
        offer_text: definition.offer_reward_text.clone(),
        auto_finish: 0,
        choices: reward_choices(definition),
        rewards: reward_fixed(definition),
        money: definition.reward_money_copper.max(0),
        quest_flags: definition.quest_flags,
        reward_spell: reward_spell_id(definition),
    })
}

pub(super) fn send_offline_quest_progress(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(source) = quest_source(core, npc) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    let completes = match source {
        QuestSource::Creature(entry) => definition.turnin_creature_entries.contains(&entry),
        QuestSource::GameObject(entry) => definition.turnin_gameobject_entries.contains(&entry),
    };
    if !completes {
        return;
    }
    let Some(journal) = core.active_quest_journal() else {
        return;
    };
    if journal.progress(quest_id).is_none() || journal.is_rewarded(quest_id) {
        return;
    }
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let items: Vec<_> = snapshot
        .inventory
        .iter()
        .chain(&snapshot.equipment)
        .copied()
        .collect();
    let event = if journal.is_complete_with(definition, &items) {
        quest_reward_panel(npc, definition)
    } else {
        quest_progress_panel(npc, definition, false)
    };
    send_gameplay_events(channels, [event]);
}

pub(super) fn send_offline_quest_reward(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(source) = quest_source(core, npc) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    let completes = match source {
        QuestSource::Creature(entry) => definition.turnin_creature_entries.contains(&entry),
        QuestSource::GameObject(entry) => definition.turnin_gameobject_entries.contains(&entry),
    };
    if !completes {
        return;
    }
    let Some(journal) = core.active_quest_journal() else {
        return;
    };
    if journal.progress(quest_id).is_none() || journal.is_rewarded(quest_id) {
        return;
    }
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let items: Vec<_> = snapshot
        .inventory
        .iter()
        .chain(&snapshot.equipment)
        .copied()
        .collect();
    if !journal.is_complete_with(definition, &items) {
        return;
    }
    send_gameplay_events(channels, [quest_reward_panel(npc, definition)]);
}

pub(super) fn send_offline_quest_template(channels: &OfflineChannels, quest_id: u32) {
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    let mut choices = [(0, 0); 6];
    for (slot, choice) in choices.iter_mut().zip(&definition.reward_choice) {
        *slot = (choice.item_entry, choice.count);
    }
    let mut rewards = [(0, 0); 4];
    for (slot, reward) in rewards.iter_mut().zip(&definition.reward_item) {
        *slot = (reward.item_entry, reward.count);
    }
    let objectives = std::array::from_fn(|slot| {
        let mut objective = benilla_protocol::messages::QuestObjective {
            creature_or_go: 0,
            required_count: 0,
            item_id: 0,
            item_count: 0,
            text: definition.objective_text(slot).to_string(),
        };
        for def_objective in definition
            .objective
            .iter()
            .filter(|objective| objective.slot as usize == slot)
        {
            match def_objective.kind {
                quest::QuestObjectiveKind::Item => {
                    objective.item_id = def_objective.entry;
                    objective.item_count = def_objective.count;
                }
                _ => {
                    objective.creature_or_go = def_objective.entry;
                    objective.required_count = def_objective.count;
                }
            }
            if objective.text.is_empty() && !def_objective.text.is_empty() {
                objective.text = def_objective.text.clone();
            }
        }
        objective
    });
    send_gameplay_events(
        channels,
        [SessionEvent::QuestTemplate(Box::new(
            benilla_protocol::messages::QuestTemplate {
                quest_id,
                method: definition.method,
                level: definition.quest_level.max(0) as u32,
                zone_or_sort: definition.zone_or_sort,
                quest_type: definition.quest_type,
                rep_objective_faction: definition.rep_objective_faction,
                rep_objective_value: definition.rep_objective_value.max(0) as u32,
                next_quest_in_chain: definition.next_quest_in_chain,
                money: definition.reward_money_copper,
                money_max_level: definition.reward_money_max_level,
                reward_spell: reward_spell_id(definition),
                src_item_id: definition.src_item_id,
                flags: definition.quest_flags,
                rewards,
                choices,
                point_map_id: definition.point_map_id,
                point_x: definition.point_x,
                point_y: definition.point_y,
                point_opt: definition.point_opt,
                title: definition.title.clone(),
                objectives_text: definition.objectives_text.clone(),
                details: definition.details.clone(),
                end_text: definition.end_text.clone(),
                objectives,
            },
        ))],
    );
}

fn quest_accept_failure(error: benilla_game_core::quest::QuestError) -> Option<SessionEvent> {
    use benilla_game_core::quest::QuestError;
    match error {
        QuestError::WrongRace => Some(SessionEvent::QuestGiverInvalid { reason: 6 }),
        QuestError::LevelTooLow => Some(SessionEvent::QuestGiverInvalid { reason: 1 }),
        QuestError::AlreadyAccepted => Some(SessionEvent::QuestGiverInvalid { reason: 13 }),
        QuestError::QuestLogFull => Some(SessionEvent::QuestLogFull),
        QuestError::PrerequisiteNotMet
        | QuestError::UnsupportedRequirement
        | QuestError::EventInactive
        | QuestError::RepeatableCooldown
        | QuestError::TimedQuestActive => Some(SessionEvent::QuestGiverInvalid { reason: 0 }),
        _ => Some(SessionEvent::QuestGiverInvalid { reason: 0 }),
    }
}

pub(super) fn quest_log_slot_fields_for(
    snapshot: &benilla_game_core::CharacterSnapshot,
    quest_id: u32,
) -> Vec<(u16, u32)> {
    let Some(slot) = snapshot.quest_journal.slot(quest_id) else {
        return Vec::new();
    };
    let Some(progress) = snapshot.quest_journal.progress(quest_id) else {
        return Vec::new();
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return Vec::new();
    };
    let items: Vec<_> = snapshot
        .inventory
        .iter()
        .chain(&snapshot.equipment)
        .copied()
        .collect();
    super::quest_log_slot_fields(
        slot,
        progress,
        definition,
        &items,
        snapshot.quest_journal.is_complete_with(definition, &items),
    )
    .into()
}

pub(super) fn accept_offline_quest(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(source) = quest_source(core, npc) else {
        return;
    };
    if quest_npc_in_range(core, npc).is_none() {
        return;
    }
    match core.accept_quest(source, quest_id) {
        Ok(()) => {
            let Some(id) = core.active().cloned() else {
                return;
            };
            let Some(snapshot) = core.snapshot(&id) else {
                return;
            };
            let mut events = vec![SessionEvent::GossipComplete];
            events.push(SessionEvent::ObjectValues {
                guid: id.presentation_guid(),
                fields: ObjectFields::from_pairs(&super::quest_log_field_pairs(snapshot)),
            });
            if let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) {
                // Recompute every chain marker from the new journal: a giver whose quest is ended
                // elsewhere drops its `!`/`?`, and the turn-in picks up the active row.
                events.extend(quest_chain_status_events(core, definition));
                if definition.is_auto_complete() {
                    events.push(SessionEvent::QuestObjectivesComplete { quest_id });
                }
            }
            send_gameplay_events(channels, events);
            save_after_command(core, persistence);
        }
        Err(error) => {
            if let Some(event) = quest_accept_failure(error) {
                send_gameplay_events(channels, [event]);
            }
        }
    }
}

pub(super) fn reward_offline_quest(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
    choice: u32,
    talents: Option<&crate::ui_talent::Talents>,
) {
    let Some(source) = quest_source(core, npc) else {
        return;
    };
    if quest_npc_in_range(core, npc).is_none() {
        return;
    }
    let reward = match core.turn_in_quest(source, quest_id, choice) {
        Ok(reward) => reward,
        Err(_) => {
            send_gameplay_events(channels, [SessionEvent::QuestGiverInvalid { reason: 0 }]);
            return;
        }
    };
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let mut values = vec![
        (1176, snapshot.money_copper),
        (716, snapshot.experience),
        (
            717,
            benilla_game_core::progression::xp_to_next_level(snapshot.level).unwrap_or(0),
        ),
        (34, u32::from(snapshot.level)),
        (22, snapshot.health),
        (1102, free_talent_points(snapshot, talents)),
    ];
    if matches!(snapshot.appearance.class, 2 | 3 | 5 | 7 | 8 | 9 | 11) {
        values.push((23, snapshot.mana));
    }
    values.extend(super::quest_log_field_pairs(snapshot));
    let mut events = vec![
        SessionEvent::QuestComplete(benilla_protocol::messages::QuestComplete {
            quest_id,
            xp: reward.experience,
            money: reward.money_copper.max(0) as u32,
            items: Vec::new(),
        }),
        SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&values),
        },
    ];
    for item in &reward.items {
        let inventory_slot_field = 532 + 2 * u16::from(item.slot);
        events.push(SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&[
                (
                    inventory_slot_field,
                    inventory_item_guid(snapshot, item) as u32,
                ),
                (
                    inventory_slot_field + 1,
                    (inventory_item_guid(snapshot, item) >> 32) as u32,
                ),
            ]),
        });
        events.push(item_create_event(id.presentation_guid(), snapshot, item));
        if let Some(info) = item_info(item.entry) {
            events.push(SessionEvent::ItemTemplate {
                entry: item.entry,
                info: Some(Box::new(info)),
            });
        }
        events.push(SessionEvent::ItemPushResult(
            benilla_protocol::messages::ItemPushResult {
                player_guid: id.presentation_guid(),
                from_npc: true,
                created: false,
                show_in_chat: true,
                bag_slot: benilla_protocol::messages::BAG_PLAYER_INVENTORY,
                item_slot: u32::from(item.slot),
                item_entry: item.entry,
                suffix_factor: 0,
                random_property_id: 0,
                count: item.count,
            },
        ));
    }
    if !reward.reputation.is_empty() {
        events.push(SessionEvent::ReputationDelta {
            standings: reward.reputation.clone(),
        });
    }
    for spell_id in &reward.learned_spells {
        events.push(SessionEvent::SpellLearned {
            spell_id: *spell_id,
        });
    }
    for (index, reached_level) in
        (reward.level - reward.levels_gained + 1..=reward.level).enumerate()
    {
        events.push(SessionEvent::LevelUp(
            benilla_protocol::messages::LevelUpInfo {
                level: u32::from(reached_level),
                health: if index == 0 {
                    reward.health_gained as i32
                } else {
                    0
                },
                powers: [
                    if index == 0 {
                        reward.mana_gained as i32
                    } else {
                        0
                    },
                    0,
                    0,
                    0,
                    0,
                ],
                stats: [0; 5],
            },
        ));
    }
    if let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) {
        // The turn-in marker clears, and a giver may now offer the next quest in the chain.
        events.extend(quest_chain_status_events(core, definition));
    }
    send_gameplay_events(channels, events);
    save_after_command(core, persistence);
}
