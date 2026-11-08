//! The live process of recog in Rust (docs/plans/2026-11-07-rust-migration.md,
//! "Target layout"): the capture buffer and the capture child processes, echo
//! cancellation, ASR on wavo, the per-speaker ingest, the refine worker, the golden
//! recorder, and over them the main loop of `recog/live.py` ([`live`]) with the bridge
//! of `recog-bridge` in the same process, and the command line of `recog/cli.py`
//! ([`cli`], the binary `recog`).
//!
//! Each module ports one Python module and is held to it by the tests under `tests/`,
//! which run the Python implementation on the same input and compare the output; the
//! main loop is held to Python by golden replay (`tests/replay_parity.rs`). Where
//! Python and Rust legitimately differ, the module's documentation says so.
//!
//! Blocking work (reading a child's pipe, running the model, the main loop's 100 ms
//! polls) runs on its own threads, as in Python; the bridge's sockets run on its tokio
//! runtime thread.
#![forbid(unsafe_code)]

pub mod asr;
pub mod capture;
pub mod cli;
pub mod clock;
pub mod config;
pub mod echo;
pub mod golden;
pub mod live;
pub mod log;
pub mod per_speaker;
pub mod refine;
