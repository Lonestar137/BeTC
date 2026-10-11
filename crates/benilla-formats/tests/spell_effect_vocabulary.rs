//! Build-8606 `Spell.dbc` rows pinning the effect vocabulary the offline cast resolver maps:
//! the four weapon-damage arms, the rogue builder and finishing move, and the on-next-swing bit.
//! Each constant is cited to these rows in `benilla-formats::spells`.

use benilla_formats::{
    load_spell_catalog, open_chain, SPELL_AURA_MOD_CONFUSE, SPELL_AURA_MOD_DECREASE_SPEED,
    SPELL_AURA_MOD_FEAR, SPELL_AURA_MOD_ROOT, SPELL_AURA_MOD_STUN, SPELL_AURA_PERIODIC_DAMAGE,
    SPELL_AURA_PERIODIC_LEECH, SPELL_AURA_PERIODIC_TRIGGER_SPELL, SPELL_EFFECT_ADD_COMBO_POINTS,
    SPELL_EFFECT_APPLY_AURA, SPELL_EFFECT_NORMALIZED_WEAPON_DMG, SPELL_EFFECT_SCHOOL_DAMAGE,
    SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL, SPELL_EFFECT_WEAPON_PERCENT_DAMAGE,
};

#[test]
fn spell_effect_vocabulary_matches_build_8606_rows() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("spell catalog");

    // Heroic Strike: "weapon damage plus $s1", queued on the next swing, "150 on the wire is
    // 15 Rage".
    let hs = cat.get(78).expect("Heroic Strike");
    assert_eq!(hs.effects, [SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL, 0, 0]);
    assert!(hs.on_next_swing());
    assert_eq!((hs.power_type, hs.mana_cost), (1, 150));
    assert_eq!(hs.effect_base_points[0], 10);

    // Sinister Strike: normalized weapon damage "in addition to your normal weapon damage",
    // "Awards $s2 combo point", 45 energy.
    let ss = cat.get(1752).expect("Sinister Strike");
    assert_eq!(
        ss.effects,
        [
            SPELL_EFFECT_NORMALIZED_WEAPON_DMG,
            SPELL_EFFECT_ADD_COMBO_POINTS,
            0
        ]
    );
    assert!(!ss.on_next_swing());
    assert_eq!((ss.power_type, ss.mana_cost), (3, 45));

    // Backstab: the percent arm beside the flat one — "causing $s2% weapon damage plus 15".
    let bs = cat.get(53).expect("Backstab");
    assert_eq!(
        bs.effects,
        [
            SPELL_EFFECT_NORMALIZED_WEAPON_DMG,
            SPELL_EFFECT_WEAPON_PERCENT_DAMAGE,
            SPELL_EFFECT_ADD_COMBO_POINTS
        ]
    );
    assert_eq!(bs.effect_base_points[1] + 1, 150, "the percent is base + 1");

    // Eviscerate: a finishing move carrying its damage per combo point.
    let ev = cat.get(2098).expect("Eviscerate");
    assert!(ev.needs_combo_points());
    assert_eq!(ev.effect_points_per_combo_point[0], 5.0);
}

/// The channel and movement-control rows the offline resolver maps: the per-tick arms a channel
/// reads (Mind Flay, Drain Life, Arcane Missiles) and the control auras (Frostbolt's snare, Frost
/// Nova's root, Hammer of Justice's stun), with the disorient families named as blockers.
#[test]
fn channel_and_control_rows_match_build_8606_shapes() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("spell catalog");

    // Drain Life: one periodic-leech aura, ticking on the amplitude, healing the caster.
    let drain = cat.get(689).expect("Drain Life");
    assert!(drain.tooltip_channeled());
    assert_eq!(drain.effects, [SPELL_EFFECT_APPLY_AURA, 0, 0]);
    assert_eq!(drain.effect_apply_aura, [SPELL_AURA_PERIODIC_LEECH, 0, 0]);
    assert_eq!(drain.effect_amplitude, [1_000, 0, 0]);

    // Mind Flay: a periodic-damage tick beside its snare.
    let flay = cat.get(15407).expect("Mind Flay");
    assert!(flay.tooltip_channeled());
    assert_eq!(
        flay.effect_apply_aura,
        [SPELL_AURA_PERIODIC_DAMAGE, SPELL_AURA_MOD_DECREASE_SPEED, 0]
    );
    assert_eq!(flay.effect_amplitude, [1_000, 0, 0]);

    // Arcane Missiles: the tick is the triggered spell's own damage on the amplitude. The first
    // arm is the row's dummy (aura 4); the trigger rides the second arm.
    let missiles = cat.get(5143).expect("Arcane Missiles");
    assert!(missiles.tooltip_channeled());
    assert_eq!(
        missiles.effect_apply_aura,
        [4, SPELL_AURA_PERIODIC_TRIGGER_SPELL, 0]
    );
    assert_eq!(missiles.effect_trigger_spell, [0, 7268, 0]);
    assert_eq!(missiles.effect_amplitude, [0, 1_000, 0]);

    // Frostbolt's snare rides its damage arm; Frost Nova roots beside its damage.
    let frostbolt = cat.get(116).expect("Frostbolt");
    assert_eq!(
        frostbolt.effects,
        [SPELL_EFFECT_APPLY_AURA, SPELL_EFFECT_SCHOOL_DAMAGE, 0]
    );
    assert_eq!(
        frostbolt.effect_apply_aura,
        [SPELL_AURA_MOD_DECREASE_SPEED, 0, 0]
    );
    let nova = cat.get(122).expect("Frost Nova");
    assert_eq!(nova.effect_apply_aura, [0, SPELL_AURA_MOD_ROOT, 0]);

    // Hammer of Justice is a bare stun: no direct amount anywhere on the row.
    let justice = cat.get(853).expect("Hammer of Justice");
    assert_eq!(justice.effects, [SPELL_EFFECT_APPLY_AURA, 0, 0]);
    assert_eq!(justice.effect_apply_aura, [SPELL_AURA_MOD_STUN, 0, 0]);

    // The disorient families carry their own aura ids and resolve as stuns (wander and flee are
    // not modeled).
    assert_eq!(
        cat.get(118).expect("Polymorph").effect_apply_aura[0],
        SPELL_AURA_MOD_CONFUSE
    );
    assert_eq!(
        cat.get(5782).expect("Fear").effect_apply_aura[0],
        SPELL_AURA_MOD_FEAR
    );
}
