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

/// K-means over cosine similarity. Returns `(centroids, assignments)`;
/// `assignments[i]` indexes into `centroids`. `k` is capped at the number of
/// vectors, and empty input or `k == 0` returns empty results.
///
/// Deterministic: the first centroid is `vectors[0]`, and each later one is the
/// vector farthest from all centroids chosen so far, so the same input always
/// clusters the same way.
pub fn kmeans(vectors: &[Vec<f32>], k: usize, max_iter: usize) -> (Vec<Vec<f32>>, Vec<i32>) {
    if vectors.is_empty() || k == 0 {
        return (vec![], vec![]);
    }

    let n = vectors.len();
    let dims = vectors[0].len();
    let k = k.min(n);

    // Initialize centroids using k-means++ style seeding
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);

    // Seed with the first vector, then repeatedly with the vector farthest
    // (by cosine distance) from every centroid chosen so far. Deterministic.
    centroids.push(vectors[0].clone());

    // Pick remaining k-1 centroids with probability proportional to distance squared
    for _ in 1..k {
        let mut best_d2 = -1.0f32;
        let mut best_idx = 0;
        for (i, v) in vectors.iter().enumerate() {
            let min_d2: f32 = centroids
                .iter()
                .map(|c| {
                    let d = 1.0 - cosine_similarity(v, c);
                    d * d
                })
                .fold(f32::MAX, f32::min);
            if min_d2 > best_d2 || best_d2 < 0.0 {
                best_d2 = min_d2;
                best_idx = i;
            }
        }
        centroids.push(vectors[best_idx].clone());
    }

    // Cluster assignments: which centroid each point belongs to
    let mut assignments = vec![0i32; n];

    for _iter in 0..max_iter {
        let mut changed = false;

        // Assign each vector to nearest centroid
        for (i, v) in vectors.iter().enumerate() {
            let mut best_centroid = 0;
            let mut best_sim = -1.0f32;
            for (j, c) in centroids.iter().enumerate() {
                let sim = cosine_similarity(v, c);
                if sim > best_sim {
                    best_sim = sim;
                    best_centroid = j as i32;
                }
            }
            if assignments[i] != best_centroid {
                changed = true;
            }
            assignments[i] = best_centroid;
        }

        if !changed {
            break;
        }

        // Recompute centroids
        let mut sums = vec![vec![0.0f32; dims]; k];
        let mut counts = vec![0usize; k];

        for (i, v) in vectors.iter().enumerate() {
            let cid = assignments[i] as usize;
            for (j, x) in v.iter().enumerate() {
                sums[cid][j] += x;
            }
            counts[cid] += 1;
        }

        for (j, c) in centroids.iter_mut().enumerate() {
            if counts[j] > 0 {
                for (component, sum) in c.iter_mut().zip(&sums[j]) {
                    *component = *sum / counts[j] as f32;
                }
            }
        }
    }

    // Fallback: recompute centroids cleanly one more time
    let mut sums = vec![vec![0.0f32; dims]; k];
    let mut counts = vec![0usize; k];
    for (i, v) in vectors.iter().enumerate() {
        let cid = assignments[i] as usize;
        for (j, x) in v.iter().enumerate() {
            sums[cid][j] += x;
        }
        counts[cid] += 1;
    }
    for (j, c) in centroids.iter_mut().enumerate() {
        if counts[j] > 0 {
            for (d, x) in c.iter_mut().enumerate() {
                *x = sums[j][d] / counts[j] as f32;
            }
        }
    }

    tracing::debug!(
        "kmeans: k={} n={} iters={} counts={:?}",
        k,
        n,
        max_iter,
        counts
    );

    (centroids, assignments)
}

#[cfg(test)]
mod tests {
    use super::{cosine_similarity, kmeans};

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

    #[test]
    fn kmeans_separates_angular_clusters() {
        // Distinct directions, not collinear vectors separated only by magnitude.
        // Neither group is axis-aligned: corrupt intermediate means must not
        // collapse both groups onto the same axis on the next iteration.
        let vectors = vec![
            vec![1.0, 3.0],
            vec![2.0, 6.0],
            vec![-1.0, 3.0],
            vec![-2.0, 6.0],
        ];
        let (centroids, assignments) = kmeans(&vectors, 2, 20);

        assert_eq!(centroids.len(), 2);
        // First two should be in one cluster, last two in another
        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[2], assignments[3]);
        assert_ne!(assignments[0], assignments[2]);
        assert_eq!(centroids[assignments[0] as usize], vec![1.5, 4.5]);
        assert_eq!(centroids[assignments[2] as usize], vec![-1.5, 4.5]);
    }

    #[test]
    fn kmeans_handles_more_clusters_than_dimensions() {
        let vectors = vec![
            vec![1.0, 3.0],
            vec![2.0, 6.0],
            vec![-1.0, 3.0],
            vec![-2.0, 6.0],
            vec![0.0, -2.0],
            vec![0.0, -4.0],
        ];
        let (centroids, assignments) = kmeans(&vectors, 3, 20);
        for pair in 0..3 {
            assert_eq!(assignments[pair * 2], assignments[pair * 2 + 1]);
            let c = &centroids[assignments[pair * 2] as usize];
            for d in 0..2 {
                assert_eq!(
                    c[d],
                    (vectors[pair * 2][d] + vectors[pair * 2 + 1][d]) / 2.0
                );
            }
        }
        assert_ne!(assignments[0], assignments[2]);
        assert_ne!(assignments[0], assignments[4]);
        assert_ne!(assignments[2], assignments[4]);
    }

    /// Seeding picks the vector that is truly farthest, including when cosines
    /// are negative. (The slumber realm split used to keep overwriting its
    /// "farthest" pick while the best similarity was negative.)
    #[test]
    fn kmeans_seeds_with_the_farthest_vector_even_for_negative_cosines() {
        let vectors = vec![
            vec![1.0, 0.0],
            vec![-0.5, 0.5],  // cosine -0.71 to the first vector
            vec![-1.0, 0.01], // cosine -1.0: the farthest
            vec![-0.9, 0.3],  // cosine -0.95
        ];
        let (centroids, assignments) = kmeans(&vectors, 2, 1);
        assert_eq!(centroids.len(), 2);
        assert_eq!(assignments[0], 0);
        assert_eq!(assignments[2], 1, "the farthest vector must seed cluster 1");
    }

    #[test]
    fn kmeans_edge_cases() {
        assert_eq!(kmeans(&[], 2, 10), (vec![], vec![]));
        assert_eq!(kmeans(&[vec![1.0]], 0, 10), (vec![], vec![]));
        let (centroids, assignments) = kmeans(&[vec![1.0, 0.0]], 2, 10);
        assert_eq!((centroids.len(), assignments), (1, vec![0]));
        let same = vec![vec![1.0, 0.0, 0.0]; 5];
        let (_, assignments) = kmeans(&same, 2, 10);
        assert_eq!(assignments.len(), 5);
    }

    #[test]
    fn kmeans_is_deterministic() {
        let vectors: Vec<Vec<f32>> = (0..20)
            .map(|i| vec![(i as f32).sin(), (i as f32 * 0.7).cos(), 0.3])
            .collect();
        assert_eq!(kmeans(&vectors, 3, 20), kmeans(&vectors, 3, 20));
    }
}
