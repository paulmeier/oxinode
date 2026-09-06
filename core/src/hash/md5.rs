//! MD5, RFC 1321.
//!
//! Here for exactly one reason: an RNode's EEPROM carries a sixteen-byte
//! checksum over its identity block, and `rnodeconf` computes that checksum
//! with MD5 and rejects the device when it does not match. The algorithm is
//! therefore a fact about the protocol rather than a choice, and it is used as
//! a checksum — the trust in a provisioned device comes from the RSA signature
//! *over* these sixteen bytes, which is made and checked on the host.
//!
//! One-shot: every input this project hashes is eleven bytes long.

/// The per-round constants, `floor(2^32 * abs(sin(i + 1)))`.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Per-round left-rotation amounts, four distinct values per sixteen-step round.
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// The MD5 digest of `data`.
pub fn digest(data: &[u8]) -> [u8; 16] {
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

    let mut block = [0u8; 64];
    let mut chunks = data.chunks_exact(64);
    for chunk in chunks.by_ref() {
        block.copy_from_slice(chunk);
        compress(&mut state, &block);
    }

    // Padding: a 0x80 byte, zeroes, then the *bit* length little-endian. It
    // needs one more block whenever the tail plus those nine bytes does not
    // fit in the sixty-four.
    let tail = chunks.remainder();
    block = [0; 64];
    block[..tail.len()].copy_from_slice(tail);
    block[tail.len()] = 0x80;
    let bits = (data.len() as u64).wrapping_mul(8);
    if tail.len() + 9 > 64 {
        compress(&mut state, &block);
        block = [0; 64];
    }
    block[56..].copy_from_slice(&bits.to_le_bytes());
    compress(&mut state, &block);

    let mut out = [0u8; 16];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
    let mut m = [0u32; 16];
    for (i, word) in m.iter_mut().enumerate() {
        *word = u32::from_le_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }

    let [mut a, mut b, mut c, mut d] = *state;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let tmp = d;
        d = c;
        c = b;
        b = b.wrapping_add(
            a.wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(m[g])
                .rotate_left(S[i]),
        );
        a = tmp;
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The test suite from RFC 1321 appendix A.5, verbatim. An implementation
    /// that passes these is the algorithm the host is using; one that passes
    /// only a round trip against itself proves nothing at all.
    #[test]
    fn the_rfc_1321_test_suite() {
        for (input, expected) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(hex(&digest(input.as_bytes())), expected, "{input:?}");
        }
    }

    /// The two lengths where the padding decides whether a second block is
    /// needed. 55 bytes is the largest that still fits with its nine bytes of
    /// padding; 56 is the first that does not, and an implementation that gets
    /// the boundary wrong passes every short vector above and then fails on
    /// real input.
    #[test]
    fn the_padding_boundary_is_at_fifty_five_bytes() {
        assert_eq!(
            hex(&digest(&[b'a'; 55])),
            "ef1772b6dff9a122358552954ad0df65"
        );
        assert_eq!(
            hex(&digest(&[b'a'; 56])),
            "3b0c8ac703f828b04c6c197006d17218"
        );
        assert_eq!(
            hex(&digest(&[b'a'; 64])),
            "014842d480b571495a4a0363793f7367"
        );
        assert_eq!(
            hex(&digest(&[b'a'; 65])),
            "c743a45e0d2e6a95cb859adae0248435"
        );
    }

    /// The actual shape the protocol hashes: eleven bytes of identity block.
    /// Product, model, hardware revision, four bytes of serial and four of
    /// manufacture time.
    #[test]
    fn an_eleven_byte_identity_block_hashes_to_sixteen_bytes() {
        let info = [
            0xF0, 0xFF, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68, 0xBB, 0xCC, 0xDD,
        ];
        let sum = digest(&info);
        assert_eq!(sum.len(), 16);
        // Pinned so a change to the implementation has to be deliberate.
        assert_eq!(hex(&sum), "42df0be1d6b0229906fde590b6eec374");
    }
}
