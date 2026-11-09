//! The capture, independent of the audio system: choosing the device and the request,
//! the rings between the stream's callback and the writer loop, and the writer loop.
//!
//! CoreAudio calls the stream's callback on its real-time IO thread. The callback
//! takes no lock and allocates nothing: it downmixes the block into a wait-free
//! single-producer ring (`rtrb`) and pushes the block's stamp into a second one. A
//! block that does not fit (the parent stopped reading for ten seconds) is dropped and
//! shows as a hole in the stamps. The Python worker's callback needed the interpreter
//! lock, and a late callback lost as much audio as it was late.
//!
//! `capture.rs` puts cpal and CoreAudio behind [`AudioSystem`] and [`Clock`] on macOS;
//! the tests put a scripted device there.

use std::io::{ErrorKind as IoErrorKind, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::args::Args;
use crate::devices::{DeviceEntry, capture_channels, find_input_device, mono_frames};
use crate::frame::encode_frame;
use crate::framer::Framer;
use crate::physical::{PhysicalFormat, Request, safe_request};

/// How often buffered frames go to the pipe.
pub const WRITE_INTERVAL: Duration = Duration::from_millis(20);
/// A stream that stopped calling back without an error is lost after this.
pub const CALLBACK_STALL_NS: i64 = 10_000_000_000;
/// Input kept while the parent does not read, in seconds of audio.
pub const RING_SECONDS: usize = 10;
/// Stamps kept while the parent does not read, in blocks.
const BLOCK_RING: usize = 4096;

/// Full scale of a 32-bit integer sample, to 1.0.
pub const I32_SCALE: f64 = 1.0 / 2_147_483_648.0;
/// Full scale of a 16-bit integer sample, to 1.0.
pub const I16_SCALE: f64 = 1.0 / 32_768.0;

/// Exit code: the stream stopped capturing; the parent's respawn opens a new one.
pub const EXIT_STREAM_LOST: i32 = 3;
/// Exit code: the named input device is not there.
pub const EXIT_DEVICE_NOT_FOUND: i32 = 4;

/// The device's current input format, as the audio system reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentFormat {
    pub rate: u32,
    pub channels: u16,
    /// The range of block sizes it accepts, when it says.
    pub buffer: Option<(u32, u32)>,
}

/// How the stream is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// The device's current rate: cpal sets the device's nominal rate to whatever the
    /// stream asks for, which would change it for every process using the device.
    pub rate: u32,
    pub channels: u16,
    /// Frames per block, `None` for the audio system's default.
    pub block_frames: Option<u32>,
    pub request: Request,
}

/// What the capture needs of the audio system.
pub trait AudioSystem {
    type Device;
    /// Kept open while the writer loop runs.
    type Stream;
    fn default_input(&self) -> Option<Self::Device>;
    /// Every device, with what the lookup by name reads of it.
    fn devices(&self) -> Vec<(Self::Device, DeviceEntry)>;
    fn name(&self, device: &Self::Device) -> String;
    fn current_format(&self, device: &Self::Device) -> Result<CurrentFormat, String>;
    /// The available physical formats of the device called `name` (`named`), or of
    /// the default input; `None` when CoreAudio does not know the device.
    fn physical_formats(&self, named: bool, name: &str) -> Option<Vec<PhysicalFormat>>;
    /// An input stream whose data callback feeds `producer` and whose error callback
    /// tells `shared`; not started yet.
    fn open(
        &self,
        device: &Self::Device,
        plan: &Plan,
        producer: Producer,
        shared: Arc<Shared>,
    ) -> Result<Self::Stream, String>;
    fn play(&self, stream: &Self::Stream) -> Result<(), String>;
}

/// The clocks and the pause of the writer loop.
pub trait Clock {
    /// Host uptime, the clock of the stream's stamps.
    fn uptime_ns(&self) -> i64;
    /// Seconds since the epoch.
    fn wall_s(&self) -> f64;
    fn sleep(&self, duration: Duration);
}

/// What the callbacks tell the writer loop: atomics, and a reason set once.
pub struct Shared {
    last_callback: AtomicI64,
    /// How long after its first sample the first block was handed over, for the log.
    first_delay_ns: AtomicI64,
    dropped: AtomicU64,
    xruns: AtomicU64,
    lost: AtomicBool,
    lost_reason: OnceLock<String>,
}

impl Shared {
    /// No callback yet: the stall is counted from `now_ns`.
    pub fn new(now_ns: i64) -> Self {
        Shared {
            last_callback: AtomicI64::new(now_ns),
            first_delay_ns: AtomicI64::new(i64::MIN),
            dropped: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            lost_reason: OnceLock::new(),
        }
    }

    /// The stream's error callback: an xrun is an IO cycle CoreAudio skipped, which
    /// the stamps show as a hole; any other error stopped the stream.
    pub fn on_error(&self, xrun: bool, reason: impl FnOnce() -> String) {
        if xrun {
            self.xruns.fetch_add(1, Ordering::Relaxed);
        } else {
            let _ = self.lost_reason.set(reason());
            self.lost.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy)]
struct Block {
    host_ns: u64,
    frames: u32,
}

/// The callback's end of the rings.
pub struct Producer {
    samples: rtrb::Producer<f32>,
    blocks: rtrb::Producer<Block>,
    shared: Arc<Shared>,
    channels: usize,
}

impl Producer {
    /// A data callback for the block stamped `host_ns` began; `now_ns` is read on the
    /// first one only.
    pub fn on_callback(&mut self, host_ns: u64, now_ns: impl FnOnce() -> i64) {
        self.shared.last_callback.store(host_ns as i64, Ordering::Relaxed);
        if self.shared.first_delay_ns.load(Ordering::Relaxed) == i64::MIN {
            self.shared.first_delay_ns.store(now_ns() - host_ns as i64, Ordering::Relaxed);
        }
    }

    /// Wait-free: copies the block's mono mix and its stamp, or counts it as dropped.
    /// `None` is a block of another sample type, which the stream does not deliver.
    pub fn push<T: Copy + Into<f64>>(&mut self, host_ns: u64, interleaved: Option<&[T]>, scale: f64) {
        let Some(interleaved) = interleaved else { return };
        let frames = interleaved.len() / self.channels;
        if self.samples.slots() < frames || self.blocks.is_full() {
            self.shared.dropped.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        if let Ok(chunk) = self.samples.write_chunk_uninit(frames) {
            chunk.fill_from_iter(mono_frames(interleaved, self.channels, scale));
            let _ = self.blocks.push(Block { host_ns, frames: frames as u32 });
        }
    }
}

/// The writer loop's end of the rings.
pub struct Consumer {
    samples: rtrb::Consumer<f32>,
    blocks: rtrb::Consumer<Block>,
    block: Vec<f32>,
}

impl Consumer {
    /// Moves every block the callback completed into `framer`.
    pub fn drain(&mut self, framer: &mut Framer) {
        while let Ok(Block { host_ns, frames }) = self.blocks.pop() {
            let Ok(chunk) = self.samples.read_chunk(frames as usize) else { break };
            let (first, second) = chunk.as_slices();
            self.block.clear();
            self.block.extend_from_slice(first);
            self.block.extend_from_slice(second);
            chunk.commit_all();
            framer.push_block(host_ns as f64 * 1e-9, &self.block);
        }
    }
}

/// The rings for a stream of `channels` at `rate`.
pub fn rings(rate: u32, channels: u16, shared: Arc<Shared>) -> (Producer, Consumer) {
    let (samples_in, samples_out) = rtrb::RingBuffer::<f32>::new(rate as usize * RING_SECONDS);
    let (blocks_in, blocks_out) = rtrb::RingBuffer::<Block>::new(BLOCK_RING);
    let producer = Producer { samples: samples_in, blocks: blocks_in, shared, channels: channels.into() };
    (producer, Consumer { samples: samples_out, blocks: blocks_out, block: Vec::new() })
}

/// Blocks of 10 ms at `rate` within the accepted `range`, else the default.
pub fn block_frames(rate: u32, range: Option<(u32, u32)>) -> Option<u32> {
    range.map(|(min, max)| (rate / 100).clamp(min, max))
}

fn block_text(block_frames: Option<u32>) -> String {
    match block_frames {
        Some(frames) => format!("{frames}-frame blocks"),
        None => "default blocks".to_owned(),
    }
}

fn pick_device<S: AudioSystem>(system: &S, name: Option<&str>, log: &mut impl Write) -> Result<S::Device, i32> {
    let Some(name) = name else {
        return system.default_input().ok_or_else(|| {
            let _ = writeln!(log, "No default input device");
            EXIT_DEVICE_NOT_FOUND
        });
    };
    let mut devices = system.devices();
    let entries: Vec<DeviceEntry> = devices.iter().map(|(_, entry)| entry.clone()).collect();
    match find_input_device(&entries, name) {
        Some(index) => Ok(devices.swap_remove(index).0),
        None => {
            let _ = writeln!(log, "Input device '{name}' not found");
            Err(EXIT_DEVICE_NOT_FOUND)
        }
    }
}

/// How to open `device` without changing it for other processes.
fn plan<S: AudioSystem>(
    system: &S,
    device: &S::Device,
    named: bool,
    name: &str,
    log: &mut impl Write,
) -> Result<Plan, i32> {
    let current = system.current_format(device).map_err(|e| {
        let _ = writeln!(log, "Cannot read the format of '{name}': {e}");
        EXIT_STREAM_LOST
    })?;
    let channels = capture_channels(current.channels);
    let block_frames = block_frames(current.rate, current.buffer);
    let Some(formats) = system.physical_formats(named, name) else {
        let _ = writeln!(log, "Cannot find '{name}' among CoreAudio's input devices to read its formats");
        return Err(EXIT_STREAM_LOST);
    };
    let Some(request) = safe_request(&formats, current.rate, channels) else {
        let _ = writeln!(log, "Not opening '{name}': every sample type could change its format for other processes");
        return Err(EXIT_STREAM_LOST);
    };
    Ok(Plan { rate: current.rate, channels, block_frames, request })
}

/// The stream opened and started, and the writer's end of its rings.
fn start<S: AudioSystem>(
    system: &S,
    device: &S::Device,
    plan: &Plan,
    name: &str,
    shared: &Arc<Shared>,
    log: &mut impl Write,
) -> Result<(S::Stream, Consumer), i32> {
    let (producer, consumer) = rings(plan.rate, plan.channels, Arc::clone(shared));
    let stream = system.open(device, plan, producer, Arc::clone(shared)).map_err(|e| {
        let _ = writeln!(log, "Cannot open '{name}' at {} Hz: {e}", plan.rate);
        EXIT_STREAM_LOST
    })?;
    system.play(&stream).map_err(|e| {
        let _ = writeln!(log, "Cannot start '{name}': {e}");
        EXIT_STREAM_LOST
    })?;
    Ok((stream, consumer))
}

/// Captures until the stream stops or the parent goes; returns the exit code.
pub fn capture<S: AudioSystem>(
    system: &S,
    args: &Args,
    out: impl Write,
    log: &mut impl Write,
    clock: &impl Clock,
) -> i32 {
    let device = match pick_device(system, args.device.as_deref(), log) {
        Ok(device) => device,
        Err(code) => return code,
    };
    let name = system.name(&device);
    let plan = match plan(system, &device, args.device.is_some(), &name, log) {
        Ok(plan) => plan,
        Err(code) => return code,
    };
    let shared = Arc::new(Shared::new(clock.uptime_ns()));
    let (_stream, mut consumer) = match start(system, &device, &plan, &name, &shared, log) {
        Ok(started) => started,
        Err(code) => return code,
    };
    let _ = writeln!(
        log,
        "Capturing '{name}' at {} Hz, {} channel(s), {}, {}, as {} Hz",
        plan.rate,
        plan.channels,
        plan.request.name(),
        block_text(plan.block_frames),
        args.sample_rate
    );
    let mut framer = Framer::new(plan.rate, args.sample_rate);
    write_loop(&mut consumer, &mut framer, &shared, out, log, clock)
}

/// What the log has already said about the callbacks.
#[derive(Default)]
struct Reported {
    delay: bool,
    xruns: u64,
    dropped: u64,
}

impl Reported {
    /// One line for the first block's delay, and one per change of the counters.
    fn report(&mut self, shared: &Shared, log: &mut impl Write) {
        if !self.delay && shared.first_delay_ns.load(Ordering::Relaxed) != i64::MIN {
            self.delay = true;
            let delay_ms = shared.first_delay_ns.load(Ordering::Relaxed) as f64 / 1e6;
            let _ = writeln!(log, "First block handed over {delay_ms:.1} ms after its first sample's stamp");
        }
        let xruns = shared.xruns.load(Ordering::Relaxed);
        if xruns != self.xruns {
            let _ = writeln!(log, "Audio overload: {} IO cycle(s) skipped", xruns - self.xruns);
            self.xruns = xruns;
        }
        let dropped = shared.dropped.load(Ordering::Relaxed);
        if dropped != self.dropped {
            let _ = writeln!(log, "{} samples dropped: the parent did not read them", dropped - self.dropped);
            self.dropped = dropped;
        }
    }
}

/// The frames completed since the last call, written to the parent; `Err` with the
/// exit code when the pipe failed.
fn write_frames(
    framer: &mut Framer,
    wall_offset: f64,
    bytes: &mut Vec<u8>,
    out: &mut impl Write,
    log: &mut impl Write,
) -> Result<(), i32> {
    bytes.clear();
    for frame in framer.take_frames() {
        encode_frame(frame.adc_time, frame.adc_time + wall_offset, &frame.samples, bytes);
    }
    if bytes.is_empty() {
        return Ok(());
    }
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == IoErrorKind::BrokenPipe => Err(0),
        Err(e) => {
            let _ = writeln!(log, "Cannot write to the parent: {e}");
            Err(EXIT_STREAM_LOST)
        }
    }
}

/// The exit code when the stream stopped capturing.
fn stopped(shared: &Shared, now_ns: i64, log: &mut impl Write) -> Option<i32> {
    if shared.lost.load(Ordering::Acquire) {
        let reason = shared.lost_reason.get().map(String::as_str).unwrap_or("unknown");
        let _ = writeln!(log, "The input stream was stopped by the audio system: {reason}");
        return Some(EXIT_STREAM_LOST);
    }
    let gap_ns = now_ns - shared.last_callback.load(Ordering::Relaxed);
    if gap_ns >= CALLBACK_STALL_NS {
        let _ = writeln!(log, "No audio callbacks for {:.0}s", gap_ns as f64 / 1e9);
        return Some(EXIT_STREAM_LOST);
    }
    None
}

/// Every [`WRITE_INTERVAL`]: the blocks to frames, the frames to the parent, the
/// callbacks' news to the log, until the stream stops or the parent goes.
pub fn write_loop(
    consumer: &mut Consumer,
    framer: &mut Framer,
    shared: &Shared,
    mut out: impl Write,
    log: &mut impl Write,
    clock: &impl Clock,
) -> i32 {
    let mut bytes = Vec::new();
    let mut reported = Reported::default();
    loop {
        clock.sleep(WRITE_INTERVAL);
        consumer.drain(framer);
        // Wall clock of a sample = its uptime stamp + (wall now - uptime now).
        let now_ns = clock.uptime_ns();
        let wall_offset = clock.wall_s() - now_ns as f64 * 1e-9;
        if let Err(code) = write_frames(framer, wall_offset, &mut bytes, &mut out, log) {
            return code;
        }
        reported.report(shared, log);
        if let Some(code) = stopped(shared, now_ns, log) {
            return code;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::*;

    const FLOAT: u32 = 9;
    const INT: u32 = 12;
    const MIC: &str = "Built-in Microphone";

    fn physical(rate: f64, flags: u32, bits: u32, channels: u32) -> PhysicalFormat {
        let bytes = bits.div_ceil(8) * channels;
        PhysicalFormat {
            rate,
            min_rate: rate,
            max_rate: rate,
            linear_pcm: true,
            flags,
            bits,
            channels,
            bytes_per_packet: bytes,
            frames_per_packet: 1,
            bytes_per_frame: bytes,
        }
    }

    fn entry(name: &str, has_input: bool) -> DeviceEntry {
        DeviceEntry { name: name.to_owned(), has_input }
    }

    /// What happens while the writer loop sleeps.
    enum Event {
        /// A data callback with float samples, as the stream delivers them.
        F32(u64, Vec<f32>),
        /// A data callback with 16-bit samples.
        I16(u64, Vec<i16>),
        /// The error callback; `None` is an xrun.
        Error(Option<&'static str>),
        /// The uptime clock moves to this.
        Now(i64),
    }

    /// A device and its stream, driven by a script played one pause at a time.
    struct Scripted {
        devices: Vec<DeviceEntry>,
        default: Option<usize>,
        format: Result<CurrentFormat, String>,
        physical: Option<Vec<PhysicalFormat>>,
        open_error: Option<&'static str>,
        play_error: Option<&'static str>,
        looked_up: RefCell<Option<(bool, String)>>,
        opened: RefCell<Option<(usize, Plan)>>,
        callbacks: RefCell<Option<(Producer, Arc<Shared>)>>,
        now_ns: Cell<i64>,
        wall_s: f64,
        pauses: RefCell<VecDeque<Vec<Event>>>,
    }

    impl Scripted {
        /// The default input: a mono float device at 48 kHz taking blocks of 15..4096 frames.
        fn new(pauses: Vec<Vec<Event>>) -> Self {
            Scripted {
                devices: vec![entry("Speakers", false), entry(MIC, true)],
                default: Some(1),
                format: Ok(CurrentFormat { rate: 48_000, channels: 1, buffer: Some((15, 4096)) }),
                physical: Some(vec![physical(48_000.0, FLOAT, 32, 1)]),
                open_error: None,
                play_error: None,
                looked_up: RefCell::new(None),
                opened: RefCell::new(None),
                callbacks: RefCell::new(None),
                now_ns: Cell::new(5_000_000_000),
                wall_s: 1_000.0,
                pauses: RefCell::new(pauses.into()),
            }
        }

        fn run(&self, device: Option<&str>, out: impl Write) -> (i32, Vec<String>) {
            let args = Args { sample_rate: 16_000, device: device.map(String::from) };
            let mut log = Vec::new();
            let code = capture(self, &args, out, &mut log, self);
            (code, String::from_utf8(log).unwrap().lines().map(String::from).collect())
        }

        /// What capture.rs does in the stream's callbacks.
        fn play_event(&self, event: Event) {
            let mut callbacks = self.callbacks.borrow_mut();
            let (producer, shared) = callbacks.as_mut().expect("events need an open stream");
            match event {
                Event::F32(host_ns, samples) => {
                    producer.on_callback(host_ns, || self.now_ns.get());
                    producer.push(host_ns, Some(samples.as_slice()), 1.0);
                    producer.push::<i32>(host_ns, None, I32_SCALE);
                    producer.push::<i16>(host_ns, None, I16_SCALE);
                }
                Event::I16(host_ns, samples) => {
                    producer.on_callback(host_ns, || self.now_ns.get());
                    producer.push::<f32>(host_ns, None, 1.0);
                    producer.push::<i32>(host_ns, None, I32_SCALE);
                    producer.push(host_ns, Some(samples.as_slice()), I16_SCALE);
                }
                Event::Error(reason) => shared.on_error(reason.is_none(), || reason.unwrap_or_default().to_owned()),
                Event::Now(now_ns) => self.now_ns.set(now_ns),
            }
        }
    }

    impl AudioSystem for Scripted {
        type Device = usize;
        type Stream = ();

        fn default_input(&self) -> Option<usize> {
            self.default
        }

        fn devices(&self) -> Vec<(usize, DeviceEntry)> {
            self.devices.iter().cloned().enumerate().collect()
        }

        fn name(&self, device: &usize) -> String {
            self.devices[*device].name.clone()
        }

        fn current_format(&self, _device: &usize) -> Result<CurrentFormat, String> {
            self.format.clone()
        }

        fn physical_formats(&self, named: bool, name: &str) -> Option<Vec<PhysicalFormat>> {
            *self.looked_up.borrow_mut() = Some((named, name.to_owned()));
            self.physical.clone()
        }

        fn open(&self, device: &usize, plan: &Plan, producer: Producer, shared: Arc<Shared>) -> Result<(), String> {
            if let Some(error) = self.open_error {
                return Err(error.into());
            }
            *self.opened.borrow_mut() = Some((*device, *plan));
            *self.callbacks.borrow_mut() = Some((producer, shared));
            Ok(())
        }

        fn play(&self, _stream: &()) -> Result<(), String> {
            self.play_error.map_or(Ok(()), |error| Err(error.into()))
        }
    }

    impl Clock for Scripted {
        fn uptime_ns(&self) -> i64 {
            self.now_ns.get()
        }

        fn wall_s(&self) -> f64 {
            self.wall_s
        }

        fn sleep(&self, duration: Duration) {
            assert_eq!(duration, WRITE_INTERVAL);
            let events = self.pauses.borrow_mut().pop_front().expect("the loop outlived its script");
            for event in events {
                self.play_event(event);
            }
        }
    }

    /// The pipe's bytes for mono blocks framed in one pass, at `wall_offset`.
    fn framed(in_rate: u32, blocks: &[(f64, Vec<f32>)], wall_offset: f64) -> Vec<u8> {
        let mut framer = Framer::new(in_rate, 16_000);
        for (adc_time, samples) in blocks {
            framer.push_block(*adc_time, samples);
        }
        let mut bytes = Vec::new();
        for frame in framer.take_frames() {
            encode_frame(frame.adc_time, frame.adc_time + wall_offset, &frame.samples, &mut bytes);
        }
        bytes
    }

    /// Wall clock minus uptime at `now_ns`, with the script's wall clock of 1000 s.
    fn wall_offset(now_ns: i64) -> f64 {
        1_000.0 - now_ns as f64 * 1e-9
    }

    fn header(bytes: &[u8]) -> (f64, f64, u32) {
        let f64_at = |at: usize| f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        (f64_at(0), f64_at(8), u32::from_le_bytes(bytes[16..20].try_into().unwrap()))
    }

    fn ramp(len: usize) -> Vec<f32> {
        (0..len).map(|i| (i as f32 / len as f32) - 0.5).collect()
    }

    #[test]
    fn the_default_input_is_captured_as_float_and_its_frames_reach_the_pipe_until_the_stream_is_lost() {
        // given: three contiguous 10 ms blocks from 1 s on, then the device is unplugged
        let blocks: Vec<(u64, Vec<f32>)> = (0..3).map(|k| (1_000_000_000 + k * 10_000_000, ramp(480))).collect();
        let system = Scripted::new(vec![
            blocks.iter().map(|(host_ns, samples)| Event::F32(*host_ns, samples.clone())).collect(),
            vec![Event::Error(Some("device unplugged"))],
        ]);
        let mut out = Vec::new();
        // when
        let (code, log) = system.run(None, &mut out);
        // then
        assert_eq!(code, EXIT_STREAM_LOST);
        let mono: Vec<(f64, Vec<f32>)> = blocks.into_iter().map(|(ns, s)| (ns as f64 * 1e-9, s)).collect();
        assert_eq!(out, framed(48_000, &mono, wall_offset(5_000_000_000)));
        let (adc_time, wall_time, count) = header(&out);
        assert!((adc_time - 1.0).abs() < 1e-9 && (wall_time - 996.0).abs() < 1e-9, "{adc_time} {wall_time}");
        assert!(count > 0);
        assert_eq!(*system.looked_up.borrow(), Some((false, MIC.to_owned())));
        let plan = Plan { rate: 48_000, channels: 1, block_frames: Some(480), request: Request::F32 };
        assert_eq!(*system.opened.borrow(), Some((1, plan)));
        assert_eq!(
            log,
            [
                "Capturing 'Built-in Microphone' at 48000 Hz, 1 channel(s), f32, 480-frame blocks, as 16000 Hz",
                "First block handed over 4000.0 ms after its first sample's stamp",
                "The input stream was stopped by the audio system: device unplugged",
            ]
        );
    }

    #[test]
    fn a_named_stereo_device_unsafe_for_float_and_32_bits_is_captured_as_16_bit_mono_until_callbacks_stop() {
        // given: an output named like the input before it, and an input whose float and
        // 32-bit integer formats differ at its rate
        let mut system = Scripted::new(vec![
            vec![Event::I16(2_000_000_000, [16_384i16, 0].repeat(480))],
            vec![Event::Now(12_000_000_000)],
        ]);
        system.devices = vec![entry("USB Mic", false), entry("Other", true), entry("USB Mic", true)];
        system.format = Ok(CurrentFormat { rate: 48_000, channels: 2, buffer: None });
        system.physical = Some(vec![physical(48_000.0, FLOAT, 32, 2), physical(48_000.0, INT, 32, 2)]);
        system.now_ns.set(3_000_000_000);
        let mut out = Vec::new();
        // when
        let (code, log) = system.run(Some("USB Mic"), &mut out);
        // then: each frame is the mean of its channels at full scale one
        assert_eq!(code, EXIT_STREAM_LOST);
        assert_eq!(out, framed(48_000, &[(2_000_000_000.0 * 1e-9, vec![0.25; 480])], wall_offset(3_000_000_000)));
        assert_eq!(*system.looked_up.borrow(), Some((true, "USB Mic".to_owned())));
        let plan = Plan { rate: 48_000, channels: 2, block_frames: None, request: Request::I16 };
        assert_eq!(*system.opened.borrow(), Some((2, plan)));
        assert_eq!(
            log,
            [
                "Capturing 'USB Mic' at 48000 Hz, 2 channel(s), i16, default blocks, as 16000 Hz",
                "First block handed over 1000.0 ms after its first sample's stamp",
                "No audio callbacks for 10s",
            ]
        );
    }

    #[test]
    fn a_device_that_cannot_be_opened_safely_exits_with_the_reason_and_writes_no_frames() {
        type Change = fn(&mut Scripted);
        let cases: [(Option<&str>, Change, i32, &str); 7] = [
            (None, |s| s.default = None, EXIT_DEVICE_NOT_FOUND, "No default input device"),
            (Some("Gone"), |_| {}, EXIT_DEVICE_NOT_FOUND, "Input device 'Gone' not found"),
            (
                None,
                |s| s.format = Err("device busy".into()),
                EXIT_STREAM_LOST,
                "Cannot read the format of 'Built-in Microphone': device busy",
            ),
            (
                Some(MIC),
                |s| s.physical = None,
                EXIT_STREAM_LOST,
                "Cannot find 'Built-in Microphone' among CoreAudio's input devices to read its formats",
            ),
            (
                None,
                |s| {
                    s.physical = Some(vec![
                        physical(48_000.0, FLOAT, 32, 1),
                        physical(48_000.0, INT, 32, 1),
                        physical(48_000.0, INT, 16, 1),
                    ])
                },
                EXIT_STREAM_LOST,
                "Not opening 'Built-in Microphone': every sample type could change its format for other processes",
            ),
            (
                None,
                |s| s.open_error = Some("format not supported"),
                EXIT_STREAM_LOST,
                "Cannot open 'Built-in Microphone' at 48000 Hz: format not supported",
            ),
            (
                None,
                |s| s.play_error = Some("not permitted"),
                EXIT_STREAM_LOST,
                "Cannot start 'Built-in Microphone': not permitted",
            ),
        ];
        for (device, change, expected_code, expected_line) in cases {
            // given
            let mut system = Scripted::new(vec![]);
            change(&mut system);
            let mut out = Vec::new();
            // when
            let (code, log) = system.run(device, &mut out);
            // then
            assert_eq!((code, log), (expected_code, vec![expected_line.to_owned()]));
            assert!(out.is_empty());
        }
    }

    struct Failing(IoErrorKind);

    impl Write for Failing {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.0, "disk full"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_parent_that_closed_the_pipe_ends_the_capture_with_0_and_any_other_write_error_with_3() {
        for (kind, expected_code, expected_tail) in [
            (IoErrorKind::BrokenPipe, 0, None),
            (IoErrorKind::Other, EXIT_STREAM_LOST, Some("Cannot write to the parent: disk full")),
        ] {
            // given: one block to write
            let system = Scripted::new(vec![vec![Event::F32(1_000_000_000, ramp(480))]]);
            // when
            let (code, log) = system.run(None, Failing(kind));
            // then
            assert_eq!(code, expected_code);
            assert_eq!(log.len(), 1 + usize::from(expected_tail.is_some()), "{log:?}");
            assert_eq!(log.get(1).map(String::as_str), expected_tail);
        }
    }

    #[test]
    fn skipped_cycles_and_a_block_the_ring_cannot_hold_are_logged_once_per_change() {
        // given: an 8 kHz device, whose ring holds ten seconds, 80000 samples
        let mut system = Scripted::new(vec![
            vec![Event::Error(None), Event::Error(None), Event::F32(1_000_000_000, vec![0.0; 80_001])],
            vec![],
            vec![Event::Error(None)],
            vec![Event::Error(Some("gone"))],
        ]);
        system.format = Ok(CurrentFormat { rate: 8_000, channels: 1, buffer: Some((15, 4096)) });
        system.physical = Some(vec![physical(8_000.0, FLOAT, 32, 1)]);
        let mut out = Vec::new();
        // when
        let (code, log) = system.run(None, &mut out);
        // then
        assert_eq!(code, EXIT_STREAM_LOST);
        assert!(out.is_empty());
        assert_eq!(
            log,
            [
                "Capturing 'Built-in Microphone' at 8000 Hz, 1 channel(s), f32, 80-frame blocks, as 16000 Hz",
                "First block handed over 4000.0 ms after its first sample's stamp",
                "Audio overload: 2 IO cycle(s) skipped",
                "80001 samples dropped: the parent did not read them",
                "Audio overload: 1 IO cycle(s) skipped",
                "The input stream was stopped by the audio system: gone",
            ]
        );
    }

    #[test]
    fn more_blocks_than_the_stamp_ring_holds_drop_the_ones_that_do_not_fit() {
        // given: 4097 one-sample callbacks before the writer wakes
        let events = (0..4097).map(|k| Event::F32(1_000_000_000 + k * 20_833, vec![0.0])).collect();
        let system = Scripted::new(vec![events, vec![Event::Error(Some("gone"))]]);
        let mut out = Vec::new();
        // when
        let (code, log) = system.run(None, &mut out);
        // then
        assert_eq!(code, EXIT_STREAM_LOST);
        assert!(log.contains(&"1 samples dropped: the parent did not read them".to_owned()), "{log:?}");
    }
}
