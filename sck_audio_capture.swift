#!/usr/bin/env swift

// ScreenCaptureKit Audio Capture Utility
// Captures system audio and outputs raw PCM to stdout
// Usage: swift sck_audio_capture.swift | python script.py

import Foundation
import ScreenCaptureKit
import CoreMedia
import AVFoundation

class AudioCapture: NSObject, SCStreamDelegate, SCStreamOutput {
    var stream: SCStream?
    let sampleRate: Double = 16000
    var running = true

    func start() async throws {
        // Get shareable content
        let content = try await SCShareableContent.current

        guard let display = content.displays.first else {
            fputs("Error: No display found\n", stderr)
            exit(1)
        }

        // Create filter - capture display (needed for audio)
        let filter = SCContentFilter(display: display, excludingWindows: [])

        // Configure stream
        let config = SCStreamConfiguration()

        // Minimal video (required)
        config.width = 2
        config.height = 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        config.showsCursor = false

        // Audio settings
        config.capturesAudio = true
        config.excludesCurrentProcessAudio = true
        config.channelCount = 1  // Mono
        config.sampleRate = 48000  // Will resample to 16kHz

        // Create stream
        stream = SCStream(filter: filter, configuration: config, delegate: self)

        // Add output
        try stream?.addStreamOutput(self, type: .audio, sampleHandlerQueue: .main)

        // Start
        try await stream?.startCapture()

        fputs("Audio capture started. Outputting raw PCM (16kHz, mono, float32) to stdout.\n", stderr)
        fputs("Press Ctrl+C to stop.\n", stderr)

        // Keep running
        signal(SIGINT) { _ in
            fputs("\nStopping...\n", stderr)
            exit(0)
        }

        while running {
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        }
    }

    // SCStreamDelegate
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fputs("Stream stopped with error: \(error)\n", stderr)
        running = false
    }

    // SCStreamOutput
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }

        // Get audio buffer
        guard let blockBuffer = CMSampleBufferGetDataBuffer(sampleBuffer) else { return }

        var length = 0
        var dataPointer: UnsafeMutablePointer<Int8>?
        CMBlockBufferGetDataPointer(blockBuffer, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &dataPointer)

        guard let data = dataPointer, length > 0 else { return }

        // Audio is float32 at 48kHz, resample to 16kHz (take every 3rd sample)
        let float32Pointer = UnsafeRawPointer(data).bindMemory(to: Float32.self, capacity: length / 4)
        let sampleCount = length / 4

        // Simple decimation (48kHz -> 16kHz = factor of 3)
        var outputSamples: [Float32] = []
        for i in stride(from: 0, to: sampleCount, by: 3) {
            outputSamples.append(float32Pointer[i])
        }

        // Write to stdout as raw bytes
        outputSamples.withUnsafeBytes { buffer in
            FileHandle.standardOutput.write(Data(buffer))
        }
    }
}

// Main
let capture = AudioCapture()

Task {
    do {
        try await capture.start()
    } catch {
        fputs("Error: \(error)\n", stderr)
        exit(1)
    }
}

RunLoop.main.run()
