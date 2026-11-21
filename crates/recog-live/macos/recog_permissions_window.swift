import AppKit
import SwiftUI

private enum Permission: String, CaseIterable {
    case microphone, systemAudio = "system-audio", zoom

    var title: String {
        switch self {
        case .microphone: return "Микрофон"
        case .systemAudio: return "Системный звук"
        case .zoom: return "Универсальный доступ для Zoom"
        }
    }

    var purpose: String {
        switch self {
        case .microphone: return "Чтобы распознавать вашу речь с микрофона."
        case .systemAudio: return "Чтобы распознавать речь собеседников, которая звучит на этом Mac."
        case .zoom: return "Чтобы определять, кто говорит в приложении Zoom."
        }
    }

    var symbol: String {
        switch self {
        case .microphone: return "mic"
        case .systemAudio: return "speaker.wave.2"
        case .zoom: return "accessibility"
        }
    }

    var settingsURL: URL {
        let anchor: String
        switch self {
        case .microphone: anchor = "Privacy_Microphone"
        case .systemAudio: anchor = "Privacy_ScreenCapture"
        case .zoom: anchor = "Privacy_Accessibility"
        }
        return URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)")!
    }

    func draggableApp(in root: URL) -> URL? {
        switch self {
        case .microphone: return nil
        case .systemAudio: return root.appendingPathComponent("Recog.app")
        case .zoom: return root.appendingPathComponent("RecogZoomHelper.app")
        }
    }
}

private enum Grant: String {
    case granted, notRequested = "not-requested", denied, restricted
    case notGranted = "not-granted", unknown, off

    var label: String {
        switch self {
        case .granted: return "Разрешено"
        case .notRequested: return "Ещё не запрашивали"
        case .denied: return "В доступе отказано"
        case .restricted: return "Ограничено системой"
        case .notGranted: return "Нет разрешения"
        case .unknown: return "Статус неизвестен"
        case .off: return "Выключено"
        }
    }
}

private struct CommandFailure: Error {
    let message: String
}

// This window never probes the system itself. Both status and requests go
// through the installer, which launches the correctly signed permission owners.
private func runCommand(in root: URL, request: Permission?) -> Result<[String: String], CommandFailure> {
    let process = Process()
    process.executableURL = root.appendingPathComponent("bin/recog")
    process.arguments = ["__macos-permission", request?.rawValue ?? "status"]
    process.currentDirectoryURL = root
    process.standardInput = FileHandle.nullDevice
    let output = Pipe()
    process.standardOutput = output
    do {
        try process.run()
    } catch {
        return .failure(CommandFailure(message: "Не удалось запустить bin/recog из папки установки."))
    }
    let data = output.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
        return .failure(CommandFailure(message: "Команда Recog завершилась с ошибкой (код \(process.terminationStatus))."))
    }
    guard let values = try? JSONDecoder().decode([String: String].self, from: data) else {
        return .failure(CommandFailure(message: "Не удалось прочитать ответ Recog о разрешениях."))
    }
    return .success(values)
}

@MainActor
private final class Permissions: ObservableObject {
    let root: URL
    let visible: [Permission]
    @Published private(set) var grants: [Permission: Grant] = [:]
    @Published private(set) var busy = false
    @Published private(set) var requesting: Permission?
    @Published private(set) var manual = Set<Permission>()
    @Published private(set) var failure: (permission: Permission?, message: String)?
    @Published var settingsError = false

    private var watching: Permission?
    private var deadline: TimeInterval = 0
    private var nextCheck: Task<Void, Never>?
    private var refreshOnCompletion = false

    init(root: URL, zoom: Bool) {
        self.root = root
        visible = zoom ? Permission.allCases : [.microphone, .systemAudio]
    }

    func refreshOnActivation() {
        if busy {
            refreshOnCompletion = true
        } else {
            run()
        }
    }

    func run(_ permission: Permission? = nil) {
        guard !busy else { return }
        busy = true
        requesting = permission
        nextCheck?.cancel()
        if let permission {
            manual.insert(permission)
            watch(permission)
        }
        if failure?.permission == permission || permission != nil {
            failure = nil
        }
        Task {
            let result = await Task.detached(priority: .userInitiated) { [root] in
                runCommand(in: root, request: permission)
            }.value
            busy = false
            requesting = nil
            switch result {
            case .success(let values):
                for item in visible {
                    grants[item] = values[item.rawValue].flatMap(Grant.init(rawValue:)) ?? .unknown
                }
                if let failed = failure?.permission, grants[failed] == .granted { failure = nil }
                if let watching, let state = grants[watching],
                   [.granted, .unknown, .restricted, .off].contains(state) {
                    self.watching = nil
                }
            case .failure(let error):
                failure = (permission, error.message)
                watching = nil
            }
            if refreshOnCompletion {
                refreshOnCompletion = false
                run()
            } else {
                scheduleCheck()
            }
        }
    }

    func openSettings(_ permission: Permission) {
        guard NSWorkspace.shared.open(permission.settingsURL) else {
            settingsError = true
            return
        }
        if grants[permission] != .granted {
            manual.insert(permission)
            watch(permission)
            scheduleCheck()
        }
    }

    private func watch(_ permission: Permission) {
        watching = permission
        deadline = ProcessInfo.processInfo.systemUptime + 120
    }

    // One delayed check after each completed command, only while the user
    // is granting this permission. Returning to the window always checks once.
    private func scheduleCheck() {
        nextCheck?.cancel()
        guard !busy, watching != nil, ProcessInfo.processInfo.systemUptime < deadline else { return }
        nextCheck = Task {
            do { try await Task.sleep(nanoseconds: 5_000_000_000) } catch { return }
            run()
        }
    }
}

private struct InstalledApp: View {
    let url: URL

    var body: some View {
        if FileManager.default.fileExists(atPath: url.path) {
            HStack(spacing: 8) {
                HStack(spacing: 4) {
                    Image(nsImage: NSWorkspace.shared.icon(forFile: url.path))
                        .resizable().frame(width: 22, height: 22)
                    Text(verbatim: url.lastPathComponent)
                }
                .padding(.horizontal, 6).padding(.vertical, 2)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 5))
                .onDrag { NSItemProvider(object: url as NSURL) }
                .accessibilityElement(children: .combine)
                .help("Перетащите \(url.lastPathComponent) в список приложений в настройках.")
                Button("Показать в Finder") {
                    NSWorkspace.shared.activateFileViewerSelecting([url])
                }
                .controlSize(.small)
                .accessibilityLabel("Показать \(url.lastPathComponent) в Finder")
            }
            Text("Перетащите приложение в список в системных настройках.")
                .font(.caption).foregroundStyle(.secondary)
        } else {
            Text("Приложение \(url.lastPathComponent) не найдено в папке установки.")
                .font(.caption).foregroundStyle(.secondary).help(url.path)
        }
    }
}

private struct PermissionRow: View {
    @ObservedObject var model: Permissions
    let permission: Permission

    private var grant: Grant? { model.grants[permission] }
    private var failed: Bool { model.failure?.permission == permission }
    private var needsSettings: Bool {
        grant == .denied || grant == .restricted || (model.manual.contains(permission) && grant == .notGranted)
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: permission.symbol)
                .font(.title3).foregroundStyle(.secondary)
                .frame(width: 34, height: 34)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 5) {
                Button { model.openSettings(permission) } label: {
                    HStack(spacing: 4) {
                        Text(permission.title).fontWeight(.semibold)
                        Image(systemName: "arrow.up.right.circle").font(.caption).foregroundStyle(.secondary)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(permission.title) — открыть системные настройки")
                .help("Открыть раздел «\(permission.title)» в системных настройках")
                Text(permission.purpose).foregroundStyle(.secondary)
                if failed, let error = model.failure {
                    Text(error.message).font(.caption)
                }
                if needsSettings, let url = permission.draggableApp(in: model.root) {
                    InstalledApp(url: url).padding(.top, 2)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            VStack(alignment: .trailing, spacing: 5) {
                if model.requesting == permission {
                    ProgressView().controlSize(.small).accessibilityLabel("Идёт запрос")
                    Text("Запрашиваем…").font(.caption)
                } else if failed {
                    action("Повторить запрос")
                } else if grant == .granted {
                    Label("Разрешено", systemImage: "checkmark.circle.fill")
                        .fontWeight(.medium).foregroundStyle(.green)
                } else {
                    Text(grant?.label ?? (model.busy ? "Проверяем…" : "Нет данных"))
                        .font(.caption).foregroundStyle(.secondary)
                    if grant == .off {
                        Button("Проверить снова") { model.run() }.disabled(model.busy)
                    } else if needsSettings {
                        Button("Открыть настройки") { model.openSettings(permission) }
                    } else {
                        action("Запросить…")
                    }
                }
            }
            .controlSize(.small)
        }
        .padding(14)
    }

    private func action(_ title: String) -> some View {
        Button(title) { model.run(permission) }
            .disabled(model.busy)
            .accessibilityLabel("\(title) \(permission.title)")
    }
}

private struct PermissionsView: View {
    @ObservedObject var model: Permissions

    @ViewBuilder private var appIcon: some View {
        let app = model.root.appendingPathComponent("Recog.app")
        let icon = Image(nsImage: NSWorkspace.shared.icon(forFile: app.path))
            .resizable().frame(width: 64, height: 64)
        if FileManager.default.fileExists(atPath: app.path) {
            let hint = "Перетащите Recog.app в список приложений в разделе записи экрана и системного звука."
            icon.onDrag { NSItemProvider(object: app as NSURL) }
                .accessibilityLabel("Приложение Recog")
                .accessibilityHint(hint)
                .help(hint)
        } else {
            icon.accessibilityHidden(true)
        }
    }

    var body: some View {
        VStack(spacing: 16) {
            appIcon
            Text("Разрешения Recog").font(.title.bold()).accessibilityAddTraits(.isHeader)
            VStack(spacing: 0) {
                ForEach(model.visible, id: \.self) { permission in
                    if permission != model.visible.first { Divider().padding(.horizontal, 14) }
                    PermissionRow(model: model, permission: permission)
                }
            }
            .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.quaternary))
            if model.failure != nil || model.grants.values.contains(.unknown) {
                HStack {
                    if let failure = model.failure, failure.permission == nil {
                        Text(failure.message).font(.callout)
                    }
                    Spacer()
                    Button("Повторить проверку") { model.run() }.disabled(model.busy)
                }
            }
            HStack {
                Text("Можно продолжить установку и выдать доступ позже.")
                    .font(.callout).foregroundStyle(.secondary)
                Spacer()
                Button("Продолжить") { NSApp.terminate(nil) }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(22)
        .frame(width: 568)
        .fixedSize(horizontal: false, vertical: true)
        .alert("Не удалось открыть системные настройки", isPresented: $model.settingsError) {
            Button("Закрыть") {}
        } message: {
            Text("Откройте «Конфиденциальность и безопасность» в системных настройках Mac.")
        }
    }
}

@MainActor
private final class WindowDelegate: NSObject, NSApplicationDelegate {
    let model: Permissions
    var window: NSWindow?

    init(_ model: Permissions) { self.model = model }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let controller = NSHostingController(rootView: PermissionsView(model: model))
        let window = NSWindow(contentViewController: controller)
        window.title = "Разрешения Recog"
        window.styleMask = [.titled, .closable, .miniaturizable]
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        self.window = window
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        model.refreshOnActivation()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard (arguments.count == 1 || (arguments.count == 2 && arguments[1] == "--zoom")),
      arguments[0].hasPrefix("/") else {
    fputs("Использование: recog-permissions /путь/к/установке [--zoom]\n", stderr)
    exit(64)
}
MainActor.assumeIsolated {
    let application = NSApplication.shared
    let root = URL(fileURLWithPath: arguments[0], isDirectory: true)
    let delegate = WindowDelegate(Permissions(root: root, zoom: arguments.count == 2))
    application.delegate = delegate
    application.setActivationPolicy(.regular)
    withExtendedLifetime(delegate) { application.run() }
}
