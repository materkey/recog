//! The complex mixed-radix FFT of `kiss_fft.c`, fixed-point build.
//!
//! Samples are 16-bit complex values. A forward transform divides by the length
//! stage by stage (each butterfly scales its inputs down), an inverse one does not.
//! The recursion of `kf_shuffle` / `kf_work` is kept as it is: it fixes the order
//! of every rounding, which is what bit-exactness depends on.

use super::butterfly::{bfly_generic, bfly2, bfly3, bfly4, bfly5};
use crate::fixed::{div32, pshr32, shl32};
use crate::math::cos_norm;

/// The largest radix `kf_bfly_generic` supports; speexdsp aborts above it.
pub(crate) const MAX_RADIX: usize = 17;

/// A 16-bit complex sample (`kiss_fft_cpx` with `kiss_fft_scalar = spx_int16_t`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cpx {
    pub(crate) r: i16,
    pub(crate) i: i16,
}

/// `sround(x)`: Q15 product back to 16 bits, rounding.
#[inline(always)]
pub(crate) const fn sround(x: i32) -> i16 {
    (x.wrapping_add(1 << 14) >> 15) as i16
}

/// `S_MUL(a, b)`.
#[inline(always)]
pub(crate) const fn s_mul(a: i16, b: i16) -> i16 {
    sround(a as i32 * b as i32)
}

impl Cpx {
    /// `C_MUL(m, a, b)`.
    #[inline(always)]
    pub(crate) const fn mul(self, b: Self) -> Self {
        let (ar, ai, br, bi) = (self.r as i32, self.i as i32, b.r as i32, b.i as i32);
        Self { r: sround((ar * br).wrapping_sub(ai * bi)), i: sround((ar * bi).wrapping_add(ai * br)) }
    }

    /// `C_MUL4(m, a, b)`: the product scaled down by 4 more.
    #[inline(always)]
    pub(crate) const fn mul4(self, b: Self) -> Self {
        let (ar, ai, br, bi) = (self.r as i32, self.i as i32, b.r as i32, b.i as i32);
        Self {
            r: pshr32((ar * br).wrapping_sub(ai * bi), 17) as i16,
            i: pshr32((ar * bi).wrapping_add(ai * br), 17) as i16,
        }
    }

    /// `C_ADD(res, a, b)`; also `C_ADDTO`.
    #[inline(always)]
    pub(crate) const fn add(self, b: Self) -> Self {
        Self { r: self.r.wrapping_add(b.r), i: self.i.wrapping_add(b.i) }
    }

    /// `C_SUB(res, a, b)`; also `C_SUBFROM`.
    #[inline(always)]
    pub(crate) const fn sub(self, b: Self) -> Self {
        Self { r: self.r.wrapping_sub(b.r), i: self.i.wrapping_sub(b.i) }
    }

    /// `C_FIXDIV(c, div)`: multiply by `SAMP_MAX / div` in Q15.
    #[inline(always)]
    pub(crate) const fn fixdiv(self, div: usize) -> Self {
        self.scale((32767 / div) as i16)
    }

    /// `C_MULBYSCALAR(c, s)`.
    #[inline(always)]
    pub(crate) const fn scale(self, s: i16) -> Self {
        Self { r: s_mul(self.r, s), i: s_mul(self.i, s) }
    }
}

/// `kf_cexp2`: the twiddle at a phase in Q17 of a full turn.
pub(crate) const fn cexp2(phase: i32) -> Cpx {
    Cpx { r: cos_norm(phase), i: cos_norm(phase - 32768) }
}

/// `kf_factor`: radices and remaining lengths, powers of 4 first, then 2, then odd
/// primes.
pub(crate) fn factor(n: usize) -> Vec<(usize, usize)> {
    let mut n = n as i64;
    let mut p: i64 = 4;
    let mut stages = Vec::new();
    loop {
        while n % p != 0 {
            p = match p {
                4 => 2,
                2 => 3,
                _ => p + 2,
            };
            // C also gives up above p = 32000, which a length below 16384 never reaches
            if p * p > n {
                p = n;
            }
        }
        n /= p;
        stages.push((p as usize, n as usize));
        if n <= 1 {
            return stages;
        }
    }
}

/// A row of twiddles split into real and imaginary parts.
#[derive(Clone, Debug, Default)]
pub(super) struct Split {
    pub(super) re: Vec<i16>,
    pub(super) im: Vec<i16>,
}

/// One stage of `kf_work`: `n` groups of radix-`p` butterflies over `m` points,
/// groups `mm` apart, twiddles every `fstride`.
#[derive(Clone, Debug)]
pub(super) struct Stage {
    pub(super) p: usize,
    pub(super) m: usize,
    pub(super) fstride: usize,
    pub(super) n: usize,
    pub(super) mm: usize,
    /// For radix 2 to 5, row `q − 1` holds `twiddles[q·j·fstride]` for `j < m`:
    /// the entries C reaches by stepping its twiddle pointers.
    pub(super) tw: Vec<Split>,
    /// `epi3` for radix 3; `ya`, `yb` for radix 5.
    pub(super) special: [Cpx; 2],
}

impl Stage {
    fn new(p: usize, m: usize, fstride: usize, mm: usize, twiddles: &[Cpx]) -> Self {
        let tw = if (2..=5).contains(&p) {
            (1..p)
                .map(|q| {
                    let (re, im) = (0..m).map(|j| twiddles[q * j * fstride]).map(|c| (c.r, c.i)).unzip();
                    Split { re, im }
                })
                .collect()
        } else {
            Vec::new()
        };
        let special = match p {
            3 => [twiddles[fstride * m], Cpx::default()],
            5 => [twiddles[fstride * m], twiddles[fstride * 2 * m]],
            _ => [Cpx::default(); 2],
        };
        Self { p, m, fstride, n: fstride, mm, tw, special }
    }
}

/// A complex FFT plan: `kiss_fft_state`.
///
/// C's `kf_shuffle` and `kf_work` recurse over the factors on every call. Both walk
/// a fixed pattern, so the plan records it once: `perm` is the input order the
/// shuffle produces and `stages` the butterfly passes in the order the recursion
/// runs them (deepest first), with the same strides. The arithmetic is unchanged.
#[derive(Clone, Debug)]
pub(crate) struct ComplexFft {
    pub(crate) inverse: bool,
    pub(crate) perm: Vec<u32>,
    stages: Vec<Stage>,
    pub(crate) twiddles: Vec<Cpx>,
}

impl ComplexFft {
    /// `kiss_fft_alloc(nfft, inverse)`. `nfft` must factor into radices of at most
    /// [`MAX_RADIX`] and stay below 16384 (callers validate it).
    pub(crate) fn new(nfft: usize, inverse: bool) -> Self {
        let factors = factor(nfft);
        let twiddles: Vec<Cpx> = (0..nfft)
            .map(|i| {
                let phase = if inverse { i as i32 } else { -(i as i32) };
                cexp2(div32(shl32(phase, 17), nfft as i32))
            })
            .collect();
        // kf_work(stage s) runs with fstride = n = Π p_t over t < s and mm = m_{s-1}
        let (mut stages, mut fstride, mut mm) = (Vec::with_capacity(factors.len()), 1, 1);
        for &(p, m) in &factors {
            stages.push(Stage::new(p, m, fstride, mm, &twiddles));
            fstride *= p;
            mm = m;
        }
        stages.reverse();
        let mut perm = vec![0u32; nfft];
        shuffle_order(&factors, &mut perm, 0, 0, 1, 0);
        Self { inverse, perm, stages, twiddles }
    }

    /// The butterfly passes of `kiss_fft` over samples already in [`perm`](Self)
    /// order, real parts in `re` and imaginary parts in `im`.
    pub(crate) fn run(&self, re: &mut [i16], im: &mut [i16]) {
        for s in &self.stages {
            match s.p {
                2 => bfly2(re, im, s, self.inverse),
                3 => bfly3(re, im, s, self.inverse),
                4 => bfly4(re, im, s, self.inverse),
                5 => bfly5(re, im, s, self.inverse),
                _ => bfly_generic(re, im, s, self.inverse, &self.twiddles),
            }
        }
    }

    /// `kiss_fft(cfg, fin, fout)` on interleaved samples, for the tests.
    #[cfg(test)]
    pub(crate) fn transform(&self, fin: &[Cpx], fout: &mut [Cpx]) {
        let (mut re, mut im): (Vec<i16>, Vec<i16>) =
            self.perm.iter().map(|&s| (fin[s as usize].r, fin[s as usize].i)).unzip();
        self.run(&mut re, &mut im);
        for (o, (r, i)) in fout.iter_mut().zip(re.into_iter().zip(im)) {
            *o = Cpx { r, i };
        }
    }
}

/// `kf_shuffle` over indices: `perm[out + j]` is the input sample that C copies to
/// `fout[out + j]`.
fn shuffle_order(factors: &[(usize, usize)], perm: &mut [u32], out: usize, at: usize, fstride: usize, stage: usize) {
    let (p, m) = factors[stage];
    let mut f = at;
    if m == 1 {
        for j in 0..p {
            perm[out + j] = f as u32;
            f += fstride;
        }
    } else {
        let mut o = out;
        for _ in 0..p {
            shuffle_order(factors, perm, o, f, fstride * p, stage + 1);
            f += fstride;
            o += m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_follow_kiss_order() {
        assert_eq!(factor(160), vec![(4, 40), (4, 10), (2, 5), (5, 1)]);
        assert_eq!(factor(240), vec![(4, 60), (4, 15), (3, 5), (5, 1)]);
        assert_eq!(factor(140), vec![(4, 35), (5, 7), (7, 1)]);
        assert_eq!(factor(2), vec![(2, 1)]);
        assert_eq!(factor(1), vec![(1, 1)]);
        assert_eq!(factor(19 * 4), vec![(4, 19), (19, 1)]);
    }

    fn dft(x: &[Cpx], inverse: bool) -> Vec<(f64, f64)> {
        let n = x.len() as f64;
        let sign = if inverse { 1.0 } else { -1.0 };
        (0..x.len())
            .map(|k| {
                x.iter().enumerate().fold((0.0, 0.0), |(re, im), (t, c)| {
                    let angle = sign * 2.0 * std::f64::consts::PI * (k * t) as f64 / n;
                    let (s, co) = angle.sin_cos();
                    (re + f64::from(c.r) * co - f64::from(c.i) * s, im + f64::from(c.r) * s + f64::from(c.i) * co)
                })
            })
            .collect()
    }

    fn check(n: usize, inverse: bool) {
        let fft = ComplexFft::new(n, inverse);
        // the inverse does not scale down, so its input must leave room for the sum
        let gain = if inverse { (8 / n).max(1) as i32 } else { 8 };
        let shrink = if inverse { n as i32 } else { 1 };
        let sample = |v: usize, modulus: usize| (((v % modulus) as i32 - modulus as i32 / 2) * gain / shrink) as i16;
        let input: Vec<Cpx> = (0..n).map(|t| Cpx { r: sample(t * 7919, 2001), i: sample(t * 104_729, 1501) }).collect();
        let mut out = vec![Cpx::default(); n];
        fft.transform(&input, &mut out);
        let scale = if inverse { 1.0 } else { 1.0 / n as f64 };
        let reference = dft(&input, inverse);
        let tolerance = if inverse { 1.0 + 0.5 * n as f64 } else { 6.0 };
        for (k, (got, want)) in out.iter().zip(&reference).enumerate() {
            let (er, ei) = ((f64::from(got.r) - want.0 * scale).abs(), (f64::from(got.i) - want.1 * scale).abs());
            assert!(er <= tolerance && ei <= tolerance, "n={n} inverse={inverse} bin {k}: {got:?} vs {want:?}");
        }
    }

    #[test]
    fn every_radix_matches_a_reference_dft() {
        for &n in &[2usize, 3, 4, 5, 7, 8, 12, 16, 17, 20, 40, 60, 64, 68, 80, 120, 128, 140, 160] {
            check(n, false);
        }
        for &n in &[2usize, 3, 4, 5, 7, 16, 60, 140] {
            check(n, true);
        }
    }
}
