import Foundation
import CoreGraphics
import AVFoundation

// Run permission probes under the service's Recog.app launcher. Neither a status
// check nor a permission request starts an audio stream.
func permissionStatus() {
    let microphone: String
    switch AVCaptureDevice.authorizationStatus(for: .audio) {
    case .authorized: microphone = "granted"
    case .notDetermined: microphone = "not-requested"
    case .denied: microphone = "denied"
    case .restricted: microphone = "restricted"
    @unknown default: microphone = "unknown"
    }
    print("microphone=\(microphone)")
    print("system-audio=\(CGPreflightScreenCaptureAccess() ? "granted" : "not-granted")")
}

switch CommandLine.arguments.dropFirst().first {
case "--permission-status":
    permissionStatus()
    exit(0)
case "--request-microphone":
    AVCaptureDevice.requestAccess(for: .audio) { _ in
        permissionStatus()
        exit(0)
    }
    RunLoop.main.run()
case "--request-system-audio":
    _ = CGRequestScreenCaptureAccess()
    permissionStatus()
    exit(0)
default:
    exit(64)
}
