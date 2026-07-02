#!/usr/bin/env python3
"""
MP4 Notes - CLI-утилита для live-транскрибирования и управления митингами.

Использует GigaAM для ASR и KTalk Chrome extension для определения спикеров.

Основные команды:
- live: live-транскрипция с микрофона/системного звука
- meetings: просмотр сохранённых митингов

Пример:
    python mp4_notes.py live --ktalk --screencapture
    python mp4_notes.py meetings
"""

import argparse
import json
import sys
from datetime import datetime
from pathlib import Path

from recog import MIC_SPEAKER_NAME


def main():
    parser = argparse.ArgumentParser(
        prog="mp4-notes",
        description="CLI-утилита для live-транскрибирования и управления митингами"
    )

    subparsers = parser.add_subparsers(dest="command", help="Доступные команды")

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
    live_parser.add_argument(
        "--mic-speaker-name",
        default=MIC_SPEAKER_NAME,
        help=f"Имя спикера для микрофонного ввода (по умолчанию: {MIC_SPEAKER_NAME})"
    )
    live_parser.add_argument(
        "--no-save-audio",
        action="store_true",
        help="Не записывать аудио встреч (playback сегментов будет недоступен; "
             "по умолчанию аудио пишется и хранится 24 часа)"
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
    meetings_parser.add_argument(
        "--refresh-cache",
        action="store_true",
        help="Пересканировать внешние директории (обновить кэш)"
    )

    args = parser.parse_args()

    if args.command == "live":
        from recog.live import live_pipeline

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
            use_screencapture=args.screencapture,
            mic_speaker_name=args.mic_speaker_name,
            save_audio=not args.no_save_audio
        )

    elif args.command == "meetings":
        from meeting_storage import MeetingStorage, get_external_paths
        storage = MeetingStorage(external_paths=get_external_paths())

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
                conf = meta.get("conference", meta.get("ktalk", {}))
                if conf.get("live_url"):
                    print(f" Conference URL: {conf['live_url']}")
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
            meetings = storage.list_meetings(refresh_cache=args.refresh_cache)

            if args.json:
                print(json.dumps(meetings, ensure_ascii=False, indent=2))
            else:
                if not meetings:
                    print("No meetings found.")
                else:
                    print(f"{'ID':<35} {'Source':<12} {'Segments':>8}  {'Duration':>10}  Title")
                    print("-" * 85)
                    for m in meetings:
                        duration = f"{m['duration_seconds']}s" if m['duration_seconds'] else "-"
                        title = m.get('title') or "(untitled)"
                        if len(title) > 25:
                            title = title[:22] + "..."
                        source = m['source']
                        if m.get('_external'):
                            source = f"{source}*"  # Mark external with *
                        print(f"{m['id']:<35} {source:<12} {m['segment_count']:>8}  {duration:>10}  {title}")

    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
