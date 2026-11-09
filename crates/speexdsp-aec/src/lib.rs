//! A safe Rust port of the acoustic echo canceller of speexdsp 1.2.1 (`mdf.c`, the
//! multidelay block frequency-domain adaptive filter), in the fixed-point build that
//! pyaec 1.0.1 ships and recog uses.
//!
//! Output is bit-exact with that build for the same input; see the crate's parity
//! harness. The port adds what C leaves to the caller: configurations speexdsp would
//! mishandle are refused, buffers are length-checked, and the sampling rate can be
//! set (pyaec never sets it, so its canceller runs with 8 kHz settings).
//!
//! ```
//! use speexdsp_aec::{Config, EchoCanceller};
//!
//! let mut aec = EchoCanceller::new(Config { sampling_rate: 16_000, ..Config::new(160, 4096) })?;
//! let (mic, far_end) = ([0i16; 160], [0i16; 160]);
//! let mut out = [0i16; 160];
//! aec.cancel(&mic, &far_end, &mut out)?;
//! # Ok::<(), speexdsp_aec::Error>(())
//! ```
//!
//! Derived from speexdsp 1.2.1 — Copyright 2002-2008 Xiph.org Foundation,
//! 2002-2008 Jean-Marc Valin, and the other holders listed in `LICENSE` — and from
//! KISS FFT, Copyright 2003-2004 Mark Borgerding and 2005-2007 Jean-Marc Valin;
//! redistributed under their BSD terms, reproduced in `LICENSE`.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
pub mod fft;
mod fixed;
mod math;
mod mdf;
mod pseudo_float;

pub use error::Error;
pub use mdf::{Config, DEFAULT_SAMPLING_RATE, EchoCanceller};
