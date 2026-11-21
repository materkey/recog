import AppKit
import SwiftUI

private let files = FileManager.default
private let meetings = URL(string: "http://127.0.0.1:9080")!
private let units = ["live", "frontend", "copilot-channel", "copilot-supervisor"]

private struct Problem: Error {
    let text: String
    init(_ text: String) { self.text = text }
}

private struct Release: Comparable {
    let core: [Int]
    let prerelease: [String]

    init?(_ tag: String) {
        let pattern = #"^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$"#
        guard tag.range(of: pattern, options: .regularExpression) != nil else { return nil }
        let version = tag.drop(while: { $0 == "v" }).prefix(while: { $0 != "+" })
            .split(separator: "-", maxSplits: 1)
        core = version[0].split(separator: ".").compactMap { Int($0) }
        guard core.count == 3 else { return nil }
        prerelease = version.count > 1 ? version[1].components(separatedBy: ".") : []
    }

    static func < (lhs: Self, rhs: Self) -> Bool {
        if lhs.core != rhs.core { return lhs.core.lexicographicallyPrecedes(rhs.core) }
        if lhs.prerelease.isEmpty || rhs.prerelease.isEmpty { return !lhs.prerelease.isEmpty && rhs.prerelease.isEmpty }
        for (a, b) in zip(lhs.prerelease, rhs.prerelease) where a != b {
            let numericA = a.allSatisfy(\.isNumber), numericB = b.allSatisfy(\.isNumber)
            if numericA != numericB { return numericA }
            if numericA && a.count != b.count { return a.count < b.count }
            return a < b
        }
        return lhs.prerelease.count < rhs.prerelease.count
    }
}

private enum InstallPlan: Equatable {
    case install, update, repair, ready, blocked
}

private struct Inspection {
    var plan: InstallPlan = .blocked
    var title = ""
    var detail = ""
    var button = ""
    var canManage = false
}

@MainActor
private struct Layout {
    let home: URL
    let package: URL
    var runtime: URL { home.appendingPathComponent(".local/share/recog/runtime") }
    var userBin: URL { home.appendingPathComponent(".local/bin") }
    var extensionFolder: URL { runtime.appendingPathComponent("browser-extension/dist") }
    var bin: URL { runtime.appendingPathComponent("bin") }
    private var agents: URL { home.appendingPathComponent("Library/LaunchAgents") }

    var setupArguments: [String] {
        // Let the release seed its conservative defaults on the first service
        // install. An existing installation may intentionally have no .env.
        let installed = units.contains { files.fileExists(atPath: agents.appendingPathComponent("com.recog.\($0).plist").path) }
        return installed ? ["setup", "--no-env"] : ["setup"]
    }

    init() {
        let env = ProcessInfo.processInfo.environment
        home = env["HOME"].flatMap { $0.hasPrefix("/") ? URL(fileURLWithPath: $0, isDirectory: true) : nil }
            ?? files.homeDirectoryForCurrentUser
        package = (Bundle.main.resourceURL ?? Bundle.main.bundleURL).appendingPathComponent("recog")
    }

    var environment: [String: String] {
        var env = ProcessInfo.processInfo.environment
        env["HOME"] = home.path
        env["PATH"] = [bin.path, userBin.path, "/opt/homebrew/bin", "/usr/local/bin",
                       "/usr/bin", "/bin", "/usr/sbin", "/sbin"].joined(separator: ":")
        return env
    }

    func executable(_ file: URL) -> Bool {
        var directory: ObjCBool = false
        return files.fileExists(atPath: file.path, isDirectory: &directory) && !directory.boolValue &&
            files.isExecutableFile(atPath: file.path)
    }

    private func tag(_ root: URL) throws -> String? {
        do {
            return try String(contentsOf: root.appendingPathComponent("recog-release"), encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines)
        } catch let error as NSError where error.domain == NSCocoaErrorDomain && error.code == NSFileReadNoSuchFileError {
            return nil
        }
    }

    // service start operates on the user's registered labels. Do not start or
    // replace someone else's jobs just because a release directory also exists.
    private func installedServices() throws -> Int {
        var count = 0
        for name in units {
            let url = agents.appendingPathComponent("com.recog.\(name).plist")
            let data: Data
            do { data = try Data(contentsOf: url) }
            catch let e as NSError where e.domain == NSCocoaErrorDomain && e.code == NSFileReadNoSuchFileError { continue }
            guard let plist = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
                  let wd = plist["WorkingDirectory"] as? String,
                  URL(fileURLWithPath: wd).standardizedFileURL.path == runtime.standardizedFileURL.path,
                  let program = (plist["ProgramArguments"] as? [String])?.first,
                  program.hasPrefix(runtime.path + "/") else {
                throw Problem("Службы Recog принадлежат другой установке. Обновите её из исходной папки или завершите переход на эту версию отдельно.")
            }
            count += 1
        }
        return count
    }

    var sameVersion: Bool {
        guard let local = try? tag(runtime), let source = try? tag(package), let version = Release(source) else { return false }
        return Release(local) == version
    }

    func inspect() -> Inspection {
        do {
            let registered = try installedServices()
            guard !files.fileExists(atPath: runtime.appendingPathComponent("crates/recog-live/Cargo.toml").path) else {
                throw Problem("В папке установки находится рабочая копия Recog. Обновите её из исходной папки.")
            }
            let installedTag = try tag(runtime)
            let installed = installedTag.flatMap(Release.init)
            if installedTag != nil && installed == nil { throw Problem("Не удалось определить установленную версию. Проверьте файл recog-release перед обновлением.") }
            let inPackage = try tag(package)
            guard let label = inPackage, let version = Release(label) else {
                throw Problem("В приложении отсутствует версия комплекта Recog. Снова скопируйте приложение с установочного диска.")
            }
            let newer = installed.map { $0 > version } ?? false
            let same = installed == version
            let hasCommand = executable(bin.appendingPathComponent("recog"))
            let owned = installed != nil && hasCommand && registered == units.count
            let installedTitle = "Установлен Recog \(installedTag ?? label)"
            let hasWeb = executable(bin.appendingPathComponent("recog-web"))
            let hasPackagedAudio = executable(bin.appendingPathComponent("ffmpeg")) && executable(bin.appendingPathComponent("recog-web-release"))
            let hasPanel = executable(bin.appendingPathComponent("recog-permissions"))
            let hasExtension = files.fileExists(atPath: extensionFolder.appendingPathComponent("manifest.json").path)
            if same || newer {
                // A later release may implement audio playback differently.
                if owned && hasWeb && hasPanel && hasExtension && (newer || hasPackagedAudio) {
                    return Inspection(plan: .ready, title: installedTitle,
                        detail: newer ? "На этом Mac уже установлена более новая версия." : "", canManage: true)
                }
                if newer {
                    return Inspection(plan: .blocked, title: installedTitle,
                        detail: "Для восстановления этой установки нужно приложение Recog версии \(installedTag!) или новее.",
                        canManage: owned)
                }
            }
            guard executable(package.appendingPathComponent("bin/recog")),
                  executable(package.appendingPathComponent("bin/recog-permissions")),
                  executable(package.appendingPathComponent("bin/recog-web")),
                  executable(package.appendingPathComponent("bin/recog-web-release")),
                  executable(package.appendingPathComponent("bin/ffmpeg")) else {
                throw Problem("В комплекте отсутствуют необходимые программы Recog или ffmpeg для звука. Снова скопируйте приложение с установочного диска.")
            }
            if same {
                return Inspection(plan: .repair, title: installedTitle, detail: "Часть компонентов отсутствует. Их можно установить заново.",
                                  button: "Восстановить установку", canManage: owned)
            }
            if installed != nil {
                return Inspection(plan: .update, title: "Доступен Recog \(label)", detail: "Установлена версия \(installedTag!). Записи и настройки сохранятся.",
                                  button: "Обновить Recog", canManage: owned)
            }
            return Inspection(plan: .install, title: "Установить Recog \(label) на этот Mac",
                              detail: "Recog распознаёт речь во встречах. Для первой загрузки модели может понадобиться интернет.",
                              button: "Установить Recog")
        } catch {
            return Inspection(plan: .blocked, title: "Установка недоступна",
                              detail: (error as? Problem)?.text ?? "Не удалось прочитать сведения об установке или её службах.")
        }
    }
}

private struct Output {
    let code: Int32
    let text: String

    func requireSuccess() throws {
        guard code != 0 else { return }
        let message: String
        if text.contains("A conference is active") { message = "Идёт встреча. Дождитесь её окончания и повторите установку." }
        else if text.contains("another checkout") { message = "Службы Recog принадлежат другой установке. Обновите её из исходной папки." }
        else if text.contains("ffmpeg") && text.contains("[missing]") { message = "Службам записи недоступен ffmpeg. Проверьте комплект приложения и повторите установку." }
        else if text.contains("No installed services") { message = "Службы ещё не установлены. Проверьте установку повторно." }
        else if text.contains("No space left") { message = "На диске недостаточно свободного места. Освободите место и повторите попытку." }
        else { message = "Recog завершил операцию с кодом \(code). Проверьте доступ к папке установки и настройки Recog, затем повторите попытку." }
        throw Problem(message)
    }
}

private enum Operation { case installing, starting, status, permissions, panel }
private enum Retry { case refresh, repeatSetup, start, permissions, meetings }
private enum ServiceState { case unknown, running, webOnly, stopped }

@MainActor
private final class Manager: ObservableObject {
    let layout = Layout()
    @Published var installation: Inspection
    @Published var operation: Operation?
    @Published var progress = ""
    @Published var error: String?
    @Published var services = ServiceState.unknown
    private var child: Process?
    private var quitting = false
    private var retry = Retry.refresh

    init() { installation = layout.inspect() }

    // Drain continuously even for large downloads, retain only a bounded tail
    // for error classification. Personal output never goes to the UI or a file.
    private func command(_ exe: URL, _ args: [String]) async throws -> Output {
        let process = Process()
        process.executableURL = exe
        process.arguments = args
        process.currentDirectoryURL = layout.home
        process.environment = layout.environment
        process.standardInput = FileHandle.nullDevice
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        do { try process.run() }
        catch { throw Problem("Не удалось открыть компонент \(exe.lastPathComponent). Проверьте установку Recog и разрешение macOS на запуск приложения.") }
        child = process
        let installing = operation == .installing
        let result = await Task.detached(priority: .userInitiated) { [weak self] in
            var tail = Data()
            var announcedExtension = false, announcedUnits = false
            while true {
                let part = pipe.fileHandleForReading.availableData
                if part.isEmpty { break }
                tail.append(part)
                if tail.count > 65536 { tail = Data(tail.suffix(65536)) }
                if installing && !announcedUnits {
                    let text = String(decoding: tail, as: UTF8.self)
                    if text.contains("Installed ") && text.contains(".plist") {
                        announcedUnits = true
                        await self?.setProgress("Запускаем службы Recog…")
                    } else if !announcedExtension && text.contains("Chrome: chrome://extensions") {
                        announcedExtension = true
                        await self?.setProgress("Подготавливаем звук и модель распознавания…")
                    }
                }
            }
            process.waitUntilExit()
            return Output(code: process.terminationStatus, text: String(decoding: tail, as: UTF8.self))
        }.value
        child = nil
        return result
    }

    private func setProgress(_ text: String) { progress = text }

    private func begin(_ op: Operation, _ label: String) -> Bool {
        guard operation == nil, !quitting else { return false }
        error = nil
        retry = .refresh
        operation = op
        progress = label
        return true
    }

    private func failed(_ cause: Error, retry: Retry) {
        error = (cause as? Problem)?.text ?? "Не удалось завершить операцию."
        self.retry = retry
    }

    private func end() {
        operation = nil
        if quitting { NSApp.reply(toApplicationShouldTerminate: true) }
    }

    func refresh() {
        guard operation == nil, !quitting else { return }
        installation = layout.inspect()
        guard installation.canManage else { services = .unknown; return }
        guard begin(.status, "Проверяем службы…") else { return }
        Task {
            do {
                let result = try await command(layout.bin.appendingPathComponent("recog"), ["service", "status"])
                try result.requireSuccess()
                let rows = result.text.components(separatedBy: .newlines)
                let names = ["live", "frontend"]
                guard names.allSatisfy({ name in rows.contains { $0.hasPrefix("\(name): ") } }) else {
                    throw Problem("Не удалось определить состояние служб Recog.")
                }
                let web = rows.contains { $0.hasPrefix("frontend: Running(") }
                let live = rows.contains { $0.hasPrefix("live: Running(") }
                services = web ? (live ? .running : .webOnly) : .stopped
            } catch {
                services = .unknown
                failed(error, retry: .refresh)
            }
            end()
        }
    }

    func install(repeatSetup: Bool = false) {
        guard operation == nil else { return }
        let current = layout.inspect()
        guard current.plan == installation.plan else { refresh(); return }
        installation = current
        let repeatSame = repeatSetup && layout.sameVersion && current.plan == .ready
        guard (repeatSame || [.install, .update, .repair].contains(current.plan)),
              begin(.installing, "Устанавливаем Recog…") else { return }
        Task {
            do {
                let result = try await command(layout.package.appendingPathComponent("bin/recog"), layout.setupArguments)
                try result.requireSuccess()
                installation = layout.inspect()
                guard installation.plan == .ready else {
                    throw Problem("Установка не завершена. " + installation.detail)
                }
                end()
                permissions()
            } catch {
                installation = layout.inspect()
                failed(error, retry: .repeatSetup)
                end()
            }
        }
    }

    func start() {
        guard operation == nil else { return }
        installation = layout.inspect()
        guard installation.canManage, begin(.starting, "Запускаем службы…") else { return }
        Task {
            do {
                let result = try await command(layout.bin.appendingPathComponent("recog"), ["service", "start"])
                try result.requireSuccess()
                end()
                refresh()
            } catch {
                failed(error, retry: .start)
                end()
            }
        }
    }

    func permissions() {
        guard operation == nil else { return }
        installation = layout.inspect()
        guard installation.canManage, begin(.permissions, "Проверяем разрешения…") else { return }
        Task {
            do {
                let status = try await command(layout.bin.appendingPathComponent("recog"), ["__macos-permission", "status"])
                try status.requireSuccess()
                // The CLI can print configuration notices to stderr.
                let zoom = status.text.components(separatedBy: .newlines).reversed().compactMap {
                    (try? JSONDecoder().decode([String: String].self, from: Data($0.utf8)))?["zoom"]
                }.first
                guard let zoom, ["off", "granted", "not-granted", "not-requested", "denied", "restricted", "unknown"].contains(zoom) else {
                    throw Problem("Не удалось проверить настройки Zoom. Повторите проверку разрешений.")
                }
                if quitting { end(); return }
                operation = .panel
                progress = "Открыто окно разрешений"
                let args = [layout.runtime.path] + (zoom == "off" ? [] : ["--zoom"])
                let panel = try await command(layout.bin.appendingPathComponent("recog-permissions"), args)
                try panel.requireSuccess()
                end()
                if !quitting { refresh() }
            } catch {
                failed(error, retry: .permissions)
                end()
            }
        }
    }

    func openMeetings() {
        if !NSWorkspace.shared.open(meetings) {
            failed(Problem("Не удалось открыть локальные встречи в браузере."), retry: .meetings)
        }
    }

    func revealExtension() {
        guard files.fileExists(atPath: layout.extensionFolder.appendingPathComponent("manifest.json").path) else {
            failed(Problem("Папка расширения не установлена. Проверьте установку повторно."), retry: .refresh)
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([layout.extensionFolder])
    }

    func tryAgain() {
        let action = retry
        error = nil
        retry = .refresh
        switch action {
        case .refresh: refresh()
        case .repeatSetup:
            installation = layout.inspect()
            install(repeatSetup: true)
        case .start: start()
        case .permissions: permissions()
        case .meetings: openMeetings()
        }
    }

    func terminate() -> NSApplication.TerminateReply {
        switch operation {
        case .installing, .starting:
            return .terminateCancel
        case .status, .permissions, .panel:
            quitting = true
            if operation == .panel, let child, child.isRunning {
                let app = NSRunningApplication(processIdentifier: child.processIdentifier)
                if app?.terminate() != true { child.terminate() }
            }
            return .terminateLater
        case nil: return .terminateNow
        }
    }
}

private struct ManagerView: View {
    @ObservedObject var model: Manager
    private var info: Inspection { model.installation }

    var body: some View {
        VStack(alignment: .leading, spacing: 19) {
            HStack(spacing: 14) {
                Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 68, height: 68).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Recog").font(.system(size: 32, weight: .bold))
                    Text("Распознавание встреч на этом Mac").foregroundStyle(.secondary)
                }
            }
            VStack(alignment: .leading, spacing: 10) {
                Text(info.title).font(.title3.bold()).accessibilityAddTraits(.isHeader)
                if !info.detail.isEmpty { Text(info.detail).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
                if model.operation != nil {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(model.progress)
                    }
                } else {
                    if !info.button.isEmpty { Button(info.button) { model.install() }.buttonStyle(.borderedProminent) }
                    if info.canManage {
                        HStack {
                            Label(serviceLabel, systemImage: model.services == .running ? "checkmark.circle.fill" : "circle.dashed")
                                .foregroundStyle(model.services == .running ? Color.green : Color.secondary)
                            Button { model.refresh() } label: { Image(systemName: "arrow.clockwise") }
                                .buttonStyle(.borderless)
                                .accessibilityLabel("Проверить состояние служб")
                                .help("Проверить состояние служб")
                            Spacer()
                            if model.services != .running {
                                Button("Запустить службы") { model.start() }
                            }
                            if model.services == .running || model.services == .webOnly {
                                Button("Открыть встречи") { model.openMeetings() }.buttonStyle(.borderedProminent)
                            }
                        }
                    } else if info.plan == .blocked && model.error == nil {
                        Button("Проверить снова") { model.refresh() }
                    }
                }
            }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
            if let error = model.error {
                HStack(alignment: .top) {
                    Image(systemName: "exclamationmark.circle").foregroundStyle(.orange)
                    Text(error).fixedSize(horizontal: false, vertical: true)
                    Spacer()
                    Button("Повторить") { model.tryAgain() }
                }
            }
            if info.canManage {
                VStack(spacing: 12) {
                    row("mic", "Разрешения на запись", "Доступ к микрофону и звуку встречи.", "Настроить…", model.permissions)
                    Divider()
                    row("puzzlepiece.extension", "Расширение браузера", "Выберите эту папку при загрузке расширения в Chrome.", "Показать в Finder", model.revealExtension)
                }
                .disabled(model.operation != nil)
            }
        }
        .padding(24).frame(width: 540).fixedSize(horizontal: false, vertical: true)
    }

    private var serviceLabel: String {
        switch model.services {
        case .running: return "Службы работают"
        case .webOnly: return "Служба записи остановлена"
        case .stopped: return "Требуется запуск служб"
        case .unknown: return "Состояние служб неизвестно"
        }
    }

    private func row(_ icon: String, _ title: String, _ text: String, _ action: String, _ run: @escaping () -> Void) -> some View {
        HStack(spacing: 12) {
            Image(systemName: icon).font(.title3).foregroundStyle(.secondary).frame(width: 28).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).fontWeight(.semibold)
                Text(text).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            Button(action, action: run)
        }
    }
}

@MainActor
private final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let model = Manager()
    var window: NSWindow?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let window = NSWindow(contentViewController: NSHostingController(rootView: ManagerView(model: model)))
        window.title = "Recog"
        window.styleMask = [.titled, .closable, .miniaturizable]
        window.isReleasedWhenClosed = false
        window.delegate = self
        self.window = window
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        model.refresh()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        window?.deminiaturize(nil)
        window?.makeKeyAndOrderFront(nil)
        if model.error == nil { model.refresh() }
        return true
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        NSApp.terminate(nil)
        return false
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        let reply = model.terminate()
        if reply == .terminateCancel, let window, window.attachedSheet == nil {
            let alert = NSAlert()
            alert.messageText = "Recog завершает операцию"
            alert.informativeText = "Дождитесь окончания. Пока идёт установка или запуск служб, окно можно свернуть."
            alert.addButton(withTitle: "Хорошо")
            alert.beginSheetModal(for: window)
        }
        return reply
    }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    let menu = NSMenu()
    let menuItem = menu.addItem(withTitle: "Recog", action: nil, keyEquivalent: "")
    let appMenu = NSMenu(title: "Recog")
    appMenu.addItem(withTitle: "Завершить Recog", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    menuItem.submenu = appMenu
    app.mainMenu = menu
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    withExtendedLifetime(delegate) { app.run() }
}
