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
// A line is printed when the state changes and at least every 2 seconds, so the
// reader can tell a silent meeting from a stalled helper.

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

var lastLine = ""
var lastEmit = Date.distantPast

while true {
    // The parent died and the helper was reparented: nobody reads the output.
    if getppid() == 1 { exit(0) }
    let current = currentSnapshot()
    if let data = try? JSONSerialization.data(withJSONObject: current, options: [.sortedKeys]),
       let line = String(data: data, encoding: .utf8) {
        if line != lastLine || Date().timeIntervalSince(lastEmit) >= heartbeatInterval {
            print(line)
            fflush(stdout)
            lastLine = line
            lastEmit = Date()
        }
    }
    Thread.sleep(forTimeInterval: pollInterval)
}
