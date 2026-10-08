use crate::messages::MovementInfo;
use crate::wire::Vector3d;
use std::sync::atomic::{AtomicU32, Ordering};

pub(super) const MOVEMENT_FLAG_FORWARD: u32 = 0x1;

static LAST_MOVEMENT_TIMESTAMP: AtomicU32 = AtomicU32::new(0);

fn elapsed_client_uptime_ms() -> u32 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    (START.get_or_init(Instant::now).elapsed().as_millis() as u32).max(1)
}

/// Ms since start for the `MovementInfo` time, as the 1.12 client's `GetTickCount()`, never 0:
/// vmangos pauses extrapolation on 0 and otherwise uses only deltas, so a full `u32` wrap is safe.
pub(super) fn client_uptime_ms() -> u32 {
    let timestamp = elapsed_client_uptime_ms();
    LAST_MOVEMENT_TIMESTAMP.store(timestamp, Ordering::Relaxed);
    timestamp
}

/// vmangos adds this report to its last client timestamp; don't report simulation time already
/// present in our monotonic `MovementInfo` clock.
fn cap_skipped_movement_ms(requested: u32, previous: u32, now: u32) -> u32 {
    requested.min(now.wrapping_sub(previous))
}

pub(super) fn skipped_movement_ms(requested: u32) -> u32 {
    let previous = LAST_MOVEMENT_TIMESTAMP.load(Ordering::Relaxed);
    if previous == 0 {
        return requested;
    }
    cap_skipped_movement_ms(requested, previous, elapsed_client_uptime_ms())
}

/// Build a `MovementInfo` stamped with [`client_uptime_ms`].
pub(super) fn movement_info(pos: [f32; 3], orientation: f32, flags: u32) -> MovementInfo {
    MovementInfo {
        flags,
        timestamp: client_uptime_ms(),
        position: Vector3d {
            x: pos[0],
            y: pos[1],
            z: pos[2],
        },
        orientation,
        // Written only under ON_TRANSPORT, which benilla does not send.
        transport: None,
        // Written only under SWIMMING; `send_movement` supplies the live pitch then.
        pitch: 0.0,
        fall_time: 0,
        jump: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Never 0, and a stamp taken right after is not smaller; it wraps at `u32::MAX` ms.
    #[test]
    fn client_uptime_ms_is_nonzero_and_back_to_back_calls_do_not_decrease() {
        let a = client_uptime_ms();
        let b = client_uptime_ms();
        assert!(a >= 1, "stamp is non-zero: {a}");
        assert!(b >= a, "the next stamp is not smaller: {a} -> {b}");
    }

    #[test]
    fn movement_info_is_stamped_nonzero() {
        let info = movement_info([1.0, 2.0, 3.0], 0.5, MOVEMENT_FLAG_FORWARD);
        assert_ne!(
            info.timestamp, 0,
            "a movement packet must carry a non-zero time"
        );
    }

    #[test]
    fn skipped_time_never_moves_the_server_clock_past_the_next_client_stamp() {
        assert_eq!(cap_skipped_movement_ms(964, 17_442, 18_198), 756);
        assert_eq!(cap_skipped_movement_ms(955, 28_746, 29_498), 752);
        assert_eq!(cap_skipped_movement_ms(50, 0xffff_fff0, 0x10), 32);
    }
}
