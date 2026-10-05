// Reads the Accessibility tree of the running Zoom application and prints one
// JSON object per line describing the meeting it is in. Read-only: it performs
// no Accessibility actions.
//
//   {"state": "not_trusted"}        this process lacks the Accessibility permission
//   {"state": "zoom_not_running"}
//   {"state": "idle"}               Zoom is running, no meeting window
//   {"state": "meeting", "view": "window" | "minimal", "tiles": [...],
//    "topic": "...", "account": "..."}
//
// A tile is {"name": ..., "description": ...}: the participant's display name and
// the tile's description, e.g. "Alice, Computer audio unmuted, Video off, active
// speaker". The description is in the Zoom UI language and is parsed by the
// caller (recog/zoom_app.py), not here.
//
// Observed on Zoom Workplace 7.1.9:
// - the meeting window has AXIdentifier zm.meeting.window.main; in minimal view
//   it is replaced by an AXSystemDialog window holding one tile, the current
//   speaker;
// - a tile is an AXTabGroup whose AXButton child is described by the display
//   name (a tile showing a profile picture has no AXStaticText with the name);
// - the topic is the description of MeetingTopBarInfoButton, which is in the
//   tree only while the toolbar is shown;
// - the account button of the main window is described as
//   "Zoom, <display name>, <status>, <plan>".
//
// A line is written when the state changes and at least every 2 seconds, so the
// reader can tell a silent meeting from a stalled helper.
//
// The helper is built into an application bundle and started through
// LaunchServices (`open -a … --args --socket <path>`), not as a child process:
// macOS grants Accessibility to the "responsible" process, which for a child
// is its parent — a Python interpreter whose path changes with every upgrade.
// Launched on its own, the bundle is the one the permission belongs to, under
// its own name and identifier. It then has no parent to inherit a pipe from,
// so it connects to a Unix socket the caller listens on, and exits as soon as
// that socket is gone. Without `--socket` it writes to stdout, for debugging.

import AppKit
import ApplicationServices
import Foundation

let zoomBundleId = "us.zoom.xos"
let meetingWindowId = "zm.meeting.window.main"
let topicButtonId = "MeetingTopBarInfoButton"
let pollInterval: TimeInterval = 0.5
let heartbeatInterval: TimeInterval = 2.0
let maxDepth = 6

func value(_ element: AXUIElement, _ name: String) -> AnyObject? {
    var out: AnyObject?
    return AXUIElementCopyAttributeValue(element, name as CFString, &out) == .success ? out : nil
}

func text(_ element: AXUIElement, _ name: String) -> String? {
    guard let string = value(element, name) as? String, !string.isEmpty else { return nil }
    return string
}

func children(_ element: AXUIElement) -> [AXUIElement] {
    (value(element, kAXChildrenAttribute as String) as? [AXUIElement]) ?? []
}

func role(_ element: AXUIElement) -> String {
    text(element, kAXRoleAttribute as String) ?? ""
}

/// A participant tile: a tab group whose description starts with the name its
/// button child is described by. The role description is localized, so it is
/// not used.
func tile(_ element: AXUIElement) -> [String: String]? {
    guard role(element) == "AXTabGroup", let description = text(element, kAXDescriptionAttribute as String)
    else { return nil }
    for child in children(element) where role(child) == "AXButton" {
        if let name = text(child, kAXDescriptionAttribute as String), description.hasPrefix(name) {
            return ["name": name, "description": description]
        }
    }
    return nil
}

struct Scan {
    var tiles: [[String: String]] = []
    var topic: String?
}

func scan(_ element: AXUIElement, depth: Int, into result: inout Scan) {
    if let found = tile(element) {
        result.tiles.append(found)
        return
    }
    if text(element, "AXIdentifier") == topicButtonId {
        result.topic = text(element, kAXDescriptionAttribute as String)
    }
    // The participants panel repeats the names without the speaker state.
    guard depth > 0, role(element) != "AXScrollArea" else { return }
    for child in children(element) {
        scan(child, depth: depth - 1, into: &result)
    }
}

func windows(_ app: AXUIElement) -> [AXUIElement] {
    var all = (value(app, kAXWindowsAttribute as String) as? [AXUIElement]) ?? []
    // kAXWindows can omit a window on another Space; the focused one is still reachable.
    if let focused = value(app, kAXFocusedWindowAttribute as String) {
        let element = focused as! AXUIElement
        if !all.contains(where: { CFEqual($0, element) }) { all.append(element) }
    }
    return all
}

/// The description of the account button in the main window, if present.
func accountDescription(_ windows: [AXUIElement]) -> String? {
    for window in windows where text(window, "AXIdentifier") != meetingWindowId {
        for child in children(window) where role(child) == "AXButton" {
            if let description = text(child, kAXDescriptionAttribute as String),
               description.hasPrefix("Zoom, "), description.split(separator: ",").count >= 3 {
                return description
            }
        }
    }
    return nil
}

func snapshot(app: AXUIElement) -> [String: Any] {
    let all = windows(app)
    var view: String?
    var result = Scan()

    if let meeting = all.first(where: { text($0, "AXIdentifier") == meetingWindowId }) {
        view = "window"
        scan(meeting, depth: maxDepth, into: &result)
    } else {
        for window in all where text(window, kAXSubroleAttribute as String) == "AXSystemDialog" {
            var candidate = Scan()
            scan(window, depth: maxDepth, into: &candidate)
            if !candidate.tiles.isEmpty {
                view = "minimal"
                result = candidate
                break
            }
        }
    }

    guard let view else { return ["state": "idle"] }
    var out: [String: Any] = ["state": "meeting", "view": view, "tiles": result.tiles]
    if let topic = result.topic { out["topic"] = topic }
    if let account = accountDescription(all) { out["account"] = account }
    return out
}

func currentSnapshot() -> [String: Any] {
    guard AXIsProcessTrusted() else { return ["state": "not_trusted"] }
    guard let zoom = NSRunningApplication.runningApplications(withBundleIdentifier: zoomBundleId).first
    else { return ["state": "zoom_not_running"] }
    let app = AXUIElementCreateApplication(zoom.processIdentifier)
    // Zoom builds parts of its tree only once an assistive client announces itself.
    AXUIElementSetAttributeValue(app, "AXManualAccessibility" as CFString, kCFBooleanTrue)
    return snapshot(app: app)
}

func connectSocket(_ path: String) -> Int32? {
    let descriptor = socket(AF_UNIX, SOCK_STREAM, 0)
    guard descriptor >= 0 else { return nil }
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    let capacity = MemoryLayout.size(ofValue: address.sun_path)
    guard path.utf8.count < capacity else {
        close(descriptor)
        return nil
    }
    withUnsafeMutablePointer(to: &address.sun_path) { pointer in
        pointer.withMemoryRebound(to: CChar.self, capacity: capacity) { _ = strncpy($0, path, capacity - 1) }
    }
    let size = socklen_t(MemoryLayout<sockaddr_un>.size)
    let status = withUnsafePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(descriptor, $0, size) }
    }
    guard status == 0 else {
        close(descriptor)
        return nil
    }
    return descriptor
}

/// Write one line; false once the reader is gone.
func send(_ line: String, to descriptor: Int32) -> Bool {
    let bytes = Array((line + "\n").utf8)
    var offset = 0
    while offset < bytes.count {
        let written = bytes[offset...].withUnsafeBytes { write(descriptor, $0.baseAddress, $0.count) }
        if written <= 0 { return false }
        offset += written
    }
    return true
}

// A closed reader must end the loop through send(), not kill the process mid-write.
signal(SIGPIPE, SIG_IGN)

var output = STDOUT_FILENO
let arguments = CommandLine.arguments
if let flag = arguments.firstIndex(of: "--socket"), flag + 1 < arguments.count {
    guard let descriptor = connectSocket(arguments[flag + 1]) else {
        FileHandle.standardError.write("zoom_ax_probe: cannot connect to \(arguments[flag + 1])\n".data(using: .utf8)!)
        exit(1)
    }
    output = descriptor
}

// Ask once: the system dialog offers to open the Accessibility pane, where this
// application is then already listed.
if !AXIsProcessTrusted() {
    let prompt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
    _ = AXIsProcessTrustedWithOptions([prompt: true] as CFDictionary)
}

var lastLine = ""
var lastEmit = Date.distantPast

while true {
    let current = currentSnapshot()
    if let data = try? JSONSerialization.data(withJSONObject: current, options: [.sortedKeys]),
       let line = String(data: data, encoding: .utf8) {
        if line != lastLine || Date().timeIntervalSince(lastEmit) >= heartbeatInterval {
            // The heartbeat doubles as the liveness check of the reader.
            if !send(line, to: output) { exit(0) }
            lastLine = line
            lastEmit = Date()
        }
    }
    Thread.sleep(forTimeInterval: pollInterval)
}
