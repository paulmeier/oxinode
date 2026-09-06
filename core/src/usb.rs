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
}
