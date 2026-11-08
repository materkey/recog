//! The live process of recog in Rust, lower layer (docs/plans/2026-11-07-rust-migration.md,
//! "Target layout"): the capture buffer and the capture child processes, echo
//! cancellation, ASR on wavo, the per-speaker ingest, the refine worker and the golden
//! recorder. The main loop (`recog/live.py`) and the bridge are not here yet.
//!
//! Each module ports one Python module and is held to it by the tests under `tests/`,
//! which run the Python implementation on the same input and compare the output. Where
//! Python and Rust legitimately differ, the module's documentation says so.
//!
//! Blocking work (reading a child's pipe, running the model) runs on its own threads,
//! as in Python: these loops wait on pipes and the GPU, not on sockets, and the tokio
//! main loop will drive them through their methods.
#![forbid(unsafe_code)]

pub mod asr;
pub mod capture;
pub mod clock;
pub mod config;
pub mod echo;
pub mod golden;
pub mod log;
pub mod per_speaker;
pub mod refine;
