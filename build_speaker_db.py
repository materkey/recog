#!/usr/bin/env python3
"""
Скрипт для создания базы спикеров с несколькими embeddings.
Извлекает сегменты из example.mp4 по таймкодам из example_result.md
"""
import json
import subprocess
import sys
import os
from pathlib import Path

# Сегменты для каждого спикера (start, end, transcript)
SEGMENTS = {
    "ruslan": {
        "display_name": "Руслан",
        "samples": [
            (0, 38, "Но, видимо, это какой-то азиатский штамм вируса..."),
            (276, 353, "Я бы не хотел такое делать, потому что чем больше репозиториев..."),  # 04:36-05:53
            (438, 466, "Я не вижу проблемы, что нас зовут на все pull-requests..."),  # 07:18-07:46
        ]
    },
    "slava": {
        "display_name": "Вячеслав",
        "samples": [
            (42, 50, "Понял. Ну, здоровья погибшим."),
            (149, 223, "Так-так-так, можем начать пока..."),  # 02:29-03:43
            (487, 538, "Я не раз в неделю, а именно когда дежурю только..."),  # 08:07-08:58
        ]
    },
    "kostya": {
        "display_name": "Костя Тимлид",
        "samples": [
            (223, 241, "Слушайте, а вот вопрос, может быть, разнести репозитории?"),  # 03:43-04:01
            (353, 423, "Ну просто смотри, тогда получается мы остаемся в ситуации..."),  # 05:53-07:03
            (510, 550, "Хорошо посмотреть, это не дешево..."),  # 08:30-09:10
        ]
    },
    "vlad": {
        "display_name": "Влад Воробъев",
        "samples": [
            (423, 427, "Ну, в инфру же нам, по идее, всё интересно..."),  # 07:03-07:07
            (903, 974, "По GitHub. Дали права на коммит..."),  # 15:03-16:14
        ]
    }
}

HF_TOKEN = os.environ.get("HF_TOKEN", "")
ROOT_DIR = Path(__file__).resolve().parent
VENV_PYTHON = str(ROOT_DIR / "venv-pyannote" / "bin" / "python")
DIARIZE_WORKER = str(ROOT_DIR / "diarize_worker.py")

def extract_embedding(wav_path: str) -> list:
    """Извлекает embedding из wav файла."""
    input_data = json.dumps({
        "audio_path": wav_path,
        "hf_token": HF_TOKEN,
        "similarity_threshold": 0.5
    })
    
    result = subprocess.run(
        [VENV_PYTHON, DIARIZE_WORKER],
        input=input_data,
        capture_output=True,
        text=True
    )
    
    if result.returncode != 0:
        print(f"Error: {result.stderr}", file=sys.stderr)
        return None
    
    data = json.loads(result.stdout)
    # Берём первый спикер (должен быть один)
    for spk_id, emb in data["embeddings"].items():
        return emb
    return None

def main():
    db = {}
    
    for speaker_id, info in SEGMENTS.items():
        print(f"\nОбработка {info['display_name']}...")
        embeddings = []
        
        for start, end, transcript in info["samples"]:
            duration = end - start
            wav_path = f"/tmp/{speaker_id}_{start}_{end}.wav"

            # Извлекаем аудио
            result = subprocess.run(
                [
                    "ffmpeg",
                    "-y",
                    "-i",
                    "example.mp4",
                    "-ss",
                    str(start),
                    "-t",
                    str(duration),
                    "-ar",
                    "16000",
                    "-ac",
                    "1",
                    wav_path,
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if result.returncode != 0:
                print(f"  [{start//60:02d}:{start%60:02d}] FAILED (ffmpeg)", file=sys.stderr)
                continue

            # Извлекаем embedding
            emb = extract_embedding(wav_path)
            if emb:
                embeddings.append({
                    "embedding": emb,
                    "source_file": "example.mp4",
                    "start_time": float(start),
                    "end_time": float(end),
                    "transcript": transcript
                })
                print(f"  [{start//60:02d}:{start%60:02d}-{end//60:02d}:{end%60:02d}] OK")
            else:
                print(f"  [{start//60:02d}:{start%60:02d}] FAILED")

            # Удаляем временный файл
            try:
                os.remove(wav_path)
            except FileNotFoundError:
                pass
        
        db[speaker_id] = {
            "display_name": info["display_name"],
            "embeddings": embeddings
        }
    
    # Сохраняем
    with open("speakers_multi.json", "w", encoding="utf-8") as f:
        json.dump(db, f, ensure_ascii=False, indent=2)
    
    print(f"\nБаза сохранена в speakers_multi.json")
    total = sum(len(s["embeddings"]) for s in db.values())
    print(f"Всего: {len(db)} спикеров, {total} embeddings")

if __name__ == "__main__":
    main()
