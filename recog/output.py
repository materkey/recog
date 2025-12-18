"""
Функции для форматирования вывода (Markdown).
"""

from datetime import datetime
from typing import List, Optional

from .models import Segment


def format_time(seconds: float, short: bool = True) -> str:
    """
    Форматирует время.

    Args:
        seconds: время в секундах
        short: True для MM:SS, False для HH:MM:SS.mmm
    """
    seconds = max(0.0, seconds)
    hours = int(seconds // 3600)
    minutes = int((seconds % 3600) // 60)

    if short:
        secs = int(seconds % 60)
        if hours > 0:
            return f"{hours}:{minutes:02d}:{secs:02d}"
        else:
            return f"{minutes:02d}:{secs:02d}"
    else:
        secs = seconds % 60
        return f"{hours:02d}:{minutes:02d}:{secs:06.3f}"


def generate_markdown(
    segments: List[Segment],
    title: str,
    include_timestamps: bool = True,
    merge_same_speaker: bool = True,
    merge_gap_threshold: float = 2.0,
    meeting_date: Optional[str] = None
) -> str:
    """
    Генерирует Markdown-файл из сегментов.

    Формат вывода (стиль example_result.md):
    # title

    **Meeting Date:** ...

    ---

    **Speaker** *[MM:SS]*: text

    Args:
        segments: список сегментов с текстом и спикерами
        title: заголовок документа
        include_timestamps: включать ли таймкоды
        merge_same_speaker: объединять ли соседние реплики одного спикера
        merge_gap_threshold: максимальный промежуток для объединения (секунды)
        meeting_date: дата встречи (опционально)
    """
    lines = [f"# {title}", ""]

    # Добавляем дату встречи, если указана
    if meeting_date:
        lines.append(f"**Meeting Date:** {meeting_date}")
    else:
        # Генерируем текущую дату
        now = datetime.now()
        date_str = now.strftime("%d %b, %Y - %I:%M %p")
        lines.append(f"**Meeting Date:** {date_str}")

    lines.append("")
    lines.append("---")
    lines.append("")

    if not segments:
        lines.append("*Транскрипт пуст*")
        return "\n".join(lines)

    # Опционально объединяем соседние сегменты одного спикера
    if merge_same_speaker:
        merged = []
        current = None

        for seg in segments:
            if current is None:
                current = Segment(
                    text=seg.text,
                    start=seg.start,
                    end=seg.end,
                    speaker_id=seg.speaker_id,
                    speaker_label=seg.speaker_label
                )
            elif (current.speaker_id == seg.speaker_id and
                  seg.start - current.end <= merge_gap_threshold):
                # Объединяем
                current.text += " " + seg.text
                current.end = seg.end
            else:
                merged.append(current)
                current = Segment(
                    text=seg.text,
                    start=seg.start,
                    end=seg.end,
                    speaker_id=seg.speaker_id,
                    speaker_label=seg.speaker_label
                )

        if current:
            merged.append(current)

        segments = merged

    # Генерируем строки в формате: **Speaker** *[MM:SS]*: text
    for seg in segments:
        if include_timestamps:
            time_str = format_time(seg.start, short=True)
            line = f"**{seg.speaker_label}** *[{time_str}]*: {seg.text}"
        else:
            line = f"**{seg.speaker_label}**: {seg.text}"

        lines.append(line)

    return "\n".join(lines)
