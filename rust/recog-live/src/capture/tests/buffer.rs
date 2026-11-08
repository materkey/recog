//! tests/test_audio_capture.py and the capture cases of tests/test_live_buffering.py.
//!
//! Not ported: the 2-D (frames, channels) cases (`stereo_frames_are_downmixed…`,
//! `single_channel_frames_are_flattened`): blocks are mono here, the sounddevice
//! capture that gave 2-D frames is not part of this crate.

use super::fakes::SR;
use crate::capture::buffer::BufferedAudioCapture;
use crate::clock::ManualClock;

fn capture(max_buffer_s: f64) -> BufferedAudioCapture {
    let cap = BufferedAudioCapture::new(SR, "test", max_buffer_s, ManualClock::new(5000.0).shared());
    cap.set_running(true);
    cap
}

fn feed(cap: &BufferedAudioCapture, seconds: f64, captured_at: f64) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (seconds * f64::from(SR)) as usize;
    cap.append(&vec![0.1; n], captured_at);
}

#[test]
fn flat_frames_get_chunk_gives_mono_float32_of_the_asked_length() {
    let cap = capture(10.0);
    feed(&cap, 3.0, 1000.0);
    let (chunk, ts) = cap.get_chunk(3.0).unwrap();
    assert_eq!(chunk.len(), 3 * SR as usize);
    assert!(chunk.iter().all(|&s| s == 0.1));
    assert_eq!(ts, 1000.0);
}

#[test]
fn stopped_capture_with_audio_get_chunk_gives_none_and_keeps_the_buffer() {
    let cap = capture(10.0);
    feed(&cap, 3.0, 1000.0);
    cap.set_running(false);
    assert!(cap.get_chunk(3.0).is_none());
    assert_eq!(cap.buffered_samples(), i64::from(3 * SR));
}

#[test]
fn audio_in_the_buffer_clear_buffer_resets_samples_and_capture_time() {
    let cap = capture(10.0);
    feed(&cap, 3.0, 1000.0);
    cap.clear_buffer();
    assert!(cap.is_empty());
    assert_eq!(cap.buffered_samples(), 0);
    assert_eq!(cap.buffer_start_ts(), None);
    assert!(cap.get_chunk(3.0).is_none());
}

#[test]
fn three_seconds_captured_at_1000_get_chunk_gives_the_capture_start_time() {
    let cap = capture(10.0);
    feed(&cap, 3.0, 1000.0);
    let (chunk, ts) = cap.get_chunk(3.0).unwrap();
    assert_eq!(chunk.len(), 48_000);
    assert!((ts - 1000.0).abs() < 1e-9);
}

#[test]
fn six_seconds_two_chunks_the_second_starts_2_5_s_later() {
    let cap = capture(10.0);
    feed(&cap, 6.0, 1000.0);
    let (_, ts1) = cap.get_chunk(3.0).unwrap();
    let (_, ts2) = cap.get_chunk(3.0).unwrap();
    assert!((ts1 - 1000.0).abs() < 1e-9);
    assert!((ts2 - 1002.5).abs() < 1e-9);
}

#[test]
fn one_second_get_chunk_of_three_gives_none() {
    let cap = capture(10.0);
    feed(&cap, 1.0, 1000.0);
    assert!(cap.get_chunk(3.0).is_none());
}

#[test]
fn fourteen_seconds_into_a_ten_second_cap_the_oldest_are_dropped_and_time_moves_on() {
    let cap = capture(10.0);
    for i in 0..14 {
        feed(&cap, 1.0, 1000.0 + f64::from(i));
    }
    assert!(cap.buffered_samples() <= i64::from(10 * SR));
    let (_, ts) = cap.get_chunk(3.0).unwrap();
    assert!(ts >= 1004.0 - 0.1);
}

#[test]
fn one_block_over_the_cap_append_reports_the_dropped_samples() {
    let cap = capture(1.0);
    assert_eq!(cap.append(&vec![0.1; 12_000], 1000.0), 0);
    assert_eq!(cap.append(&vec![0.1; 12_000], 1000.75), 12_000);
    assert_eq!(cap.buffered_samples(), 12_000);
}

#[test]
fn realtime_consumer_and_a_device_gap_get_chunk_stamps_from_the_wall_clock_of_the_block() {
    let cap = capture(10.0);
    feed(&cap, 3.0, 1000.0);
    let (_, ts1) = cap.get_chunk(3.0).unwrap();
    assert!((ts1 - 1000.0).abs() < 1e-9);
    feed(&cap, 2.5, 1010.0);
    let (_, ts2) = cap.get_chunk(3.0).unwrap();
    assert!((ts2 - (1012.5 - 3.0)).abs() < 0.01);
}

#[test]
fn backlog_beyond_the_resync_limit_get_chunk_keeps_the_spoken_time() {
    let cap = capture(20.0);
    feed(&cap, 8.0, 1000.0);
    feed(&cap, 1.0, 1100.0);
    let (_, ts) = cap.get_chunk(3.0).unwrap();
    assert!((ts - 1000.0).abs() < 1e-9);
}

#[test]
fn source_boundary_mark_drops_the_buffer_and_the_next_chunk_carries_the_new_epoch() {
    let cap = capture(10.0);
    feed(&cap, 3.0, 1000.0);
    let _ = cap.get_chunk(3.0).unwrap();
    assert_eq!(cap.chunk_epoch(), 0);
    cap.mark_source_boundary();
    assert_eq!(cap.source_epoch(), 1);
    assert!(cap.is_empty());
    assert_eq!(cap.chunk_epoch(), 0);
    feed(&cap, 3.0, 1010.0);
    let _ = cap.get_chunk(3.0).unwrap();
    assert_eq!(cap.chunk_epoch(), 1);
}
