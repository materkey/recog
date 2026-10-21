// recog_launcher: the executable of Recog.app, which launchd starts in place of
// the Python interpreter of the live service.
//
// macOS asks for Screen Recording and the microphone on behalf of the
// responsible process, which for a launchd job is the job itself and for its
// children is inherited. Started directly, that was the Homebrew interpreter:
// the dialogs named "python3.12", and its ad-hoc identity changed with every
// interpreter upgrade, silently invalidating the grant of a running service.
// Started through this launcher, the permissions belong to "Recog", whose
// code changes only with this file.
//
// Usage: recog_launcher <program> [arguments...]
// The program is started as a child (not exec'd: the process image would
// become the interpreter again), termination signals are forwarded to it, and
// the launcher exits with the child's status.

import Darwin
import Dispatch

let arguments = Array(CommandLine.arguments.dropFirst())
guard !arguments.isEmpty else {
    fputs("usage: recog_launcher <program> [arguments...]\n", stderr)
    exit(64)
}

let forwarded: [Int32] = [SIGTERM, SIGINT, SIGHUP, SIGQUIT]

// Ignored here so that only the dispatch sources below see them; the child
// gets the default dispositions back through POSIX_SPAWN_SETSIGDEF, since it
// would otherwise inherit the ignored ones and could not be stopped.
for signo in forwarded {
    signal(signo, SIG_IGN)
}

var attributes: posix_spawnattr_t? = nil
posix_spawnattr_init(&attributes)
var defaults = sigset_t()
sigemptyset(&defaults)
for signo in forwarded {
    sigaddset(&defaults, signo)
}
posix_spawnattr_setsigdefault(&attributes, &defaults)
posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETSIGDEF))

var argv: [UnsafeMutablePointer<CChar>?] = arguments.map { strdup($0) }
argv.append(nil)

var child: pid_t = 0
let spawned = posix_spawnp(&child, arguments[0], nil, &attributes, argv, environ)
if spawned != 0 {
    fputs("recog_launcher: cannot start \(arguments[0]): \(String(cString: strerror(spawned)))\n", stderr)
    exit(127)
}

let signalQueue = DispatchQueue(label: "recog.launcher.signals")
let sources = forwarded.map { signo -> DispatchSourceSignal in
    let source = DispatchSource.makeSignalSource(signal: signo, queue: signalQueue)
    source.setEventHandler { kill(child, signo) }
    source.resume()
    return source
}

var status: Int32 = 0
while waitpid(child, &status, 0) == -1 {
    if errno != EINTR {
        fputs("recog_launcher: waitpid failed: \(String(cString: strerror(errno)))\n", stderr)
        exit(1)
    }
}

let termination = status & 0x7f
if termination == 0 {
    exit((status >> 8) & 0xff)
}
// Killed by a signal: the shell convention, so launchd logs a failure.
exit(128 + termination)
