//! The capture on macOS: cpal and CoreAudio behind the worker's [`AudioSystem`], the
//! host uptime clock behind its [`Clock`].
//!
//! Everything here only hands calls through; what to open, how to read the callbacks
//! and when to stop is decided in `worker.rs`, which is tested on every system. The
//! stream's data callback stays lock-free and allocation-free: it stamps the block and
//! copies it into the worker's wait-free rings.

use std::fs::File;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coreaudio::audio_unit::macos_helpers::{
    get_default_device_id, get_device_id_from_name, get_supported_physical_stream_formats,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, SampleFormat, StreamConfig, SupportedBufferSize};
use nix::time::{ClockId, clock_gettime};

use crate::args::Args;
use crate::devices::DeviceEntry;
use crate::physical::PhysicalFormat;
use crate::worker::{self, AudioSystem, Clock, CurrentFormat, I16_SCALE, I32_SCALE, Plan, Producer, Shared};

/// `kAudioFormatLinearPCM`, 'lpcm'.
const LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");

/// cpal's sample type of each `Request`, in the order of its variants.
const SAMPLE_FORMATS: [SampleFormat; 3] = [SampleFormat::F32, SampleFormat::I32, SampleFormat::I16];

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

fn buffer_range(size: &SupportedBufferSize) -> Option<(u32, u32)> {
    if let SupportedBufferSize::Range { min, max } = size { Some((*min, *max)) } else { None }
}

/// The CoreAudio id (an `AudioDeviceID`) of the input device called `name`, or of
/// the default input.
fn device_id(named: bool, name: &str) -> Option<u32> {
    if named { get_device_id_from_name(name, true) } else { get_default_device_id(true) }
}

/// cpal's default host.
struct CoreAudio {
    host: cpal::Host,
}

impl AudioSystem for CoreAudio {
    type Device = cpal::Device;
    type Stream = cpal::Stream;

    fn default_input(&self) -> Option<cpal::Device> {
        self.host.default_input_device()
    }

    fn devices(&self) -> Vec<(cpal::Device, DeviceEntry)> {
        let devices: Vec<cpal::Device> = self.host.devices().map(|all| all.collect()).unwrap_or_default();
        devices
            .into_iter()
            .map(|device| {
                let entry = DeviceEntry { name: device_name(&device), has_input: device.supports_input() };
                (device, entry)
            })
            .collect()
    }

    fn name(&self, device: &cpal::Device) -> String {
        device_name(device)
    }

    fn current_format(&self, device: &cpal::Device) -> Result<CurrentFormat, String> {
        let config = device.default_input_config().map_err(|e| e.to_string())?;
        Ok(CurrentFormat {
            rate: config.sample_rate(),
            channels: config.channels(),
            buffer: buffer_range(config.buffer_size()),
        })
    }

    fn physical_formats(&self, named: bool, name: &str) -> Option<Vec<PhysicalFormat>> {
        let id = device_id(named, name)?;
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

    fn open(
        &self,
        device: &cpal::Device,
        plan: &Plan,
        mut producer: Producer,
        shared: Arc<Shared>,
    ) -> Result<cpal::Stream, String> {
        let buffer_size = plan.block_frames.map_or(BufferSize::Default, BufferSize::Fixed);
        let config = StreamConfig { channels: plan.channels, sample_rate: plan.rate, buffer_size };
        // The stream delivers the requested type only: the other two read as `None`.
        let on_data = move |data: &cpal::Data, info: &cpal::InputCallbackInfo| {
            let host_ns = info.timestamp().callback.as_nanos() as u64;
            producer.on_callback(host_ns, uptime_ns);
            producer.push(host_ns, data.as_slice::<f32>(), 1.0);
            producer.push(host_ns, data.as_slice::<i32>(), I32_SCALE);
            producer.push(host_ns, data.as_slice::<i16>(), I16_SCALE);
        };
        let on_error = move |error: cpal::Error| shared.on_error(error.kind() == ErrorKind::Xrun, || error.to_string());
        let sample_format = SAMPLE_FORMATS[plan.request as usize];
        device.build_input_stream_raw(config, sample_format, on_data, on_error, None).map_err(|e| e.to_string())
    }

    fn play(&self, stream: &cpal::Stream) -> Result<(), String> {
        stream.play().map_err(|e| e.to_string())
    }
}

/// The host uptime clock of CoreAudio's stamps, and a real pause.
struct SystemClock;

impl Clock for SystemClock {
    fn uptime_ns(&self) -> i64 {
        uptime_ns()
    }

    fn wall_s(&self) -> f64 {
        wall_s()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Captures until the stream stops or the parent goes; returns the exit code.
pub fn run(args: &Args, out: File) -> i32 {
    worker::capture(&CoreAudio { host: cpal::default_host() }, args, out, &mut std::io::stderr(), &SystemClock)
}
