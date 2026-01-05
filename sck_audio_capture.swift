#!/usr/bin/env swift

// ScreenCaptureKit Audio Capture Utility
// Captures system audio and outputs raw PCM to stdout
// Usage: swift sck_audio_capture.swift | python script.py

import Foundation
import ScreenCaptureKit
import CoreMedia
import AVFoundation
import Darwin

/// Fixed-capacity ring buffer to avoid memory fragmentation from Data.append/removeFirst.
/// Uses a pre-allocated [UInt8] array with head/tail indices for zero-copy circular access.
final class RingBuffer {
    private var storage: [UInt8]
    private var head: Int = 0  // Read position
    private var tail: Int = 0  // Write position
    private var count_: Int = 0
    private let capacity: Int

    init(capacity: Int) {
        self.capacity = capacity
        self.storage = [UInt8](repeating: 0, count: capacity)
    }

    var count: Int { count_ }
    var isEmpty: Bool { count_ == 0 }
    var freeSpace: Int { capacity - count_ }

    /// Append data, dropping oldest bytes if necessary to make room.
    /// Maintains float32 alignment (4 bytes) when dropping.
    func append(_ data: Data) {
        guard !data.isEmpty else { return }

        var bytesToWrite = data.count

        // If data won't fit, drop oldest bytes (aligned to 4 bytes)
        if bytesToWrite > freeSpace {
            let overflow = bytesToWrite - freeSpace
            let alignedDrop = ((overflow + 3) / 4) * 4
            drop(min(alignedDrop, count_))
        }

        // Truncate if still too large
        bytesToWrite = min(bytesToWrite, freeSpace)
        guard bytesToWrite > 0 else { return }

        data.withUnsafeBytes { raw in
            guard let src = raw.baseAddress?.assumingMemoryBound(to: UInt8.self) else { return }

            let firstChunk = min(bytesToWrite, capacity - tail)
            storage[tail..<(tail + firstChunk)].withUnsafeMutableBytes { dest in
                dest.copyBytes(from: UnsafeBufferPointer(start: src, count: firstChunk))
            }

            let secondChunk = bytesToWrite - firstChunk
            if secondChunk > 0 {
                storage[0..<secondChunk].withUnsafeMutableBytes { dest in
                    dest.copyBytes(from: UnsafeBufferPointer(start: src + firstChunk, count: secondChunk))
                }
            }

            tail = (tail + bytesToWrite) % capacity
            count_ += bytesToWrite
        }
    }

    /// Drop oldest n bytes from buffer.
    func drop(_ n: Int) {
        let toDrop = min(n, count_)
        head = (head + toDrop) % capacity
        count_ -= toDrop
    }

    /// Write up to `maxBytes` from buffer to file descriptor.
    /// Returns bytes written (>0), 0 for would-block, or -1 for error with errno set.
    func writeToFD(_ fd: Int32, maxBytes: Int) -> Int {
        guard count_ > 0 else { return 0 }

        let toWrite = min(maxBytes, count_, capacity - head)
        guard toWrite > 0 else { return 0 }

        let written = storage.withUnsafeBytes { raw -> Int in
            let base = raw.baseAddress!.advanced(by: head)
            return Darwin.write(fd, base, toWrite)
        }

        if written > 0 {
            head = (head + written) % capacity
            count_ -= written
        }

        return written
    }
}

enum CaptureError: Error, CustomStringConvertible {
    case noDisplay

    var description: String {
        switch self {
        case .noDisplay:
            return "No display found"
        }
    }
}

final class StdoutWriter {
    private let fd: Int32
    private let queue = DispatchQueue(label: "stdout.writer", qos: .userInitiated)
    private let buffer: RingBuffer  // Fixed-capacity ring buffer, no realloc
    private var flushScheduled = false

    init(fd: Int32 = STDOUT_FILENO, maxBufferBytes: Int = 1_000_000) {
        self.fd = fd
        self.buffer = RingBuffer(capacity: maxBufferBytes)

        signal(SIGPIPE, SIG_IGN)
        setNonBlocking(fd)
    }

    func enqueue(_ data: Data) {
        guard !data.isEmpty else { return }
        queue.async {
            // RingBuffer.append handles overflow with aligned dropping internally
            self.buffer.append(data)
            self.flushIfNeeded()
        }
    }

    private func flushIfNeeded() {
        guard !flushScheduled else { return }
        flushScheduled = true
        flushLoop()
    }

    private func flushLoop() {
        while !buffer.isEmpty {
            let written = buffer.writeToFD(fd, maxBytes: buffer.count)

            if written > 0 {
                continue
            }

            let err = errno
            if err == EINTR {
                continue
            }
            if err == EAGAIN || err == EWOULDBLOCK {
                queue.asyncAfter(deadline: .now() + .milliseconds(10)) { [weak self] in
                    self?.flushLoop()
                }
                return
            }
            if err == EPIPE {
                exit(0)
            }

            fputs("stdout write failed (errno \(err))\n", stderr)
            exit(1)
        }

        flushScheduled = false
    }
}

private func setNonBlocking(_ fd: Int32) {
    let flags = fcntl(fd, F_GETFL)
    guard flags != -1 else { return }
    _ = fcntl(fd, F_SETFL, flags | O_NONBLOCK)
}

class AudioCapture: NSObject, SCStreamDelegate, SCStreamOutput {
    private var stream: SCStream?

    private let targetSampleRate: Int = 16000
    private let streamSampleRate: Int = 48000

    private let audioQueue = DispatchQueue(label: "audio.processing", qos: .userInitiated)
    private let watchdogQueue = DispatchQueue(label: "audio.watchdog", qos: .utility)

    private let stdoutWriter = StdoutWriter(maxBufferBytes: 2_000_000)

    private let stateQueue = DispatchQueue(label: "audio.capture.state", qos: .utility)
    private var lastAudioUptimeNanos: UInt64 = DispatchTime.now().uptimeNanoseconds
    private var restartInFlight = false
    private var shouldStop = false
    private var watchdogTimer: DispatchSourceTimer?

    func start() async {
        fputs("Starting ScreenCaptureKit audio capture...\n", stderr)
        startWatchdog()
        await startWithRetry(reason: "initial")
    }

    func requestStop() {
        var timer: DispatchSourceTimer?
        var currentStream: SCStream?
        stateQueue.sync {
            shouldStop = true
            timer = watchdogTimer
            watchdogTimer = nil
            currentStream = stream
            stream = nil
        }

        timer?.cancel()
        if let currentStream {
            Task { try? await currentStream.stopCapture() }
        }
    }

    private func startWatchdog() {
        let timer = DispatchSource.makeTimerSource(queue: watchdogQueue)
        timer.schedule(deadline: .now() + .seconds(5), repeating: .seconds(2))
        timer.setEventHandler { [weak self] in
            self?.checkHealth()
        }
        timer.resume()

        stateQueue.sync {
            watchdogTimer = timer
        }
    }

    private func checkHealth() {
        var shouldStop = false
        var last: UInt64 = 0
        stateQueue.sync {
            shouldStop = self.shouldStop
            last = self.lastAudioUptimeNanos
        }

        guard !shouldStop else { return }

        let now = DispatchTime.now().uptimeNanoseconds
        let delta = now > last ? now - last : 0

        // If we haven't received any audio callbacks for a while, restart the stream.
        // (If the system is truly silent, ScreenCaptureKit still delivers silent buffers.)
        if delta > 10_000_000_000 {
            scheduleRestart(reason: "no audio callbacks for \(Double(delta) / 1_000_000_000)s")
        }
    }

    private func scheduleRestart(reason: String) {
        var shouldSchedule = false
        stateQueue.sync {
            if !restartInFlight && !shouldStop {
                restartInFlight = true
                shouldSchedule = true
            }
        }
        guard shouldSchedule else { return }

        Task { [weak self] in
            guard let self else { return }
            await self.restartLoop(reason: reason)
        }
    }

    private func restartLoop(reason: String) async {
        fputs("Restarting ScreenCaptureKit stream (\(reason))...\n", stderr)

        var shouldStop: Bool = false
        var currentStream: SCStream?
        stateQueue.sync {
            shouldStop = self.shouldStop
            currentStream = stream
            stream = nil
        }

        if shouldStop {
            stateQueue.sync {
                restartInFlight = false
            }
            return
        }

        if let currentStream {
            try? await currentStream.stopCapture()
        }

        await startWithRetry(reason: reason)

        stateQueue.sync {
            restartInFlight = false
        }
    }

    private func startWithRetry(reason: String) async {
        var attempt = 0
        var delaySeconds: Double = 0.5
        var lastLoggedError: String?
        var lastLogUptimeNanos: UInt64 = 0
        let minLogIntervalNanos: UInt64 = 5 * 60 * 1_000_000_000

        while true {
            let shouldStop = stateQueue.sync { self.shouldStop }
            if shouldStop { return }

            do {
                try await startOnce()
                fputs("Audio capture running. Output: PCM float32, \(targetSampleRate) Hz, mono.\n", stderr)
                return
            } catch {
                attempt += 1
                if isFatalStartError(error) {
                    fputs("Fatal ScreenCaptureKit error (no retries): \(error)\n", stderr)
                    exit(1)
                }
                let nowNanos = DispatchTime.now().uptimeNanoseconds
                let errString = String(describing: error)
                let shouldLog = (attempt == 1)
                    || (errString != lastLoggedError)
                    || (nowNanos > lastLogUptimeNanos && nowNanos - lastLogUptimeNanos >= minLogIntervalNanos)
                if shouldLog {
                    fputs("Start failed (\(reason), attempt \(attempt)): \(errString)\n", stderr)
                    lastLoggedError = errString
                    lastLogUptimeNanos = nowNanos
                }
                let sleepNs = UInt64(delaySeconds * 1_000_000_000)
                try? await Task.sleep(nanoseconds: sleepNs)
                delaySeconds = min(delaySeconds * 2, 10.0)
            }
        }
    }

    private func startOnce() async throws {
        let content = try await SCShareableContent.current

        guard let display = content.displays.first else {
            throw CaptureError.noDisplay
        }

        let filter = SCContentFilter(display: display, excludingWindows: [])

        let config = SCStreamConfiguration()

        // Minimal video (required for audio capture)
        config.width = 2
        config.height = 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        config.showsCursor = false

        // Audio settings
        config.capturesAudio = true
        config.excludesCurrentProcessAudio = true
        config.channelCount = 1
        config.sampleRate = streamSampleRate

        let newStream = SCStream(filter: filter, configuration: config, delegate: self)
        try newStream.addStreamOutput(self, type: .audio, sampleHandlerQueue: audioQueue)

        stateQueue.sync {
            stream = newStream
            lastAudioUptimeNanos = DispatchTime.now().uptimeNanoseconds
        }

        try await newStream.startCapture()
    }

    // SCStreamDelegate
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        let shouldStop = stateQueue.sync { self.shouldStop }

        guard !shouldStop else { return }

        fputs("Stream stopped: \(error)\n", stderr)
        scheduleRestart(reason: "stream stopped")
    }

    // SCStreamOutput
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }

        stateQueue.async {
            self.lastAudioUptimeNanos = DispatchTime.now().uptimeNanoseconds
        }

        guard let blockBuffer = CMSampleBufferGetDataBuffer(sampleBuffer) else { return }

        var length = 0
        var dataPointer: UnsafeMutablePointer<Int8>?
        let status = CMBlockBufferGetDataPointer(
            blockBuffer,
            atOffset: 0,
            lengthAtOffsetOut: nil,
            totalLengthOut: &length,
            dataPointerOut: &dataPointer
        )

        if status == kCMBlockBufferNoErr, let dataPointer, length > 0 {
            let sampleCount = length / MemoryLayout<Float32>.size
            guard sampleCount > 0 else { return }

            let floatPtr = UnsafeRawPointer(dataPointer).bindMemory(to: Float32.self, capacity: sampleCount)
            writeDownsampled(floatPtr: floatPtr, sampleCount: sampleCount)
            return
        }

        // Fallback: copy bytes out of a non-contiguous CMBlockBuffer
        let totalLength = CMBlockBufferGetDataLength(blockBuffer)
        guard totalLength > 0 else { return }

        var copied = Data(count: totalLength)
        let copyStatus = copied.withUnsafeMutableBytes { dest -> OSStatus in
            guard let base = dest.baseAddress else { return -1 }
            return CMBlockBufferCopyDataBytes(blockBuffer, atOffset: 0, dataLength: totalLength, destination: base)
        }
        guard copyStatus == kCMBlockBufferNoErr else { return }

        let sampleCount = copied.count / MemoryLayout<Float32>.size
        guard sampleCount > 0 else { return }

        copied.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            let floatPtr = base.bindMemory(to: Float32.self, capacity: sampleCount)
            writeDownsampled(floatPtr: floatPtr, sampleCount: sampleCount)
        }
    }

    private func writeDownsampled(floatPtr: UnsafePointer<Float32>, sampleCount: Int) {
        // ScreenCaptureKit outputs float32 at 48kHz, convert to 16kHz by simple decimation (48k -> 16k = factor 3).
        let outputCount = (sampleCount + 2) / 3
        var outputSamples = [Float32](repeating: 0, count: outputCount)

        var outIdx = 0
        var inIdx = 0
        while inIdx < sampleCount {
            outputSamples[outIdx] = floatPtr[inIdx]
            outIdx += 1
            inIdx += 3
        }

        outputSamples.withUnsafeBytes { buf in
            stdoutWriter.enqueue(Data(buf))
        }
    }

    private func isFatalStartError(_ error: Error) -> Bool {
        // TCC denied (no Screen Recording permission) won't self-heal with retries.
        let ns = error as NSError
        return ns.domain == "com.apple.ScreenCaptureKit.SCStreamErrorDomain" && ns.code == -3801
    }
}

// Main
let capture = AudioCapture()

Task {
    await capture.start()
}

RunLoop.main.run()
