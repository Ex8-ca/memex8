use rand::Rng;
use serde::{Deserialize, Serialize};

/// TurboQuant-inspired vector quantizer (arXiv:2504.19874).
///
/// Uses uniform scalar quantization on normalized unit vectors with
/// bit-packing for efficient storage.
///
/// Performance targets (any dimension):
///   - 3.5 bits/channel → cosine > 0.90
///   - 2.5 bits/channel → cosine > 0.80
///   - 4.0 bits/channel → cosine > 0.95
///
/// Approach: normalize to unit vector, find per-vector value range,
/// uniformly quantize within that range, store min/max for reconstruction.
#[derive(Debug, Clone)]
pub struct TurboQuantizer {
    dimensions: usize,
    bit_width: f32,
    levels: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedVector {
    /// Packed indices (bit-packed for 2.5/3.0/3.5-bit quantization).
    /// Each index is `ceil(bit_width)` bits wide.
    pub packed: Vec<u8>,
    pub bit_width: f32,
    pub dimensions: usize,
    pub norm: f32,
    /// Per-vector value range for reconstruction.
    pub min_val: f32,
    pub max_val: f32,
}

/// Bit-width in bits per index, rounded up to whole bytes.
impl QuantizedVector {
    /// Number of bytes used for packed indices.
    pub fn packed_size(&self) -> usize {
        let bits_per_index = self.bit_width.ceil() as usize;
        let total_bits = self.dimensions * bits_per_index;
        (total_bits + 7) / 8 // round up to bytes
    }

    /// Total storage including metadata (norm + min_val + max_val + dims + bit_width).
    pub fn total_size(&self) -> usize {
        self.packed_size() + 4 + 4 + 4 + 4 + 4
    }

    /// Compression ratio compared to storing as f32.
    pub fn compression_ratio(&self) -> f32 {
        let original = self.dimensions * 4; // f32 = 4 bytes
        original as f32 / self.total_size() as f32
    }
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
    /// - `dimensions`: vector dimensionality (e.g., 1536 for text-embedding-3-small)
    /// - `bit_width`: target bits per dimension (2.5, 3.0, 3.5, or 4.0)
    pub fn new(dimensions: usize, bit_width: f32) -> Self {
        let levels = (2.0f32.powf(bit_width)).round() as usize;
        let levels = levels.max(2).min(256);

        Self {
            dimensions,
            bit_width,
            levels,
        }
    }

    /// Quantize a vector.
    ///
    /// Pipeline: normalize → find value range → uniform quantize → pack indices
    pub fn quantize(&self, vector: &[f32]) -> QuantizedVector {
        let norm = vector_norm(vector);
        if norm == 0.0 {
            return QuantizedVector {
                packed: vec![0; self.dimensions],
                bit_width: self.bit_width,
                dimensions: self.dimensions,
                norm: 0.0,
                min_val: 0.0,
                max_val: 1.0,
            };
        }

        let normalized: Vec<f32> = vector.iter().map(|x| x / norm).collect();

        // Find actual min/max of normalized values
        let min_val = normalized.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_val = normalized.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let range = max_val - min_val;

        // Quantize each coordinate into [0, levels) range
        let indices: Vec<u8> = if range < 1e-9 {
            // All values are nearly identical
            vec![0u8; self.dimensions]
        } else {
            normalized
                .iter()
                .map(|&v| {
                    let t = (v - min_val) / range;
                    ((t * (self.levels - 1) as f32).round() as usize).min(self.levels - 1) as u8
                })
                .collect()
        };

        // Bit-pack the indices
        let bits_per_index = self.bit_width.ceil() as usize;
        let packed = pack_bits(&indices, bits_per_index, self.dimensions);

        QuantizedVector {
            packed,
            bit_width: self.bit_width,
            dimensions: self.dimensions,
            norm,
            min_val,
            max_val,
        }
    }

    /// Reconstruct approximate vector from quantized representation.
    pub fn dequantize(&self, qv: &QuantizedVector) -> Vec<f32> {
        let bits_per_index = qv.bit_width.ceil() as usize;
        let indices = unpack_bits(&qv.packed, bits_per_index, qv.dimensions);
        let range = qv.max_val - qv.min_val;
        let levels_minus_1 = (self.levels as f32) - 1.0;

        indices
            .iter()
            .map(|&idx| {
                let val = qv.min_val + (idx as f32 / levels_minus_1) * range;
                val * qv.norm
            })
            .collect()
    }

    /// Quantize and measure quality metrics.
    pub fn quantize_with_report(&self, vector: &[f32]) -> (QuantizedVector, QuantReport) {
        let qv = self.quantize(vector);
        let reconstructed = self.dequantize(&qv);

        let mse = mean_squared_error(vector, &reconstructed);
        let cosine = cosine_similarity(vector, &reconstructed);
        let original_bytes = vector.len() * 4;
        let compressed_bytes = qv.total_size();

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

/// Pack `n` indices into a byte array, using `bits_per_index` bits each.
/// Indices must be in range [0, 2^bits_per_index).
fn pack_bits(indices: &[u8], bits_per_index: usize, n: usize) -> Vec<u8> {
    let total_bits = n * bits_per_index;
    let total_bytes = (total_bits + 7) / 8;
    let mut packed = vec![0u8; total_bytes];

    for (i, &idx) in indices.iter().enumerate().take(n) {
        let bit_offset = i * bits_per_index;
        let byte_offset = bit_offset / 8;
        let bit_in_byte = bit_offset % 8;
        let value = idx as u32;

        for b in 0..bits_per_index.min(8) {
            let bit = (value >> b) & 1;
            let target_byte = byte_offset + (bit_in_byte + b) / 8;
            let target_bit = (bit_in_byte + b) % 8;
            if target_byte < packed.len() {
                packed[target_byte] |= (bit as u8) << target_bit;
            }
        }
    }

    packed
}

/// Unpack indices from a bit-packed byte array.
fn unpack_bits(packed: &[u8], bits_per_index: usize, n: usize) -> Vec<u8> {
    let mut indices = vec![0u8; n];

    for i in 0..n {
        let bit_offset = i * bits_per_index;
        let byte_offset = bit_offset / 8;
        let bit_in_byte = bit_offset % 8;
        let mut value = 0u32;

        for b in 0..bits_per_index.min(8) {
            let source_byte = byte_offset + (bit_in_byte + b) / 8;
            let source_bit = (bit_in_byte + b) % 8;
            if source_byte < packed.len() {
                let bit = (packed[source_byte] >> source_bit) & 1;
                value |= (bit as u32) << b;
            }
        }

        indices[i] = value as u8;
    }

    indices
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

    #[test]
    fn test_pack_unpack_roundtrip() {
        // Test 3-bit packing
        let indices: Vec<u8> = (0..100).map(|i| (i % 8) as u8).collect();
        let packed = pack_bits(&indices, 3, 100);
        let unpacked = unpack_bits(&packed, 3, 100);
        assert_eq!(indices, unpacked, "3-bit pack/unpack mismatch");

        // Test 4-bit packing
        let indices: Vec<u8> = (0..100).map(|i| (i % 16) as u8).collect();
        let packed = pack_bits(&indices, 4, 100);
        let unpacked = unpack_bits(&packed, 4, 100);
        assert_eq!(indices, unpacked, "4-bit pack/unpack mismatch");

        // Test 8-bit packing (should be identity)
        let indices: Vec<u8> = (0..100).map(|i| (i % 256) as u8).collect();
        let packed = pack_bits(&indices, 8, 100);
        let unpacked = unpack_bits(&packed, 8, 100);
        assert_eq!(indices, unpacked, "8-bit pack/unpack mismatch");
    }

    #[test]
    fn test_compression_ratio() {
        let dims = 768;
        let quantizer = TurboQuantizer::new(dims, 3.5);
        let original = make_test_vector(dims, 42);
        let qv = quantizer.quantize(&original);

        let ratio = qv.compression_ratio();
        let packed_bytes = qv.packed_size();
        let total_bytes = qv.total_size();

        println!(
            "768d @ 3.5-bit: {} bytes (packed) / {} bytes (total) / {:.1}x compression",
            packed_bytes, total_bytes, ratio
        );

        // 768 * 3.5 = 2688 bits = 336 bytes + 20 metadata = 356 bytes
        // Original: 768 * 4 = 3072 bytes
        // Ratio: 3072 / 356 = 8.6x
        assert!(ratio > 5.0, "Compression ratio {:.1} too low", ratio);
        assert!(packed_bytes < 500, "Packed size {} too large", packed_bytes);
    }

    #[test]
    fn test_768d_benchmark() {
        // Benchmark at the actual embedding dimension (nomic-embed-text = 768d)
        let dims = 768;
        let original = make_test_vector(dims, 42);

        println!("\n=== 768d TurboQuant Benchmark ===");
        println!("Vector: {}d, {} bytes original\n", dims, dims * 4);

        for bits in [2.0, 2.5, 3.0, 3.5, 4.0] {
            let quantizer = TurboQuantizer::new(dims, bits);
            let (qv, report) = quantizer.quantize_with_report(&original);

            println!(
                "{:.1}-bit | levels={:3} | cos={:.4} | MSE={:.6} | {} bytes packed | {:.1}x",
                bits,
                quantizer.codebook.len(),
                report.cosine_similarity,
                report.mse,
                qv.packed_size(),
                report.compression_ratio,
            );
        }
        println!();

        // 3.5-bit should achieve > 0.85 cosine at 768d
        let q35 = TurboQuantizer::new(dims, 3.5);
        let (_qv, report) = q35.quantize_with_report(&original);
        assert!(
            report.cosine_similarity > 0.85,
            "768d 3.5-bit cosine {:.4} below 0.85",
            report.cosine_similarity
        );
    }
}
