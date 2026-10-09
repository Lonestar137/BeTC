use bevy::prelude::*;
use std::collections::BTreeMap;

use benilla_assets::materials::WowModelMaterial;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BodyProportions {
    pub(crate) enabled: bool,
    pub(crate) height: i8,
    pub(crate) shoulders: i8,
    pub(crate) arms: i8,
    pub(crate) waist: i8,
    pub(crate) hips: i8,
    pub(crate) bust: i8,
    pub(crate) seat: i8,
}

#[derive(Resource, Default)]
pub(crate) struct BodyProportionRoster(pub(crate) BTreeMap<u64, BodyProportions>);

impl BodyProportions {
    pub(crate) fn is_valid(self) -> bool {
        (-10..=35).contains(&self.height)
            && [self.shoulders, self.arms, self.waist, self.hips, self.bust]
                .into_iter()
                .all(|value| (-20..=20).contains(&value))
            && (-20..=40).contains(&self.seat)
    }

    pub(crate) fn height_scale(self) -> f32 {
        1.0 + f32::from(self.height.clamp(-10, 35)) / 100.0
    }

    pub(crate) fn has_shape_changes(self) -> bool {
        self.shoulders != 0
            || self.arms != 0
            || self.waist != 0
            || self.hips != 0
            || self.bust != 0
            || self.seat != 0
    }
}

#[cfg(feature = "offline")]
impl From<benilla_game_core::BodyProportions> for BodyProportions {
    fn from(value: benilla_game_core::BodyProportions) -> Self {
        Self {
            enabled: value.enabled,
            height: value.height,
            shoulders: value.shoulders,
            arms: value.arms,
            waist: value.waist,
            hips: value.hips,
            bust: value.bust,
            seat: value.seat,
        }
    }
}

#[cfg(feature = "offline")]
impl From<BodyProportions> for benilla_game_core::BodyProportions {
    fn from(value: BodyProportions) -> Self {
        Self {
            enabled: value.enabled,
            height: value.height,
            shoulders: value.shoulders,
            arms: value.arms,
            waist: value.waist,
            hips: value.hips,
            bust: value.bust,
            seat: value.seat,
        }
    }
}

pub(crate) fn bounds_uniform(bounds: benilla_formats::M2Bounds) -> Vec4 {
    let a = benilla_assets::coords::wow_to_bevy(bounds.bbox_min);
    let b = benilla_assets::coords::wow_to_bevy(bounds.bbox_max);
    let min = a.min(b);
    let max = a.max(b);
    Vec4::new(min.y, max.y, (max.x - min.x) * 0.5, (max.z - min.z) * 0.5)
}

pub(crate) fn shape_material(
    materials: &mut Assets<WowModelMaterial>,
    source: &Handle<WowModelMaterial>,
    proportions: BodyProportions,
    race: u8,
    gender: u8,
    bounds: Option<benilla_formats::M2Bounds>,
) -> Handle<WowModelMaterial> {
    let Some(bounds) = bounds else {
        return source.clone();
    };
    if !proportions.enabled || !proportions.is_valid() || !proportions.has_shape_changes() {
        return source.clone();
    }
    let Some(mut material) = materials.get(source.id()).cloned() else {
        return source.clone();
    };
    if !apply_profile(&mut material, proportions, race, gender, bounds) {
        return source.clone();
    }
    materials.add(material)
}

pub(crate) fn apply_profile(
    material: &mut WowModelMaterial,
    proportions: BodyProportions,
    race: u8,
    gender: u8,
    bounds: benilla_formats::M2Bounds,
) -> bool {
    let Some((shape, shape2, b, regions)) = shape_uniforms(proportions, race, gender, bounds)
    else {
        return false;
    };
    let p = &mut material.extension;
    p.body_shape = shape;
    p.body_shape2 = shape2;
    p.body_shape_bounds = b;
    p.body_shape_regions = regions;
    true
}

fn shape_uniforms(
    proportions: BodyProportions,
    race: u8,
    gender: u8,
    bounds: benilla_formats::M2Bounds,
) -> Option<(Vec4, Vec4, Vec4, Vec4)> {
    let b = bounds_uniform(bounds);
    if !proportions.enabled
        || !proportions.is_valid()
        || !proportions.has_shape_changes()
        || !b.is_finite()
        || b.y <= b.x
        || b.z <= 0.0
        || b.w <= 0.0
    {
        return None;
    }
    let regions = match race {
        4 => Vec4::new(0.09, 0.0, 0.09, 0.0), // Night Elf pelvis and seat sit higher in the model.
        10 => Vec4::new(0.0, -0.12, 0.0, 0.0), // Blood Elf chest sits below the head-heavy bounds.
        _ => Vec4::ZERO,
    };
    Some((
        Vec4::new(
            f32::from(proportions.shoulders) / 100.0,
            f32::from(proportions.arms) / 100.0,
            f32::from(proportions.waist) / 100.0,
            f32::from(proportions.hips) / 100.0,
        ),
        Vec4::new(
            f32::from(proportions.bust) / 100.0,
            f32::from(proportions.seat) / 100.0,
            f32::from(gender == 1),
            1.0,
        ),
        b,
        regions,
    ))
}

pub(crate) fn height_scale(proportions: BodyProportions) -> f32 {
    if proportions.enabled {
        proportions.height_scale()
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bounds() -> benilla_formats::M2Bounds {
        benilla_formats::M2Bounds {
            sphere_radius: 1.0,
            bbox_min: [-1.0, -0.5, 0.0],
            bbox_max: [1.0, 0.5, 2.0],
            vert_max_from_origin: 2.0,
            ring_footprint: 1.0,
            pivot_z: None,
            stand_box_z: 2.0,
            swim_pivot_drop: 0.0,
        }
    }

    #[test]
    fn default_profile_is_an_exact_visual_identity_and_height_is_capped() {
        let default = BodyProportions::default();
        assert_eq!(height_scale(default), 1.0);
        assert!(shape_uniforms(default, 10, 1, test_bounds()).is_none());

        let tall = BodyProportions {
            enabled: true,
            height: 35,
            ..default
        };
        assert!((height_scale(tall) - 1.35).abs() < 1e-6);
        assert!(shape_uniforms(tall, 10, 1, test_bounds()).is_none());
        assert!(
            shape_uniforms(BodyProportions { height: 36, ..tall }, 10, 1, test_bounds()).is_none()
        );
    }

    #[test]
    fn model_shape_uniforms_are_bounded_and_gender_aware() {
        let proportions = BodyProportions {
            enabled: true,
            shoulders: 20,
            arms: -20,
            waist: -10,
            hips: 15,
            bust: 20,
            seat: 10,
            ..default()
        };
        let (shape, female, bounds, blood_elf_regions) =
            shape_uniforms(proportions, 10, 1, test_bounds()).unwrap();
        assert_eq!(shape, Vec4::new(0.2, -0.2, -0.1, 0.15));
        assert_eq!(female, Vec4::new(0.2, 0.1, 1.0, 1.0));
        assert!(bounds.y > bounds.x && bounds.z > 0.0 && bounds.w > 0.0);
        assert_eq!(blood_elf_regions.y, -0.12);

        let max_seat = BodyProportions {
            seat: 40,
            ..proportions
        };
        let (_, seat_shape, _, _) = shape_uniforms(max_seat, 1, 1, test_bounds()).unwrap();
        assert_eq!(seat_shape.y, 0.4);

        let (_, male, _, night_elf_regions) =
            shape_uniforms(proportions, 4, 0, test_bounds()).unwrap();
        assert_eq!(male.z, 0.0, "bust deformation is disabled for male models");
        assert_eq!(night_elf_regions.x, 0.09);
        assert_eq!(night_elf_regions.z, 0.09);
    }

    #[test]
    fn buttocks_accepts_forty_percent_without_widening_other_shape_limits() {
        let profile = BodyProportions {
            enabled: true,
            seat: 40,
            ..default()
        };
        assert!(profile.is_valid());
        assert!(
            !BodyProportions {
                seat: 41,
                ..profile
            }
            .is_valid()
        );
        assert!(
            !BodyProportions {
                hips: 21,
                ..profile
            }
            .is_valid()
        );
    }
}
