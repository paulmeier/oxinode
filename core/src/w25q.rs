//! The W25Q128 on the Base Duo: what its answers mean, and the pattern the
//! bring-up image leaves on it.
//!
//! The board carries 16 MB of QSPI NOR flash, a Winbond W25Q128JVPIQ, on
//! P0.03 (CLK), P0.26 (/CS) and P0.30, P0.29, P0.28, P0.02 (IO0 to IO3). The
//! firmware drives it, and the `qspi` image proves it; this is the part that
//! needs no chip. It reads the JEDEC ID and the status registers and says
//! what they mean, and it builds and checks the test pattern, so the image
//! can say after a reset whether what it wrote is still there.
//!
//! # Deep power-down
//!
//! The chip draws microamps idle and about a microamp in deep power-down
//! (`B9h`), and in deep power-down it answers nothing but `ABh`, which wakes
//! it. A reset of the nRF52 does not reset the flash, so a chip a previous
//! boot put to sleep is still asleep when the next one starts. Anything
//! that talks to it therefore wakes it first. The JEDEC ID shows which state
//! it is in: asleep, the chip leaves its output undriven and the ID reads as
//! [`Verdict::Silent`].
//!
//! # The test pattern
//!
//! One 4 KB sector, the chip's last ([`TEST_SECTOR`]), well away from where
//! anything that keeps data will start. It holds a [`MAGIC`] header and a
//! seed, then bytes from a generator started on that seed. After a reset,
//! [`check`] reads the seed back from the header, regenerates the bytes, and
//! counts the ones that differ. A sector that is all `0xFF` is erased, not
//! broken, and one with some other header was not written by this.

/// Read the manufacturer, memory type and capacity: three bytes.
pub const JEDEC_ID: u8 = 0x9F;
/// Read status register 1: BUSY in bit 0, write enable latched in bit 1.
pub const READ_STATUS_1: u8 = 0x05;
/// Read status register 2: QE, the quad enable, in bit 1.
pub const READ_STATUS_2: u8 = 0x35;
/// Clear the write enable latch.
pub const WRITE_DISABLE: u8 = 0x04;
/// Enter deep power-down.
pub const POWER_DOWN: u8 = 0xB9;
/// Leave deep power-down. Also the only command a sleeping chip hears.
pub const RELEASE_POWER_DOWN: u8 = 0xAB;

/// From `B9h` to deep power-down, at most (tDP).
pub const POWER_DOWN_US: u32 = 3;
/// From `ABh` to taking commands again, at most (tRES1).
pub const RELEASE_US: u32 = 3;

/// Winbond's JEDEC manufacturer ID.
pub const WINBOND: u8 = 0xEF;
/// What the Rev 01 schematic populates: W25Q128JV, 16 MB.
pub const CAPACITY: u32 = 16 * 1024 * 1024;
/// The smallest unit of erase.
pub const SECTOR: u32 = 4096;
/// The sector the bring-up image writes: the chip's last.
pub const TEST_SECTOR: u32 = CAPACITY - SECTOR;

/// What the JEDEC ID read gave back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub manufacturer: u8,
    pub memory_type: u8,
    /// The capacity as a power of two: `0x18` is 2²⁴ bytes.
    pub capacity: u8,
}

/// What an [`Identity`] says about the chip on the other end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A Winbond W25Q128JV: 16 MB, the part the schematic calls for.
    W25q128,
    /// Something answered, and it is not that.
    Other,
    /// All zeros or all ones: nothing drove the line. A chip in deep
    /// power-down, one that is not powered, or a wrong pin.
    Silent,
}

impl Identity {
    pub const fn from_bytes(id: [u8; 3]) -> Self {
        Self {
            manufacturer: id[0],
            memory_type: id[1],
            capacity: id[2],
        }
    }

    /// Bytes, if the capacity code is one a JEDEC part uses.
    pub fn capacity_bytes(&self) -> Option<u32> {
        (0x10..=0x1F)
            .contains(&self.capacity)
            .then(|| 1u32 << self.capacity)
    }

    pub fn verdict(&self) -> Verdict {
        let bytes = [self.manufacturer, self.memory_type, self.capacity];
        if bytes == [0x00; 3] || bytes == [0xFF; 3] {
            return Verdict::Silent;
        }
        // 0x40 in the default SPI mode; 0x70 for the "M" parts, which ship
        // with QPI and DTR enabled. Both are the same array.
        let w25q_jv = matches!(self.memory_type, 0x40 | 0x70);
        if self.manufacturer == WINBOND && w25q_jv && self.capacity_bytes() == Some(CAPACITY) {
            Verdict::W25q128
        } else {
            Verdict::Other
        }
    }
}

/// Status registers 1 and 2, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub sr1: u8,
    pub sr2: u8,
}

impl Status {
    /// An erase or a program is still running.
    pub const fn busy(&self) -> bool {
        self.sr1 & 0x01 != 0
    }

    /// Write enable is latched: a write or erase would be accepted.
    pub const fn write_enabled(&self) -> bool {
        self.sr1 & 0x02 != 0
    }

    /// Quad enable. Without it IO2 and IO3 are /WP and /HOLD, and the four
    /// line reads and writes do not work.
    pub const fn quad_enabled(&self) -> bool {
        self.sr2 & 0x02 != 0
    }
}

/// The first eight bytes of a sector this wrote.
pub const MAGIC: [u8; 8] = *b"oxinode1";
/// The magic and the seed, little-endian, in front of the generated bytes.
pub const HEADER: usize = MAGIC.len() + 4;

/// One step of xorshift32. Zero would stay zero, so a seed of zero is moved.
const fn step(x: u32) -> u32 {
    let x = if x == 0 { 0x6F78_696E } else { x };
    let x = x ^ (x << 13);
    let x = x ^ (x >> 17);
    x ^ (x << 5)
}

/// Fill `sector` with the pattern for `seed`: the header, then generated
/// bytes. Any length; the bring-up image uses one [`SECTOR`].
pub fn fill(sector: &mut [u8], seed: u32) {
    let mut header = [0u8; HEADER];
    header[..MAGIC.len()].copy_from_slice(&MAGIC);
    header[MAGIC.len()..].copy_from_slice(&seed.to_le_bytes());
    let n = header.len().min(sector.len());
    sector[..n].copy_from_slice(&header[..n]);
    let mut x = seed;
    for b in sector.iter_mut().skip(HEADER) {
        x = step(x);
        *b = x as u8;
    }
}

/// What was found in a sector read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// Every byte `0xFF`.
    Erased,
    /// This module's header, with that seed, and the generated bytes
    /// compared one by one.
    Pattern {
        seed: u32,
        /// Bytes after the header that differ from the pattern.
        wrong: usize,
        /// The offset in the sector of the first of them.
        first_wrong: Option<usize>,
    },
    /// Something that is neither.
    Foreign,
}

/// Look at a sector read back.
pub fn check(sector: &[u8]) -> Found {
    if sector.iter().all(|&b| b == 0xFF) {
        return Found::Erased;
    }
    if sector.len() < HEADER || sector[..MAGIC.len()] != MAGIC {
        return Found::Foreign;
    }
    let mut seed = [0u8; 4];
    seed.copy_from_slice(&sector[MAGIC.len()..HEADER]);
    let seed = u32::from_le_bytes(seed);
    let mut x = seed;
    let mut wrong = 0;
    let mut first_wrong = None;
    for (i, &b) in sector.iter().enumerate().skip(HEADER) {
        x = step(x);
        if b != x as u8 {
            wrong += 1;
            first_wrong.get_or_insert(i);
        }
    }
    Found::Pattern {
        seed,
        wrong,
        first_wrong,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_part_on_the_schematic_is_recognised() {
        let id = Identity::from_bytes([0xEF, 0x40, 0x18]);
        assert_eq!(id.capacity_bytes(), Some(CAPACITY));
        assert_eq!(id.verdict(), Verdict::W25q128);
        assert_eq!(
            Identity::from_bytes([0xEF, 0x70, 0x18]).verdict(),
            Verdict::W25q128
        );
    }

    #[test]
    fn the_part_the_variant_declares_is_not_it() {
        // W25Q32JV, 4 MB: what the board's Meshtastic variant claims.
        assert_eq!(
            Identity::from_bytes([0xEF, 0x40, 0x16]).verdict(),
            Verdict::Other
        );
        // Another maker's 16 MB part.
        assert_eq!(
            Identity::from_bytes([0xC2, 0x20, 0x18]).verdict(),
            Verdict::Other
        );
    }

    #[test]
    fn an_undriven_line_is_silent_not_another_part() {
        assert_eq!(Identity::from_bytes([0x00; 3]).verdict(), Verdict::Silent);
        assert_eq!(Identity::from_bytes([0xFF; 3]).verdict(), Verdict::Silent);
        assert_eq!(Identity::from_bytes([0xFF; 3]).capacity_bytes(), None);
    }

    #[test]
    fn the_status_bits() {
        let s = Status {
            sr1: 0x03,
            sr2: 0x02,
        };
        assert!(s.busy() && s.write_enabled() && s.quad_enabled());
        let s = Status {
            sr1: 0x00,
            sr2: 0xFD,
        };
        assert!(!s.busy() && !s.write_enabled() && !s.quad_enabled());
    }

    #[test]
    fn the_test_sector_is_the_last_and_aligned() {
        assert_eq!(TEST_SECTOR % SECTOR, 0);
        assert_eq!(TEST_SECTOR + SECTOR, CAPACITY);
    }

    fn sector(seed: u32) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR as usize];
        fill(&mut s, seed);
        s
    }

    #[test]
    fn a_pattern_reads_back_whole() {
        for seed in [0, 1, 0xDEAD_BEEF, u32::MAX] {
            let s = sector(seed);
            assert_eq!(&s[..MAGIC.len()], &MAGIC);
            assert_eq!(
                check(&s),
                Found::Pattern {
                    seed,
                    wrong: 0,
                    first_wrong: None
                }
            );
        }
    }

    #[test]
    fn a_pattern_is_not_constant_and_depends_on_the_seed() {
        let a = sector(1);
        let b = sector(2);
        assert_ne!(a[HEADER..], b[HEADER..]);
        // Every byte value turns up: a stuck data line would show.
        let mut seen = [false; 256];
        for &x in &a[HEADER..] {
            seen[x as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "a sector covers every byte value");
        // And no byte is 0xFF all the way, so a pattern is never read as erased.
        assert!(a[HEADER..].iter().any(|&x| x != 0xFF));
    }

    #[test]
    fn damage_is_counted_and_located() {
        let mut s = sector(7);
        s[100] ^= 0x01;
        s[4000] = !s[4000];
        assert_eq!(
            check(&s),
            Found::Pattern {
                seed: 7,
                wrong: 2,
                first_wrong: Some(100)
            }
        );
    }

    #[test]
    fn a_bit_stuck_on_one_data_line_ruins_about_half_of_it() {
        // IO1 stuck high: bit 1 of every byte reads one. The magic has
        // bytes with that bit clear, so the whole sector is foreign...
        let stuck = |b: &u8| b | 0x02;
        let s: Vec<u8> = sector(9).iter().map(stuck).collect();
        assert_eq!(check(&s), Found::Foreign);
        // ...and past the header, about half the bytes are wrong.
        let mut s = sector(9);
        for b in &mut s[HEADER..] {
            *b = stuck(b);
        }
        let Found::Pattern { wrong, .. } = check(&s) else {
            panic!("the header was left alone");
        };
        let body = SECTOR as usize - HEADER;
        assert!(
            wrong > body / 3 && wrong < 2 * body / 3,
            "{wrong} of {body}"
        );
    }

    #[test]
    fn erased_and_foreign_are_not_damage() {
        assert_eq!(check(&[0xFF; SECTOR as usize]), Found::Erased);
        assert_eq!(check(&[0x00; SECTOR as usize]), Found::Foreign);
        let mut s = sector(3);
        s[0] = b'X';
        assert_eq!(check(&s), Found::Foreign);
    }
}
