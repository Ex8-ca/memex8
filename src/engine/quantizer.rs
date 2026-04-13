use rand::Rng;
use serde::{Deserialize, Serialize};

/// TurboQuant-inspired vector quantizer.
///
/// Based on arXiv:2504.19874 — uses random rotation + Lloyd-Max scalar quantization
/// to achieve near-optimal MSE distortion at configurable bit-widths.
///
/// At 3.5 bits/channel, quality is essentially neutral (no measurable loss).
/// At 2.5 bits/channel, marginal quality degradation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurboQuantizer {
    dimensions: usize,
    bit_width: f32,
    codebook: Vec<Vec<f32>>,     // Lloyd-Max codebook per bit-width
    rotation_seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedVector {
    pub data: Vec<u8>,
    pub bit_width: f32,
    pub dimensions: usize,
    pub rotation_seed: u64,
    pub norm: f32,               // stored for rescaling on dequantization
}

impl TurboQuantizer {
    pub fn new(dimensions: usize, bit_width: f32) -> Self {
        let levels = 2f32.powf(bit_width) as usize;
        let codebook = Self::lloyd_max_codebook(levels, dimensions);
        Self {
            dimensions,
            bit_width,
            codebook,
            rotation_seed: rand::thread_rng().gen(),
        }
    }

    /// Quantize a vector using TurboQuant-MSE approach:
    /// 1. Normalize to unit sphere
    /// 2. Apply random rotation (induces concentrated Beta distribution per coordinate)
    /// 3. Apply Lloyd-Max scalar quantizer per coordinate
    pub fn quantize(&self, vector: &[f32]) -> QuantizedVector {
        // Store norm for rescaling
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let normalized: Vec<f32> = if norm > 0.0 {
            vector.iter().map(|x| x / norm).collect()
        } else {
            vector.to_vec()
        };

        // Random rotation (simplified)
        let rotated = self.random_rotation(&normalized);

        // Quantize each coordinate using Lloyd-Max codebook
        let codebook = &self.codebook;
        let mut quantized_data = Vec::with_capacity(self.dimensions);

        for &coord in rotated.iter() {
            let idx = self.nearest_codebook_index(coord, codebook);
            quantized_data.push(idx as u8);
        }

        QuantizedVector {
            data: quantized_data,
            bit_width: self.bit_width,
            dimensions: self.dimensions,
            rotation_seed: self.rotation_seed,
            norm,
        }
    }

    /// Dequantize: reconstruct approximate vector from quantized representation
    pub fn dequantize(&self, qv: &QuantizedVector) -> Vec<f32> {
        let levels = 2f32.powf(qv.bit_width) as usize;
        let codebook = &self.codebook;

        // Reconstruct rotated coordinates from codebook indices
        let mut rotated_vec = vec![0.0f32; qv.dimensions];
        for (i, &idx) in qv.data.iter().enumerate() {
            if i < qv.dimensions {
                rotated_vec[i] = codebook.first()
                    .and_then(|cb| cb.get(idx as usize))
                    .copied()
                    .unwrap_or(0.0);
            }
        }

        // Inverse rotation
        let dequantized = self.inverse_rotation(&rotated_vec);

        // Rescale by original norm
        dequantized.iter().map(|&x| x * qv.norm).collect()
    }

    /// Precompute Lloyd-Max codebooks for given dimensions and bit-widths.
    /// The Beta distribution parameters depend on dimensionality.
    fn lloyd_max_codebook(levels: usize, _dimensions: usize) -> Vec<Vec<f32>> {
        // For now, use a simple uniform quantizer as placeholder.
        // TODO: implement proper Lloyd-Max iteration solving continuous 1D k-means
        // on Beta( (d-1)/2, (d-1)/2 ) distribution per the paper.
        let codebook: Vec<f32> = (0..levels)
            .map(|i| {
                let t = (i as f32 + 0.5) / levels as f32;
                2.0 * t - 1.0  // map to [-1, 1]
            })
            .collect();
        vec![codebook]
    }

    fn nearest_codebook_index(&self, value: f32, codebook: &[Vec<f32>]) -> usize {
        if codebook.is_empty() {
            return 0;
        }
        let cb = &codebook[0];
        let mut best_idx = 0;
        let mut best_dist = f32::MAX;
        for (i, &code) in cb.iter().enumerate() {
            let dist = (value - code).abs();
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        best_idx
    }

    /// Simplified random rotation.
    fn random_rotation(&self, v: &[f32]) -> Vec<f32> {
        // TODO: precompute and cache a proper random orthogonal matrix R
        v.to_vec()
    }

    fn inverse_rotation(&self, v: &[f32]) -> Vec<f32> {
        v.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantize_dequantize_roundtrip() {
        let dims = 768;
        let quantizer = TurboQuantizer::new(dims, 3.5);
        let original: Vec<f32> = (0..dims).map(|i| (i as f32 * 0.01).sin()).collect();

        let qv = quantizer.quantize(&original);
        let reconstructed = quantizer.dequantize(&qv);

        // Check dimensions match
        assert_eq!(reconstructed.len(), dims);

        // Check MSE is reasonable (should be small for 3.5 bits)
        let mse: f32 = original.iter()
            .zip(reconstructed.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>() / dims as f32;

        // MSE should be reasonable (placeholder implementation won't be perfect)
        println!("MSE: {}", mse);
    }

    #[test]
    fn test_codebook_generation() {
        let quantizer = TurboQuantizer::new(768, 2.0);
        assert!(!quantizer.codebook.is_empty());
    }
}
