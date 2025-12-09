#!/usr/bin/env python3
"""
MP4 Notes - CLI-утилита для транскрибирования аудио/видео с диаризацией спикеров.

Использует GigaAM для ASR и pyannote.audio для диаризации.
Выход: Markdown-файл с разметкой по спикерам.

Основные команды:
- transcribe: транскрибирование файла с диаризацией
- enroll-speaker: добавление известного спикера в базу

Пример:
    python mp4_notes.py transcribe example.mp4 --out example_result.md
    python mp4_notes.py enroll-speaker slava sample1.wav --display-name "Слава"
"""

import argparse
import json
import sys
from datetime import datetime
from pathlib import Path

from recog import (
    DEFAULT_MODEL,
    DEFAULT_CHUNK_DURATION,
    DEFAULT_OVERLAP,
    transcribe_pipeline,
    enroll_speaker_pipeline,
    build_speaker_db_pipeline,
    live_pipeline,
)


def main():
    parser = argparse.ArgumentParser(
        prog="mp4-notes",
        description="CLI-утилита для транскрибирования аудио/видео с диаризацией спикеров"
    )

    subparsers = parser.add_subparsers(dest="command", help="Доступные команды")

    # Команда: transcribe
    transcribe_parser = subparsers.add_parser(
        "transcribe",
        help="Транскрибирование файла с диаризацией"
    )
    transcribe_parser.add_argument(
        "input",
        help="Путь к аудио/видео файлу (mp4, mp3, wav и др.)"
    )
    transcribe_parser.add_argument(
        "-o", "--out",
        help="Путь к выходному .md файлу (по умолчанию: input.md)"
    )
    transcribe_parser.add_argument(
        "-m", "--model",
        default=DEFAULT_MODEL,
        help=f"Модель GigaAM (по умолчанию: {DEFAULT_MODEL})"
    )
    transcribe_parser.add_argument(
        "--mode",
        choices=["longform", "simple"],
        default="simple",
        help="Режим транскрибирования (по умолчанию: simple)"
    )
    transcribe_parser.add_argument(
        "--device",
        help="Устройство (cuda/cpu/auto)"
    )
    transcribe_parser.add_argument(
        "--speaker-db",
        help="Путь к файлу базы спикеров (JSON)"
    )
    transcribe_parser.add_argument(
        "--no-timestamps",
        action="store_true",
        help="Не включать таймкоды в вывод"
    )
    transcribe_parser.add_argument(
        "--no-diarization",
        action="store_true",
        help="Отключить диаризацию (один спикер)"
    )
    # ADR-0007: удалён --pyannote3, унификация на pyannote 4.x
    transcribe_parser.add_argument(
        "--chunk-duration",
        type=float,
        default=DEFAULT_CHUNK_DURATION,
        help=f"Длительность чанка (сек) для режима simple (по умолчанию: {DEFAULT_CHUNK_DURATION})"
    )
    transcribe_parser.add_argument(
        "--overlap",
        type=float,
        default=DEFAULT_OVERLAP,
        help=f"Перекрытие чанков (сек) для режима simple (по умолчанию: {DEFAULT_OVERLAP})"
    )

    # Команда: live (ADR-0010)
    live_parser = subparsers.add_parser(
        "live",
        help="Live транскрипция с микрофона/BlackHole"
    )
    live_parser.add_argument(
        "--mic-device",
        default="auto",
        help="ID устройства микрофона или 'auto' для встроенного MacBook (по умолчанию: auto)"
    )
    live_parser.add_argument(
        "--system-device",
        help="ID устройства системного звука или 'auto' для BlackHole"
    )
    live_parser.add_argument(
        "--chunk-duration",
        type=float,
        default=3.0,
        help="Длительность чанка для ASR (сек, по умолчанию: 3.0)"
    )
    live_parser.add_argument(
        "--speaker-db",
        help="Путь к файлу базы спикеров (JSON)"
    )
    live_parser.add_argument(
        "--output",
        help="Сохранять транскрипт в файл"
    )
    live_parser.add_argument(
        "--no-diarization",
        action="store_true",
        help="Только ASR без идентификации спикеров"
    )
    live_parser.add_argument(
        "--list-devices",
        action="store_true",
        help="Показать доступные аудио устройства"
    )
    live_parser.add_argument(
        "--ktalk",
        action="store_true",
        help="Использовать KTalk Chrome extension для определения спикеров (вместо embedding-based)"
    )
    live_parser.add_argument(
        "--screencapture",
        action="store_true",
        help="Использовать ScreenCaptureKit для захвата системного звука (не нужен BlackHole, громкость работает)"
    )

    # Команда: meetings (просмотр сохранённых митингов)
    meetings_parser = subparsers.add_parser(
        "meetings",
        help="Просмотр сохранённых митингов"
    )
    meetings_parser.add_argument(
        "meeting_id",
        nargs="?",
        help="ID митинга для просмотра (без аргумента - список всех)"
    )
    meetings_parser.add_argument(
        "--json",
        action="store_true",
        help="Вывести в JSON формате"
    )

    # Команда: enroll-speaker
    enroll_parser = subparsers.add_parser(
        "enroll-speaker",
        help="Добавление известного спикера в базу"
    )
    enroll_parser.add_argument(
        "speaker_id",
        help="Идентификатор спикера (например: slava)"
    )
    enroll_parser.add_argument(
        "audio_files",
        nargs="+",
        help="Аудио/видео файлы с речью спикера"
    )
    enroll_parser.add_argument(
        "--display-name",
        required=True,
        help="Отображаемое имя спикера (например: 'Слава')"
    )
    enroll_parser.add_argument(
        "--speaker-db",
        default="speakers.json",
        help="Путь к файлу базы спикеров (по умолчанию: speakers.json)"
    )

    # Команда: build-speaker-db
    build_db_parser = subparsers.add_parser(
        "build-speaker-db",
        help="Создание базы спикеров из файла (диаризация + эмбеддинги)"
    )
    build_db_parser.add_argument(
        "input",
        help="Путь к аудио/видео файлу"
    )
    build_db_parser.add_argument(
        "--speaker-db",
        default="speakers.json",
        help="Путь к файлу базы спикеров (по умолчанию: speakers.json)"
    )
    build_db_parser.add_argument(
        "--mapping",
        nargs="*",
        help="Маппинг speaker_id:display_name (например: speaker_0:Руслан speaker_1:Вячеслав)"
    )

    args = parser.parse_args()

    if args.command == "transcribe":
        input_path = args.input
        output_path = args.out or str(Path(input_path).with_suffix(".md"))

        transcribe_pipeline(
            input_path=input_path,
            output_path=output_path,
            model_name=args.model,
            mode=args.mode,
            device=args.device,
            speaker_db_path=args.speaker_db,
            include_timestamps=not args.no_timestamps,
            no_diarization=args.no_diarization,
            chunk_duration=args.chunk_duration,
            overlap=args.overlap
        )

    elif args.command == "live":
        # Convert device ID to int if it's a number
        def parse_device(dev):
            if dev is None:
                return None
            try:
                return int(dev)
            except ValueError:
                return dev

        live_pipeline(
            mic_device=parse_device(args.mic_device),
            system_device=parse_device(args.system_device),
            speaker_db_path=args.speaker_db,
            chunk_duration=args.chunk_duration,
            output_file=args.output,
            no_diarization=args.no_diarization,
            list_devices=args.list_devices,
            use_ktalk=args.ktalk,
            use_screencapture=args.screencapture
        )

    elif args.command == "meetings":
        from meeting_storage import MeetingStorage
        storage = MeetingStorage()

        if args.meeting_id:
            # Показать конкретный митинг
            meeting = storage.get_meeting(args.meeting_id)
            if not meeting:
                print(f"Meeting not found: {args.meeting_id}", file=sys.stderr)
                sys.exit(1)

            if args.json:
                print(json.dumps(meeting, ensure_ascii=False, indent=2))
            else:
                meta = meeting["meta"]
                segments = meeting["segments"]

                # Meta info
                print(f"{'=' * 60}")
                print(f" Meeting: {meta['id']}")
                print(f" Source: {meta['source']}")
                if meta.get("title"):
                    print(f" Title: {meta['title']}")
                if meta["ktalk"].get("live_url"):
                    print(f" KTalk URL: {meta['ktalk']['live_url']}")
                print(f" Created: {meta['created_at']}")
                print(f" Duration: {meta['stats']['duration_seconds']}s")
                print(f" Segments: {meta['stats']['segment_count']}")
                print(f"{'=' * 60}")
                print()

                # Transcript in "Copy all" format
                def get_uncertain_suffix(seg):
                    if not seg.get("uncertain"):
                        return ""
                    if seg.get("uncertain_reason") == "mics_off":
                        return " (🔇?)"
                    return " (?)"

                # Конвертация в локальное время
                def ts_to_local(ts_iso: str) -> str:
                    """Конвертирует ISO timestamp в локальное HH:MM:SS."""
                    try:
                        dt = datetime.fromisoformat(ts_iso)
                        return dt.astimezone().strftime("%H:%M:%S")
                    except:
                        return ""

                lines = []
                for seg in segments:
                    speaker = seg["speaker"] + get_uncertain_suffix(seg)
                    time = ts_to_local(seg.get("ts", "")) or seg.get("ts_display", "")
                    text = seg["text"]
                    lines.append(f"**{speaker}** *[{time}]*: {text}")

                print("\n\n".join(lines))
        else:
            # Показать список всех митингов
            meetings = storage.list_meetings()

            if args.json:
                print(json.dumps(meetings, ensure_ascii=False, indent=2))
            else:
                if not meetings:
                    print("No meetings found.")
                else:
                    print(f"{'ID':<35} {'Source':<8} {'Segments':>8}  {'Duration':>10}  Title")
                    print("-" * 80)
                    for m in meetings:
                        duration = f"{m['duration_seconds']}s" if m['duration_seconds'] else "-"
                        title = m.get('title') or "(untitled)"
                        if len(title) > 25:
                            title = title[:22] + "..."
                        print(f"{m['id']:<35} {m['source']:<8} {m['segment_count']:>8}  {duration:>10}  {title}")

    elif args.command == "enroll-speaker":
        enroll_speaker_pipeline(
            speaker_id=args.speaker_id,
            audio_files=args.audio_files,
            display_name=args.display_name,
            speaker_db_path=args.speaker_db
        )

    elif args.command == "build-speaker-db":
        # Парсим маппинг
        speaker_mapping = None
        if args.mapping:
            speaker_mapping = {}
            for m in args.mapping:
                if ":" in m:
                    sid, name = m.split(":", 1)
                    speaker_mapping[sid] = name

        build_speaker_db_pipeline(
            input_path=args.input,
            speaker_db_path=args.speaker_db,
            speaker_mapping=speaker_mapping
        )

    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
