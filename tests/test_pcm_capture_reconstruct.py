import json
import struct
import wave

from scripts.pcm_capture_reconstruct import reconstruct_pid


def _sample(value: int) -> bytes:
    return struct.pack("<h", value)


def test_reconstruct_pid_orders_by_timestamps_when_seq_resets(tmp_path):
    """
    given: one participant's track reconnects and seq restarts at 0
    when: reconstructing captured chunks
    then: chunks are emitted in timeline order instead of seq order
    """
    raw_path = tmp_path / "pid.raw"
    raw_path.write_bytes(_sample(100) + _sample(200) + _sample(300))
    jsonl_path = tmp_path / "pid.jsonl"
    metas = [
        {
            "pid": "pid",
            "speaker_name": "Alice",
            "seq": 0,
            "chunk_start_ts": 0.0,
            "chunk_end_ts": 1.0,
            "sample_rate": 1,
            "byte_offset": 0,
            "num_bytes": 2,
        },
        {
            "pid": "pid",
            "speaker_name": "Alice",
            "seq": 1,
            "chunk_start_ts": 1.0,
            "chunk_end_ts": 2.0,
            "sample_rate": 1,
            "byte_offset": 2,
            "num_bytes": 2,
        },
        {
            "pid": "pid",
            "speaker_name": "Alice",
            "seq": 0,
            "chunk_start_ts": 2.0,
            "chunk_end_ts": 3.0,
            "sample_rate": 1,
            "byte_offset": 4,
            "num_bytes": 2,
        },
    ]
    jsonl_path.write_text("\n".join(json.dumps(meta) for meta in metas))

    out_wav = tmp_path / "out.wav"
    info = reconstruct_pid(raw_path, jsonl_path, out_wav)

    with wave.open(str(out_wav), "rb") as wf:
        frames = wf.readframes(wf.getnframes())
    samples = list(struct.unpack(f"<{len(frames) // 2}h", frames))

    assert samples == [100, 200, 300]
    assert info["chunk_count"] == 3
    assert info["gaps_count"] == 0
