//! `wavo_asr` for Python: GigaAM v3 e2e_rnnt on [wavo](https://crates.io/crates/wavo), the
//! model recog's PyTorch backend runs, on the GPU through wgpu (Metal on macOS).
//!
//! ```python
//! import numpy as np, wavo_asr
//! model = wavo_asr.Model()                       # gigaam-v3 from the Hugging Face cache
//! text, tokens = model.transcribe(pcm.astype("<f4").tobytes())
//! # tokens: [(piece, start_ms), ...], SentencePiece pieces, "▁" starts a word
//! ```
//!
//! `transcribe` runs without the interpreter lock; calls from several threads take turns
//! inside wavo.
#![forbid(unsafe_code)]

use std::ops::ControlFlow;
use std::path::PathBuf;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

/// The hub name of GigaAM v3 e2e_rnnt (Q8_0 GGUF) in wavo's model table.
const GIGAAM: &str = "gigaam-v3";

/// GigaAM v3 e2e_rnnt loaded on the GPU.
#[pyclass(module = "wavo_asr", frozen)]
struct Model {
    inner: wavo::Model,
}

fn error(e: wavo::Error) -> PyErr {
    match e {
        wavo::Error::AudioTooLong { .. } => PyValueError::new_err(e.to_string()),
        _ => PyRuntimeError::new_err(e.to_string()),
    }
}

fn samples(bytes: &[u8]) -> PyResult<Vec<f32>> {
    let (quads, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(PyValueError::new_err("pcm: the byte count is not a whole number of float32 samples"));
    }
    Ok(quads.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

#[pymethods]
impl Model {
    /// Loads the GGUF at `path`, or `gigaam-v3` from the Hugging Face cache (`HF_HOME`),
    /// downloading it there first when it is missing.
    #[new]
    #[pyo3(signature = (path = None))]
    fn new(py: Python<'_>, path: Option<PathBuf>) -> PyResult<Self> {
        py.detach(|| {
            let path = match path {
                Some(path) => path,
                None => {
                    let hub = wavo::hub::Hub::new();
                    match hub.find(GIGAAM) {
                        Some(path) => path,
                        None => hub.pull(GIGAAM, |_| ControlFlow::Continue(())).map_err(error)?,
                    }
                }
            };
            let inner = wavo::Model::load_with(&path, wavo::Backend::Gpu).map_err(error)?;
            Ok(Self { inner })
        })
    }

    /// The text of 16 kHz mono float32 little-endian samples in [-1, 1], and its tokens as
    /// `(piece, start_ms)`. Up to 37.5 s; longer audio raises ValueError.
    fn transcribe(&self, py: Python<'_>, pcm: &[u8]) -> PyResult<(String, Vec<(String, u32)>)> {
        let pcm = samples(pcm)?;
        let transcript = py.detach(|| self.inner.transcribe(&pcm)).map_err(error)?;
        let tokens = transcript.tokens.into_iter().map(|t| (t.piece, t.start_ms)).collect();
        Ok((transcript.text, tokens))
    }

    /// The GPU wavo runs on, such as `Apple M3 Pro Metal`.
    #[getter]
    fn device(&self) -> String {
        let d = self.inner.device();
        format!("{} {:?}", d.name, d.api)
    }
}

#[pymodule]
fn wavo_asr(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Model>()
}
