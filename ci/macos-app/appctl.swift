// What ci/test-app.sh asks of Launch Services and the window server, about a
// running application. Compiled once per test run with swiftc.
//
//   appctl pids <bundle-id>      the pid of each running application with it
//   appctl bundle <pid>          the bundle identifier Launch Services has for pid
//   appctl windows <pid>         one "width height" line per on-screen, ordinary
//                                window pid owns; exits 1 if there are none
//   appctl quit <bundle-id>      ask each running application with it to quit,
//                                as the Quit menu item would; exits 1 if none was asked
//   appctl ax                    whether this process is trusted for Accessibility
import AppKit
import ApplicationServices
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2 else { exit(2) }

switch args[1] {
case "pids" where args.count == 3:
    for app in NSRunningApplication.runningApplications(withBundleIdentifier: args[2]) {
        print(app.processIdentifier)
    }
case "bundle" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    print(NSRunningApplication(processIdentifier: pid)?.bundleIdentifier ?? "")
case "windows" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]]) ?? []
    var found = 0
    for window in list {
        guard (window[kCGWindowOwnerPID as String] as? Int32) == pid,
              (window[kCGWindowLayer as String] as? Int) == 0,
              let bounds = window[kCGWindowBounds as String] as? [String: Any] else { continue }
        print("\(bounds["Width"] ?? 0) \(bounds["Height"] ?? 0)")
        found += 1
    }
    exit(found > 0 ? 0 : 1)
case "quit" where args.count == 3:
    var asked = false
    for app in NSRunningApplication.runningApplications(withBundleIdentifier: args[2]) {
        asked = app.terminate() || asked
    }
    exit(asked ? 0 : 1)
case "ax" where args.count == 2:
    print(AXIsProcessTrusted() ? "trusted" : "not trusted")
default:
    exit(2)
}
