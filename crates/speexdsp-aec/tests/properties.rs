//! Properties of the public API under arbitrary input (proptest).
//!
//! The test profile keeps overflow checks on: any arithmetic the port does not
//! wrap explicitly would panic here instead of silently diverging from C.

use proptest::prelude::*;
use speexdsp_aec::fft::SpeexFft;
use speexdsp_aec::{Config, EchoCanceller, Error};

/// Frame sizes covering every radix the fixed-point FFT has (2, 3, 4, 5, generic 7).
const FRAMES: [usize; 11] = [8, 12, 16, 20, 40, 60, 64, 80, 120, 140, 160];

/// Samples biased towards the edges C mishandles: full scale, -32768, near zero.
fn samples(len: usize) -> impl Strategy<Value = Vec<i16>> {
    prop::collection::vec(
        prop_oneof![4 => any::<i16>(), 1 => Just(i16::MIN), 1 => Just(i16::MAX), 4 => -64i16..64],
        len,
    )
}

/// A valid configuration with a few frames of microphone and far end.
fn case() -> impl Strategy<Value = (Config, Vec<i16>, Vec<i16>)> {
    let rates = prop::sample::select(vec![8000u32, 11025, 16000, 22050, 32000]);
    (0..FRAMES.len(), 1usize..3000, rates, 1usize..16).prop_flat_map(|(f, filter_length, sampling_rate, count)| {
        let n = FRAMES[f];
        let config = Config { frame_size: n, filter_length, sampling_rate };
        (Just(config), samples(n * count), samples(n * count))
    })
}

fn run(config: Config, mic: &[i16], far: &[i16]) -> Vec<i16> {
    let mut aec = EchoCanceller::new(config).unwrap();
    let n = config.frame_size;
    let mut out = vec![0i16; mic.len()];
    for ((m, f), o) in mic.chunks_exact(n).zip(far.chunks_exact(n)).zip(out.chunks_exact_mut(n)) {
        aec.cancel(m, f, o).unwrap();
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn the_canceller_is_total_and_deterministic((config, mic, far) in case()) {
        let first = run(config, &mic, &far);
        let second = run(config, &mic, &far);
        prop_assert_eq!(first.len(), mic.len());
        prop_assert_eq!(first, second);
    }

    #[test]
    fn a_silent_microphone_stays_silent((config, _mic, far) in case()) {
        let silence = vec![0i16; far.len()];
        prop_assert!(run(config, &silence, &far).iter().all(|&v| v == 0));
    }

    #[test]
    fn a_refused_call_leaves_the_state_alone((config, mic, far) in case(), cut in 1usize..8) {
        let n = config.frame_size;
        let mut probe = EchoCanceller::new(config).unwrap();
        let mut out = vec![0i16; mic.len()];
        for (k, ((m, f), o)) in mic.chunks_exact(n).zip(far.chunks_exact(n)).zip(out.chunks_exact_mut(n)).enumerate() {
            if k == 1 {
                let short = &m[..n - cut.min(n - 1)];
                let refused = probe.cancel(short, f, o);
                prop_assert!(
                    matches!(refused, Err(Error::FrameLength { .. })),
                    "a short frame must be refused, got {:?}",
                    refused
                );
            }
            probe.cancel(m, f, o).unwrap();
        }
        prop_assert_eq!(out, run(config, &mic, &far));
    }

    #[test]
    fn configuration_checks_never_panic(frame_size in 0usize..20_000, filter_length in 0usize..200_000, rate in 0u32..70_000) {
        match EchoCanceller::new(Config { frame_size, filter_length, sampling_rate: rate }) {
            Ok(aec) => {
                prop_assert_eq!(aec.frame_size(), frame_size);
                prop_assert!(aec.filter_length() >= filter_length);
                prop_assert_eq!(aec.sampling_rate(), rate);
                prop_assert!(!aec.is_adapted());
            }
            Err(e) => prop_assert!(!e.to_string().is_empty()),
        }
    }

    /// `spx_fft` scales its input up in place and back down: without -32768 the
    /// caller gets the frame back unchanged, whatever the spectrum. (A round trip
    /// through `spx_ifft` is not a property of the C code: its butterflies can
    /// overflow on a frame scaled near full scale, which `tests/fft_parity.rs`
    /// checks bit for bit against C instead.)
    #[test]
    fn spx_fft_gives_its_input_back(f in 0..FRAMES.len(), seed in prop::collection::vec(-32767i16..=32767, 320)) {
        let n = 2 * FRAMES[f];
        let mut fft = SpeexFft::new(n).unwrap();
        let mut input: Vec<i16> = seed.iter().copied().cycle().take(n).collect();
        let original = input.clone();
        let mut spectrum = vec![0i16; n];
        fft.forward(&mut input, &mut spectrum);
        prop_assert_eq!(input, original);
    }
}

#[test]
fn rates_and_lengths_are_reported() {
    let mut aec = EchoCanceller::new(Config::new(160, 4096)).unwrap();
    assert_eq!((aec.frame_size(), aec.filter_length(), aec.sampling_rate()), (160, 4160, 8000));
    aec.set_sampling_rate(16_000).unwrap();
    assert!(matches!(aec.set_sampling_rate(0), Err(Error::UnsupportedSamplingRate { .. })));
    assert_eq!(aec.sampling_rate(), 16_000, "a refused rate leaves the old one");
    let err = aec.cancel(&[0; 160], &[0; 160], &mut [0; 80]).unwrap_err();
    assert_eq!(err, Error::FrameLength { expected: 160, mic: 160, far_end: 160, out: 80 });
    let err = aec.cancel(&[0; 160], &[0; 100], &mut [0; 160]).unwrap_err();
    assert_eq!(err, Error::FrameLength { expected: 160, mic: 160, far_end: 100, out: 160 });
}
