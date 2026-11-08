//! The constants of `recog::config` against `recog/config.py`.

mod common;

use recog_live::config::*;

#[test]
fn python_config_constants_compared_then_every_value_is_the_same() {
    let Some(python) = common::python() else { return };
    let sandbox = tempfile::tempdir().unwrap();
    let answer = common::run_python(&python, "constants.py", &serde_json::json!({}), sandbox.path());
    #[allow(clippy::cast_precision_loss)]
    let ours = [
        ("VAD_RMS_THRESHOLD", VAD_RMS_THRESHOLD),
        ("SPEAKER_TURN_GAP", SPEAKER_TURN_GAP),
        ("PCM_CHUNK_DURATION_S", PCM_CHUNK_DURATION_S),
        ("PCM_CHUNK_PAYLOAD_S", PCM_CHUNK_PAYLOAD_S),
        ("PCM_BATCH_INTERVAL_MS", PCM_BATCH_INTERVAL_MS as f64),
        ("PCM_MAX_BATCH_SIZE", PCM_MAX_BATCH_SIZE as f64),
        ("PCM_CHUNK_OVERLAP_MS", PCM_CHUNK_OVERLAP_MS as f64),
        ("PCM_SILENCE_WATCHDOG_S", PCM_SILENCE_WATCHDOG_S),
        ("PCM_REFINE_WINDOW_S", PCM_REFINE_WINDOW_S),
        ("PCM_REFINE_MONOLOG_CAP_S", PCM_REFINE_MONOLOG_CAP_S),
        ("CAPTURE_RESYNC_MAX_S", CAPTURE_RESYNC_MAX_S),
    ];
    for (name, value) in ours {
        assert_eq!(answer[name].as_f64(), Some(value), "{name}");
    }
}
