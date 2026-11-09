//! The MDF echo canceller of speexdsp's `mdf.c`: fixed point, two-path
//! (foreground/background) filter, AUMDF weight constraint.

mod adapt;
mod cancel;
mod settings;
mod spectral;
mod state;
mod two_path;

pub(crate) use settings::validate_half_size;
pub use settings::{Config, DEFAULT_SAMPLING_RATE};
pub use state::EchoCanceller;
