#!/usr/bin/env python3
"""
Embedding extraction worker for speaker enrollment.
Runs in venv-pyannote to use ECAPA-TDNN.

Usage:
    echo '{"audio_path": "/path/to/audio.wav"}' | ./venv-pyannote/bin/python embedding_worker.py

Input JSON:
    {"audio_path": "/path/to/audio.wav"}

Output JSON:
    {"embedding": [float, ...], "duration": float}
"""

import json
import sys

import numpy as np
import soundfile as sf
import torch
from speechbrain.inference.speaker import EncoderClassifier


def extract_embedding(audio_path: str) -> dict:
    """
    Extract ECAPA-TDNN embedding from audio file.

    Returns dict with:
        - embedding: list of 192 floats
        - duration: audio duration in seconds
    """
    # Load ECAPA-TDNN model
    classifier = EncoderClassifier.from_hparams(
        source="speechbrain/spkrec-ecapa-voxceleb",
        savedir="/tmp/speechbrain_ecapa"
    )

    # Load audio
    waveform, sample_rate = sf.read(audio_path)
    if len(waveform.shape) > 1:
        waveform = waveform.mean(axis=1)  # stereo -> mono

    duration = len(waveform) / sample_rate

    # Extract embedding
    audio_tensor = torch.from_numpy(waveform).float().unsqueeze(0)
    embedding = classifier.encode_batch(audio_tensor)

    return {
        "embedding": embedding.squeeze().cpu().numpy().tolist(),
        "duration": duration
    }


def main():
    try:
        input_data = json.load(sys.stdin)
        audio_path = input_data["audio_path"]

        result = extract_embedding(audio_path)
        print(json.dumps(result))

    except Exception as e:
        print(json.dumps({"error": str(e)}))
        sys.exit(1)


if __name__ == "__main__":
    main()
