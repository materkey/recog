//! What the captures call on the echo stage: `ChildMicCapture` hands it the
//! microphone's frames, `FramedSystemCapture` the reference, from two reader threads.

/// The echo stage as the captures see it.
///
/// Both captures hold it through an `Arc`, so the methods take `&self`; an `Err` is
/// the exception Python's capture catches, after which it leaves the stage.
pub trait EchoSink: Send + Sync {
    /// `EchoStage.add_mic`: whatever is ready of the microphone, in order.
    ///
    /// # Errors
    /// Any failure of the stage; the capture then goes on raw.
    fn add_mic(&self, samples: &[f32], time_s: f64) -> Result<Vec<f32>, String>;

    /// `EchoStage.flush_raw`: what is still held, raw, as the stage is left.
    fn flush_raw(&self) -> Vec<f32>;

    /// `EchoStage.active`: whether the last block was cancelled.
    fn active(&self) -> bool;

    /// `EchoStage.add_reference`.
    ///
    /// # Errors
    /// Any failure of the stage; the system capture then stops feeding it.
    fn add_reference(&self, samples: &[f32], time_s: f64, echo_possible: bool) -> Result<(), String>;
}
