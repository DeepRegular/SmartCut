//! VC-1's 8x8 transform, forwards and back, and its quantizer.
//!
//! The format defines only the inverse transform -- what a decoder must do
//! with the numbers it is given -- and leaves the forward one to whoever
//! writes the encoder. [`inverse`] is the definition, ported from the
//! decoder so that a test can hold the two against each other; [`forward`]
//! is its algebraic inverse.
//!
//! The matrix is orthogonal with a different norm on each row, which is why
//! [`forward`] scales each coefficient by a factor of its own rather than by
//! one constant. That is also why the numbers here are floating point: the
//! spec's own forward transform is an integer approximation whose rounding
//! only matters to an encoder trying to be bit-exact with somebody else's,
//! and this one is not.

/// The transform matrix: row `k` is the basis of coefficient `k`.
pub const T: [[i32; 8]; 8] = [
    [12, 12, 12, 12, 12, 12, 12, 12],
    [16, 15, 9, 4, -4, -9, -15, -16],
    [16, 6, -6, -16, -16, -6, 6, 16],
    [15, -4, -16, -9, 9, 16, 4, -15],
    [12, -12, -12, 12, 12, -12, -12, 12],
    [9, -16, 4, 15, -15, -4, 16, -9],
    [6, -16, 16, -6, -6, 16, -16, 6],
    [4, -9, 15, -16, 16, -15, 9, -4],
];

/// `T` times its own transpose is diagonal; these are the diagonal entries.
const NORM: [f64; 8] = [
    1152.0, 1156.0, 1168.0, 1156.0, 1152.0, 1156.0, 1168.0, 1156.0,
];

/// What the inverse transform divides by once both passes have run.
const RANGE: f64 = 1024.0;

/// Transform an 8x8 block of samples into coefficients.
///
/// Samples are signed, i.e. the picture's values less 128, which is the form
/// the decoder adds back. Coefficients come out in the order the decoder
/// reads them, which is transposed from the obvious one -- see
/// [`crate::tables::SCAN_INTERLACED`].
pub fn forward(samples: &[i16; 64]) -> [f64; 64] {
    // M = T . samples' . T', then each entry scaled by the norms of the two
    // rows that produced it.
    let mut tmp = [[0.0f64; 8]; 8];
    for (i, row) in T.iter().enumerate() {
        for b in 0..8 {
            let mut acc = 0.0;
            for a in 0..8 {
                acc += row[a] as f64 * samples[b * 8 + a] as f64;
            }
            tmp[i][b] = acc;
        }
    }
    let mut out = [0.0f64; 64];
    for i in 0..8 {
        for (j, row) in T.iter().enumerate() {
            let mut acc = 0.0;
            for b in 0..8 {
                acc += tmp[i][b] * row[b] as f64;
            }
            out[i * 8 + j] = acc * RANGE / (NORM[i] * NORM[j]);
        }
    }
    out
}

/// The inverse transform, exactly as a decoder performs it.
///
/// Kept so the encoder can be tested against the thing it has to satisfy,
/// and so the reconstruction a later picture would predict from is the one
/// the decoder will actually hold.
pub fn inverse(block: &mut [i32; 64]) {
    let mut temp = [0i32; 64];
    for i in 0..8 {
        let s = |k: usize| block[k * 8 + i];
        let t1 = 12 * (s(0) + s(4)) + 4;
        let t2 = 12 * (s(0) - s(4)) + 4;
        let t3 = 16 * s(2) + 6 * s(6);
        let t4 = 6 * s(2) - 16 * s(6);
        let (t5, t6, t7, t8) = (t1 + t3, t2 + t4, t2 - t4, t1 - t3);
        let u1 = 16 * s(1) + 15 * s(3) + 9 * s(5) + 4 * s(7);
        let u2 = 15 * s(1) - 4 * s(3) - 16 * s(5) - 9 * s(7);
        let u3 = 9 * s(1) - 16 * s(3) + 4 * s(5) + 15 * s(7);
        let u4 = 4 * s(1) - 9 * s(3) + 15 * s(5) - 16 * s(7);
        let row = &mut temp[i * 8..i * 8 + 8];
        row[0] = (t5 + u1) >> 3;
        row[1] = (t6 + u2) >> 3;
        row[2] = (t7 + u3) >> 3;
        row[3] = (t8 + u4) >> 3;
        row[4] = (t8 - u4) >> 3;
        row[5] = (t7 - u3) >> 3;
        row[6] = (t6 - u2) >> 3;
        row[7] = (t5 - u1) >> 3;
    }
    for i in 0..8 {
        let s = |k: usize| temp[k * 8 + i];
        let t1 = 12 * (s(0) + s(4)) + 64;
        let t2 = 12 * (s(0) - s(4)) + 64;
        let t3 = 16 * s(2) + 6 * s(6);
        let t4 = 6 * s(2) - 16 * s(6);
        let (t5, t6, t7, t8) = (t1 + t3, t2 + t4, t2 - t4, t1 - t3);
        let u1 = 16 * s(1) + 15 * s(3) + 9 * s(5) + 4 * s(7);
        let u2 = 15 * s(1) - 4 * s(3) - 16 * s(5) - 9 * s(7);
        let u3 = 9 * s(1) - 16 * s(3) + 4 * s(5) + 15 * s(7);
        let u4 = 4 * s(1) - 9 * s(3) + 15 * s(5) - 16 * s(7);
        block[i] = (t5 + u1) >> 7;
        block[8 + i] = (t6 + u2) >> 7;
        block[16 + i] = (t7 + u3) >> 7;
        block[24 + i] = (t8 + u4) >> 7;
        block[32 + i] = (t8 - u4 + 1) >> 7;
        block[40 + i] = (t7 - u3 + 1) >> 7;
        block[48 + i] = (t6 - u2 + 1) >> 7;
        block[56 + i] = (t5 - u1 + 1) >> 7;
    }
}

/// The quantizer a picture is written with.
///
/// VC-1 has two, and which one a picture uses is not the encoder's choice
/// alone: the entry-point header may have settled it for the whole stream.
/// They differ only in where the levels sit -- the non-uniform one leaves a
/// wider gap around zero, which is where most of a coefficient block is.
#[derive(Debug, Clone, Copy)]
pub struct Quant {
    /// PQUANT: the step, as the picture header states it.
    pub step: i32,
    /// What a DC level is multiplied by.
    pub dc_scale: i32,
    /// Whether the levels sit evenly, or with a wider gap around zero.
    pub uniform: bool,
}

impl Quant {
    pub fn new(step: i32, uniform: bool) -> Self {
        Self {
            step,
            dc_scale: crate::tables::DC_SCALE[step.clamp(1, 31) as usize],
            uniform,
        }
    }

    /// The distance between one reconstruction level and the next.
    fn stride(&self) -> i32 {
        self.step * 2
    }

    /// How far the first level sits beyond the last one.
    fn offset(&self) -> i32 {
        if self.uniform {
            0
        } else {
            self.step
        }
    }

    /// Round an AC coefficient to a level.
    pub fn level(&self, coefficient: f64) -> i32 {
        let magnitude = coefficient.abs();
        // Zero is the only level that reconstructs to zero; the next one up
        // reconstructs to a whole step plus the offset, so the boundary
        // between them sits halfway.
        if magnitude * 2.0 < (self.stride() + self.offset()) as f64 {
            return 0;
        }
        let level = ((magnitude - self.offset() as f64) / self.stride() as f64)
            .round()
            .max(1.0);
        // A level is written in eight bits at most, escape or no escape.
        let level = level.min(255.0) as i32;
        if coefficient < 0.0 {
            -level
        } else {
            level
        }
    }

    /// What the decoder will make of a level again.
    pub fn reconstruct(&self, level: i32) -> i32 {
        match level {
            0 => 0,
            n if n > 0 => n * self.stride() + self.offset(),
            n => n * self.stride() - self.offset(),
        }
    }

    /// Round a DC coefficient to a level. The DC has a scale of its own and
    /// no offset.
    pub fn dc_level(&self, coefficient: f64) -> i32 {
        (coefficient / self.dc_scale as f64).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(samples: [i16; 64]) -> ([i32; 64], f64) {
        let coefficients = forward(&samples);
        let mut block = [0i32; 64];
        for (b, c) in block.iter_mut().zip(coefficients.iter()) {
            *b = c.round() as i32;
        }
        inverse(&mut block);
        let error = samples
            .iter()
            .zip(block.iter())
            .map(|(&s, &b)| (s as f64 - b as f64).powi(2))
            .sum::<f64>()
            / 64.0;
        (block, error.sqrt())
    }

    #[test]
    fn a_flat_block_survives_the_round_trip() {
        for value in [-128i16, -37, 0, 41, 127] {
            let (block, error) = round_trip([value; 64]);
            assert!(
                error < 0.6,
                "flat {value} came back with error {error}: {block:?}"
            );
        }
    }

    #[test]
    fn a_detailed_block_survives_the_round_trip() {
        // A deterministic spread of values, so the test says the same thing
        // every time it runs.
        let mut samples = [0i16; 64];
        let mut state = 12345u32;
        for s in samples.iter_mut() {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            *s = ((state >> 16) % 256) as i16 - 128;
        }
        let (_, error) = round_trip(samples);
        assert!(error < 1.0, "error {error}");
    }

    #[test]
    fn quantising_lands_on_the_nearest_level() {
        let q = Quant::new(6, false);
        for level in -8..=8 {
            let reconstructed = q.reconstruct(level);
            assert_eq!(q.level(reconstructed as f64), level, "level {level}");
        }
        // Just inside the dead zone around zero.
        assert_eq!(q.level((q.reconstruct(1) as f64) / 2.0 - 0.1), 0);
    }

    #[test]
    fn quantised_blocks_stay_close() {
        let q = Quant::new(4, false);
        let mut samples = [0i16; 64];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = ((i as i16 * 7) % 200) - 100;
        }
        let coefficients = forward(&samples);
        let mut block = [0i32; 64];
        block[0] = q.dc_level(coefficients[0]) * q.dc_scale;
        for k in 1..64 {
            block[k] = q.reconstruct(q.level(coefficients[k]));
        }
        inverse(&mut block);
        let error = samples
            .iter()
            .zip(block.iter())
            .map(|(&s, &b)| (s as f64 - b as f64).powi(2))
            .sum::<f64>()
            / 64.0;
        assert!(error.sqrt() < 6.0, "error {}", error.sqrt());
    }
}
