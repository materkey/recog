//! `speex-aec`: runs the canceller over the parity harness's cases.
//!
//! ```text
//! speex-aec CASE_DIR... [--write NAME] [--rate HZ]
//! ```
//! Each case directory holds `meta.json` (`frame_size`, `filter_length`),
//! `mic.i16` and `ref.i16` (little-endian int16). Frames are streamed through the
//! canceller, so memory stays at the size of its state whatever the length of the
//! recording. With `--write NAME` the output goes to `CASE_DIR/NAME`. One JSON line
//! on stdout reports the time spent in the canceller, in the shape of
//! `harness/legacy_speex.py`.
#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use speexdsp_aec::{Config, DEFAULT_SAMPLING_RATE, EchoCanceller};

/// Command-line options.
struct Options {
    cases: Vec<PathBuf>,
    write: Option<String>,
    rate: u32,
}

/// What one case took.
#[derive(Default)]
struct Totals {
    samples: usize,
    busy: Duration,
    checksum: i64,
    resets: u64,
}

const USAGE: &str = "usage: speex-aec CASE_DIR... [--write NAME] [--rate HZ]";

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options { cases: Vec::new(), write: None, rate: DEFAULT_SAMPLING_RATE };
    while let Some(arg) = args.next() {
        apply(&mut options, arg, &mut args)?;
    }
    if options.cases.is_empty() {
        return Err(USAGE.into());
    }
    Ok(options)
}

/// One argument: an option, taking its value from `args`, or a case directory.
fn apply(options: &mut Options, arg: String, args: &mut impl Iterator<Item = String>) -> Result<(), String> {
    match arg.as_str() {
        "--write" => options.write = Some(value(args, "--write needs a file name")?),
        "--rate" => options.rate = parse_rate(&value(args, "--rate needs a value")?)?,
        flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
        _ => options.cases.push(PathBuf::from(arg)),
    }
    Ok(())
}

/// The argument after an option, or `missing` as the error.
fn value(args: &mut impl Iterator<Item = String>, missing: &str) -> Result<String, String> {
    args.next().ok_or_else(|| missing.to_string())
}

fn parse_rate(value: &str) -> Result<u32, String> {
    value.parse().map_err(|_| format!("bad --rate {value}"))
}

/// The integer after `"key":` in a flat JSON object.
fn json_usize(text: &str, key: &str) -> Result<usize, String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle).ok_or(format!("meta.json has no {key}"))? + needle.len();
    let rest = text[at..].trim_start().strip_prefix(':').ok_or(format!("meta.json: no value for {key}"))?;
    let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().map_err(|_| format!("meta.json: bad {key}"))
}

/// Fills `frame` from `reader`; false at a clean end of the stream.
fn read_frame(reader: &mut impl Read, bytes: &mut [u8], frame: &mut [i16]) -> Result<bool, String> {
    match reader.read_exact(bytes) {
        Ok(()) => {
            for (sample, pair) in frame.iter_mut().zip(bytes.as_chunks::<2>().0.iter()) {
                *sample = i16::from_le_bytes([pair[0], pair[1]]);
            }
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// An I/O result with the path in its error.
fn at<T>(path: &Path, result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| format!("{}: {e}", path.display()))
}

fn open(path: &Path) -> Result<BufReader<File>, String> {
    at(path, File::open(path)).map(BufReader::new)
}

fn create(path: &Path) -> Result<BufWriter<File>, String> {
    at(path, File::create(path)).map(BufWriter::new)
}

/// A case directory, read at the boundary: its configuration parsed once, its
/// streams open.
struct Case {
    config: Config,
    mic: BufReader<File>,
    far: BufReader<File>,
    sink: Option<BufWriter<File>>,
}

fn open_case(dir: &Path, rate: u32, write: Option<&str>) -> Result<Case, String> {
    let meta_path = dir.join("meta.json");
    let meta = at(&meta_path, fs::read_to_string(&meta_path))?;
    let size = Config::new(json_usize(&meta, "frame_size")?, json_usize(&meta, "filter_length")?);
    let config = Config { sampling_rate: rate, ..size };
    let (mic, far) = (open(&dir.join("mic.i16"))?, open(&dir.join("ref.i16"))?);
    let sink = write.map(|name| create(&dir.join(name))).transpose()?;
    Ok(Case { config, mic, far, sink })
}

impl Totals {
    fn add_frame(&mut self, busy: Duration, out: &[i16]) {
        self.busy += busy;
        self.samples += out.len();
        self.checksum = out.iter().fold(self.checksum, |acc, &v| acc.wrapping_mul(31).wrapping_add(i64::from(v)));
    }
}

/// Streams one case through a fresh canceller.
fn run_case(dir: &Path, rate: u32, write: Option<&str>) -> Result<Totals, String> {
    let mut case = open_case(dir, rate, write)?;
    let mut aec = EchoCanceller::new(case.config).map_err(|e| e.to_string())?;
    let mut totals = stream(&mut aec, &mut case).map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Some(sink) = case.sink.as_mut() {
        sink.flush().map_err(|e| e.to_string())?;
    }
    totals.resets = aec.resets();
    Ok(totals)
}

/// Every whole frame of the case through `aec`, its output to the sink if any.
fn stream(aec: &mut EchoCanceller, case: &mut Case) -> Result<Totals, String> {
    let n = case.config.frame_size;
    let mut bytes = vec![0u8; 2 * n];
    let (mut m, mut f, mut o) = (vec![0i16; n], vec![0i16; n], vec![0i16; n]);
    let mut totals = Totals::default();
    while read_frame(&mut case.mic, &mut bytes, &mut m)? {
        if !read_frame(&mut case.far, &mut bytes, &mut f)? {
            return Err("ref.i16 is shorter than mic.i16".into());
        }
        let start = Instant::now();
        aec.cancel(&m, &f, &mut o).expect("the buffers are one frame long");
        totals.add_frame(start.elapsed(), &o);
        if let Some(sink) = case.sink.as_mut() {
            let out: Vec<u8> = o.iter().flat_map(|v| v.to_le_bytes()).collect();
            sink.write_all(&out).map_err(|e| e.to_string())?;
        }
    }
    Ok(totals)
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let mut all = Totals::default();
    let mut resets = Vec::with_capacity(options.cases.len());
    for dir in &options.cases {
        match run_case(dir, options.rate, options.write.as_deref()) {
            Ok(t) => {
                all.samples += t.samples;
                all.busy += t.busy;
                all.checksum = all.checksum.wrapping_mul(1_000_003).wrapping_add(t.checksum);
                resets.push(t.resets.to_string());
            }
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!(
        "{{\"mode\": \"rust\", \"cases\": {}, \"kernel_seconds\": {}, \"audio_seconds\": {}, \"checksum\": {}, \"resets\": [{}]}}",
        options.cases.len(),
        all.busy.as_secs_f64(),
        all.samples as f64 / 16000.0,
        all.checksum,
        resets.join(", ")
    );
    ExitCode::SUCCESS
}
