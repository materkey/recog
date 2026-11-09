//! Bit-exact parity with pyaec past the minimal adaptation phase, without golden data.
//!
//! The other data-free tests stop before the filter adapts (`is_adapted`), so the
//! learning rate it switches to then is checked only by `tests/parity.rs`, whose
//! golden data CI does not have. Here seven cases, made below from fixed seeds with
//! integer arithmetic only, run long enough for the filter to spend most of their
//! frames adapted (one, [`Scene::DirectThenRoom`], is first held back by its leak
//! estimate), and their output is compared with what pyaec 1.0.1's own speexdsp gave
//! for the same input. `tests/fixtures/adapted_golden.txt` keeps that as SHA-256
//! digests, per case: the input, the whole output, and each block of [`BLOCK_FRAMES`]
//! frames, so a failure names the first block that diverges; and the last output
//! samples, to show by how much.
//!
//! The fixture was written on Linux x86_64 with pyaec 1.0.1 from PyPI (the
//! `py3-none-manylinux_2_17_x86_64` wheel; the digest of its `libaec.so` is in the
//! fixture's header), from `rust/`:
//!
//! ```text
//! SPEEXDSP_AEC_CASES_OUT=$PWD/cases cargo test -p speexdsp-aec --test adapted_golden -- --ignored
//! python3 -m venv pyaec && pyaec/bin/pip install pyaec==1.0.1
//! pyaec/bin/python speexdsp-aec/tests/fixtures/adapted_golden.py cases > speexdsp-aec/tests/fixtures/adapted_golden.txt
//! ```
//!
//! pyaec's macOS arm64 build (`libaec.dylib` sha256 `8e84101e…`, the one the port was
//! first held to) gives the same digests. A change to a case or to the generator
//! changes its input digest, which the test checks first: regenerate the fixture then,
//! never edit it by hand.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use speexdsp_aec::{Config, EchoCanceller};

/// Frames per digest of the output.
const BLOCK_FRAMES: usize = 50;
/// Output samples the fixture keeps as they are, from the end.
const LAST_SAMPLES: usize = 8;

/// What the microphone hears besides noise.
#[derive(Clone, Copy)]
enum Scene {
    /// The far end through a room.
    Echo,
    /// The echo, and from two fifths in a near-end talker too.
    DoubleTalk,
    /// The echo through another room from half way.
    PathChange,
    /// A near-end talker, and the echo only from a quarter in: the filter has spent
    /// its minimal adaptation before there is an echo to find.
    LateEcho,
    /// A lone direct path, then from a quarter in another room: the filter models the
    /// path so well that its leak estimate stays under the 3% it needs to count as
    /// adapted until the room changes.
    DirectThenRoom,
}

struct Case {
    name: &'static str,
    seed: u64,
    frame_size: usize,
    filter_length: usize,
    frames: usize,
    scene: Scene,
}

/// recog's configuration (160, 4096) and windows of 2⁸, 2⁵·5, 2⁵·3·5 and 2⁷ samples.
const CASES: [Case; 7] = [
    Case { name: "echo_160_4096", seed: 1, frame_size: 160, filter_length: 4096, frames: 400, scene: Scene::Echo },
    Case {
        name: "doubletalk_160_1600",
        seed: 2,
        frame_size: 160,
        filter_length: 1600,
        frames: 300,
        scene: Scene::DoubleTalk,
    },
    Case {
        name: "path_change_128_1024",
        seed: 3,
        frame_size: 128,
        filter_length: 1024,
        frames: 400,
        scene: Scene::PathChange,
    },
    Case { name: "echo_80_800", seed: 4, frame_size: 80, filter_length: 800, frames: 400, scene: Scene::Echo },
    Case {
        name: "doubletalk_240_2400",
        seed: 5,
        frame_size: 240,
        filter_length: 2400,
        frames: 250,
        scene: Scene::DoubleTalk,
    },
    Case { name: "late_echo_64_512", seed: 6, frame_size: 64, filter_length: 512, frames: 400, scene: Scene::LateEcho },
    // one of the few seeds whose lone path the filter models that well
    Case {
        name: "direct_then_room_160_1600",
        seed: 12,
        frame_size: 160,
        filter_length: 1600,
        frames: 400,
        scene: Scene::DirectThenRoom,
    },
];

/// xorshift64*, the cases' only source of randomness: integers throughout, so the
/// input is the same bytes on every platform.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Uniform in `lo..=hi`, but for a modulo bias that does not matter here.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        lo + (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32 % (hi - lo + 1)
    }

    fn sign(&mut self) -> i32 {
        if self.range(0, 1) == 0 { -1 } else { 1 }
    }
}

/// Low-passed noise in bursts of 25–150 ms at 16 kHz, a quarter of them silent, each
/// at a gain up to `level` (Q15).
fn speech(rng: &mut Rng, len: usize, level: i32) -> Vec<i32> {
    let (mut slow, mut slower, mut gain, mut left) = (0i32, 0i32, 0i32, 0i32);
    (0..len)
        .map(|_| {
            if left == 0 {
                left = rng.range(400, 2400);
                gain = if rng.range(0, 3) == 0 { 0 } else { rng.range(level / 4, level) };
            }
            left -= 1;
            slow += (rng.range(-32767, 32767) - slow) >> 1;
            slower += (slow - slower) >> 1;
            (slower * gain) >> 15
        })
        .collect()
}

/// A direct path after `delay` samples and a tail decaying over `span` more, as
/// (lag, gain in Q15).
fn room(rng: &mut Rng, delay: usize, span: i32) -> Vec<(usize, i32)> {
    let mut taps = vec![(delay, rng.sign() * rng.range(8000, 20000))];
    for k in 1..=16 {
        let lag = delay + rng.range(1, span) as usize;
        taps.push((lag, rng.sign() * rng.range(500, 4000) / k));
    }
    taps
}

fn echo(far: &[i32], taps: &[(usize, i32)]) -> Vec<i32> {
    (0..far.len())
        .map(|t| {
            let sum: i64 = taps
                .iter()
                .filter(|&&(lag, _)| lag <= t)
                .map(|&(lag, gain)| i64::from(far[t - lag]) * i64::from(gain))
                .sum();
            (sum >> 15) as i32
        })
        .collect()
}

/// A room whose echo fits in the filter, its direct path in the first quarter.
fn room_for(rng: &mut Rng, case: &Case) -> Vec<(usize, i32)> {
    let delay = rng.range(20, case.filter_length as i32 / 4) as usize;
    room(rng, delay, case.filter_length as i32 / 3)
}

/// (microphone, far end) of a case.
fn signals(case: &Case) -> (Vec<i16>, Vec<i16>) {
    let mut rng = Rng::new(case.seed);
    let len = case.frames * case.frame_size;
    let far = speech(&mut rng, len, 12000);
    let first = room_for(&mut rng, case);
    let mut mic = echo(&far, &first);
    match case.scene {
        Scene::Echo => {}
        Scene::DoubleTalk => {
            let near = speech(&mut rng, len, 4000);
            for (m, v) in mic.iter_mut().zip(near).skip(len * 2 / 5) {
                *m += v;
            }
        }
        Scene::PathChange => {
            let second = room_for(&mut rng, case);
            mic[len / 2..].copy_from_slice(&echo(&far, &second)[len / 2..]);
        }
        Scene::LateEcho => {
            mic[..len / 4].fill(0);
            for (m, v) in mic.iter_mut().zip(speech(&mut rng, len, 6000)) {
                *m += v;
            }
        }
        Scene::DirectThenRoom => {
            let second = room_for(&mut rng, case);
            mic[..len / 4].copy_from_slice(&echo(&far, &first[..1])[..len / 4]);
            mic[len / 4..].copy_from_slice(&echo(&far, &second)[len / 4..]);
        }
    }
    let to_i16 = |v: i32| v.clamp(-32768, 32767) as i16;
    let mic = mic.into_iter().map(|v| to_i16(v + rng.range(-16, 16))).collect();
    (mic, far.into_iter().map(to_i16).collect())
}

/// The port's output for a case, and for how many frames it had adapted.
fn cancel(case: &Case, mic: &[i16], far: &[i16]) -> (Vec<i16>, usize) {
    let n = case.frame_size;
    let mut aec = EchoCanceller::new(Config::new(n, case.filter_length)).unwrap();
    let mut out = vec![0i16; mic.len()];
    let mut adapted = 0;
    for ((m, f), o) in mic.chunks_exact(n).zip(far.chunks_exact(n)).zip(out.chunks_exact_mut(n)) {
        aec.cancel(m, f, o).unwrap();
        adapted += usize::from(aec.is_adapted());
    }
    (out, adapted)
}

fn bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn sha256(samples: &[i16]) -> String {
    Sha256::digest(bytes(samples)).iter().map(|b| format!("{b:02x}")).collect()
}

/// The fixture's cases: name, then `key=value` fields.
fn fixture() -> BTreeMap<String, BTreeMap<String, String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adapted_golden.txt");
    let text = fs::read_to_string(&path).unwrap();
    let field = |word: &str| {
        let (key, value) = word.split_once('=').unwrap();
        (key.to_owned(), value.to_owned())
    };
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let mut words = line.split_whitespace();
            (words.next().unwrap().to_owned(), words.map(field).collect())
        })
        .collect()
}

/// How a case departs from pyaec, if it does.
fn divergence(case: &Case, want: &BTreeMap<String, String>) -> Option<String> {
    let (mic, far) = signals(case);
    if sha256(&mic) != want["mic"] || sha256(&far) != want["ref"] {
        return Some("the input is not the one pyaec ran: regenerate the fixture".into());
    }
    let (out, adapted) = cancel(case, &mic, &far);
    if adapted < case.frames / 2 {
        return Some(format!("adapted for {adapted} of {} frames only", case.frames));
    }
    let blocks = out.chunks(BLOCK_FRAMES * case.frame_size).map(|block| sha256(block)[..16].to_owned());
    if let Some(k) = blocks.zip(want["blocks"].split(',')).position(|(ours, theirs)| ours != theirs) {
        let last = &out[out.len() - LAST_SAMPLES..];
        let frames = k * BLOCK_FRAMES..(k + 1) * BLOCK_FRAMES;
        return Some(format!("frames {frames:?} differ; last samples: pyaec {}, port {last:?}", want["last"]));
    }
    (sha256(&out) != want["out"]).then(|| "the output digest differs".into())
}

#[test]
fn long_echoes_when_the_filter_has_adapted_then_the_output_is_pyaecs() {
    let expected = fixture();
    let mut names: Vec<&str> = CASES.iter().map(|c| c.name).collect();
    names.sort_unstable();
    let listed: Vec<&str> = expected.keys().map(String::as_str).collect();
    assert_eq!(listed, names, "the fixture's cases are not the test's: regenerate it");
    let failures: Vec<String> = CASES
        .iter()
        .filter_map(|case| divergence(case, &expected[case.name]).map(|why| format!("{}: {why}", case.name)))
        .collect();
    assert!(failures.is_empty(), "{} of {} cases diverge:\n{}", failures.len(), CASES.len(), failures.join("\n"));
}

#[test]
#[ignore = "writes the cases for tests/fixtures/adapted_golden.py; see the module docs"]
fn the_cases_when_asked_then_written_for_pyaec() {
    let root = std::env::var_os("SPEEXDSP_AEC_CASES_OUT").expect("SPEEXDSP_AEC_CASES_OUT: where to write the cases");
    for case in &CASES {
        let dir = Path::new(&root).join(case.name);
        fs::create_dir_all(&dir).unwrap();
        let (mic, far) = signals(case);
        fs::write(dir.join("mic.i16"), bytes(&mic)).unwrap();
        fs::write(dir.join("ref.i16"), bytes(&far)).unwrap();
        let meta = format!("{{\"frame_size\": {}, \"filter_length\": {}}}\n", case.frame_size, case.filter_length);
        fs::write(dir.join("meta.json"), meta).unwrap();
    }
}
