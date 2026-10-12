use super::*;

pub(super) fn exit_after_roster_boot(
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

pub(super) fn enter_offline_smoke_character(
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

pub(super) fn exit_after_world_smoke(
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
pub(super) fn drive_offline_smoke_cast(
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
pub(super) fn drive_offline_smoke_walk(
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
