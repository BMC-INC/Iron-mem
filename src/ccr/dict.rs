//! Dictionary-assisted zstd codec — byte-exact reversible, and beats the plain
//! zstd floor on small/medium blobs that share structure with a trained
//! dictionary. The dictionary is content-addressed and recorded on each blob
//! (`blobs.dict_hash`), so the exact dictionary is always available at decode
//! time. Dictionaries may therefore be retrained freely without ever breaking
//! an already-stored blob's round-trip.
//!
//! Per-content-type *transforms* (log timestamp-delta templating, diff-token
//! preprocessing, AST normalization for code) are intentionally **out of scope**
//! for now: they run on top of zstd — which already factors out recurring
//! prefixes — for marginal additional gain, while any imperfection in the
//! inverse silently breaks the byte-exact contract. Per-type dictionaries
//! capture the same recurring structure safely, so they are the per-type codec.
//! The bespoke transforms are documented future depth, not a stub.

use crate::ccr::codec::Codec;
use crate::db::{self, Database};
use std::io::Read;

/// zstd codec bound to a specific dictionary.
pub struct DictZstdCodec {
    dict: Vec<u8>,
    level: i32,
}

impl DictZstdCodec {
    pub fn new(dict: Vec<u8>, level: i32) -> Self {
        Self { dict, level }
    }
}

impl Codec for DictZstdCodec {
    fn id(&self) -> &'static str {
        "dict+zstd"
    }

    fn compress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut c = zstd::bulk::Compressor::with_dictionary(self.level, &self.dict)?;
        Ok(c.compress(input)?)
    }

    fn decompress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        // Streaming decode so we don't need to know the original size up front.
        let mut dec = zstd::stream::read::Decoder::with_dictionary(input, &self.dict)?;
        let mut out = Vec::new();
        dec.read_to_end(&mut out)?;
        Ok(out)
    }
}

/// Minimum number of samples before attempting to train a dictionary. zstd's
/// trainer needs a reasonable corpus; below this we stay on the floor codec.
pub const MIN_DICT_SAMPLES: usize = 32;

/// Default trained-dictionary size cap (bytes).
pub const DEFAULT_DICT_SIZE: usize = 16 * 1024;

/// Train a zstd dictionary from sample byte slices. Returns `None` (rather than
/// erroring) when there isn't enough material to train a useful dictionary — the
/// caller then falls back to the plain zstd floor, which is always reversible.
pub fn train_dict<S: AsRef<[u8]>>(samples: &[S], max_size: usize) -> Option<Vec<u8>> {
    if samples.len() < MIN_DICT_SAMPLES {
        return None;
    }
    match zstd::dict::from_samples(samples, max_size) {
        Ok(d) if !d.is_empty() => Some(d),
        _ => None,
    }
}

/// Sample cap when training a dictionary.
pub const TRAIN_SAMPLE_LIMIT: i64 = 256;

/// Resolve the dictionary to use for `content_type`: the latest trained one if
/// it exists, otherwise lazily train (and persist, content-addressed) a new one
/// from recently-stored blobs of this type once `MIN_DICT_SAMPLES` exist.
/// Returns `(dict_hash, dict_bytes)`, or `None` when there isn't yet enough
/// material — the caller then uses the plain zstd floor.
///
/// Because dictionaries are content-addressed and every blob records its
/// `dict_hash`, a newly-trained dictionary never invalidates older blobs.
pub async fn select_dict(
    db: &Database,
    content_type: &str,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    if let Some(hash) = db::latest_dict_hash(db, content_type).await? {
        if let Some(bytes) = db::get_dict(db, &hash).await? {
            return Ok(Some((hash, bytes)));
        }
    }

    // No dictionary yet — gather decompressed samples and try to train one.
    let hashes = db::recent_blob_hashes_by_type(db, content_type, TRAIN_SAMPLE_LIMIT).await?;
    if hashes.len() < MIN_DICT_SAMPLES {
        return Ok(None);
    }
    let mut samples: Vec<Vec<u8>> = Vec::with_capacity(hashes.len());
    for h in &hashes {
        if let Ok(bytes) = crate::ccr::load_blob(db, h).await {
            samples.push(bytes);
        }
    }
    match train_dict(&samples, DEFAULT_DICT_SIZE) {
        Some(dict) => {
            let hash = crate::ccr::sha256_hex(&dict);
            db::insert_dict(db, &hash, content_type, &dict).await?;
            Ok(Some((hash, dict)))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccr::zstd_codec::ZstdCodec;

    /// A corpus of structurally-similar JSON-ish records (what a tool-output
    /// stream of the same shape looks like) — ideal material for a dictionary.
    fn corpus() -> Vec<Vec<u8>> {
        (0..2000)
            .map(|i| {
                format!(
                    "{{\"event\":\"build\",\"id\":{i},\"status\":\"ok\",\"module\":\"crate::feature_{}\",\"detail\":\"compiled successfully in {} ms\"}}",
                    i % 37,
                    (i * 7) % 900
                )
                .into_bytes()
            })
            .collect()
    }

    #[test]
    fn dict_codec_round_trips_byte_exact() {
        let samples = corpus();
        let dict = train_dict(&samples, DEFAULT_DICT_SIZE).expect("dictionary trains from corpus");
        let codec = DictZstdCodec::new(dict, 19);

        for s in samples.iter().take(50) {
            let c = codec.compress(s).unwrap();
            assert_eq!(
                codec.decompress(&c).unwrap(),
                *s,
                "dict round-trip byte-exact"
            );
        }
        // Edge inputs the dict was not trained on must still round-trip exactly.
        for s in [&b""[..], "héllo ✓ 日本語 🦀 multibyte".as_bytes()] {
            let c = codec.compress(s).unwrap();
            assert_eq!(codec.decompress(&c).unwrap(), s);
        }
    }

    #[test]
    fn dict_beats_floor_on_small_similar_records() {
        let samples = corpus();
        let dict = train_dict(&samples, DEFAULT_DICT_SIZE).expect("dict");
        let codec = DictZstdCodec::new(dict, 19);
        let floor = ZstdCodec { level: 19 };

        let one = &samples[123];
        let dict_len = codec.compress(one).unwrap().len();
        let floor_len = floor.compress(one).unwrap().len();
        assert!(
            dict_len < floor_len,
            "dict ({dict_len} B) should beat the floor ({floor_len} B) on a small structured record"
        );
    }

    #[test]
    fn too_few_samples_skips_training() {
        let few: Vec<Vec<u8>> = (0..4).map(|i| format!("row {i}").into_bytes()).collect();
        assert!(train_dict(&few, DEFAULT_DICT_SIZE).is_none());
    }
}
