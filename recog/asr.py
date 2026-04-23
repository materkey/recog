"""
Модуль ASR (Automatic Speech Recognition) на базе GigaAM.
"""

import re
import sys
import threading
from typing import Dict, List, Optional

import numpy as np
import torch

from .config import DEFAULT_MODEL, DEFAULT_CHUNK_DURATION, DEFAULT_OVERLAP
from .models import Segment


class ASRModule:
    """Модуль для транскрибирования с GigaAM."""

    def __init__(self, model_name: str = DEFAULT_MODEL, device: Optional[str] = None):
        import gigaam
        from gigaam.preprocess import SAMPLE_RATE

        self.gigaam = gigaam
        self.SAMPLE_RATE = SAMPLE_RATE
        self.model_name = model_name
        self.device = device
        self.model = None
        # Serializes model load and model.forward across threads: live
        # mic/system ASR runs on the main loop, PerSpeakerIngest batches run
        # on a worker thread, and PyTorch modules are not safe for concurrent
        # forward on a shared instance.
        self._model_lock = threading.Lock()

    def load_model(self, quiet: bool = False):
        """Загружает модель GigaAM."""
        if self.model is None:
            with self._model_lock:
                if self.model is None:
                    if not quiet:
                        print(f"Загрузка модели {self.model_name}...", file=sys.stderr)
                    self.model = self.gigaam.load_model(self.model_name, device=self.device)
        return self.model

    def transcribe_longform(self, audio_path: str) -> List[Segment]:
        """Транскрибирует с использованием longform режима (pyannote VAD)."""
        model = self.load_model()
        results = model.transcribe_longform(audio_path)
        results = self._remove_duplicate_words(results)

        segments = []
        for r in results:
            start, end = r["boundaries"]
            segments.append(Segment(
                text=r["transcription"],
                start=start,
                end=end
            ))
        return segments

    def transcribe_simple(
        self,
        audio_path: str,
        chunk_duration: float = DEFAULT_CHUNK_DURATION,
        overlap: float = DEFAULT_OVERLAP,
        quiet: bool = False
    ) -> List[Segment]:
        """
        Транскрибирует, разбивая на фиксированные чанки.
        Не требует pyannote VAD.
        """
        from gigaam.preprocess import load_audio
        from tqdm import tqdm

        model = self.load_model()
        audio = load_audio(audio_path)
        total_samples = audio.shape[0]
        total_duration = total_samples / self.SAMPLE_RATE

        if chunk_duration <= 0:
            raise ValueError(f"chunk_duration must be > 0, got {chunk_duration}")
        if overlap < 0:
            raise ValueError(f"overlap must be >= 0, got {overlap}")
        if overlap >= chunk_duration:
            raise ValueError(
                f"overlap must be < chunk_duration (got overlap={overlap}, chunk_duration={chunk_duration})"
            )

        chunk_samples = int(chunk_duration * self.SAMPLE_RATE)
        overlap_samples = int(overlap * self.SAMPLE_RATE)
        step_samples = chunk_samples - overlap_samples
        if step_samples <= 0:
            raise ValueError(
                f"Invalid chunking: step_samples must be > 0 (got {step_samples}); "
                f"choose smaller overlap or larger chunk_duration."
            )

        device = model._device
        dtype = model._dtype

        results = []
        start_sample = 0

        # Подсчёт количества чанков для прогресс-бара
        num_chunks = max(1, (total_samples - overlap_samples) // step_samples + 1)
        if not quiet:
            print(f"Длительность аудио: {total_duration:.1f} сек ({total_duration/60:.1f} мин)", file=sys.stderr)

        with tqdm(total=num_chunks, desc="ASR", unit="chunk", file=sys.stderr, disable=quiet) as pbar:
            while start_sample < total_samples:
                end_sample = min(start_sample + chunk_samples, total_samples)
                chunk = audio[start_sample:end_sample]

                if chunk.shape[0] < self.SAMPLE_RATE * 0.5:
                    break

                wav = chunk.to(device).to(dtype).unsqueeze(0)
                length = torch.full([1], wav.shape[-1], device=device)

                with torch.inference_mode():
                    encoded, encoded_len = model.forward(wav, length)
                    # Upstream decode() returns List[Tuple[str, token_ids, token_frames)];
                    # we only need the first sample's text.
                    text = model.decoding.decode(model.head, encoded, encoded_len)[0][0]

                if text.strip():
                    start_time = start_sample / self.SAMPLE_RATE
                    end_time = end_sample / self.SAMPLE_RATE
                    results.append({
                        "transcription": text,
                        "boundaries": (start_time, end_time)
                    })

                start_sample += step_samples
                pbar.update(1)

        results = self._remove_duplicate_words(results)

        segments = []
        for r in results:
            start, end = r["boundaries"]
            segments.append(Segment(
                text=r["transcription"],
                start=start,
                end=end
            ))
        return segments

    def transcribe_batch(self, pcm_list: List[np.ndarray]) -> List[str]:
        """Batched forward pass for per-speaker PCM chunks.

        Pads inputs to the max-length in the batch, runs a single
        `model.forward` + `model.decoding.decode`, and returns decoded texts
        in the same order as the input list.

        Args:
            pcm_list: list of 1-D numpy arrays containing audio at
                `SAMPLE_RATE`.  Accepts either Int16 (LE) or float32 in
                [-1, 1]; Int16 is auto-normalized.

        Returns:
            List of transcribed texts, one per input, in input order.
        """
        if not pcm_list:
            return []

        model = self.load_model(quiet=True)
        device = model._device
        dtype = model._dtype

        float_list: List[np.ndarray] = []
        for pcm in pcm_list:
            if pcm.dtype == np.int16:
                f = pcm.astype(np.float32) / 32768.0
            elif pcm.dtype == np.float32:
                f = pcm
            else:
                f = pcm.astype(np.float32)
            float_list.append(f)

        lengths = [len(f) for f in float_list]
        max_len = max(lengths)
        padded = np.zeros((len(float_list), max_len), dtype=np.float32)
        for i, f in enumerate(float_list):
            if len(f) > 0:
                padded[i, :len(f)] = f

        wav_batch = torch.from_numpy(padded).to(device).to(dtype)
        length_batch = torch.tensor(lengths, dtype=torch.long, device=device)

        with self._model_lock, torch.inference_mode():
            encoded, encoded_len = model.forward(wav_batch, length_batch)
            # Upstream decode() returns List[Tuple[str, token_ids, token_frames)];
            # token ids/frames are unused here.
            decoded = model.decoding.decode(model.head, encoded, encoded_len)

        return [entry[0] for entry in decoded]

    def _remove_duplicate_words(
        self,
        results: List[Dict],
        max_overlap_words: int = 10
    ) -> List[Dict]:
        """Удаляет дублирующиеся слова на границах сегментов."""
        if len(results) < 2:
            return results

        cleaned = [results[0].copy()]

        for i in range(1, len(results)):
            prev_text = cleaned[-1]["transcription"]
            curr_text = results[i]["transcription"]

            prev_words = prev_text.split()
            curr_words = curr_text.split()

            if not prev_words or not curr_words:
                cleaned.append(results[i].copy())
                continue

            best_overlap = 0
            check_words = min(max_overlap_words, len(prev_words), len(curr_words))

            for overlap_len in range(check_words, 0, -1):
                prev_suffix = prev_words[-overlap_len:]
                curr_prefix = curr_words[:overlap_len]

                prev_normalized = [re.sub(r'[^\w]', '', w.lower()) for w in prev_suffix]
                curr_normalized = [re.sub(r'[^\w]', '', w.lower()) for w in curr_prefix]

                if prev_normalized == curr_normalized:
                    best_overlap = overlap_len
                    break

            if best_overlap > 0:
                new_text = " ".join(curr_words[best_overlap:])
            else:
                new_text = curr_text

            if new_text.strip():
                cleaned.append({
                    "transcription": new_text,
                    "boundaries": results[i]["boundaries"]
                })

        return cleaned
