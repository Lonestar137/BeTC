//! Build-8606 `Spell.dbc` rows pinning the effect vocabulary the offline cast resolver maps:
//! the four weapon-damage arms, the rogue builder and finishing move, and the on-next-swing bit.
//! Each constant is cited to these rows in `benilla-formats::spells`.

use benilla_formats::{
    load_spell_catalog, open_chain, SPELL_EFFECT_ADD_COMBO_POINTS,
    SPELL_EFFECT_NORMALIZED_WEAPON_DMG, SPELL_EFFECT_WEAPON_DAMAGE_NOSCHOOL,
    SPELL_EFFECT_WEAPON_PERCENT_DAMAGE,
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
