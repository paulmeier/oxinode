//! Host-side conventions carried over the USB serial link.

/// Baud rate hosts use to ask a board to enter its bootloader.
///
/// Nothing transmits at 1200 baud for real, which is exactly why Arduino and
/// Adafruit tooling overloaded it. oxinode's own traffic runs at 115200.
pub const BOOTLOADER_TOUCH_BAUD: u32 = 1200;

/// Whether the host just performed the "1200-baud touch".
///
/// The gesture is *open the port at 1200 baud, then close it*, and the close is
/// the operative half: DTR dropping is what says the host is finished. Acting on
/// the open alone would reboot the board the instant a tool probed it at that
/// rate, and inverting the DTR test makes the board unflashable without reaching
/// for the reset button.
pub fn is_bootloader_touch(data_rate: u32, dtr: bool) -> bool {
    data_rate == BOOTLOADER_TOUCH_BAUD && !dtr
}

/// How long after boot a 1200-baud close is ignored.
///
/// The line coding survives in the *host's* terminal settings, not ours, and
/// macOS re-applies the cached settings for a device path when it probes a
/// newly attached device. So a board that has just been flashed can come up,
/// be opened at 1200 by nobody in particular, and be closed again — which is
/// indistinguishable, byte for byte, from a deliberate touch.
///
/// A board that acted on it reboots straight back into its bootloader, and
/// then does it again on the next boot. That is not hypothetical: it is what
/// this rule was added for.
///
/// Two seconds is chosen against the flasher's own timing rather than by feel.
/// `adafruit-nrfutil` holds the port open at 1200 for 100 ms and then allows
/// **1.5 s** for the device to reboot and enumerate, so a real touch is always
/// aimed at an instance that has been running far longer than this. Anything
/// seen inside the window is leftover line state.
pub const TOUCH_GRACE_MS: u64 = 2_000;

/// [`is_bootloader_touch`], ignoring anything seen too soon after boot.
///
/// See [`TOUCH_GRACE_MS`]. This is the one callers should use; the bare rule
/// is kept because it is the thing being qualified, and because the tests
/// below are about it.
pub fn is_bootloader_touch_after(data_rate: u32, dtr: bool, uptime_ms: u64) -> bool {
    uptime_ms >= TOUCH_GRACE_MS && is_bootloader_touch(data_rate, dtr)
}

// The window has to sit between two numbers from the flasher: it must outlast
// the 1.5 s `adafruit-nrfutil` allows for a touched board to reboot and
// enumerate -- so that a leftover line state at boot is never mistaken for a
// touch aimed at this instance -- and it must stay short enough that a board
// which really was touched is not left waiting.
const _: () = assert!(TOUCH_GRACE_MS >= 1_600);
const _: () = assert!(TOUCH_GRACE_MS <= 5_000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_only_on_close_at_1200() {
        assert!(is_bootloader_touch(1200, false));
    }

    #[test]
    fn does_not_fire_while_the_port_is_still_open() {
        assert!(!is_bootloader_touch(1200, true));
    }

    #[test]
    fn ignores_every_other_baud_rate() {
        // 115200 in particular: the rate Reticulum will hold the port open at,
        // opening and closing it routinely.
        for rate in [0, 9600, 57600, 115_200, 921_600] {
            assert!(!is_bootloader_touch(rate, false), "rate {rate} triggered");
            assert!(!is_bootloader_touch(rate, true), "rate {rate} triggered");
        }
    }

    #[test]
    fn the_magic_rate_is_the_documented_one() {
        assert_eq!(BOOTLOADER_TOUCH_BAUD, 1200);
    }

    /// A board that has only just booted must ignore the condition, or a host
    /// re-applying its cached 1200-baud settings to a freshly attached device
    /// sends it straight back to the bootloader -- and then again, and again.
    #[test]
    fn a_touch_in_the_first_moments_after_boot_is_not_one() {
        for uptime in [0, 1, 100, 1_999] {
            assert!(
                !is_bootloader_touch_after(1200, false, uptime),
                "fired at {uptime} ms"
            );
        }
    }

    /// And once past it, the rule is exactly the rule.
    #[test]
    fn after_the_grace_window_nothing_else_changes() {
        for uptime in [2_000, 2_001, 60_000, u64::MAX] {
            assert!(is_bootloader_touch_after(1200, false, uptime));
            assert!(!is_bootloader_touch_after(1200, true, uptime));
            assert!(!is_bootloader_touch_after(115_200, false, uptime));
        }
    }
}
