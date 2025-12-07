#!/usr/bin/env python3
"""Записывает голос с микрофона и добавляет в базу спикеров."""
import sys
import numpy as np
import sounddevice as sd
import tempfile
import scipy.io.wavfile as wav

def record_audio(duration=5, sample_rate=16000):
    """Записывает аудио с микрофона."""
    print(f"Запись {duration} секунд... Говорите!")
    audio = sd.rec(int(duration * sample_rate), samplerate=sample_rate, channels=1, dtype='float32')
    sd.wait()
    print("Запись завершена.")
    return audio.flatten()

def get_embedding(audio, sample_rate=16000):
    """Извлекает embedding через SpeechBrain ECAPA."""
    from speechbrain.inference.speaker import EncoderClassifier
    import torch
    
    classifier = EncoderClassifier.from_hparams(
        source="speechbrain/spkrec-ecapa-voxceleb",
        savedir="/tmp/speechbrain_ecapa"
    )
    
    # Конвертируем в tensor
    waveform = torch.from_numpy(audio).unsqueeze(0)
    
    # Извлекаем embedding
    embedding = classifier.encode_batch(waveform)
    return embedding.squeeze().cpu().numpy()

def main():
    if len(sys.argv) < 2:
        print("Использование: python add_speaker_live.py <имя_спикера> [длительность_сек]")
        sys.exit(1)
    
    speaker_name = sys.argv[1]
    duration = int(sys.argv[2]) if len(sys.argv) > 2 else 5
    
    # Записываем аудио
    audio = record_audio(duration)
    
    # Извлекаем embedding
    print("Извлечение embedding...")
    embedding = get_embedding(audio)
    print(f"Embedding shape: {embedding.shape}")
    
    # Загружаем базу и добавляем спикера
    import json
    from pathlib import Path
    
    db_path = Path(__file__).parent / "speakers_ecapa.json"
    
    if db_path.exists():
        with open(db_path, 'r') as f:
            db = json.load(f)
    else:
        db = {}
    
    # Генерируем ID
    speaker_id = speaker_name.lower().replace(" ", "_")
    
    if speaker_id in db:
        # Добавляем embedding к существующему спикеру
        db[speaker_id]["embeddings"].append({
            "embedding": embedding.tolist(),
            "source_file": "live_recording",
            "start_time": 0.0,
            "end_time": float(duration),
            "transcript": "live recording"
        })
        print(f"Добавлен embedding к существующему спикеру: {speaker_name}")
    else:
        # Создаём нового спикера
        db[speaker_id] = {
            "display_name": speaker_name,
            "embeddings": [{
                "embedding": embedding.tolist(),
                "source_file": "live_recording",
                "start_time": 0.0,
                "end_time": float(duration),
                "transcript": "live recording"
            }]
        }
        print(f"Создан новый спикер: {speaker_name}")
    
    # Сохраняем
    with open(db_path, 'w') as f:
        json.dump(db, f, ensure_ascii=False, indent=2)
    
    print(f"База сохранена в {db_path}")
    print(f"Всего спикеров: {len(db)}")

if __name__ == "__main__":
    main()
