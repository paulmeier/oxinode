//! LoRa modulation parameters, and how long a packet takes to send.
//!
//! Airtime is not a nicety. It is the timeout for waiting on `TxDone`, so
//! getting it wrong turns a working transmission into a spurious error or a
//! failed one into a hang. It is also the thing the RNode protocol layer
//! needs in order to answer `rnodeconf`, and the thing a duty-cycle limit is
//! computed from.
//!
//! The formula is Semtech's, implemented from the specification rather than
//! copied: symbol time, a preamble of `n + 4.25` symbols, and a payload symbol
//! count that depends on spreading factor, coding rate, header mode, CRC and
//! the low-data-rate optimisation. Everything here is integer arithmetic in
//! microseconds — a float would be more readable and would quietly disagree
//! with itself across builds.

/// Spreading factors the LR1121 supports.
pub const SF_MIN: u8 = 5;
/// See [`SF_MIN`].
pub const SF_MAX: u8 = 12;

/// Bandwidths, in hertz, with the code the chip uses for each.
pub const BANDWIDTHS: [(u8, u32); 4] = [
    (0x03, 62_500),
    (0x04, 125_000),
    (0x05, 250_000),
    (0x06, 500_000),
];

/// Coding-rate code for 4/5, the shortest.
pub const CR_4_5: u8 = 0x01;

/// How long one symbol lasts, in microseconds.
///
/// `2^SF / bandwidth`. At SF12 and 125 kHz this is 32.768 ms; at SF5 and
/// 500 kHz it is 64 µs. The range is why the airtime of a "LoRa packet" is not
/// a single number anyone can carry in their head.
pub const fn symbol_time_us(sf: u8, bandwidth_hz: u32) -> u32 {
    ((1u64 << sf) * 1_000_000 / bandwidth_hz as u64) as u32
}

/// Whether the low-data-rate optimisation is required.
///
/// Mandatory once a symbol lasts longer than 16 ms, where clock drift over a
/// single symbol becomes significant. It costs two bits of the payload symbol
/// denominator, which is why it cannot simply be left on.
pub const fn low_data_rate_optimize(sf: u8, bandwidth_hz: u32) -> bool {
    symbol_time_us(sf, bandwidth_hz) > 16_000
}

/// Number of symbols the payload occupies.
///
/// The `max(_, 0)` in the published formula is why this is written with a
/// signed intermediate: for a short payload at a high spreading factor the
/// numerator goes negative, and an unsigned subtraction would wrap into an
/// enormous packet rather than clamping to none.
pub const fn payload_symbols(
    sf: u8,
    coding_rate: u8,
    payload_len: u8,
    explicit_header: bool,
    crc: bool,
    ldro: bool,
) -> u32 {
    let sf = sf as i64;
    let numerator = 8 * payload_len as i64 - 4 * sf + 28 + if crc { 16 } else { 0 }
        - if explicit_header { 0 } else { 20 };
    let denominator = 4 * (sf - if ldro { 2 } else { 0 });
    if numerator <= 0 {
        return 8;
    }
    // Ceiling division, then one block per coding-rate step.
    let blocks = (numerator + denominator - 1) / denominator;
    8 + (blocks * (coding_rate as i64 + 4)) as u32
}

/// How long a packet occupies the air, in microseconds.
pub const fn airtime_us(
    sf: u8,
    bandwidth_hz: u32,
    coding_rate: u8,
    preamble_symbols: u16,
    payload_len: u8,
    explicit_header: bool,
    crc: bool,
) -> u32 {
    let tsym = symbol_time_us(sf, bandwidth_hz) as u64;
    let ldro = low_data_rate_optimize(sf, bandwidth_hz);
    // Preamble is n + 4.25 symbols; kept in quarters to stay integer.
    let preamble = (4 * preamble_symbols as u64 + 17) * tsym / 4;
    let payload =
        payload_symbols(sf, coding_rate, payload_len, explicit_header, crc, ldro) as u64 * tsym;
    (preamble + payload) as u32
}

/// The configuration the bring-up image transmits with.
///
/// SF7 at 125 kHz with the shortest coding rate: fast enough that a bench test
/// is not spent waiting, slow enough to be an ordinary LoRa packet rather than
/// an exotic one. Not a claim about what the modem should default to.
pub mod bench {
    /// Spreading factor.
    pub const SF: u8 = 7;
    /// Bandwidth, in hertz.
    pub const BANDWIDTH_HZ: u32 = 125_000;
    /// Coding rate code.
    pub const CODING_RATE: u8 = super::CR_4_5;
    /// Preamble length in symbols; 8 is the usual choice above SF6.
    pub const PREAMBLE: u16 = 8;
    /// Payload length in bytes.
    pub const PAYLOAD_LEN: u8 = 16;
    /// Sync word. `0x12` is the private-network value, which is what RNode
    /// uses; `0x34` would announce this as LoRaWAN, which it is not.
    pub const SYNC_WORD: u8 = 0x12;

    /// Airtime of the bench packet, in microseconds.
    pub const AIRTIME_US: u32 = super::airtime_us(
        SF,
        BANDWIDTH_HZ,
        CODING_RATE,
        PREAMBLE,
        PAYLOAD_LEN,
        true,
        true,
    );
}

// The timeout for waiting on TxDone is derived from the airtime, so the airtime
// must be a sane number before anything depends on it.
const _: () = assert!(
    bench::AIRTIME_US > 10_000 && bench::AIRTIME_US < 200_000,
    "the bench packet's airtime is implausible; a TxDone timeout built on it \
     would either fire early or hide a hang"
);
const _: () = assert!(
    !low_data_rate_optimize(bench::SF, bench::BANDWIDTH_HZ),
    "SF7 at 125 kHz does not need the low-data-rate optimisation; if it does, \
     the symbol-time calculation is wrong"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_time_is_two_to_the_sf_over_bandwidth() {
        // SF7 at 125 kHz: 128/125000 s = 1.024 ms.
        assert_eq!(symbol_time_us(7, 125_000), 1_024);
        // SF12 at 125 kHz: 4096/125000 s = 32.768 ms.
        assert_eq!(symbol_time_us(12, 125_000), 32_768);
        // SF5 at 500 kHz: 32/500000 s = 64 us.
        assert_eq!(symbol_time_us(5, 500_000), 64);
    }

    /// Worked by hand from the specification, so the implementation has
    /// something to disagree with. SF7, 125 kHz, CR 4/5, 8-symbol preamble,
    /// 16-byte payload, explicit header, CRC on:
    ///
    /// * Tsym = 1.024 ms
    /// * preamble = (8 + 4.25) x 1.024 = 12.544 ms
    /// * numerator = 8x16 - 4x7 + 28 + 16 = 144; denominator = 28
    /// * ceil(144/28) = 6 blocks, x5 = 30, +8 = 38 symbols
    /// * payload = 38 x 1.024 = 38.912 ms
    /// * total = 51.456 ms
    #[test]
    fn the_bench_packet_matches_the_hand_calculation() {
        assert_eq!(payload_symbols(7, CR_4_5, 16, true, true, false), 38);
        assert_eq!(airtime_us(7, 125_000, CR_4_5, 8, 16, true, true), 51_456);
        assert_eq!(bench::AIRTIME_US, 51_456);
    }

    /// The clamp that stops an unsigned subtraction wrapping. At SF12 a
    /// zero-length payload makes the numerator negative; the answer is the
    /// 8-symbol floor, not four billion symbols.
    #[test]
    fn a_payload_too_short_to_fill_a_block_clamps_rather_than_wrapping() {
        assert_eq!(payload_symbols(12, CR_4_5, 0, true, false, true), 8);
        assert!(airtime_us(12, 125_000, CR_4_5, 8, 0, true, false) < 1_000_000);
    }

    #[test]
    fn the_low_data_rate_optimisation_turns_on_where_it_should() {
        // Symbol time crosses 16 ms between SF10 and SF11 at 125 kHz.
        assert!(!low_data_rate_optimize(10, 125_000));
        assert!(low_data_rate_optimize(11, 125_000));
        assert!(low_data_rate_optimize(12, 125_000));
        // ...and not at all at 500 kHz, where symbols are four times shorter.
        assert!(!low_data_rate_optimize(12, 500_000));
    }

    #[test]
    fn airtime_rises_with_payload_and_with_spreading_factor() {
        let base = airtime_us(7, 125_000, CR_4_5, 8, 16, true, true);
        assert!(airtime_us(7, 125_000, CR_4_5, 8, 32, true, true) > base);
        assert!(airtime_us(8, 125_000, CR_4_5, 8, 16, true, true) > base);
        assert!(airtime_us(7, 125_000, CR_4_5, 16, 16, true, true) > base);
        // ...and falls with bandwidth.
        assert!(airtime_us(7, 250_000, CR_4_5, 8, 16, true, true) < base);
    }

    /// A longer coding rate sends more redundancy, so it must cost more.
    #[test]
    fn a_longer_coding_rate_costs_airtime() {
        let cr45 = airtime_us(7, 125_000, 0x01, 8, 16, true, true);
        let cr48 = airtime_us(7, 125_000, 0x04, 8, 16, true, true);
        assert!(cr48 > cr45, "{cr48} should exceed {cr45}");
    }

    /// The extremes of what the chip allows, to be sure nothing overflows or
    /// divides by zero on the way through.
    #[test]
    fn every_supported_configuration_produces_a_sane_number() {
        for sf in SF_MIN..=SF_MAX {
            for (_, bw) in BANDWIDTHS {
                for len in [0u8, 1, 16, 255] {
                    let t = airtime_us(sf, bw, CR_4_5, 8, len, true, true);
                    assert!(t > 0, "sf{sf} bw{bw} len{len}");
                    // The worst case, SF12 at 62.5 kHz with 255 bytes, is a
                    // few seconds -- long, but not hours.
                    assert!(t < 30_000_000, "sf{sf} bw{bw} len{len} -> {t} us");
                }
            }
        }
    }

    /// The bandwidth codes are the chip's; a wrong one would transmit at the
    /// wrong rate while every calculation here stayed self-consistent.
    #[test]
    fn the_bandwidth_codes_are_the_chips() {
        assert_eq!(BANDWIDTHS[1], (0x04, 125_000));
        assert_eq!(
            BANDWIDTHS
                .iter()
                .find(|(_, hz)| *hz == bench::BANDWIDTH_HZ)
                .map(|(code, _)| *code),
            Some(0x04)
        );
    }
}
