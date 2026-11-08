//! What the echo stage promises a user, the cases of `tests/test_echo_cancel.py` (the
//! timeline's own cases are unit tests of `echo::placer`):
//! - speech from the speakers leaves the microphone;
//! - without an echo the microphone stays exactly what it was: no reference, another
//!   output, a reference nobody hears, or a quiet reference;
//! - both streams are placed by their time stamps, holes filled with zeros.

mod echo_signals;

use echo_signals::{Event, SRU, START_S, delay_of, echo_of, events, feed, plain_events, rms, run, speech_like};
use recog_live::echo::numpy::{Complex64, rfft};
use recog_live::echo::{BLOCK, EchoPathDetector, EchoStage, SR, inverse_low_cut_fir, peak_correlation};

fn joined(calls: Vec<(Vec<f32>, bool)>) -> Vec<f32> {
    calls.into_iter().flat_map(|(out, _)| out).collect()
}

fn assert_raw(out: &[f32], mic: &[f32]) {
    let delay = delay_of(out, mic);
    assert_eq!(&out[delay..], &mic[..out.len() - delay], "the output is the delayed raw microphone");
}

/// given: a microphone hearing only the speakers
/// when: the stage has had time to settle
/// then: the level is far below the raw one, under the VAD threshold
#[test]
fn speakers_only_stage_settles_then_the_speech_leaves_the_microphone() {
    let reference = speech_like(30.0, 1, 0.05);
    let mic = echo_of(&reference);
    let (out, active) = run(&mic, Some(&reference), true);
    assert!(active);
    let tail = 10 * SRU;
    let level = rms(&out[out.len() - tail..]);
    assert!(level < rms(&mic[mic.len() - tail..]) * 0.2, "{level}");
    assert!(level < 0.005, "{level}");
}

/// given: the user speaks over the echo, at the level of a quiet voice
/// when: the stage runs
/// then: the voice keeps most of its level while the echo goes
#[test]
fn voice_over_the_echo_stage_runs_then_the_voice_stays() {
    let reference = speech_like(40.0, 1, 0.05);
    let mut voice = speech_like(40.0, 2, 0.012);
    voice[..25 * SRU].iter_mut().for_each(|v| *v = 0.0);
    let mic: Vec<f32> = echo_of(&reference).iter().zip(&voice).map(|(e, v)| e + v).collect();
    let (out, _) = run(&mic, Some(&reference), true);
    let tail = 10 * SRU;
    let expected = rms(&voice[voice.len() - tail..]);
    let got = rms(&out[out.len() - tail..]);
    assert!(got > expected * 0.6 && got < expected * 1.6, "{got} against {expected}");
}

/// given: the system capture is absent, so no reference ever comes
/// when: the microphone runs
/// then: nothing is held back for long and every sample is the raw one
#[test]
fn no_reference_microphone_runs_then_every_sample_is_the_raw_one() {
    let mic = speech_like(5.0, 3, 0.05);
    let (out, active) = run(&mic, None, true);
    assert!(!active);
    assert!(out.len() > mic.len() - SRU / 2);
    assert_raw(&out, &mic);
}

/// given: the output is headphones, so the reference plays and nobody hears it
/// when: an echo-looking signal is at the microphone
/// then: the stage does not touch it
#[test]
fn output_elsewhere_echo_looking_signal_then_the_stage_does_not_touch_it() {
    let reference = speech_like(10.0, 1, 0.05);
    let mic = echo_of(&reference);
    let (out, active) = run(&mic, Some(&reference), false);
    assert!(!active);
    assert_raw(&out, &mic);
}

/// given: built-in speakers are the default, but the call plays elsewhere: the
/// reference plays and the microphone hears only a voice
/// when: the stage runs
/// then: no echo path is found and the voice is untouched
#[test]
fn reference_nobody_hears_stage_runs_then_the_voice_is_untouched() {
    let reference = speech_like(15.0, 1, 0.05);
    let mic = speech_like(15.0, 2, 0.02);
    let (out, active) = run(&mic, Some(&reference), true);
    assert!(!active);
    assert_raw(&out, &mic);
}

/// given: the stage is settled on an echo, then the speakers go silent and the user speaks
/// when: the reference has been quiet long enough
/// then: the output is the raw microphone again, not the thinned Speex output
#[test]
fn quiet_reference_after_an_echo_long_enough_then_the_raw_voice_is_back() {
    let n = 40 * SRU;
    let mut reference = speech_like(40.0, 1, 0.05);
    reference[25 * SRU..].iter_mut().for_each(|v| *v = 0.0);
    let mut voice = speech_like(40.0, 2, 0.012);
    voice[..28 * SRU].iter_mut().for_each(|v| *v = 0.0);
    let mic: Vec<f32> = echo_of(&reference).iter().zip(&voice).map(|(e, v)| e + v).collect();
    let (out, active) = run(&mic, Some(&reference), true);
    assert!(active);
    let delay = delay_of(&out[29 * SRU..], &mic[29 * SRU..]);
    // out[i] is mic[i - delay]: the stage holds audio back
    let got = &out[35 * SRU + delay..n - 2 * SRU];
    let want = &mic[35 * SRU..n - 2 * SRU - delay];
    assert_eq!(got.len(), want.len());
    for (a, b) in got.iter().zip(want) {
        assert!((a - b).abs() <= 1e-6 + 1e-5 * b.abs(), "{a} against {b}");
    }
}

/// given: settled on the speakers, the output leaves for 10 s and comes back
/// when: the echo returns with the output
/// then: the first second back is already suppressed well, far better than from nothing
#[test]
fn output_switch_echo_returns_then_the_paused_filter_suppresses_at_once() {
    let reference = speech_like(50.0, 1, 0.05);
    let mic = echo_of(&reference);
    let away = |i: usize| (25 * SRU..35 * SRU).contains(&i);
    let evs = events(&mic, Some(&reference), 160, 341, &|i| !away(i), &|i, b| {
        Some((i, if away(i) { vec![0.0; b.len()] } else { b.to_vec() }))
    });
    let out = joined(feed(&EchoStage::new(), &evs));
    let back = &out[36 * SRU..38 * SRU];
    let (fresh, _) = run(&mic[..2 * SRU], Some(&reference[..2 * SRU]), true);
    assert!(rms(back) < rms(&fresh[fresh.len() - SRU..]), "{} against {}", rms(back), rms(&fresh[fresh.len() - SRU..]));
}

/// given: the microphone lost 40 ms, which audio closing up would turn into a shift
/// when: the next block arrives with its real time
/// then: the output stays as long as the time that passed, and the echo stays down
#[test]
fn hole_in_the_microphone_next_block_with_its_time_then_it_is_filled_and_the_echo_stays_down() {
    let reference = speech_like(30.0, 1, 0.05);
    let mic = echo_of(&reference);
    let hole = 20 * SRU..20 * SRU + 640;
    let evs =
        events(&mic, Some(&reference), 160, 341, &|_| true, &|i, b| (!hole.contains(&i)).then(|| (i, b.to_vec())));
    let out = joined(feed(&EchoStage::new(), &evs));
    assert!(out.len().abs_diff(mic.len()) < SRU / 2);
    assert!(rms(&out[out.len() - 5 * SRU..]) < 0.005);
}

/// given: the microphone lost 10 samples, as the child's capture does now and then
/// when: the blocks after it arrive with their real times
/// then: the echo stays down right after the hole instead of returning for seconds
#[test]
fn hole_shorter_than_a_millisecond_blocks_after_it_then_the_echo_stays_down() {
    let reference = speech_like(30.0, 1, 0.05);
    let mic = echo_of(&reference);
    let (hole_at, lost) = (20 * SRU, 10);
    let evs = events(&mic, Some(&reference), 160, 341, &|_| true, &|i, b| {
        Some(if i == hole_at { (i + lost, b[lost..].to_vec()) } else { (i, b.to_vec()) })
    });
    let out = joined(feed(&EchoStage::new(), &evs));
    let before = rms(&out[hole_at - 3 * SRU..hole_at]);
    let after = rms(&out[hole_at + SRU / 2..hole_at + 3 * SRU]);
    assert!(after < before * 2.0, "{after} against {before}");
}

#[test]
fn delayed_copy_correlation_sees_it_and_not_another_signal() {
    let reference: Vec<f64> = speech_like(3.3, 1, 0.05).iter().map(|&v| f64::from(v)).collect();
    let (window, lead) = (3 * SRU, (0.3 * f64::from(SR)) as usize);
    let mut delayed = vec![0.0; 800];
    delayed.extend(&reference);
    let noise = echo_signals::Normal::new(1).vec(1e-3, window);
    let echo: Vec<f64> = delayed[lead..lead + window].iter().zip(&noise).map(|(e, n)| e + n).collect();
    let other: Vec<f64> = speech_like(3.0, 2, 0.05).iter().map(|&v| f64::from(v)).collect();
    assert!(peak_correlation(&echo, &reference) > 0.5);
    assert!(peak_correlation(&other, &reference) < 0.1);
}

#[test]
fn echo_then_silence_then_a_voice_detector_enters_ignores_silence_and_leaves() {
    let mut detector = EchoPathDetector::new(SR);
    let reference32 = speech_like(60.0, 1, 0.05);
    let reference: Vec<f64> = reference32.iter().map(|&v| f64::from(v)).collect();
    let echo: Vec<f64> = echo_of(&reference32).iter().map(|&v| f64::from(v)).collect();
    let voice: Vec<f64> = speech_like(60.0, 2, 0.02).iter().map(|&v| f64::from(v)).collect();
    assert!(!detector.present);

    let mut entered_at = None;
    for i in (0..15 * SRU).step_by(BLOCK) {
        detector.feed(&echo[i..i + BLOCK], &reference[i..i + BLOCK]);
        if detector.present && entered_at.is_none() {
            entered_at = Some(i as f64 / f64::from(SR));
        }
    }
    assert!(entered_at.is_some_and(|t| t < 5.0), "{entered_at:?}");

    let silence = vec![0.0; BLOCK];
    let room = echo_signals::Normal::new(5).vec(7e-4, BLOCK);
    for _ in 0..30 * SRU / BLOCK {
        detector.feed(&room, &silence);
    }
    assert!(detector.present, "a silent reference says nothing about the path");

    let mut left_at = None;
    for i in (15 * SRU..40 * SRU).step_by(BLOCK) {
        detector.feed(&voice[i..i + BLOCK], &reference[i..i + BLOCK]);
        if !detector.present && left_at.is_none() {
            left_at = Some((i - 15 * SRU) as f64 / f64::from(SR));
        }
    }
    assert!(left_at.is_some_and(|t| t < 12.0), "{left_at:?}");
}

/// given: the filter measured from the canceller itself
/// then: it lifts the low band Speex cuts by about 14 dB, and is near 0 dB above 1 kHz
#[test]
fn measured_inverse_filter_lifts_what_speex_cuts_and_leaves_the_rest() {
    let h = inverse_low_cut_fir(1024, 1e-3);
    assert_eq!(h.len(), 1024);
    let response = rfft(&h, 16384);
    let db = |frequency: f64| {
        let k = (frequency * 16384.0 / f64::from(SR)).round() as usize;
        let z: Complex64 = response[k];
        20.0 * z.norm().log10()
    };
    assert!(db(150.0) > 8.0, "{}", db(150.0));
    assert!(db(2000.0).abs() < 1.5, "{}", db(2000.0));
}

#[test]
fn stage_left_midway_flush_raw_gives_back_everything_held() {
    let mic = speech_like(1.0, 3, 0.05);
    let stage = EchoStage::new();
    let mut calls = feed(&stage, &plain_events(&mic, None, true));
    calls.extend(feed(&stage, &[Event::Flush]));
    let out = joined(calls);
    // the delay line's 256 samples come out too, after everything given
    assert_eq!(out.len(), mic.len() + 256);
    assert_eq!(&out[256..], &mic[..]);
}

/// given: the stage behind `Arc<dyn EchoSink>`, as the captures hold it
/// when: the system capture's thread feeds the reference while the microphone's feeds
/// the microphone
/// then: nothing blocks and every microphone sample comes back
#[test]
fn two_reader_threads_stage_shared_then_every_microphone_sample_comes_back() {
    use recog_live::echo::EchoSink;
    use std::sync::Arc;

    let reference = speech_like(5.0, 1, 0.05);
    let mic = echo_of(&reference);
    let sink: Arc<dyn EchoSink> = Arc::new(EchoStage::new());
    let feeder = {
        let sink = sink.clone();
        std::thread::spawn(move || {
            for (k, block) in reference.chunks(341).enumerate() {
                sink.add_reference(block, START_S + (k * 341) as f64 / f64::from(SR), true).unwrap();
            }
        })
    };
    let mut out = Vec::new();
    for (k, block) in mic.chunks(160).enumerate() {
        out.extend(sink.add_mic(block, START_S + (k * 160) as f64 / f64::from(SR)).unwrap());
    }
    feeder.join().unwrap();
    out.extend(sink.flush_raw());
    assert_eq!(out.len(), mic.len() + 256);
}
