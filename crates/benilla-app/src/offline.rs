//! Offline startup adapter: owns the game-core instance without introducing transport behavior.

use std::collections::BTreeMap;
use std::path::Path;

use benilla_game_core::{
    decode_profile, decode_snapshot, Appearance, CharacterAura, CharacterId, CombatUpdate,
    CreatureAiUpdate, GameCore, HostileState, SaveError, WorldPosition,
};
use benilla_protocol::messages::{buy_result, sell_result};
use benilla_protocol::{CharAction, CharCreateReq, EntityKind, ObjectFields, SessionEvent};
use bevy::prelude::*;

use crate::local_state;
use crate::net::{CharActionResultMessage, CharListMessage, CharRequest, OfflineChannels};

mod persistence;
use persistence::OfflinePersistence;
mod progression;
use progression::{
    buy_offline_trainer_spell, free_talent_points, learn_offline_talent, send_offline_trainer_list,
    starter_action_buttons,
};
mod quests;
use quests::{
    accept_offline_quest, quest_giver_guid, quest_log_slot_fields_for, reward_offline_quest,
    send_offline_gossip_menu, send_offline_npc_text, send_offline_quest_details,
    send_offline_quest_greeting, send_offline_quest_progress, send_offline_quest_reward,
    send_offline_quest_template, send_offline_questgiver_status,
};
#[cfg(test)]
use quests::{quest_npc_entry, quest_npc_in_range, quest_status_icon};
mod smoke;
use smoke::{
    drive_offline_smoke_cast, drive_offline_smoke_walk, enter_offline_smoke_character,
    exit_after_roster_boot, exit_after_world_smoke,
};
mod vendor;
use vendor::{
    buy_offline_vendor_item, buyback_offline_item, imported_template_for_guid, sell_offline_item,
    send_offline_vendor_inventory, OfflineVendorBuyback,
};
#[cfg(test)]
use vendor::{BUYBACK_SLOT_FIRST, VENDOR_STOCK_UNLIMITED};

const OFFLINE_CONTENT_RADIUS_YARDS: f32 = benilla_game_core::CONTENT_RADIUS_YARDS;
const OFFLINE_CREATURE_WALK_SPEED_YARDS_PER_SEC: f32 = 2.5;
const OFFLINE_CREATURE_AI_STEP_MS: u32 = 200;
/// NPC interaction range in yards: matches the client's 5.5556 yd service range
/// (`crate::target::cursor_mode::SERVICE_RANGE_SQ` = 30.864).
const OFFLINE_INTERACTION_RANGE_YARDS: f32 = 5.5556;

#[derive(Resource, Default)]
struct OfflinePatrols(BTreeMap<u64, ActivePatrol>);

/// The presentation GUIDs streamed into the session so far (world entry included), so a travel
/// re-query emits only what is new. Project decision: spawns stream in, nothing streams out.
#[derive(Resource, Default)]
struct OfflineStreamedSpawns(std::collections::HashSet<u64>);

/// Re-run the population query once the player has travelled this far, rather than every frame.
const OFFLINE_STREAM_STEP_YARDS: f32 = 20.0;

struct ActivePatrol {
    waypoints: Vec<benilla_game_core::content::CreatureWaypoint>,
    waypoint_index: usize,
    leg_start: [f32; 3],
    position: [f32; 3],
    current_position: [f32; 3],
    leg_duration_ms: u32,
    wait_ms: u32,
    remaining_ms: u32,
    spline_id: u32,
}

#[derive(Resource, Default)]
pub(crate) struct OfflineGameCore(pub(crate) GameCore);

pub(crate) struct OfflineGamePlugin;

impl Plugin for OfflineGamePlugin {
    fn build(&self, app: &mut App) {
        let (core, writable) = match load_core() {
            Ok(Some(core)) => (core, true),
            Ok(None) => (GameCore::default(), true),
            Err(error) => {
                error!("offline save load failed; persistence disabled: {error}");
                (GameCore::default(), false)
            }
        };
        app.insert_resource(OfflineGameCore(core))
            .insert_resource(OfflinePersistence::new(writable))
            .init_resource::<crate::body_shape::BodyProportionRoster>()
            .init_resource::<OfflinePatrols>()
            .init_resource::<OfflineStreamedSpawns>()
            .init_resource::<OfflineVendorBuyback>()
            .add_systems(Startup, publish_local_roster)
            .add_systems(Startup, publish_body_proportions)
            .add_systems(
                Update,
                drive_local_requests
                    .in_set(benilla_world::schedule::WorldStage::Net)
                    .before(crate::net::apply_net_updates),
            )
            .add_systems(
                Update,
                publish_body_proportions
                    .in_set(benilla_world::schedule::WorldStage::Net)
                    .after(drive_local_requests)
                    .before(crate::net::apply_net_updates),
            )
            .add_systems(
                Update,
                drive_offline_gameplay
                    .in_set(benilla_world::schedule::WorldStage::Net)
                    .after(drive_local_requests)
                    .before(crate::net::apply_net_updates),
            )
            .add_systems(
                Update,
                advance_offline_patrols
                    .in_set(benilla_world::schedule::WorldStage::Net)
                    .after(drive_offline_gameplay)
                    .before(crate::net::apply_net_updates),
            )
            .add_systems(
                Update,
                tick_offline_creature_ai
                    .in_set(benilla_world::schedule::WorldStage::Net)
                    .after(advance_offline_patrols)
                    .before(crate::net::apply_net_updates),
            )
            .add_systems(
                Update,
                stream_offline_spawns
                    .in_set(benilla_world::schedule::WorldStage::Net)
                    .after(tick_offline_creature_ai)
                    .before(crate::net::apply_net_updates),
            )
            .add_systems(
                Update,
                update_local_player.after(benilla_world::schedule::WorldStage::Input),
            );
        crate::shutdown::on_app_exit(app, save_on_exit.into_configs());
        let smoke = std::env::var_os("BENILLA_OFFLINE_SMOKE").is_some();
        if std::env::var_os("BENILLA_OFFLINE_BOOT_CHECK").is_some() && !smoke {
            app.add_systems(Update, exit_after_roster_boot);
        }
        if smoke {
            app.add_systems(
                Update,
                enter_offline_smoke_character.before(drive_local_requests),
            )
            .add_systems(
                Update,
                drive_offline_smoke_walk.before(crate::bindings::BindingSet),
            )
            .add_systems(
                Update,
                drive_offline_smoke_cast.before(drive_offline_gameplay),
            )
            .add_systems(Update, exit_after_world_smoke);
        }
    }
}

fn publish_body_proportions(
    core: Res<OfflineGameCore>,
    mut profiles: ResMut<crate::body_shape::BodyProportionRoster>,
) {
    let next: BTreeMap<_, _> = core
        .0
        .character_ids()
        .filter_map(|id| {
            core.0
                .snapshot(id)
                .map(|snapshot| (id.presentation_guid(), snapshot.body_proportions.into()))
        })
        .collect();
    if profiles.0 != next {
        profiles.0 = next;
    }
}

fn publish_local_roster(
    core: Res<OfflineGameCore>,
    catalog: Option<Res<crate::entities::CharCreate>>,
    mut roster: MessageWriter<CharListMessage>,
) {
    send_roster(&core.0, catalog.as_deref(), &mut roster);
}

fn send_roster(
    core: &GameCore,
    catalog: Option<&crate::entities::CharCreate>,
    roster: &mut MessageWriter<CharListMessage>,
) {
    let characters = core
        .character_ids()
        .filter_map(|id| core.snapshot(id))
        .map(|snapshot| {
            let p = snapshot.position;
            let mut equipment = [benilla_protocol::CharEnumItem::default(); 19];
            if let Some(catalog) = catalog {
                for item in catalog.0.start_outfit(
                    snapshot.appearance.race,
                    snapshot.appearance.class,
                    snapshot.appearance.gender,
                ) {
                    if let Some(slot) = inventory_type_to_roster_slot(item.inv_type) {
                        equipment[slot] = benilla_protocol::CharEnumItem {
                            display_id: item.display_id,
                            inventory_type: item.inv_type,
                        };
                    }
                }
            }
            benilla_protocol::Character {
                guid: snapshot.id.presentation_guid(),
                name: snapshot.name.clone(),
                race: snapshot.appearance.race,
                class: snapshot.appearance.class,
                gender: snapshot.appearance.gender,
                skin: snapshot.appearance.skin,
                face: snapshot.appearance.face,
                hair_style: snapshot.appearance.hair_style,
                hair_color: snapshot.appearance.hair_color,
                facial_hair: snapshot.appearance.facial_hair,
                level: snapshot.level,
                zone: 0,
                map: p.map_id,
                position: benilla_protocol::wire::Vector3d {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                },
                flags: 0,
                equipment,
                pet_display_id: 0,
                pet_level: 0,
                pet_family: 0,
            }
        })
        .collect();
    roster.write(CharListMessage {
        characters,
        realm: None,
    });
}

fn load_core() -> Result<Option<GameCore>, String> {
    let profile_path = local_state::offline_profile_path()
        .ok_or_else(|| "local state root is unavailable".to_string())?;
    let Some(profile) = read_checked_document(&profile_path, decode_profile)? else {
        return Ok(None);
    };
    let mut snapshots = Vec::new();
    for (id, _) in profile.entries() {
        let path = local_state::offline_character_path(id.as_str())
            .ok_or_else(|| format!("invalid character id {}", id.as_str()))?;
        let snapshot = read_checked_document(&path, decode_snapshot)?
            .ok_or_else(|| format!("missing character snapshot for {}", id.as_str()))?;
        snapshots.push(snapshot);
    }
    GameCore::restore(profile, snapshots)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn read_checked_document<T>(
    path: &Path,
    decode: impl Fn(&str) -> Result<T, SaveError>,
) -> Result<Option<T>, String> {
    let backup = local_state::backup_path(path);
    let primary_error = match std::fs::read_to_string(path) {
        Ok(source) => match decode(&source) {
            Ok(document) => return Ok(Some(document)),
            Err(error) => Some(error.to_string()),
        },
        Err(error) if path.exists() => Some(error.to_string()),
        Err(_) => None,
    };

    let backup_source = match std::fs::read_to_string(&backup) {
        Ok(source) => match decode(&source) {
            Ok(document) => {
                warn!("offline save recovered from {}", backup.display());
                local_state::write_atomic_bytes(path, source.as_bytes()).map_err(|error| {
                    format!("repairing {} from backup: {error}", path.display())
                })?;
                return Ok(Some(document));
            }
            Err(error) => Some(error.to_string()),
        },
        Err(error) if backup.exists() => Some(error.to_string()),
        Err(_) => None,
    };

    match (primary_error, backup_source) {
        (None, None) => Ok(None),
        (Some(error), None) | (None, Some(error)) => Err(format!("{}: {error}", path.display())),
        (Some(primary), Some(backup)) => Err(format!(
            "{} is invalid ({primary}); backup is invalid ({backup})",
            path.display()
        )),
    }
}

/// The offline population's per-faction reaction table for a character's race, built from the
/// installed build-8606 `FactionTemplate.dbc` comparator (unit template toward the race's player
/// template). A missing catalog or race template reads every template friendly, so the population
/// stays inert rather than guessing hostility.
fn unit_reactions(
    factions: Option<&crate::target::Factions>,
    race: u8,
) -> benilla_game_core::UnitReactions {
    use benilla_formats::Reaction;
    let Some(catalog) = factions.map(|factions| factions.catalog()) else {
        return benilla_game_core::UnitReactions::default();
    };
    let Some(player) = catalog.race_template(race) else {
        return benilla_game_core::UnitReactions::default();
    };
    benilla_game_core::UnitReactions::from_pairs(catalog.templates().map(|(id, template)| {
        let reaction = match template.reaction_toward(player) {
            Reaction::Hostile => benilla_game_core::UnitReaction::Hostile,
            Reaction::Neutral => benilla_game_core::UnitReaction::Neutral,
            Reaction::Friendly => benilla_game_core::UnitReaction::Friendly,
        };
        (id, reaction)
    }))
}

fn drive_local_requests(
    channels: Res<OfflineChannels>,
    mut core: ResMut<OfflineGameCore>,
    mut persistence: ResMut<OfflinePersistence>,
    catalog: Option<Res<crate::entities::CharCreate>>,
    creatures: Option<Res<crate::entities::Creatures>>,
    factions: Option<Res<crate::target::Factions>>,
    talents: Option<Res<crate::ui_talent::Talents>>,
    mut names: ResMut<crate::names::NameCache>,
    mut patrols: ResMut<OfflinePatrols>,
    mut streamed: ResMut<OfflineStreamedSpawns>,
    mut roster: MessageWriter<CharListMessage>,
    mut action_results: MessageWriter<CharActionResultMessage>,
) {
    while let Ok(request) = channels.picks.try_recv() {
        match request {
            CharRequest::Enter(guid) => {
                let Some(id) = core.0.id_for_presentation_guid(guid).cloned() else {
                    warn!("offline roster: refused unknown character {guid:#x}");
                    continue;
                };
                if core.0.select_character(&id).is_err() {
                    continue;
                }
                if let Some(race) = core
                    .0
                    .snapshot(&id)
                    .map(|snapshot| snapshot.appearance.race)
                {
                    core.0
                        .set_unit_reactions(unit_reactions(factions.as_deref(), race));
                }
                match core.0.enter_world() {
                    Ok(snapshot) => {
                        emit_world_entry_with_talents(
                            &channels,
                            &snapshot,
                            core.0.hostiles(),
                            catalog.as_deref(),
                            creatures.as_deref(),
                            factions.as_deref(),
                            talents.as_deref(),
                            &mut names,
                            &mut patrols,
                            &mut streamed.0,
                        );
                        save_after_command(&core.0, &mut persistence);
                    }
                    Err(error) => warn!("offline roster: enter failed: {error}"),
                }
            }
            CharRequest::Delete(guid) => {
                let result = core
                    .0
                    .id_for_presentation_guid(guid)
                    .cloned()
                    .ok_or(benilla_game_core::CoreError::CharacterNotFound)
                    .and_then(|id| core.0.delete_character(&id));
                let code = if result.is_ok() {
                    save_after_command(&core.0, &mut persistence);
                    send_roster(&core.0, catalog.as_deref(), &mut roster);
                    benilla_protocol::messages::CHAR_DELETE_SUCCESS
                } else {
                    0x2F
                };
                action_results.write(CharActionResultMessage {
                    action: CharAction::Delete,
                    code,
                });
            }
            CharRequest::Create {
                request,
                body_proportions,
            } => {
                let code = match create_local_character(
                    &mut core.0,
                    catalog.as_deref(),
                    request,
                    body_proportions,
                ) {
                    Ok(()) => {
                        save_after_command(&core.0, &mut persistence);
                        send_roster(&core.0, catalog.as_deref(), &mut roster);
                        benilla_protocol::messages::CHAR_CREATE_SUCCESS
                    }
                    Err(code) => code,
                };
                action_results.write(CharActionResultMessage {
                    action: CharAction::Create,
                    code,
                });
            }
            CharRequest::Abandon => {
                if core.0.active().is_some() {
                    if let Err(error) = core.0.leave_world() {
                        warn!("offline world: could not leave cleanly: {error}");
                    }
                    save_after_command(&core.0, &mut persistence);
                }
            }
        }
    }
}

fn create_local_character(
    core: &mut GameCore,
    catalog: Option<&crate::entities::CharCreate>,
    request: CharCreateReq,
    body_proportions: crate::body_shape::BodyProportions,
) -> Result<(), u8> {
    let invalid = benilla_protocol::messages::CHAR_CREATE_ERROR;
    let Some(catalog) = catalog else {
        return Err(invalid);
    };
    if request.gender > 1 || !catalog.0.allows(request.race, request.class) {
        return Err(invalid);
    }
    let Some(ranges) = catalog.0.ranges(request.race, request.gender) else {
        return Err(invalid);
    };
    if request.skin >= ranges.skin
        || request.face >= ranges.face
        || request.hair_style >= ranges.hair_style
        || request.hair_color >= ranges.hair_color
        || request.facial_hair >= ranges.facial_hair
    {
        return Err(invalid);
    }

    let appearance = Appearance {
        race: request.race,
        class: request.class,
        gender: request.gender,
        skin: request.skin,
        face: request.face,
        hair_style: request.hair_style,
        hair_color: request.hair_color,
        facial_hair: request.facial_hair,
    };
    let Some(start) = starting_position(request.race) else {
        return Err(invalid);
    };
    let id = CharacterId::parse(format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    ))
    .expect("random 128-bit character ID is lowercase hexadecimal");
    if !body_proportions.is_valid() {
        return Err(invalid);
    }
    core.create_character(id.clone(), &request.name, appearance, start)
        .and_then(|_| core.set_body_proportions(&id, body_proportions.into()))
        .map(|_| ())
        .map_err(|error| match error {
            benilla_game_core::CoreError::DuplicateName => {
                benilla_protocol::messages::CHAR_CREATE_NAME_IN_USE
            }
            benilla_game_core::CoreError::RosterFull => {
                benilla_protocol::messages::CHAR_CREATE_SERVER_LIMIT
            }
            _ => invalid,
        })
}

fn inventory_type_to_roster_slot(inventory_type: u8) -> Option<usize> {
    Some(match inventory_type {
        1 => 0,        // head
        2 => 1,        // neck
        3 => 2,        // shoulders
        4 => 3,        // shirt
        5 | 20 => 4,   // chest or robe
        6 => 5,        // waist
        7 => 6,        // legs
        8 => 7,        // feet
        9 => 8,        // wrists
        10 => 9,       // hands
        11 => 10,      // finger
        12 => 12,      // trinket
        13 | 17 => 15, // weapon or two-hand
        14 => 16,      // shield/off hand
        15 => 17,      // ranged
        16 => 14,      // cloak
        19 => 18,      // tabard
        _ => return None,
    })
}

/// CMaNGOS `[DND]` (Do Not Disturb) templates are test/dummy creatures — the TAR pedestals and
/// event bunnies — that the source database never intends to spawn for players. The converter
/// carries them through, so the offline adapter drops them rather than litter the starting zone
/// with innkeeper/auctioneer pedestals.
fn is_dnd_test_creature(template: &benilla_game_core::content::CreatureTemplate) -> bool {
    template.name.starts_with("[DND]")
}

#[cfg(test)]
fn emit_world_entry(
    channels: &OfflineChannels,
    snapshot: &benilla_game_core::CharacterSnapshot,
    hostiles: &[HostileState],
    catalog: Option<&crate::entities::CharCreate>,
    creatures: Option<&crate::entities::Creatures>,
    factions: Option<&crate::target::Factions>,
    names: &mut crate::names::NameCache,
    patrols: &mut OfflinePatrols,
) {
    let mut streamed = std::collections::HashSet::new();
    emit_world_entry_with_talents(
        channels,
        snapshot,
        hostiles,
        catalog,
        creatures,
        factions,
        None,
        names,
        patrols,
        &mut streamed,
    );
}

fn emit_world_entry_with_talents(
    channels: &OfflineChannels,
    snapshot: &benilla_game_core::CharacterSnapshot,
    hostiles: &[HostileState],
    catalog: Option<&crate::entities::CharCreate>,
    creatures: Option<&crate::entities::Creatures>,
    factions: Option<&crate::target::Factions>,
    talents: Option<&crate::ui_talent::Talents>,
    names: &mut crate::names::NameCache,
    patrols: &mut OfflinePatrols,
    streamed: &mut std::collections::HashSet<u64>,
) {
    patrols.0.clear();
    streamed.clear();
    // The summoned pets' identities join the creature-name cache at world entry, so a summon's
    // unit, pet tab and combat log resolve whenever one appears.
    for template in benilla_game_core::summon::summons() {
        names.insert_creature(
            template.entry,
            Some(crate::names::CreatureRecord {
                name: template.name.clone(),
                subname: None,
                creature_type: template.creature_type,
                pet_family: template.family,
                rank: 0,
                type_flags: 0,
                civilian: false,
                racial_leader: false,
                display_id: template.display_id,
            }),
        );
    }
    let guid = snapshot.id.presentation_guid();
    let position = [
        snapshot.position.x,
        snapshot.position.y,
        snapshot.position.z,
    ];
    let appearance = snapshot.appearance;
    // Byte 3 of UNIT_FIELD_BYTES_0 is the power type: the client picks the power bar from it, so
    // a warrior declared mana would show an eternally empty mana-style bar.
    let race_class_gender = u32::from(appearance.race)
        | (u32::from(appearance.class) << 8)
        | (u32::from(appearance.gender) << 16)
        | (u32::from(benilla_game_core::PowerType::of_class(appearance.class).wire()) << 24);
    let player_bytes = u32::from(appearance.skin)
        | (u32::from(appearance.face) << 8)
        | (u32::from(appearance.hair_style) << 16)
        | (u32::from(appearance.hair_color) << 24);
    let display_id =
        catalog.and_then(|catalog| catalog.0.body_display(appearance.race, appearance.gender));
    let mut pairs = vec![
        (2, 0x19),                                  // OBJECT_FIELD_TYPE: object, unit, player
        (22, snapshot.health),                      // UNIT_FIELD_HEALTH
        (28, benilla_game_core::PLAYER_MAX_HEALTH), // UNIT_FIELD_MAXHEALTH
        (34, u32::from(snapshot.level)),            // UNIT_FIELD_LEVEL
        (1176, snapshot.money_copper),              // PLAYER_FIELD_COINAGE
        (716, snapshot.experience),                 // PLAYER_XP
        (
            717,
            benilla_game_core::progression::xp_to_next_level(snapshot.level).unwrap_or(0),
        ), // PLAYER_NEXT_LEVEL_XP
        (1102, free_talent_points(snapshot, talents)), // PLAYER_CHARACTER_POINTS1
        // PLAYER_FIELD_WATCHED_FACTION_INDEX, signed: absent fields zero-fill, and 0 is a real
        // reputation slot (Bloodsail Buccaneers), so "watch nothing" must be written as -1 or a
        // fresh character's watch bar shows that faction.
        (
            1261,
            benilla_protocol::messages::WATCHED_FACTION_NONE as u32,
        ),
        (46, 0x8),                                            // UNIT_FLAG_PLAYER_CONTROLLED
        (36, race_class_gender),                              // UNIT_FIELD_BYTES_0
        (193, player_bytes),                                  // PLAYER_BYTES
        (194, u32::from(appearance.facial_hair) | (2 << 24)), // PLAYER_BYTES_2: normal exhaustion state in byte 3
    ];
    pairs.extend(character_aura_field_pairs(&snapshot.auras));
    pairs.extend(quest_log_field_pairs(snapshot));
    pairs.extend(player_item_field_pairs(snapshot));
    if let Some(faction) = factions.and_then(|factions| factions.race_template(appearance.race)) {
        pairs.push((35, faction));
    }
    // The class's power pools: mana 23/29, rage 24/30, focus 25/31, energy 26/32. Rage lives in
    // tenths in the descriptor (`UnitPower` divides on read).
    let (max_mana, max_rage, max_focus, max_energy) =
        benilla_game_core::power_pool_caps(appearance.class);
    pairs.extend([
        (23, snapshot.mana),
        (24, snapshot.rage.saturating_mul(10)),
        (25, snapshot.focus),
        (26, snapshot.energy),
        (29, max_mana),
        (30, max_rage.saturating_mul(10)),
        (31, max_focus),
        (32, max_energy),
    ]);
    if let Some(display_id) = display_id {
        pairs.push((131, display_id));
        pairs.push((132, display_id));
    }
    let fields = benilla_protocol::ObjectFields::from_pairs(&pairs)
        .into_created(benilla_protocol::messages::ObjectType::Player);
    let mut events = vec![
        SessionEvent::Connected {
            self_guid: guid,
            name: snapshot.name.clone(),
            billing_time_rested: 0,
            tutorial_flags: None,
            addon_info: None,
        },
        SessionEvent::Worldport {
            map_id: snapshot.position.map_id,
            position,
            orientation: snapshot.position.orientation,
            needs_ack: false,
        },
        SessionEvent::ObjectCreate {
            guid,
            kind: benilla_protocol::EntityKind::Player,
            display_id,
            position,
            orientation: snapshot.position.orientation,
            scale: crate::body_shape::height_scale(snapshot.body_proportions.into()),
            speeds: None,
            mover: None,
            transport_progress: None,
            transport: None,
            spline: None,
            fields,
        },
    ];
    for item in snapshot.inventory.iter().chain(snapshot.equipment.iter()) {
        events.push(item_create_event(guid, snapshot, item));
        if let Some(info) = item_info(item.entry) {
            events.push(SessionEvent::ItemTemplate {
                entry: item.entry,
                info: Some(Box::new(info)),
            });
        }
    }
    events.push(SessionEvent::SpellBook {
        spell_ids: snapshot.known_spell_ids.clone(),
        cooldowns: Vec::new(),
    });
    events.push(SessionEvent::ActionButtons {
        buttons: starter_action_buttons(appearance.race, appearance.class),
    });
    events.extend(snapshot.auras.iter().filter_map(|aura| {
        aura.remaining_ms
            .map(|remaining_ms| SessionEvent::AuraDuration {
                slot: aura.slot,
                remaining_ms,
            })
    }));
    let content = benilla_game_core::content::creature_content();
    for spawn in content.spawns_near(
        snapshot.position.map_id,
        snapshot.position.x,
        snapshot.position.y,
        OFFLINE_CONTENT_RADIUS_YARDS,
    ) {
        if let Some(guid) = emit_imported_spawn(
            &mut events,
            content,
            spawn,
            creatures,
            hostiles,
            &snapshot.corpse_loot,
            names,
            patrols,
        ) {
            streamed.insert(guid);
        }
    }
    events.push(SessionEvent::TimeSpeed {
        hours: ((snapshot.game_time_minutes % 1440) / 60) as u8,
        minutes: (snapshot.game_time_minutes % 60) as u8,
        day_serial: (snapshot.game_time_minutes / 1440) as u32,
        timescale: 1.0,
    });
    for event in events {
        if channels.events.send(event).is_err() {
            error!("offline session adapter: event receiver was dropped");
            break;
        }
    }
}

/// Emit one converted creature spawn into `events`: its name-cache record, the `ObjectCreate`
/// carrying its combat state, and the patrol spline when the row's `movement_type` is 2. Returns
/// the presentation GUID, or `None` for a `[DND]` test template the converter carries but no
/// spawn should show. Shared by world entry and the travel stream so both emit identically.
#[allow(clippy::too_many_arguments)]
fn emit_imported_spawn(
    events: &mut Vec<SessionEvent>,
    content: &benilla_game_core::content::CreatureContent,
    spawn: &benilla_game_core::content::CreatureSpawn,
    creatures: Option<&crate::entities::Creatures>,
    hostiles: &[HostileState],
    corpse_loot: &[benilla_game_core::loot::CorpseLoot],
    names: &mut crate::names::NameCache,
    patrols: &mut OfflinePatrols,
) -> Option<u64> {
    let template = content
        .template(spawn.entry)
        .expect("converted spawn template was validated by the content loader");
    if is_dnd_test_creature(template) {
        return None;
    }
    let waypoint_path: Vec<_> = content.waypoints_for(spawn.source_guid).copied().collect();
    let scale = creatures
        .and_then(|catalog| catalog.model_scale(template.display_id))
        .unwrap_or(1.0)
        * template.scale;
    names.insert_creature(
        template.entry,
        Some(crate::names::CreatureRecord {
            name: template.name.clone(),
            subname: (!template.subname.is_empty()).then(|| template.subname.clone()),
            creature_type: template.creature_type,
            pet_family: 0,
            rank: 0,
            type_flags: 0,
            civilian: false,
            racial_leader: false,
            display_id: template.display_id,
        }),
    );
    let guid = imported_guid(template.entry, spawn.source_guid);
    let combat_state = hostiles.iter().find(|hostile| {
        hostile.position.map_id == spawn.map_id && hostile.spawn_id == spawn.source_guid
    });
    let health = combat_state.map_or(100, |hostile| hostile.health);
    let max_health = combat_state.map_or(100, |hostile| hostile.max_health);
    let mut fields = vec![
        (2, 0x9),
        (3, template.entry),
        (4, scale.to_bits()),
        (22, health),
        (28, max_health),
        (34, u32::from(template.level)),
        (35, template.faction),
        (147, template.npc_flags),
        (130, 1.5f32.to_bits()),
        (131, template.display_id),
        (132, template.display_id),
    ];
    if combat_state.is_some_and(|hostile| hostile.health == 0) {
        let has_loot = corpse_loot
            .iter()
            .find(|corpse| corpse.spawn_id == spawn.source_guid)
            .is_some_and(|corpse| {
                (!corpse.money_looted && corpse.loot.gold > 0)
                    || corpse
                        .loot
                        .items
                        .iter()
                        .zip(&corpse.looted_items)
                        .any(|(_, looted)| !looted)
            });
        fields.push((143, u32::from(has_loot)));
    }
    if let Some(addon) = content.addon_for(spawn.source_guid) {
        if addon.stand_state != 0 {
            fields.push((138, u32::from(addon.stand_state)));
        }
        if addon.emote != 0 {
            fields.push((148, addon.emote));
        }
        if addon.sheath_state != 0 {
            fields.push((164, u32::from(addon.sheath_state)));
        }
    }
    if let Some((_, sex)) =
        creatures.and_then(|creatures| creatures.display_race_sex(template.display_id))
    {
        fields.push((36, u32::from(sex) << 16));
    }
    let fields = ObjectFields::from_pairs(&fields)
        .into_created(benilla_protocol::messages::ObjectType::Unit);
    events.push(SessionEvent::ObjectCreate {
        guid,
        kind: EntityKind::Unit,
        display_id: Some(template.display_id),
        position: [spawn.x, spawn.y, spawn.z],
        orientation: spawn.orientation,
        scale,
        speeds: None,
        mover: None,
        transport_progress: None,
        transport: None,
        spline: None,
        fields,
    });
    if spawn.movement_type == 2 {
        if let Some(first) = waypoint_path.first().copied() {
            let start = [spawn.x, spawn.y, spawn.z];
            let target = [first.x, first.y, first.z];
            let duration_ms = patrol_leg_duration_ms(start, target);
            events.push(patrol_move(guid, start, target, 1, duration_ms));
            patrols.0.insert(
                guid,
                ActivePatrol {
                    waypoints: waypoint_path,
                    waypoint_index: 0,
                    leg_start: start,
                    position: target,
                    current_position: start,
                    leg_duration_ms: duration_ms,
                    wait_ms: first.wait_ms,
                    remaining_ms: duration_ms.saturating_add(first.wait_ms),
                    spline_id: 1,
                },
            );
        }
    }
    Some(guid)
}

fn character_aura_field_pairs(auras: &[CharacterAura]) -> Vec<(u16, u32)> {
    use benilla_protocol::messages::{
        FIELD_UNIT_AURA, FIELD_UNIT_AURAAPPLICATIONS, FIELD_UNIT_AURAFLAGS, FIELD_UNIT_AURALEVELS,
        UNIT_AURA_SLOTS,
    };

    let mut fields = BTreeMap::new();
    for slot in 0..UNIT_AURA_SLOTS {
        fields.insert(FIELD_UNIT_AURA + u16::from(slot), 0);
    }
    for word in 0..u16::from(UNIT_AURA_SLOTS / 8) {
        fields.insert(FIELD_UNIT_AURAFLAGS + word, 0);
    }
    for word in 0..u16::from(UNIT_AURA_SLOTS / 4) {
        fields.insert(FIELD_UNIT_AURALEVELS + word, 0);
        fields.insert(FIELD_UNIT_AURAAPPLICATIONS + word, 0);
    }
    for aura in auras {
        let slot = u16::from(aura.slot);
        fields.insert(FIELD_UNIT_AURA + slot, aura.spell_id);
        *fields.entry(FIELD_UNIT_AURAFLAGS + slot / 8).or_default() |=
            u32::from(aura.flags) << ((slot % 8) * 4);
        *fields.entry(FIELD_UNIT_AURALEVELS + slot / 4).or_default() |=
            u32::from(aura.level) << ((slot % 4) * 8);
        *fields
            .entry(FIELD_UNIT_AURAAPPLICATIONS + slot / 4)
            .or_default() |= u32::from(aura.stacks.saturating_sub(1)) << ((slot % 4) * 8);
    }
    fields.into_iter().collect()
}

fn hostile_spell_state_events(
    guid: u64,
    hostile: &benilla_game_core::HostileState,
    expired_aura: Option<u32>,
) -> Vec<SessionEvent> {
    let auras = if hostile.burn_spell_id != 0 && hostile.burn_remaining_ms > 0 {
        vec![CharacterAura {
            slot: 32,
            spell_id: hostile.burn_spell_id,
            flags: 0x08,
            level: hostile.burn_level,
            stacks: 1,
            remaining_ms: Some(hostile.burn_remaining_ms),
        }]
    } else {
        Vec::new()
    };
    // Dynamic-flag bit 0x20 means feign death; health zero alone represents an ordinary death.
    let mut fields = vec![(22, hostile.health), (143, 0)];
    fields.extend(character_aura_field_pairs(&auras));
    let mut events = vec![SessionEvent::ObjectValues {
        guid,
        fields: ObjectFields::from_pairs(&fields),
    }];
    if let Some(aura) = auras.first() {
        events.push(SessionEvent::AuraDuration {
            slot: aura.slot,
            remaining_ms: aura.remaining_ms.unwrap_or(0),
        });
    } else if expired_aura.is_some() {
        events.push(SessionEvent::AuraDuration {
            slot: 32,
            remaining_ms: 0,
        });
    }
    events
}

fn starting_position(race: u8) -> Option<WorldPosition> {
    // Project-authored build-8606 starting anchors; gameplay rules and provenance are recorded in
    // `crates/benilla-game-core/RULES.md`.
    let (map_id, x, y, z) = match race {
        1 => (0, -8949.95, -132.49, 83.53),
        2 | 8 => (1, -618.518, -4251.67, 38.718),
        3 | 7 => (0, -6240.32, 331.033, 382.758),
        4 => (1, 10311.3, 832.463, 1326.41),
        5 => (0, 1676.35, 1678.21, 121.67),
        6 => (1, -2917.58, -257.98, 52.9968),
        10 => (530, 10349.6, -6357.3, 33.4),
        11 => (530, -3961.64, -13931.2, 100.615),
        _ => return None,
    };
    Some(WorldPosition {
        map_id,
        x,
        y,
        z,
        orientation: 0.0,
    })
}

fn imported_guid(entry: u32, source_guid: u32) -> u64 {
    (u64::from(benilla_protocol::guid::HIGH_UNIT) << 48)
        | (u64::from(entry) << 24)
        | u64::from(source_guid & 0x00ff_ffff)
}

/// The summoned pet's unit guid: its summon-pack creature entry with one synthetic spawn id (only
/// one pet is up at a time).
fn summon_guid(entry: u32) -> u64 {
    imported_guid(entry, 1)
}

/// The summoned pet's action bar: the fixed command and reaction buttons in vmangos's order
/// (`Player::PetSpellInitialize`), defensive react and follow command as the default state.
fn summon_pet_bar(pet_guid: u64) -> benilla_protocol::messages::PetSpells {
    use benilla_protocol::messages as m;
    let command =
        |action: u32| m::PetActionEntry::from((u32::from(m::PET_ACT_COMMAND) << 24) | action);
    let reaction =
        |react: u32| m::PetActionEntry::from((u32::from(m::PET_ACT_REACTION) << 24) | react);
    let mut bar = [m::PetActionEntry::default(); m::PET_ACTION_SLOTS];
    bar[0] = command(m::PET_COMMAND_ATTACK);
    bar[1] = command(m::PET_COMMAND_FOLLOW);
    bar[2] = command(m::PET_COMMAND_STAY);
    bar[3] = reaction(m::PET_REACT_PASSIVE);
    bar[4] = reaction(m::PET_REACT_DEFENSIVE);
    bar[5] = reaction(m::PET_REACT_AGGRESSIVE);
    m::PetSpells {
        pet_guid,
        duration_ms: 0,
        state: m::PET_REACT_DEFENSIVE | (m::PET_COMMAND_FOLLOW << 8),
        bar,
        spells: Vec::new(),
        cooldowns: Vec::new(),
    }
}

/// The movement control a row's aura arms carry: stun (Hammer of Justice; the disorient families
/// act as a stun here — wander and flee are not modeled), root (Frost Nova) or snare (Frostbolt's
/// -40), for the row's own duration. The strongest arm of the row wins, and an arm without a row
/// duration lands no control.
fn control_effect(
    durations: &benilla_formats::SpellDurationCatalog,
    spell: &benilla_formats::SpellDisplay,
) -> Option<benilla_game_core::ControlEffect> {
    let rank = |kind: benilla_game_core::ControlKind| match kind {
        benilla_game_core::ControlKind::Stun => 2,
        benilla_game_core::ControlKind::Root => 1,
        benilla_game_core::ControlKind::Snare => 0,
    };
    let mut control: Option<benilla_game_core::ControlEffect> = None;
    for slot in 0..3 {
        let kind = match spell.effect_apply_aura[slot] {
            benilla_formats::SPELL_AURA_MOD_STUN
            | benilla_formats::SPELL_AURA_MOD_CONFUSE
            | benilla_formats::SPELL_AURA_MOD_FEAR => benilla_game_core::ControlKind::Stun,
            benilla_formats::SPELL_AURA_MOD_ROOT => benilla_game_core::ControlKind::Root,
            benilla_formats::SPELL_AURA_MOD_DECREASE_SPEED => benilla_game_core::ControlKind::Snare,
            _ => continue,
        };
        let Ok(duration_ms) = u32::try_from(durations.get(spell.duration_index)?.base_ms) else {
            continue;
        };
        // A snare's strength is the row's own percent (Frostbolt's -40).
        let slow_percent = if kind == benilla_game_core::ControlKind::Snare {
            spell
                .base_effect_range(slot)
                .map(|(min, _)| min.saturating_neg().clamp(1, 99) as u32)
                .unwrap_or(50)
        } else {
            0
        };
        let candidate = benilla_game_core::ControlEffect {
            kind,
            slow_percent,
            duration_ms,
        };
        if duration_ms > 0
            && control.is_none_or(|current| rank(candidate.kind) > rank(current.kind))
        {
            control = Some(candidate);
        }
    }
    control
}

/// `CreatureType` 11 in the converted template: a totem creature stands and pulses; every other
/// summoned creature is a guardian or observer.
const TOTEM_CREATURE_TYPE: u32 = 11;

/// The totem-style behavior the converted summon implies. A totem creature folds its converted
/// passive rows into the buff it holds and the pulse it fires: a periodic trigger fires the
/// trigger row's own arm (Earthbind's snare every 3000ms), a periodic heal ticks on its own
/// cadence (Healing Stream), and a static arm is the standing buff (Stoneskin). A guardian (the
/// pack's `swing` entries) swings the creature baseline like every creature in the simulation,
/// and anything else stands inert (the Eye of Kilrogg's vision is a named fidelity).
fn totem_behavior(
    catalog: &benilla_formats::SpellCatalog,
    durations: &benilla_formats::SpellDurationCatalog,
    spell: &benilla_formats::SpellDisplay,
    template: &benilla_game_core::summon::SummonTemplate,
) -> benilla_game_core::TotemBehavior {
    use benilla_game_core::{TotemBehavior, TotemPulse};
    let duration_ms = durations
        .get(spell.duration_index)
        .and_then(|duration| u32::try_from(duration.base_ms).ok())
        .unwrap_or(0);
    let mut behavior = TotemBehavior {
        buff_spell: None,
        pulse: None,
        pulse_ms: 0,
        duration_ms,
    };
    if template.swing {
        behavior.pulse = Some(TotemPulse::Damage {
            min: benilla_game_core::CREATURE_DAMAGE,
            max: benilla_game_core::CREATURE_DAMAGE,
            school: 0,
        });
        behavior.pulse_ms = benilla_game_core::CREATURE_SWING_MS;
        return behavior;
    }
    if template.creature_type != TOTEM_CREATURE_TYPE {
        return behavior;
    }
    for &passive in &template.spells {
        let Some(row) = catalog.get(passive) else {
            continue;
        };
        for slot in 0..3 {
            let amplitude = row.effect_amplitude[slot];
            match row.effect_apply_aura[slot] {
                benilla_formats::SPELL_AURA_PERIODIC_TRIGGER_SPELL if amplitude > 0 => {
                    if behavior.pulse.is_none() {
                        if let Some(pulse) =
                            pulse_from_trigger(catalog, durations, row.effect_trigger_spell[slot])
                        {
                            behavior.pulse = Some(pulse);
                            behavior.pulse_ms = amplitude;
                        }
                    }
                }
                benilla_formats::SPELL_AURA_PERIODIC_HEAL if amplitude > 0 => {
                    if behavior.pulse.is_none() {
                        if let Some((min, max)) = row.base_effect_range(slot) {
                            behavior.pulse = Some(TotemPulse::Heal {
                                min: min.max(0) as u32,
                                max: max.max(0) as u32,
                            });
                            behavior.pulse_ms = amplitude;
                        }
                    }
                }
                0 => {}
                // A static arm is the buff the totem holds while it stands. Behavior outside the
                // pulse vocabulary (Mana Spring's resource restore) folds here too and stays a
                // named fidelity (RULES.md, "Totems").
                _ => {
                    if behavior.buff_spell.is_none() {
                        behavior.buff_spell = Some(passive);
                    }
                }
            }
        }
        // A plain damage or heal row is the totem's own attack ("Attack" 6350): it fires on the
        // creature cadence like a creature's swing.
        if behavior.pulse.is_none() {
            if let Some(pulse) = pulse_from_trigger(catalog, durations, passive) {
                behavior.pulse = Some(pulse);
                behavior.pulse_ms = benilla_game_core::CREATURE_SWING_MS;
            }
        }
    }
    behavior
}

/// The pulse a periodic trigger's row carries: its damage or heal arm, or the movement control its
/// aura arms hold (Earthbind 3600's snare). Anything else folds to no pulse (a named fidelity).
fn pulse_from_trigger(
    catalog: &benilla_formats::SpellCatalog,
    durations: &benilla_formats::SpellDurationCatalog,
    trigger: u32,
) -> Option<benilla_game_core::TotemPulse> {
    use benilla_game_core::TotemPulse;
    let row = catalog.get(trigger)?;
    for slot in 0..3 {
        match row.effects[slot] {
            benilla_formats::SPELL_EFFECT_SCHOOL_DAMAGE
            | benilla_formats::SPELL_EFFECT_WEAPON_DAMAGE
            | benilla_formats::SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL
            | benilla_formats::SPELL_EFFECT_NORMALIZED_WEAPON_DMG => {
                let (min, max) = row.base_effect_range(slot)?;
                return Some(TotemPulse::Damage {
                    min: min.max(0) as u32,
                    max: max.max(0) as u32,
                    school: 1_u32.checked_shl(row.school).unwrap_or(1),
                });
            }
            benilla_formats::SPELL_EFFECT_HEAL => {
                let (min, max) = row.base_effect_range(slot)?;
                return Some(TotemPulse::Heal {
                    min: min.max(0) as u32,
                    max: max.max(0) as u32,
                });
            }
            _ => {}
        }
    }
    control_effect(durations, row).map(TotemPulse::Control)
}

fn combat_target_guid(hostile: &HostileState) -> Option<u64> {
    Some(imported_guid(hostile.creature_entry, hostile.spawn_id))
}

fn inventory_item_guid(
    snapshot: &benilla_game_core::CharacterSnapshot,
    item: &benilla_game_core::quest::InventoryItem,
) -> u64 {
    let owner = snapshot.id.presentation_guid() & 0xffff_ffff;
    let counter = (owner << 16) | (item.instance_id & 0xffff);
    (u64::from(benilla_protocol::guid::HIGH_ITEM) << 48) | counter
}

/// Every inventory field the player object carries: the 19 worn slots and 16 backpack slots as
/// item guids (0 when empty, so a vacated slot clears live), each worn item's public entry for the
/// model, and `UNIT_FIELD_RESISTANCES[0]` = the worn armor total. Shared by world entry and every
/// live inventory move.
fn player_item_field_pairs(snapshot: &benilla_game_core::CharacterSnapshot) -> Vec<(u16, u32)> {
    use benilla_game_core::quest::{BACKPACK_SLOTS, EQUIPMENT_SLOTS};

    let mut pairs = Vec::with_capacity(76);
    for slot in 0..EQUIPMENT_SLOTS {
        let item = snapshot.equipment.iter().find(|item| item.slot == slot);
        let guid = item
            .map(|item| inventory_item_guid(snapshot, item))
            .unwrap_or(0);
        let field = FIELD_PLAYER_INV_SLOT_HEAD + 2 * u16::from(slot);
        pairs.push((field, guid as u32));
        pairs.push((field + 1, (guid >> 32) as u32));
        pairs.push((
            FIELD_PLAYER_VISIBLE_ITEM_1_CREATOR + 2 + 12 * u16::from(slot),
            item.map(|item| item.entry).unwrap_or(0),
        ));
    }
    for slot in 0..BACKPACK_SLOTS {
        let guid = snapshot
            .inventory
            .iter()
            .find(|item| item.slot == slot)
            .map(|item| inventory_item_guid(snapshot, item))
            .unwrap_or(0);
        let field = FIELD_PLAYER_PACK_SLOT_1 + 2 * u16::from(slot);
        pairs.push((field, guid as u32));
        pairs.push((field + 1, (guid >> 32) as u32));
    }
    let armor: u32 = snapshot
        .equipment
        .iter()
        .filter_map(|item| benilla_game_core::quest::item_template(item.entry))
        .map(|template| template.armor)
        .sum();
    pairs.push((FIELD_UNIT_RESISTANCES, armor));
    pairs
}

/// `PLAYER_FIELD_INV_SLOT_HEAD`.
const FIELD_PLAYER_INV_SLOT_HEAD: u16 = 486;
/// `PLAYER_FIELD_PACK_SLOT_1`.
const FIELD_PLAYER_PACK_SLOT_1: u16 = 532;
/// `PLAYER_VISIBLE_ITEM_1_CREATOR`; the entry sits two dwords in, each slot 12 wide.
const FIELD_PLAYER_VISIBLE_ITEM_1_CREATOR: u16 = 258;
/// `UNIT_FIELD_RESISTANCES[0]`, the armor total.
const FIELD_UNIT_RESISTANCES: u16 = 155;
/// The wire bag byte that names the player's own inventory array (equipment + backpack + bank).
const BAG_PLAYER_INVENTORY: u8 = 255;
/// The first backpack slot in that array; equipment is `0..19`.
const SLOT_PACK_FIRST: u8 = 23;

/// A wire `(bag, slot)` on the player's own inventory array as a core item position; `None` for a
/// bag we do not model (an equipped bag) or a slot we do not carry (bank, keyring).
fn wire_item_slot(bag_index: u8, slot: u8) -> Option<benilla_game_core::ItemSlot> {
    use benilla_game_core::quest::{BACKPACK_SLOTS, EQUIPMENT_SLOTS};
    use benilla_game_core::ItemSlot;
    if bag_index != BAG_PLAYER_INVENTORY {
        return None;
    }
    if slot < EQUIPMENT_SLOTS {
        Some(ItemSlot::Equipment(slot))
    } else if (SLOT_PACK_FIRST..SLOT_PACK_FIRST + BACKPACK_SLOTS).contains(&slot) {
        Some(ItemSlot::Backpack(slot - SLOT_PACK_FIRST))
    } else {
        None
    }
}

/// Push the player's item fields after a live change and persist the character.
fn publish_offline_inventory(
    core: &benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
) {
    let Some(snapshot) = core.active().and_then(|id| core.snapshot(id)) else {
        return;
    };
    let mut pairs = player_item_field_pairs(snapshot);
    pairs.extend(quest_log_field_pairs(snapshot));
    send_gameplay_events(
        channels,
        [SessionEvent::ObjectValues {
            guid: snapshot.id.presentation_guid(),
            fields: ObjectFields::from_pairs(&pairs),
        }],
    );
    save_after_command(core, persistence);
}

fn offline_auto_equip(
    core: &mut benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    bag_index: u8,
    slot: u8,
) {
    let Some(benilla_game_core::ItemSlot::Backpack(backpack_slot)) =
        wire_item_slot(bag_index, slot)
    else {
        return;
    };
    match core.equip_from_backpack(backpack_slot) {
        Ok(()) => publish_offline_inventory(core, persistence, channels),
        Err(error) => debug!("offline inventory: auto-equip {backpack_slot} refused: {error}"),
    }
}

fn offline_swap_inv(
    core: &mut benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    src_slot: u8,
    dst_slot: u8,
) {
    let (Some(from), Some(to)) = (
        wire_item_slot(BAG_PLAYER_INVENTORY, src_slot),
        wire_item_slot(BAG_PLAYER_INVENTORY, dst_slot),
    ) else {
        return;
    };
    match core.move_item(from, to) {
        Ok(()) => publish_offline_inventory(core, persistence, channels),
        Err(error) => debug!("offline inventory: swap {src_slot}/{dst_slot} refused: {error}"),
    }
}

fn offline_swap_item(
    core: &mut benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    src: (u8, u8),
    dst: (u8, u8),
) {
    let (Some(from), Some(to)) = (wire_item_slot(src.0, src.1), wire_item_slot(dst.0, dst.1))
    else {
        return;
    };
    match core.move_item(from, to) {
        Ok(()) => publish_offline_inventory(core, persistence, channels),
        Err(error) => debug!(
            "offline inventory: swap {:?}/{:?} refused: {error}",
            src, dst
        ),
    }
}

fn offline_store_bag(
    core: &mut benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    src_bag: u8,
    src_slot: u8,
) {
    let Some(benilla_game_core::ItemSlot::Equipment(equip_slot)) =
        wire_item_slot(src_bag, src_slot)
    else {
        return;
    };
    match core.unequip_to_backpack(equip_slot) {
        Ok(()) => publish_offline_inventory(core, persistence, channels),
        Err(error) => debug!("offline inventory: unequip {equip_slot} refused: {error}"),
    }
}

fn offline_use_item(
    core: &mut benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    bag_index: u8,
    slot: u8,
) {
    let Some(benilla_game_core::ItemSlot::Backpack(backpack_slot)) =
        wire_item_slot(bag_index, slot)
    else {
        return;
    };
    if core.equip_from_backpack(backpack_slot).is_ok() {
        publish_offline_inventory(core, persistence, channels);
    }
}

fn offline_destroy_item(
    core: &mut benilla_game_core::GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    bag_index: u8,
    slot: u8,
) {
    let Some(at) = wire_item_slot(bag_index, slot) else {
        return;
    };
    match core.destroy_item(at) {
        Ok(()) => publish_offline_inventory(core, persistence, channels),
        Err(error) => debug!("offline inventory: destroy {slot} refused: {error}"),
    }
}

fn quest_log_slot_fields(
    slot: usize,
    progress: &benilla_game_core::quest::QuestProgress,
    definition: &benilla_game_core::quest::QuestDefinition,
    inventory: &[benilla_game_core::quest::InventoryItem],
    complete: bool,
) -> [(u16, u32); 3] {
    let base = 198 + 3 * slot as u16;
    let count_state = progress
        .slot_counts(definition, inventory)
        .iter()
        .enumerate()
        .fold(0u32, |word, (index, &count)| {
            word | (count.min(0x3f) << (index * 6))
        })
        | (u32::from(complete) << 24);
    [
        (base, progress.quest_id),
        (base + 1, count_state),
        (base + 2, 0),
    ]
}

fn quest_log_field_pairs(snapshot: &benilla_game_core::CharacterSnapshot) -> Vec<(u16, u32)> {
    let items: Vec<_> = snapshot
        .inventory
        .iter()
        .chain(&snapshot.equipment)
        .copied()
        .collect();
    let mut pairs = Vec::new();
    for (slot, progress) in snapshot.quest_journal.entries.iter().enumerate() {
        let Some(definition) = benilla_game_core::quest::quest_definition(progress.quest_id) else {
            continue;
        };
        pairs.extend(quest_log_slot_fields(
            slot,
            progress,
            definition,
            &items,
            snapshot.quest_journal.is_complete_with(definition, &items),
        ));
    }
    // Zero the log's empty tail. A turn-in or abandon frees a slot, and the client keeps whatever
    // the server last told it; a slot it is never told to zero stays occupied, so the quest (and
    // its objective lines) would sit in the quest log forever. `PLAYER_QUEST_LOG_<n>_1 == 0` is the
    // wire's "empty" (`player_quest_log`), and the id, count-state and timer are written together.
    for slot in snapshot.quest_journal.entries.len()
        ..usize::from(benilla_protocol::messages::PLAYER_QUEST_LOG_SLOTS)
    {
        let base = 198 + 3 * slot as u16;
        pairs.extend([(base, 0), (base + 1, 0), (base + 2, 0)]);
    }
    pairs
}

fn item_info(entry: u32) -> Option<benilla_protocol::ItemInfo> {
    let item = benilla_game_core::quest::item_template(entry)?;
    Some(benilla_protocol::ItemInfo {
        class: item.class,
        subclass: item.subclass,
        name: item.name.clone(),
        display_info_id: item.display_id,
        quality: item.quality,
        flags: 0,
        buy_price: item.buy_price,
        sell_price: item.sell_price,
        inventory_type: item.inventory_type,
        allowable_class: item.allowable_class,
        allowable_race: item.allowable_race,
        item_level: item.item_level,
        required_level: 0,
        required_skill: 0,
        required_skill_rank: 0,
        required_spell: 0,
        required_honor_rank: 0,
        required_city_rank: 0,
        required_rep_faction: 0,
        required_rep_rank: 0,
        max_count: 0,
        stackable: item.stackable,
        container_slots: 0,
        stats: Vec::new(),
        damages: Vec::new(),
        dmg_min: 0.0,
        dmg_max: 0.0,
        dmg_type: 0,
        armor: item.armor,
        resistances: [0; 6],
        delay_ms: 0,
        ammo_type: 0,
        ranged_mod_range: 0.0,
        spells: Vec::new(),
        spell_charges_0: 0,
        use_spell: None,
        bonding: item.bonding,
        description: String::new(),
        page_text: 0,
        language_id: 0,
        page_material: 0,
        start_quest: 0,
        lock_id: 0,
        material: item.material as u32,
        sheath: 0,
        random_property: 0,
        block: 0,
        item_set: 0,
        max_durability: item.max_durability,
        area: 0,
        map: 0,
        bag_family: 0,
    })
}

fn item_create_event(
    player_guid: u64,
    snapshot: &benilla_game_core::CharacterSnapshot,
    item: &benilla_game_core::quest::InventoryItem,
) -> SessionEvent {
    let guid = inventory_item_guid(snapshot, item);
    let max_durability = item_info(item.entry).map_or(0, |info| info.max_durability);
    SessionEvent::ItemCreate {
        guid,
        container: false,
        fields: ObjectFields::from_pairs(&[
            (2, 0x3),
            (3, item.entry),
            (4, 1.0f32.to_bits()),
            (6, player_guid as u32),
            (7, (player_guid >> 32) as u32),
            (8, player_guid as u32),
            (9, (player_guid >> 32) as u32),
            (14, item.count),
            (46, max_durability),
            (47, max_durability),
        ])
        .into_created(benilla_protocol::messages::ObjectType::Item),
    }
}

fn open_offline_loot(core: &mut GameCore, channels: &OfflineChannels, guid: u64) {
    let Some(hostile) = core
        .hostiles()
        .iter()
        .find(|hostile| combat_target_guid(hostile) == Some(guid))
        .copied()
    else {
        send_gameplay_events(
            channels,
            [SessionEvent::LootError {
                guid,
                error: benilla_protocol::messages::loot_error::DIDNT_KILL,
            }],
        );
        return;
    };
    let Some(player_position) = core
        .active()
        .and_then(|id| core.snapshot(id))
        .map(|snapshot| snapshot.position)
    else {
        return;
    };
    if player_position.map_id != hostile.position.map_id
        || (player_position.x - hostile.position.x).powi(2)
            + (player_position.y - hostile.position.y).powi(2)
            + (player_position.z - hostile.position.z).powi(2)
            > OFFLINE_INTERACTION_RANGE_YARDS * OFFLINE_INTERACTION_RANGE_YARDS
    {
        send_gameplay_events(
            channels,
            [SessionEvent::LootError {
                guid,
                error: benilla_protocol::messages::loot_error::TOO_FAR,
            }],
        );
        return;
    }
    let window = match core.open_corpse_loot(hostile.spawn_id) {
        Ok(window) => window,
        Err(_) => {
            send_gameplay_events(
                channels,
                [SessionEvent::LootError {
                    guid,
                    error: benilla_protocol::messages::loot_error::DIDNT_KILL,
                }],
            );
            return;
        }
    };
    let items = window
        .items
        .into_iter()
        .map(|(slot, item)| benilla_protocol::messages::LootItem {
            slot,
            item_id: item.entry,
            count: item.count,
            display_info_id: benilla_game_core::quest::item_template(item.entry)
                .map_or(0, |template| template.display_id),
            random_property_id: 0,
            slot_type: benilla_protocol::messages::slot_type::ALLOW_LOOT,
        })
        .collect();
    send_gameplay_events(
        channels,
        [SessionEvent::LootResponse {
            guid,
            loot_type: benilla_protocol::messages::loot_type::CORPSE,
            gold: window.gold,
            items,
        }],
    );
}

fn take_offline_loot_item(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    slot: u8,
) {
    let spawn_id = core.open_loot_spawn_id();
    let item = match core.take_corpse_loot(slot) {
        Ok(item) => {
            info!(
                "offline loot: stored item {} x{} in backpack slot {} from loot slot {slot}",
                item.entry, item.count, item.slot
            );
            item
        }
        Err(error) => {
            info!("offline loot: could not take slot {slot}: {error:?}");
            return;
        }
    };
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let mut events = vec![
        SessionEvent::LootRemoved { slot },
        SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&player_item_field_pairs(snapshot)),
        },
    ];
    if let Some(event) = spawn_id.and_then(|spawn_id| offline_corpse_flags(core, spawn_id)) {
        events.push(event);
    }
    if let Some(info) = item_info(item.entry) {
        events.push(SessionEvent::ItemTemplate {
            entry: item.entry,
            info: Some(Box::new(info)),
        });
    }
    events.push(item_create_event(id.presentation_guid(), snapshot, &item));
    events.push(SessionEvent::ItemPushResult(
        benilla_protocol::messages::ItemPushResult {
            player_guid: id.presentation_guid(),
            from_npc: false,
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
    send_gameplay_events(channels, events);
    save_after_command(core, persistence);
}

fn take_offline_loot_money(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
) {
    let spawn_id = core.open_loot_spawn_id();
    let amount = match core.take_corpse_money() {
        Ok(amount) => amount,
        Err(error) => {
            debug!("offline loot: could not take corpse money: {error:?}");
            return;
        }
    };
    let Some(id) = core.active() else { return };
    let Some(snapshot) = core.snapshot(id) else {
        return;
    };
    let mut events = Vec::new();
    if amount > 0 {
        events.push(SessionEvent::LootMoneyNotify { amount });
    }
    events.extend([
        SessionEvent::LootClearMoney,
        SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&[(1176, snapshot.money_copper)]),
        },
    ]);
    if let Some(event) = spawn_id.and_then(|spawn_id| offline_corpse_flags(core, spawn_id)) {
        events.push(event);
    }
    send_gameplay_events(channels, events);
    save_after_command(core, persistence);
}

fn offline_corpse_flags(core: &GameCore, spawn_id: u32) -> Option<SessionEvent> {
    let hostile = core.hostile(spawn_id)?;
    let guid = combat_target_guid(&hostile)?;
    // Health marks an ordinary death; dynamic-flag bit 0x20 is reserved for feign death.
    Some(SessionEvent::ObjectValues {
        guid,
        fields: ObjectFields::from_pairs(&[(143, u32::from(core.corpse_has_loot(spawn_id)))]),
    })
}

fn patrol_leg_duration_ms(start: [f32; 3], target: [f32; 3]) -> u32 {
    let distance = start
        .into_iter()
        .zip(target)
        .map(|(from, to)| (to - from).powi(2))
        .sum::<f32>()
        .sqrt();
    (distance / OFFLINE_CREATURE_WALK_SPEED_YARDS_PER_SEC * 1_000.0)
        .round()
        .clamp(1.0, u32::MAX as f32) as u32
}

fn patrol_move(
    guid: u64,
    start: [f32; 3],
    target: [f32; 3],
    spline_id: u32,
    duration_ms: u32,
) -> SessionEvent {
    SessionEvent::MonsterMove {
        guid,
        transport: None,
        start,
        spline_id,
        path: vec![start, target],
        facing: benilla_protocol::MonsterMoveFacing::None,
        stop: false,
        duration_ms,
        flying: false,
        run_mode: false,
    }
}

fn advance_offline_patrols(
    channels: Res<OfflineChannels>,
    mut core: ResMut<OfflineGameCore>,
    time: Res<Time>,
    mut patrols: ResMut<OfflinePatrols>,
) {
    if core.0.active().is_none() {
        patrols.0.clear();
        return;
    }
    let elapsed_ms = (time.delta_secs_f64() * 1_000.0).round() as u32;
    let mut remove = Vec::new();
    for (&guid, patrol) in &mut patrols.0 {
        let hostile = core
            .0
            .hostiles()
            .iter()
            .find(|hostile| combat_target_guid(hostile) == Some(guid))
            .copied();
        if hostile.is_some_and(|hostile| hostile.engaged || hostile.provoked || hostile.evading) {
            remove.push(guid);
            continue;
        }
        let event = advance_patrol(guid, patrol, elapsed_ms);
        if let Some(hostile) = hostile {
            let [x, y, z] = patrol.current_position;
            let orientation = (patrol.position[1] - patrol.leg_start[1])
                .atan2(patrol.position[0] - patrol.leg_start[0]);
            core.0.update_hostile_position(
                hostile.spawn_id,
                WorldPosition {
                    map_id: hostile.position.map_id,
                    x,
                    y,
                    z,
                    orientation,
                },
                patrol.spline_id,
            );
        }
        if patrol.waypoints.len() == 1 && patrol.remaining_ms == 0 {
            remove.push(guid);
        }
        if let Some(event) = event {
            if channels.events.send(event).is_err() {
                error!("offline patrol adapter: event receiver was dropped");
                break;
            }
        }
    }
    for guid in remove {
        patrols.0.remove(&guid);
    }
}

fn advance_patrol(guid: u64, patrol: &mut ActivePatrol, elapsed_ms: u32) -> Option<SessionEvent> {
    if elapsed_ms < patrol.remaining_ms {
        patrol.remaining_ms -= elapsed_ms;
        let remaining_leg_ms = patrol
            .remaining_ms
            .saturating_sub(patrol.wait_ms)
            .min(patrol.leg_duration_ms);
        let progress = if patrol.leg_duration_ms == 0 {
            1.0
        } else {
            (patrol.leg_duration_ms - remaining_leg_ms) as f32 / patrol.leg_duration_ms as f32
        };
        patrol.current_position = std::array::from_fn(|index| {
            patrol.leg_start[index] + (patrol.position[index] - patrol.leg_start[index]) * progress
        });
        return None;
    }
    if patrol.waypoints.len() == 1 {
        patrol.current_position = patrol.position;
        patrol.remaining_ms = 0;
        return None;
    }
    let next_index = (patrol.waypoint_index + 1) % patrol.waypoints.len();
    let waypoint = patrol.waypoints[next_index];
    let target = [waypoint.x, waypoint.y, waypoint.z];
    let start = patrol.position;
    let duration_ms = patrol_leg_duration_ms(start, target);
    patrol.spline_id = patrol.spline_id.wrapping_add(1).max(1);
    let event = patrol_move(guid, start, target, patrol.spline_id, duration_ms);
    patrol.waypoint_index = next_index;
    patrol.leg_start = start;
    patrol.position = target;
    patrol.current_position = start;
    patrol.leg_duration_ms = duration_ms;
    patrol.wait_ms = waypoint.wait_ms;
    patrol.remaining_ms = duration_ms.saturating_add(waypoint.wait_ms);
    Some(event)
}

const SPELL_EFFECT_APPLY_AURA: u32 = 6;
const SPELL_AURA_PERIODIC_DAMAGE: u32 = 3;

/// Secondary mechanics a resolved cast may carry beside its direct effect: the builder's combo
/// points, the interrupt's school lockout, and a trigger (Charge's stun). Named here so a row
/// carrying one still resolves its direct arm instead of falling through unsupported.
fn is_secondary_mechanic(effect: u32) -> bool {
    matches!(
        effect,
        benilla_formats::SPELL_EFFECT_ADD_COMBO_POINTS
            | benilla_formats::SPELL_EFFECT_INTERRUPT_CAST
            | benilla_formats::SPELL_EFFECT_TRIGGER_SPELL
            | benilla_formats::SPELL_EFFECT_DUMMY
    )
}

/// How one spell's effects resolve: the direct damage/heal slot (absent on an aura-only row), an
/// optional percent-weapon slot merged into the same swing (Backstab's "150% weapon damage plus
/// 15"), and whether any arm is a weapon arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectPlan {
    direct: Option<usize>,
    percent: Option<usize>,
    has_weapon: bool,
}

fn plan_direct_effect(effects: [u32; 3], auras: [u32; 3]) -> Option<EffectPlan> {
    let mut direct = None;
    let mut percent = None;
    let mut has_weapon = false;
    let mut has_aura = false;
    for (slot, effect) in effects.into_iter().enumerate() {
        let weapon = matches!(
            effect,
            benilla_formats::SPELL_EFFECT_WEAPON_DAMAGE
                | benilla_formats::SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL
                | benilla_formats::SPELL_EFFECT_WEAPON_PERCENT_DAMAGE
                | benilla_formats::SPELL_EFFECT_NORMALIZED_WEAPON_DMG
        );
        match effect {
            0 => {}
            benilla_formats::SPELL_EFFECT_SCHOOL_DAMAGE
            | benilla_formats::SPELL_EFFECT_HEAL
            | benilla_formats::SPELL_EFFECT_WEAPON_DAMAGE
            | benilla_formats::SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL
            | benilla_formats::SPELL_EFFECT_NORMALIZED_WEAPON_DMG
            | benilla_formats::SPELL_EFFECT_SUMMON
            | benilla_formats::SPELL_EFFECT_SUMMON_TOTEM => {
                if direct.replace(slot).is_some() {
                    return None;
                }
                has_weapon |= weapon;
            }
            benilla_formats::SPELL_EFFECT_WEAPON_PERCENT_DAMAGE => {
                // A percent arm merges into the swing beside a flat one (Backstab 53); alone it
                // is the direct arm (its own percent is all the swing gets).
                if percent.replace(slot).is_some() {
                    return None;
                }
                has_weapon = true;
                if direct.is_none() {
                    direct = Some(slot);
                }
            }
            SPELL_EFFECT_APPLY_AURA => {
                // Auras resolve: periodic damage is the DoT, the movement-control auras become
                // control arms, the rest is buff/debuff state.
                has_aura = true;
            }
            effect if is_secondary_mechanic(effect) => {}
            _ => return None,
        }
    }
    // A row of pure secondary mechanics (a trigger alone, Charge's stun) resolves to nothing the
    // simulation can apply; it is an explicit unsupported blocker, not a silent no-op buff.
    if direct.is_none() && !has_aura {
        return None;
    }
    // A periodic-damage aura beside a heal arm is no player spell's shape; refuse it rather than
    // guess which arm wins. A DoT beside damage (Immolate) or alone (Corruption) resolves.
    let periodic_damage = (0..3).any(|slot| {
        effects[slot] == SPELL_EFFECT_APPLY_AURA && auras[slot] == SPELL_AURA_PERIODIC_DAMAGE
    });
    if periodic_damage
        && direct.is_some_and(|slot| effects[slot] == benilla_formats::SPELL_EFFECT_HEAL)
    {
        return None;
    }
    Some(EffectPlan {
        direct,
        percent,
        has_weapon,
    })
}

fn drive_offline_gameplay(
    channels: Res<OfflineChannels>,
    mut core: ResMut<OfflineGameCore>,
    mut persistence: ResMut<OfflinePersistence>,
    time: Res<Time>,
    player: Option<Res<crate::player::Player>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    skill_lines: Option<Res<crate::ui_spellbook::SkillLines>>,
    talents: Option<Res<crate::ui_talent::Talents>>,
    mut buyback: Option<ResMut<OfflineVendorBuyback>>,
) {
    if let Some(player) = player.as_deref() {
        sync_offline_player_pose(&mut core.0, player);
    }
    while let Ok(command) = channels.commands.try_recv() {
        match &command {
            crate::net::ClientCommand::ItemQuery { entry, .. } => {
                let info = item_info(*entry).map(Box::new);
                send_gameplay_events(
                    &channels,
                    [SessionEvent::ItemTemplate {
                        entry: *entry,
                        info,
                    }],
                );
                continue;
            }
            crate::net::ClientCommand::Loot { guid } => {
                open_offline_loot(&mut core.0, &channels, *guid);
                continue;
            }
            crate::net::ClientCommand::AutostoreLootItem { slot } => {
                info!("offline loot: received item-pickup command for slot {slot}");
                take_offline_loot_item(&mut core.0, &mut persistence, &channels, *slot);
                continue;
            }
            crate::net::ClientCommand::LootMoney => {
                take_offline_loot_money(&mut core.0, &mut persistence, &channels);
                continue;
            }
            crate::net::ClientCommand::LootRelease { guid } => {
                core.0.close_corpse_loot();
                send_gameplay_events(
                    &channels,
                    [SessionEvent::LootReleaseResponse { guid: *guid }],
                );
                continue;
            }
            crate::net::ClientCommand::GossipHello { guid } => {
                send_offline_gossip_menu(&mut core.0, &channels, buyback.as_deref_mut(), *guid);
                continue;
            }
            crate::net::ClientCommand::ListInventory { guid } => {
                send_offline_vendor_inventory(&core.0, &channels, buyback.as_deref_mut(), *guid);
                continue;
            }
            crate::net::ClientCommand::BuyItem {
                vendor,
                entry,
                count,
            } => {
                buy_offline_vendor_item(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    buyback.as_deref_mut(),
                    *vendor,
                    *entry,
                    *count,
                    None,
                );
                continue;
            }
            crate::net::ClientCommand::BuyItemInSlot {
                vendor,
                entry,
                bag_guid,
                bag_slot,
                count,
            } => {
                buy_offline_vendor_item(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    buyback.as_deref_mut(),
                    *vendor,
                    *entry,
                    *count,
                    Some((*bag_guid, *bag_slot)),
                );
                continue;
            }
            crate::net::ClientCommand::SellItem {
                vendor,
                item_guid,
                count,
            } => {
                sell_offline_item(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    buyback.as_deref_mut(),
                    *vendor,
                    *item_guid,
                    *count,
                );
                continue;
            }
            crate::net::ClientCommand::BuybackItem { vendor, slot } => {
                buyback_offline_item(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    buyback.as_deref_mut(),
                    *vendor,
                    *slot,
                );
                continue;
            }
            crate::net::ClientCommand::NpcTextQuery { text_id, guid } => {
                send_offline_npc_text(&core.0, &channels, *guid, *text_id);
                continue;
            }
            crate::net::ClientCommand::QuestgiverStatusQuery { npc } => {
                send_offline_questgiver_status(&core.0, &channels, *npc);
                continue;
            }
            crate::net::ClientCommand::QuestgiverHello { npc } => {
                send_offline_quest_greeting(&core.0, &channels, *npc);
                continue;
            }
            crate::net::ClientCommand::QuestgiverQuery { npc, quest } => {
                send_offline_quest_details(&core.0, &channels, *npc, *quest);
                continue;
            }
            crate::net::ClientCommand::QuestgiverAccept { npc, quest } => {
                accept_offline_quest(&mut core.0, &mut persistence, &channels, *npc, *quest);
                continue;
            }
            crate::net::ClientCommand::QuestgiverComplete { npc, quest } => {
                send_offline_quest_progress(&core.0, &channels, *npc, *quest);
                continue;
            }
            crate::net::ClientCommand::QuestgiverRequestReward { npc, quest } => {
                send_offline_quest_reward(&core.0, &channels, *npc, *quest);
                continue;
            }
            crate::net::ClientCommand::QuestgiverChooseReward { npc, quest, choice } => {
                reward_offline_quest(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    *npc,
                    *quest,
                    *choice,
                    talents.as_deref(),
                );
                continue;
            }
            crate::net::ClientCommand::QuestQuery { quest } => {
                send_offline_quest_template(&channels, *quest);
                continue;
            }
            crate::net::ClientCommand::TrainerList { trainer } => {
                send_offline_trainer_list(
                    &core.0,
                    &channels,
                    *trainer,
                    spells.as_deref(),
                    skill_lines.as_deref(),
                );
                continue;
            }
            crate::net::ClientCommand::TrainerBuySpell { trainer, spell_id } => {
                buy_offline_trainer_spell(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    *trainer,
                    *spell_id,
                    spells.as_deref(),
                    skill_lines.as_deref(),
                );
                continue;
            }
            crate::net::ClientCommand::LearnTalent { talent_id, rank } => {
                learn_offline_talent(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    talents.as_deref(),
                    skill_lines.as_deref(),
                    *talent_id,
                    *rank,
                );
                continue;
            }
            crate::net::ClientCommand::CancelCast { spell_id } => {
                if core.0.cancel_spell(*spell_id) {
                    save_after_command(&core.0, &mut persistence);
                }
                continue;
            }
            crate::net::ClientCommand::CancelAura { spell_id } => {
                cancel_offline_aura(&mut core.0, &mut persistence, &channels, *spell_id);
                continue;
            }
            crate::net::ClientCommand::SetWatchedFaction { rep_list_id } => {
                set_offline_watched_faction(&core.0, &channels, *rep_list_id);
                continue;
            }
            crate::net::ClientCommand::AttackSwing { guid } => {
                start_offline_auto_attack(&mut core.0, &channels, *guid);
                continue;
            }
            crate::net::ClientCommand::AttackStop => {
                stop_offline_auto_attack(&mut core.0, &channels);
                continue;
            }
            crate::net::ClientCommand::CancelAutoRepeat => {
                core.0.stop_auto_shot();
                continue;
            }
            crate::net::ClientCommand::CancelChannelling { spell_id: _ } => {
                // The client cancels its own channel bar; the simulation stops ticking.
                core.0.cancel_channel();
                continue;
            }
            crate::net::ClientCommand::RepopRequest => {
                respawn_offline_character(&mut core.0, &mut persistence, &channels);
                continue;
            }
            crate::net::ClientCommand::AutoEquipItem { bag_index, slot } => {
                offline_auto_equip(&mut core.0, &mut persistence, &channels, *bag_index, *slot);
                continue;
            }
            crate::net::ClientCommand::SwapInvItem { src_slot, dst_slot } => {
                offline_swap_inv(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    *src_slot,
                    *dst_slot,
                );
                continue;
            }
            crate::net::ClientCommand::SwapItem {
                dst_bag,
                dst_slot,
                src_bag,
                src_slot,
            } => {
                offline_swap_item(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    (*src_bag, *src_slot),
                    (*dst_bag, *dst_slot),
                );
                continue;
            }
            crate::net::ClientCommand::AutoStoreBagItem {
                src_bag, src_slot, ..
            } => {
                offline_store_bag(
                    &mut core.0,
                    &mut persistence,
                    &channels,
                    *src_bag,
                    *src_slot,
                );
                continue;
            }
            crate::net::ClientCommand::UseItem {
                bag_index, slot, ..
            } => {
                offline_use_item(&mut core.0, &mut persistence, &channels, *bag_index, *slot);
                continue;
            }
            crate::net::ClientCommand::DestroyItem {
                bag_index, slot, ..
            } => {
                offline_destroy_item(&mut core.0, &mut persistence, &channels, *bag_index, *slot);
                continue;
            }
            _ => {}
        }
        if matches!(
            &command,
            crate::net::ClientCommand::Logout | crate::net::ClientCommand::ForceLogout
        ) {
            if core.0.active().is_some() {
                match core.0.leave_world() {
                    Ok(_) => {
                        save_after_command(&core.0, &mut persistence);
                        send_gameplay_events(&channels, [SessionEvent::LoggedOut]);
                    }
                    Err(error) => warn!("offline gameplay: logout failed: {error}"),
                }
            }
            continue;
        }
        // The white ranged shots (Auto Shot 75, Shoot Bow, Throw, wand Shoot) are the ranged auto
        // attack: the client animates the draw, the simulation swings the ranged slot's weapon.
        if let crate::net::ClientCommand::CastSpell { spell_id, target } = &command {
            let white_shot = spells.as_deref().is_some_and(|spells| {
                spells
                    .catalog
                    .get(*spell_id)
                    .is_some_and(|spell| spell.melee_white_damage() || spell.auto_repeat())
            });
            if white_shot {
                start_offline_auto_shot(&mut core.0, &channels, *spell_id, *target);
                continue;
            }
        }
        let cast_data = match &command {
            crate::net::ClientCommand::CastSpell { spell_id, .. } => spells
                .as_deref()
                .and_then(|spells| {
                    let spell = spells.catalog.get(*spell_id)?;
                    let power = benilla_game_core::PowerType::from_spell_power(spell.power_type)?;
                    let caster_level = core
                        .0
                        .active()
                        .and_then(|id| core.0.snapshot(id))
                        .map(|snapshot| u32::from(snapshot.level))?;
                    let plan = plan_direct_effect(spell.effects, spell.effect_apply_aura)?;
                    // The cost is stated in the spell's own power: mana and energy spend it as
                    // written, rage is stored tenfold ("150 on the wire is 15 Rage"), and a
                    // health cost is an explicit unsupported blocker.
                    let raw_cost = spell.mana_cost_at_level(
                        caster_level,
                        if power == benilla_game_core::PowerType::Mana {
                            benilla_game_core::PLAYER_MAX_MANA
                        } else {
                            0
                        },
                    );
                    let cost = benilla_game_core::PowerCost {
                        kind: power,
                        amount: if power == benilla_game_core::PowerType::Rage {
                            raw_cost / 10
                        } else {
                            raw_cost
                        },
                    };
                    let periodic = (0..3).find_map(|slot| {
                        if spell.effects[slot] != SPELL_EFFECT_APPLY_AURA
                            || spell.effect_apply_aura[slot] != SPELL_AURA_PERIODIC_DAMAGE
                        {
                            return None;
                        }
                        let (min, max) = spell.base_effect_range(slot)?;
                        let tick_ms = spell.effect_amplitude[slot];
                        let duration_ms =
                            u32::try_from(spells.durations.get(spell.duration_index)?.base_ms)
                                .ok()?;
                        (tick_ms > 0 && duration_ms >= tick_ms).then_some(
                            benilla_game_core::PeriodicDamage {
                                min: u32::try_from(min).ok()?,
                                max: u32::try_from(max).ok()?,
                                tick_ms,
                                duration_ms,
                            },
                        )
                    });
                    // Movement control rides the row's aura arms (Hammer of Justice's stun,
                    // Frostbolt's snare) for the row's own duration.
                    let control = control_effect(&spells.durations, spell);
                    // A channeled row ticks its per-tick arm instead of landing a direct amount:
                    // periodic damage (Mind Flay), periodic leech (Drain Life), or the periodic
                    // trigger's own damage arm (Arcane Missiles 5143's trigger 7268).
                    let channel_effect = spell
                        .tooltip_channeled()
                        .then(|| {
                            (0..3).find_map(|slot| {
                                let tick_ms = spell.effect_amplitude[slot];
                                let (min, max) = match spell.effect_apply_aura[slot] {
                                    benilla_formats::SPELL_AURA_PERIODIC_DAMAGE
                                    | benilla_formats::SPELL_AURA_PERIODIC_LEECH => {
                                        spell.base_effect_range(slot)?
                                    }
                                    benilla_formats::SPELL_AURA_PERIODIC_TRIGGER_SPELL => {
                                        let trigger =
                                            spells.catalog.get(spell.effect_trigger_spell[slot])?;
                                        (0..3).find_map(|arm| {
                                            (trigger.effects[arm]
                                                == benilla_formats::SPELL_EFFECT_SCHOOL_DAMAGE)
                                                .then(|| trigger.base_effect_range(arm))
                                                .flatten()
                                        })?
                                    }
                                    _ => return None,
                                };
                                let duration_ms = u32::try_from(
                                    spells.durations.get(spell.duration_index)?.base_ms,
                                )
                                .ok()?;
                                let ticks = if tick_ms > 0 && duration_ms >= tick_ms {
                                    duration_ms / tick_ms
                                } else {
                                    0
                                };
                                (ticks > 0).then_some(
                                    benilla_game_core::DirectSpellEffect::Channel {
                                        tick_ms,
                                        ticks,
                                        min: u32::try_from(min).ok()?,
                                        max: u32::try_from(max).ok()?,
                                        school: 1_u32.checked_shl(spell.school)?,
                                        drain: spell.effect_apply_aura[slot]
                                            == benilla_formats::SPELL_AURA_PERIODIC_LEECH,
                                        control,
                                    },
                                )
                            })
                        })
                        .flatten();
                    // A summon row names its creature in `EffectMiscValue`; the sim arms the pet.
                    let summon = (0..3).find_map(|slot| {
                        (spell.effects[slot] == benilla_formats::SPELL_EFFECT_SUMMON)
                            .then(|| spell.effect_misc_value[slot].max(0) as u32)
                    });
                    // A totem-style summon names its creature the same way; the converted
                    // creature's own rows carry its behavior.
                    let totem = (0..3).find_map(|slot| {
                        (spell.effects[slot] == benilla_formats::SPELL_EFFECT_SUMMON_TOTEM)
                            .then(|| {
                                let entry = spell.effect_misc_value[slot].max(0) as u32;
                                Some((entry, benilla_game_core::summon::summon_template(entry)?))
                            })
                            .flatten()
                    });
                    let effect = if let Some((creature_entry, template)) = totem {
                        benilla_game_core::DirectSpellEffect::Totem {
                            creature_entry,
                            behavior: totem_behavior(
                                &spells.catalog,
                                &spells.durations,
                                spell,
                                template,
                            ),
                        }
                    } else if let Some(creature_entry) = summon {
                        benilla_game_core::DirectSpellEffect::Summon { creature_entry }
                    } else if let Some(channel) = channel_effect {
                        channel
                    } else if let Some(control) = control.filter(|_| plan.direct.is_none()) {
                        // A bare control spell (Hammer of Justice's stun) lands its control arm.
                        benilla_game_core::DirectSpellEffect::Control(control)
                    } else {
                        match plan.direct {
                            Some(slot)
                                if spell.effects[slot] == benilla_formats::SPELL_EFFECT_HEAL =>
                            {
                                let (min, max) = spell.base_effect_range(slot)?;
                                benilla_game_core::DirectSpellEffect::Heal {
                                    min: u32::try_from(min).ok()?,
                                    max: u32::try_from(max).ok()?,
                                }
                            }
                            Some(slot) => {
                                let (min, max) = spell.base_effect_range(slot)?;
                                let min = u32::try_from(min).ok()?;
                                let max = u32::try_from(max).ok()?;
                                let school = 1_u32.checked_shl(spell.school)?;
                                if plan.has_weapon {
                                    let percent_effect = plan
                                        .percent
                                        .filter(|slot| {
                                            spell.effects[*slot]
                                            == benilla_formats::SPELL_EFFECT_WEAPON_PERCENT_DAMAGE
                                        })
                                        .unwrap_or(slot);
                                    let percent_only = spell.effects[slot]
                                        == benilla_formats::SPELL_EFFECT_WEAPON_PERCENT_DAMAGE;
                                    let percent =
                                        u32::try_from(spell.effect_base_points[percent_effect] + 1)
                                            .ok()?;
                                    let bonus = if percent_only {
                                        benilla_game_core::WeaponBonus::of_percent(percent)
                                    } else {
                                        benilla_game_core::WeaponBonus { percent, min, max }
                                    };
                                    benilla_game_core::DirectSpellEffect::WeaponDamage {
                                        bonus,
                                        school,
                                        periodic,
                                    }
                                } else {
                                    benilla_game_core::DirectSpellEffect::Damage {
                                        min,
                                        max,
                                        school,
                                        periodic,
                                        control,
                                    }
                                }
                            }
                            None => benilla_game_core::DirectSpellEffect::Aura {
                                school: 1_u32.checked_shl(spell.school).unwrap_or(1),
                                periodic: None,
                                // The row's own lifetime; 0 (or the permanent sentinel) means the
                                // aura lasts until it is cancelled.
                                duration_ms: spells
                                    .durations
                                    .get(spell.duration_index)
                                    .and_then(|duration| u32::try_from(duration.base_ms).ok())
                                    .unwrap_or(0),
                            },
                        }
                    };
                    // Rogue mechanics: a finishing move spends the target's points, adding
                    // `EffectPointsPerComboPoint` each; a builder effect awards one per hit.
                    let combo = if spell.needs_combo_points() {
                        let per_point = spell
                            .effect_points_per_combo_point
                            .iter()
                            .fold(0.0_f32, |high, points| high.max(*points))
                            .round() as u32;
                        benilla_game_core::ComboEffect::Finisher { per_point }
                    } else if spell
                        .effects
                        .contains(&benilla_formats::SPELL_EFFECT_ADD_COMBO_POINTS)
                    {
                        benilla_game_core::ComboEffect::Build
                    } else {
                        benilla_game_core::ComboEffect::None
                    };
                    let range = benilla_formats::min_max_range(
                        spell,
                        spells.ranges.get(spell.range_index),
                        benilla_formats::RangeUnit::still(0.0),
                        benilla_formats::RangeTargets::default(),
                    );
                    let range_yards = match (effect, range) {
                        (benilla_game_core::DirectSpellEffect::Heal { .. }, None)
                        | (benilla_game_core::DirectSpellEffect::Aura { .. }, None) => 0.0,
                        (_, Some((_, max))) => max,
                        _ => return None,
                    };
                    Some((
                        cost,
                        spells.cast_time_ms(
                            spell,
                            caster_level,
                            &crate::spell::SpellModifiers::default(),
                        ),
                        spell.recovery_ms,
                        spell.start_recovery_ms,
                        range_yards,
                        effect,
                        combo,
                        spell.on_next_swing(),
                    ))
                })
                .or_else(|| {
                    (*spell_id == benilla_game_core::FIREBALL_SPELL_ID).then_some((
                        benilla_game_core::PowerCost::mana(benilla_game_core::FIREBALL_MANA_COST),
                        benilla_game_core::FIREBALL_CAST_MS,
                        0,
                        benilla_game_core::FIREBALL_CAST_MS,
                        benilla_game_core::FIREBALL_RANGE_YARDS,
                        benilla_game_core::DirectSpellEffect::Damage {
                            min: benilla_game_core::FIREBALL_RANK1_DAMAGE_MIN,
                            max: benilla_game_core::FIREBALL_RANK1_DAMAGE_MAX,
                            school: 4,
                            periodic: Some(benilla_game_core::PeriodicDamage {
                                min: benilla_game_core::FIREBALL_BURN_DAMAGE,
                                max: benilla_game_core::FIREBALL_BURN_DAMAGE,
                                tick_ms: benilla_game_core::FIREBALL_BURN_TICK_MS,
                                duration_ms: benilla_game_core::FIREBALL_BURN_MS,
                            }),
                            control: None,
                        },
                        benilla_game_core::ComboEffect::None,
                        false,
                    ))
                }),
            _ => None,
        };
        if let Some((
            cost,
            cast_time_ms,
            spell_cooldown_ms,
            cooldown_ms,
            range_yards,
            effect,
            combo,
            on_next_swing,
        )) = cast_data
        {
            let started = start_offline_cast(
                &mut core.0,
                &channels,
                command,
                cost,
                cast_time_ms,
                spell_cooldown_ms,
                cooldown_ms,
                range_yards,
                effect,
                combo,
                on_next_swing,
            );
            if started {
                save_after_command(&core.0, &mut persistence);
            }
        } else if let crate::net::ClientCommand::CastSpell { spell_id, .. } = &command {
            warn!("offline gameplay: spell {spell_id} has no available direct damage/heal handler");
        }
    }
    if core.0.active().is_none() {
        return;
    }
    let elapsed_ms = (time.delta_secs_f64() * 1000.0).round() as u32;
    let mut changed =
        advance_offline_combat_with_talents(&mut core.0, &channels, elapsed_ms, talents.as_deref());
    let expired_auras = core.0.advance_active_auras(elapsed_ms);
    if !expired_auras.is_empty() {
        publish_offline_auras(&core.0, &channels, &expired_auras);
        changed = true;
    }
    if changed {
        save_after_command(&core.0, &mut persistence);
    }
}

fn sync_offline_player_pose(core: &mut GameCore, player: &crate::player::Player) {
    if !player.active {
        return;
    }
    let Some(snapshot) = core.active().and_then(|id| core.snapshot(id)) else {
        return;
    };
    let [x, y, z] = benilla_assets::coords::bevy_to_wow(player.pos);
    let position = WorldPosition {
        map_id: snapshot.position.map_id,
        x,
        y,
        z,
        orientation: player.facing(),
    };
    if let Err(error) = core.update_active(position, 0) {
        debug!("offline gameplay: couldn't sync player pose before command: {error}");
    }
}

fn cancel_offline_aura(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    spell_id: u32,
) {
    let Some(aura) = core
        .active_auras()
        .and_then(|auras| auras.iter().find(|aura| aura.spell_id == spell_id))
        .copied()
    else {
        return;
    };
    core.remove_active_aura(aura.slot);
    publish_offline_auras(
        core,
        channels,
        &[CharacterAura {
            remaining_ms: Some(0),
            ..aura
        }],
    );
    save_after_command(core, persistence);
}

/// Round-trip `CMSG_SET_WATCHED_FACTION` offline: the watched slot is a server descriptor field
/// with no client mirror (`0x4d6b60`), so the answer is a `PLAYER_FIELD_WATCHED_FACTION_INDEX`
/// update, exactly the packet the reference's own `SetWatchedFactionIndex` waits for.
fn set_offline_watched_faction(core: &GameCore, channels: &OfflineChannels, rep_list_id: i32) {
    let Some(id) = core.active() else { return };
    send_gameplay_events(
        channels,
        [SessionEvent::ObjectValues {
            guid: id.presentation_guid(),
            fields: ObjectFields::from_pairs(&[(1261, rep_list_id as u32)]),
        }],
    );
}

fn publish_offline_auras(core: &GameCore, channels: &OfflineChannels, removed: &[CharacterAura]) {
    let Some(id) = core.active() else { return };
    let mut events = vec![SessionEvent::ObjectValues {
        guid: id.presentation_guid(),
        fields: ObjectFields::from_pairs(&character_aura_field_pairs(
            core.active_auras().unwrap_or_default(),
        )),
    }];
    events.extend(removed.iter().map(|aura| SessionEvent::AuraDuration {
        slot: aura.slot,
        remaining_ms: 0,
    }));
    send_gameplay_events(channels, events);
}

fn respawn_offline_character(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
) {
    if core.active_health() != 0 {
        return;
    }
    let Some(character) = core.active().and_then(|id| core.snapshot(id)).cloned() else {
        return;
    };
    let Some(position) = starting_position(character.appearance.race) else {
        warn!(
            "offline death recovery: no starting anchor for race {}",
            character.appearance.race
        );
        return;
    };
    let stopped_attack = core.stop_auto_attack();
    let snapshot = match core.respawn_active(position) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!("offline death recovery failed: {error}");
            return;
        }
    };
    let player_guid = snapshot.id.presentation_guid();
    let mut events = Vec::new();
    if let Some(spawn_id) = stopped_attack {
        if let Some(hostile) = core.hostile(spawn_id) {
            if let Some(victim) = combat_target_guid(&hostile) {
                events.push(SessionEvent::AttackStop {
                    attacker: player_guid,
                    victim,
                });
            }
        }
    }
    events.push(SessionEvent::ObjectValues {
        guid: player_guid,
        fields: ObjectFields::from_pairs(&[
            (22, benilla_game_core::PLAYER_MAX_HEALTH),
            (23, core.active_mana()),
        ]),
    });
    if character.position.map_id == position.map_id {
        events.push(SessionEvent::Teleport {
            guid: player_guid,
            counter: 0,
            position: [position.x, position.y, position.z],
            orientation: position.orientation,
        });
    } else {
        events.push(SessionEvent::Worldport {
            map_id: position.map_id,
            position: [position.x, position.y, position.z],
            orientation: position.orientation,
            needs_ack: false,
        });
    }
    send_gameplay_events(channels, events);
    save_after_command(core, persistence);
}

fn imported_spawn_for_guid(
    guid: u64,
) -> Option<&'static benilla_game_core::content::CreatureSpawn> {
    let entry = benilla_protocol::guid::entry(guid)?;
    let source_guid = (guid & 0x00ff_ffff) as u32;
    benilla_game_core::content::creature_content()
        .spawn
        .iter()
        .find(|spawn| spawn.entry == entry && spawn.source_guid & 0x00ff_ffff == source_guid)
}

#[allow(clippy::too_many_arguments)]
fn start_offline_cast(
    core: &mut GameCore,
    channels: &OfflineChannels,
    command: crate::net::ClientCommand,
    cost: benilla_game_core::PowerCost,
    cast_time_ms: u32,
    spell_cooldown_ms: u32,
    cooldown_ms: u32,
    range_yards: f32,
    effect: benilla_game_core::DirectSpellEffect,
    combo: benilla_game_core::ComboEffect,
    on_next_swing: bool,
) -> bool {
    let crate::net::ClientCommand::CastSpell { spell_id, target } = command else {
        return false;
    };
    let Some(id) = core.active().cloned() else {
        return false;
    };
    let player_guid = id.presentation_guid();
    let (presentation_target, cast_target) = match effect {
        benilla_game_core::DirectSpellEffect::Summon { .. }
        | benilla_game_core::DirectSpellEffect::Totem { .. } => (
            Some(player_guid),
            benilla_game_core::SpellTarget::SelfCharacter,
        ),
        benilla_game_core::DirectSpellEffect::Damage { .. }
        | benilla_game_core::DirectSpellEffect::WeaponDamage { .. }
        | benilla_game_core::DirectSpellEffect::Channel { .. }
        | benilla_game_core::DirectSpellEffect::Control(_) => {
            let Some(target) = target else { return false };
            let Some(spawn_id) = core
                .hostiles()
                .iter()
                .find(|hostile| combat_target_guid(hostile) == Some(target))
                .map(|hostile| hostile.spawn_id)
            else {
                return false;
            };
            (
                Some(target),
                benilla_game_core::SpellTarget::Hostile(spawn_id),
            )
        }
        benilla_game_core::DirectSpellEffect::Heal { .. } => match target {
            Some(t) if t != player_guid => {
                // The heal lands on a friendly unit sharing the imported spawn identity.
                let Some(ally) = core
                    .allies()
                    .iter()
                    .find(|ally| imported_guid(ally.creature_entry, ally.spawn_id) == t)
                else {
                    return false;
                };
                (Some(t), benilla_game_core::SpellTarget::Ally(ally.spawn_id))
            }
            _ => (
                Some(player_guid),
                benilla_game_core::SpellTarget::SelfCharacter,
            ),
        },
        benilla_game_core::DirectSpellEffect::Aura { .. } => {
            // Aura-only spells target self, a friendly unit, or the current hostile target.
            match target {
                Some(t) if t == player_guid => (
                    Some(player_guid),
                    benilla_game_core::SpellTarget::SelfCharacter,
                ),
                Some(t) => {
                    if let Some(ally) = core
                        .allies()
                        .iter()
                        .find(|ally| imported_guid(ally.creature_entry, ally.spawn_id) == t)
                    {
                        (Some(t), benilla_game_core::SpellTarget::Ally(ally.spawn_id))
                    } else {
                        let Some(spawn_id) = core
                            .hostiles()
                            .iter()
                            .find(|hostile| combat_target_guid(hostile) == Some(t))
                            .map(|hostile| hostile.spawn_id)
                        else {
                            return false;
                        };
                        (Some(t), benilla_game_core::SpellTarget::Hostile(spawn_id))
                    }
                }
                None => (
                    Some(player_guid),
                    benilla_game_core::SpellTarget::SelfCharacter,
                ),
            }
        }
    };
    match core.begin_cast(benilla_game_core::CastRequest {
        spell_id,
        target: cast_target,
        cost,
        cast_time_ms,
        spell_cooldown_ms,
        cooldown_ms,
        range_yards,
        effect,
        combo,
        on_next_swing,
    }) {
        Ok(benilla_game_core::CastOutcome::QueuedMelee) => {
            info!(
                "offline cast: spell {spell_id} queued on the next swing ({:?} cost {}/{})",
                cost.kind,
                cost.amount,
                core.active_power(cost.kind)
            );
            true
        }
        Ok(benilla_game_core::CastOutcome::Cast) => {
            info!(
                "offline cast: spell {spell_id} -> {cast_time_ms}ms ({:?} cost {})",
                cost.kind, cost.amount
            );
            let mut events = vec![SessionEvent::SpellStart {
                caster: player_guid,
                spell_id,
                cast_flags: 0x2,
                cast_time_ms,
                target: presentation_target,
                ammo_display_id: None,
            }];
            events.extend(player_power_events(player_guid, core));
            send_gameplay_events(channels, events);
            true
        }
        Err(error) => {
            debug!("offline gameplay: rejected spell {spell_id}: {error}");
            false
        }
    }
}

/// The player's four power pools as descriptor writes: mana 23, rage 24 (tenths in the
/// descriptor), focus 25, energy 26.
fn player_power_events(guid: u64, core: &GameCore) -> [SessionEvent; 1] {
    [SessionEvent::ObjectValues {
        guid,
        fields: ObjectFields::from_pairs(&[
            (22, core.active_health()),
            (23, core.active_mana()),
            (24, core.active_rage().saturating_mul(10)),
            (25, core.active_focus()),
            (26, core.active_energy()),
        ]),
    }]
}

fn start_offline_auto_attack(core: &mut GameCore, channels: &OfflineChannels, target: u64) {
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(spawn_id) = core
        .hostiles()
        .iter()
        .find(|hostile| combat_target_guid(hostile) == Some(target))
        .map(|hostile| hostile.spawn_id)
    else {
        return;
    };
    match core.start_auto_attack(spawn_id) {
        Ok(()) => send_gameplay_events(
            channels,
            [SessionEvent::AttackStart {
                attacker: id.presentation_guid(),
                victim: target,
            }],
        ),
        Err(error) => debug!("offline gameplay: rejected auto-attack at {target:#x}: {error}"),
    }
}

fn start_offline_auto_shot(
    core: &mut GameCore,
    channels: &OfflineChannels,
    spell_id: u32,
    target: Option<u64>,
) {
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(target) = target else {
        return;
    };
    let Some(spawn_id) = core
        .hostiles()
        .iter()
        .find(|hostile| combat_target_guid(hostile) == Some(target))
        .map(|hostile| hostile.spawn_id)
    else {
        return;
    };
    match core.start_auto_shot(spell_id, spawn_id) {
        Ok(()) => send_gameplay_events(
            channels,
            [SessionEvent::SpellStart {
                caster: id.presentation_guid(),
                spell_id,
                cast_flags: 0x2,
                cast_time_ms: 0,
                target: Some(target),
                ammo_display_id: None,
            }],
        ),
        Err(error) => {
            debug!("offline gameplay: rejected ranged shot {spell_id} at {target:#x}: {error}")
        }
    }
}

fn stop_offline_auto_attack(core: &mut GameCore, channels: &OfflineChannels) {
    let Some(spawn_id) = core.stop_auto_attack() else {
        return;
    };
    let Some(id) = core.active() else { return };
    let Some(hostile) = core.hostile(spawn_id) else {
        return;
    };
    let Some(victim) = combat_target_guid(&hostile) else {
        return;
    };
    send_gameplay_events(
        channels,
        [SessionEvent::AttackStop {
            attacker: id.presentation_guid(),
            victim,
        }],
    );
}

fn advance_offline_combat(
    core: &mut GameCore,
    channels: &OfflineChannels,
    elapsed_ms: u32,
) -> bool {
    advance_offline_combat_with_talents(core, channels, elapsed_ms, None)
}

fn advance_offline_combat_with_talents(
    core: &mut GameCore,
    channels: &OfflineChannels,
    elapsed_ms: u32,
    talents: Option<&crate::ui_talent::Talents>,
) -> bool {
    if core.active().is_none() {
        return false;
    }
    let pools_before = (
        core.active_health(),
        core.active_mana(),
        core.active_rage(),
        core.active_focus(),
        core.active_energy(),
    );
    let mut changed = false;
    let updates = core.advance_combat(elapsed_ms);
    for update in updates {
        match update {
            CombatUpdate::Impact(impact) => {
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![
                    SessionEvent::SpellGo {
                        caster: id.presentation_guid(),
                        spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                        cast_flags: 0x100,
                        hits: vec![guid],
                        misses: Vec::new(),
                        target: Some(guid),
                        go_target: None,
                        dest: None,
                        ammo_display_id: None,
                        item_caster: None,
                    },
                    SessionEvent::SpellDamageLog(benilla_protocol::messages::SpellDamageLog {
                        target: guid,
                        attacker: id.presentation_guid(),
                        spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                        damage: impact.damage,
                        school: 4,
                        absorb: 0,
                        resist: 0,
                        periodic: false,
                        blocked: 0,
                        hit_info: 0,
                    }),
                ];
                events.extend(hostile_spell_state_events(
                    guid,
                    &hostile,
                    impact
                        .killed
                        .then_some(benilla_game_core::FIREBALL_SPELL_ID),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::Experience {
                spawn_id,
                amount,
                level,
                levels_gained,
                experience_in_level,
                health_gained,
                mana_gained,
            } => {
                changed = true;
                let Some(id) = core.active().cloned() else {
                    continue;
                };
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(victim) = combat_target_guid(&hostile) else {
                    continue;
                };
                let Some(snapshot) = core.snapshot(&id) else {
                    continue;
                };
                let mut player_fields = vec![
                    (22, snapshot.health),
                    (34, u32::from(level)),
                    (716, experience_in_level),
                    (
                        717,
                        benilla_game_core::progression::xp_to_next_level(level).unwrap_or(0),
                    ),
                    (1102, free_talent_points(snapshot, talents)),
                ];
                if matches!(snapshot.appearance.class, 2 | 3 | 5 | 7 | 8 | 9 | 11) {
                    player_fields.push((23, snapshot.mana));
                }
                let mut events = vec![
                    SessionEvent::XpGain(benilla_protocol::messages::XpGain {
                        victim,
                        total: amount,
                        base: amount,
                        kill: true,
                    }),
                    SessionEvent::ObjectValues {
                        guid: id.presentation_guid(),
                        fields: ObjectFields::from_pairs(&player_fields),
                    },
                ];
                for (index, reached_level) in (level - levels_gained + 1..=level).enumerate() {
                    events.push(SessionEvent::LevelUp(
                        benilla_protocol::messages::LevelUpInfo {
                            level: u32::from(reached_level),
                            health: if index == 0 { health_gained as i32 } else { 0 },
                            powers: [if index == 0 { mana_gained as i32 } else { 0 }, 0, 0, 0, 0],
                            stats: [0; 5],
                        },
                    ));
                }
                send_gameplay_events(channels, events);
            }
            CombatUpdate::LootReady { spawn_id, has_loot } => {
                changed = true;
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                send_gameplay_events(
                    channels,
                    [SessionEvent::ObjectValues {
                        guid,
                        fields: ObjectFields::from_pairs(&[(143, u32::from(has_loot))]),
                    }],
                );
            }
            CombatUpdate::SpellImpact {
                spell_id,
                school,
                impact,
            } => {
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![
                    SessionEvent::SpellGo {
                        caster: id.presentation_guid(),
                        spell_id,
                        cast_flags: 0x100,
                        hits: vec![guid],
                        misses: Vec::new(),
                        target: Some(guid),
                        go_target: None,
                        dest: None,
                        ammo_display_id: None,
                        item_caster: None,
                    },
                    SessionEvent::SpellDamageLog(benilla_protocol::messages::SpellDamageLog {
                        target: guid,
                        attacker: id.presentation_guid(),
                        spell_id,
                        damage: impact.damage,
                        school: u8::try_from(school).unwrap_or(0),
                        absorb: 0,
                        resist: 0,
                        periodic: false,
                        blocked: 0,
                        hit_info: 0,
                    }),
                ];
                events.extend(hostile_spell_state_events(
                    guid,
                    &hostile,
                    impact.killed.then_some(spell_id),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::SpellHeal {
                spell_id,
                amount,
                remaining_health,
            } => {
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let guid = id.presentation_guid();
                send_gameplay_events(
                    channels,
                    [
                        SessionEvent::SpellGo {
                            caster: guid,
                            spell_id,
                            cast_flags: 0x100,
                            hits: vec![guid],
                            misses: Vec::new(),
                            target: Some(guid),
                            go_target: None,
                            dest: None,
                            ammo_display_id: None,
                            item_caster: None,
                        },
                        SessionEvent::SpellHealLog(benilla_protocol::messages::SpellHealLog {
                            target: guid,
                            healer: guid,
                            spell_id,
                            amount,
                            critical: false,
                        }),
                        SessionEvent::ObjectValues {
                            guid,
                            fields: ObjectFields::from_pairs(&[(22, remaining_health)]),
                        },
                    ],
                );
            }
            CombatUpdate::MeleeSpellImpact {
                spell_id,
                school,
                impact,
            } => {
                // A queued melee ability (Heroic Strike) taking over a swing: the spell's own
                // go and damage log, not the white swing's.
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![
                    SessionEvent::SpellGo {
                        caster: id.presentation_guid(),
                        spell_id,
                        cast_flags: 0x100,
                        hits: vec![guid],
                        misses: Vec::new(),
                        target: Some(guid),
                        go_target: None,
                        dest: None,
                        ammo_display_id: None,
                        item_caster: None,
                    },
                    SessionEvent::SpellDamageLog(benilla_protocol::messages::SpellDamageLog {
                        target: guid,
                        attacker: id.presentation_guid(),
                        spell_id,
                        damage: impact.damage,
                        school: u8::try_from(school).unwrap_or(0),
                        absorb: 0,
                        resist: 0,
                        periodic: false,
                        blocked: 0,
                        hit_info: 0,
                    }),
                ];
                events.extend(hostile_spell_state_events(
                    guid,
                    &hostile,
                    impact.killed.then_some(spell_id),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::RangedImpact { spell_id, impact } => {
                // A ranged white swing (Auto Shot, Shoot, Throw): the shot's own go and damage
                // log rides the white shot spell, not the melee attacker state.
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![
                    SessionEvent::SpellGo {
                        caster: id.presentation_guid(),
                        spell_id,
                        cast_flags: 0x100,
                        hits: vec![guid],
                        misses: Vec::new(),
                        target: Some(guid),
                        go_target: None,
                        dest: None,
                        ammo_display_id: None,
                        item_caster: None,
                    },
                    SessionEvent::SpellDamageLog(benilla_protocol::messages::SpellDamageLog {
                        target: guid,
                        attacker: id.presentation_guid(),
                        spell_id,
                        damage: impact.damage,
                        school: 0,
                        absorb: 0,
                        resist: 0,
                        periodic: false,
                        blocked: 0,
                        hit_info: 0,
                    }),
                ];
                events.extend(hostile_spell_state_events(
                    guid,
                    &hostile,
                    impact.killed.then_some(spell_id),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::ChannelStart {
                spell_id,
                duration_ms,
            } => {
                // The channel bar runs on the client's own timer from this start event.
                changed = true;
                send_gameplay_events(
                    channels,
                    [SessionEvent::ChannelStart {
                        spell_id,
                        duration_ms,
                    }],
                );
            }
            CombatUpdate::ChannelEnd { spell_id } => {
                changed = true;
                let _ = spell_id;
            }
            CombatUpdate::Summoned { creature_entry } => {
                // The pet's unit object and action bar come up together, as vmangos's
                // `Pet::Create` + `PetSpellInitialize` pair does.
                changed = true;
                let Some(template) = benilla_game_core::summon::summon_template(creature_entry)
                else {
                    continue;
                };
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(position) = core.snapshot(id).map(|snapshot| snapshot.position) else {
                    continue;
                };
                let guid = summon_guid(creature_entry);
                let fields = [
                    (2, 0x9), // OBJECT_FIELD_TYPE: object and unit
                    (3, creature_entry),
                    (4, template.scale.to_bits()),
                    (22, template.health),
                    (28, template.health),
                    (34, 1),
                    (35, template.faction),
                    (130, 1.5f32.to_bits()),
                    (131, template.display_id),
                    (132, template.display_id),
                ];
                send_gameplay_events(
                    channels,
                    [
                        SessionEvent::ObjectCreate {
                            guid,
                            kind: EntityKind::Unit,
                            display_id: Some(template.display_id),
                            position: [position.x, position.y, position.z],
                            orientation: position.orientation,
                            scale: template.scale,
                            speeds: None,
                            mover: None,
                            transport_progress: None,
                            transport: None,
                            spline: None,
                            fields: ObjectFields::from_pairs(&fields)
                                .into_created(benilla_protocol::messages::ObjectType::Unit),
                        },
                        SessionEvent::PetSpells(Box::new(summon_pet_bar(guid))),
                    ],
                );
            }
            CombatUpdate::SummonImpact {
                creature_entry,
                impact,
            } => {
                // The pet's white swing: its own attacker state from its unit guid.
                changed = true;
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(victim) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![SessionEvent::AttackerState(
                    benilla_protocol::messages::AttackerState {
                        attacker: summon_guid(creature_entry),
                        victim,
                        hit_info: 0,
                        damage: impact.damage,
                        victim_state: 1,
                        absorb: 0,
                        resist: 0,
                        blocked: 0,
                        melee_spell_id: 0,
                        school: 0,
                    },
                )];
                events.extend(hostile_spell_state_events(
                    victim,
                    &hostile,
                    impact.killed.then_some(0),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::SummonDismissed { creature_entry } => {
                // The pet object leaves and the bar goes with it (a zero-guid `PetSpells` is the
                // teardown).
                changed = true;
                send_gameplay_events(
                    channels,
                    [
                        SessionEvent::ObjectDestroyed(summon_guid(creature_entry)),
                        SessionEvent::PetSpells(Box::new(benilla_protocol::messages::PetSpells {
                            ..Default::default()
                        })),
                    ],
                );
            }
            CombatUpdate::TotemSummoned {
                spell_id,
                creature_entry,
                buff_spell: _,
            } => {
                // The totem's unit stands at its owner's feet for the row's own lifetime. Its
                // passive buff is held as totem state; the buff's numbers and icon are not
                // modeled (RULES.md, "Totems").
                changed = true;
                let Some(template) = benilla_game_core::summon::summon_template(creature_entry)
                else {
                    continue;
                };
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(position) = core.snapshot(id).map(|snapshot| snapshot.position) else {
                    continue;
                };
                let guid = imported_guid(creature_entry, spell_id);
                let fields = [
                    (2, 0x9), // OBJECT_FIELD_TYPE: object and unit
                    (3, creature_entry),
                    (4, template.scale.to_bits()),
                    (22, template.health),
                    (28, template.health),
                    (34, 1),
                    (35, template.faction),
                    (130, 1.5f32.to_bits()),
                    (131, template.display_id),
                    (132, template.display_id),
                ];
                send_gameplay_events(
                    channels,
                    [SessionEvent::ObjectCreate {
                        guid,
                        kind: EntityKind::Unit,
                        display_id: Some(template.display_id),
                        position: [position.x, position.y, position.z],
                        orientation: position.orientation,
                        scale: template.scale,
                        speeds: None,
                        mover: None,
                        transport_progress: None,
                        transport: None,
                        spline: None,
                        fields: ObjectFields::from_pairs(&fields)
                            .into_created(benilla_protocol::messages::ObjectType::Unit),
                    }],
                );
            }
            CombatUpdate::TotemDismissed {
                spell_id,
                creature_entry,
            } => {
                changed = true;
                send_gameplay_events(
                    channels,
                    [SessionEvent::ObjectDestroyed(imported_guid(
                        creature_entry,
                        spell_id,
                    ))],
                );
            }
            CombatUpdate::TotemImpact {
                spell_id,
                creature_entry,
                school,
                impact,
            } => {
                // The totem's pulse rides its own attacker state from its unit guid.
                changed = true;
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(victim) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![SessionEvent::AttackerState(
                    benilla_protocol::messages::AttackerState {
                        attacker: imported_guid(creature_entry, spell_id),
                        victim,
                        hit_info: 0,
                        damage: impact.damage,
                        victim_state: 1,
                        absorb: 0,
                        resist: 0,
                        blocked: 0,
                        melee_spell_id: spell_id,
                        school: u8::try_from(school).unwrap_or(0),
                    },
                )];
                events.extend(hostile_spell_state_events(
                    victim,
                    &hostile,
                    impact.killed.then_some(0),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::TotemHeal {
                spell_id,
                creature_entry,
                amount,
                remaining_health,
            } => {
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let guid = id.presentation_guid();
                let healer = imported_guid(creature_entry, spell_id);
                send_gameplay_events(
                    channels,
                    [
                        SessionEvent::SpellGo {
                            caster: healer,
                            spell_id,
                            cast_flags: 0x100,
                            hits: vec![guid],
                            misses: Vec::new(),
                            target: Some(guid),
                            go_target: None,
                            dest: None,
                            ammo_display_id: None,
                            item_caster: None,
                        },
                        SessionEvent::SpellHealLog(benilla_protocol::messages::SpellHealLog {
                            target: guid,
                            healer,
                            spell_id,
                            amount,
                            critical: false,
                        }),
                        SessionEvent::ObjectValues {
                            guid,
                            fields: ObjectFields::from_pairs(&[(22, remaining_health)]),
                        },
                    ],
                );
            }
            CombatUpdate::MeleeImpact(impact) => {
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(victim) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![SessionEvent::AttackerState(
                    benilla_protocol::messages::AttackerState {
                        attacker: id.presentation_guid(),
                        victim,
                        hit_info: 0,
                        damage: impact.damage,
                        victim_state: 1,
                        absorb: 0,
                        resist: 0,
                        blocked: 0,
                        melee_spell_id: 0,
                        school: 0,
                    },
                )];
                events.extend(hostile_spell_state_events(
                    victim,
                    &hostile,
                    impact.killed.then_some(0),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::DotImpact(impact) => {
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![SessionEvent::SpellDamageLog(
                    benilla_protocol::messages::SpellDamageLog {
                        target: guid,
                        attacker: id.presentation_guid(),
                        spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                        damage: impact.damage,
                        school: 4,
                        absorb: 0,
                        resist: 0,
                        periodic: true,
                        blocked: 0,
                        hit_info: 0,
                    },
                )];
                events.extend(hostile_spell_state_events(
                    guid,
                    &hostile,
                    Some(benilla_game_core::FIREBALL_SPELL_ID),
                ));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::PeriodicImpact {
                spell_id,
                school,
                impact,
            } => {
                changed = true;
                let Some(id) = core.active() else {
                    continue;
                };
                let Some(hostile) = core.hostile(impact.spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                let mut events = vec![SessionEvent::SpellDamageLog(
                    benilla_protocol::messages::SpellDamageLog {
                        target: guid,
                        attacker: id.presentation_guid(),
                        spell_id,
                        damage: impact.damage,
                        school: u8::try_from(school).unwrap_or(0),
                        absorb: 0,
                        resist: 0,
                        periodic: true,
                        blocked: 0,
                        hit_info: 0,
                    },
                )];
                events.extend(hostile_spell_state_events(guid, &hostile, Some(spell_id)));
                send_gameplay_events(channels, events);
            }
            CombatUpdate::QuestObjective(update) => {
                let Some(id) = core.active().cloned() else {
                    continue;
                };
                let Some(snapshot) = core.snapshot(&id) else {
                    continue;
                };
                let mut events = vec![
                    SessionEvent::QuestObjectiveKill {
                        quest_id: update.quest_id,
                        entry: update.creature_entry,
                        count: update.count,
                        required: update.required,
                    },
                    SessionEvent::ObjectValues {
                        guid: id.presentation_guid(),
                        fields: ObjectFields::from_pairs(&quest_log_slot_fields_for(
                            snapshot,
                            update.quest_id,
                        )),
                    },
                ];
                if update.complete {
                    events.push(SessionEvent::QuestObjectivesComplete {
                        quest_id: update.quest_id,
                    });
                    if let Some(definition) =
                        benilla_game_core::quest::quest_definition(update.quest_id)
                    {
                        if let Some(giver_entry) = definition.giver_creature_entries.first() {
                            if let Some(giver) = quest_giver_guid(core, *giver_entry) {
                                events.push(SessionEvent::QuestGiverStatus {
                                    npc: giver,
                                    status: benilla_protocol::messages::dialog_status::REWARD2,
                                });
                            }
                        }
                    }
                }
                send_gameplay_events(channels, events);
                changed = true;
            }
            CombatUpdate::Respawned { spawn_id } => {
                changed = true;
                if core.active().is_none() {
                    continue;
                }
                if let Some(hostile) = core.hostile(spawn_id) {
                    let Some(guid) = combat_target_guid(&hostile) else {
                        continue;
                    };
                    let event = SessionEvent::ObjectValues {
                        guid,
                        fields: ObjectFields::from_pairs(&[(22, hostile.health), (143, 0)]),
                    };
                    send_gameplay_events(channels, [event]);
                }
            }
            CombatUpdate::AllyHeal {
                spawn_id,
                remaining_health,
                ..
            } => {
                changed = true;
                let Some(ally) = core.ally(spawn_id) else {
                    continue;
                };
                let guid = imported_guid(ally.creature_entry, ally.spawn_id);
                send_gameplay_events(
                    channels,
                    [SessionEvent::ObjectValues {
                        guid,
                        fields: ObjectFields::from_pairs(&[(22, remaining_health)]),
                    }],
                );
            }
            CombatUpdate::AllyAura {
                spawn_id,
                applied,
                remaining_ms,
                ..
            } => {
                // Friendly buff/debuff state lives in the simulation; the NPC aura descriptor
                // block is not written yet, so only the simulation changes here.
                changed = true;
                let _ = (spawn_id, applied, remaining_ms);
            }
            CombatUpdate::PlayerAura {
                slot, remaining_ms, ..
            } => {
                // The buff's aura block goes up with the cast; a timed buff carries its remaining
                // lifetime, and a permanent one shows the icon with no timer.
                changed = true;
                publish_offline_auras(core, channels, &[]);
                if let Some(remaining_ms) = remaining_ms {
                    send_gameplay_events(
                        channels,
                        [SessionEvent::AuraDuration { slot, remaining_ms }],
                    );
                }
            }
            CombatUpdate::AttackStopped { spawn_id } => {
                // The client stays engaged until this echo; without it the reference's
                // already-attacking skip keeps it from ever sending another swing — the state a
                // kill leaves behind.
                changed = true;
                let victim = core
                    .hostile(spawn_id)
                    .and_then(|hostile| combat_target_guid(&hostile));
                if let (Some(victim), Some(id)) = (victim, core.active().cloned()) {
                    send_gameplay_events(
                        channels,
                        [SessionEvent::AttackStop {
                            attacker: id.presentation_guid(),
                            victim,
                        }],
                    );
                }
            }
        }
    }
    let _ = core.advance_power_regen(elapsed_ms);
    // The pools move with swings, taken hits and regen alike: publish whenever they changed this
    // round so the client's power bars follow every gain and spend.
    let pools_now = (
        core.active_health(),
        core.active_mana(),
        core.active_rage(),
        core.active_focus(),
        core.active_energy(),
    );
    if pools_now != pools_before {
        if let Some(id) = core.active() {
            send_gameplay_events(channels, player_power_events(id.presentation_guid(), core));
        }
    }
    changed
}

fn tick_offline_creature_ai(
    channels: Res<OfflineChannels>,
    mut core: ResMut<OfflineGameCore>,
    mut patrols: ResMut<OfflinePatrols>,
    time: Res<Time>,
    mut elapsed: Local<u32>,
) {
    if core.0.active().is_none() {
        *elapsed = 0;
        return;
    }
    *elapsed = elapsed.saturating_add((time.delta_secs_f64() * 1_000.0).round() as u32);
    if *elapsed < OFFLINE_CREATURE_AI_STEP_MS {
        return;
    }
    let elapsed_ms = std::mem::replace(&mut *elapsed, 0);
    advance_offline_creature_ai(&mut core.0, &channels, &mut patrols, elapsed_ms);
}

fn advance_offline_creature_ai(
    core: &mut GameCore,
    channels: &OfflineChannels,
    patrols: &mut OfflinePatrols,
    elapsed_ms: u32,
) {
    let Some(player) = core.active().and_then(|id| core.snapshot(id)) else {
        return;
    };
    let player_guid = player.id.presentation_guid();
    let updates = core.advance_creature_ai(elapsed_ms);
    for update in updates {
        match update {
            CreatureAiUpdate::Aggro { spawn_id } => {
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                patrols.0.remove(&guid);
                send_gameplay_events(
                    channels,
                    [SessionEvent::AiReaction {
                        unit: guid,
                        reaction: 2,
                    }],
                );
            }
            CreatureAiUpdate::AttackStart { spawn_id } => {
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                send_gameplay_events(
                    channels,
                    [SessionEvent::AttackStart {
                        attacker: guid,
                        victim: player_guid,
                    }],
                );
            }
            CreatureAiUpdate::AttackStop { spawn_id } => {
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                send_gameplay_events(
                    channels,
                    [SessionEvent::AttackStop {
                        attacker: guid,
                        victim: player_guid,
                    }],
                );
            }
            CreatureAiUpdate::Move {
                spawn_id,
                start,
                end,
                spline_id,
                duration_ms,
            } => {
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                send_gameplay_events(
                    channels,
                    [SessionEvent::MonsterMove {
                        guid,
                        transport: None,
                        start: [start.x, start.y, start.z],
                        spline_id,
                        path: vec![[start.x, start.y, start.z], [end.x, end.y, end.z]],
                        facing: benilla_protocol::MonsterMoveFacing::Angle(end.orientation),
                        stop: false,
                        duration_ms,
                        flying: false,
                        run_mode: true,
                    }],
                );
            }
            CreatureAiUpdate::MeleeImpact {
                spawn_id,
                damage,
                remaining_player_health,
            } => {
                let Some(hostile) = core.hostile(spawn_id) else {
                    continue;
                };
                let Some(guid) = combat_target_guid(&hostile) else {
                    continue;
                };
                send_gameplay_events(
                    channels,
                    [
                        SessionEvent::AttackerState(benilla_protocol::messages::AttackerState {
                            attacker: guid,
                            victim: player_guid,
                            hit_info: 0,
                            damage,
                            victim_state: 1,
                            absorb: 0,
                            resist: 0,
                            blocked: 0,
                            melee_spell_id: 0,
                            school: 0,
                        }),
                        SessionEvent::ObjectValues {
                            guid: player_guid,
                            fields: ObjectFields::from_pairs(&[(22, remaining_player_health)]),
                        },
                    ],
                );
                // The hit moves the pools too: health down, warrior rage up.
                send_gameplay_events(channels, player_power_events(player_guid, core));
            }
        }
    }
}

fn send_gameplay_events(
    channels: &OfflineChannels,
    events: impl IntoIterator<Item = SessionEvent>,
) {
    for event in events {
        if channels.events.send(event).is_err() {
            error!("offline gameplay adapter: event receiver was dropped");
            break;
        }
    }
}

fn save_after_command(core: &GameCore, persistence: &mut OfflinePersistence) {
    persistence.queue(core);
}

fn save_on_exit(core: Res<OfflineGameCore>, mut persistence: ResMut<OfflinePersistence>) {
    persistence.queue(&core.0);
    persistence.finish();
}

/// Stream the converted population in as the player travels: once the pose has moved
/// [`OFFLINE_STREAM_STEP_YARDS`], extend the simulation with `populate_near` and emit an
/// `ObjectCreate` (plus its patrol) for every spawn not yet streamed. Nothing is removed
/// (project decision): a mob stays once seen.
fn stream_offline_spawns(
    channels: Res<OfflineChannels>,
    mut core: ResMut<OfflineGameCore>,
    creatures: Option<Res<crate::entities::Creatures>>,
    mut names: ResMut<crate::names::NameCache>,
    mut patrols: ResMut<OfflinePatrols>,
    mut streamed: ResMut<OfflineStreamedSpawns>,
    mut last: Local<Option<WorldPosition>>,
) {
    let Some(id) = core.0.active().cloned() else {
        *last = None;
        return;
    };
    let Some(position) = core.0.snapshot(&id).map(|snapshot| snapshot.position) else {
        return;
    };
    let moved = last.is_none_or(|last| {
        (last.x - position.x).powi(2) + (last.y - position.y).powi(2)
            > OFFLINE_STREAM_STEP_YARDS * OFFLINE_STREAM_STEP_YARDS
    });
    if !moved {
        return;
    }
    *last = Some(position);
    core.0.populate_near(position, OFFLINE_CONTENT_RADIUS_YARDS);
    let content = benilla_game_core::content::creature_content();
    let mut events = Vec::new();
    for spawn in content.spawns_near(
        position.map_id,
        position.x,
        position.y,
        OFFLINE_CONTENT_RADIUS_YARDS,
    ) {
        if streamed
            .0
            .contains(&imported_guid(spawn.entry, spawn.source_guid))
        {
            continue;
        }
        if let Some(guid) = emit_imported_spawn(
            &mut events,
            content,
            spawn,
            creatures.as_deref(),
            core.0.hostiles(),
            &[],
            &mut names,
            &mut patrols,
        ) {
            streamed.0.insert(guid);
        }
    }
    send_gameplay_events(&channels, events);
}

fn update_local_player(
    time: Res<Time>,
    player: Res<crate::player::Player>,
    channels: Option<Res<OfflineChannels>>,
    mut core: ResMut<OfflineGameCore>,
    mut persistence: ResMut<OfflinePersistence>,
    mut save_elapsed: Local<f32>,
) {
    persistence.poll();
    let Some(id) = core.0.active().cloned() else {
        return;
    };
    if player.active {
        let [x, y, z] = benilla_assets::coords::bevy_to_wow(player.pos);
        let Some(snapshot) = core.0.snapshot(&id) else {
            return;
        };
        let position = benilla_game_core::WorldPosition {
            map_id: snapshot.position.map_id,
            x,
            y,
            z,
            orientation: player.facing(),
        };
        let elapsed_ms = (time.delta_secs_f64() * 1000.0).round() as u64;
        if let Err(error) = core.0.update_active(position, elapsed_ms) {
            warn!("offline simulation rejected player state: {error}");
            return;
        }
        for quest_id in core.0.drain_failed_quests() {
            if let Some(channels) = channels.as_deref() {
                send_gameplay_events(
                    channels,
                    [SessionEvent::QuestFailed {
                        quest_id,
                        timed: true,
                    }],
                );
            }
        }
        *save_elapsed += time.delta_secs();
    }
    if *save_elapsed >= 5.0 {
        *save_elapsed = 0.0;
        save_after_command(&core.0, &mut persistence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// Swing the project baseline in tests that assert swing numbers: the main hand goes to the
    /// backpack, so the assertion is not written against one race's converted starter weapon
    /// (mirrors `benilla-game-core`'s test helper).
    fn disarm(core: &mut GameCore) {
        let _ = core.unequip_to_backpack(15);
    }

    fn test_reactions(race: u8) -> benilla_game_core::UnitReactions {
        use benilla_game_core::UnitReaction::{Friendly, Hostile, Neutral};

        let (hostile, neutral): (&[u32], &[u32]) = if race == 10 {
            (&[12, 14, 16, 38, 91, 114], &[7, 15, 188, 189, 1703])
        } else if race == 1 {
            (&[17], &[25, 31])
        } else {
            (&[], &[])
        };
        benilla_game_core::UnitReactions::from_pairs(
            hostile
                .iter()
                .copied()
                .map(|faction| (faction, Hostile))
                .chain(neutral.iter().copied().map(|faction| (faction, Neutral)))
                .chain([(1604, Friendly)]),
        )
    }

    fn enter_test_world(core: &mut GameCore, race: u8) {
        core.set_unit_reactions(test_reactions(race));
        core.enter_world().unwrap();
    }

    fn northshire_hostile(core: &GameCore) -> benilla_game_core::HostileState {
        core.hostiles()
            .iter()
            .find(|hostile| hostile.creature_entry == 38)
            .copied()
            .expect("Northshire population includes a Defias Thug")
    }

    #[test]
    fn cast_resolver_rejects_unsupported_secondary_effects() {
        assert_eq!(
            plan_direct_effect([2, 6, 0], [0, 3, 0]),
            Some(EffectPlan {
                direct: Some(0),
                percent: None,
                has_weapon: false,
            }),
            "Fireball's direct and periodic damage are both supported"
        );
        assert_eq!(
            plan_direct_effect([6, 2, 0], [33, 0, 0]),
            Some(EffectPlan {
                direct: Some(1),
                percent: None,
                has_weapon: false,
            }),
            "Frostbolt's slow aura is supported as a secondary effect"
        );
        assert_eq!(
            plan_direct_effect([2, 6, 0], [0, 33, 0]),
            Some(EffectPlan {
                direct: Some(0),
                percent: None,
                has_weapon: false,
            }),
            "damage with a slow aura is supported"
        );
        assert_eq!(
            plan_direct_effect([2, 10, 0], [0, 0, 0]),
            None,
            "a cast with multiple direct effects is not modeled"
        );
        assert_eq!(
            plan_direct_effect([121, 31, 80], [0, 0, 0]),
            Some(EffectPlan {
                direct: Some(0),
                percent: Some(1),
                has_weapon: true,
            }),
            "Backstab's percent arm merges into its flat weapon arm, beside the builder"
        );
        assert_eq!(
            plan_direct_effect([17, 0, 0], [0, 0, 0]),
            Some(EffectPlan {
                direct: Some(0),
                percent: None,
                has_weapon: true,
            }),
            "Heroic Strike's weapon-damage-noschool arm resolves"
        );
        assert_eq!(
            plan_direct_effect([62, 6, 0], [0, 0, 0]),
            None,
            "mana burn (62) is not a weapon arm and nothing models it yet"
        );
        assert_eq!(
            plan_direct_effect([64, 0, 0], [0, 0, 0]),
            None,
            "a row of pure secondary mechanics resolves to nothing the simulation can apply"
        );
    }

    /// Phase 7's coverage audit over the converted starter set: every castable starter spell of
    /// every race and class resolves through a named handler. Passive rows never cast, the Attack
    /// pseudo-spell routes to auto-attack, and learn wrappers are the trainer system's own.
    #[test]
    fn every_castable_starter_spell_resolves_through_a_named_handler() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("spell catalog");
        let mut blockers = Vec::new();
        let mut resolved = 0usize;
        for race in 1..=11u8 {
            for class in 1..=11u8 {
                for spell_id in benilla_game_core::progression::starter_spell_ids(race, class) {
                    if spell_id == benilla_game_core::ATTACK_SPELL_ID {
                        continue;
                    }
                    let Some(spell) = catalog.get(spell_id) else {
                        blockers.push((race, class, spell_id, String::from("missing row")));
                        continue;
                    };
                    if spell.passive
                        || spell.cast_ui != 0
                        || spell
                            .effects
                            .contains(&benilla_formats::SPELL_EFFECT_LEARN_SPELL)
                    {
                        continue;
                    }
                    if plan_direct_effect(spell.effects, spell.effect_apply_aura).is_some() {
                        resolved += 1;
                    } else {
                        blockers.push((race, class, spell_id, spell.name.clone()));
                    }
                }
            }
        }
        assert!(
            resolved > 50,
            "castable starter spells resolved through named handlers: {resolved}"
        );
        assert!(
            blockers.is_empty(),
            "castable starter spells without a handler (a spellbook entry is not proof its \
             effect works): {blockers:?}"
        );
    }

    /// The audit's acquisition slice: every trainer-offered and talent-learned spell resolves
    /// through a named handler, or every effect it carries names one of these explicit
    /// unsupported families. The pinned list is the only way a player-acquirable spell may fall
    /// through, so the coverage can only shrink knowingly.
    #[test]
    fn trainer_and_talent_spells_resolve_or_name_an_unsupported_family() {
        fn unaccounted_families(
            catalog: &benilla_formats::SpellCatalog,
            spell_id: u32,
            unaccounted: &mut Vec<(u32, String, u32)>,
        ) -> usize {
            let Some(spell) = catalog.get(spell_id) else {
                unaccounted.push((spell_id, String::from("missing row"), 0));
                return 1;
            };
            if spell.passive || spell.cast_ui != 0 || spell_id == benilla_game_core::ATTACK_SPELL_ID
            {
                return 0;
            }
            if plan_direct_effect(spell.effects, spell.effect_apply_aura).is_none() {
                for effect in spell.effects.into_iter().filter(|effect| *effect != 0) {
                    if !BLOCKERS.contains(&effect) {
                        unaccounted.push((spell_id, spell.name.clone(), effect));
                    }
                }
            }
            // A summon arm resolves only through a converted creature entry.
            for slot in 0..3 {
                let effect = spell.effects[slot];
                if (effect == benilla_formats::SPELL_EFFECT_SUMMON
                    || effect == benilla_formats::SPELL_EFFECT_SUMMON_TOTEM)
                    && benilla_game_core::summon::summon_template(
                        spell.effect_misc_value[slot].max(0) as u32,
                    )
                    .is_none()
                {
                    unaccounted.push((spell_id, spell.name.clone(), effect));
                }
            }
            1
        }

        // Explicit unsupported effect families (RULES.md "Spell Effect Coverage"): dummy-armed
        // and health-cost mechanics, mana burn and resource restore, charge-like movement,
        // summoned world objects, the area and mana-leech channel arms, and the acquisition and
        // tool families the trainer and profession systems own.
        const BLOCKERS: &[u32] = &[
            benilla_formats::SPELL_EFFECT_DUMMY,
            benilla_formats::SPELL_EFFECT_ENERGIZE,
            benilla_formats::SPELL_EFFECT_MANA_BURN,
            benilla_formats::SPELL_EFFECT_CHARGE,
            benilla_formats::SPELL_EFFECT_CREATE_ITEM,
            benilla_formats::SPELL_EFFECT_ENCHANT_ITEM,
            benilla_formats::SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY,
            benilla_formats::SPELL_EFFECT_LEARN_SPELL,
            benilla_formats::SPELL_EFFECT_LEARN_PET_SPELL,
            benilla_formats::SPELL_EFFECT_SKILL_STEP,
            benilla_formats::SPELL_EFFECT_SKINNING,
            benilla_formats::SPELL_EFFECT_TRADE_SKILL,
            benilla_formats::SPELL_EFFECT_PROSPECTING,
            benilla_formats::SPELL_EFFECT_SUMMON_OBJECT_WILD,
            39, // SPELL_EFFECT_LANGUAGE (the language-learning family)
            5,  // SPELL_EFFECT_TELEPORT_UNITS (Hearthstone 8690)
            27, // SPELL_EFFECT_PERSISTENT_AREA_AURA (Blizzard 10's area arm)
            33, // SPELL_EFFECT_OPEN_LOCK (Pick Lock 1804)
        ];
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("spell catalog");
        let talent_catalog =
            benilla_formats::load_talent_catalog(&mut chain).expect("load Talent/TalentTab");
        let mut audited = 0usize;
        let mut unaccounted = Vec::new();
        for offer in benilla_game_core::progression::progression_data()
            .trainer_spell
            .iter()
        {
            let taught = catalog
                .learned_spell(offer.spell_id)
                .unwrap_or(offer.spell_id);
            audited += unaccounted_families(&catalog, taught, &mut unaccounted);
        }
        for race in 1..=11 {
            for class in 1..=11 {
                for tab in talent_catalog.tabs_for_class(race, class) {
                    for talent in talent_catalog.talents_in_tab(tab.id) {
                        for spell_id in talent.ranks.iter().copied().filter(|id| *id != 0) {
                            audited += unaccounted_families(&catalog, spell_id, &mut unaccounted);
                        }
                    }
                }
            }
        }
        assert!(
            audited > 100,
            "acquisition spells audited through the resolver: {audited}"
        );
        assert!(
            unaccounted.is_empty(),
            "player-acquirable spells without a handler (the pinned families are the only \
             allowed blockers): {unaccounted:?}"
        );
    }

    /// Acceptance: a friendly spell resolves on self and on an ally through the adapter's target
    /// mapping, instead of vanishing: the self heal reports on the player and a friendly buff
    /// lands in the ally's simulation state.
    #[test]
    fn self_and_ally_casts_map_their_targets_through_the_adapter() {
        let id = CharacterId::parse("00000000000000000000000000000005").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Solaen",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        core.enter_world().unwrap();
        let player_guid = id.presentation_guid();
        let ally = core.allies().first().expect("converted friendly NPC");
        let ally_guid = imported_guid(ally.creature_entry, ally.spawn_id);
        let ally_spawn = ally.spawn_id;

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };

        // The self-cast path: the spell aims at the player and its heal resolves on the player.
        assert!(start_offline_cast(
            &mut core,
            &channels,
            crate::net::ClientCommand::CastSpell {
                spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                target: Some(player_guid),
            },
            benilla_game_core::PowerCost::mana(10),
            0,
            0,
            0,
            0.0,
            benilla_game_core::DirectSpellEffect::Heal { min: 20, max: 20 },
            benilla_game_core::ComboEffect::None,
            false,
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::SpellStart { target: Some(guid), .. } if guid == player_guid
        ));
        advance_offline_combat(&mut core, &channels, 1);
        let heal_events: Vec<_> = received.try_iter().collect();
        assert!(
            heal_events.iter().any(|event| matches!(
                event,
                SessionEvent::SpellHealLog(log)
                    if log.target == player_guid && log.healer == player_guid
            )),
            "the self heal resolves on the player: {heal_events:?}"
        );

        // The ally path: a friendly unit's guid maps to its simulation spawn, and the buff lands
        // in the ally's state instead of being swallowed.
        assert!(start_offline_cast(
            &mut core,
            &channels,
            crate::net::ClientCommand::CastSpell {
                spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                target: Some(ally_guid),
            },
            benilla_game_core::PowerCost::mana(5),
            0,
            0,
            0,
            30.0,
            benilla_game_core::DirectSpellEffect::Aura {
                school: 1,
                periodic: None,
                duration_ms: 0,
            },
            benilla_game_core::ComboEffect::None,
            false,
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::SpellStart { target: Some(guid), .. } if guid == ally_guid
        ));
        advance_offline_combat(&mut core, &channels, 1);
        assert!(
            core.ally(ally_spawn)
                .expect("the ally is in the simulation")
                .auras
                .iter()
                .any(|aura| aura.spell_id == benilla_game_core::FIREBALL_SPELL_ID),
            "the friendly buff lands on the ally"
        );
    }

    #[test]
    fn game_core_stays_engine_free_and_is_available_to_the_adapter() {
        let core = OfflineGameCore::default();
        assert_eq!(core.0, benilla_game_core::GameCore::default());
    }

    #[test]
    fn offline_unequip_moves_worn_armor_to_the_backpack_and_announces_the_slot() {
        use crossbeam_channel::unbounded;
        let mut core = OfflineGameCore::default();
        let appearance = benilla_game_core::Appearance {
            race: 10,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let start = benilla_game_core::WorldPosition {
            map_id: 530,
            x: 10_349.6,
            y: -6_357.3,
            z: 33.4,
            orientation: 0.0,
        };
        core.0
            .create_character(
                benilla_game_core::CharacterId::parse("0".repeat(31) + "1").unwrap(),
                "Alara",
                appearance,
                start,
            )
            .unwrap();
        core.0.enter_world().unwrap();
        assert_eq!(core.0.active_armor(), 5, "the starting outfit is worn");

        let (_picks_tx, picks) = unbounded();
        let (events, events_rx) = unbounded();
        let (_commands_tx, commands) = unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        let mut persistence = OfflinePersistence::new(false);

        offline_store_bag(
            &mut core.0,
            &mut persistence,
            &channels,
            BAG_PLAYER_INVENTORY,
            4,
        );

        assert!(
            !core
                .0
                .active_equipment()
                .unwrap()
                .iter()
                .any(|item| item.slot == 4),
            "the robe left the chest slot"
        );
        assert!(
            core.0
                .active_inventory()
                .unwrap()
                .iter()
                .any(|item| item.entry == 20893),
            "the robe is now in the backpack"
        );
        assert_eq!(core.0.active_armor(), 2, "only the pants remain worn");
        assert!(
            matches!(events_rx.try_recv(), Ok(SessionEvent::ObjectValues { .. })),
            "the adapter publishes the changed item fields"
        );
    }

    /// The overhead marker and the minimap quest dot both key off the dialog status: a rewardable
    /// quest must answer `REWARD2` (7), the only status the minimap classifier dots (`quest_dot_cell`
    /// compares to 7) and the gold `?` the marker layer draws — never `REWARD_REP`'s light-blue `?`.
    #[test]
    fn rewardable_quests_map_to_status_seven_for_the_marker_and_minimap_dot() {
        use benilla_game_core::quest::QuestOfferState;
        use benilla_protocol::messages::dialog_status;
        assert_eq!(
            quest_status_icon(QuestOfferState::Available),
            dialog_status::AVAILABLE
        );
        assert_eq!(
            quest_status_icon(QuestOfferState::Incomplete),
            dialog_status::INCOMPLETE
        );
        assert_eq!(
            quest_status_icon(QuestOfferState::Rewardable),
            dialog_status::REWARD2
        );
    }

    #[test]
    fn waypoint_patrols_walk_between_source_points_at_the_local_walk_speed() {
        assert_eq!(
            patrol_leg_duration_ms([0.0, 0.0, 0.0], [5.0, 0.0, 0.0]),
            2_000
        );
        assert_eq!(patrol_leg_duration_ms([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]), 1);
        let event = patrol_move(42, [1.0, 2.0, 3.0], [4.0, 5.0, 6.0], 7, 2_000);
        let SessionEvent::MonsterMove {
            guid,
            start,
            path,
            duration_ms,
            run_mode,
            ..
        } = event
        else {
            panic!("patrol leg must use the creature spline event");
        };
        assert_eq!(guid, 42);
        assert_eq!(start, [1.0, 2.0, 3.0]);
        assert_eq!(path, [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        assert_eq!(duration_ms, 2_000);
        assert!(!run_mode);
    }

    #[test]
    fn waypoint_patrol_scheduler_honors_waits_and_loops_to_the_first_point() {
        let mut patrol = ActivePatrol {
            waypoints: vec![
                benilla_game_core::content::CreatureWaypoint {
                    source_guid: 7,
                    point: 1,
                    x: 2.5,
                    y: 0.0,
                    z: 0.0,
                    orientation: 0.0,
                    wait_ms: 100,
                },
                benilla_game_core::content::CreatureWaypoint {
                    source_guid: 7,
                    point: 2,
                    x: 7.5,
                    y: 0.0,
                    z: 0.0,
                    orientation: 0.0,
                    wait_ms: 250,
                },
            ],
            waypoint_index: 0,
            leg_start: [0.0, 0.0, 0.0],
            position: [2.5, 0.0, 0.0],
            current_position: [2.5, 0.0, 0.0],
            leg_duration_ms: 1_000,
            wait_ms: 100,
            remaining_ms: 1_100,
            spline_id: 1,
        };

        assert!(advance_patrol(42, &mut patrol, 1_099).is_none());
        assert_eq!(patrol.remaining_ms, 1);
        assert_eq!(patrol.current_position, [2.5, 0.0, 0.0]);
        assert!(matches!(
            advance_patrol(42, &mut patrol, 1),
            Some(SessionEvent::MonsterMove {
                guid: 42,
                start,
                spline_id: 2,
                path,
                duration_ms: 2_000,
                run_mode: false,
                ..
            }) if start == [2.5, 0.0, 0.0]
                && path == [[2.5, 0.0, 0.0], [7.5, 0.0, 0.0]]
        ));
        assert_eq!(patrol.waypoint_index, 1);
        assert_eq!(patrol.leg_start, [2.5, 0.0, 0.0]);
        assert_eq!(patrol.current_position, [2.5, 0.0, 0.0]);
        assert_eq!(patrol.leg_duration_ms, 2_000);
        assert_eq!(patrol.wait_ms, 250);
        assert_eq!(patrol.remaining_ms, 2_250);

        assert!(advance_patrol(42, &mut patrol, 2_249).is_none());
        assert_eq!(patrol.remaining_ms, 1);
        assert_eq!(patrol.current_position, [7.5, 0.0, 0.0]);
        assert!(matches!(
            advance_patrol(42, &mut patrol, 1),
            Some(SessionEvent::MonsterMove {
                guid: 42,
                start,
                spline_id: 3,
                path,
                duration_ms: 2_000,
                run_mode: false,
                ..
            }) if start == [7.5, 0.0, 0.0]
                && path == [[7.5, 0.0, 0.0], [2.5, 0.0, 0.0]]
        ));
        assert_eq!(patrol.waypoint_index, 0);
        assert_eq!(patrol.remaining_ms, 2_100);
    }

    #[test]
    fn combat_position_tracks_interpolated_patrol_pose_before_aggro() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let start = starting_position(1).unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id,
            "Alaera",
            Appearance {
                race: 1,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            start,
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let hostile = northshire_hostile(&core);
        let spawn_id = hostile.spawn_id;
        core.update_active(
            WorldPosition {
                x: hostile.position.x + 10.0,
                ..hostile.position
            },
            0,
        )
        .unwrap();
        let spawn = [hostile.position.x, hostile.position.y, hostile.position.z];
        let waypoint = |point, x| benilla_game_core::content::CreatureWaypoint {
            source_guid: spawn_id,
            point,
            x,
            y: spawn[1],
            z: spawn[2],
            orientation: 0.0,
            wait_ms: 0,
        };
        let guid = imported_guid(hostile.creature_entry, spawn_id);
        let mut patrols = OfflinePatrols::default();
        patrols.0.insert(
            guid,
            ActivePatrol {
                waypoints: vec![waypoint(1, spawn[0] + 5.0), waypoint(2, spawn[0] + 10.0)],
                waypoint_index: 0,
                leg_start: spawn,
                position: [spawn[0] + 5.0, spawn[1], spawn[2]],
                current_position: spawn,
                leg_duration_ms: 2_000,
                wait_ms: 0,
                remaining_ms: 2_000,
                spline_id: 3,
            },
        );
        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, _received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(patrols)
        .insert_resource(Time::<()>::default());
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(1_000));
        app.world_mut()
            .run_system_once(advance_offline_patrols)
            .expect("patrol pose synchronizes with combat state");

        let mut core = app.world_mut().resource_mut::<OfflineGameCore>();
        let current = core.0.hostile(spawn_id).unwrap().position;
        assert_eq!(current.x, spawn[0] + 2.5);
        assert_eq!(core.0.hostile(spawn_id).unwrap().spline_id, 3);
        let updates = core.0.advance_creature_ai(200);
        assert!(updates.iter().any(|update| matches!(
            update,
            CreatureAiUpdate::Move { spawn_id: actual, start, spline_id, .. }
                if *actual == spawn_id && *start == current && *spline_id == 4
        )));
    }

    #[test]
    fn new_characters_start_in_their_race_area_including_humans_in_northshire() {
        let human = starting_position(1).unwrap();
        assert_eq!(human.map_id, 0);
        assert_eq!((human.x, human.y, human.z), (-8949.95, -132.49, 83.53));
        assert_eq!(starting_position(10).unwrap().map_id, 530);
        assert_eq!(starting_position(11).unwrap().map_id, 530);
        for race in [2, 3, 4, 5, 6, 7, 8] {
            let start = starting_position(race).unwrap();
            assert!(start.x.is_finite() && start.y.is_finite() && start.z.is_finite());
        }
        assert!(starting_position(9).is_none());
    }

    #[test]
    fn offline_world_entry_emits_the_nearby_imported_population_with_stable_ids() {
        let content = benilla_game_core::content::creature_content();
        let template_entries: std::collections::BTreeSet<_> = content
            .template
            .iter()
            .map(|template| template.entry)
            .collect();
        for (race, name) in [(10, "Alaera"), (1, "Aldric")] {
            let appearance = Appearance {
                race,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            };
            let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
            let mut core = GameCore::default();
            core.create_character(
                id.clone(),
                name,
                appearance,
                starting_position(race).unwrap(),
            )
            .unwrap();
            core.set_unit_reactions(test_reactions(race));
            let snapshot = core.enter_world().unwrap();

            let (_pick_tx, picks) = crossbeam_channel::unbounded();
            let (events, received) = crossbeam_channel::unbounded();
            let (_command_tx, commands) = crossbeam_channel::unbounded();
            let mut names = crate::names::NameCache::default();
            let mut patrols = OfflinePatrols::default();
            let mut streamed = std::collections::HashSet::new();
            emit_world_entry_with_talents(
                &OfflineChannels {
                    picks,
                    events,
                    commands,
                },
                &snapshot,
                core.hostiles(),
                None,
                None,
                None,
                None,
                &mut names,
                &mut patrols,
                &mut streamed,
            );
            let events: Vec<_> = received.try_iter().collect();

            let player_fields = events
                .iter()
                .find_map(|event| match event {
                    SessionEvent::ObjectCreate {
                        kind: EntityKind::Player,
                        fields,
                        ..
                    } => Some(fields),
                    _ => None,
                })
                .expect("world entry emits the player create");
            assert_eq!(
                player_fields.player_watched_faction(),
                Some(-1),
                "a fresh character watches nothing: absent fields zero-fill, and slot 0 is the \
                 Bloodsail Buccaneers, so the sentinel must be written or the watch bar replaces \
                 the XP bar"
            );

            let nearby: Vec<_> = content
                .spawns_near(
                    snapshot.position.map_id,
                    snapshot.position.x,
                    snapshot.position.y,
                    OFFLINE_CONTENT_RADIUS_YARDS,
                )
                .filter(|spawn| {
                    content
                        .template(spawn.entry)
                        .is_some_and(|template| !is_dnd_test_creature(template))
                })
                .collect();
            let expected: BTreeMap<_, _> = nearby
                .iter()
                .map(|spawn| (imported_guid(spawn.entry, spawn.source_guid), spawn.entry))
                .collect();
            assert_eq!(expected.len(), nearby.len(), "imported identity collision");
            let imported_units: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    SessionEvent::ObjectCreate {
                        guid,
                        kind: EntityKind::Unit,
                        fields,
                        ..
                    } => fields
                        .object_entry()
                        .filter(|entry| template_entries.contains(entry))
                        .map(|entry| (*guid, entry)),
                    _ => None,
                })
                .collect();
            let emitted: BTreeMap<_, _> = imported_units.iter().copied().collect();
            assert_eq!(
                emitted.len(),
                imported_units.len(),
                "duplicate emitted identity"
            );
            assert_eq!(
                emitted, expected,
                "emitted population must match nearby source spawns"
            );
            for spawn in &nearby {
                let template = content.template(spawn.entry).unwrap();
                assert_eq!(
                    names.peek(imported_guid(spawn.entry, spawn.source_guid)),
                    Some(template.name.as_str()),
                    "every streamed creature must have its converted name cached"
                );
            }
            assert!(patrols.0.values().any(|patrol| patrol.waypoints.len() > 1));

            {
                // Travel beyond the entry radius: the next query streams the newly-near spawns
                // on this map and never repeats a GUID the entry already emitted.
                let entry_guids: std::collections::HashSet<u64> = events
                    .iter()
                    .filter_map(|event| match event {
                        SessionEvent::ObjectCreate {
                            guid,
                            kind: EntityKind::Unit,
                            ..
                        } => Some(*guid),
                        _ => None,
                    })
                    .collect();
                let distance = |spawn: &benilla_game_core::content::CreatureSpawn| {
                    (spawn.x - snapshot.position.x).powi(2)
                        + (spawn.y - snapshot.position.y).powi(2)
                };
                let far = content
                    .spawn
                    .iter()
                    .filter(|spawn| {
                        spawn.map_id == snapshot.position.map_id
                            && content
                                .template(spawn.entry)
                                .is_some_and(|template| !is_dnd_test_creature(template))
                            && distance(spawn)
                                > OFFLINE_CONTENT_RADIUS_YARDS * OFFLINE_CONTENT_RADIUS_YARDS
                    })
                    .min_by(|a, b| distance(a).total_cmp(&distance(b)))
                    .expect("a converted spawn beyond the entry radius");
                core.update_active(
                    WorldPosition {
                        map_id: far.map_id,
                        x: far.x,
                        y: far.y,
                        z: far.z,
                        orientation: far.orientation,
                    },
                    0,
                )
                .unwrap();

                let (_pick_tx, picks) = crossbeam_channel::unbounded();
                let (events, received) = crossbeam_channel::unbounded();
                let (_command_tx, commands) = crossbeam_channel::unbounded();
                let mut app = App::new();
                app.insert_resource(OfflineChannels {
                    picks,
                    events,
                    commands,
                })
                .insert_resource(OfflineGameCore(core))
                .insert_resource(OfflineStreamedSpawns(streamed.clone()))
                .insert_resource(patrols)
                .insert_resource(names);
                app.world_mut()
                    .run_system_once(stream_offline_spawns)
                    .expect("the travel stream runs");
                let streamed_events: Vec<_> = received.try_iter().collect();
                let fresh: Vec<_> = streamed_events
                    .iter()
                    .filter_map(|event| match event {
                        SessionEvent::ObjectCreate {
                            guid,
                            kind: EntityKind::Unit,
                            fields,
                            ..
                        } => fields
                            .object_entry()
                            .filter(|entry| template_entries.contains(entry))
                            .map(|entry| (*guid, entry)),
                        _ => None,
                    })
                    .collect();
                assert!(
                    !fresh.is_empty(),
                    "travel shows population the entry radius never reached"
                );
                assert!(
                    fresh.iter().all(|(guid, _)| !entry_guids.contains(guid)),
                    "streaming must not re-create what the entry already showed"
                );
                let unique: std::collections::HashSet<u64> =
                    fresh.iter().map(|(guid, _)| *guid).collect();
                assert_eq!(
                    unique.len(),
                    fresh.len(),
                    "no duplicate creates in one stream"
                );
                assert!(
                    fresh
                        .iter()
                        .any(|(guid, _)| *guid == imported_guid(far.entry, far.source_guid)),
                    "the spawn the player travelled toward is among the streamed creates"
                );
            }
        }
    }

    #[test]
    fn imported_spawn_identity_resolves_outside_sunstrider() {
        let content = benilla_game_core::content::creature_content();
        let spawn = content
            .spawn
            .iter()
            .find(|spawn| spawn.map_id != 530)
            .expect("the full world pack includes non-Sunstrider maps");
        let resolved = imported_spawn_for_guid(imported_guid(spawn.entry, spawn.source_guid))
            .expect("a non-Sunstrider unit GUID resolves to its converted spawn");

        assert_eq!(resolved.map_id, spawn.map_id);
        assert_eq!(resolved.entry, spawn.entry);
        assert_eq!(resolved.source_guid, spawn.source_guid);
    }

    /// The "Show as Experience Bar" box's `SetWatchedFactionIndex` reaches the offline adapter as
    /// `CMSG_SET_WATCHED_FACTION`; watching is not optimistic (`0x4d6b60`), so the answer must be
    /// the descriptor update stock `ReputationFrame.xml` waits for.
    #[test]
    fn offline_watched_faction_command_answers_with_the_descriptor_update() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id,
            "Alaera",
            Appearance {
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        let player_guid = core.active().unwrap().presentation_guid();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (command_tx, commands) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());

        command_tx
            .send(crate::net::ClientCommand::SetWatchedFaction { rep_list_id: 20 })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("watch command handled");
        let events: Vec<_> = received.try_iter().collect();
        let watched = events.iter().find_map(|event| match event {
            SessionEvent::ObjectValues { guid, fields } if *guid == player_guid => {
                fields.player_watched_faction()
            }
            _ => None,
        });
        assert_eq!(
            watched,
            Some(20),
            "the watched slot comes back as a field update: {events:?}"
        );

        command_tx
            .send(crate::net::ClientCommand::SetWatchedFaction {
                rep_list_id: benilla_protocol::messages::WATCHED_FACTION_NONE,
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("unwatch command handled");
        let events: Vec<_> = received.try_iter().collect();
        let watched = events.iter().find_map(|event| match event {
            SessionEvent::ObjectValues { guid, fields } if *guid == player_guid => {
                fields.player_watched_faction()
            }
            _ => None,
        });
        assert_eq!(watched, Some(-1), "unticking sends -1, not slot 0");
    }

    #[test]
    fn offline_gossip_hello_command_reaches_erona_menu() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id,
            "Alaera",
            Appearance {
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);

        let erona = benilla_game_core::content::creature_content()
            .spawn
            .iter()
            .find(|spawn| spawn.map_id == 530 && spawn.entry == 15_278)
            .expect("converted Magistrix Erona spawn");
        let erona_guid = imported_guid(erona.entry, erona.source_guid);
        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        commands
            .send(crate::net::ClientCommand::GossipHello { guid: erona_guid })
            .unwrap();

        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline GossipHello command is handled");

        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::GossipMenu { npc, text_id: 7_781, quests, .. }
                if npc == erona_guid
                    && quests.iter().any(|(quest, icon, _, title)| {
                        *quest == 8_325
                            && *icon == benilla_protocol::messages::dialog_status::AVAILABLE
                            && title == "Reclaiming Sunstrider Isle"
                    })
        ));
    }

    #[test]
    fn a_gossip_questgiver_with_no_gossip_text_opens_its_quest_list() {
        // Conservator Ilthalaine (2079) carries `UNIT_NPC_FLAG_GOSSIP` but has no converted gossip
        // menu, so her map to a level-1 night elf is a textless `SMSG_GOSSIP_MESSAGE` (`text_id`
        // 0) whose only content is the quest list. The right-click routes through the gossip arm
        // (GOSSIP precedes QUESTGIVER), so the client must open that menu instead of stalling on
        // the absent NPC text: that stall left her marker drawn and every click unanswered.
        let id = CharacterId::parse("0000000000000000000000000000000e").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id,
            "Anaya",
            Appearance {
                race: 4,
                class: 4,
                gender: 1,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(4).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 4);

        let spawn = benilla_game_core::content::creature_content()
            .spawn
            .iter()
            .find(|spawn| spawn.map_id == 1 && spawn.entry == 2079)
            .expect("converted Conservator Ilthalaine spawn");
        let guid = imported_guid(spawn.entry, spawn.source_guid);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };

        assert!(
            benilla_game_core::quest::gossip_for(2079).is_none(),
            "Ilthalaine has no converted gossip text"
        );
        send_offline_gossip_menu(&mut core, &channels, None, guid);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::GossipMenu { npc, text_id: 0, quests, .. }
                if npc == guid
                    && quests.iter().any(|(quest, icon, _, title)| {
                        *quest == 456
                            && *icon == benilla_protocol::messages::dialog_status::AVAILABLE
                            && title == "The Balance of Nature"
                    })
        ));
    }

    #[test]
    fn a_completed_turn_in_row_is_active_in_the_gossip_menu() {
        // The reported Durotar flow: Kaltunk (10176) starts 4641 and Gornek (3143) turns it in.
        // Gornek has `UNIT_NPC_FLAG_GOSSIP` and no gossip text, so his menu is `text_id` 0, and
        // the completed row must carry `REWARD_REP` (4) — which the client's `{3,4}` test reads as
        // active — not the overhead marker's `REWARD2` (7), which reads as an available quest and
        // answers the click with a detail query the server refuses.
        let id = CharacterId::parse("0000000000000000000000000000000f").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id,
            "Sdaaaz",
            Appearance {
                race: 2,
                class: 1,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(2).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 2);

        let content = benilla_game_core::content::creature_content();
        let spawn = |entry: u32| {
            content
                .spawn
                .iter()
                .find(|spawn| spawn.map_id == 1 && spawn.entry == entry)
                .unwrap_or_else(|| panic!("converted spawn for entry {entry}"))
        };
        let kaltunk = spawn(10176);
        let gornek = spawn(3143);
        let kaltunk_guid = imported_guid(kaltunk.entry, kaltunk.source_guid);
        let gornek_guid = imported_guid(gornek.entry, gornek.source_guid);

        core.update_active(
            WorldPosition {
                map_id: 1,
                x: kaltunk.x,
                y: kaltunk.y,
                z: kaltunk.z,
                orientation: kaltunk.orientation,
            },
            0,
        )
        .unwrap();
        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        let mut persistence = OfflinePersistence::new(false);
        accept_offline_quest(&mut core, &mut persistence, &channels, kaltunk_guid, 4641);
        assert!(
            core.active_quest_journal()
                .unwrap()
                .progress(4641)
                .is_some(),
            "Kaltunk offers 4641 to a level-1 orc"
        );
        let _ = received.try_iter().count();

        send_offline_gossip_menu(&mut core, &channels, None, gornek_guid);
        let menu = received
            .try_iter()
            .find_map(|event| match event {
                SessionEvent::GossipMenu {
                    npc,
                    text_id,
                    quests,
                    ..
                } if npc == gornek_guid => Some((text_id, quests)),
                _ => None,
            })
            .expect("Gornek opens a gossip menu");
        assert_eq!(menu.0, 0, "no converted gossip text");
        assert!(menu.1.iter().any(|(quest, icon, _, _)| {
            *quest == 4641 && *icon == benilla_protocol::messages::dialog_status::REWARD_REP
        }));
    }

    #[test]
    fn offline_gossip_hello_on_a_trainer_opens_its_trainer_list() {
        let content = benilla_game_core::content::creature_content();
        let spawn = content
            .spawn
            .iter()
            .find(|spawn| spawn.map_id == 530 && spawn.entry == 15_285)
            .expect("converted Pathstalker Kariel spawn");
        let id = CharacterId::parse("0000000000000000000000000000000c").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id,
            "Shalara",
            Appearance {
                race: 10,
                class: 4,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        core.update_active(
            WorldPosition {
                map_id: spawn.map_id,
                x: spawn.x,
                y: spawn.y,
                z: spawn.z,
                orientation: spawn.orientation,
            },
            0,
        )
        .unwrap();
        let guid = imported_guid(spawn.entry, spawn.source_guid);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        commands
            .send(crate::net::ClientCommand::GossipHello { guid })
            .unwrap();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("trainer GossipHello is handled");

        assert!(received.try_iter().any(|event| matches!(
            event,
            SessionEvent::TrainerList { trainer, services, .. }
                if trainer == guid && !services.is_empty()
        )));
    }

    #[test]
    fn offline_quest_interaction_enforces_range_and_completes_the_stock_reward_flow() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            Appearance {
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);

        let erona = benilla_game_core::content::creature_content()
            .spawn
            .iter()
            .find(|spawn| spawn.map_id == 530 && spawn.entry == 15_278)
            .expect("converted Magistrix Erona spawn");
        let erona_guid = imported_guid(erona.entry, erona.source_guid);
        let erona_position = WorldPosition {
            map_id: erona.map_id,
            x: erona.x,
            y: erona.y,
            z: erona.z,
            orientation: erona.orientation,
        };
        let start_position = core.snapshot(&id).unwrap().position;
        let distance_sq = (start_position.x - erona_position.x).powi(2)
            + (start_position.y - erona_position.y).powi(2)
            + (start_position.z - erona_position.z).powi(2);
        assert!(distance_sq > OFFLINE_INTERACTION_RANGE_YARDS * OFFLINE_INTERACTION_RANGE_YARDS);
        assert_eq!(quest_npc_entry(&core, erona_guid), Some(15_278));
        assert_eq!(quest_npc_in_range(&core, erona_guid), None);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        send_offline_questgiver_status(&core, &channels, erona_guid);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::QuestGiverStatus { npc, status }
                if npc == erona_guid
                    && status == benilla_protocol::messages::dialog_status::AVAILABLE
        ));
        send_offline_gossip_menu(&mut core, &channels, None, erona_guid);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::GossipMenu { npc, text_id: 7_781, quests, .. }
                if npc == erona_guid
                    && quests.iter().any(|(quest, icon, _, title)| {
                        *quest == 8_325
                            && *icon == benilla_protocol::messages::dialog_status::AVAILABLE
                            && title == "Reclaiming Sunstrider Isle"
                    })
        ));
        send_offline_npc_text(&core, &channels, erona_guid, 7_781);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::NpcGreeting { text_id: 7_781, blocks }
                if blocks[0].female.contains("Sunstrider Isle")
        ));

        let mut persistence = OfflinePersistence::new(false);
        core.update_active(erona_position, 0).unwrap();
        assert_eq!(quest_npc_in_range(&core, erona_guid), Some(15_278));
        core.update_active(
            WorldPosition {
                x: erona_position.x + OFFLINE_INTERACTION_RANGE_YARDS + 1.0,
                ..erona_position
            },
            0,
        )
        .unwrap();
        assert_eq!(quest_npc_in_range(&core, erona_guid), None);
        accept_offline_quest(&mut core, &mut persistence, &channels, erona_guid, 8_325);
        assert!(core.active_quest_journal().unwrap().entries.is_empty());
        assert!(received.try_iter().next().is_none());
        core.update_active(erona_position, 0).unwrap();

        accept_offline_quest(&mut core, &mut persistence, &channels, erona_guid, 8_325);
        assert!(core
            .active_quest_journal()
            .unwrap()
            .progress(8_325)
            .is_some());
        let acceptance_events: Vec<_> = received.try_iter().collect();
        assert!(acceptance_events
            .iter()
            .any(|event| matches!(event, SessionEvent::GossipComplete)));

        let mana_wyrm = core
            .hostiles()
            .iter()
            .find(|hostile| hostile.creature_entry == 15_274)
            .copied()
            .expect("nearby Mana Wyrm combat state");
        core.update_active(mana_wyrm.position, 0).unwrap();
        for expected_count in 1..=8 {
            if expected_count > 1 {
                advance_offline_combat(&mut core, &channels, benilla_game_core::HOSTILE_RESPAWN_MS);
            }
            core.start_auto_attack(mana_wyrm.spawn_id).unwrap();
            // The starter weapon's converted swing numbers decide how long the kill takes, so
            // step one swing interval at a time until the killing blow lands.
            let killed = (0..64).any(|_| {
                advance_offline_combat(&mut core, &channels, benilla_game_core::MELEE_SWING_MS);
                core.hostile(mana_wyrm.spawn_id)
                    .is_none_or(|hostile| hostile.health == 0)
            });
            assert!(killed, "the Mana Wyrm dies under auto-attack");
            let combat_events: Vec<_> = received.try_iter().collect();
            assert!(combat_events.iter().any(|event| matches!(
                event,
                SessionEvent::QuestObjectiveKill {
                    quest_id: 8_325,
                    count,
                    required: 8,
                    ..
                } if *count == expected_count
            )));
            if expected_count == 8 {
                assert!(combat_events.iter().any(|event| matches!(
                    event,
                    SessionEvent::QuestObjectivesComplete { quest_id: 8_325 }
                )));
                assert!(combat_events.iter().any(|event| matches!(
                    event,
                    SessionEvent::LevelUp(info) if info.level == 2
                )));
                assert!(combat_events.iter().any(|event| matches!(
                    event,
                    SessionEvent::ObjectValues { guid, fields }
                        if *guid == id.presentation_guid()
                            && fields.unit_health() == Some(benilla_game_core::PLAYER_MAX_HEALTH)
                            && fields.unit_power(0) == Some(benilla_game_core::PLAYER_MAX_MANA)
                )));
            }
        }
        core.update_active(erona_position, 0).unwrap();
        send_offline_quest_progress(&core, &channels, erona_guid, 8_325);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::QuestOffer(ref offer) if offer.quest_id == 8_325
        ));
        reward_offline_quest(
            &mut core,
            &mut persistence,
            &channels,
            erona_guid,
            8_325,
            0,
            None,
        );
        let reward_events: Vec<_> = received.try_iter().collect();
        assert!(reward_events.iter().any(|event| matches!(
            event,
            SessionEvent::QuestComplete(complete)
                if complete.quest_id == 8_325 && complete.xp == 100 && complete.money == 30
        )));
        assert!(reward_events
            .iter()
            .any(|event| matches!(event, SessionEvent::ItemCreate { .. })));
        assert!(reward_events.iter().any(|event| matches!(
            event,
            SessionEvent::ItemPushResult(result)
                if result.item_entry == 20_997 && result.count == 1
        )));
        // The turn-in frees log slot 0; the update must zero its fields, or the client's log keeps
        // showing the completed quest (and its objectives) as if it were still taken.
        assert!(reward_events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == id.presentation_guid()
                    && fields.player_quest_log(0).is_some_and(|slot| slot.quest_id == 0)
        )));
        let snapshot = core.snapshot(&id).unwrap();
        assert_eq!(snapshot.experience, 100);
        assert_eq!(snapshot.money_copper, 30);
        assert_eq!(snapshot.inventory.len(), 1);
    }

    /// A blood elf standing on the converted vendor `entry`'s spawn, and its presentation guid.
    fn vendor_at(entry: u32) -> (GameCore, CharacterId, u64) {
        let spawn = benilla_game_core::content::creature_content()
            .spawn
            .iter()
            .find(|spawn| spawn.map_id == 530 && spawn.entry == entry)
            .expect("converted vendor spawn");
        let id = CharacterId::parse("00000000000000000000000000000007").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Trades",
            Appearance {
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        core.enter_world().unwrap();
        core.update_active(
            WorldPosition {
                map_id: spawn.map_id,
                x: spawn.x,
                y: spawn.y,
                z: spawn.z,
                orientation: spawn.orientation,
            },
            0,
        )
        .unwrap();
        let guid = imported_guid(spawn.entry, spawn.source_guid);
        (core, id, guid)
    }

    /// The offline gameplay system with the vendor ledger seated.
    fn vendor_harness(
        core: GameCore,
    ) -> (
        App,
        crossbeam_channel::Receiver<SessionEvent>,
        crossbeam_channel::Sender<crate::net::ClientCommand>,
    ) {
        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (command_tx, commands) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(OfflineVendorBuyback::default())
        .insert_resource(Time::<()>::default());
        (app, received, command_tx)
    }

    #[test]
    fn offline_list_inventory_serves_the_converted_vendor_stock() {
        let (core, _, guid) = vendor_at(15_289);
        let (mut app, received, command_tx) = vendor_harness(core);
        command_tx
            .send(crate::net::ClientCommand::ListInventory { guid })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("list inventory handled");
        let events: Vec<_> = received.try_iter().collect();
        let listed = events
            .iter()
            .find_map(|event| match event {
                SessionEvent::VendorInventory { vendor, items } if *vendor == guid => {
                    Some(items.clone())
                }
                _ => None,
            })
            .expect("the vendor's stock list");
        let expected = benilla_game_core::vendor::vendor_items(15_289).count();
        assert_eq!(listed.len(), expected);
        assert!(expected > 1);
        for (index, row) in listed.iter().enumerate() {
            assert_eq!(row.slot, index as u32 + 1, "1-based wire slot");
            assert_eq!(row.current_count, VENDOR_STOCK_UNLIMITED);
            let template =
                benilla_game_core::quest::item_template(row.entry).expect("stock template");
            assert_eq!(row.price, template.buy_price, "the row shows the buy price");
            assert_eq!(row.display_id, template.display_id);
            assert!(events.iter().any(|event| matches!(
                event,
                SessionEvent::ItemTemplate { entry, info: Some(_) } if *entry == row.entry
            )));
        }
    }

    #[test]
    fn offline_buying_deducts_copper_grants_the_item_and_answers_the_stock() {
        let (mut core, id, guid) = vendor_at(15_289);
        core.credit_money(1_000_000).unwrap();
        let row = *benilla_game_core::vendor::vendor_items(15_289)
            .next()
            .expect("stock row");
        let template = benilla_game_core::quest::item_template(row.item).expect("template");
        let (mut app, received, command_tx) = vendor_harness(core);
        command_tx
            .send(crate::net::ClientCommand::BuyItem {
                vendor: guid,
                entry: row.item,
                count: 1,
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("buy handled");

        let expected_money = 1_000_000 - template.buy_price;
        let core = &app.world().resource::<OfflineGameCore>().0;
        let snapshot = core.snapshot(&id).unwrap();
        assert_eq!(snapshot.money_copper, expected_money);
        let bought = snapshot
            .inventory
            .iter()
            .find(|item| item.entry == row.item)
            .expect("the bought item is carried");
        assert_eq!(
            bought.count, row.buy_count,
            "BuyCount delivered per purchase"
        );

        let events: Vec<_> = received.try_iter().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::VendorBuyResult { vendor, slot: 1, new_count, purchase_count }
                if *vendor == guid
                    && *new_count == VENDOR_STOCK_UNLIMITED
                    && *purchase_count == row.buy_count
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { fields, .. }
                if fields.player_money() == Some(expected_money)
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, SessionEvent::ItemCreate { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::ItemPushResult(push)
                if push.item_entry == row.item && push.count == row.buy_count
        )));
    }

    #[test]
    fn offline_selling_credits_the_price_and_buyback_reclaims_it() {
        let (mut core, id, guid) = vendor_at(15_289);
        // Refreshing Spring Water: a cheap stack with a known sell price.
        let entry = 159;
        let item = core.grant_item(entry, 5, None).unwrap();
        let item_guid = inventory_item_guid(core.snapshot(&id).unwrap(), &item);
        let sell_price = benilla_game_core::quest::item_template(entry)
            .expect("template")
            .sell_price;
        let (mut app, received, command_tx) = vendor_harness(core);
        command_tx
            .send(crate::net::ClientCommand::SellItem {
                vendor: guid,
                item_guid,
                count: 0,
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("sell handled");

        let core = &app.world().resource::<OfflineGameCore>().0;
        let snapshot = core.snapshot(&id).unwrap();
        assert!(
            snapshot.inventory.iter().all(|item| item.entry != entry),
            "the sold stack left the bag"
        );
        assert_eq!(snapshot.money_copper, sell_price * 5);
        let events: Vec<_> = received.try_iter().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { fields, .. }
                if fields.player_buyback_slot(0).is_some_and(|guid| guid != 0)
                    && fields.player_buyback_price(0) == Some(sell_price * 5)
        )));

        command_tx
            .send(crate::net::ClientCommand::BuybackItem {
                vendor: guid,
                slot: BUYBACK_SLOT_FIRST,
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("buyback handled");
        let core = &app.world().resource::<OfflineGameCore>().0;
        let snapshot = core.snapshot(&id).unwrap();
        assert_eq!(snapshot.money_copper, 0, "buyback costs what it sold for");
        let reclaimed = snapshot
            .inventory
            .iter()
            .find(|item| item.entry == entry)
            .expect("the stack came back");
        assert_eq!(reclaimed.count, 5);
        let events: Vec<_> = received.try_iter().collect();
        assert!(events.iter().any(
            |event| matches!(event, SessionEvent::ObjectDestroyed(guid) if *guid == item_guid)
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { fields, .. }
                if fields.player_buyback_slot(0).is_none_or(|guid| guid == 0)
        )));
    }

    /// Vinemaster Suntouched is GOSSIP|INNKEEPER with no quest in the pack: the ladder's gossip
    /// arm opens no empty frame and the stock falls through instead.
    #[test]
    fn offline_empty_gossip_falls_through_to_the_vendor_stock() {
        let (core, _, guid) = vendor_at(16_442);
        let (mut app, received, command_tx) = vendor_harness(core);
        command_tx
            .send(crate::net::ClientCommand::GossipHello { guid })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("gossip handled");
        let events: Vec<_> = received.try_iter().collect();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SessionEvent::GossipMenu { .. })),
            "no quest, no empty gossip menu: {events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::VendorInventory { vendor, items } if *vendor == guid && !items.is_empty()
        )));
    }

    #[test]
    fn one_point_waypoint_path_tracks_its_leg_without_repeating() {
        let content = benilla_game_core::content::creature_content();
        let spawn = content
            .spawn
            .iter()
            .find(|spawn| {
                spawn.map_id == 530
                    && spawn.movement_type == 2
                    && content.waypoints_for(spawn.source_guid).count() == 1
            })
            .expect("the imported pack includes one-point waypoint paths");
        let waypoint = content
            .waypoints_for(spawn.source_guid)
            .next()
            .copied()
            .unwrap();
        let id = CharacterId::parse("00000000000000000000000000000003").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "OnePoint",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        core.enter_world().unwrap();
        core.update_active(
            WorldPosition {
                map_id: spawn.map_id,
                x: spawn.x,
                y: spawn.y,
                z: spawn.z,
                orientation: spawn.orientation,
            },
            0,
        )
        .unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let mut names = crate::names::NameCache::default();
        let mut patrols = OfflinePatrols::default();
        emit_world_entry(
            &OfflineChannels {
                picks,
                events,
                commands,
            },
            core.snapshot(&id).unwrap(),
            &[],
            None,
            None,
            None,
            &mut names,
            &mut patrols,
        );

        let guid = imported_guid(spawn.entry, spawn.source_guid);
        let start = [spawn.x, spawn.y, spawn.z];
        let target = [waypoint.x, waypoint.y, waypoint.z];
        let events: Vec<_> = received.try_iter().collect();
        let movement_events: Vec<_> = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SessionEvent::MonsterMove {
                        guid: moved_guid,
                        ..
                    } if *moved_guid == guid
                )
            })
            .collect();
        assert_eq!(
            movement_events.len(),
            1,
            "the one waypoint makes one initial leg"
        );
        assert!(matches!(
            movement_events[0],
            SessionEvent::MonsterMove {
                start: actual_start,
                path,
                duration_ms,
                ..
            } if *actual_start == start
                && path == &vec![start, target]
                && *duration_ms == patrol_leg_duration_ms(start, target)
        ));
        let patrol = patrols
            .0
            .get_mut(&guid)
            .expect("the current pose must be tracked until the one leg finishes");
        assert_eq!(patrol.waypoints.len(), 1);
        let duration = patrol.remaining_ms;
        assert!(advance_patrol(guid, patrol, duration).is_none());
        assert_eq!(patrol.current_position, target);
        assert_eq!(patrol.remaining_ms, 0);
        assert!(advance_patrol(guid, patrol, 1_000).is_none());
        assert_eq!(patrol.spline_id, 1, "the path never loops a second time");
    }

    #[test]
    fn starter_spellbook_and_action_bar_come_from_converted_race_class_data() {
        let mage_spells = benilla_game_core::progression::starter_spell_ids(10, 8);
        assert!(mage_spells.contains(&benilla_game_core::ATTACK_SPELL_ID));
        assert!(mage_spells.contains(&benilla_game_core::FIREBALL_SPELL_ID));
        let mage_buttons = starter_action_buttons(10, 8);
        assert_eq!(
            mage_buttons
                .iter()
                .map(|button| (button.slot, button.action, button.kind))
                .collect::<Vec<_>>(),
            [
                (0, 6603, 0),
                (1, 133, 0),
                (2, 168, 0),
                (3, 28734, 0),
                (4, 28730, 0),
                (10, 159, 128),
                (11, 20857, 128)
            ]
        );

        let warrior_spells = benilla_game_core::progression::starter_spell_ids(1, 1);
        assert!(warrior_spells.contains(&benilla_game_core::ATTACK_SPELL_ID));
        assert!(!warrior_spells.contains(&benilla_game_core::FIREBALL_SPELL_ID));
        assert!(!starter_action_buttons(1, 1).is_empty());
    }

    #[test]
    fn converted_starter_trainer_and_action_spells_exist_in_the_tbc_catalog() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let progression = benilla_game_core::progression::progression_data();
        let mut ids = std::collections::BTreeSet::new();
        ids.extend(progression.starter_spell.iter().map(|spell| spell.spell_id));
        ids.extend(progression.trainer_spell.iter().map(|spell| spell.spell_id));
        ids.extend(
            progression
                .action_bar
                .iter()
                .filter(|action| action.action_type == 0)
                .map(|action| action.action),
        );
        let missing: Vec<_> = ids
            .into_iter()
            .filter(|spell_id| spells.catalog.get(*spell_id).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "converted player spell sources must resolve in build-8606 Spell.dbc: {missing:?}"
        );
        let missing_taught: Vec<_> = progression
            .trainer_spell
            .iter()
            .filter_map(|offer| {
                let taught = spells
                    .catalog
                    .learned_spell(offer.spell_id)
                    .unwrap_or(offer.spell_id);
                spells
                    .catalog
                    .get(taught)
                    .is_none()
                    .then_some((offer.spell_id, taught))
            })
            .collect();
        assert!(
            missing_taught.is_empty(),
            "every trainer learn effect must resolve to its player spell: {missing_taught:?}"
        );

        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open build-8606 data");
        let talent_catalog =
            benilla_formats::load_talent_catalog(&mut chain).expect("load Talent/TalentTab");
        let mut missing_talents = Vec::new();
        for race in 1..=11 {
            for class in 1..=11 {
                for tab in talent_catalog.tabs_for_class(race, class) {
                    for talent in talent_catalog.talents_in_tab(tab.id) {
                        for spell_id in talent.ranks.iter().copied().filter(|id| *id != 0) {
                            if spells.catalog.get(spell_id).is_none() {
                                missing_talents.push((talent.id, spell_id));
                            }
                        }
                    }
                }
            }
        }
        assert!(
            missing_talents.is_empty(),
            "every build-8606 talent rank must resolve in Spell.dbc: {missing_talents:?}"
        );
    }

    #[test]
    fn imported_sunstrider_combat_keeps_source_identity_and_rejects_friendly_units() {
        let id = CharacterId::parse("00000000000000000000000000000002").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        disarm(&mut core);

        let content = benilla_game_core::content::creature_content();
        let nearby: Vec<_> = content
            .spawns_near(
                530,
                starting_position(10).unwrap().x,
                starting_position(10).unwrap().y,
                OFFLINE_CONTENT_RADIUS_YARDS,
            )
            .collect();
        let hostile = core.hostiles()[0];
        let hostile_spawn = nearby
            .iter()
            .find(|spawn| spawn.source_guid == hostile.spawn_id)
            .expect("combat state refers to a converted spawn");
        let hostile_template = content.template(hostile_spawn.entry).unwrap();
        assert!(
            test_reactions(10).reaction(hostile_template.faction)
                != benilla_game_core::UnitReaction::Friendly,
            "combat state holds an attackable faction, got {}",
            hostile_template.faction
        );
        let hostile_guid = imported_guid(hostile_spawn.entry, hostile_spawn.source_guid);

        let friendly_spawn = nearby
            .iter()
            .find(|spawn| {
                content
                    .template(spawn.entry)
                    .is_some_and(|template| template.faction == 1604)
            })
            .expect("nearby friendly Blood Elf NPC");
        let friendly_template = content.template(friendly_spawn.entry).unwrap();
        let friendly_guid = imported_guid(friendly_spawn.entry, friendly_spawn.source_guid);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        let mut names = crate::names::NameCache::default();
        let mut patrols = OfflinePatrols::default();
        emit_world_entry(
            &channels,
            core.snapshot(&id).unwrap(),
            core.hostiles(),
            None,
            None,
            None,
            &mut names,
            &mut patrols,
        );
        let entry_events: Vec<_> = received.try_iter().collect();
        let initial_unit_health = |guid| {
            entry_events.iter().find_map(|event| match event {
                SessionEvent::ObjectCreate {
                    guid: created_guid,
                    kind: EntityKind::Unit,
                    fields,
                    ..
                } if *created_guid == guid => Some(fields.unit_health()),
                _ => None,
            })
        };
        assert_eq!(initial_unit_health(hostile_guid), Some(Some(60)));
        assert_eq!(initial_unit_health(friendly_guid), Some(Some(100)));
        assert_ne!(hostile_template.faction, friendly_template.faction);

        core.update_active(hostile.position, 0).unwrap();
        start_offline_auto_attack(&mut core, &channels, friendly_guid);
        start_offline_cast(
            &mut core,
            &channels,
            crate::net::ClientCommand::CastSpell {
                spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                target: Some(friendly_guid),
            },
            benilla_game_core::PowerCost::mana(benilla_game_core::FIREBALL_MANA_COST),
            benilla_game_core::FIREBALL_CAST_MS,
            0,
            benilla_game_core::FIREBALL_CAST_MS,
            benilla_game_core::FIREBALL_RANGE_YARDS,
            benilla_game_core::DirectSpellEffect::Damage {
                min: 30,
                max: 30,
                school: 4,
                periodic: None,
                control: None,
            },
            benilla_game_core::ComboEffect::None,
            false,
        );
        assert!(
            received.try_iter().next().is_none(),
            "friendly units sharing the unit presentation are not combat targets"
        );

        let neutral_spawn = nearby
            .iter()
            .find(|spawn| {
                content
                    .template(spawn.entry)
                    .is_some_and(|template| template.faction == 7)
            })
            .expect("nearby neutral Mana Wyrm");
        let neutral = core
            .hostile(neutral_spawn.source_guid)
            .expect("neutral Mana Wyrm is a core combat target");
        let neutral_guid = imported_guid(neutral_spawn.entry, neutral_spawn.source_guid);
        assert_eq!(initial_unit_health(neutral_guid), Some(Some(60)));
        core.update_active(neutral.position, 0).unwrap();
        start_offline_auto_attack(&mut core, &channels, neutral_guid);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::AttackStart { victim, .. } if victim == neutral_guid
        ));
        advance_offline_combat(&mut core, &channels, benilla_game_core::MELEE_SWING_MS);
        let neutral_hit: Vec<_> = received.try_iter().collect();
        assert!(neutral_hit.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == neutral_guid
                    && fields.unit_health()
                        == Some(benilla_game_core::HOSTILE_MAX_HEALTH
                            - benilla_game_core::MELEE_DAMAGE)
        )));
        stop_offline_auto_attack(&mut core, &channels);
        let _ = received.try_iter().count();

        core.update_active(hostile.position, 0).unwrap();
        start_offline_auto_attack(&mut core, &channels, hostile_guid);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::AttackStart { victim, .. } if victim == hostile_guid
        ));
        advance_offline_combat(&mut core, &channels, benilla_game_core::MELEE_SWING_MS);
        let melee_events: Vec<_> = received.try_iter().collect();
        assert!(melee_events.iter().any(|event| matches!(
            event,
            SessionEvent::AttackerState(state)
                if state.victim == hostile_guid
                    && state.damage == benilla_game_core::MELEE_DAMAGE
        )));
        assert!(melee_events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == hostile_guid
                    && fields.unit_health()
                        == Some(benilla_game_core::HOSTILE_MAX_HEALTH
                            - benilla_game_core::MELEE_DAMAGE)
        )));
        stop_offline_auto_attack(&mut core, &channels);
        let _ = received.try_iter().count();

        for expected_health in [25, 0] {
            start_offline_cast(
                &mut core,
                &channels,
                crate::net::ClientCommand::CastSpell {
                    spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                    target: Some(hostile_guid),
                },
                benilla_game_core::PowerCost::mana(benilla_game_core::FIREBALL_MANA_COST),
                benilla_game_core::FIREBALL_CAST_MS,
                0,
                benilla_game_core::FIREBALL_CAST_MS,
                benilla_game_core::FIREBALL_RANGE_YARDS,
                benilla_game_core::DirectSpellEffect::Damage {
                    min: 30,
                    max: 30,
                    school: 4,
                    periodic: None,
                    control: None,
                },
                benilla_game_core::ComboEffect::None,
                false,
            );
            assert!(matches!(
                received.try_recv().unwrap(),
                SessionEvent::SpellStart { target: Some(guid), .. } if guid == hostile_guid
            ));
            received.try_recv().unwrap(); // Mana update.
            advance_offline_combat(&mut core, &channels, benilla_game_core::FIREBALL_CAST_MS);
            let impact_events: Vec<_> = received.try_iter().collect();
            assert!(impact_events.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectValues { guid, fields }
                    if *guid == hostile_guid && fields.unit_health() == Some(expected_health)
            )));
            if expected_health == 0 {
                assert!(impact_events.iter().any(|event| matches!(
                    event,
                    SessionEvent::XpGain(gain)
                        if gain.victim == hostile_guid && gain.total > 0 && gain.kill
                )));
                assert_eq!(
                    core.snapshot(&id).unwrap().experience,
                    benilla_game_core::progression::creature_kill_xp(
                        1,
                        content.template(hostile_spawn.entry).unwrap().level,
                    )
                );
            }
        }

        assert_eq!(core.hostile(hostile.spawn_id).unwrap().health, 0);
        advance_offline_combat(&mut core, &channels, benilla_game_core::HOSTILE_RESPAWN_MS);
        assert_eq!(core.hostile(hostile.spawn_id).unwrap().health, 60);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::ObjectValues { guid, fields }
                if guid == hostile_guid && fields.unit_health() == Some(60)
        ));
    }

    #[test]
    fn offline_corpse_loot_opens_and_item_pickup_updates_inventory() {
        let id = CharacterId::parse("00000000000000000000000000000005").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            Appearance {
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        disarm(&mut core);
        let wyrm = core
            .hostiles()
            .iter()
            .find(|hostile| hostile.creature_entry == 15_274)
            .copied()
            .expect("nearby Mana Wyrm");
        let guid = imported_guid(wyrm.creature_entry, wyrm.spawn_id);
        core.update_active(wyrm.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        core.start_auto_attack(wyrm.spawn_id).unwrap();
        advance_offline_combat(
            &mut core,
            &channels,
            benilla_game_core::MELEE_SWING_MS
                * (benilla_game_core::HOSTILE_MAX_HEALTH / benilla_game_core::MELEE_DAMAGE),
        );
        let death_events: Vec<_> = received.try_iter().collect();
        assert!(death_events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid: target, fields }
                if *target == guid
                    && fields.unit_lootable()
                    && !fields.unit_dynflag_dead()
        )));

        core.update_active(
            WorldPosition {
                x: wyrm.position.x + OFFLINE_INTERACTION_RANGE_YARDS + 1.0,
                ..wyrm.position
            },
            0,
        )
        .unwrap();
        let mut live_player = crate::player::Player::default();
        live_player.active = true;
        live_player.pos = benilla_assets::coords::wow_to_bevy([
            wyrm.position.x,
            wyrm.position.y,
            wyrm.position.z,
        ]);
        let mut app = App::new();
        app.insert_resource(channels)
            .insert_resource(OfflineGameCore(core))
            .insert_resource(OfflinePersistence::new(false))
            .insert_resource(Time::<()>::default())
            .insert_resource(live_player);
        command_tx
            .send(crate::net::ClientCommand::Loot { guid })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline corpse-loot command is handled");
        let response = received.try_recv().expect("stock loot response");
        let SessionEvent::LootResponse {
            guid: opened,
            gold,
            items,
            ..
        } = response
        else {
            panic!("expected loot response, got {response:?}");
        };
        assert_eq!(opened, guid);
        assert!(items.len() <= benilla_game_core::loot::MAX_CORPSE_ITEMS);
        assert!(
            !items.is_empty(),
            "the converted Mana Wyrm table rolls an item"
        );
        if let Some(row) = items.first() {
            command_tx
                .send(crate::net::ClientCommand::AutostoreLootItem { slot: row.slot })
                .unwrap();
            app.world_mut()
                .run_system_once(drive_offline_gameplay)
                .expect("offline loot-item command is handled");
            let take_events: Vec<_> = received.try_iter().collect();
            assert!(take_events.iter().any(
                |event| matches!(event, SessionEvent::LootRemoved { slot } if *slot == row.slot)
            ));
            assert!(take_events.iter().any(|event| matches!(
                event,
                SessionEvent::ItemPushResult(push) if push.item_entry == row.item_id
            )));
            assert!(app
                .world()
                .resource::<OfflineGameCore>()
                .0
                .active_inventory()
                .unwrap()
                .iter()
                .any(|item| item.entry == row.item_id));
        }
        command_tx
            .send(crate::net::ClientCommand::LootMoney)
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline loot-money command is handled");
        assert_eq!(
            app.world()
                .resource::<OfflineGameCore>()
                .0
                .active_money_copper(),
            Some(gold)
        );
        let money_events: Vec<_> = received.try_iter().collect();
        assert!(money_events
            .iter()
            .any(|event| matches!(event, SessionEvent::LootClearMoney)));
        if gold > 0 {
            assert!(money_events.iter().any(|event| matches!(
                event,
                SessionEvent::LootMoneyNotify { amount } if *amount == gold
            )));
        }
        command_tx
            .send(crate::net::ClientCommand::LootRelease { guid })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline loot-release command is handled");
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::LootReleaseResponse { guid: released } if released == guid
        ));
    }

    #[test]
    fn imported_spawn_preserves_nonzero_addon_sheath_state() {
        let content = benilla_game_core::content::creature_content();
        let (spawn, addon) = content
            .spawn
            .iter()
            .find_map(|spawn| {
                let addon = content.addon_for(spawn.source_guid)?;
                let template = content.template(spawn.entry)?;
                (addon.sheath_state != 0 && !is_dnd_test_creature(template))
                    .then_some((spawn, addon))
            })
            .expect("converted world content has a nonzero addon sheath state");
        let guid = imported_guid(spawn.entry, spawn.source_guid);
        let mut events = Vec::new();
        let mut names = crate::names::NameCache::default();
        let mut patrols = OfflinePatrols::default();

        assert_eq!(
            emit_imported_spawn(
                &mut events,
                content,
                spawn,
                None,
                &[],
                &[],
                &mut names,
                &mut patrols,
            ),
            Some(guid)
        );
        let fields = events
            .iter()
            .find_map(|event| match event {
                SessionEvent::ObjectCreate {
                    guid: emitted,
                    fields,
                    ..
                } if *emitted == guid => Some(fields),
                _ => None,
            })
            .expect("converted spawn object");
        assert_eq!(
            fields.unit_sheath_state(),
            Some(addon.sheath_state),
            "the TBC addon sheath byte reaches UNIT_FIELD_BYTES_2"
        );
    }

    #[test]
    fn offline_action_bar_cast_damages_and_respawns_an_imported_hostile() {
        let appearance = Appearance {
            race: 1,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            appearance,
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let hostile = northshire_hostile(&core);
        let hostile_guid = imported_guid(hostile.creature_entry, hostile.spawn_id);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        let mut names = crate::names::NameCache::default();
        let mut patrols = OfflinePatrols::default();
        emit_world_entry(
            &channels,
            core.snapshot(&id).unwrap(),
            core.hostiles(),
            None,
            None,
            None,
            &mut names,
            &mut patrols,
        );
        let entry_events: Vec<_> = received.try_iter().collect();
        assert!(entry_events.iter().any(|event| matches!(
            event,
            SessionEvent::SpellBook { spell_ids, .. }
                if spell_ids == &benilla_game_core::progression::starter_spell_ids(1, 8)
        )));
        assert!(entry_events.iter().any(|event| matches!(
            event,
            SessionEvent::ActionButtons { buttons }
                if buttons == &starter_action_buttons(1, 8)
        )));
        let player_guid = id.presentation_guid();
        let player = entry_events.iter().find_map(|event| match event {
            SessionEvent::ObjectCreate {
                guid,
                kind: EntityKind::Player,
                fields,
                ..
            } if *guid == player_guid => Some(fields),
            _ => None,
        });
        assert_eq!(
            player.unwrap().object_type(),
            Some(benilla_protocol::messages::ObjectType::Player)
        );
        assert_eq!(
            player.unwrap().unit_flags(),
            0x8,
            "the offline player carries UNIT_FLAG_PLAYER_CONTROLLED for TBC attack rules"
        );
        assert_eq!(
            player.unwrap().player_rest_state(),
            Some(2),
            "PLAYER_BYTES_2 byte 3 must be NORMAL, not the nil Exhaustion row"
        );
        assert!(
            entry_events
                .iter()
                .filter(|event| matches!(
                    event,
                    SessionEvent::ObjectCreate {
                        kind: EntityKind::Unit,
                        ..
                    }
                ))
                .count()
                >= 3,
            "nearby converted creatures are emitted"
        );
        let mob = entry_events.iter().find_map(|event| match event {
            SessionEvent::ObjectCreate {
                guid,
                kind: EntityKind::Unit,
                fields,
                ..
            } if *guid == hostile_guid => Some(fields),
            _ => None,
        });
        assert_eq!(mob.unwrap().unit_health(), Some(60));
        assert_eq!(
            mob.unwrap().object_type(),
            Some(benilla_protocol::messages::ObjectType::Unit)
        );

        core.update_active(hostile.position, 0).unwrap();
        for expected_health in [30, 0] {
            start_offline_cast(
                &mut core,
                &channels,
                crate::net::ClientCommand::CastSpell {
                    spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                    target: Some(hostile_guid),
                },
                benilla_game_core::PowerCost::mana(30),
                benilla_game_core::FIREBALL_CAST_MS,
                0,
                benilla_game_core::FIREBALL_CAST_MS,
                benilla_game_core::FIREBALL_RANGE_YARDS,
                benilla_game_core::DirectSpellEffect::Damage {
                    min: 30,
                    max: 30,
                    school: 4,
                    periodic: None,
                    control: None,
                },
                benilla_game_core::ComboEffect::None,
                false,
            );
            assert!(matches!(
                received.try_recv().unwrap(),
                SessionEvent::SpellStart { spell_id: 133, .. }
            ));
            let mana = received.try_recv().unwrap();
            // Second cast starts from 75: one `MANA_REGEN_PER_TICK` lands while advancing the first
            // cast's 1500 ms (`MANA_REGEN_TICK_MS` is 1000 ms).
            assert!(matches!(
                mana,
                SessionEvent::ObjectValues { guid, fields }
                    if guid == id.presentation_guid()
                        && fields.unit_power(0)
                            == Some(if expected_health == 30 {
                                70
                            } else {
                                70 + benilla_game_core::MANA_REGEN_PER_TICK - 30
                            })
            ));
            advance_offline_combat(&mut core, &channels, benilla_game_core::FIREBALL_CAST_MS);
            let impact_events: Vec<_> = received.try_iter().collect();
            assert!(impact_events
                .iter()
                .any(|event| matches!(event, SessionEvent::SpellGo { spell_id: 133, .. })));
            assert!(impact_events.iter().any(
                |event| matches!(event, SessionEvent::SpellDamageLog(log) if log.damage == 30)
            ));
            assert!(impact_events.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectValues { guid, fields }
                    if *guid == hostile_guid && fields.unit_health() == Some(expected_health)
            )));
            if expected_health == 0 {
                // An ordinary death carries health zero; the dead dynflag stays reserved for
                // feign death (`UNIT_DYNFLAG_DEAD`), as the descriptor writers record it.
                assert!(impact_events.iter().any(|event| matches!(
                    event,
                    SessionEvent::ObjectValues { guid, fields }
                        if *guid == hostile_guid
                            && fields.unit_health() == Some(0)
                            && !fields.unit_dynflag_dead()
                )));
            }
        }
        assert_eq!(core.hostile(hostile.spawn_id).unwrap().health, 0);
        advance_offline_combat(&mut core, &channels, benilla_game_core::HOSTILE_RESPAWN_MS);
        assert_eq!(core.hostile(hostile.spawn_id).unwrap().health, 60);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::ObjectValues { guid, ref fields }
                if guid == hostile_guid && fields.unit_health() == Some(60)
        ));
    }

    #[test]
    fn offline_attack_action_starts_swings_stops_and_updates_hostile_health() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let appearance = Appearance {
            race: 1,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            appearance,
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        disarm(&mut core);
        let hostile = northshire_hostile(&core);
        let target = imported_guid(hostile.creature_entry, hostile.spawn_id);
        core.update_active(
            WorldPosition {
                x: hostile.position.x + 2.0,
                ..hostile.position
            },
            0,
        )
        .unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());

        commands
            .send(crate::net::ClientCommand::AttackSwing { guid: target })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline attack command is handled");
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::AttackStart { attacker, victim }
                if attacker == id.presentation_guid() && victim == target
        ));

        let channels = app.world().resource::<OfflineChannels>().clone();
        advance_offline_combat(
            &mut app.world_mut().resource_mut::<OfflineGameCore>().0,
            &channels,
            benilla_game_core::MELEE_SWING_MS,
        );
        let swing: Vec<_> = received.try_iter().collect();
        assert!(swing.iter().any(|event| matches!(
            event,
            SessionEvent::AttackerState(state)
                if state.attacker == id.presentation_guid()
                    && state.victim == target
                    && state.damage == benilla_game_core::MELEE_DAMAGE
        )));
        assert!(swing.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == target
                    && fields.unit_health()
                        == Some(benilla_game_core::HOSTILE_MAX_HEALTH - benilla_game_core::MELEE_DAMAGE)
        )));

        commands
            .send(crate::net::ClientCommand::AttackStop)
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline stop command is handled");
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::AttackStop { attacker, victim }
                if attacker == id.presentation_guid() && victim == target
        ));
        let channels = app.world().resource::<OfflineChannels>().clone();
        advance_offline_combat(
            &mut app.world_mut().resource_mut::<OfflineGameCore>().0,
            &channels,
            benilla_game_core::MELEE_SWING_MS,
        );
        assert!(
            received.try_iter().next().is_none(),
            "stopped attacks do no more damage"
        );
    }

    /// Acceptance: a hunter's bow is a working weapon. Auto Shot (75) is the white ranged swing:
    /// the shot lands with the converted Warder's Shortbow's numbers (5-10 every 2,300 ms) and
    /// keeps shooting until the client cancels the auto-repeat.
    #[test]
    fn offline_ranged_auto_shot_shoots_the_target_with_the_converted_bow() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("00000000000000000000000000000006").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 3,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Vellas",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        let hostile = core.hostiles().first().copied().expect("imported target");
        core.update_active(
            WorldPosition {
                x: hostile.position.x + 10.0,
                ..hostile.position
            },
            0,
        )
        .unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        let target = imported_guid(hostile.creature_entry, hostile.spawn_id);
        commands
            .send(crate::net::ClientCommand::CastSpell {
                spell_id: 75,
                target: Some(target),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the white shot command is handled");
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::SpellStart { spell_id: 75, target: Some(guid), .. } if guid == target
        ));

        // The bow draws its converted 2,300 ms and then the shot lands.
        let channels = app.world().resource::<OfflineChannels>().clone();
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 2_299);
            assert!(received.try_iter().next().is_none(), "the bow draws first");
            advance_offline_combat(core, &channels, 1);
        }
        let shot: Vec<_> = received.try_iter().collect();
        assert!(shot.iter().any(|event| matches!(
            event,
            SessionEvent::SpellGo { spell_id: 75, hits, .. } if hits.as_slice() == [target]
        )));
        assert!(shot.iter().any(|event| matches!(
            event,
            SessionEvent::SpellDamageLog(log)
                if log.spell_id == 75
                    && log.target == target
                    && (5..=10).contains(&log.damage)
        )));

        // The client cancels the auto-repeat and the shooting stops.
        commands
            .send(crate::net::ClientCommand::CancelAutoRepeat)
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the auto-repeat cancel is handled");
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 2_300);
        }
        assert!(
            received.try_iter().next().is_none(),
            "a cancelled shot stays quiet"
        );
    }

    /// Acceptance: a warlock's demon is a working pet. Summon Imp (688) brings the converted imp
    /// (entry 416) up as a unit with its pet bar, and it swings with its owner's attack.
    #[test]
    fn offline_summon_imp_brings_up_the_pet_its_bar_and_its_swings() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("00000000000000000000000000000007").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 9,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Velindra",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        assert!(
            core.snapshot(&id).unwrap().known_spell_ids.contains(&688),
            "the authored stock-TBC grant puts Summon Imp in the spellbook"
        );
        let hostile = core.hostiles().first().copied().expect("imported target");
        core.update_active(hostile.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        commands
            .send(crate::net::ClientCommand::CastSpell {
                spell_id: 688,
                target: Some(id.presentation_guid()),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the summon command is handled");
        let channels = app.world().resource::<OfflineChannels>().clone();
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 20_000);
        }
        let pet_guid = summon_guid(416);
        let summoned: Vec<_> = received.try_iter().collect();
        assert!(
            summoned.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectCreate { guid, .. } if *guid == pet_guid
            )),
            "the imp's unit comes up: {summoned:?}"
        );
        assert!(summoned.iter().any(|event| matches!(
            event,
            SessionEvent::PetSpells(bar) if bar.pet_guid == pet_guid
        )));

        // The pet joins its owner's attack and swings the creature baseline at the target.
        commands
            .send(crate::net::ClientCommand::AttackSwing {
                guid: imported_guid(hostile.creature_entry, hostile.spawn_id),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the attack command is handled");
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, benilla_game_core::CREATURE_SWING_MS);
        }
        let swings: Vec<_> = received.try_iter().collect();
        assert!(
            swings.iter().any(|event| matches!(
                event,
                SessionEvent::AttackerState(state)
                    if state.attacker == pet_guid
                        && state.damage == benilla_game_core::CREATURE_DAMAGE
            )),
            "the pet's swing lands: {swings:?}"
        );
    }

    /// The totem behaviors fold from the converted rows: Earthbind's snare pulse, Searing's own
    /// attack, Healing Stream's heal tick, and Stoneskin's standing buff.
    #[test]
    fn totem_behaviors_fold_from_the_converted_creature_rows() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("spell catalog");
        let durations = benilla_formats::load_spell_durations(&mut chain).expect("durations");
        let fold = |spell_id: u32, entry: u32| {
            let spell = catalog.get(spell_id).expect("summon row");
            let template = benilla_game_core::summon::summon_template(entry).expect("converted");
            totem_behavior(&catalog, &durations, spell, template)
        };
        use benilla_game_core::{ControlEffect, ControlKind, TotemPulse};

        // Earthbind Totem: the converted passive (6474) triggers Earthbind (3600) every 3000ms —
        // the snare, for the trigger's own 5s — and the totem lives the row's 45s.
        let earthbind = fold(2484, 2630);
        assert_eq!(earthbind.buff_spell, Some(6474));
        assert_eq!(earthbind.pulse_ms, 3_000);
        assert_eq!(earthbind.duration_ms, 45_000);
        assert_eq!(
            earthbind.pulse,
            Some(TotemPulse::Control(ControlEffect {
                kind: ControlKind::Snare,
                slow_percent: 50,
                duration_ms: 5_000,
            }))
        );

        // Searing Totem: its own "Attack" row (6350) is the pulse, on the creature cadence.
        let searing = fold(6363, 3902);
        assert_eq!(searing.pulse_ms, benilla_game_core::CREATURE_SWING_MS);
        let Some(TotemPulse::Damage { min, max, school }) = searing.pulse else {
            panic!("Searing bolts: {searing:?}");
        };
        assert!(min <= max && min > 0, "{searing:?}");
        assert_eq!(
            school,
            1_u32
                .checked_shl(catalog.get(6350).expect("Attack row").school)
                .unwrap_or(1)
        );

        // Healing Stream: the converted heal tick (6371) every 2000ms.
        let healing = fold(6375, 3906);
        assert_eq!(healing.pulse_ms, 2_000);
        assert!(matches!(
            healing.pulse,
            Some(TotemPulse::Heal { min, max }) if min <= max && min > 0
        ));

        // Stoneskin is a standing buff (8156) with no pulse of its own.
        let stoneskin = fold(8154, 5919);
        assert_eq!(stoneskin.buff_spell, Some(8156));
        assert_eq!(stoneskin.pulse, None);
    }

    /// Acceptance: a totem is a standing summon that works. Earthbind Totem (2484) puts the
    /// converted totem (entry 2630) on the ground for its row's 45s, its snare sweep rides the
    /// totem's own attacker state at the engaged target, and the unit leaves when the lifetime
    /// runs out.
    #[test]
    fn offline_earthbind_totem_stands_sweeps_and_expires() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("00000000000000000000000000000008").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 7,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Sheylen",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        let hostile = core.hostiles().first().copied().expect("imported target");
        core.update_active(hostile.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        commands
            .send(crate::net::ClientCommand::CastSpell {
                spell_id: 2484,
                target: Some(id.presentation_guid()),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the totem command is handled");
        let channels = app.world().resource::<OfflineChannels>().clone();
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 1);
        }
        let totem_guid = imported_guid(2630, 2484);
        let summoned: Vec<_> = received.try_iter().collect();
        assert!(
            summoned.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectCreate { guid, .. } if *guid == totem_guid
            )),
            "the totem's unit stands: {summoned:?}"
        );

        // The sweep rides the totem's own attacker state at the engaged target.
        commands
            .send(crate::net::ClientCommand::AttackSwing {
                guid: imported_guid(hostile.creature_entry, hostile.spawn_id),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the attack command is handled");
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 3_000);
        }
        let pulses: Vec<_> = received.try_iter().collect();
        assert!(
            pulses.iter().any(|event| matches!(
                event,
                SessionEvent::AttackerState(state)
                    if state.attacker == totem_guid
                        && state.damage == 0
                        && state.melee_spell_id == 2484
            )),
            "the snare sweep lands: {pulses:?}"
        );

        // The totem's unit leaves when the row's lifetime runs out.
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 42_000);
        }
        let expired: Vec<_> = received.try_iter().collect();
        assert!(
            expired.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectDestroyed(guid) if *guid == totem_guid
            )),
            "the totem expires: {expired:?}"
        );
    }

    /// Acceptance: a buff shows on the character. Demon Skin (687, a warlock create-time spell)
    /// lands in the character's aura block with its row's lifetime, published with the duration
    /// the buff frame counts down.
    #[test]
    fn offline_buffs_show_in_the_character_aura_block() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("00000000000000000000000000000009").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 9,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Velindra",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        let hostile = core.hostiles().first().copied().expect("imported target");
        core.update_active(hostile.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        commands
            .send(crate::net::ClientCommand::CastSpell {
                spell_id: 687,
                target: Some(id.presentation_guid()),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the buff command is handled");
        let channels = app.world().resource::<OfflineChannels>().clone();
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, 1);
        }
        let buffed: Vec<_> = received.try_iter().collect();
        assert!(
            buffed.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectValues { guid, fields }
                    if *guid == id.presentation_guid()
                        && (0..48).any(|slot| fields
                            .unit_aura(slot)
                            .is_some_and(|aura| aura.spell_id == 687))
            )),
            "the buff's aura block is written: {buffed:?}"
        );
        assert!(
            buffed.iter().any(|event| matches!(
                event,
                SessionEvent::AuraDuration { remaining_ms, .. } if *remaining_ms > 0
            )),
            "the buff's duration is published: {buffed:?}"
        );
    }

    /// Acceptance: the warrior's rage bar follows the fight — the swing's gain and the creature
    /// hit's gain both republish the pools.
    #[test]
    fn offline_warrior_rage_moves_the_power_bar() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("0000000000000000000000000000000a").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Braddok",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        let hostile = core.hostiles().first().copied().expect("imported target");
        core.update_active(hostile.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        // Dealt: the swing's rage moves the bar.
        commands
            .send(crate::net::ClientCommand::AttackSwing {
                guid: imported_guid(hostile.creature_entry, hostile.spawn_id),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the attack command is handled");
        let channels = app.world().resource::<OfflineChannels>().clone();
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            advance_offline_combat(core, &channels, benilla_game_core::MELEE_SWING_MS);
        }
        let swung: Vec<_> = received.try_iter().collect();
        assert!(
            swung.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectValues { guid, fields }
                    if *guid == id.presentation_guid()
                        && fields.unit_power(1).is_some_and(|rage| rage > 0)
            )),
            "the swing's rage reaches the bar: {swung:?}"
        );

        // Taken: the creature's hit pays rage too, and the bar follows.
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            let mut patrols = OfflinePatrols::default();
            advance_offline_creature_ai(
                core,
                &channels,
                &mut patrols,
                benilla_game_core::CREATURE_SWING_MS,
            );
        }
        let hit: Vec<_> = received.try_iter().collect();
        assert!(
            hit.iter().any(|event| matches!(
                event,
                SessionEvent::ObjectValues { guid, fields }
                    if *guid == id.presentation_guid()
                        && fields.unit_power(1).is_some_and(|rage| rage > 0)
                        && fields
                            .unit_health()
                            .is_some_and(|health| health < benilla_game_core::PLAYER_MAX_HEALTH)
            )),
            "the taken hit's rage reaches the bar: {hit:?}"
        );
    }

    /// Acceptance: a kill releases the client. The killing swing's stop echoes as `AttackStop` —
    /// the reference client stays engaged until it hears one and sends no further swings while
    /// engaged — and the next mob accepts the attack right away.
    #[test]
    fn offline_killing_a_mob_echoes_the_stop_and_the_next_mob_accepts_the_attack() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("0000000000000000000000000000000b").unwrap();
        let appearance = Appearance {
            race: 10,
            class: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Braddok",
            appearance,
            starting_position(10).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 10);
        let first = core.hostiles()[0];
        let second = core.hostiles()[1];
        core.update_active(first.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        commands
            .send(crate::net::ClientCommand::AttackSwing {
                guid: imported_guid(first.creature_entry, first.spawn_id),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the attack command is handled");
        let channels = app.world().resource::<OfflineChannels>().clone();
        let mut stopped = false;
        for _ in 0..200 {
            {
                let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
                advance_offline_combat(core, &channels, benilla_game_core::MELEE_SWING_MS);
            }
            let events: Vec<_> = received.try_iter().collect();
            if events.iter().any(|event| {
                matches!(
                    event,
                    SessionEvent::AttackStop { victim, .. }
                        if *victim == imported_guid(first.creature_entry, first.spawn_id)
                )
            }) {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "the killing swing echoes the attack stop");

        // The next mob accepts the attack right away: the client is no longer engaged.
        {
            let core = &mut app.world_mut().resource_mut::<OfflineGameCore>().0;
            core.update_active(second.position, 0).unwrap();
        }
        commands
            .send(crate::net::ClientCommand::AttackSwing {
                guid: imported_guid(second.creature_entry, second.spawn_id),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("the second attack command is handled");
        let events: Vec<_> = received.try_iter().collect();
        assert!(
            events.iter().any(|event| matches!(
                event,
                SessionEvent::AttackStart { victim, .. }
                    if *victim == imported_guid(second.creature_entry, second.spawn_id)
            )),
            "the second mob takes the attack: {events:?}"
        );
    }

    #[test]
    fn offline_entry_restores_aura_descriptor_slots_and_remaining_durations() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            Appearance {
                race: 1,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let hostile = northshire_hostile(&core);
        core.update_active(hostile.position, 0).unwrap();
        core.advance_creature_ai(benilla_game_core::CREATURE_SWING_MS);
        core.begin_fireball(
            benilla_game_core::FIREBALL_SPELL_ID,
            hostile.spawn_id,
            benilla_game_core::FIREBALL_MANA_COST,
            benilla_game_core::FIREBALL_CAST_MS,
            benilla_game_core::FIREBALL_CAST_MS,
            benilla_game_core::FIREBALL_RANGE_YARDS,
            30,
            30,
        )
        .unwrap();
        core.advance_combat(benilla_game_core::FIREBALL_CAST_MS);
        let buff = CharacterAura {
            slot: 5,
            spell_id: 1_001,
            flags: 0x03,
            level: 1,
            stacks: 2,
            remaining_ms: Some(3_500),
        };
        let debuff = CharacterAura {
            slot: 36,
            spell_id: 1_002,
            flags: 0x02,
            level: 1,
            stacks: 1,
            remaining_ms: None,
        };
        core.set_active_aura(buff).unwrap();
        core.set_active_aura(debuff).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        let mut names = crate::names::NameCache::default();
        let mut patrols = OfflinePatrols::default();
        emit_world_entry(
            &channels,
            core.snapshot(&id).unwrap(),
            core.hostiles(),
            None,
            None,
            None,
            &mut names,
            &mut patrols,
        );
        let entry_events: Vec<_> = received.try_iter().collect();
        let player = entry_events
            .iter()
            .find_map(|event| match event {
                SessionEvent::ObjectCreate {
                    guid,
                    kind: EntityKind::Player,
                    fields,
                    ..
                } if *guid == id.presentation_guid() => Some(fields),
                _ => None,
            })
            .expect("world entry includes the saved player aura fields");
        assert_eq!(
            player.unit_health(),
            Some(benilla_game_core::PLAYER_MAX_HEALTH - benilla_game_core::CREATURE_DAMAGE)
        );
        assert_eq!(
            player.unit_power(0),
            Some(benilla_game_core::STARTER_MANA - benilla_game_core::FIREBALL_MANA_COST)
        );
        assert_eq!(
            player.unit_aura(buff.slot),
            Some(benilla_protocol::messages::UnitAuraSlot {
                slot: buff.slot,
                spell_id: buff.spell_id,
                flags: buff.flags,
                level: buff.level,
                stacks: buff.stacks,
            })
        );
        assert_eq!(
            player.unit_aura(debuff.slot).unwrap().spell_id,
            debuff.spell_id
        );
        assert!(entry_events.iter().any(|event| matches!(
            event,
            SessionEvent::AuraDuration { slot, remaining_ms }
                if *slot == buff.slot && *remaining_ms == 3_500
        )));
        assert!(!entry_events.iter().any(|event| matches!(
            event,
            SessionEvent::AuraDuration { slot, .. } if *slot == debuff.slot
        )));

        let expired = core.advance_active_auras(3_500);
        publish_offline_auras(&core, &channels, &expired);
        let expiration_events: Vec<_> = received.try_iter().collect();
        assert!(expiration_events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == id.presentation_guid()
                    && fields.unit_aura(buff.slot).is_none()
                    && fields.unit_aura(debuff.slot).is_some()
        )));
        assert!(expiration_events.iter().any(|event| matches!(
            event,
            SessionEvent::AuraDuration { slot, remaining_ms: 0 } if *slot == buff.slot
        )));

        let mut persistence = OfflinePersistence::new(false);
        cancel_offline_aura(&mut core, &mut persistence, &channels, debuff.spell_id);
        let cancelled_events: Vec<_> = received.try_iter().collect();
        assert!(cancelled_events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == id.presentation_guid() && fields.unit_aura(debuff.slot).is_none()
        )));
        assert!(cancelled_events.iter().any(|event| matches!(
            event,
            SessionEvent::AuraDuration { slot, remaining_ms: 0 } if *slot == debuff.slot
        )));
        assert!(core.active_auras().unwrap().is_empty());
    }

    #[test]
    fn hostile_ai_events_drive_chase_attack_and_player_health() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let appearance = Appearance {
            race: 1,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            appearance,
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let hostile = northshire_hostile(&core);
        let target = imported_guid(hostile.creature_entry, hostile.spawn_id);
        core.update_active(
            WorldPosition {
                x: hostile.position.x + 10.0,
                ..hostile.position
            },
            0,
        )
        .unwrap();
        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let channels = OfflineChannels {
            picks,
            events,
            commands,
        };
        let mut patrols = OfflinePatrols::default();

        advance_offline_creature_ai(&mut core, &channels, &mut patrols, 500);
        let chase_events: Vec<_> = received.try_iter().collect();
        assert!(chase_events.iter().any(|event| matches!(
            event,
            SessionEvent::AiReaction { unit, reaction: 2 } if *unit == target
        )));
        assert!(chase_events.iter().any(|event| matches!(
            event,
            SessionEvent::MonsterMove { guid, run_mode: true, path, .. }
                if *guid == target && path.len() == 2
        )));

        let contact = core.hostile(hostile.spawn_id).unwrap().position;
        core.update_active(contact, 0).unwrap();
        advance_offline_creature_ai(&mut core, &channels, &mut patrols, 0);
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::AttackStart { attacker, victim }
                if attacker == target && victim == id.presentation_guid()
        ));

        advance_offline_creature_ai(
            &mut core,
            &channels,
            &mut patrols,
            benilla_game_core::CREATURE_SWING_MS,
        );
        let impact_events: Vec<_> = received.try_iter().collect();
        assert!(impact_events.iter().any(|event| matches!(
            event,
            SessionEvent::AttackerState(state)
                if state.attacker == target
                    && state.victim == id.presentation_guid()
                    && state.damage == benilla_game_core::CREATURE_DAMAGE
        )));
        assert!(impact_events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == id.presentation_guid()
                    && fields.unit_health()
                        == Some(benilla_game_core::PLAYER_MAX_HEALTH - benilla_game_core::CREATURE_DAMAGE)
        )));
    }

    #[test]
    fn repop_request_recovers_dead_character_at_race_start() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let appearance = Appearance {
            race: 1,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let start = starting_position(1).unwrap();
        let mut core = GameCore::default();
        core.create_character(id.clone(), "Alaera", appearance, start)
            .unwrap();
        enter_test_world(&mut core, 1);
        let target = northshire_hostile(&core);
        core.update_active(target.position, 0).unwrap();
        core.advance_creature_ai(benilla_game_core::CREATURE_SWING_MS * 40);
        assert_eq!(core.active_health(), 0);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());
        commands
            .send(crate::net::ClientCommand::RepopRequest)
            .unwrap();

        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline release-spirit command is handled");
        let events: Vec<_> = received.try_iter().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == id.presentation_guid()
                    && fields.unit_health() == Some(benilla_game_core::PLAYER_MAX_HEALTH)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::Teleport { guid, position, .. }
                if *guid == id.presentation_guid()
                    && *position == [start.x, start.y, start.z]
        )));
        let core = &app.world().resource::<OfflineGameCore>().0;
        assert_eq!(core.active_health(), benilla_game_core::PLAYER_MAX_HEALTH);
        assert_eq!(core.snapshot(&id).unwrap().position, start);
    }

    #[test]
    fn offline_fireball_uses_project_rules_when_spell_catalog_is_unavailable() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let appearance = Appearance {
            race: 1,
            class: 8,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            appearance,
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let hostile = northshire_hostile(&core);
        let target = imported_guid(hostile.creature_entry, hostile.spawn_id);
        core.update_active(hostile.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());

        commands
            .send(crate::net::ClientCommand::CastSpell {
                spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                target: Some(target),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline Fireball cast is handled without a Spell.dbc resource");
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::SpellStart {
                spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                cast_time_ms: benilla_game_core::FIREBALL_CAST_MS,
                ..
            }
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::ObjectValues { fields, .. }
                if fields.unit_power(0)
                    == Some(benilla_game_core::STARTER_MANA - benilla_game_core::FIREBALL_MANA_COST)
        ));

        let channels = app.world().resource::<OfflineChannels>().clone();
        advance_offline_combat(
            &mut app.world_mut().resource_mut::<OfflineGameCore>().0,
            &channels,
            benilla_game_core::FIREBALL_CAST_MS,
        );
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::SpellGo { spell_id, .. }
                if spell_id == benilla_game_core::FIREBALL_SPELL_ID
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            SessionEvent::SpellDamageLog(log)
                if log.spell_id == benilla_game_core::FIREBALL_SPELL_ID
                    && (benilla_game_core::FIREBALL_RANK1_DAMAGE_MIN
                        ..=benilla_game_core::FIREBALL_RANK1_DAMAGE_MAX)
                        .contains(&log.damage)
        ));
    }

    #[test]
    fn offline_fireball_uses_build_8606_damage_and_publishes_its_periodic_debuff() {
        let Some(spells) = crate::ui_action::real_spells() else {
            return;
        };
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            Appearance {
                race: 1,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let hostile = northshire_hostile(&core);
        let target = imported_guid(hostile.creature_entry, hostile.spawn_id);
        core.update_active(hostile.position, 0).unwrap();

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_receiver) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_receiver,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default())
        .insert_resource(spells);

        commands
            .send(crate::net::ClientCommand::CastSpell {
                spell_id: benilla_game_core::FIREBALL_SPELL_ID,
                target: Some(target),
            })
            .unwrap();
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline cast reads the real Spell.dbc catalog");
        let cast_start: Vec<_> = received.try_iter().collect();
        assert!(cast_start.iter().any(|event| matches!(
            event,
            SessionEvent::SpellStart {
                spell_id: 133,
                cast_time_ms: 1_500,
                ..
            }
        )));
        assert!(cast_start.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { fields, .. }
                if fields.unit_power(0)
                    == Some(benilla_game_core::STARTER_MANA - 30)
        )));

        let channels = app.world().resource::<OfflineChannels>().clone();
        advance_offline_combat(
            &mut app.world_mut().resource_mut::<OfflineGameCore>().0,
            &channels,
            benilla_game_core::FIREBALL_CAST_MS,
        );
        let impact: Vec<_> = received.try_iter().collect();
        assert!(impact.iter().any(|event| matches!(
            event,
            SessionEvent::SpellDamageLog(log)
                if log.spell_id == 133 && (14..=22).contains(&log.damage)
        )));
        assert!(impact.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == target && fields.unit_aura(32).is_some_and(|aura| aura.spell_id == 133)
        )));
        assert!(impact.iter().any(|event| matches!(
            event,
            SessionEvent::AuraDuration {
                slot: 32,
                remaining_ms: 4_000
            }
        )));

        advance_offline_combat(
            &mut app.world_mut().resource_mut::<OfflineGameCore>().0,
            &channels,
            benilla_game_core::FIREBALL_BURN_TICK_MS * 2,
        );
        let expiry: Vec<_> = received.try_iter().collect();
        assert!(expiry.iter().any(|event| matches!(
            event,
            SessionEvent::SpellDamageLog(log)
                if log.spell_id == 133 && log.periodic && log.damage == 1
        )));
        assert!(
            expiry.iter().any(|event| matches!(
                event,
                SessionEvent::AuraDuration {
                    slot: 32,
                    remaining_ms: 0
                }
            )),
            "expired Fireball aura must clear its duration: {expiry:?}"
        );
        assert!(expiry.iter().any(|event| matches!(
            event,
            SessionEvent::ObjectValues { guid, fields }
                if *guid == target && fields.unit_aura(32).is_none()
        )));
    }

    #[test]
    fn offline_logout_leaves_the_world_and_emits_the_normal_logout_event() {
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            Appearance {
                race: 1,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(1).unwrap(),
        )
        .unwrap();
        enter_test_world(&mut core, 1);
        let saved_position = WorldPosition {
            map_id: 0,
            x: -8900.0,
            y: -100.0,
            z: 84.0,
            orientation: 1.0,
        };
        core.update_active(saved_position, 0).unwrap();

        let (pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(OfflineChannels {
            picks,
            events,
            commands: command_rx,
        })
        .insert_resource(OfflineGameCore(core))
        .insert_resource(OfflinePersistence::new(false))
        .insert_resource(Time::<()>::default());
        drop(pick_tx);

        commands
            .send(crate::net::ClientCommand::Logout)
            .expect("offline command receiver is alive");
        app.world_mut()
            .run_system_once(drive_offline_gameplay)
            .expect("offline gameplay command loop runs");

        let core = &app.world().resource::<OfflineGameCore>().0;
        assert!(
            core.active().is_none(),
            "logout clears the active character"
        );
        assert!(
            core.hostiles().is_empty(),
            "leaving clears the loaded creatures"
        );
        assert_eq!(core.snapshot(&id).unwrap().position, saved_position);
        assert!(matches!(received.try_recv(), Ok(SessionEvent::LoggedOut)));
    }

    #[test]
    fn a_corrupt_primary_document_recovers_and_repairs_from_its_valid_backup() {
        let dir = std::env::temp_dir().join(format!("benilla-offline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("offline/profile.toml");
        local_state::write_atomic_bytes_with_backup(&path, b"42").unwrap();
        local_state::write_atomic_bytes_with_backup(&path, b"not a number").unwrap();

        let restored = read_checked_document(&path, |text| {
            text.parse::<u32>()
                .map_err(|error| SaveError::InvalidToml(error.to_string()))
        })
        .unwrap();

        assert_eq!(restored, Some(42));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "42");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shutdown_saves_the_latest_pose_even_when_exit_is_announced_in_post_update() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};

        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!("benilla-offline-exit-{}", std::process::id()));
        let _capture = EnvGuard::unset("WOW_CAPTURE");
        let _home = EnvGuard::set("BENILLA_HOME", dir.to_str().unwrap());
        let id = CharacterId::parse("00000000000000000000000000000001").unwrap();
        let mut core = GameCore::default();
        core.create_character(
            id.clone(),
            "Alaera",
            Appearance {
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            starting_position(10).unwrap(),
        )
        .unwrap();
        core.enter_world().unwrap();
        let path = local_state::offline_character_path(id.as_str()).unwrap();
        let initial = core.encode_snapshot(&id).unwrap();
        local_state::write_atomic_bytes_with_backup(&path, initial.as_bytes()).unwrap();

        let mut app = App::new();
        app.insert_resource(OfflineGameCore(core))
            .insert_resource(OfflinePersistence::new(true))
            .insert_resource(crate::player::Player::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, update_local_player)
            .add_systems(PostUpdate, |mut exit: MessageWriter<AppExit>| {
                exit.write(AppExit::Success);
            });
        crate::shutdown::on_app_exit(&mut app, save_on_exit.into_configs());
        let latest = WorldPosition {
            x: 10320.0,
            y: -6390.0,
            z: 35.0,
            orientation: 1.0,
            ..starting_position(10).unwrap()
        };
        {
            let mut player = app.world_mut().resource_mut::<crate::player::Player>();
            player.active = true;
            player.pos = benilla_assets::coords::wow_to_bevy([latest.x, latest.y, latest.z]);
        }
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(100));
        app.update();

        let saved = decode_snapshot(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            (saved.position.x, saved.position.y, saved.position.z),
            (latest.x, latest.y, latest.z)
        );
        assert_eq!(saved.play_time_ms, 100);
        assert_eq!(
            std::fs::read_to_string(local_state::backup_path(&path)).unwrap(),
            initial
        );
        let profile = decode_profile(
            &std::fs::read_to_string(local_state::offline_profile_path().unwrap()).unwrap(),
        )
        .unwrap();
        assert!(GameCore::restore(profile, [saved]).is_ok());
        app.update();
        assert_eq!(
            std::fs::read_to_string(local_state::backup_path(&path)).unwrap(),
            initial
        );
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn build_8606_blood_elf_creation_enters_map_530_with_local_appearance() {
        let Some(data) = benilla_formats::wow_data() else {
            eprintln!("skipping: no client install");
            return;
        };
        if !benilla_formats::is_tbc_data_dir(&data) {
            eprintln!("skipping: resolved install is not TBC");
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open TBC archive chain");
        let catalog = crate::entities::CharCreate(
            benilla_formats::CharCreateCatalog::load(&mut chain)
                .expect("load TBC character creation catalog"),
        );
        let mut core = GameCore::default();
        let proportions = crate::body_shape::BodyProportions {
            enabled: true,
            height: 35,
            waist: -10,
            hips: 15,
            bust: 10,
            seat: 12,
            ..default()
        };
        create_local_character(
            &mut core,
            Some(&catalog),
            CharCreateReq {
                name: "Alaera".into(),
                race: 10,
                class: 8,
                gender: 0,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            },
            proportions,
        )
        .expect("create Blood Elf Mage");

        let id = core.selected().expect("created character selected").clone();
        let snapshot = core.snapshot(&id).expect("created snapshot");
        assert_eq!(snapshot.name, "Alaera");
        assert_eq!(snapshot.body_proportions, proportions.into());
        assert_eq!(
            (snapshot.appearance.race, snapshot.appearance.class),
            (10, 8)
        );
        assert_eq!(snapshot.position.map_id, 530);
        assert_eq!(core.enter_world().expect("enter local world").id, id);

        let (_pick_tx, picks) = crossbeam_channel::unbounded();
        let (events, received) = crossbeam_channel::unbounded();
        let (_command_tx, commands) = crossbeam_channel::unbounded();
        let mut names = crate::names::NameCache::default();
        let mut patrols = OfflinePatrols::default();
        emit_world_entry(
            &OfflineChannels {
                picks,
                events,
                commands,
            },
            core.snapshot(&id).expect("active snapshot"),
            core.hostiles(),
            Some(&catalog),
            None,
            None,
            &mut names,
            &mut patrols,
        );
        let events: Vec<_> = received.try_iter().collect();
        let patrol_guid = imported_guid(15_274, 54_942);
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::MonsterMove { guid, path, run_mode: false, .. }
                if *guid == patrol_guid && path.len() == 2
        )));
        assert!(patrols.0.contains_key(&patrol_guid));
        assert!(
            matches!(events.first(), Some(SessionEvent::Connected { name, .. }) if name == "Alaera")
        );
        assert!(matches!(
            events.get(1),
            Some(SessionEvent::Worldport { map_id: 530, .. })
        ));
        let Some(SessionEvent::ObjectCreate {
            fields,
            display_id,
            scale,
            ..
        }) = events.get(2)
        else {
            panic!("world entry did not emit the player create event: {events:?}");
        };
        assert_eq!(*display_id, catalog.0.body_display(10, 0));
        assert!((*scale - 1.35).abs() < 1e-5);
        assert_eq!(
            (
                fields.unit_race(),
                fields.unit_class(),
                fields.unit_gender()
            ),
            (Some(10), Some(8), Some(0))
        );
        assert_eq!(
            (fields.player_skin(), fields.player_face()),
            (Some(0), Some(0))
        );
        assert_eq!(fields.unit_health(), Some(100));
    }

    #[test]
    fn build_8606_unit_reactions_match_the_imported_world_factions() {
        let Some(data) = benilla_formats::wow_data() else {
            eprintln!("skipping: no client install");
            return;
        };
        if !benilla_formats::is_tbc_data_dir(&data) {
            eprintln!("skipping: resolved install is not TBC");
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open TBC archive chain");
        let factions = crate::target::Factions::from_catalog(
            benilla_formats::load_faction_catalog(&mut chain)
                .expect("load build-8606 faction templates"),
        );
        let content = benilla_game_core::content::creature_content();
        for race in [1, 10] {
            let player = factions
                .catalog()
                .race_template(race)
                .expect("race faction template");
            let reactions = unit_reactions(Some(&factions), race);
            for (id, template) in factions.catalog().templates() {
                let expected = match template.reaction_toward(player) {
                    benilla_formats::Reaction::Hostile => benilla_game_core::UnitReaction::Hostile,
                    benilla_formats::Reaction::Neutral => benilla_game_core::UnitReaction::Neutral,
                    benilla_formats::Reaction::Friendly => {
                        benilla_game_core::UnitReaction::Friendly
                    }
                };
                assert_eq!(
                    reactions.reaction(id),
                    expected,
                    "race {race}, faction template {id}"
                );
            }
            for creature in &content.template {
                assert_eq!(
                    reactions.reaction(creature.faction),
                    match factions
                        .catalog()
                        .template(creature.faction)
                        .expect("every imported faction resolves")
                        .reaction_toward(player)
                    {
                        benilla_formats::Reaction::Hostile => {
                            benilla_game_core::UnitReaction::Hostile
                        }
                        benilla_formats::Reaction::Neutral => {
                            benilla_game_core::UnitReaction::Neutral
                        }
                        benilla_formats::Reaction::Friendly => {
                            benilla_game_core::UnitReaction::Friendly
                        }
                    },
                    "race {race}, creature {}, faction {}",
                    creature.entry,
                    creature.faction
                );
            }
        }
    }

    #[test]
    fn build_8606_fireball_mana_cost_matches_the_offline_combat_rule() {
        let Some(data) = benilla_formats::wow_data() else {
            eprintln!("skipping: no client install");
            return;
        };
        if !benilla_formats::is_tbc_data_dir(&data) {
            eprintln!("skipping: resolved install is not TBC");
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open TBC archive chain");
        let spells =
            benilla_formats::load_spell_catalog(&mut chain).expect("load build-8606 spell catalog");
        let fireball = spells
            .get(benilla_game_core::FIREBALL_SPELL_ID)
            .expect("Fireball rank 1");
        assert_eq!(fireball.name, "Fireball");
        assert_eq!(fireball.rank.as_deref(), Some("Rank 1"));
        assert_eq!(fireball.visual, 67);
        assert_eq!(
            fireball.icon.as_deref(),
            Some("Interface\\Icons\\Spell_Fire_FlameBolt")
        );
        assert_eq!(fireball.range_index, 35);
        assert_eq!(fireball.implicit_target_a1, 6);
        assert_eq!(
            crate::spell::cast_target::cast_target_mask(fireball),
            0x0080
        );
        assert_eq!(fireball.school, 2);
        assert_eq!(fireball.prevention_type, 1);
        assert_eq!(fireball.mana_cost, 30);
        assert_eq!(spells.get(168).unwrap().implicit_target_a1, 1);
        assert_eq!(spells.get(1459).unwrap().implicit_target_a1, 21);
        let cast_times = benilla_formats::load_spell_cast_times(&mut chain)
            .expect("load build-8606 spell cast times");
        assert_eq!(
            cast_times
                .get(fireball.casting_time_index)
                .unwrap()
                .resolved_ms(1, fireball.base_level),
            1_500
        );
        let ranges =
            benilla_formats::load_spell_ranges(&mut chain).expect("load build-8606 spell ranges");
        assert_eq!(ranges.get(fireball.range_index).unwrap().max, 35.0);
    }
}
