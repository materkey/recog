//! `np.random.default_rng(0).normal(...)`, draw for draw: numpy's PCG64 (XSL-RR) and
//! its ziggurat for the standard normal. The inverse filter is measured with this noise,
//! so the Rust measurement starts from the very samples the Python one does.

use super::ziggurat_tables::{FI_DOUBLE, KI_DOUBLE, PCG64_SEED0_INC, PCG64_SEED0_STATE, WI_DOUBLE};

const PCG64_MULTIPLIER: u128 = 0x2360_ed05_1fc6_5da4_4385_df64_9fcc_f645;
const ZIGGURAT_NOR_R: f64 = 3.654_152_885_361_009;
const ZIGGURAT_NOR_INV_R: f64 = 0.273_661_237_329_758_3;

/// numpy's `PCG64` bit generator.
#[derive(Debug, Clone)]
pub struct Pcg64 {
    state: u128,
    inc: u128,
}

impl Pcg64 {
    /// The generator of `np.random.default_rng(0)`.
    #[must_use]
    pub fn seed0() -> Self {
        Self { state: PCG64_SEED0_STATE, inc: PCG64_SEED0_INC }
    }

    /// `pcg64_next64`: step, then the XSL-RR output of the new state.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(PCG64_MULTIPLIER).wrapping_add(self.inc);
        #[allow(clippy::cast_possible_truncation)]
        let (hi, lo, rot) = ((self.state >> 64) as u64, self.state as u64, (self.state >> 122) as u32);
        (hi ^ lo).rotate_right(rot)
    }

    /// `next_double`: 53 random bits in [0, 1).
    pub fn next_double(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let bits = (self.next_u64() >> 11) as f64;
        bits * (1.0 / 9_007_199_254_740_992.0)
    }

    /// `random_standard_normal`.
    pub fn standard_normal(&mut self) -> f64 {
        loop {
            let mut r = self.next_u64();
            let idx = (r & 0xff) as usize;
            r >>= 8;
            let sign = r & 0x1;
            let rabs = (r >> 1) & 0x000f_ffff_ffff_ffff;
            #[allow(clippy::cast_precision_loss)]
            let mut x = rabs as f64 * WI_DOUBLE[idx];
            if sign & 0x1 == 1 {
                x = -x;
            }
            if rabs < KI_DOUBLE[idx] {
                return x;
            }
            if idx == 0 {
                loop {
                    let xx = -ZIGGURAT_NOR_INV_R * (-self.next_double()).ln_1p();
                    let yy = -(-self.next_double()).ln_1p();
                    if yy + yy > xx * xx {
                        return if (rabs >> 8) & 0x1 == 1 { -(ZIGGURAT_NOR_R + xx) } else { ZIGGURAT_NOR_R + xx };
                    }
                }
            } else if (FI_DOUBLE[idx - 1] - FI_DOUBLE[idx]) * self.next_double() + FI_DOUBLE[idx] < (-0.5 * x * x).exp()
            {
                return x;
            }
        }
    }

    /// `Generator.normal(loc, scale, n)`.
    pub fn normal(&mut self, loc: f64, scale: f64, n: usize) -> Vec<f64> {
        (0..n).map(|_| loc + scale * self.standard_normal()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_zero_first_draws_are_numpys() {
        // np.random.default_rng(0).standard_normal(3)
        let mut rng = Pcg64::seed0();
        let draws: Vec<f64> = (0..3).map(|_| rng.standard_normal()).collect();
        assert_eq!(draws, vec![0.125_730_221_093_393_3, -0.132_104_863_291_301_9, 0.640_422_650_443_282_1]);
    }
}
