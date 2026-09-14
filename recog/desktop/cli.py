"""Local-only configuration: no HTTP endpoint can select an inspector target."""

import json
from pathlib import Path


def configure_parser(subparsers):
    parser = subparsers.add_parser("desktop", help="Интеграция с настольным Толком")
    commands = parser.add_subparsers(dest="desktop_command", required=True)
    enable = commands.add_parser("enable", help="Включить автоподключение в web-сервере")
    enable.add_argument("--app", required=True, help="Путь к официальному AppImage или .app")
    commands.add_parser("disable", help="Выключить автоподключение новых процессов")
    commands.add_parser("attach", help="Подключить работающий Толк вручную")
    info = commands.add_parser("status", help="Состояние автоподключения")
    info.add_argument("--json", action="store_true")


def main(args, data_dir: Path) -> int:
    from .processes import application_path, find_targets
    from .service import status
    from .storage import prepare_bundle, read_json, write_json
    from .worker import run

    root = data_dir / "desktop"
    try:
        if args.desktop_command == "enable":
            app = application_path(args.app)
            source = Path(__file__).resolve().parents[2] / "browser-extension/dist"
            digest, _ = prepare_bundle(source, root)
            write_json(
                root / "config.json",
                {"enabled": True, "app": str(app), "bundle": digest},
            )
            print("Автоподключение включено. Оно работает, пока запущен web-сервер Recog.")
        elif args.desktop_command == "disable":
            config = read_json(root / "config.json")
            write_json(root / "config.json", {**config, "enabled": False})
            print(
                "Автоподключение выключено. Уже загруженное расширение действует до выхода из Толка."
            )
        elif args.desktop_command == "attach":
            config = read_json(root / "config.json")
            targets = find_targets(application_path(config["app"]))
            if len(targets) != 1:
                raise ValueError("Нужен ровно один работающий основной процесс выбранного Толка")
            result = run(root, targets[0])
            print(json.dumps(result, ensure_ascii=False))
            return 0 if result["state"] == "attached" else 1
        else:
            result = status(root)
            if args.json:
                print(json.dumps(result, ensure_ascii=False))
            else:
                print(result["state"] + (": " + result["reason"] if result.get("reason") else ""))
        return 0
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(f"Desktop integration: {exc}")
        return 1
