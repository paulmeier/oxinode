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

/// How long the log pump holds off after the host raises DTR before it drains.
///
/// Raising DTR is not the terminal program's doing: the host's serial driver
/// does it inside `open(2)`, before the program that asked has done anything
/// else. Measured on macOS 26 on 2026-09-10, `TIOCMGET` straight after a bare
/// `open` already reports DTR and RTS high. The program then sets the line
/// coding and, almost always, **flushes the input queue** before its first
/// read -- pyserial does (`tcflush(TCIFLUSH)` at the end of `open()`), and so
/// do `screen`, `picocom` and `minicom`. Anything the board sent between the
/// open and that flush is thrown away.
///
/// A board whose ring holds the whole of boot sends the whole of boot within
/// a millisecond of noticing DTR, so this is exactly the race the boot log
/// loses. That is what looked like DTR failing on the second CDC function
/// (issue #10): the pump polls DTR every 20 ms and pyserial's open takes about
/// 4 ms, so the log vanished on roughly one open in five -- and every time
/// once the flush was moved 50 ms after the open. DTR on the second function
/// was fine throughout.
///
/// A quarter of a second is far beyond any open sequence and below what a
/// person at a terminal notices.
pub const OPEN_SETTLE_MS: u64 = 250;

// Long enough that a slow host has finished opening, short enough that a
// person does not wonder whether the port is dead.
const _: () = assert!(OPEN_SETTLE_MS >= 100);
const _: () = assert!(OPEN_SETTLE_MS <= 1_000);

/// When the log pump may drain the ring, decided from DTR and the clock.
///
/// `true` from [`DrainGate::poll`] means "send now". It is `false` while the
/// port is closed, and for [`OPEN_SETTLE_MS`] after each time it opens; a drop
/// of DTR closes the gate, and the next raise starts the wait again.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainGate {
    /// When DTR was last seen rising, while it is up.
    raised_at: Option<u64>,
}

impl DrainGate {
    pub const fn new() -> Self {
        Self { raised_at: None }
    }

    /// Feed the current line state and clock; `true` means drain now.
    pub fn poll(&mut self, dtr: bool, now_ms: u64) -> bool {
        if !dtr {
            self.raised_at = None;
            return false;
        }
        let raised_at = *self.raised_at.get_or_insert(now_ms);
        now_ms.saturating_sub(raised_at) >= OPEN_SETTLE_MS
    }
}

#[cfg(test)]
mod drain_gate {
    use super::*;

    #[test]
    fn a_closed_port_never_drains() {
        let mut gate = DrainGate::new();
        for now in [0, 1, OPEN_SETTLE_MS, 60_000, u64::MAX] {
            assert!(!gate.poll(false, now), "drained at {now} ms with DTR low");
        }
    }

    /// The host raises DTR inside `open(2)` and flushes its input a few
    /// milliseconds later. Draining inside that gap loses the log.
    #[test]
    fn an_open_port_drains_once_the_host_has_had_time_to_flush() {
        let mut gate = DrainGate::new();
        assert!(!gate.poll(true, 1_000));
        assert!(!gate.poll(true, 1_004));
        assert!(!gate.poll(true, 1_000 + OPEN_SETTLE_MS - 1));
        assert!(gate.poll(true, 1_000 + OPEN_SETTLE_MS));
        assert!(gate.poll(true, 60_000));
    }

    /// A drop is a close, or a program that lowers DTR on purpose. Either way
    /// the next raise is a fresh open, with a fresh flush behind it.
    #[test]
    fn a_drop_closes_the_gate_and_a_raise_starts_the_wait_again() {
        let mut gate = DrainGate::new();
        assert!(gate.poll(true, 0) || gate.poll(true, OPEN_SETTLE_MS));
        assert!(!gate.poll(false, OPEN_SETTLE_MS + 1));
        assert!(!gate.poll(true, OPEN_SETTLE_MS + 2));
        assert!(!gate.poll(true, 2 * OPEN_SETTLE_MS + 1));
        assert!(gate.poll(true, 2 * OPEN_SETTLE_MS + 2));
    }

    /// The images that pass `|| true` see the same rule: one wait after the
    /// endpoint comes up, and then the stream.
    #[test]
    fn a_gate_fed_true_from_the_start_opens_after_one_settle() {
        let mut gate = DrainGate::new();
        assert!(!gate.poll(true, 0));
        assert!(gate.poll(true, OPEN_SETTLE_MS));
    }

    /// The clock is milliseconds since boot and only goes up, but the rule
    /// should not care if it were handed something odd.
    #[test]
    fn a_clock_that_goes_backwards_does_not_panic() {
        let mut gate = DrainGate::new();
        assert!(!gate.poll(true, 5_000));
        assert!(!gate.poll(true, 10));
    }
}
