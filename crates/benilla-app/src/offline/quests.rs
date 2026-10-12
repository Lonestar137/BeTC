use super::*;

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

pub(super) fn quest_status_icon(status: benilla_game_core::quest::QuestOfferState) -> u32 {
    match status {
        benilla_game_core::quest::QuestOfferState::Available => {
            benilla_protocol::messages::dialog_status::AVAILABLE
        }
        benilla_game_core::quest::QuestOfferState::Incomplete => {
            benilla_protocol::messages::dialog_status::INCOMPLETE
        }
        benilla_game_core::quest::QuestOfferState::Rewardable => {
            benilla_protocol::messages::dialog_status::REWARD2
        }
    }
}

fn quest_gossip_rows(core: &GameCore, npc_entry: u32) -> Vec<(u32, u32, u32, String)> {
    core.quest_gossip_rows(npc_entry)
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            (
                row.quest_id,
                quest_status_icon(row.icon),
                row.level,
                row.title,
            )
        })
        .collect()
}

pub(super) fn send_offline_gossip_menu(
    core: &GameCore,
    channels: &OfflineChannels,
    buyback: Option<&mut OfflineVendorBuyback>,
    guid: u64,
) {
    let Some(npc_entry) = quest_npc_entry(core, guid) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_pack()
        .quest
        .iter()
        .find(|quest| quest.giver_creature_entries.contains(&npc_entry))
    else {
        // The static slice has no converted gossip-menu option rows. Preserve useful services
        // behind the GOSSIP bit: trainers open their converted trainer list, and vendors fall
        // through to stock rather than opening an empty frame.
        if let Some(template) = imported_template_for_guid(guid) {
            if template.npc_flags & crate::target::cursor_mode::npc_flags::TRAINER_MASK != 0 {
                send_offline_trainer_list(core, channels, guid, None, None);
            } else if template.npc_flags & crate::target::cursor_mode::npc_flags::VENDOR_MASK != 0 {
                send_offline_vendor_inventory(core, channels, buyback, guid);
            }
        }
        return;
    };
    send_gameplay_events(
        channels,
        [SessionEvent::GossipMenu {
            npc: guid,
            text_id: definition.gossip_text_id,
            options: Vec::new(),
            quests: quest_gossip_rows(core, npc_entry),
        }],
    );
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
    let Some(definition) = benilla_game_core::quest::quest_pack()
        .quest
        .iter()
        .find(|quest| {
            quest.giver_creature_entries.contains(&npc_entry) && quest.gossip_text_id == text_id
        })
    else {
        return;
    };
    let mut blocks = vec![
        benilla_protocol::messages::NpcTextBlock::default();
        benilla_protocol::messages::NPC_TEXT_BLOCKS
    ];
    blocks[0] = benilla_protocol::messages::NpcTextBlock {
        probability: definition.gossip_text_probability,
        male: definition.gossip_text_male.clone(),
        female: definition.gossip_text_female.clone(),
    };
    send_gameplay_events(channels, [SessionEvent::NpcGreeting { text_id, blocks }]);
}

pub(super) fn send_offline_questgiver_status(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
) {
    let Some(spawn) = imported_spawn_for_guid(npc) else {
        return;
    };
    let status = core
        .quest_dialog_status(spawn.entry)
        .ok()
        .flatten()
        .map(quest_status_icon)
        .unwrap_or(benilla_protocol::messages::dialog_status::NONE);
    info!(
        "offline questgiver status: entry {} -> {status}",
        spawn.entry
    );
    send_gameplay_events(channels, [SessionEvent::QuestGiverStatus { npc, status }]);
}

pub(super) fn send_offline_quest_greeting(core: &GameCore, channels: &OfflineChannels, npc: u64) {
    let Some(npc_entry) = quest_npc_entry(core, npc) else {
        return;
    };
    let quests = quest_gossip_rows(core, npc_entry)
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
    send_gameplay_events(
        channels,
        [SessionEvent::QuestGreeting(
            benilla_protocol::messages::QuestGiverList {
                npc,
                greeting: String::new(),
                emote_delay: 0,
                emote: 0,
                quests,
            },
        )],
    );
}

fn quest_reward_items(
    definition: &benilla_game_core::quest::QuestDefinition,
) -> Vec<benilla_protocol::messages::QuestRewardItem> {
    definition
        .reward_choice
        .iter()
        .map(|choice| benilla_protocol::messages::QuestRewardItem {
            item_id: choice.item_entry,
            count: choice.count,
            display_id: choice.display_id,
        })
        .collect()
}

pub(super) fn send_offline_quest_details(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(npc_entry) = quest_npc_entry(core, npc) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    if !definition.giver_creature_entries.contains(&npc_entry)
        || !core.quest_gossip_rows(npc_entry).is_ok_and(|rows| {
            rows.iter().any(|row| {
                row.quest_id == quest_id
                    && row.icon == benilla_game_core::quest::QuestOfferState::Available
            })
        })
    {
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
                choices: quest_reward_items(definition),
                rewards: Vec::new(),
                money: definition.reward_money_copper as i32,
                reward_spell: 0,
            },
        )],
    );
}

fn quest_progress_panel(
    npc: u64,
    definition: &benilla_game_core::quest::QuestDefinition,
    is_complete: bool,
) -> SessionEvent {
    SessionEvent::QuestProgress(benilla_protocol::messages::QuestRequestItems {
        npc,
        quest_id: definition.id,
        title: definition.title.clone(),
        request_text: definition.request_items_text.clone(),
        emote: 0,
        close_on_cancel: 0,
        required_money: 0,
        required_items: Vec::new(),
        is_complete,
    })
}

fn quest_reward_panel(
    npc: u64,
    definition: &benilla_game_core::quest::QuestDefinition,
) -> SessionEvent {
    SessionEvent::QuestOffer(benilla_protocol::messages::QuestOfferReward {
        npc,
        quest_id: definition.id,
        title: definition.title.clone(),
        offer_text: definition.offer_reward_text.clone(),
        auto_finish: 0,
        choices: quest_reward_items(definition),
        rewards: Vec::new(),
        money: definition.reward_money_copper as i32,
        quest_flags: definition.quest_flags,
        reward_spell: 0,
    })
}

pub(super) fn send_offline_quest_progress(
    core: &GameCore,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(npc_entry) = quest_npc_entry(core, npc) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    if !definition.turnin_creature_entries.contains(&npc_entry) {
        return;
    }
    let Some(journal) = core.active_quest_journal() else {
        return;
    };
    let Some(progress) = journal.progress(quest_id) else {
        return;
    };
    if progress.reward_choice.is_some() {
        return;
    }
    let event = if journal.is_complete(quest_id) {
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
    let Some(npc_entry) = quest_npc_entry(core, npc) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_definition(quest_id) else {
        return;
    };
    if !definition.turnin_creature_entries.contains(&npc_entry)
        || !core.active_quest_journal().is_some_and(|journal| {
            journal.is_complete(quest_id)
                && journal
                    .progress(quest_id)
                    .is_some_and(|progress| progress.reward_choice.is_none())
        })
    {
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
    let objectives = std::array::from_fn(|index| {
        definition
            .objective
            .get(index)
            .map(|objective| benilla_protocol::messages::QuestObjective {
                creature_or_go: objective.creature_entry,
                required_count: objective.count,
                item_id: 0,
                item_count: 0,
                text: objective.text.clone(),
            })
            .unwrap_or(benilla_protocol::messages::QuestObjective {
                creature_or_go: 0,
                required_count: 0,
                item_id: 0,
                item_count: 0,
                text: String::new(),
            })
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
                rep_objective_faction: 0,
                rep_objective_value: 0,
                next_quest_in_chain: definition.next_quest_in_chain,
                money: definition.reward_money_copper as i32,
                money_max_level: definition.reward_money_max_level,
                reward_spell: 0,
                src_item_id: definition.source_item_entry,
                flags: definition.quest_flags,
                rewards: [(0, 0); 4],
                choices,
                point_map_id: 0,
                point_x: 0.0,
                point_y: 0.0,
                point_opt: 0,
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
    match error {
        benilla_game_core::quest::QuestError::WrongRace => {
            Some(SessionEvent::QuestGiverInvalid { reason: 6 })
        }
        benilla_game_core::quest::QuestError::LevelTooLow => {
            Some(SessionEvent::QuestGiverInvalid { reason: 1 })
        }
        benilla_game_core::quest::QuestError::AlreadyAccepted => {
            Some(SessionEvent::QuestGiverInvalid { reason: 13 })
        }
        benilla_game_core::quest::QuestError::QuestLogFull => Some(SessionEvent::QuestLogFull),
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
    super::quest_log_slot_fields(slot, progress, snapshot.quest_journal.is_complete(quest_id))
        .into()
}

pub(super) fn accept_offline_quest(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    npc: u64,
    quest_id: u32,
) {
    let Some(npc_entry) = quest_npc_in_range(core, npc) else {
        return;
    };
    match core.accept_quest(npc_entry, quest_id) {
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
            if let Some(giver) = quest_giver_guid(core, npc_entry) {
                events.push(SessionEvent::QuestGiverStatus {
                    npc: giver,
                    status: benilla_protocol::messages::dialog_status::INCOMPLETE,
                });
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
    let Some(npc_entry) = quest_npc_in_range(core, npc) else {
        return;
    };
    let reward = match core.turn_in_quest(npc_entry, quest_id, choice) {
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
    let item_guid = inventory_item_guid(snapshot, &reward.item);
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
    values.extend(quest_log_slot_fields_for(snapshot, quest_id));
    let inventory_slot_field = 532 + 2 * u16::from(reward.item.slot);
    values.extend([
        (inventory_slot_field, item_guid as u32),
        (inventory_slot_field + 1, (item_guid >> 32) as u32),
    ]);
    let mut events = vec![
        SessionEvent::QuestComplete(benilla_protocol::messages::QuestComplete {
            quest_id,
            xp: reward.experience,
            money: reward.money_copper,
            items: Vec::new(),
        }),
        SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&values),
        },
        item_create_event(id.presentation_guid(), snapshot, &reward.item),
    ];
    if let Some(info) = item_info(reward.item.entry) {
        events.push(SessionEvent::ItemTemplate {
            entry: reward.item.entry,
            info: Some(Box::new(info)),
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
    events.push(SessionEvent::ItemPushResult(
        benilla_protocol::messages::ItemPushResult {
            player_guid: id.presentation_guid(),
            from_npc: true,
            created: false,
            show_in_chat: true,
            bag_slot: benilla_protocol::messages::BAG_PLAYER_INVENTORY,
            item_slot: u32::from(reward.item.slot),
            item_entry: reward.item.entry,
            suffix_factor: 0,
            random_property_id: 0,
            count: reward.item.count,
        },
    ));
    if let Some(giver) = quest_giver_guid(core, npc_entry) {
        events.push(SessionEvent::QuestGiverStatus {
            npc: giver,
            status: benilla_protocol::messages::dialog_status::NONE,
        });
    }
    send_gameplay_events(channels, events);
    save_after_command(core, persistence);
}
