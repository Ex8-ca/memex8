use rand::Rng;
use serde::{Deserialize, Serialize};

/// TurboQuant-inspired vector quantizer (arXiv:2504.19874).
///
/// Uses random sign rotation + Lloyd-Max scalar quantization on the induced
/// Beta distribution to achieve near-optimal MSE distortion at configurable bit-widths.
///
/// Performance targets (768-d embeddings):
///   - 3.5 bits/channel → cosine > 0.95
///   - 2.5 bits/channel → cosine > 0.80
///   - 4.0 bits/channel → cosine > 0.98
#[derive(Debug, Clone)]
pub struct TurboQuantizer {
    dimensions: usize,
    bit_width: f32,
    levels: usize,
    codebook: Vec<f32>,
    rotation_seed: u64,
    rotation_matrix: Vec<f32>, // flattened row-major orthogonal matrix
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedVector {
    pub indices: Vec<u8>,
    pub bit_width: f32,
    pub dimensions: usize,
    pub rotation_seed: u64,
    pub norm: f32,
}

/// Quantization quality report.
#[derive(Debug, Clone)]
pub struct QuantReport {
    pub bit_width: f32,
    pub dimensions: usize,
    pub mse: f32,
    pub cosine_similarity: f32,
    pub storage_bytes: usize,
    pub compression_ratio: f32,
}

impl TurboQuantizer {
    /// Create a new TurboQuantizer.
    ///
    /// - `dimensions`: vector dimensionality (e.g., 768 for nomic-embed-text)
    /// - `bit_width`: target bits per dimension (2.5, 3.0, 3.5, or 4.0)
    pub fn new(dimensions: usize, bit_width: f32) -> Self {
        let levels = (2.0f32.powf(bit_width)).round() as usize;
        let levels = levels.max(2).min(256);

        // Generate Lloyd-Max codebook for the Beta distribution
        let codebook = Self::lloyd_max_codebook(levels, dimensions);

        // Generate a random orthogonal rotation matrix via QR decomposition
        let mut rng = rand::thread_rng();
        let rotation_seed: u64 = rng.gen();
        let rotation_matrix = Self::random_orthogonal_matrix(dimensions, rotation_seed);

        Self {
            dimensions,
            bit_width,
            levels,
            codebook,
            rotation_seed,
            rotation_matrix,
        }
    }

    /// Quantize a vector.
    ///
    /// Pipeline: normalize → random rotation → scalar quantize → pack indices
    pub fn quantize(&self, vector: &[f32]) -> QuantizedVector {
        let norm = vector_norm(vector);
        if norm == 0.0 {
            return QuantizedVector {
                indices: vec![0; self.dimensions],
                bit_width: self.bit_width,
                dimensions: self.dimensions,
                rotation_seed: self.rotation_seed,
                norm: 0.0,
            };
        }

        let normalized: Vec<f32> = vector.iter().map(|x| x / norm).collect();

        // Apply random rotation
        let rotated = mat_vec_mul(&self.rotation_matrix, &normalized, self.dimensions);

        // Scalar quantize each coordinate
        let indices: Vec<u8> = rotated
            .iter()
            .map(|&coord| self.nearest_codebook_index(coord) as u8)
            .collect();

        QuantizedVector {
            indices,
            bit_width: self.bit_width,
            dimensions: self.dimensions,
            rotation_seed: self.rotation_seed,
            norm,
        }
    }

    /// Reconstruct approximate vector from quantized representation.
    pub fn dequantize(&self, qv: &QuantizedVector) -> Vec<f32> {
        // Map indices to codebook values
        let rotated: Vec<f32> = qv
            .indices
            .iter()
            .map(|&idx| self.codebook.get(idx as usize).copied().unwrap_or(0.0))
            .collect();

        // Inverse rotation (transpose for orthogonal matrix)
        let recovered = mat_vec_mul_transpose(&self.rotation_matrix, &rotated, self.dimensions);

        // Rescale
        recovered.iter().map(|x| x * qv.norm).collect()
    }

    /// Quantize and measure quality metrics.
    pub fn quantize_with_report(&self, vector: &[f32]) -> (QuantizedVector, QuantReport) {
        let qv = self.quantize(vector);
        let reconstructed = self.dequantize(&qv);

        let mse = mean_squared_error(vector, &reconstructed);
        let cosine = cosine_similarity(vector, &reconstructed);
        let original_bytes = vector.len() * 4;
        let compressed_bytes = qv.indices.len() + 12;

        let report = QuantReport {
            bit_width: self.bit_width,
            dimensions: self.dimensions,
            mse,
            cosine_similarity: cosine,
            storage_bytes: compressed_bytes,
            compression_ratio: original_bytes as f32 / compressed_bytes as f32,
        };

        (qv, report)
    }

    // ─── Random Orthogonal Matrix via QR ──────────────────────────────────────

    /// Generate a random orthogonal matrix via QR decomposition of a random
    /// Gaussian matrix. This produces a uniformly random rotation from the
    /// Haar measure on O(n).
    fn random_orthogonal_matrix(n: usize, seed: u64) -> Vec<f32> {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);

        // Generate random Gaussian matrix
        let mut a: Vec<f32> = (0..n * n)
            .map(|_| {
                let u1: f32 = rng.gen_range(0.0001..1.0);
                let u2: f32 = rng.gen_range(0.0001..1.0);
                (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
            })
            .collect();

        // QR decomposition via modified Gram-Schmidt
        let mut q = vec![0.0; n * n];

        for j in 0..n {
            // Copy column j
            for i in 0..n {
                q[i * n + j] = a[i * n + j];
            }

            // Orthogonalize against previous columns
            for k in 0..j {
                let dot: f32 = (0..n).map(|i| q[i * n + j] * q[i * n + k]).sum();
                for i in 0..n {
                    q[i * n + j] -= dot * q[i * n + k];
                }
            }

            // Normalize
            let norm: f32 = (0..n).map(|i| q[i * n + j].powi(2)).sum::<f32>().sqrt();
            if norm > 0.0 {
                for i in 0..n {
                    q[i * n + j] /= norm;
                }
            }
        }

        q
    }

    // ─── Lloyd-Max Codebook ──────────────────────────────────────────────────

    /// Generate optimal Lloyd-Max codebook for the Beta((d-1)/2, (d-1)/2)
    /// distribution scaled to [-1, 1].
    ///
    /// After random rotation of a unit vector in R^d, each coordinate follows
    /// this distribution. The Lloyd-Max algorithm finds the optimal scalar
    /// quantizer (minimizing expected squared error) for this distribution.
    fn lloyd_max_codebook(levels: usize, dimensions: usize) -> Vec<f32> {
        if levels <= 1 {
            return vec![0.0];
        }

        let alpha = (dimensions as f32 - 1.0) / 2.0;

        // Initialize with uniform quantization of [-1, 1]
        let mut codebook: Vec<f32> = (0..levels)
            .map(|i| 2.0 * (i as f32 + 0.5) / levels as f32 - 1.0)
            .collect();

        // Lloyd-Max iteration (50 iterations, convergence check)
        let samples = Self::sample_beta(alpha, 50_000);

        for _iter in 0..50 {
            let mut centroids = vec![0.0f32; levels];
            let mut counts = vec![0usize; levels];

            for &sample in &samples {
                let idx = find_nearest(&codebook, sample);
                centroids[idx] += sample;
                counts[idx] += 1;
            }

            let mut converged = true;
            for i in 0..levels {
                if counts[i] > 0 {
                    centroids[i] /= counts[i] as f32;
                } else {
                    // Re-initialize empty cell
                    centroids[i] = 2.0 * (i as f32 + 0.5) / levels as f32 - 1.0;
                }
                if (centroids[i] - codebook[i]).abs() > 1e-7 {
                    converged = false;
                }
            }

            codebook = centroids;
            if converged {
                break;
            }
        }

        // Ensure monotonic (required for binary search)
        codebook.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        codebook
    }

    /// Sample from Beta(alpha, alpha) scaled to [-1, 1].
    fn sample_beta(alpha: f32, n: usize) -> Vec<f32> {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(12345);
        let mut samples = Vec::with_capacity(n);

        for _ in 0..n {
            let x = sample_gamma(alpha, &mut rng);
            let y = sample_gamma(alpha, &mut rng);
            let beta = x / (x + y);
            samples.push(2.0 * beta - 1.0);
        }

        samples
    }

    // ─── Codebook Lookup ─────────────────────────────────────────────────────

    fn nearest_codebook_index(&self, value: f32) -> usize {
        find_nearest(&self.codebook, value)
    }
}

// ─── Linear Algebra ───────────────────────────────────────────────────────────

/// Matrix-vector multiplication: y = M * x where M is n×n stored row-major.
fn mat_vec_mul(m: &[f32], x: &[f32], n: usize) -> Vec<f32> {
    let mut y = vec![0.0f32; n];
    for i in 0..n {
        for j in 0..n {
            y[i] += m[i * n + j] * x[j];
        }
    }
    y
}

/// Matrix-transpose-vector multiplication: y = M^T * x.
/// For orthogonal M, M^{-1} = M^T.
fn mat_vec_mul_transpose(m: &[f32], x: &[f32], n: usize) -> Vec<f32> {
    let mut y = vec![0.0f32; n];
    for i in 0..n {
        for j in 0..n {
            y[i] += m[j * n + i] * x[j]; // transpose: M[j][i] instead of M[i][j]
        }
    }
    y
}

// ─── Utility Functions ────────────────────────────────────────────────────────

fn vector_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn mean_squared_error(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f32::MAX;
    }
    a.iter()
        .zip(b.iter())
        .take(n)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        / n as f32
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).take(n).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().take(n).map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().take(n).map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

/// Binary search for nearest value in sorted array.
fn find_nearest(sorted: &[f32], value: f32) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    if sorted.len() == 1 {
        return 0;
    }

    let idx = sorted.partition_point(|&x| x < value);

    if idx == 0 {
        0
    } else if idx >= sorted.len() {
        sorted.len() - 1
    } else {
        let dist_left = (value - sorted[idx - 1]).abs();
        let dist_right = (sorted[idx] - value).abs();
        if dist_left <= dist_right {
            idx - 1
        } else {
            idx
        }
    }
}

// ─── Gamma Distribution Sampling ─────────────────────────────────────────────

fn sample_gamma(alpha: f32, rng: &mut impl rand::Rng) -> f32 {
    if alpha < 1.0 {
        let u: f32 = rng.gen_range(0.0001..1.0);
        return sample_gamma(alpha + 1.0, rng) * u.powf(1.0 / alpha);
    }

    let d = alpha - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();

    loop {
        let v = {
            let u1: f32 = rng.gen_range(0.0001..1.0);
            let u2: f32 = rng.gen_range(0.0001..1.0);
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
        };

        let candidate = d * (1.0 + c * v).powi(3);
        if candidate > 0.0 {
            let u: f32 = rng.gen();
            if u < 1.0 - 0.0331 * (v * v).powi(2) {
                return candidate;
            }
            if u.ln() < 0.5 * v * v + d * (1.0 - candidate + candidate.ln()) {
                return candidate;
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_vector(dims: usize, seed: u64) -> Vec<f32> {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        // Create a normalized random vector
        let v: Vec<f32> = (0..dims).map(|_| rng.gen::<f32>() - 0.5).collect();
        let norm = vector_norm(&v);
        v.iter().map(|x| x / norm).collect()
    }

    #[test]
    fn test_rotation_roundtrip() {
        let dims = 128; // smaller for QR speed
        let quantizer = TurboQuantizer::new(dims, 4.0);
        let original = make_test_vector(dims, 42);

        // Forward rotation
        let rotated = mat_vec_mul(&quantizer.rotation_matrix, &original, dims);
        // Inverse rotation (transpose)
        let recovered = mat_vec_mul_transpose(&quantizer.rotation_matrix, &rotated, dims);

        let mse: f32 = original
            .iter()
            .zip(recovered.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            / dims as f32;

        assert!(mse < 1e-8, "Rotation roundtrip MSE {} too high", mse);
    }

    #[test]
    fn test_roundtrip_4_0_bits() {
        let dims = 128;
        let quantizer = TurboQuantizer::new(dims, 4.0);
        let original = make_test_vector(dims, 42);

        let (_qv, report) = quantizer.quantize_with_report(&original);

        println!(
            "4.0-bit ({}d): cos={:.4} MSE={:.6} compression={:.1}x",
            dims, report.cosine_similarity, report.mse, report.compression_ratio
        );

        assert!(
            report.cosine_similarity > 0.95,
            "4.0-bit cosine {:.4} below 0.95",
            report.cosine_similarity
        );
    }

    #[test]
    fn test_roundtrip_3_5_bits() {
        let dims = 128;
        let quantizer = TurboQuantizer::new(dims, 3.5);
        let original = make_test_vector(dims, 42);

        let (_qv, report) = quantizer.quantize_with_report(&original);

        println!(
            "3.5-bit ({}d): cos={:.4} MSE={:.6}",
            dims, report.cosine_similarity, report.mse
        );

        assert!(
            report.cosine_similarity > 0.90,
            "3.5-bit cosine {:.4} below 0.90",
            report.cosine_similarity
        );
    }

    #[test]
    fn test_roundtrip_2_5_bits() {
        let dims = 128;
        let quantizer = TurboQuantizer::new(dims, 2.5);
        let original = make_test_vector(dims, 42);

        let (_qv, report) = quantizer.quantize_with_report(&original);

        println!(
            "2.5-bit ({}d): cos={:.4} MSE={:.6}",
            dims, report.cosine_similarity, report.mse
        );

        assert!(
            report.cosine_similarity > 0.70,
            "2.5-bit cosine {:.4} below 0.70",
            report.cosine_similarity
        );
    }

    #[test]
    fn test_codebook_sorted() {
        let quantizer = TurboQuantizer::new(128, 3.5);
        for i in 1..quantizer.codebook.len() {
            assert!(
                quantizer.codebook[i] >= quantizer.codebook[i - 1],
                "Codebook not sorted at index {}",
                i
            );
        }
    }

    #[test]
    fn test_codebook_in_range() {
        let quantizer = TurboQuantizer::new(128, 3.5);
        for &val in &quantizer.codebook {
            assert!(
                val >= -1.0 && val <= 1.0,
                "Codebook value {} outside [-1, 1]",
                val
            );
        }
    }

    #[test]
    fn test_norm_preservation() {
        let dims = 128;
        let quantizer = TurboQuantizer::new(dims, 3.5);
        let original: Vec<f32> = (0..dims).map(|i| i as f32 * 0.1).collect();

        let original_norm = vector_norm(&original);
        let qv = quantizer.quantize(&original);

        assert!((qv.norm - original_norm).abs() < 1e-4);
    }

    #[test]
    fn test_bit_width_scaling() {
        let dims = 128;
        let original = make_test_vector(dims, 42);

        let mut prev_cosine = -1.0f32;

        for bits in [2.0, 2.5, 3.0, 3.5, 4.0] {
            let quantizer = TurboQuantizer::new(dims, bits);
            let (_qv, report) = quantizer.quantize_with_report(&original);

            println!(
                "{:.1}-bit: cos={:.4} MSE={:.6}",
                bits, report.cosine_similarity, report.mse
            );

            // Higher bits should improve quality
            assert!(
                report.cosine_similarity >= prev_cosine - 0.02,
                "{:.1}-bit cosine {:.4} worse than previous {:.4}",
                bits, report.cosine_similarity, prev_cosine
            );
            prev_cosine = report.cosine_similarity;
        }
    }
}
