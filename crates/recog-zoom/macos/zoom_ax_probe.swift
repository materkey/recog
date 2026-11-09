// Reads the Accessibility tree of the running Zoom application and prints one
// JSON object per line describing the meeting it is in. Read-only: it performs
// no Accessibility actions.
//
//   {"state": "not_trusted"}        this process lacks the Accessibility permission;
//                                   the helper exits after saying so
//   {"state": "zoom_not_running"}
//   {"state": "idle", "account": "..."}   Zoom is running, no meeting window
//   {"state": "meeting", "view": "window" | "minimal", "tiles": [...],
//    "title": "...", "account": "...", "menu": [[...], ...]}
//
// "title" is the title of the meeting window, absent in minimal view.
//
// "menu" lists the titles of the menu bar's items as paths, e.g. ["Meeting",
// "Mute audio"], down to three levels and without the Apple menu; absent when
// the menu bar cannot be read. Unlike the meeting toolbar, which leaves the
// tree when the pointer is not over the window, the menu bar is always there,
// and the meeting menu's audio item follows the user's own microphone.
//
// A tile is {"description": ..., "children": [{"role": ..., "description": ...,
// "value": ...}, ...]}: a described tab group, e.g. "Alice, Computer audio
// unmuted, Video off, active speaker", and what its direct children say about
// themselves. Every described tab group is reported. Which of them show a
// participant, under what name and in what state is decided by the caller
// (recog/zoom_app.py), not here: the wording belongs to Zoom and changes with
// it, while a change of this file changes the helper's signature and costs the
// user the permission.
//
// Observed on Zoom Workplace 7.1.9:
// - the meeting window has AXIdentifier zm.meeting.window.main; in minimal view
//   it is replaced by an AXSystemDialog window holding one tile, the current
//   speaker;
// - a participant tile is an AXTabGroup with an AXButton child that names the
//   participant (a tile showing a profile picture has no AXStaticText with
//   the name);
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
let pollInterval: TimeInterval = 0.5
let heartbeatInterval: TimeInterval = 2.0
let maxDepth = 6
let menuDepth = 3

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

/// A described tab group with the role, description and text value of each of
/// its direct children. A participant tile is one of these.
func tile(_ element: AXUIElement) -> [String: Any]? {
    guard role(element) == "AXTabGroup", let description = text(element, kAXDescriptionAttribute as String)
    else { return nil }
    let parts = children(element).map { child -> [String: String] in
        var part = ["role": role(child)]
        part["description"] = text(child, kAXDescriptionAttribute as String)
        part["value"] = text(child, kAXValueAttribute as String)
        return part
    }
    return ["description": description, "children": parts]
}

/// Collect the described tab groups under `element`. The toolbar's info button,
/// which looks like the meeting topic, is deliberately not read: on a live
/// meeting it carried the free-plan timer ("Meeting  40-minutes") instead.
func scan(_ element: AXUIElement, depth: Int, into tiles: inout [[String: Any]]) {
    if let found = tile(element) { tiles.append(found) }
    // The participants panel repeats the names without the speaker state.
    guard depth > 0, role(element) != "AXScrollArea" else { return }
    for child in children(element) {
        scan(child, depth: depth - 1, into: &tiles)
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

/// The titles of the menu bar's items as paths from a top-level menu, read
/// without opening any menu. The Apple menu, always the first, is skipped.
func menuPaths(_ app: AXUIElement) -> [[String]]? {
    guard let bar = value(app, kAXMenuBarAttribute as String) else { return nil }
    var paths: [[String]] = []
    func walk(_ element: AXUIElement, _ path: [String]) {
        for child in children(element) {
            // An item holds its submenu in an untitled AXMenu child.
            if role(child) == "AXMenu" {
                walk(child, path)
                continue
            }
            guard let title = text(child, kAXTitleAttribute as String) else { continue }
            let next = path + [title]
            if next.count > 1 { paths.append(next) }
            if next.count < menuDepth { walk(child, next) }
        }
    }
    for item in children(bar as! AXUIElement).dropFirst() {
        guard let title = text(item, kAXTitleAttribute as String) else { continue }
        walk(item, [title])
    }
    return paths
}

func snapshot(app: AXUIElement) -> [String: Any] {
    let all = windows(app)
    var view: String?
    var title: String?
    var tiles: [[String: Any]] = []

    if let meeting = all.first(where: { text($0, "AXIdentifier") == meetingWindowId }) {
        view = "window"
        title = text(meeting, kAXTitleAttribute as String)
        scan(meeting, depth: maxDepth, into: &tiles)
    } else {
        // Whether one of these dialogs is the minimal view is the caller's
        // call: it is, when a tab group in it turns out to show a participant.
        for window in all where text(window, kAXSubroleAttribute as String) == "AXSystemDialog" {
            scan(window, depth: maxDepth, into: &tiles)
        }
        if !tiles.isEmpty { view = "minimal" }
    }

    // The account is reported outside a meeting too: the main window is not
    // always reachable during one, and the name is needed from its first tile.
    var out: [String: Any] = ["state": "idle"]
    if let view {
        out = ["state": "meeting", "view": view, "tiles": tiles]
        if let menu = menuPaths(app) { out["menu"] = menu }
    }
    if let title { out["title"] = title }
    if let account = accountDescription(all) { out["account"] = account }
    return out
}

func currentSnapshot() -> [String: Any] {
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

// Without the permission, say so and exit. macOS answers AXIsProcessTrusted()
// from a per-process cache: a helper that stays running never learns that the
// permission was granted. The caller starts a fresh one every few seconds,
// with --no-prompt so that only the first start shows the system dialog. That
// dialog offers to open the Accessibility pane, where this application is then
// already listed.
if !AXIsProcessTrusted() {
    if !arguments.contains("--no-prompt") {
        let prompt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        _ = AXIsProcessTrustedWithOptions([prompt: true] as CFDictionary)
    }
    _ = send("{\"state\":\"not_trusted\"}", to: output)
    exit(2)
}

var lastLine = ""
var lastEmit = Date.distantPast

while true {
    // No run loop drains the main thread's autorelease pool here: without a
    // pool of its own every poll leaves its Foundation and AppKit objects
    // behind for the lifetime of the process.
    autoreleasepool {
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
    }
    Thread.sleep(forTimeInterval: pollInterval)
}
