//! Runs a Rust GigaAM runtime over the corpus `prep.py` cut, the way `ref.py` runs recog's
//! PyTorch model, and writes the same JSON shape for `score.py`.
//!
//! asrbench <wavo|gigastt> <corpus dir> <out.json> [backend]; run.sh runs them all
//!   wavo:    backend auto | gpu | ane (Neural Engine)
//!   gigastt: backend auto | cpu | cuda | coreml; model dir from GIGASTT_MODEL_DIR, encoder
//!            threads from GIGASTT_THREADS (default 1, as gigastt)
//! Peak memory comes from the runner (`/usr/bin/time`), not from here.
#![forbid(unsafe_code)]

mod runtime;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Item {
    id: String,
    offset: usize,
    len: usize,
}

#[derive(Deserialize)]
struct Manifest {
    shapes: BTreeMap<String, Vec<Item>>,
}

/// One recognized word: text, start and end in ms from the window start.
#[derive(Serialize, Clone)]
pub struct Word(pub String, pub u32, pub u32);

pub struct Recognized {
    pub text: String,
    pub words: Vec<Word>,
}

/// What every runtime offers the bench.
pub trait Runtime {
    fn device(&self) -> String;
    fn transcribe(&mut self, pcm: &[f32]) -> Recognized;
    /// Starts a decode of `pcm`, asks it to stop after `after_ms`, and returns the time from that
    /// request to the call's return, or None when the runtime cannot stop a decode under way.
    fn abort_latency_ms(&mut self, pcm: &[f32], after_ms: u64) -> Option<f64>;
}

fn read_f32(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (name, corpus, out) = (&args[1], Path::new(&args[2]), &args[3]);
    let backend = args.get(4).map(String::as_str).unwrap_or("auto");
    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(corpus.join("manifest.json")).unwrap()).unwrap();
    let pcm: BTreeMap<&str, Vec<f32>> =
        manifest.shapes.keys().map(|s| (s.as_str(), read_f32(&corpus.join(format!("{s}.f32"))))).collect();
    let window = |shape: &str, it: &Item| &pcm[shape][it.offset..it.offset + it.len];

    let t = Instant::now();
    let mut rt = runtime::load(name, backend);
    let load_s = t.elapsed().as_secs_f64();
    let live = &manifest.shapes["live"];
    rt.transcribe(window("live", &live[0])); // warm-up, not timed

    let (mut texts, mut times) = (BTreeMap::new(), Vec::new());
    for it in live {
        let t = Instant::now();
        let r = rt.transcribe(window("live", it));
        times.push(t.elapsed().as_secs_f64());
        texts.insert(it.id.clone(), Value::String(r.text));
    }
    // No runtime has a batched forward over clips: a group of 8 is 8 calls in a row.
    let batch_times: Vec<f64> = times.chunks(8).map(|g| g.iter().sum()).collect();

    let (mut refine, mut refine_times) = (BTreeMap::new(), Vec::new());
    for it in &manifest.shapes["refine"] {
        let t = Instant::now();
        let r = rt.transcribe(window("refine", it));
        refine_times.push(t.elapsed().as_secs_f64());
        refine.insert(it.id.clone(), json!({"text": r.text, "words": r.words}));
    }

    // Abort: the longest refine windows, stopped 100 ms in (the live path wins the lock).
    let mut long: Vec<&Item> = manifest.shapes["refine"].iter().collect();
    long.sort_by_key(|it| std::cmp::Reverse(it.len));
    let abort: Vec<Option<f64>> =
        long.iter().take(10).map(|it| rt.abort_latency_ms(window("refine", it), 100)).collect();

    let result = json!({
        "runtime": name, "device": rt.device(), "load_s": load_s,
        "live": texts, "batch": Value::Null, "refine": refine, "abort_ms": abort,
        "times": {"live": times, "batch": batch_times, "refine": refine_times},
    });
    std::fs::write(out, serde_json::to_string(&result).unwrap()).unwrap();
    eprintln!(
        "{name} {}: load {load_s:.1}s, live {:.1}s, refine {:.1}s, abort {:?}",
        rt.device(),
        times.iter().sum::<f64>(),
        refine_times.iter().sum::<f64>(),
        abort
    );
}
