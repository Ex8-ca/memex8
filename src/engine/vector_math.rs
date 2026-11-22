//! Vector similarity, in one place.
//!
//! Realm assignment, association building, k-means clustering and the slumber
//! realm split all compare embeddings with [`cosine_similarity`], so they all
//! agree on its edge cases.

/// Cosine similarity of two vectors, in `[-1, 1]`.
///
/// Returns `0.0` ("unrelated") when the vectors cannot be compared: either is
/// empty, either has zero length, or their dimensions differ. A dimension
/// mismatch means the embedding model changed under stored vectors; comparing
/// a prefix would produce a score that looks meaningful and is not, and a
/// `0.0` can only cause a new realm or a missed link, never a wrong match.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::cosine_similarity;

    #[test]
    fn identical_vectors_score_one() {
        let v = [0.1f32, 0.2, 0.3];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_and_perpendicular_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn scale_does_not_matter() {
        assert!((cosine_similarity(&[1.0, 2.0, 3.0], &[10.0, 20.0, 30.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_and_empty_vectors_are_unrelated_to_everything() {
        let z = [0.0f32; 4];
        let v = [1.0f32, 0.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&z, &v), 0.0);
        assert_eq!(cosine_similarity(&v, &z), 0.0);
        assert_eq!(cosine_similarity(&z, &z), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[], &v), 0.0);
    }

    #[test]
    fn mismatched_dimensions_are_unrelated_not_prefix_compared() {
        // The prefixes are identical; a prefix comparison would score 1.0, and
        // dot-over-prefix with full-length norms would score about 0.71.
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0, 1.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn high_dimensional_unrelated_vectors_are_nearly_orthogonal() {
        let a: Vec<f32> = (0..1536).map(|i| ((i * 7) as f32).sin()).collect();
        let b: Vec<f32> = (0..1536).map(|i| ((i * 13) as f32).cos()).collect();
        assert!(cosine_similarity(&a, &b).abs() < 0.3);
    }

    #[test]
    fn small_noise_keeps_vectors_close() {
        let v: Vec<f32> = (0..128).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut noisy = v.clone();
        noisy[10] += 0.01;
        noisy[50] -= 0.01;
        assert!(cosine_similarity(&v, &noisy) > 0.99);
    }
}
