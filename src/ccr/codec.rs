//! Codec trait + registry for the CCR blob store.
//!
//! **Reversibility contract:** every registered codec satisfies
//! `decompress(compress(x)) == x` byte-for-byte. No lossy codec is ever
//! registered. The contract is enforced by per-codec round-trip property tests
//! and, at runtime, by `load_blob` re-hashing the decompressed bytes against
//! the content-address key.

// Registry items are consumed by `store_blob`/`load_blob` once CCR is wired in
// (Task 1.4+); allow until then.
#![allow(dead_code)]

use crate::ccr::detect::ContentType;
use crate::ccr::zstd_codec::ZstdCodec;

/// A reversible compression codec.
pub trait Codec: Send + Sync {
    /// Stable identifier persisted in the `blobs.codec` column. Used to
    /// reconstruct the correct decoder via [`codec_by_id`].
    fn id(&self) -> &'static str;
    fn compress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>>;
    fn decompress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>>;
}

/// The default zstd compression level for the floor codec.
const ZSTD_LEVEL: i32 = 19;

/// Select the codec to use when **storing** a blob of the given content type.
///
/// Chunk 1: every content type maps to the universal zstd floor codec. Chunk 2
/// swaps in per-type codecs (still byte-exact reversible) without changing this
/// signature.
pub fn codec_for(_ct: ContentType) -> Box<dyn Codec> {
    Box::new(ZstdCodec { level: ZSTD_LEVEL })
}

/// Reconstruct a codec from the `id()` persisted alongside a stored blob, so
/// `load_blob` can decode bytes written by any codec version.
pub fn codec_by_id(id: &str) -> anyhow::Result<Box<dyn Codec>> {
    match id {
        "zstd" => Ok(Box::new(ZstdCodec { level: ZSTD_LEVEL })),
        other => anyhow::bail!("unknown CCR codec id: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_for_every_type_has_stable_id() {
        for ct in [
            ContentType::Json,
            ContentType::Code,
            ContentType::Log,
            ContentType::Diff,
            ContentType::Text,
            ContentType::Binary,
        ] {
            assert_eq!(codec_for(ct).id(), "zstd");
        }
    }

    #[test]
    fn codec_by_id_round_trips_via_registry() {
        let stored = codec_for(ContentType::Json);
        let restored = codec_by_id(stored.id()).expect("known id resolves");
        let blob = stored.compress(b"{\"k\":\"v\"}").unwrap();
        assert_eq!(restored.decompress(&blob).unwrap(), b"{\"k\":\"v\"}");
    }

    #[test]
    fn codec_by_id_rejects_unknown() {
        assert!(codec_by_id("brotli").is_err());
    }
}
