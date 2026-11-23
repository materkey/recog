import AppKit
import Sparkle
import SwiftUI

private let files = FileManager.default
private let meetings = URL(string: "http://127.0.0.1:9080")!
private let units = ["live", "frontend", "copilot-channel", "copilot-supervisor"]

private struct Problem: Error {
    let text: String
    init(_ text: String) { self.text = text }
}

// The services are registered by another copy of Recog, usually a git checkout.
private struct ForeignServices: Error {
    let folder: URL?
    let dataDir: String?
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
    var shouldSyncOnLaunch = false
    var resumingUpdate = false
    var foreign: Foreign?
}

// What the window offers when another installation owns the services.
private struct Foreign {
    let folder: URL?
    let command: String
    let meetings: String
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
            let plist = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any]
            let wd = plist?["WorkingDirectory"] as? String
            let ours = wd.map { URL(fileURLWithPath: $0).standardizedFileURL.path == runtime.standardizedFileURL.path } ?? false
            guard ours, let program = (plist?["ProgramArguments"] as? [String])?.first,
                  program.hasPrefix(runtime.path + "/") else {
                let env = plist?["EnvironmentVariables"] as? [String: Any]
                throw ForeignServices(folder: ours ? nil : wd.map { URL(fileURLWithPath: $0, isDirectory: true) },
                                      dataDir: env?["RECOG_DATA_DIR"] as? String)
            }
            count += 1
        }
        return count
    }

    var sameVersion: Bool {
        guard let local = try? tag(runtime), let source = try? tag(package), let version = Release(source) else { return false }
        return Release(local) == version
    }

    // setup copies recog-release before it finishes registering and starting
    // services. Keep an unfinished update visible even if its tag now matches.
    var pendingRuntimeUpdate: String? {
        get { UserDefaults.standard.string(forKey: "RecogPendingRuntimeRelease") }
        nonmutating set { UserDefaults.standard.set(newValue, forKey: "RecogPendingRuntimeRelease") }
    }

    func markRuntimeUpdatePending() throws {
        guard let target = try tag(package), Release(target) != nil else {
            throw Problem("Не удалось определить версию обновления Recog.")
        }
        pendingRuntimeUpdate = target
    }

    func inspect(afterSuccessfulSetup: Bool = false) -> Inspection {
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
            let pending = !afterSuccessfulSetup && pendingRuntimeUpdate == label
            let hasCommand = executable(bin.appendingPathComponent("recog"))
            let owned = installed != nil && hasCommand && registered == units.count
            let installedTitle = "Установлен Recog \(installedTag ?? label)"
            let hasWeb = executable(bin.appendingPathComponent("recog-web"))
            let hasPackagedAudio = executable(bin.appendingPathComponent("ffmpeg")) && executable(bin.appendingPathComponent("recog-web-release"))
            let hasPanel = executable(bin.appendingPathComponent("recog-permissions"))
            let hasExtension = files.fileExists(atPath: extensionFolder.appendingPathComponent("manifest.json").path)
            if same || newer {
                // A later release may implement audio playback differently.
                if owned && hasWeb && hasPanel && hasExtension && (newer || (hasPackagedAudio && !pending)) {
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
                if pending {
                    return Inspection(plan: .repair, title: "Обновление Recog не завершено",
                        detail: "Повторите обновление, чтобы завершить установку версии \(label).",
                        button: "Повторить обновление", canManage: owned, resumingUpdate: true)
                }
                return Inspection(plan: .repair, title: installedTitle, detail: "Часть компонентов отсутствует. Их можно установить заново.",
                                  button: "Восстановить установку", canManage: owned)
            }
            if installed != nil {
                // The Finder distribution includes both the web launcher and
                // its renamed executable. A Homebrew installation can share
                // this runtime directory, so its release tag alone is not proof.
                let managed = owned && hasWeb && hasPackagedAudio && hasPanel && hasExtension &&
                    !files.fileExists(atPath: runtime.appendingPathComponent("recog-homebrew").path)
                return Inspection(plan: .update, title: pending ? "Обновление Recog не завершено" : "Доступен Recog \(label)",
                                  detail: "Установлена версия \(installedTag!). Записи и настройки сохранятся.",
                                  button: pending ? "Повторить обновление" : "Обновить Recog", canManage: owned,
                                  shouldSyncOnLaunch: managed && !pending)
            }
            return Inspection(plan: .install, title: "Установить Recog \(label) на этот Mac",
                              detail: "Recog распознаёт речь во встречах. Для первой загрузки модели может понадобиться интернет.",
                              button: "Установить Recog")
        } catch let other as ForeignServices {
            return foreign(other)
        } catch {
            return Inspection(plan: .blocked, title: "Установка недоступна",
                              detail: (error as? Problem)?.text ?? "Не удалось прочитать сведения об установке или её службах.")
        }
    }

    // Name the other installation and the command that hands the services over.
    // The window does not remove someone else's services itself.
    private func foreign(_ other: ForeignServices) -> Inspection {
        var isFolder: ObjCBool = false
        let folder = other.folder.flatMap { files.fileExists(atPath: $0.path, isDirectory: &isFolder) && isFolder.boolValue ? $0 : nil }
        let own = folder.map { $0.appendingPathComponent("bin/recog") }
        let program = own.flatMap { executable($0) ? $0 : nil } ?? package.appendingPathComponent("bin/recog")
        let appData = home.appendingPathComponent(".recog").standardizedFileURL.path
        let data = other.dataDir.map { URL(fileURLWithPath: $0).standardizedFileURL.path } ?? appData
        var meetings = data == appData
            ? "Встречи и настройки в \(shown(appData)) останутся."
            : "Встречи той установки хранятся в \(shown(data)), а приложение ищет их в \(shown(appData))."
        if let folder, files.fileExists(atPath: folder.appendingPathComponent(".env").path) {
            meetings += " Файл .env из той папки приложение не читает."
        }
        return Inspection(plan: .blocked,
            title: folder == nil ? "Recog уже установлен другим способом" : "Recog уже установлен из другой папки",
            detail: (folder.map { "Службы записи работают из папки \(shown($0.path))." }
                ?? "Службы записи зарегистрированы другой установкой Recog.") + " Приложение не меняет чужую установку.",
            foreign: Foreign(folder: folder, command: shellWord(program.path) + " service uninstall", meetings: meetings))
    }

    private func shown(_ path: String) -> String {
        path.hasPrefix(home.path + "/") ? "~/" + path.dropFirst(home.path.count + 1) : path
    }

    // A path the user can paste into a shell as is.
    private func shellWord(_ path: String) -> String {
        let plain = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/._-+@%:,")
        let short = shown(path)
        if short.unicodeScalars.allSatisfy({ plain.contains($0) || $0 == "~" && short.hasPrefix("~/") }) { return short }
        return "'" + path.replacingOccurrences(of: "'", with: #"'\''"#) + "'"
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
    var deferredAppRelaunch: (() -> Void)?
    var modifyingRuntime: Bool { operation == .installing || operation == .starting }

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

    @discardableResult private func end() -> Bool {
        operation = nil
        if let resume = deferredAppRelaunch {
            deferredAppRelaunch = nil
            resume()
            return false
        }
        if quitting { NSApp.reply(toApplicationShouldTerminate: true); return false }
        return true
    }

    func launch() {
        installation = layout.inspect()
        if installation.shouldSyncOnLaunch { install(onLaunch: true) } else { refresh() }
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

    func install(repeatSetup: Bool = false, onLaunch: Bool = false) {
        guard operation == nil else { return }
        let current = layout.inspect()
        guard current.plan == installation.plan, !onLaunch || current.shouldSyncOnLaunch else { refresh(); return }
        let updating = current.plan == .update || current.resumingUpdate
        installation = current
        let repeatSame = repeatSetup && layout.sameVersion && current.plan == .ready
        guard (repeatSame || [.install, .update, .repair].contains(current.plan)),
              begin(.installing, "Устанавливаем Recog…") else { return }
        Task {
            do {
                if current.plan == .update { try layout.markRuntimeUpdatePending() }
                let result = try await command(layout.package.appendingPathComponent("bin/recog"), layout.setupArguments)
                try result.requireSuccess()
                installation = layout.inspect(afterSuccessfulSetup: true)
                guard installation.plan == .ready else {
                    throw Problem("Установка не завершена. " + installation.detail)
                }
                layout.pendingRuntimeUpdate = nil
                if end() {
                    if updating { refresh() } else { permissions() }
                }
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
                if end() { refresh() }
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

    func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    func reveal(_ folder: URL) {
        NSWorkspace.shared.activateFileViewerSelecting([folder])
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
    @State private var copied = false
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
            if let foreign = info.foreign {
                VStack(alignment: .leading, spacing: 12) {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: "arrow.right.circle").font(.title3).foregroundStyle(.secondary).frame(width: 28).accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Перейти на это приложение").fontWeight(.semibold)
                            Text("Вне звонка выполните в Терминале команду: она остановит и удалит службы той установки. Затем нажмите «Проверить снова» и установите Recog.")
                                .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                            HStack(alignment: .top) {
                                Text(foreign.command).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                                Spacer()
                                Button(copied ? "Скопировано" : "Скопировать") {
                                    model.copy(foreign.command)
                                    copied = true
                                    DispatchQueue.main.asyncAfter(deadline: .now() + 2) { copied = false }
                                }
                            }
                            .padding(8).background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 6))
                            Text(foreign.meetings).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    if let folder = foreign.folder {
                        Divider()
                        row("folder", "Остаться на той установке", "Обновляйте её как раньше. Это приложение для неё не нужно.",
                            "Показать папку") { model.reveal(folder) }
                    }
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
private final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate, NSMenuItemValidation, SPUUpdaterDelegate {
    let model = Manager()
    var window: NSWindow?
    private lazy var updates = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)

    func addUpdateItems(to menu: NSMenu) {
        menu.addItem(withTitle: "Проверить обновления…", action: #selector(checkForUpdates(_:)), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Проверять обновления автоматически", action: #selector(toggleAutomaticChecks(_:)), keyEquivalent: "").target = self
        menu.addItem(.separator())
    }

    @objc private func checkForUpdates(_ sender: NSMenuItem) {
        guard !model.modifyingRuntime && updates.updater.canCheckForUpdates else { return }
        updates.checkForUpdates(sender)
    }

    @objc private func toggleAutomaticChecks(_ sender: NSMenuItem) {
        updates.updater.automaticallyChecksForUpdates.toggle()
        sender.state = updates.updater.automaticallyChecksForUpdates ? .on : .off
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(checkForUpdates(_:)) {
            return !model.modifyingRuntime && updates.updater.canCheckForUpdates
        }
        if item.action == #selector(toggleAutomaticChecks(_:)) {
            item.state = updates.updater.automaticallyChecksForUpdates ? .on : .off
        }
        return true
    }

    func updater(_ updater: SPUUpdater, mayPerform updateCheck: SPUUpdateCheck) throws {
        if model.modifyingRuntime {
            throw NSError(domain: "com.recog.manager", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Дождитесь завершения установки или запуска служб Recog."])
        }
    }

    func updater(_ updater: SPUUpdater, shouldPostponeRelaunchForUpdate item: SUAppcastItem,
                 untilInvokingBlock installHandler: @escaping () -> Void) -> Bool {
        guard model.modifyingRuntime else { return false }
        model.deferredAppRelaunch = installHandler
        return true
    }

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
        model.launch()
        updates.startUpdater()
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
    delegate.addUpdateItems(to: appMenu)
    appMenu.addItem(withTitle: "Завершить Recog", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    menuItem.submenu = appMenu
    app.mainMenu = menu
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    withExtendedLifetime(delegate) { app.run() }
}
