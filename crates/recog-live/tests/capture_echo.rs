//! The captures' frame decoders wired to the real echo stage, as `create_system_capture`
//! and `ChildMicCapture` wire them: the stage's own tests feed it directly.

use std::sync::Arc;

use recog_live::capture::framed::{MicDecoder, ReferenceDecoder, encode_mic_frame, encode_reference_frame};
use recog_live::capture::subprocess::{DecodeContext, Decoder as _};
use recog_live::clock::ManualClock;
use recog_live::echo::{EchoSink, EchoStage};
use recog_live::log::Logger;

#[test]
fn no_echo_possible_decoders_with_the_real_stage_then_the_microphone_comes_out_raw_and_whole() {
    // given: a stage both decoders share, a silent reference that cannot make an echo
    let stage: Arc<dyn EchoSink> = EchoStage::shared();
    let mut mic = MicDecoder::new(Some(stage.clone()));
    let mut reference = ReferenceDecoder::new(Some(stage.clone()));
    let clock = ManualClock::new(1_700_000_000.0);
    let (log, captured) = Logger::capture(clock.shared());
    let ctx = DecodeContext { wall: 1_700_000_000.0, silence_s: 0.0, name: "mic", sample_rate: 16_000, log: &log };
    #[allow(clippy::cast_precision_loss)]
    let input: Vec<f32> = (0..16_000 * 3).map(|i| ((i as f32) * 0.01).sin() * 0.1).collect();

    // when: three seconds of 10 ms frames on both pipes, then the stage is left
    let mut out = Vec::new();
    for (k, block) in input.chunks(160).enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let t = 100.0 + k as f64 * 0.01;
        let silent = vec![0.0f32; 160];
        let _ = reference.decode(&encode_reference_frame(t + 0.05, ctx.wall, false, &silent), &ctx);
        for (samples, _) in mic.decode(&encode_mic_frame(t, ctx.wall, block), &ctx) {
            out.extend(samples);
        }
    }
    let held = input.len() - out.len();
    out.extend(stage.flush_raw());

    // then: the delay line's zeros, then the microphone sample for sample
    assert!(held > 0, "the stage holds audio back for the reference");
    assert!(out[..256].iter().all(|&v| v == 0.0));
    assert_eq!(&out[256..], &input[..]);
    assert!(!stage.active());
    assert!(!captured.text().contains("Echo cancelling"), "{}", captured.text());
}
