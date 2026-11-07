//! The butterflies of `kiss_fft.c`, fixed-point paths, over split real and
//! imaginary arrays (struct of arrays).
//!
//! Each pass does what its C counterpart does to `Fout`, element by element, so
//! the results are C's bit for bit. Only the layout differs: C keeps `{r, i}`
//! pairs and walks the twiddle table at stride `fstride`; here the parts are
//! separate and each stage has its twiddles in contiguous rows, so the inner loops
//! of radix 2 and 4 are independent lane by lane and vectorise.
//!
//! Forward transforms scale by 1/radix inside the butterfly: radix 2 and 4 with
//! their own shift arithmetic, radix 3, 5 and the generic one with `C_FIXDIV`.

use super::complex::{Cpx, Stage, s_mul};
use crate::fixed::{add32, mult16_16, pshr32, shl32, sub32};

/// `PSHR16(x, 2)` stored back into 16 bits.
#[inline(always)]
const fn quarter(x: i16) -> i16 {
    ((x as i32 + 2) >> 2) as i16
}

#[inline(always)]
fn load(re: &[i16], im: &[i16], k: usize) -> Cpx {
    Cpx { r: re[k], i: im[k] }
}

#[inline(always)]
fn store(re: &mut [i16], im: &mut [i16], k: usize, c: Cpx) {
    re[k] = c.r;
    im[k] = c.i;
}

/// `kf_bfly2`: `n` groups `mm` apart, each with `m` butterflies.
pub(super) fn bfly2(re: &mut [i16], im: &mut [i16], s: &Stage, inverse: bool) {
    let m = s.m;
    let (wr, wi) = (&s.tw[0].re[..m], &s.tw[0].im[..m]);
    for g in 0..s.n {
        let base = g * s.mm;
        let (ar, br) = re[base..base + 2 * m].split_at_mut(m);
        let (ai, bi) = im[base..base + 2 * m].split_at_mut(m);
        if inverse {
            for j in 0..m {
                let a = Cpx { r: ar[j], i: ai[j] };
                let t = Cpx { r: br[j], i: bi[j] }.mul(Cpx { r: wr[j], i: wi[j] });
                let (low, high) = (a.add(t), a.sub(t));
                (ar[j], ai[j], br[j], bi[j]) = (low.r, low.i, high.r, high.i);
            }
        } else {
            for j in 0..m {
                // the same as below with the input halved, keeping the precision
                let (b_r, b_i, w_r, w_i) = (br[j] as i32, bi[j] as i32, wr[j] as i32, wi[j] as i32);
                let tr = sub32(mult16_16(b_r, w_r), mult16_16(b_i, w_i)) >> 1;
                let ti = add32(mult16_16(b_i, w_r), mult16_16(b_r, w_i)) >> 1;
                let (a_r, a_i) = (shl32(ar[j] as i32, 14), shl32(ai[j] as i32, 14));
                br[j] = pshr32(sub32(a_r, tr), 15) as i16;
                bi[j] = pshr32(sub32(a_i, ti), 15) as i16;
                ar[j] = pshr32(add32(a_r, tr), 15) as i16;
                ai[j] = pshr32(add32(a_i, ti), 15) as i16;
            }
        }
    }
}

/// Four consecutive runs of `m` samples.
fn quarters(x: &mut [i16], m: usize) -> [&mut [i16]; 4] {
    let (q0, rest) = x.split_at_mut(m);
    let (q1, rest) = rest.split_at_mut(m);
    let (q2, q3) = rest.split_at_mut(m);
    [q0, q1, q2, q3]
}

/// `kf_bfly4`.
pub(super) fn bfly4(re: &mut [i16], im: &mut [i16], s: &Stage, inverse: bool) {
    if inverse {
        bfly4_pass::<true>(re, im, s);
    } else {
        bfly4_pass::<false>(re, im, s);
    }
}

fn bfly4_pass<const INVERSE: bool>(re: &mut [i16], im: &mut [i16], s: &Stage) {
    let m = s.m;
    let tw = |q: usize, j: usize| Cpx { r: s.tw[q].re[j], i: s.tw[q].im[j] };
    for g in 0..s.n {
        let base = g * s.mm;
        let [r0, r1, r2, r3] = quarters(&mut re[base..base + 4 * m], m);
        let [i0, i1, i2, i3] = quarters(&mut im[base..base + 4 * m], m);
        for j in 0..m {
            let (f1, f2, f3) = (Cpx { r: r1[j], i: i1[j] }, Cpx { r: r2[j], i: i2[j] }, Cpx { r: r3[j], i: i3[j] });
            let mut f0 = Cpx { r: r0[j], i: i0[j] };
            let (s0, s1, s2) = if INVERSE {
                (f1.mul(tw(0, j)), f2.mul(tw(1, j)), f3.mul(tw(2, j)))
            } else {
                f0 = Cpx { r: quarter(f0.r), i: quarter(f0.i) };
                // C also quarters Fout[m2] here, a store it overwrites right after
                (f1.mul4(tw(0, j)), f2.mul4(tw(1, j)), f3.mul4(tw(2, j)))
            };
            let s5 = f0.sub(s1);
            let f0 = f0.add(s1);
            let (s3, s4) = (s0.add(s2), s0.sub(s2));
            let (out0, out2) = (f0.add(s3), f0.sub(s3));
            let (out1, out3) = if INVERSE {
                (
                    Cpx { r: s5.r.wrapping_sub(s4.i), i: s5.i.wrapping_add(s4.r) },
                    Cpx { r: s5.r.wrapping_add(s4.i), i: s5.i.wrapping_sub(s4.r) },
                )
            } else {
                (
                    Cpx { r: s5.r.wrapping_add(s4.i), i: s5.i.wrapping_sub(s4.r) },
                    Cpx { r: s5.r.wrapping_sub(s4.i), i: s5.i.wrapping_add(s4.r) },
                )
            };
            (r0[j], i0[j], r1[j], i1[j]) = (out0.r, out0.i, out1.r, out1.i);
            (r2[j], i2[j], r3[j], i3[j]) = (out2.r, out2.i, out3.r, out3.i);
        }
    }
}

/// `kf_bfly3`.
pub(super) fn bfly3(re: &mut [i16], im: &mut [i16], s: &Stage, inverse: bool) {
    let m = s.m;
    let epi3 = s.special[0];
    for g in 0..s.n {
        let base = g * s.mm;
        for k in 0..m {
            let a = base + k;
            let (mut f0, mut f1, mut f2) = (load(re, im, a), load(re, im, a + m), load(re, im, a + 2 * m));
            if !inverse {
                (f0, f1, f2) = (f0.fixdiv(3), f1.fixdiv(3), f2.fixdiv(3));
            }
            let s1 = f1.mul(Cpx { r: s.tw[0].re[k], i: s.tw[0].im[k] });
            let s2 = f2.mul(Cpx { r: s.tw[1].re[k], i: s.tw[1].im[k] });
            let s3 = s1.add(s2);
            let s0 = s1.sub(s2).scale(epi3.i);
            let half =
                Cpx { r: (f0.r as i32 - ((s3.r as i32) >> 1)) as i16, i: (f0.i as i32 - ((s3.i as i32) >> 1)) as i16 };
            store(re, im, a, f0.add(s3));
            store(re, im, a + 2 * m, Cpx { r: half.r.wrapping_add(s0.i), i: half.i.wrapping_sub(s0.r) });
            store(re, im, a + m, Cpx { r: half.r.wrapping_sub(s0.i), i: half.i.wrapping_add(s0.r) });
        }
    }
}

/// `kf_bfly5`.
pub(super) fn bfly5(re: &mut [i16], im: &mut [i16], s: &Stage, inverse: bool) {
    let m = s.m;
    let (ya, yb) = (s.special[0], s.special[1]);
    // sums of 16-bit terms, computed in int and stored into 16 bits
    let sum3 = |a: i16, b: i16, c: i16| (a as i32 + b as i32 + c as i32) as i16;
    for g in 0..s.n {
        let base = g * s.mm;
        for u in 0..m {
            let idx = [base + u, base + m + u, base + 2 * m + u, base + 3 * m + u, base + 4 * m + u];
            let mut f = [Cpx::default(); 5];
            for (c, &k) in f.iter_mut().zip(&idx) {
                let value = load(re, im, k);
                *c = if inverse { value } else { value.fixdiv(5) };
            }
            let tw = |q: usize| Cpx { r: s.tw[q].re[u], i: s.tw[q].im[u] };
            let s0 = f[0];
            let (s1, s2, s3, s4) = (f[1].mul(tw(0)), f[2].mul(tw(1)), f[3].mul(tw(2)), f[4].mul(tw(3)));
            let (s7, s10, s8, s9) = (s1.add(s4), s1.sub(s4), s2.add(s3), s2.sub(s3));
            let out0 = Cpx {
                r: (s0.r as i32 + (s7.r as i32 + s8.r as i32)) as i16,
                i: (s0.i as i32 + (s7.i as i32 + s8.i as i32)) as i16,
            };
            let s5 = Cpx {
                r: sum3(s0.r, s_mul(s7.r, ya.r), s_mul(s8.r, yb.r)),
                i: sum3(s0.i, s_mul(s7.i, ya.r), s_mul(s8.i, yb.r)),
            };
            let s6 = Cpx {
                r: (s_mul(s10.i, ya.i) as i32 + s_mul(s9.i, yb.i) as i32) as i16,
                i: (-(s_mul(s10.r, ya.i) as i32) - s_mul(s9.r, yb.i) as i32) as i16,
            };
            let s11 = Cpx {
                r: sum3(s0.r, s_mul(s7.r, yb.r), s_mul(s8.r, ya.r)),
                i: sum3(s0.i, s_mul(s7.i, yb.r), s_mul(s8.i, ya.r)),
            };
            let s12 = Cpx {
                r: (-(s_mul(s10.i, yb.i) as i32) + s_mul(s9.i, ya.i) as i32) as i16,
                i: (s_mul(s10.r, yb.i) as i32 - s_mul(s9.r, ya.i) as i32) as i16,
            };
            let outs = [out0, s5.sub(s6), s11.add(s12), s11.sub(s12), s5.add(s6)];
            for (&k, c) in idx.iter().zip(outs) {
                store(re, im, k, c);
            }
        }
    }
}

/// `kf_bfly_generic`: any radix up to 17, twiddles from the full table.
pub(super) fn bfly_generic(re: &mut [i16], im: &mut [i16], s: &Stage, inverse: bool, twiddles: &[Cpx]) {
    let (m, p, nfft) = (s.m, s.p, twiddles.len());
    let mut scratch = [Cpx::default(); super::complex::MAX_RADIX];
    for g in 0..s.n {
        let base = g * s.mm;
        for u in 0..m {
            for (q1, slot) in scratch.iter_mut().take(p).enumerate() {
                let value = load(re, im, base + u + q1 * m);
                *slot = if inverse { value } else { value.fixdiv(p) };
            }
            for q1 in 0..p {
                let k = u + q1 * m;
                let mut twidx = 0;
                let mut acc = scratch[0];
                for value in &scratch[1..p] {
                    twidx += s.fstride * k;
                    if twidx >= nfft {
                        twidx -= nfft;
                    }
                    acc = acc.add(value.mul(twiddles[twidx]));
                }
                store(re, im, base + k, acc);
            }
        }
    }
}
