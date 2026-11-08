//! The decoders against Python's `_decode_read`: the same byte streams, cut into reads
//! at random points, through `SubprocessPCMCapture`, `ChildMicCapture` (with no echo
//! stage and with a deterministic fake one that holds samples back, toggles `active`
//! and fails once) and `FramedSystemCapture`. Per read the batches must match (sha256
//! of the float32 samples, their count, the capture time as an exact `f64`), as must
//! every call the stage got and every log line.

mod common;

use std::sync::{Arc, Mutex};

use common::Rng;
use recog_live::capture::framed::{child_mic_capture, encode_mic_frame, encode_reference_frame, framed_system_capture};
use recog_live::capture::subprocess::SubprocessCapture;
use recog_live::clock::{Clock as _, ManualClock};
use recog_live::echo::EchoSink;
use recog_live::log::Logger;
use serde_json::{Value as Json, json};
use sha2::{Digest as _, Sha256};

const SR: u32 = 16_000;
const EPOCH: f64 = 1.762e9;

fn digest(samples: &[f32]) -> Json {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    json!({"len": samples.len(), "sha256": sha})
}

#[derive(Clone, Copy)]
struct StageSpec {
    hold_cycle: &'static [usize],
    fail_at: usize,
    active_period: usize,
}

struct StageState {
    held: Vec<f32>,
    calls: usize,
    active: bool,
    record: Vec<Json>,
}

/// The fake stage of tests/parity/capture_decoders.py, operation for operation.
struct Stage {
    spec: StageSpec,
    state: Mutex<StageState>,
}

impl Stage {
    fn new(spec: StageSpec) -> Arc<Self> {
        Arc::new(Self {
            spec,
            state: Mutex::new(StageState { held: Vec::new(), calls: 0, active: false, record: Vec::new() }),
        })
    }
}

impl EchoSink for Stage {
    fn add_mic(&self, samples: &[f32], time_s: f64) -> Result<Vec<f32>, String> {
        let mut s = self.state.lock().unwrap();
        s.calls += 1;
        if s.calls == self.spec.fail_at {
            return Err(format!("stage failed at call {}", s.calls));
        }
        s.record.push(json!(["mic", time_s, digest(samples)]));
        let mut joined = std::mem::take(&mut s.held);
        joined.extend(samples.iter().map(|x| x * 0.5));
        let hold = self.spec.hold_cycle[s.calls % self.spec.hold_cycle.len()].min(joined.len());
        s.held = joined.split_off(joined.len() - hold);
        s.active = (s.calls / self.spec.active_period) % 2 == 1;
        Ok(joined)
    }

    fn flush_raw(&self) -> Vec<f32> {
        std::mem::take(&mut self.state.lock().unwrap().held)
    }

    fn active(&self) -> bool {
        self.state.lock().unwrap().active
    }

    fn add_reference(&self, samples: &[f32], time_s: f64, echo_possible: bool) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        s.calls += 1;
        if s.calls == self.spec.fail_at {
            return Err(format!("stage failed at call {}", s.calls));
        }
        s.record.push(json!(["reference", time_s, echo_possible, digest(samples)]));
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Pcm,
    Mic,
    Reference,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Pcm => "pcm",
            Kind::Mic => "mic",
            Kind::Reference => "reference",
        }
    }
}

fn values(rng: &mut Rng, n: usize, zeros: bool) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation)]
    (0..n).map(|_| if zeros { 0.0 } else { (rng.uniform() * 2.0 - 1.0) as f32 }).collect()
}

/// The stream's bytes: raw PCM, or frames whose ADC time runs on with holes and jitter.
fn stream(kind: Kind, rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    let mut adc = 100.0 + rng.uniform();
    for _ in 0..rng.range(20, 120) {
        let count = usize::try_from(rng.range(1, 400)).unwrap();
        let zeros = rng.uniform() < 0.3;
        let samples = values(rng, count, zeros);
        match kind {
            Kind::Pcm => out.extend(samples.iter().flat_map(|s| s.to_le_bytes())),
            Kind::Mic => out.extend(encode_mic_frame(adc, EPOCH + adc - 50.0, &samples)),
            Kind::Reference => {
                out.extend(encode_reference_frame(adc, EPOCH + adc - 50.0, rng.uniform() < 0.5, &samples))
            }
        }
        #[allow(clippy::cast_precision_loss)]
        {
            adc += count as f64 / f64::from(SR);
        }
        let r = rng.uniform();
        if r < 0.15 {
            adc += 0.004 + rng.uniform() * 0.06;
        } else if r < 0.3 {
            adc += (rng.uniform() - 0.5) * 1e-4;
        }
    }
    out
}

/// Reads of the stream cut at random points, each with its wall and monotonic time;
/// now and then a respawn between them.
fn reads(kind: Kind, rng: &mut Rng) -> Vec<Json> {
    let data = stream(kind, rng);
    let mut reads = Vec::new();
    let mut at = 0;
    let mut mono = 0.0;
    while at < data.len() {
        let size = usize::try_from(rng.range(1, 3000)).unwrap().min(data.len() - at);
        // Silence clocks above and below ONLY_ZEROS_REPORT_S.
        mono += rng.uniform() * 4.0;
        if rng.uniform() < 0.05 {
            mono = 0.0;
        }
        reads.push(json!({"data": common::b64(&data[at..at + size]), "wall": EPOCH + mono, "mono": mono}));
        at += size;
        if rng.uniform() < 0.03 {
            reads.push(json!({"new_child": true}));
        }
    }
    reads
}

fn run_rust(kind: Kind, stage: Option<StageSpec>, reads: &[Json]) -> Json {
    let clock = ManualClock::new(EPOCH);
    let (log, captured) = Logger::capture(ManualClock::new(0.0).shared());
    let stage = stage.map(Stage::new);
    let sink = stage.clone().map(|s| s as Arc<dyn EchoSink>);
    let builder = match kind {
        Kind::Pcm => SubprocessCapture::builder(vec!["unused".into()], "pcm"),
        Kind::Mic => child_mic_capture(vec!["unused".into()], sink),
        Kind::Reference => match sink {
            Some(sink) => framed_system_capture(vec!["unused".into()], sink),
            None => panic!("a reference capture has a stage"),
        },
    };
    let capture = builder.sample_rate(SR).max_buffer_s(10.0).clock(clock.shared()).logger(log).build();
    capture.set_last_signal_at(0.0);
    let mut out = Vec::new();
    for read in reads {
        if read.get("new_child").is_some() {
            capture.on_new_child();
            out.push(Json::Null);
            continue;
        }
        let mono = read["mono"].as_f64().unwrap();
        clock.set(mono);
        assert_eq!(clock.wall(), read["wall"].as_f64().unwrap());
        let batches = capture.decode_read(&common::unb64(read["data"].as_str().unwrap()));
        out.push(Json::Array(
            batches
                .iter()
                .map(|(samples, at)| {
                    let mut d = digest(samples);
                    d["at"] = json!(at);
                    d
                })
                .collect(),
        ));
    }
    let stderr: String = captured.lines().iter().map(|l| format!("{l}\n")).collect();
    json!({
        "reads": out,
        "stage": stage.map(|s| Json::Array(s.state.lock().unwrap().record.clone())),
        "stderr": stderr,
    })
}

fn compare(kind: Kind, stage: Option<StageSpec>, seeds: u64) {
    let Some(python) = common::python() else { return };
    let mut batches = 0;
    let mut lines = 0;
    for seed in 0..seeds {
        let mut rng = Rng::new(seed * 31 + kind as u64);
        let reads = reads(kind, &mut rng);
        let request = json!({
            "kind": kind.name(),
            "sample_rate": SR,
            "stage": stage.map(|s| json!({"hold_cycle": s.hold_cycle, "fail_at": s.fail_at, "active_period": s.active_period})),
            "reads": reads,
        });
        let sandbox = tempfile::tempdir().unwrap();
        let python_out = common::run_python(&python, "capture_decoders.py", &request, sandbox.path());
        let rust_out = run_rust(kind, stage, &reads);
        assert_eq!(python_out["stderr"], rust_out["stderr"], "{} seed {seed} log", kind.name());
        assert_eq!(python_out["stage"], rust_out["stage"], "{} seed {seed} stage calls", kind.name());
        let (p, r) = (python_out["reads"].as_array().unwrap(), rust_out["reads"].as_array().unwrap());
        assert_eq!(p.len(), r.len());
        for (i, (p, r)) in p.iter().zip(r).enumerate() {
            assert_eq!(p, r, "{} seed {seed} read {i}", kind.name());
        }
        batches += r.iter().filter_map(Json::as_array).map(Vec::len).sum::<usize>();
        lines += rust_out["stderr"].as_str().unwrap().lines().count();
    }
    eprintln!("{} {}: {seeds} seeds, {batches} batches and {lines} log lines identical", kind.name(), stage.is_some());
    assert!(batches > 0);
}

const HOLDING: StageSpec = StageSpec { hold_cycle: &[0, 160, 37, 1300, 5], fail_at: 0, active_period: 7 };
const FAILING: StageSpec = StageSpec { hold_cycle: &[0, 160, 37, 1300, 5], fail_at: 40, active_period: 3 };
const FAILING_REFERENCE: StageSpec = StageSpec { hold_cycle: &[0], fail_at: 25, active_period: 1 };

#[test]
fn same_pcm_bytes_in_random_reads_the_pcm_decoder_gives_python_s_samples_and_times() {
    compare(Kind::Pcm, None, 20);
}

#[test]
fn same_mic_frames_without_a_stage_the_mic_decoder_gives_python_s_samples_times_and_log() {
    compare(Kind::Mic, None, 20);
}

#[test]
fn same_mic_frames_through_a_holding_stage_the_mic_decoder_matches_python_call_for_call() {
    compare(Kind::Mic, Some(HOLDING), 20);
}

#[test]
fn same_mic_frames_and_a_stage_failing_midway_both_go_raw_after_the_same_flush() {
    compare(Kind::Mic, Some(FAILING), 20);
}

#[test]
fn same_reference_frames_and_a_stage_failing_midway_both_feed_and_leave_it_alike() {
    compare(Kind::Reference, Some(FAILING_REFERENCE), 20);
}
