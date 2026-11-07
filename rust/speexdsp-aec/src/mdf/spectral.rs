//! The static helpers of `mdf.c`: the DC notch, the inner product, power spectra
//! and the spectral products of the filter, in the packed layout of
//! [`SpeexFft`](crate::fft::SpeexFft).
//!
//! All sums are 32-bit and wrap, as in C. A wrapping sum does not depend on the
//! order of its terms, which is what lets stage 3 reorder these loops for SIMD
//! without changing a single output bit.

use crate::fixed::{
    abs32, div32, extract16, mac16_16, mult16_16, mult16_16_q, mult16_32_q15, pshr32, q15, saturate, shl32, to16,
};
use crate::math::sqrt;
use crate::pseudo_float::PFloat;

/// Filter weights are kept in Q(15 + `WEIGHT_SHIFT`).
pub(super) const WEIGHT_SHIFT: u32 = 11;

/// `filter_dc_notch16`: remove DC (and, at a too small `radius`, much more) from
/// the microphone. `mem` is the filter's two-sample state.
pub(super) fn filter_dc_notch16(input: &[i16], radius: i16, out: &mut [i16], mem: &mut [i32; 2]) {
    let radius = radius as i32;
    let den2 = to16(
        mult16_16_q(radius, radius, 15)
            + mult16_16_q(q15::POINT_7, mult16_16_q(32767 - radius, 32767 - radius, 15), 15),
    );
    for (o, &vin) in out.iter_mut().zip(input) {
        let vin = vin as i32;
        let vout = mem[0].wrapping_add(shl32(vin, 15));
        mem[0] = mem[1].wrapping_add(shl32(shl32(-vin, 15).wrapping_add(mult16_32_q15(radius, vout)), 1));
        mem[1] = shl32(vin, 15).wrapping_sub(mult16_32_q15(den2, vout));
        *o = saturate(pshr32(mult16_32_q15(radius, vout), 15), 32767) as i16;
    }
}

/// `mdf_inner_prod`: Σ x·y over pairs of samples, each pair shifted down by 6
/// before it is added. An odd last sample is ignored.
pub(super) fn inner_prod(x: &[i16], y: &[i16]) -> i32 {
    x.as_chunks::<2>().0.iter().zip(y.as_chunks::<2>().0.iter()).fold(0i32, |sum, (a, b)| {
        let part = mac16_16(mult16_16(a[0] as i32, b[0] as i32), a[1] as i32, b[1] as i32);
        sum.wrapping_add(part >> 6)
    })
}

/// `power_spectrum`: |X|² per bin (`N / 2 + 1` bins) into `ps`. C adds each
/// channel's spectrum into zeroed arrays (`power_spectrum_accum`); with one
/// channel that is this.
pub(super) fn power_spectrum(x: &[i16], ps: &mut [i32]) {
    let n = x.len();
    ps[0] = mult16_16(x[0] as i32, x[0] as i32);
    for (j, pair) in x[1..n - 1].as_chunks::<2>().0.iter().enumerate() {
        ps[j + 1] = mult16_16(pair[0] as i32, pair[0] as i32).wrapping_add(mult16_16(pair[1] as i32, pair[1] as i32));
    }
    ps[n / 2] = mult16_16(x[n - 1] as i32, x[n - 1] as i32);
}

/// `spectral_mul_accum` (32-bit weights, their top 16 bits used) and
/// `spectral_mul_accum16` (16-bit weights): Σ over partitions of X·W, into `acc`.
///
/// Each item of `partitions` is one far-end spectrum with its filter partition, `N`
/// packed values each; `sums` is scratch of length `N`.
///
/// C loops over bins outside and partitions inside, at stride `N`. Here the
/// partitions are outside and the bins inside, contiguous, which vectorises (the
/// interleaved re/im pairs load as one de-interleaving load). Every bin's sum is the
/// same set of 32-bit wrapping terms, and such a sum does not depend on the order of
/// its terms, so the result is bit for bit C's; only the final rounding shift is
/// order-sensitive, and it is applied once, as in C.
pub(super) fn spectral_mul_accum<'a, W: Copy + 'a>(
    partitions: impl Iterator<Item = (&'a [i16], &'a [W])>,
    acc: &mut [i16],
    sums: &mut [i32],
    top16: impl Fn(W) -> i32,
) {
    let n = acc.len();
    sums.fill(0);
    for (xp, wp) in partitions {
        sums[0] = mac16_16(sums[0], xp[0] as i32, top16(wp[0]));
        sums[n - 1] = mac16_16(sums[n - 1], xp[n - 1] as i32, top16(wp[n - 1]));
        let bins = sums[1..n - 1]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(xp[1..n - 1].as_chunks::<2>().0.iter())
            .zip(wp[1..n - 1].as_chunks::<2>().0.iter());
        for ((s, x), w) in bins {
            let (xr, xi, wr, wi) = (x[0] as i32, x[1] as i32, top16(w[0]), top16(w[1]));
            s[0] = s[0].wrapping_add(xr * wr).wrapping_sub(xi * wi);
            s[1] = s[1].wrapping_add(xi * wr).wrapping_add(xr * wi);
        }
    }
    for (a, &s) in acc.iter_mut().zip(sums.iter()) {
        *a = pshr32(s, WEIGHT_SHIFT) as i16;
    }
}

/// `weighted_spectral_mul_conj`: `prod = p · w · X · conj(Y)` per bin, where `w`
/// is the per-bin learning rate and `p` the partition's share of it.
pub(super) fn weighted_spectral_mul_conj(w: &[PFloat], p: PFloat, x: &[i16], y: &[i16], prod: &mut [i32]) {
    let n = prod.len();
    prod[0] = p.amult(w[0]).mul32(mult16_16(x[0] as i32, y[0] as i32));
    prod[n - 1] = p.amult(w[n / 2]).mul32(mult16_16(x[n - 1] as i32, y[n - 1] as i32));
    let bins = prod[1..n - 1]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(x[1..n - 1].as_chunks::<2>().0.iter())
        .zip(y[1..n - 1].as_chunks::<2>().0.iter())
        .zip(&w[1..n / 2]);
    for (((out, xs), ys), &rate) in bins {
        let weight = p.amult(rate);
        let (xr, xi, yr, yi) = (xs[0] as i32, xs[1] as i32, ys[0] as i32, ys[1] as i32);
        out[0] = weight.mul32(mac16_16(mult16_16(xr, yr), xi, yi));
        // `-X[i+1]` is an int: 32768 for -32768, cut back to -32768 by MULT16_16
        out[1] = weight.mul32(mac16_16(mult16_16(-xi, yr), xr, yi));
    }
}

/// `mdf_adjust_prop`: share of the learning rate per partition, from the energy
/// of its weights. `w` holds `prop.len()` partitions.
pub(super) fn adjust_prop(w: &[i32], prop: &mut [i16]) {
    let n = w.len() / prop.len();
    let mut max_sum: i16 = 1;
    let mut prop_sum: i32 = 1;
    for (p, partition) in prop.iter_mut().zip(w.chunks_exact(n)) {
        let mut tmp: i32 = 1;
        for &weight in partition {
            let top = extract16(weight >> 18) as i32;
            tmp = tmp.wrapping_add(mult16_16(top, top));
        }
        // "Just a security in case an overflow were to occur"
        tmp = abs32(tmp).min(536_870_912);
        *p = sqrt(tmp);
        max_sum = max_sum.max(*p);
    }
    let floor = mult16_16_q(q15::POINT_1, max_sum as i32, 15);
    for p in prop.iter_mut() {
        *p = (*p as i32 + floor) as i16;
        prop_sum = prop_sum.wrapping_add(*p as i32);
    }
    for p in prop.iter_mut() {
        *p = div32(mult16_16(q15::POINT_99, *p as i32), prop_sum) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_product_shifts_each_pair() {
        assert_eq!(inner_prod(&[64, 64, 1, 1], &[1, 1, 64, 64]), 2 + 2);
        // the odd last sample does not count
        assert_eq!(inner_prod(&[8, 8, 1000], &[8, 8, 1000]), (64 + 64) >> 6);
        assert_eq!(inner_prod(&[], &[]), 0);
    }

    #[test]
    fn power_spectrum_of_a_packed_frame() {
        let x = [3i16, 1, 2, -2, 4, 5];
        let mut ps = [10i32; 4];
        power_spectrum(&x, &mut ps);
        assert_eq!(ps, [9, 1 + 4, 4 + 16, 25]);
    }

    #[test]
    fn spectral_product_of_one_partition_is_a_complex_product() {
        // DC 2, bin (1 + 2i), Nyquist -3, times DC 1, bin (3 - i), Nyquist 2, in Q11
        let x = [2i16, 1, 2, -3];
        let w = [1i16 << 11, 3 << 11, -1 << 11, 2 << 11];
        let (mut acc, mut sums) = ([0i16; 4], [0i32; 4]);
        spectral_mul_accum(std::iter::once((&x[..], &w[..])), &mut acc, &mut sums, |v: i16| v as i32);
        assert_eq!(acc, [2, 5, 5, -6]);
        // the 32-bit variant uses the top 16 bits
        let w32: Vec<i32> = w.iter().map(|&v| (v as i32) << 16).collect();
        spectral_mul_accum(std::iter::once((&x[..], &w32[..])), &mut acc, &mut sums, |v: i32| v >> 16);
        assert_eq!(acc, [2, 5, 5, -6]);
        // two partitions add up before the rounding shift
        let twice = [(&x[..], &w[..]), (&x[..], &w[..])];
        spectral_mul_accum(twice.into_iter(), &mut acc, &mut sums, |v: i16| v as i32);
        assert_eq!(acc, [4, 10, 10, -12]);
    }

    #[test]
    fn notch_removes_dc() {
        let input = [1000i16; 4000];
        let mut out = [0i16; 4000];
        let mut mem = [0i32; 2];
        filter_dc_notch16(&input, q15::POINT_982 as i16, &mut out, &mut mem);
        assert!(out[3999].abs() <= 1, "DC left: {}", out[3999]);
        assert!(out[0] > 900, "the step passes at first: {}", out[0]);
    }

    #[test]
    fn prop_shares_sum_to_about_point_99() {
        let mut w = vec![0i32; 4 * 8];
        w[..8].iter_mut().for_each(|v| *v = 1 << 28);
        let mut prop = [0i16; 4];
        adjust_prop(&w, &mut prop);
        // prop_sum starts at 1 and each share truncates, so the total falls a little short
        let total: i32 = prop.iter().map(|&p| p as i32).sum();
        assert!(total <= 32440 && total > 32440 - 32, "{prop:?}");
        assert!(prop[0] > prop[1] && prop[1] == prop[3], "{prop:?}");
    }
}
