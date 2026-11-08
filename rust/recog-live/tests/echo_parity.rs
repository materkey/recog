//! The echo stage against `recog/echo_cancel.py` on the same input
//! (`tests/parity/echo_parity.py` runs the Python side).
//!
//! Exact: numpy's sums, std, median, percentile and cumsum as ported, the window, the
//! measurement noise, the placer, the ring, the high-pass, the echo path decisions, and
//! the whole stage wherever it is raw. Within tolerance: what the FFTs and the
//! convolution touch (see `recog_live::echo`), with the numbers printed.

mod common;
mod echo_signals;

use common::{f32_b64, f32_from_b64, f64_b64, f64_from_b64, run_python};
use echo_signals::{Event, Normal, SRU, echo_of, events, feed, plain_events, speech_like};
use recog_live::echo::filters::{HighPass, to16};
use recog_live::echo::numpy::{cumsum, mean, median, percentile_linear, std, sum};
use recog_live::echo::random::Pcg64;
use recog_live::echo::suppressor::Suppressor;
use recog_live::echo::{
    BLOCK, EchoPathDetector, EchoStage, HIGH_PASS_HZ, ReferenceRing, SR, TimelinePlacer, inverse_low_cut_fir,
    peak_correlation,
};
use serde_json::{Value as Json, json};

fn python(request: &Json) -> Option<Json> {
    let python = common::python()?;
    let sandbox = tempfile::tempdir().unwrap();
    Some(run_python(&python, "echo_parity.py", request, sandbox.path()))
}

fn to_f64(x: &[f32]) -> Vec<f64> {
    x.iter().map(|&v| f64::from(v)).collect()
}

#[test]
fn random_arrays_numpy_reductions_compared_then_every_bit_is_numpys() {
    let sizes = [1, 2, 3, 7, 8, 9, 100, 128, 129, 136, 160, 257, 1280, 8192, 8200, 48_000, 100_003];
    let mut rng = Normal::new(11);
    let arrays: Vec<Vec<f64>> =
        sizes.iter().map(|&n| (0..n).map(|i| rng.sample(1.0) * 10f64.powi((i % 7) - 3)).collect()).collect();
    let request = json!({"op": "numpy", "arrays": arrays.iter().map(|a| f64_b64(a)).collect::<Vec<_>>()});
    let Some(answer) = python(&request) else { return };
    for (a, got) in arrays.iter().zip(answer["arrays"].as_array().unwrap()) {
        let scalars = f64_from_b64(got["scalars"].as_str().unwrap());
        let ours = [sum(a), mean(a), std(a), median(&mut a.clone()), percentile_linear(&mut a.clone(), 30.0)];
        for (name, (o, p)) in ["sum", "mean", "std", "median", "p30"].iter().zip(ours.iter().zip(&scalars)) {
            assert_eq!(o.to_bits(), p.to_bits(), "{name} of {} values: {o} against {p}", a.len());
        }
        assert_eq!(cumsum(a), f64_from_b64(got["cumsum"].as_str().unwrap()));
    }
    assert_eq!(Suppressor::new().window(), &f64_from_b64(answer["window"].as_str().unwrap())[..]);
}

#[test]
fn seed_zero_measurement_noise_compared_then_every_draw_is_numpys() {
    let n = 30 * SRU;
    let Some(answer) = python(&json!({"op": "normal", "n": n})) else { return };
    let ours = Pcg64::seed0().normal(0.0, 0.02, n);
    let theirs = f64_from_b64(answer["noise"].as_str().unwrap());
    let first_difference = ours.iter().zip(&theirs).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(first_difference, None);
}

/// Stamped blocks the way the captures deliver them: jitter, drift, holes of a few to
/// hundreds of samples, overlaps, an hour-long gap, varied lengths.
fn stamped_blocks(seed: u64) -> Vec<(Vec<f64>, f64)> {
    let mut rng = common::Rng::new(seed);
    let mut t = 1234.5678;
    let mut blocks = Vec::new();
    for k in 0..3000 {
        let n = [160, 341, 480, 1024, 7][rng.range(0, 5) as usize];
        let samples: Vec<f64> = (0..n).map(|i| f64::from(k % 13) + f64::from(i as u16) * 1e-3).collect();
        blocks.push((samples, t));
        let mut step = n as f64 / f64::from(SR) * (1.0 + (rng.uniform() - 0.5) * 2e-4);
        match rng.range(0, 40) {
            0 => step += rng.range(1, 12) as f64 / f64::from(SR),
            1 => step += rng.range(30, 900) as f64 / f64::from(SR),
            2 => step -= rng.range(1, 200) as f64 / f64::from(SR),
            3 if k % 500 == 7 => step += 3600.0,
            _ => step += (rng.uniform() - 0.5) * 2e-6,
        }
        t += step;
    }
    blocks
}

#[test]
fn stamped_block_sequences_placer_compared_then_placement_is_pythons_exactly() {
    for seed in [1, 2, 3] {
        let blocks = stamped_blocks(seed);
        let request = json!({"op": "placer", "blocks": blocks.iter().map(|(s, t)| json!({"samples": f64_b64(s), "t": t})).collect::<Vec<_>>()});
        let Some(answer) = python(&request) else { return };
        let mut placer = TimelinePlacer::new(SR);
        for (i, ((samples, t), theirs)) in blocks.iter().zip(answer["blocks"].as_array().unwrap()).enumerate() {
            let (placed, jumped) = placer.place(samples, *t);
            assert_eq!(placed, f64_from_b64(theirs["placed"].as_str().unwrap()), "seed {seed} block {i}");
            assert_eq!(jumped, theirs["jumped"].as_bool().unwrap(), "seed {seed} block {i}");
            assert_eq!(placer.next_position, theirs["next"].as_i64(), "seed {seed} block {i}");
        }
    }
}

#[test]
fn random_writes_and_reads_ring_compared_then_every_read_is_pythons() {
    let mut rng = common::Rng::new(7);
    let mut ops = Vec::new();
    let mut position: i64 = 5_000_000;
    for k in 0..2000 {
        if rng.range(0, 3) == 0 {
            let n = rng.range(0, 3000) as usize;
            let at = position + rng.range(-70_000, 5000);
            ops.push(json!({"op": "read", "position": at, "n": n}));
        } else {
            let n = [341, 1, 0, 70_000, 2000][rng.range(0, 5) as usize];
            let samples: Vec<f64> = (0..n).map(|i| f64::from(k) + f64::from(i as u32 % 1000) * 1e-4).collect();
            let at = position + rng.range(-500, 80_000);
            ops.push(json!({"op": "write", "position": at, "samples": f64_b64(&samples)}));
            position = at + n as i64;
        }
    }
    let Some(answer) = python(&json!({"op": "ring", "ops": ops})) else { return };
    let mut ring = ReferenceRing::new();
    for (i, (op, theirs)) in ops.iter().zip(answer["ops"].as_array().unwrap()).enumerate() {
        let at = op["position"].as_i64().unwrap();
        if op["op"] == "write" {
            ring.write(at, &f64_from_b64(op["samples"].as_str().unwrap()));
            assert_eq!(ring.end, theirs["end"].as_i64(), "op {i}");
        } else {
            let (samples, real) = ring.read(at, op["n"].as_u64().unwrap() as usize);
            assert_eq!(samples, f64_from_b64(theirs["samples"].as_str().unwrap()), "op {i}");
            assert_eq!(real, theirs["real"].as_bool().unwrap(), "op {i}");
        }
    }
}

#[test]
fn speech_blocks_high_pass_compared_then_every_bit_is_pythons() {
    let signal = to_f64(&speech_like(3.0, 4, 0.05));
    let blocks: Vec<Vec<f64>> = signal.chunks(1280).map(<[f64]>::to_vec).collect();
    let request = json!({"op": "highpass", "blocks": blocks.iter().map(|b| f64_b64(b)).collect::<Vec<_>>()});
    let Some(answer) = python(&request) else { return };
    let mut hp = HighPass::new(HIGH_PASS_HZ);
    for (block, theirs) in blocks.iter().zip(answer["blocks"].as_array().unwrap()) {
        assert_eq!(hp.block(block), f64_from_b64(theirs.as_str().unwrap()));
    }
}

#[test]
fn echo_and_other_signals_peak_correlation_compared_then_within_rounding_of_pythons() {
    let reference = to_f64(&speech_like(3.3, 1, 0.05));
    let echo = to_f64(&echo_of(&speech_like(3.3, 1, 0.05)));
    let other = to_f64(&speech_like(3.3, 2, 0.05));
    let window = 3 * SRU;
    let pairs = [(echo[echo.len() - window..].to_vec(), reference.clone()), (other[..window].to_vec(), reference)];
    let request = json!({"op": "correlation", "pairs": pairs.iter().map(|(m, r)| json!({"mic": f64_b64(m), "reference": f64_b64(r)})).collect::<Vec<_>>()});
    let Some(answer) = python(&request) else { return };
    let theirs = f64_from_b64(answer["peaks"].as_str().unwrap());
    for ((m, r), p) in pairs.iter().zip(&theirs) {
        let ours = peak_correlation(m, r);
        assert!((ours - p).abs() < 1e-12, "{ours} against {p}");
    }
}

#[test]
fn echo_silence_and_voice_detector_compared_then_presence_is_pythons_block_for_block() {
    let reference32 = speech_like(40.0, 1, 0.05);
    let reference = to_f64(&reference32);
    let echo = to_f64(&echo_of(&reference32));
    let voice = to_f64(&speech_like(40.0, 2, 0.02));
    let mut blocks = Vec::new();
    for i in (0..40 * SRU).step_by(BLOCK) {
        let (mic, reference) = match i / SRU {
            0..15 => (echo[i..i + BLOCK].to_vec(), reference[i..i + BLOCK].to_vec()),
            15..20 => (voice[i..i + BLOCK].to_vec(), vec![0.0; BLOCK]),
            _ => (voice[i..i + BLOCK].to_vec(), reference[i..i + BLOCK].to_vec()),
        };
        blocks.push((mic, reference));
    }
    let request = json!({"op": "detector", "blocks": blocks.iter().map(|(m, r)| json!({"mic": f64_b64(m), "reference": f64_b64(r)})).collect::<Vec<_>>()});
    let Some(answer) = python(&request) else { return };
    let mut detector = EchoPathDetector::new(SR);
    let ours: Vec<bool> = blocks
        .iter()
        .map(|(m, r)| {
            detector.feed(m, r);
            detector.present
        })
        .collect();
    let theirs: Vec<bool> = answer["present"].as_array().unwrap().iter().map(|v| v.as_bool().unwrap()).collect();
    assert_eq!(ours, theirs);
    assert!(ours.contains(&true) && !ours.last().unwrap());
}

fn python_taps() -> Option<Vec<f64>> {
    Some(f64_from_b64(python(&json!({"op": "inverse"}))?["taps"].as_str().unwrap()))
}

/// The taps differ only by the FFT and the solve: a few parts in 10^10 of the largest
/// tap, and the response by far less than 0.001 dB.
#[test]
fn inverse_filter_measured_by_both_then_the_taps_agree_to_rounding() {
    let Some(theirs) = python_taps() else { return };
    let ours = inverse_low_cut_fir(1024, 1e-3);
    let peak = theirs.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let worst = ours.iter().zip(&theirs).fold(0.0_f64, |m, (a, b)| m.max((a - b).abs()));
    eprintln!("inverse taps: max |Rust - Python| = {worst:e}, largest tap {peak:e}");
    assert!(worst < 1e-9 * peak, "{worst:e}");
}

/// The scenarios of the stage, each a stream of reference and microphone calls.
fn scenarios() -> Vec<(&'static str, Vec<Event>)> {
    let reference = speech_like(20.0, 1, 0.05);
    let echo = echo_of(&reference);
    let voice = speech_like(20.0, 2, 0.02);
    let quiet_voice = speech_like(20.0, 2, 0.012);
    let over: Vec<f32> =
        echo.iter().zip(&quiet_voice).enumerate().map(|(i, (e, v))| e + if i >= 12 * SRU { *v } else { 0.0 }).collect();
    let mut silent_later = reference.clone();
    silent_later[12 * SRU..].iter_mut().for_each(|v| *v = 0.0);
    let echo_silent_later: Vec<f32> = echo_of(&silent_later)
        .iter()
        .zip(&quiet_voice)
        .enumerate()
        .map(|(i, (e, v))| e + if i >= 13 * SRU { *v } else { 0.0 })
        .collect();

    let mut list = vec![
        ("no reference", plain_events(&voice[..5 * SRU], None, true)),
        ("output elsewhere", plain_events(&echo, Some(&reference), false)),
        ("reference nobody hears", plain_events(&voice, Some(&reference), true)),
        ("echo from the speakers", plain_events(&echo, Some(&reference), true)),
        ("voice over the echo", plain_events(&over, Some(&reference), true)),
        ("quiet reference", plain_events(&echo_silent_later, Some(&silent_later), true)),
    ];
    let away = |i: usize| (8 * SRU..13 * SRU).contains(&i);
    list.push((
        "output switch",
        events(&echo, Some(&reference), 160, 341, &|i| !away(i), &|i, b| {
            Some((i, if away(i) { vec![0.0; b.len()] } else { b.to_vec() }))
        }),
    ));
    let hole = 10 * SRU..10 * SRU + 640;
    list.push((
        "40 ms hole",
        events(&echo, Some(&reference), 160, 341, &|_| true, &|i, b| (!hole.contains(&i)).then(|| (i, b.to_vec()))),
    ));
    list.push((
        "10 sample hole",
        events(&echo, Some(&reference), 160, 341, &|_| true, &|i, b| {
            Some(if i == 10 * SRU { (i + 10, b[10..].to_vec()) } else { (i, b.to_vec()) })
        }),
    ));
    // An hour away mid-stream on both streams, then a flush as the stage is left.
    let mut jump = plain_events(&echo[..8 * SRU], Some(&reference[..8 * SRU]), true);
    for event in plain_events(&echo[8 * SRU..], Some(&reference[8 * SRU..]), true) {
        jump.push(match event {
            Event::Reference { samples, t, echo_possible } => {
                Event::Reference { samples, t: t + 3600.0 + 8.0, echo_possible }
            }
            Event::Mic { samples, t } => Event::Mic { samples, t: t + 3600.0 + 8.0 },
            Event::Flush => Event::Flush,
        });
    }
    jump.push(Event::Flush);
    list.push(("an hour away, then flushed", jump));
    list
}

fn event_json(event: &Event) -> Json {
    match event {
        Event::Reference { samples, t, echo_possible } => {
            json!({"kind": "ref", "samples": f32_b64(samples), "t": t, "echo_possible": echo_possible})
        }
        Event::Mic { samples, t } => json!({"kind": "mic", "samples": f32_b64(samples), "t": t}),
        Event::Flush => json!({"kind": "flush"}),
    }
}

/// given: the same reference and microphone calls, and Python's inverse taps
/// when: both stages run them
/// then: every call gives the same number of samples and the same active flag, every
/// sample of a stage that never cancelled is the same bit for bit, and cancelled audio
/// stays within rounding of Python's (the numbers are printed)
#[test]
fn same_streams_both_stages_run_then_raw_is_exact_and_cancelled_within_rounding() {
    let Some(taps) = python_taps() else { return };
    let scenarios = scenarios();
    let request = json!({
        "op": "stage",
        "taps": f64_b64(&taps),
        "scenarios": scenarios.iter().map(|(_, evs)| evs.iter().map(event_json).collect::<Vec<_>>()).collect::<Vec<_>>(),
    });
    let answer = python(&request).unwrap();
    for ((name, evs), theirs) in scenarios.iter().zip(answer["scenarios"].as_array().unwrap()) {
        let stage = EchoStage::with_inverse(taps.clone());
        let ours = feed(&stage, evs);
        let theirs: Vec<(Vec<f32>, bool)> = theirs
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (f32_from_b64(c["out"].as_str().unwrap()), c["active"].as_bool().unwrap()))
            .collect();
        assert_eq!(ours.len(), theirs.len(), "{name}");
        let mut ever_active = false;
        let (mut total, mut differing, mut lsb, mut worst) = (0usize, 0usize, 0usize, 0.0_f64);
        for (k, ((o, oa), (t, ta))) in ours.iter().zip(&theirs).enumerate() {
            assert_eq!(o.len(), t.len(), "{name}: call {k} length");
            assert_eq!(oa, ta, "{name}: call {k} active");
            ever_active |= *oa;
            for (a, b) in o.iter().zip(t) {
                total += 1;
                if a.to_bits() != b.to_bits() {
                    differing += 1;
                    worst = worst.max(f64::from((a - b).abs()));
                    if to16(f64::from(*a)) != to16(f64::from(*b)) {
                        lsb += 1;
                    }
                }
            }
        }
        eprintln!(
            "{name}: {total} samples, {differing} differ ({:.4}%), {lsb} at int16, max |diff| {worst:e}, active {ever_active}",
            100.0 * differing as f64 / total.max(1) as f64
        );
        if !ever_active {
            assert_eq!(differing, 0, "{name}: a stage that never cancelled is bit-exact");
        }
        // Measured: under 0.03% of the samples differ, by one float32 ulp, and none at
        // the int16 Speex sees, so Speex's adaptation is identical.
        assert!(worst < 1e-6, "{name}: max |diff| {worst:e}");
        assert_eq!(lsb, 0, "{name}: samples differing at int16");
        assert!(differing * 100 < total, "{name}: {differing} of {total} samples differ");
    }
}
