//! The universal byte-exact reversible codec — the zstd "floor" that every
//! content type can fall back to. `zstd::encode_all` / `decode_all` are
//! lossless, so `decompress(compress(x)) == x` holds for every input. This is
//! the codec used for all content types in Chunk 1; Chunk 2 introduces
//! per-type codecs that still satisfy the same reversibility contract.

use crate::ccr::codec::Codec;

/// Lossless zstd codec. `level` controls compression effort (1..=22); it does
/// not affect decompression, which is self-describing in the zstd frame.
pub struct ZstdCodec {
    pub level: i32,
}

impl Codec for ZstdCodec {
    fn id(&self) -> &'static str {
        "zstd"
    }

    fn compress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(zstd::encode_all(input, self.level)?)
    }

    fn decompress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(zstd::decode_all(input)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes (xorshift64) so the 1 MB round-trip is
    /// reproducible and needs no `rand` dependency. This is the incompressible
    /// worst case, which is exactly what we want to stress for reversibility.
    fn pseudo_random(len: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push((state & 0xFF) as u8);
        }
        out
    }

    fn assert_round_trips(codec: &ZstdCodec, input: &[u8]) {
        let compressed = codec.compress(input).expect("compress");
        let restored = codec.decompress(&compressed).expect("decompress");
        assert_eq!(restored, input, "round-trip must be byte-for-byte exact");
    }

    #[test]
    fn round_trips_empty() {
        assert_round_trips(&ZstdCodec { level: 19 }, b"");
    }

    #[test]
    fn round_trips_ascii() {
        assert_round_trips(
            &ZstdCodec { level: 19 },
            b"the quick brown fox jumps over the lazy dog",
        );
    }

    #[test]
    fn round_trips_multibyte() {
        let s = "héllo wörld ✓ — 日本語 — 🦀 multibyte";
        assert_round_trips(&ZstdCodec { level: 19 }, s.as_bytes());
    }

    #[test]
    fn round_trips_one_megabyte_random() {
        let data = pseudo_random(1024 * 1024);
        assert_round_trips(&ZstdCodec { level: 19 }, &data);
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(ZstdCodec { level: 1 }.id(), "zstd");
        assert_eq!(ZstdCodec { level: 19 }.id(), "zstd");
    }
}
