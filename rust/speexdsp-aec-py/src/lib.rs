//! `speexdsp_aec` for Python: the echo canceller of the `speexdsp-aec` crate.
//!
//! ```python
//! import numpy as np, speexdsp_aec
//! aec = speexdsp_aec.EchoCanceller(160, 4096)          # pyaec's 8 kHz settings
//! out = np.frombuffer(aec.cancel(mic.tobytes(), ref.tobytes()), dtype=np.int16)
//! ```
//!
//! `cancel` takes any whole number of frames of 16-bit little-endian samples and runs
//! them without the interpreter lock.
#![forbid(unsafe_code)]

use aec::{Config, DEFAULT_SAMPLING_RATE};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// Speex's MDF echo canceller (fixed point), bit-exact with pyaec for the same settings.
#[pyclass(module = "speexdsp_aec")]
struct EchoCanceller {
    inner: aec::EchoCanceller,
}

fn samples(name: &str, bytes: &[u8]) -> PyResult<Vec<i16>> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(PyValueError::new_err(format!("{name}: an odd number of bytes is not 16-bit audio")));
    }
    Ok(pairs.iter().map(|b| i16::from_le_bytes(*b)).collect())
}

#[pymethods]
impl EchoCanceller {
    /// `frame_size` samples per frame, an echo of up to `filter_length` samples, constants
    /// tuned for `sampling_rate` (8000 by default, the rate pyaec always runs with).
    #[new]
    #[pyo3(signature = (frame_size, filter_length, sampling_rate = DEFAULT_SAMPLING_RATE))]
    fn new(frame_size: usize, filter_length: usize, sampling_rate: u32) -> PyResult<Self> {
        let config = Config { frame_size, filter_length, sampling_rate };
        aec::EchoCanceller::new(config).map(|inner| Self { inner }).map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// The microphone with the echo of `far_end` taken out, both whole frames of int16 LE.
    fn cancel<'py>(&mut self, py: Python<'py>, mic: &[u8], far_end: &[u8]) -> PyResult<Bound<'py, PyBytes>> {
        let (mic, far_end) = (samples("mic", mic)?, samples("far_end", far_end)?);
        let n = self.inner.frame_size();
        if mic.len() != far_end.len() || !mic.len().is_multiple_of(n) {
            return Err(PyValueError::new_err(format!(
                "mic and far_end must be the same whole number of {n}-sample frames, got {} and {}",
                mic.len(),
                far_end.len()
            )));
        }
        let inner = &mut self.inner;
        let out = py.detach(move || {
            let mut out = vec![0i16; mic.len()];
            for ((m, f), o) in mic.chunks_exact(n).zip(far_end.chunks_exact(n)).zip(out.chunks_exact_mut(n)) {
                inner.cancel(m, f, o).expect("frames are checked above");
            }
            out
        });
        let bytes: Vec<u8> = out.iter().flat_map(|s| s.to_le_bytes()).collect();
        Ok(PyBytes::new(py, &bytes))
    }

    /// Back to the state of a new canceller with the same settings.
    fn reset(&mut self) {
        self.inner.reset();
    }

    #[getter]
    fn frame_size(&self) -> usize {
        self.inner.frame_size()
    }

    #[getter]
    fn sampling_rate(&self) -> u32 {
        self.inner.sampling_rate()
    }
}

#[pymodule]
fn speexdsp_aec(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<EchoCanceller>()
}
