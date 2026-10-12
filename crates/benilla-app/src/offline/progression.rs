use super::*;

pub(super) fn free_talent_points(
    snapshot: &benilla_game_core::CharacterSnapshot,
    talents: Option<&crate::ui_talent::Talents>,
) -> u32 {
    let earned = u32::from(snapshot.level.saturating_sub(9));
    if earned == 0 {
        return 0;
    }
    let Some(talents) = talents else { return 0 };
    let talent_spells: std::collections::BTreeSet<u32> = talents
        .catalog
        .tabs_for_class(snapshot.appearance.race, snapshot.appearance.class)
        .into_iter()
        .flat_map(|tab| talents.catalog.talents_in_tab(tab.id))
        .flat_map(|talent| talent.ranks)
        .filter(|spell_id| *spell_id != 0)
        .collect();
    let spent = snapshot
        .known_spell_ids
        .iter()
        .filter(|spell_id| talent_spells.contains(spell_id))
        .count() as u32;
    earned.saturating_sub(spent)
}

pub(super) fn starter_action_buttons(
    race: u8,
    class: u8,
) -> Vec<benilla_protocol::messages::ActionButton> {
    benilla_game_core::progression::starting_actions(race, class)
        .map(|action| benilla_protocol::messages::ActionButton {
            slot: action.button,
            action: action.action,
            kind: action.action_type,
        })
        .collect()
}

pub(super) fn send_offline_trainer_list(
    core: &GameCore,
    channels: &OfflineChannels,
    trainer: u64,
    spells: Option<&crate::ui_action::Spells>,
    skill_lines: Option<&crate::ui_spellbook::SkillLines>,
) {
    let Some(trainer_entry) = benilla_protocol::guid::entry(trainer) else {
        return;
    };
    let services = core
        .trainer_services_with_ranks(
            trainer_entry,
            |spell_id| {
                spells
                    .and_then(|spells| spells.catalog.learned_spell(spell_id))
                    .unwrap_or(spell_id)
            },
            |spell_id| skill_lines.and_then(|lines| lines.catalog.rank_successor(spell_id)),
        )
        .unwrap_or_default()
        .into_iter()
        .map(|service| {
            let offer = service.offer;
            let mut req_spells = [0; 3];
            for (slot, spell_id) in req_spells.iter_mut().zip(&offer.prerequisite_spell_ids) {
                *slot = *spell_id;
            }
            let state = match service.state {
                benilla_game_core::TrainerServiceState::Learnable => {
                    benilla_protocol::messages::trainer_spell_state::GREEN
                }
                benilla_game_core::TrainerServiceState::Known => {
                    benilla_protocol::messages::trainer_spell_state::GRAY
                }
                benilla_game_core::TrainerServiceState::Unavailable(_) => {
                    benilla_protocol::messages::trainer_spell_state::RED
                }
            };
            benilla_protocol::messages::TrainerSpell {
                spell: offer.spell_id,
                state,
                cost: offer.cost_copper,
                can_learn_primary_prof: false,
                is_primary_prof_first_rank: false,
                req_level: offer.required_level,
                req_skill: offer.required_skill_id,
                req_skill_value: offer.required_skill_value,
                req_spells,
            }
        })
        .collect();
    send_gameplay_events(
        channels,
        [SessionEvent::TrainerList {
            trainer,
            trainer_type: 0,
            services,
            greeting: String::new(),
        }],
    );
}

pub(super) fn buy_offline_trainer_spell(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    trainer: u64,
    spell_id: u32,
    spells: Option<&crate::ui_action::Spells>,
    skill_lines: Option<&crate::ui_spellbook::SkillLines>,
) {
    let Some(trainer_entry) = benilla_protocol::guid::entry(trainer) else {
        send_gameplay_events(
            channels,
            [SessionEvent::TrainerBuyFailed {
                trainer,
                spell_id,
                error: benilla_protocol::messages::train_fail::UNAVAILABLE,
            }],
        );
        return;
    };
    match core.buy_trainer_spell_with_ranks(
        trainer_entry,
        spell_id,
        |spell_id| {
            spells
                .and_then(|spells| spells.catalog.learned_spell(spell_id))
                .unwrap_or(spell_id)
        },
        |spell_id| skill_lines.and_then(|lines| lines.catalog.rank_successor(spell_id)),
    ) {
        Ok(purchase) => {
            let Some(id) = core.active() else { return };
            let mut events = vec![SessionEvent::TrainerBuySucceeded { trainer, spell_id }];
            events.extend(
                purchase
                    .removed_spell_ids
                    .into_iter()
                    .map(|spell_id| SessionEvent::SpellRemoved { spell_id }),
            );
            events.push(SessionEvent::SpellLearned {
                spell_id: purchase.learned_spell_id,
            });
            events.push(SessionEvent::ObjectValues {
                guid: id.presentation_guid(),
                fields: ObjectFields::from_pairs(&[(1176, purchase.remaining_copper)]),
            });
            send_gameplay_events(channels, events);
            save_after_command(core, persistence);
        }
        Err(error) => {
            let code = match error {
                benilla_game_core::progression::TrainerError::InsufficientFunds => {
                    benilla_protocol::messages::train_fail::NOT_ENOUGH_MONEY
                }
                benilla_game_core::progression::TrainerError::UnsupportedSkill => {
                    benilla_protocol::messages::train_fail::NOT_ENOUGH_SKILL
                }
                _ => benilla_protocol::messages::train_fail::UNAVAILABLE,
            };
            send_gameplay_events(
                channels,
                [SessionEvent::TrainerBuyFailed {
                    trainer,
                    spell_id,
                    error: code,
                }],
            );
        }
    }
}

pub(super) fn learn_offline_talent(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    talents: Option<&crate::ui_talent::Talents>,
    skill_lines: Option<&crate::ui_spellbook::SkillLines>,
    talent_id: u32,
    rank: u32,
) {
    let Some(talents) = talents else {
        warn!("offline gameplay: talent catalog is unavailable");
        return;
    };
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let appearance = snapshot.appearance;
    let Some((tab, talent)) = talents
        .catalog
        .tabs_for_class(appearance.race, appearance.class)
        .into_iter()
        .find_map(|tab| {
            talents
                .catalog
                .talents_in_tab(tab.id)
                .iter()
                .find(|talent| talent.id == talent_id)
                .map(|talent| (tab, talent))
        })
    else {
        warn!("offline gameplay: talent {talent_id} is not available to this race/class");
        return;
    };
    let rank_spell_ids: Vec<_> = talent
        .ranks
        .iter()
        .copied()
        .take_while(|spell_id| *spell_id != 0)
        .collect();
    let prerequisite_spell_id = if talent.prereq_talent == 0 {
        0
    } else {
        talents
            .catalog
            .talent(tab.id, talent.prereq_talent)
            .and_then(|prerequisite| prerequisite.ranks.get(talent.prereq_rank as usize).copied())
            .unwrap_or(0)
    };
    let tree_spell_ids: Vec<_> = talents
        .catalog
        .talents_in_tab(tab.id)
        .iter()
        .flat_map(|talent| talent.ranks)
        .filter(|spell_id| *spell_id != 0)
        .collect();
    let class_spell_ids: Vec<_> = talents
        .catalog
        .tabs_for_class(appearance.race, appearance.class)
        .into_iter()
        .flat_map(|tab| talents.catalog.talents_in_tab(tab.id))
        .flat_map(|talent| talent.ranks)
        .filter(|spell_id| *spell_id != 0)
        .collect();
    let required_spell_id = if talent.required_spell == 0
        || snapshot.known_spell_ids.contains(&talent.required_spell)
        || skill_lines.is_some_and(|lines| {
            lines.catalog.higher_rank_known(
                talent.required_spell,
                &snapshot.known_spell_ids.iter().copied().collect(),
            )
        }) {
        0
    } else {
        talent.required_spell
    };
    match core.learn_talent(benilla_game_core::TalentGrantRules {
        race_mask: tab.race_mask,
        class_mask: tab.class_mask,
        class_spell_ids: &class_spell_ids,
        tree_spell_ids: &tree_spell_ids,
        rank_spell_ids: &rank_spell_ids,
        rank,
        tier: talent.row,
        required_spell_id,
        prerequisite_spell_id,
    }) {
        Ok(purchase) => {
            let mut events: Vec<_> = purchase
                .learned_spell_ids
                .into_iter()
                .map(|spell_id| SessionEvent::SpellLearned { spell_id })
                .collect();
            if let Some(id) = core.active() {
                events.push(SessionEvent::ObjectValues {
                    guid: id.presentation_guid(),
                    fields: ObjectFields::from_pairs(&[(1102, purchase.remaining_points)]),
                });
            }
            send_gameplay_events(channels, events);
            save_after_command(core, persistence);
        }
        Err(error) => warn!("offline gameplay: rejected talent {talent_id} rank {rank}: {error:?}"),
    }
}
