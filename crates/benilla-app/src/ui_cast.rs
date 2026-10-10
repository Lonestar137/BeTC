//! The cast-bar feed: our own cast's wire edges become the `SPELLCAST_*` events stock
//! `CastingBarFrame.lua:6-13` registers for, named from `Spell.dbc`, and the VM's casting flag
//! is pushed each frame. The cast lifecycle it reads is `crate::spell`'s.

use std::time::Instant;

use bevy::prelude::*;

use benilla_ui::script::{ScriptValue, UiScript};

use crate::creature_anim::Casting;
use crate::net::{GuidIndex, SelfGuid};
use crate::spell::{inflight, PendingCast, QueuedMeleeSpell};
use crate::ui_action::Spells;
use crate::ui_unit::UnitFeed;

/// One edge of our own cast, queued by the spell's packet handlers and its local cancel.
pub(crate) enum CastBarEdge {
    /// `SMSG_SPELL_START` with a nonzero cast time; an instant opens no bar, as in the reference.
    Start { spell_id: u32, cast_time_ms: u32 },
    /// `SMSG_SPELL_GO`: the cast completed.
    Stop,
    /// `SMSG_CAST_RESULT` failure: the bar, if shown, reads "Failed".
    Failed,
    /// `SMSG_SPELL_FAILED_OTHER`: our cast was interrupted.
    Interrupted,
    /// `SMSG_SPELL_DELAYED`: pushback extends the cast by `delay_ms`; the bar slides, never
    /// cancels.
    Delayed { delay_ms: u32 },
    /// `MSG_CHANNEL_START`, sent by vmangos to the caster only; the channel bar reads
    /// `UnitChannelInfo`.
    ChannelStart { spell_id: u32, duration_ms: u32 },
    /// `MSG_CHANNEL_UPDATE`, also caster-only: the time left, 0 when the channel is over.
    ChannelUpdate { remaining_ms: u32 },
}

/// The queue of [`CastBarEdge`]s, drained by [`feed_cast_bar`].
#[derive(Resource, Default)]
pub(crate) struct CastBarFeed(pub(crate) Vec<CastBarEdge>);

/// `SPELLCAST_START`'s name argument: empty for an unknown spell, or when `AttributesEx3 & 0x4`
/// is set, where the reference pushes `""` (`0x6e7a2d`) and still fires, so the bar is a
/// full-length untitled one. The channel bar reads `UnitChannelInfo` instead.
fn cast_bar_label(spells: Option<&crate::ui_action::Spells>, id: u32) -> String {
    spells
        .and_then(|s| s.catalog.get(id))
        .filter(|d| !d.no_casting_bar_text())
        .map(|d| d.name.clone())
        .unwrap_or_default()
}

/// Drain the queue into the stock events: `SPELLCAST_START(name, ms)`,
/// `SPELLCAST_CHANNEL_START(ms, name)` (reversed, as `CastingBarFrame.lua:80-84` reads it),
/// `SPELLCAST_CHANNEL_UPDATE(ms)` and the argless rest. A channel update of 0 is
/// `SPELLCAST_CHANNEL_STOP`: vmangos ends a channel that way, finished or interrupted.
///
/// First it pushes the flag `SpellStopCasting` (`0x6e6e80`) answers from: auto-repeat or the
/// in-flight slot, never a channel, taken after the local cancel. A queued strike counts, so the
/// first Esc stops it and `ToggleGameMenu` skips `ClearTarget()` (`UIParent.lua:1489-1492`).
pub(crate) fn feed_cast_bar(
    script: Option<NonSendMut<UiScript>>,
    mut feed: ResMut<CastBarFeed>,
    spells: Option<Res<Spells>>,
    pending: Res<PendingCast>,
    queued_melee: Res<QueuedMeleeSpell>,
    auto_repeat: Res<crate::spell::AutoRepeatActive>,
    self_guid: Res<SelfGuid>,
    index: Res<GuidIndex>,
    casting: Query<&Casting>,
) {
    let Some(mut script) = script else {
        return;
    };
    let now = Instant::now();
    // The auto-repeat key (`0xceac30`), which the reference reads first, then the in-flight slot
    // the cancel resolves, so the flag and the cancel agree.
    let started = self_guid
        .0
        .as_ref()
        .and_then(|g| index.0.get(g))
        .and_then(|&e| casting.get(e).ok())
        .map(|c| c.spell_id);
    script.set_casting(
        auto_repeat.0.is_some() || inflight(&pending, &queued_melee, started, now).is_some(),
    );
    for edge in feed.0.drain(..) {
        let player = vec![ScriptValue::Str("player".into())];
        match edge {
            CastBarEdge::Start {
                spell_id,
                cast_time_ms,
            } => {
                let now_secs = script.now();
                script.set_active_cast(
                    cast_bar_label(spells.as_deref(), spell_id),
                    String::new(),
                    now_secs,
                    now_secs + f64::from(cast_time_ms) / 1000.0,
                );
                script.fire_event("UNIT_SPELLCAST_START", player);
            }
            CastBarEdge::Stop => {
                script.clear_active_cast();
                script.fire_event("UNIT_SPELLCAST_STOP", player);
            }
            CastBarEdge::Failed => {
                script.clear_active_cast();
                script.fire_event("UNIT_SPELLCAST_FAILED", player);
            }
            CastBarEdge::Interrupted => {
                script.clear_active_cast();
                script.fire_event("UNIT_SPELLCAST_INTERRUPTED", player);
            }
            CastBarEdge::Delayed { .. } => {
                script.fire_event("UNIT_SPELLCAST_DELAYED", player);
            }
            CastBarEdge::ChannelStart { .. } => {
                script.fire_event("UNIT_SPELLCAST_CHANNEL_START", player);
            }
            CastBarEdge::ChannelUpdate { remaining_ms } => {
                script.fire_event(
                    if remaining_ms == 0 {
                        "UNIT_SPELLCAST_CHANNEL_STOP"
                    } else {
                        "UNIT_SPELLCAST_CHANNEL_UPDATE"
                    },
                    player,
                );
            }
        }
    }
}

/// The cast-bar feed, drained before the VM ticks so an edge and its first `OnUpdate` share a
/// frame, and after the local cancel so a move or Esc kills the bar on the next tick.
pub(crate) struct UiCastPlugin;

impl Plugin for UiCastPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CastBarFeed>().add_systems(
            Update,
            // After the spell's local cancel, whose edges this drains the same frame.
            feed_cast_bar
                .in_set(UnitFeed)
                .after(crate::spell::LocalCancel),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spell 22810, "Opening - No Text", carries `SPELL_ATTR_EX3_NO_CASTING_BAR_TEXT`.
    #[test]
    fn the_no_casting_bar_text_attribute_blanks_the_label() {
        use benilla_formats::{SpellCatalog, SpellDisplay};
        let named = |name: &str, attributes_ex3: u32| SpellDisplay {
            name: name.to_string(),
            attributes_ex3,
            ..Default::default()
        };
        let spells = crate::ui_action::Spells {
            catalog: SpellCatalog::from_displays(
                [
                    (6478, named("Opening", 0)),
                    (22810, named("Opening - No Text", 0x4)),
                ]
                .into_iter()
                .collect(),
            ),
            ..crate::ui_action::Spells::empty_for_tests()
        };
        assert_eq!(cast_bar_label(Some(&spells), 6478), "Opening");
        assert_eq!(
            cast_bar_label(Some(&spells), 22810),
            "",
            "the event still fires carrying its duration — a full-length UNTITLED bar, \
             not a hidden one (0x6e7a33 pushes the empty string, then fires as usual)"
        );
        // An unknown id, and no client data at all.
        assert_eq!(cast_bar_label(Some(&spells), 133), "");
        assert_eq!(cast_bar_label(None, 6478), "");
    }
}
