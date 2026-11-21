// Independent release check using only the same public key embedded in Recog.
import CryptoKit
import Foundation

let args = Array(CommandLine.arguments.dropFirst())
guard args.count == 5,
      let rawKey = Data(base64Encoded: try String(contentsOfFile: args[0], encoding: .utf8)
        .trimmingCharacters(in: .whitespacesAndNewlines)) else { exit(2) }
let key = try Curve25519.Signing.PublicKey(rawRepresentation: rawKey)
for (file, encoded) in [(args[1], args[2]), (args[3], args[4])] {
    let data = try Data(contentsOf: URL(fileURLWithPath: file), options: .mappedIfSafe)
    guard let signature = Data(base64Encoded: encoded),
          key.isValidSignature(signature, for: data) else {
        FileHandle.standardError.write(Data("Sparkle signature verification failed\n".utf8))
        exit(1)
    }
}
