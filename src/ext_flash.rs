//! The W25Q128 QSPI flash, from the firmware's side.
//!
//! Two ways to reach the chip. [`Bitbang`] drives its six pins as GPIOs,
//! one bit at a time, and is used for the commands that have to work
//! whatever state the chip is in: waking it, reading its ID, and putting it
//! to sleep. [`config`] is for the QSPI peripheral, which the `qspi` image
//! uses for erasing, writing and reading.
//!
//! # Why the peripheral does not wake the chip
//!
//! `embassy-nrf`'s custom instructions have the peripheral poll the busy bit
//! before sending anything. A chip in deep power-down answers nothing but
//! `ABh`, and a reset of the nRF52 does not wake it, so the first instruction
//! after a reboot could wait on a status that never comes. Waking it on the
//! GPIOs first makes that impossible.
//!
//! # Why sleeping is done here too
//!
//! The peripheral can enter deep power-down itself, but when it is dropped
//! it lets go of the data pins and leaves them floating. The product image
//! never uses the chip, so it calls [`Bitbang::sleep`] at boot and keeps the
//! [`Bitbang`] for the rest of its life: /CS held high, the clock low, /WP
//! and /HOLD high, and IO1 pulled down, so nothing floats.
//!
//! IO1 is the chip's output in single-line SPI. The pull-down makes an
//! undriven line read zero, which is how [`Bitbang::jedec`] tells a sleeping
//! chip ([`w25q::Verdict::Silent`]) from an awake one.

use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pin, Pull};
use embassy_nrf::qspi;
use embassy_nrf::Peri;
use embassy_time::{block_for, Duration};
use oxinode_core::w25q::{self, Identity};

/// The chip on six GPIOs, in SPI mode 0, most significant bit first.
pub struct Bitbang<'d> {
    sck: Output<'d>,
    cs: Output<'d>,
    /// DI in single-line SPI.
    io0: Output<'d>,
    /// DO in single-line SPI.
    io1: Input<'d>,
    /// /WP: high, so status writes are not blocked.
    _io2: Output<'d>,
    /// /HOLD: low would pause the chip mid-command.
    _io3: Output<'d>,
}

impl<'d> Bitbang<'d> {
    /// The pins in the order of the board page: SCK, /CS, IO0 to IO3.
    pub fn new(
        sck: Peri<'d, impl Pin>,
        cs: Peri<'d, impl Pin>,
        io0: Peri<'d, impl Pin>,
        io1: Peri<'d, impl Pin>,
        io2: Peri<'d, impl Pin>,
        io3: Peri<'d, impl Pin>,
    ) -> Self {
        Self {
            cs: Output::new(cs, Level::High, OutputDrive::Standard),
            sck: Output::new(sck, Level::Low, OutputDrive::Standard),
            io0: Output::new(io0, Level::Low, OutputDrive::Standard),
            io1: Input::new(io1, Pull::Down),
            _io2: Output::new(io2, Level::High, OutputDrive::Standard),
            _io3: Output::new(io3, Level::High, OutputDrive::Standard),
        }
    }

    /// One byte out and one in. The chip samples on the rising edge and
    /// changes its output on the falling one, so DO is read with the clock
    /// low, just before it rises.
    fn exchange(&mut self, out: u8) -> u8 {
        let mut into = 0;
        for bit in (0..8).rev() {
            self.io0.set_level(Level::from(out & (1 << bit) != 0));
            into = (into << 1) | u8::from(self.io1.is_high());
            self.sck.set_high();
            self.sck.set_low();
        }
        into
    }

    /// One command: /CS down, `out`, then `into.len()` bytes back, /CS up.
    fn command(&mut self, out: &[u8], into: &mut [u8]) {
        self.cs.set_low();
        for &b in out {
            self.exchange(b);
        }
        for b in into.iter_mut() {
            *b = self.exchange(0);
        }
        self.cs.set_high();
    }

    /// A command that answers with bytes: a status register, say.
    pub fn read(&mut self, opcode: u8, into: &mut [u8]) {
        self.command(&[opcode], into);
    }

    /// Release from deep power-down. Harmless to a chip that is awake.
    pub fn wake(&mut self) {
        self.command(&[w25q::RELEASE_POWER_DOWN], &mut []);
        block_for(Duration::from_micros(u64::from(w25q::RELEASE_US)));
    }

    /// Into deep power-down, where it stays until [`Self::wake`].
    pub fn sleep(&mut self) {
        self.command(&[w25q::POWER_DOWN], &mut []);
        block_for(Duration::from_micros(u64::from(w25q::POWER_DOWN_US)));
    }

    /// The JEDEC ID; [`w25q::Verdict::Silent`] from a sleeping chip.
    pub fn jedec(&mut self) -> Identity {
        let mut id = [0u8; 3];
        self.command(&[w25q::JEDEC_ID], &mut id);
        Identity::from_bytes(id)
    }
}

/// Wake the chip, check it is there, and put it to sleep, keeping its pins
/// driven for as long as the returned value lives. What the product image
/// does at boot; says what it found.
pub fn park<'d>(mut bus: Bitbang<'d>) -> (Bitbang<'d>, Identity, Identity) {
    bus.wake();
    let awake = bus.jedec();
    bus.sleep();
    let asleep = bus.jedec();
    (bus, awake, asleep)
}

/// The peripheral's configuration for this chip: 16 MB, 24-bit addresses,
/// single-line reads and writes unless `quad`, at 8 MHz.
///
/// Single-line by default because it needs nothing of the chip: the
/// four-line opcodes only work with its QE bit set, which the bring-up
/// reports rather than assumes.
pub fn config(quad: bool) -> qspi::Config {
    let mut config = qspi::Config::default();
    config.capacity = w25q::CAPACITY;
    config.frequency = qspi::Frequency::M8;
    if quad {
        config.read_opcode = qspi::ReadOpcode::Read4io;
        config.write_opcode = qspi::WriteOpcode::Pp4io;
    } else {
        config.read_opcode = qspi::ReadOpcode::Fastread;
        config.write_opcode = qspi::WriteOpcode::Pp;
    }
    config
}
