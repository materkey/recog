//! KISS FFT as speexdsp builds it in fixed point (`kiss_fft.c`, `kiss_fftr.c`,
//! `fftwrap.c`).
//!
//! Twiddles come from the integer `spx_cos_norm`, never from libm, so every value
//! here is a pure function of the input bits.

mod butterfly;
mod complex;
mod real;

pub(crate) use complex::{MAX_RADIX, factor};
pub use real::SpeexFft;
