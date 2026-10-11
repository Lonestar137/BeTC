//! Offline startup adapter: owns the game-core instance without introducing transport behavior.

use std::collections::BTreeMap;
use std::path::Path;

use benilla_game_core::{
    decode_profile, decode_snapshot, Appearance, CharacterAura, CharacterId, CombatUpdate,
    CreatureAiUpdate, GameCore, HostileState, SaveError, WorldPosition,
};
use benilla_protocol::{CharAction, CharCreateReq, EntityKind, ObjectFields, SessionEvent};
use bevy::prelude::*;

use crate::local_state;
use crate::net::{CharActionResultMessage, CharListMessage, CharRequest, OfflineChannels};

mod persistence;
use persistence::OfflinePersistence;

const OFFLINE_CONTENT_RADIUS_YARDS: f32 = benilla_game_core::SUNSTRIDER_CONTENT_RADIUS_YARDS;
const OFFLINE_CREATURE_WALK_SPEED_YARDS_PER_SEC: f32 = 2.5;
const OFFLINE_CREATURE_AI_STEP_MS: u32 = 200;
/// NPC interaction range in yards: matches the client's 5.5556 yd service range
/// (`crate::target::cursor_mode::SERVICE_RANGE_SQ` = 30.864).
const OFFLINE_INTERACTION_RANGE_YARDS: f32 = 5.5556;

#[derive(Resource, Default)]
struct OfflinePatrols(BTreeMap<u64, ActivePatrol>);

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
    emit_world_entry_with_talents(
        channels, snapshot, hostiles, catalog, creatures, factions, None, names, patrols,
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
) {
    patrols.0.clear();
    let guid = snapshot.id.presentation_guid();
    let position = [
        snapshot.position.x,
        snapshot.position.y,
        snapshot.position.z,
    ];
    let appearance = snapshot.appearance;
    let race_class_gender = u32::from(appearance.race)
        | (u32::from(appearance.class) << 8)
        | (u32::from(appearance.gender) << 16);
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
        (46, 0x8),                                  // UNIT_FLAG_PLAYER_CONTROLLED
        (36, race_class_gender),                    // UNIT_FIELD_BYTES_0
        (193, player_bytes),                        // PLAYER_BYTES
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
    if snapshot.position.map_id != 530 {
        for hostile in hostiles {
            let entry = hostile_entry();
            let guid = hostile_guid(hostile.spawn_id);
            // This installed model is covered by the build-8606 display-catalog test. The prior Mana
            // Wyrm path had no display row, making the adapter emit a deliberate fallback cube.
            let model_path = "Creature\\Wolf\\Wolf.m2";
            let display_id =
                creatures.and_then(|catalog| catalog.display_for_model_path(model_path));
            if display_id.is_none() {
                warn!(
                "offline Sunstrider hostile has no renderable build-8606 display for {model_path}"
            );
            }
            let scale = display_id
                .and_then(|display_id| {
                    creatures.and_then(|catalog| catalog.model_scale(display_id))
                })
                .unwrap_or(1.0);
            let mut mob_fields = vec![
                (2, 0x9), // OBJECT_FIELD_TYPE: object and unit
                (3, entry),
                (4, scale.to_bits()), // OBJECT_FIELD_SCALE_X: display/model scale folded by the adapter
                (22, hostile.health),
                (28, hostile.max_health),
                (34, 1),
                (35, 14), // Monster faction template
                (130, 1.5f32.to_bits()),
            ];
            if let Some(display_id) = display_id {
                mob_fields.extend([(131, display_id), (132, display_id)]);
            }
            names.insert_creature(
                entry,
                Some(crate::names::CreatureRecord {
                    name: hostile_name().to_string(),
                    subname: None,
                    creature_type: 1,
                    pet_family: 0,
                    rank: 0,
                    type_flags: 0,
                    civilian: false,
                    racial_leader: false,
                    display_id: display_id.unwrap_or(0),
                }),
            );
            events.push(SessionEvent::ObjectCreate {
                guid,
                kind: EntityKind::Unit,
                display_id,
                position: [hostile.position.x, hostile.position.y, hostile.position.z],
                orientation: hostile.position.orientation,
                scale,
                speeds: None,
                mover: None,
                transport_progress: None,
                transport: None,
                spline: None,
                fields: ObjectFields::from_pairs(&mob_fields)
                    .into_created(benilla_protocol::messages::ObjectType::Unit),
            });
        }
    }
    let content = benilla_game_core::content::starting_zone_content();
    for spawn in content.spawns_near(
        snapshot.position.map_id,
        snapshot.position.x,
        snapshot.position.y,
        OFFLINE_CONTENT_RADIUS_YARDS,
    ) {
        let template = content
            .template(spawn.entry)
            .expect("converted spawn template was validated by the content loader");
        if is_dnd_test_creature(template) {
            continue;
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
            let has_loot = snapshot
                .corpse_loot
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
        if let Some(gender) = benilla_game_core::quest::quest_pack()
            .quest
            .iter()
            .find(|quest| quest.giver_creature_entries.contains(&template.entry))
            .map(|quest| quest.gossip_npc_gender)
        {
            fields.push((36, u32::from(gender) << 16));
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

fn free_talent_points(
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

fn hostile_entry() -> u32 {
    91_002
}

fn hostile_name() -> &'static str {
    "Dawnpath Stalker"
}

fn hostile_guid(spawn_id: u32) -> u64 {
    (u64::from(benilla_protocol::guid::HIGH_UNIT) << 48)
        | (u64::from(hostile_entry()) << 24)
        | u64::from(spawn_id & 0x00ff_ffff)
}

fn imported_guid(entry: u32, source_guid: u32) -> u64 {
    (u64::from(benilla_protocol::guid::HIGH_UNIT) << 48)
        | (u64::from(entry) << 24)
        | u64::from(source_guid & 0x00ff_ffff)
}

fn combat_target_guid(hostile: &HostileState) -> Option<u64> {
    if hostile.position.map_id != 530 {
        return Some(hostile_guid(hostile.spawn_id));
    }
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
    send_gameplay_events(
        channels,
        [SessionEvent::ObjectValues {
            guid: snapshot.id.presentation_guid(),
            fields: ObjectFields::from_pairs(&player_item_field_pairs(snapshot)),
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
    complete: bool,
) -> [(u16, u32); 3] {
    let base = 198 + 3 * slot as u16;
    if progress.reward_choice.is_some() {
        return [(base, 0), (base + 1, 0), (base + 2, 0)];
    }
    let count_state = progress
        .objective_counts
        .iter()
        .take(4)
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
    snapshot
        .quest_journal
        .entries
        .iter()
        .enumerate()
        .flat_map(|(slot, progress)| {
            quest_log_slot_fields(
                slot,
                progress,
                snapshot.quest_journal.is_complete(progress.quest_id),
            )
        })
        .collect()
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
            | benilla_formats::SPELL_EFFECT_NORMALIZED_WEAPON_DMG => {
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
                // Auras resolve: periodic damage is the DoT, the rest is buff/debuff state.
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
                send_offline_gossip_menu(&core.0, &channels, *guid);
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
            crate::net::ClientCommand::AttackSwing { guid } => {
                start_offline_auto_attack(&mut core.0, &channels, *guid);
                continue;
            }
            crate::net::ClientCommand::AttackStop => {
                stop_offline_auto_attack(&mut core.0, &channels);
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
                    let effect = match plan.direct {
                        Some(slot) if spell.effects[slot] == benilla_formats::SPELL_EFFECT_HEAL => {
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
                                }
                            }
                        }
                        None => benilla_game_core::DirectSpellEffect::Aura {
                            school: 1_u32.checked_shl(spell.school).unwrap_or(1),
                            periodic: None,
                        },
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
    benilla_game_core::content::starting_zone_content()
        .spawn
        .iter()
        .find(|spawn| {
            spawn.map_id == 530
                && spawn.entry == entry
                && spawn.source_guid & 0x00ff_ffff == source_guid
        })
}

fn quest_npc_entry(core: &GameCore, guid: u64) -> Option<u32> {
    let spawn = imported_spawn_for_guid(guid)?;
    let template = benilla_game_core::content::starting_zone_content().template(spawn.entry)?;
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
fn quest_npc_in_range(core: &GameCore, guid: u64) -> Option<u32> {
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

fn quest_giver_guid(core: &GameCore, npc_entry: u32) -> Option<u64> {
    let player = core.snapshot(core.active()?)?.position;
    benilla_game_core::content::starting_zone_content()
        .spawns_near(
            player.map_id,
            player.x,
            player.y,
            OFFLINE_CONTENT_RADIUS_YARDS,
        )
        .find(|spawn| spawn.entry == npc_entry)
        .map(|spawn| imported_guid(spawn.entry, spawn.source_guid))
}

fn quest_status_icon(status: benilla_game_core::quest::QuestOfferState) -> u32 {
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

fn send_offline_gossip_menu(core: &GameCore, channels: &OfflineChannels, guid: u64) {
    let Some(npc_entry) = quest_npc_entry(core, guid) else {
        return;
    };
    let Some(definition) = benilla_game_core::quest::quest_pack()
        .quest
        .iter()
        .find(|quest| quest.giver_creature_entries.contains(&npc_entry))
    else {
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

fn send_offline_npc_text(core: &GameCore, channels: &OfflineChannels, guid: u64, text_id: u32) {
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

fn send_offline_questgiver_status(core: &GameCore, channels: &OfflineChannels, npc: u64) {
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

fn send_offline_quest_greeting(core: &GameCore, channels: &OfflineChannels, npc: u64) {
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

fn send_offline_quest_details(
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

fn send_offline_quest_progress(
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

fn send_offline_quest_reward(core: &GameCore, channels: &OfflineChannels, npc: u64, quest_id: u32) {
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

fn send_offline_quest_template(channels: &OfflineChannels, quest_id: u32) {
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

fn quest_log_slot_fields_for(
    snapshot: &benilla_game_core::CharacterSnapshot,
    quest_id: u32,
) -> Vec<(u16, u32)> {
    let Some(slot) = snapshot.quest_journal.slot(quest_id) else {
        return Vec::new();
    };
    let Some(progress) = snapshot.quest_journal.progress(quest_id) else {
        return Vec::new();
    };
    quest_log_slot_fields(slot, progress, snapshot.quest_journal.is_complete(quest_id)).into()
}

fn accept_offline_quest(
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
                fields: ObjectFields::from_pairs(&quest_log_field_pairs(snapshot)),
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

fn reward_offline_quest(
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

fn starter_action_buttons(race: u8, class: u8) -> Vec<benilla_protocol::messages::ActionButton> {
    benilla_game_core::progression::starting_actions(race, class)
        .map(|action| benilla_protocol::messages::ActionButton {
            slot: action.button,
            action: action.action,
            kind: action.action_type,
        })
        .collect()
}

fn send_offline_trainer_list(
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

fn buy_offline_trainer_spell(
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

fn learn_offline_talent(
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
        benilla_game_core::DirectSpellEffect::Damage { .. }
        | benilla_game_core::DirectSpellEffect::WeaponDamage { .. } => {
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
        }
    }
    if core.advance_power_regen(elapsed_ms) {
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

fn update_local_player(
    time: Res<Time>,
    player: Res<crate::player::Player>,
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
        *save_elapsed += time.delta_secs();
    }
    if *save_elapsed >= 5.0 {
        *save_elapsed = 0.0;
        save_after_command(&core.0, &mut persistence);
    }
}

fn exit_after_roster_boot(
    state: Res<State<crate::char_select::ClientState>>,
    mut frames: Local<u8>,
    mut exit: MessageWriter<AppExit>,
) {
    if *state.get() != crate::char_select::ClientState::CharSelect {
        return;
    }
    *frames = frames.saturating_add(1);
    if *frames == 3 {
        info!("offline: local roster reached without transport");
        exit.write(AppExit::Success);
    }
}

fn enter_offline_smoke_character(
    state: Res<State<crate::char_select::ClientState>>,
    core: Res<OfflineGameCore>,
    pick: Res<crate::net::CharPick>,
    mut requested: Local<bool>,
) {
    if *requested || *state.get() != crate::char_select::ClientState::CharSelect {
        return;
    }
    let Some(id) = core.0.selected() else {
        return;
    };
    if pick
        .0
        .send(CharRequest::Enter(id.presentation_guid()))
        .is_ok()
    {
        *requested = true;
        info!("offline smoke: entering selected local character");
    }
}

fn exit_after_world_smoke(
    mut commands: Commands,
    state: Res<State<crate::char_select::ClientState>>,
    loading: Res<crate::loading_screen::LoadingScreen>,
    core: Res<OfflineGameCore>,
    player: Res<crate::player::Player>,
    camera: Query<&Transform, With<benilla_world::view::WorldCamera>>,
    self_store: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    time: Res<Time>,
    mut entered_at: Local<Option<f32>>,
    mut shot_requested: Local<bool>,
    mut smoke_seconds: Local<Option<f32>>,
    mut exit: MessageWriter<AppExit>,
) {
    if *state.get() != crate::char_select::ClientState::InWorld || loading.covering() {
        *entered_at = None;
        return;
    }
    let started = *entered_at.get_or_insert(time.elapsed_secs());
    let elapsed = time.elapsed_secs() - started;
    let hold = *smoke_seconds.get_or_insert_with(|| {
        std::env::var("BENILLA_OFFLINE_SMOKE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .unwrap_or(6.0)
    });
    if elapsed >= hold && !*shot_requested {
        let camera_pose = camera
            .single()
            .ok()
            .map(|transform| (transform.translation, transform.forward().as_vec3()));
        let self_health = self_store
            .single()
            .ok()
            .map(|store| (store.0.unit_health(), store.0.unit_max_health()));
        info!(
            "offline smoke: player active={} settling={} pos={:?}, camera={camera_pose:?}, health={self_health:?}",
            player.active, player.settling, player.pos,
        );
        if std::env::var_os("BENILLA_OFFLINE_SMOKE_CAST").is_some() {
            let first_health = core.0.hostiles().first().map(|mob| mob.health);
            info!(
                "offline smoke: combat spawned={} first_hostile_health={first_health:?} mana={}",
                core.0.hostiles().len(),
                core.0.active_mana(),
            );
        }
        if let Ok(path) = std::env::var("BENILLA_OFFLINE_SMOKE_SHOT") {
            use bevy::render::view::screenshot::{save_to_disk, Screenshot};
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(path.clone()));
            info!("offline smoke: capturing world to {path}");
        }
        *shot_requested = true;
    }
    if elapsed >= hold + 1.0 {
        info!("offline smoke: world stayed active for {hold:.0} seconds");
        exit.write(AppExit::Success);
    }
}

/// `BENILLA_OFFLINE_SMOKE_CAST=1` sends two real offline Fireball commands at the first spawned
/// hostile, through the same command channel as the stock action bar.
fn drive_offline_smoke_cast(
    state: Res<State<crate::char_select::ClientState>>,
    loading: Res<crate::loading_screen::LoadingScreen>,
    time: Res<Time>,
    core: Res<OfflineGameCore>,
    spells: Option<Res<crate::ui_action::Spells>>,
    commands: Res<crate::net::NetCommands>,
    mut casts_sent: Local<u8>,
    mut entered_at: Local<Option<f32>>,
) {
    if std::env::var_os("BENILLA_OFFLINE_SMOKE_CAST").is_none() {
        return;
    }
    if *state.get() != crate::char_select::ClientState::InWorld || loading.covering() {
        *entered_at = None;
        *casts_sent = 0;
        return;
    }
    let Some(spells) = spells else {
        return;
    };
    if spells
        .catalog
        .get(benilla_game_core::FIREBALL_SPELL_ID)
        .is_none()
    {
        return;
    }
    let elapsed = time.elapsed_secs() - *entered_at.get_or_insert(time.elapsed_secs());
    if *casts_sent >= 2 || elapsed < f32::from(*casts_sent) * 2.0 {
        return;
    }
    if core.0.active().is_none() {
        return;
    }
    let Some(target) = core.0.hostiles().first().and_then(combat_target_guid) else {
        return;
    };
    if commands
        .0
        .send(crate::net::ClientCommand::CastSpell {
            spell_id: benilla_game_core::FIREBALL_SPELL_ID,
            target: Some(target),
        })
        .is_ok()
    {
        *casts_sent += 1;
        info!("offline smoke: sent Fireball command {}/2", *casts_sent);
    }
}

/// `BENILLA_OFFLINE_SMOKE_WALK_SECONDS`: optionally exercise the real forward binding after the
/// loading cover clears; the default smoke remains stationary.
fn drive_offline_smoke_walk(
    state: Res<State<crate::char_select::ClientState>>,
    loading: Res<crate::loading_screen::LoadingScreen>,
    time: Res<Time>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut keyboard: MessageWriter<bevy::input::keyboard::KeyboardInput>,
    mut walk_seconds: Local<Option<f32>>,
    mut started: Local<Option<f32>>,
    mut finished: Local<bool>,
) {
    let seconds = *walk_seconds.get_or_insert_with(|| {
        std::env::var("BENILLA_OFFLINE_SMOKE_WALK_SECONDS")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .unwrap_or(0.0)
    });
    if seconds == 0.0 {
        return;
    }
    if *state.get() != crate::char_select::ClientState::InWorld || loading.covering() {
        *started = None;
        keys.release(KeyCode::KeyW);
        return;
    }
    let start = *started.get_or_insert_with(|| time.elapsed_secs());
    if time.elapsed_secs() - start < seconds {
        keys.press(KeyCode::KeyW);
        keyboard.write(bevy::input::keyboard::KeyboardInput {
            key_code: KeyCode::KeyW,
            logical_key: bevy::input::keyboard::Key::Unidentified(
                bevy::input::keyboard::NativeKey::Unidentified,
            ),
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: time.elapsed_secs() > start,
            window: Entity::PLACEHOLDER,
        });
    } else if !*finished {
        keys.release(KeyCode::KeyW);
        keyboard.write(bevy::input::keyboard::KeyboardInput {
            key_code: KeyCode::KeyW,
            logical_key: bevy::input::keyboard::Key::Unidentified(
                bevy::input::keyboard::NativeKey::Unidentified,
            ),
            state: bevy::input::ButtonState::Released,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        info!("offline smoke: forward input held for {seconds:.1}s");
        *finished = true;
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
        core.enter_world().unwrap();
        let spawn_id = benilla_game_core::HOSTILE_SPAWN_ID;
        let hostile = core.hostile(spawn_id).unwrap();
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
        let guid = hostile_guid(spawn_id);
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
        let content = benilla_game_core::content::starting_zone_content();
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
            let snapshot = core.enter_world().unwrap();

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
                &snapshot,
                core.hostiles(),
                None,
                None,
                None,
                &mut names,
                &mut patrols,
            );
            let events: Vec<_> = received.try_iter().collect();

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
            let practice_guids = [
                hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID),
                hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID + 1),
                hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID + 2),
            ];
            let emitted_practice_guids: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    SessionEvent::ObjectCreate { guid, .. } if practice_guids.contains(guid) => {
                        Some(*guid)
                    }
                    _ => None,
                })
                .collect();
            if snapshot.position.map_id == 530 {
                assert!(
                    emitted_practice_guids.is_empty(),
                    "Sunstrider uses source spawns, not duplicate practice actors"
                );
            } else {
                assert_eq!(
                    emitted_practice_guids.len(),
                    practice_guids.len(),
                    "the non-Sunstrider practice harness remains available"
                );
            }
            assert!(patrols.0.values().any(|patrol| patrol.waypoints.len() > 1));
        }
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
        core.enter_world().unwrap();

        let erona = benilla_game_core::content::starting_zone_content()
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
        core.enter_world().unwrap();

        let erona = benilla_game_core::content::starting_zone_content()
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
        send_offline_gossip_menu(&core, &channels, erona_guid);
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
                if complete.quest_id == 8_325 && complete.xp == 50 && complete.money == 30
        )));
        assert!(reward_events
            .iter()
            .any(|event| matches!(event, SessionEvent::ItemCreate { .. })));
        assert!(reward_events.iter().any(|event| matches!(
            event,
            SessionEvent::ItemPushResult(result)
                if result.item_entry == 20_997 && result.count == 1
        )));
        let snapshot = core.snapshot(&id).unwrap();
        assert_eq!(snapshot.experience, 50);
        assert_eq!(snapshot.money_copper, 30);
        assert_eq!(snapshot.inventory.len(), 1);
    }

    #[test]
    fn one_point_waypoint_path_tracks_its_leg_without_repeating() {
        let content = benilla_game_core::content::starting_zone_content();
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
        core.enter_world().unwrap();
        disarm(&mut core);

        let content = benilla_game_core::content::starting_zone_content();
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
            benilla_game_core::SUNSTRIDER_ATTACKABLE_FACTION_TEMPLATES
                .contains(&hostile_template.faction),
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
        core.enter_world().unwrap();
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
    fn offline_action_bar_cast_damages_and_respawns_a_spawned_hostile() {
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
        core.enter_world().unwrap();

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
        let addon_unit = entry_events.iter().find_map(|event| match event {
            SessionEvent::ObjectCreate { guid, fields, .. }
                if *guid == imported_guid(26_007, 96_146) =>
            {
                Some(fields)
            }
            _ => None,
        });
        assert_eq!(
            addon_unit.and_then(ObjectFields::unit_sheath_state),
            Some(1)
        );
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
            "the legacy combat harness remains available outside Sunstrider"
        );
        let hostile_guid = hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID);
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
        assert_eq!(
            core.hostile(benilla_game_core::HOSTILE_SPAWN_ID)
                .unwrap()
                .health,
            0
        );
        advance_offline_combat(&mut core, &channels, benilla_game_core::HOSTILE_RESPAWN_MS);
        assert_eq!(
            core.hostile(benilla_game_core::HOSTILE_SPAWN_ID)
                .unwrap()
                .health,
            60
        );
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
        core.enter_world().unwrap();
        disarm(&mut core);
        let start = core.snapshot(&id).unwrap().position;
        core.update_active(
            WorldPosition {
                x: start.x + 8.0,
                ..start
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

        let target = hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID);
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
        core.enter_world().unwrap();
        let hostile = core.hostile(benilla_game_core::HOSTILE_SPAWN_ID).unwrap();
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
        core.enter_world().unwrap();
        let target = hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID);
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

        let contact = core
            .hostile(benilla_game_core::HOSTILE_SPAWN_ID)
            .unwrap()
            .position;
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
        core.enter_world().unwrap();
        let target = core.hostiles()[0];
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
        core.enter_world().unwrap();

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

        let target = hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID);
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
        core.enter_world().unwrap();

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

        let target = hostile_guid(benilla_game_core::HOSTILE_SPAWN_ID);
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
        core.enter_world().unwrap();
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
            "leaving clears the practice mobs"
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
    fn build_8606_practice_hostiles_are_attackable_by_human_and_blood_elf_players() {
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
        let reputations = crate::net::Reputations::default();
        for race in [1, 10] {
            let player_faction = factions.race_template(race).expect("race faction template");
            let player = crate::net::ObjectStore(
                ObjectFields::from_pairs(&[(35, player_faction)])
                    .into_created(benilla_protocol::messages::ObjectType::Player),
            );
            let hostile = crate::net::ObjectStore(
                ObjectFields::from_pairs(&[(35, 14)])
                    .into_created(benilla_protocol::messages::ObjectType::Unit),
            );
            assert!(
                crate::target::can_attack(
                    Some(&hostile),
                    Some(&factions),
                    &reputations,
                    Some(&player),
                ),
                "race {race} must see Monster template 14 as hostile"
            );
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
