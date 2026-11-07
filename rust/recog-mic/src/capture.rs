//! The capture on macOS: a cpal input stream whose callback only copies, and a writer
//! loop on the main thread.
//!
//! CoreAudio calls the stream's callback on its real-time IO thread. The callback
//! takes no lock and allocates nothing: it downmixes the block into a wait-free
//! single-producer ring (`rtrb`) and pushes the block's stamp into a second one. A
//! block that does not fit (the parent stopped reading for ten seconds) is dropped and
//! shows as a hole in the stamps. The Python worker's callback needed the interpreter
//! lock, and a late callback lost as much audio as it was late.

use std::fs::File;
use std::io::{ErrorKind as IoErrorKind, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coreaudio::audio_unit::macos_helpers::{
    get_default_device_id, get_device_id_from_name, get_supported_physical_stream_formats,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, SampleFormat, StreamConfig, SupportedBufferSize};
use nix::time::{ClockId, clock_gettime};

use crate::args::Args;
use crate::devices::{DeviceEntry, capture_channels, find_input_device, mono_frames};
use crate::frame::encode_frame;
use crate::framer::Framer;
use crate::physical::{PhysicalFormat, Request, safe_request};

/// `kAudioFormatLinearPCM`, 'lpcm'.
const LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");

/// How often buffered frames go to the pipe.
const WRITE_INTERVAL: Duration = Duration::from_millis(20);
/// A stream that stopped calling back without an error is lost after this.
const CALLBACK_STALL_NS: i64 = 10_000_000_000;
/// Input kept while the parent does not read, in seconds of audio.
const RING_SECONDS: usize = 10;

/// Exit code: the stream stopped capturing; the parent's respawn opens a new one.
pub const EXIT_STREAM_LOST: i32 = 3;
/// Exit code: the named input device is not there.
pub const EXIT_DEVICE_NOT_FOUND: i32 = 4;

#[derive(Clone, Copy)]
struct Block {
    host_ns: u64,
    frames: u32,
}

/// The callback's end of the rings.
struct Producer {
    samples: rtrb::Producer<f32>,
    blocks: rtrb::Producer<Block>,
    dropped: Arc<AtomicU64>,
    channels: usize,
}

impl Producer {
    /// Wait-free: copies the block's mono mix and its stamp, or counts it as dropped.
    fn push<T: Copy + Into<f64>>(&mut self, host_ns: u64, interleaved: &[T], scale: f64) {
        let frames = interleaved.len() / self.channels;
        if self.samples.slots() < frames || self.blocks.is_full() {
            self.dropped.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        if let Ok(chunk) = self.samples.write_chunk_uninit(frames) {
            chunk.fill_from_iter(mono_frames(interleaved, self.channels, scale));
            let _ = self.blocks.push(Block { host_ns, frames: frames as u32 });
        }
    }
}

/// The device's available physical formats as cpal will read them, by its id there.
fn physical_formats(name: Option<&str>, device_name: &str) -> Option<Vec<PhysicalFormat>> {
    let id = match name {
        None => get_default_device_id(true)?,
        Some(_) => get_device_id_from_name(device_name, true)?,
    };
    // cpal treats a failed read as no match, so do the same.
    let formats = get_supported_physical_stream_formats(id).unwrap_or_default();
    Some(
        formats
            .iter()
            .map(|ranged| {
                let format = &ranged.mFormat;
                PhysicalFormat {
                    rate: format.mSampleRate,
                    min_rate: ranged.mSampleRateRange.mMinimum,
                    max_rate: ranged.mSampleRateRange.mMaximum,
                    linear_pcm: format.mFormatID == LINEAR_PCM,
                    flags: format.mFormatFlags,
                    bits: format.mBitsPerChannel,
                    channels: format.mChannelsPerFrame,
                    bytes_per_packet: format.mBytesPerPacket,
                    frames_per_packet: format.mFramesPerPacket,
                    bytes_per_frame: format.mBytesPerFrame,
                }
            })
            .collect(),
    )
}

fn uptime_ns() -> i64 {
    let now = clock_gettime(ClockId::from_raw(libc::CLOCK_UPTIME_RAW)).expect("CLOCK_UPTIME_RAW is always available");
    now.tv_sec() * 1_000_000_000 + now.tv_nsec()
}

fn wall_s() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn device_name(device: &cpal::Device) -> String {
    device.description().map(|description| description.name().to_owned()).unwrap_or_default()
}

fn pick_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device, i32> {
    let Some(name) = name else {
        return host.default_input_device().ok_or_else(|| {
            eprintln!("No default input device");
            EXIT_DEVICE_NOT_FOUND
        });
    };
    let mut devices: Vec<cpal::Device> = host.devices().map(|all| all.collect()).unwrap_or_default();
    let entries: Vec<DeviceEntry> = devices
        .iter()
        .map(|device| DeviceEntry { name: device_name(device), has_input: device.supports_input() })
        .collect();
    match find_input_device(&entries, name) {
        Some(index) => Ok(devices.swap_remove(index)),
        None => {
            eprintln!("Input device '{name}' not found");
            Err(EXIT_DEVICE_NOT_FOUND)
        }
    }
}

/// Captures until the stream stops or the parent goes; returns the exit code.
pub fn run(args: &Args, mut out: File) -> i32 {
    let host = cpal::default_host();
    let device = match pick_device(&host, args.device.as_deref()) {
        Ok(device) => device,
        Err(code) => return code,
    };
    let name = device_name(&device);
    let supported = match device.default_input_config() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Cannot read the format of '{name}': {e}");
            return EXIT_STREAM_LOST;
        }
    };
    // The device's current rate: cpal sets the device's nominal rate to whatever the
    // stream asks for, which would change it for every process using the device.
    let in_rate = supported.sample_rate();
    let channels = capture_channels(supported.channels());
    let buffer_size = match supported.buffer_size() {
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed((in_rate / 100).clamp(*min, *max)),
        SupportedBufferSize::Unknown => BufferSize::Default,
    };
    let config = StreamConfig { channels, sample_rate: in_rate, buffer_size };
    let Some(formats) = physical_formats(args.device.as_deref(), &name) else {
        eprintln!("Cannot find '{name}' among CoreAudio's input devices to read its formats");
        return EXIT_STREAM_LOST;
    };
    let Some(request) = safe_request(&formats, in_rate, channels) else {
        eprintln!("Not opening '{name}': every sample type could change its format for other processes");
        return EXIT_STREAM_LOST;
    };
    let (sample_format, scale) = match request {
        Request::F32 => (SampleFormat::F32, 1.0),
        Request::I32 => (SampleFormat::I32, 1.0 / 2_147_483_648.0),
        Request::I16 => (SampleFormat::I16, 1.0 / 32_768.0),
    };

    let ring_samples = in_rate as usize * RING_SECONDS;
    let (samples_in, mut samples_out) = rtrb::RingBuffer::<f32>::new(ring_samples);
    let (blocks_in, mut blocks_out) = rtrb::RingBuffer::<Block>::new(4096);
    let last_callback = Arc::new(AtomicI64::new(uptime_ns()));
    let dropped = Arc::new(AtomicU64::new(0));
    let xruns = Arc::new(AtomicU64::new(0));
    let lost = Arc::new(AtomicBool::new(false));
    let lost_reason: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
    // How long after its first sample the first block was handed over, for the log.
    let first_delay_ns = Arc::new(AtomicI64::new(i64::MIN));

    let on_data = {
        let last_callback = Arc::clone(&last_callback);
        let first_delay_ns = Arc::clone(&first_delay_ns);
        let mut producer = Producer {
            samples: samples_in,
            blocks: blocks_in,
            dropped: Arc::clone(&dropped),
            channels: channels.into(),
        };
        move |data: &cpal::Data, info: &cpal::InputCallbackInfo| {
            let host_ns = info.timestamp().callback.as_nanos() as u64;
            last_callback.store(host_ns as i64, Ordering::Relaxed);
            if first_delay_ns.load(Ordering::Relaxed) == i64::MIN {
                first_delay_ns.store(uptime_ns() - host_ns as i64, Ordering::Relaxed);
            }
            match request {
                Request::F32 => data.as_slice::<f32>().map(|s| producer.push(host_ns, s, scale)),
                Request::I32 => data.as_slice::<i32>().map(|s| producer.push(host_ns, s, scale)),
                Request::I16 => data.as_slice::<i16>().map(|s| producer.push(host_ns, s, scale)),
            };
        }
    };
    let on_error = {
        let xruns = Arc::clone(&xruns);
        let lost = Arc::clone(&lost);
        let lost_reason = Arc::clone(&lost_reason);
        move |error: cpal::Error| {
            if error.kind() == ErrorKind::Xrun {
                // An IO cycle CoreAudio skipped; the stamps show the hole.
                xruns.fetch_add(1, Ordering::Relaxed);
            } else {
                let _ = lost_reason.set(error.to_string());
                lost.store(true, Ordering::Release);
            }
        }
    };
    let stream = match device.build_input_stream_raw(config, sample_format, on_data, on_error, None) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Cannot open '{name}' at {in_rate} Hz: {e}");
            return EXIT_STREAM_LOST;
        }
    };
    if let Err(e) = stream.play() {
        eprintln!("Cannot start '{name}': {e}");
        return EXIT_STREAM_LOST;
    }
    let block_text = match buffer_size {
        BufferSize::Fixed(frames) => format!("{frames}-frame blocks"),
        BufferSize::Default => "default blocks".to_owned(),
    };
    eprintln!(
        "Capturing '{name}' at {in_rate} Hz, {channels} channel(s), {sample_format}, {block_text}, as {} Hz",
        args.sample_rate
    );

    let mut framer = Framer::new(in_rate, args.sample_rate);
    let mut block = Vec::new();
    let mut bytes = Vec::new();
    let (mut reported_xruns, mut reported_dropped) = (0, 0);
    let mut delay_reported = false;
    loop {
        thread::sleep(WRITE_INTERVAL);
        while let Ok(Block { host_ns, frames }) = blocks_out.pop() {
            let Ok(chunk) = samples_out.read_chunk(frames as usize) else { break };
            let (first, second) = chunk.as_slices();
            block.clear();
            block.extend_from_slice(first);
            block.extend_from_slice(second);
            chunk.commit_all();
            framer.push_block(host_ns as f64 * 1e-9, &block);
        }
        // Wall clock of a sample = its uptime stamp + (wall now - uptime now).
        let now_ns = uptime_ns();
        let wall_offset = wall_s() - now_ns as f64 * 1e-9;
        bytes.clear();
        for frame in framer.take_frames() {
            encode_frame(frame.adc_time, frame.adc_time + wall_offset, &frame.samples, &mut bytes);
        }
        if !bytes.is_empty()
            && let Err(e) = out.write_all(&bytes).and_then(|()| out.flush())
        {
            if e.kind() == IoErrorKind::BrokenPipe {
                return 0;
            }
            eprintln!("Cannot write to the parent: {e}");
            return EXIT_STREAM_LOST;
        }
        if !delay_reported && first_delay_ns.load(Ordering::Relaxed) != i64::MIN {
            delay_reported = true;
            let delay_ms = first_delay_ns.load(Ordering::Relaxed) as f64 / 1e6;
            eprintln!("First block handed over {delay_ms:.1} ms after its first sample's stamp");
        }
        let xrun_count = xruns.load(Ordering::Relaxed);
        if xrun_count != reported_xruns {
            eprintln!("Audio overload: {} IO cycle(s) skipped", xrun_count - reported_xruns);
            reported_xruns = xrun_count;
        }
        let dropped_count = dropped.load(Ordering::Relaxed);
        if dropped_count != reported_dropped {
            eprintln!("{} samples dropped: the parent did not read them", dropped_count - reported_dropped);
            reported_dropped = dropped_count;
        }
        if lost.load(Ordering::Acquire) {
            let reason = lost_reason.get().map(String::as_str).unwrap_or("unknown");
            eprintln!("The input stream was stopped by the audio system: {reason}");
            return EXIT_STREAM_LOST;
        }
        let gap_ns = now_ns - last_callback.load(Ordering::Relaxed);
        if gap_ns >= CALLBACK_STALL_NS {
            eprintln!("No audio callbacks for {:.0}s", gap_ns as f64 / 1e9);
            return EXIT_STREAM_LOST;
        }
    }
}
