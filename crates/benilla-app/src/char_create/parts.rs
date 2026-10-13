//! The create screen's markers: [`super::screen`] spawns them, [`super::refresh`] drives them.

use bevy::prelude::*;

/// The screen root; `s` is the glue scale the tree was built at, rebuilt when it changes.
#[derive(Component)]
pub(super) struct CharCreateUi {
    pub(super) s: f32,
}
#[derive(Component)]
pub(super) struct InfoPanel;
#[derive(Component)]
pub(super) struct BodyOptionsPanel;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BodyShapeField {
    Height,
    Shoulders,
    Arms,
    Waist,
    Hips,
    Bust,
    Seat,
}

impl BodyShapeField {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Height => "Height",
            Self::Shoulders => "Shoulders",
            Self::Arms => "Arms",
            Self::Waist => "Waist",
            Self::Hips => "Hips",
            Self::Bust => "Bust",
            Self::Seat => "Rear",
        }
    }

    pub(super) fn range(self) -> (i8, i8) {
        match self {
            Self::Height => (-10, 35),
            Self::Seat => (-20, 40),
            _ => (-20, 20),
        }
    }

    pub(super) fn get(self, p: crate::body_shape::BodyProportions) -> i8 {
        match self {
            Self::Height => p.height,
            Self::Shoulders => p.shoulders,
            Self::Arms => p.arms,
            Self::Waist => p.waist,
            Self::Hips => p.hips,
            Self::Bust => p.bust,
            Self::Seat => p.seat,
        }
    }

    pub(super) fn set(self, p: &mut crate::body_shape::BodyProportions, value: i8) {
        let (min, max) = self.range();
        let value = value.clamp(min, max);
        match self {
            Self::Height => p.height = value,
            Self::Shoulders => p.shoulders = value,
            Self::Arms => p.arms = value,
            Self::Waist => p.waist = value,
            Self::Hips => p.hips = value,
            Self::Bust => p.bust = value,
            Self::Seat => p.seat = value,
        }
    }
}

#[derive(Component)]
pub(super) struct BodyShapeRow(pub(super) BodyShapeField);
#[derive(Component)]
pub(super) struct BodySlider(pub(super) BodyShapeField);
#[derive(Component)]
pub(super) struct BodySliderKnob(pub(super) BodyShapeField);
#[derive(Component)]
pub(super) struct BodySliderValue(pub(super) BodyShapeField);
/// The status line.
#[derive(Component)]
pub(super) struct StatusLine;

/// The three info panels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InfoKind {
    Faction,
    Race,
    Class,
}

/// An icon whose texture rect follows the selection.
#[derive(Component)]
pub(super) enum DynIcon {
    /// A race button's icon; the rect follows the selected sex.
    Race(u8),
    /// A class slot's icon; its class follows the selected race.
    ClassSlot(u8),
    /// An info-panel header icon.
    Info(InfoKind),
}

/// A text whose content follows the selection.
#[derive(Component, Clone)]
pub(super) enum DynText {
    DialLabel(u8),
    Name,
    InfoTitle(InfoKind),
    InfoBody(InfoKind),
    /// The race panel's ability lines, the reference's separate gold
    /// `CharacterCreateRaceAbilityText`.
    InfoAbilities,
    BodyShapeStatus,
    /// A class slot's hover label.
    ClassSlotLabel(u8),
}

/// A tint that follows the selected faction, the reference's `SetBackdropColor`: an `ImageNode`
/// tint with art, a `BackgroundColor` without.
#[derive(Component)]
pub(super) enum DynTint {
    Backdrop,
    BoxFill,
}

/// A dial row; row 4, facial hair, hides when the race's token is `NONE`, as in the reference.
#[derive(Component)]
pub(super) struct DialRow(pub(super) u8);
/// An info panel's scrollable body, wheel-scrolled while hovered.
#[derive(Component)]
pub(super) struct InfoScroll;
/// A scrollbar arrow (`GlueScrollUp/DownButtonTemplate`), disabled at its end stop.
#[derive(Component)]
pub(super) struct ScrollArrow {
    pub(super) scroll: Entity,
    /// The up button, scrolling toward 0.
    pub(super) up: bool,
    /// The click step in logical px, the reference's `GetHeight()/2`: half the track.
    pub(super) step: f32,
}
/// The scrollbar knob (`UI-ScrollBar-Knob`), draggable.
#[derive(Component)]
pub(super) struct ScrollThumb {
    pub(super) scroll: Entity,
    /// Track minus knob, logical px.
    pub(super) travel: f32,
}
/// A scrollbar piece hidden while its frame has nothing to scroll: the reference's
/// `scrollBarHideable` slider and the track art's range-changed hide.
#[derive(Component)]
pub(super) struct ScrollHides {
    pub(super) scroll: Entity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_slider_reaches_forty_percent_but_other_shape_sliders_stop_at_twenty() {
        assert_eq!(BodyShapeField::Seat.range(), (-20, 40));
        assert_eq!(BodyShapeField::Hips.range(), (-20, 20));

        let mut proportions = crate::body_shape::BodyProportions::default();
        BodyShapeField::Seat.set(&mut proportions, 50);
        assert_eq!(proportions.seat, 40);
        BodyShapeField::Hips.set(&mut proportions, 50);
        assert_eq!(proportions.hips, 20);
    }
}
