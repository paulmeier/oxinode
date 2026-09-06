//! How Meshtastic picks a frequency, so oxinode can listen to one.
//!
//! **This is not a feature.** oxinode is an RNode, not a Meshtastic node, and
//! nothing here will survive into the shipping protocol. It exists because the
//! only radio peer on the bench is a second Base Duo running stock Meshtastic,
//! and to hear it you have to know where it is transmitting — which Meshtastic
//! does not store as a number. It *derives* the channel from a hash of the
//! channel name, so the frequency is a computed property of the configuration
//! rather than part of it, and asking the device does not get you it.
//!
//! Deriving it here rather than hard-coding 906.875 MHz means the arithmetic is
//! tested, and means a different preset or region can be followed without
//! guessing.

/// Meshtastic's string hash: djb2.
///
/// `h = h * 33 + c`, seeded at 5381, wrapping at 32 bits. The wrap is
/// load-bearing — the intermediate exceeds `u32` on the fourth character of
/// "LongFast" — so this uses `wrapping_mul` and `wrapping_add` deliberately
/// rather than by omission.
pub const fn djb2(s: &[u8]) -> u32 {
    let mut hash: u32 = 5381;
    let mut i = 0;
    while i < s.len() {
        hash = hash.wrapping_mul(33).wrapping_add(s[i] as u32);
        i += 1;
    }
    hash
}

/// A Meshtastic region's usable band, in hertz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Bottom of the band.
    pub start_hz: u32,
    /// Top of the band.
    pub end_hz: u32,
}

/// `US`, region code 1: 902–928 MHz.
pub const US: Region = Region {
    start_hz: 902_000_000,
    end_hz: 928_000_000,
};

impl Region {
    /// How many channels of this bandwidth fit in the band.
    pub const fn channel_count(&self, bandwidth_hz: u32) -> u32 {
        (self.end_hz - self.start_hz) / bandwidth_hz
    }

    /// Centre frequency of a channel, in hertz.
    ///
    /// Channels start half a bandwidth above the band edge, so the first one
    /// sits entirely inside the band rather than half outside it.
    pub const fn channel_hz(&self, bandwidth_hz: u32, channel: u32) -> u32 {
        self.start_hz + bandwidth_hz / 2 + channel * bandwidth_hz
    }

    /// Which channel a named Meshtastic channel lands on.
    pub const fn channel_for_name(&self, name: &[u8], bandwidth_hz: u32) -> u32 {
        djb2(name) % self.channel_count(bandwidth_hz)
    }
}

/// The modem settings of the `LONG_FAST` preset, which is Meshtastic's default.
pub mod long_fast {
    /// Spreading factor.
    pub const SF: u8 = 11;
    /// Bandwidth, in hertz.
    pub const BANDWIDTH_HZ: u32 = 250_000;
    /// Coding-rate code: 4/5.
    pub const CODING_RATE: u8 = 0x01;
    /// The channel name the default primary channel hashes as — the preset's
    /// name, not the empty string the configuration actually stores.
    pub const CHANNEL_NAME: &[u8] = b"LongFast";
    /// Preamble length in symbols.
    pub const PREAMBLE: u16 = 16;
    /// Meshtastic's LoRa sync word.
    ///
    /// Believed to be `0x2B`; not verified against Meshtastic's source from
    /// here. If nothing is ever received, this is the first thing to try
    /// changing — `0x12` (private) and `0x34` (LoRaWAN) are the alternatives.
    pub const SYNC_WORD: u8 = 0x2B;
}

/// Where the second bench board is transmitting: US region, `LONG_FAST`
/// preset, default primary channel.
pub const US_LONG_FAST_HZ: u32 = US.channel_hz(
    long_fast::BANDWIDTH_HZ,
    US.channel_for_name(long_fast::CHANNEL_NAME, long_fast::BANDWIDTH_HZ),
);

const _: () = assert!(
    US_LONG_FAST_HZ == 906_875_000,
    "US LongFast is 906.875 MHz -- a number quoted all over the Meshtastic \
     community, so a mismatch means the channel arithmetic is wrong"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// 906.875 MHz is the number every Meshtastic user in the US knows. If the
    /// hash, the modulus or the half-bandwidth offset were wrong, it would come
    /// out as some other perfectly plausible channel — which is exactly the
    /// kind of error that looks like a broken receiver.
    #[test]
    fn us_long_fast_is_the_frequency_everybody_quotes() {
        assert_eq!(US_LONG_FAST_HZ, 906_875_000);
    }

    #[test]
    fn the_us_band_holds_a_hundred_and_four_channels_at_250_khz() {
        assert_eq!(US.channel_count(250_000), 104);
        assert_eq!(
            US.channel_for_name(long_fast::CHANNEL_NAME, long_fast::BANDWIDTH_HZ),
            19
        );
    }

    /// The first channel sits half a bandwidth inside the band, and the last
    /// one has to fit too.
    #[test]
    fn every_channel_lies_inside_the_band() {
        let bw = 250_000;
        for ch in 0..US.channel_count(bw) {
            let f = US.channel_hz(bw, ch);
            assert!(
                f - bw / 2 >= US.start_hz,
                "channel {ch} runs off the bottom"
            );
            assert!(f + bw / 2 <= US.end_hz, "channel {ch} runs off the top");
        }
    }

    /// djb2 overflows a `u32` partway through "LongFast". Wrapping is the
    /// specified behaviour; a debug build that panicked instead would take the
    /// frequency calculation with it.
    #[test]
    fn the_hash_wraps_rather_than_overflowing() {
        assert_eq!(djb2(b""), 5381);
        assert_eq!(djb2(b"a"), 5381u32.wrapping_mul(33).wrapping_add(97));
        // The value that produces channel 19.
        assert_eq!(djb2(b"LongFast") % 104, 19);
        // Long inputs must not panic in any profile.
        let long = [b'x'; 64];
        let _ = djb2(&long);
    }

    /// Different channel names land on different channels, or the hash is not
    /// doing anything.
    #[test]
    fn different_channel_names_spread_across_the_band() {
        let bw = 250_000;
        let names: [&[u8]; 4] = [b"LongFast", b"MediumSlow", b"ShortTurbo", b"VeryLongSlow"];
        let mut seen = [0u32; 4];
        for (i, n) in names.iter().enumerate() {
            seen[i] = US.channel_for_name(n, bw);
        }
        for i in 0..names.len() {
            for j in i + 1..names.len() {
                assert_ne!(seen[i], seen[j], "{i} and {j} collide");
            }
        }
    }
}
