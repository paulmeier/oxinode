//! Just enough linker-script reading to recover the memory map from `memory.x`.
//!
//! `memory.x` is the single place the flash layout is written down, but the
//! layout is needed in two other places: `build.rs` has to bake the load address
//! into the firmware so it can point `VTOR` at its own vector table, and the
//! flashing tools need it to build a UF2. Re-typing `0x26000` in each of those
//! is how a board ends up linked at one address and flashed to another.

/// A `MEMORY { ... }` region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub origin: u32,
    pub length: u32,
}

impl Region {
    /// First address past the region.
    pub const fn end(&self) -> u32 {
        // Saturating rather than wrapping: a region that runs to the top of the
        // address space should report the top, not zero.
        self.origin.saturating_add(self.length)
    }

    /// Whether `[start, start + len)` fits entirely inside this region.
    pub const fn contains_range(&self, start: u32, len: u32) -> bool {
        start >= self.origin && start.saturating_add(len) <= self.end()
    }
}

/// Origin of the `FLASH` region: the address the firmware image is linked at.
pub fn parse_flash_origin(script: &str) -> Option<u32> {
    Some(parse_region(script, "FLASH")?.origin)
}

/// Find a named region in a linker script's `MEMORY` block.
///
/// Deliberately not a general linker-script parser. It looks for a line shaped
/// like `NAME : ORIGIN = <n>, LENGTH = <n>` and understands the number formats
/// GNU ld accepts in practice (hex, decimal, `K`/`M` suffixes). Anything more
/// exotic than that in `memory.x` should be reported as unparsable rather than
/// guessed at, hence the `Option`.
pub fn parse_region(script: &str, name: &str) -> Option<Region> {
    for line in script.lines() {
        // Comment bodies in memory.x are indented under a leading `*`, so a
        // trimmed line starting with the region name is a real declaration and
        // not prose that happens to mention FLASH.
        let line = line.trim();
        let rest = match line.strip_prefix(name) {
            Some(rest) => rest,
            None => continue,
        };
        // Guard against `FLASH_EXTRA` matching a request for `FLASH`.
        let rest = rest.trim_start();
        let rest = match rest.strip_prefix(':') {
            Some(rest) => rest,
            None => continue,
        };

        let origin = parse_field(rest, "ORIGIN")?;
        let length = parse_field(rest, "LENGTH")?;
        return Some(Region { origin, length });
    }
    None
}

/// Pull `KEY = <number>` out of a region declaration's tail.
fn parse_field(decl: &str, key: &str) -> Option<u32> {
    let (_, after) = decl.split_once(key)?;
    let value = after.trim_start().strip_prefix('=')?.trim_start();
    parse_number(value)
}

/// Parse a linker-script integer: `0x1234`, `4096`, `824K`, `1M`.
///
/// Stops at the first character that cannot continue the number, so trailing
/// `, LENGTH = ...` or a comment does not need to be stripped first.
fn parse_number(text: &str) -> Option<u32> {
    let (digits, radix) = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => (hex, 16),
        None => (text, 10),
    };

    let taken: &str = {
        let end = digits
            .find(|c: char| !c.is_digit(radix))
            .unwrap_or(digits.len());
        &digits[..end]
    };
    if taken.is_empty() {
        return None;
    }

    let value = u32::from_str_radix(taken, radix).ok()?;

    // A suffix only makes sense on a bare decimal count; `0x400K` is not
    // something ld accepts and should not be quietly reinterpreted.
    let suffix = digits[taken.len()..].chars().next();
    match (radix, suffix) {
        (10, Some('K')) | (10, Some('k')) => value.checked_mul(1024),
        (10, Some('M')) | (10, Some('m')) => value.checked_mul(1024 * 1024),
        _ => Some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real thing, so these tests fail if `memory.x` grows a shape the
    /// parser cannot read.
    const MEMORY_X: &str = include_str!("../../memory.x");

    #[test]
    fn reads_the_projects_own_memory_x() {
        let flash = parse_region(MEMORY_X, "FLASH").expect("FLASH region");
        let ram = parse_region(MEMORY_X, "RAM").expect("RAM region");

        // Pinned deliberately. These are not arbitrary numbers: 0x26000 is where
        // the S140 SoftDevice hands off, and 0xEA000 is the highest address the
        // board's bootloader will write -- 40K below the 0xF4000 where its own
        // code starts, because it reserves that much for application data. If
        // this test fails, either the board's bootloader changed or someone is
        // about to build an image it will refuse to flash.
        assert_eq!(flash.origin, 0x0002_6000);
        assert_eq!(flash.length, 784 * 1024);
        assert_eq!(flash.end(), 0x000E_A000);

        assert_eq!(ram.origin, 0x2000_0000);
        assert_eq!(ram.length, 256 * 1024);

        assert_eq!(parse_flash_origin(MEMORY_X), Some(0x0002_6000));
    }

    #[test]
    fn prose_mentioning_a_region_name_is_not_a_declaration() {
        // The layout diagram in memory.x is full of addresses and the word FLASH.
        // Trip on it and build.rs bakes in a wrong VTOR.
        let script = "\
             /* The FLASH region below used to be ORIGIN = 0x00000000, LENGTH = 1M\n\
              * before the bootloader existed. Do not go back to that.\n\
              */\n\
             MEMORY\n\
             {\n\
               FLASH : ORIGIN = 0x00026000, LENGTH = 824K\n\
             }\n";
        assert_eq!(parse_flash_origin(script), Some(0x0002_6000));
    }

    #[test]
    fn tolerates_whitespace_and_ordering() {
        let script = "MEMORY {\n  RAM:ORIGIN=0x20000000,LENGTH=256K\n\tFLASH   :   ORIGIN   =   0x26000 ,  LENGTH  =  824K\n}";
        assert_eq!(
            parse_region(script, "FLASH"),
            Some(Region {
                origin: 0x26000,
                length: 843_776
            })
        );
        assert_eq!(
            parse_region(script, "RAM"),
            Some(Region {
                origin: 0x2000_0000,
                length: 262_144
            })
        );
    }

    #[test]
    fn does_not_match_a_longer_region_name() {
        let script = "FLASH_BOOT : ORIGIN = 0x000F4000, LENGTH = 40K";
        assert_eq!(parse_region(script, "FLASH"), None);
    }

    #[test]
    fn missing_or_malformed_regions_are_none_not_zero() {
        assert_eq!(parse_flash_origin(""), None);
        assert_eq!(
            parse_flash_origin("RAM : ORIGIN = 0x20000000, LENGTH = 256K"),
            None
        );
        // Truncated: an origin with no length is not a usable region.
        assert_eq!(parse_region("FLASH : ORIGIN = 0x26000", "FLASH"), None);
        // Not a number at all.
        assert_eq!(
            parse_region("FLASH : ORIGIN = start, LENGTH = 1K", "FLASH"),
            None
        );
    }

    #[test]
    fn number_formats() {
        assert_eq!(parse_number("0x26000"), Some(0x26000));
        assert_eq!(parse_number("0X26000"), Some(0x26000));
        assert_eq!(parse_number("4096"), Some(4096));
        assert_eq!(parse_number("824K"), Some(843_776));
        assert_eq!(parse_number("1M"), Some(1024 * 1024));
        assert_eq!(parse_number("2k"), Some(2048));
        // Stops cleanly at the separator rather than swallowing it.
        assert_eq!(parse_number("0x26000, LENGTH = 824K"), Some(0x26000));
        assert_eq!(parse_number("824K, X"), Some(843_776));
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("K"), None);
        // Hex digits are not decimal digits; `0xABC` must not parse as 0.
        assert_eq!(parse_number("ABC"), None);
    }

    #[test]
    fn suffix_overflow_is_rejected_rather_than_wrapping() {
        assert_eq!(parse_number("4194304K"), None);
        assert_eq!(parse_number("4096M"), None);
    }

    #[test]
    fn region_geometry() {
        let r = Region {
            origin: 0x26000,
            length: 0xCE000,
        };
        assert_eq!(r.end(), 0xF4000);
        assert!(r.contains_range(0x26000, 0xCE000));
        assert!(r.contains_range(0x26000, 0));
        assert!(!r.contains_range(0x26000, 0xCE001));
        assert!(!r.contains_range(0x25FFF, 1));
        // An image that would wrap the address space is out of range, not inside.
        assert!(!r.contains_range(0xF3FFF, u32::MAX));
    }
}
