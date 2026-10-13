//! The stowed-weapon hip attach bones (HumanMale 29 and 30) are carried into every sequence; build
//! 8606 animates them rather than pinning them to one global-sequence constant key.

use benilla_formats::{open_chain, parse_m2_animations};

/// The hip-sheath attach bone (attachment 32).
const HIP_BONE: u16 = 29;

#[test]
fn stow_attach_bones_are_carried_into_every_sequence() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let bytes = chain
        .read_file("Character\\Human\\Male\\HumanMale.m2")
        .expect("HumanMale.m2 in the chain");
    let anims = parse_m2_animations(&bytes);
    assert!(!anims.is_empty(), "HumanMale should have sequences");

    for anim in &anims {
        let hip = anim
            .bones
            .iter()
            .find(|bk| bk.bone == HIP_BONE)
            .unwrap_or_else(|| {
                panic!(
                    "anim {}: hip attach bone {HIP_BONE} missing from the keyframe set — the \
                     global-seq constant was dropped again",
                    anim.anim_id
                )
            });
        assert!(
            !hip.rotation.is_empty(),
            "anim {}: hip attach bone {HIP_BONE} carries no rotation key",
            anim.anim_id
        );
    }
}

