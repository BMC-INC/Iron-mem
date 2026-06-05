//! Encode/decode embeddings to bytes, plus similarity helpers.
//!
//! Embeddings are stored as f32 little-endian byte blobs. We store
//! unit-normalized vectors so cosine similarity reduces to a dot product.

/// f32 little-endian byte encoding (length = dim * 4).
pub fn encode(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode an f32 little-endian byte blob back into a vector.
pub fn decode(bytes: &[u8]) -> Vec<f32> {
    debug_assert_eq!(
        bytes.len() % 4,
        0,
        "embedding blob length must be a multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// L2-normalize; zero vectors are returned unchanged.
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let mag = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / mag).collect()
}

/// Dot product. For unit-normalized inputs this equals cosine similarity.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![0.0_f32, 1.5, -2.25, 3.0];
        let bytes = encode(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        assert_eq!(decode(&bytes), v);
    }

    #[test]
    fn normalize_makes_unit_length() {
        let v = normalize(&[3.0, 4.0]);
        let mag = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((mag - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_zero_vector_is_unchanged() {
        assert_eq!(normalize(&[0.0, 0.0, 0.0]), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn dot_of_normalized_in_range() {
        let a = normalize(&[1.0, 2.0, 3.0]);
        let b = normalize(&[2.0, 1.0, 0.5]);
        let d = dot(&a, &b);
        assert!((-1.0001..=1.0001).contains(&d));
    }
}
