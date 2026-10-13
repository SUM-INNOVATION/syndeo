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
//   appctl ax-close <pid>        press the close button of each of pid's windows, as a
//                                person would; exits 1 if there was none to press
//   appctl ax-tree <pid>         each accessibility element in pid's windows, one
//                                "role<TAB>text<TAB>title" line each
//   appctl front-tree <pid>      the same, for pid's front window only
//   appctl titles <pid>          the title of each window Accessibility has for pid
//   appctl activate <pid>        bring pid to the front; exits 1 if it is not frontmost
//   appctl type <pid> <text>     bring pid to the front, then ⌘L, the text and Return,
//                                as keystrokes; exits 1 if pid is not frontmost
//   appctl keys <pid> <combo>    bring pid to the front and press one key: cmd+w,
//                                cmd+t, cmd+l, cmd+r, cmd+[, cmd+], cmd+. or return
//   appctl press <pid> <id>      press the element with that accessibility identifier
//                                in pid's front window; exits 1 if there is none
//   appctl value <pid> <id>      that element's value
//   appctl menu <pid> <menu> <item>   press a menu item, as a click on it would
//   appctl press-link <pid> <text>    press the first link in the front window whose
//                                text contains <text>
import AppKit
import ApplicationServices
import CoreGraphics

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
}

func windows(of pid: Int32) -> [AXUIElement] {
    attribute(AXUIElementCreateApplication(pid), kAXWindowsAttribute) as? [AXUIElement] ?? []
}

func children(_ element: AXUIElement) -> [AXUIElement] {
    attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []
}

func oneLine(_ value: CFTypeRef?) -> String {
    (value as? String ?? "").replacingOccurrences(of: "\n", with: " ")
}

func walk(_ element: AXUIElement, depth: Int) {
    let role = attribute(element, kAXRoleAttribute) as? String ?? "?"
    let text = oneLine(attribute(element, kAXValueAttribute))
    let title = oneLine(attribute(element, kAXTitleAttribute))
    print("\(role)\t\(text.prefix(300))\t\(title.prefix(120))")
    guard depth > 0 else { return }
    for child in children(element) {
        walk(child, depth: depth - 1)
    }
}

func asElement(_ value: CFTypeRef?) -> AXUIElement? {
    guard let value, CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
    return (value as! AXUIElement)
}

/// The window in front: the main window, else the focused one, else the first.
func frontWindow(of pid: Int32) -> AXUIElement? {
    let app = AXUIElementCreateApplication(pid)
    return asElement(attribute(app, kAXMainWindowAttribute))
        ?? asElement(attribute(app, kAXFocusedWindowAttribute))
        ?? windows(of: pid).first
}

func find(_ element: AXUIElement, depth: Int, _ matches: (AXUIElement) -> Bool) -> AXUIElement? {
    if matches(element) { return element }
    guard depth > 0 else { return nil }
    for child in children(element) {
        if let found = find(child, depth: depth - 1, matches) { return found }
    }
    return nil
}

func identified(_ pid: Int32, _ id: String) -> AXUIElement? {
    guard let window = frontWindow(of: pid) else { return nil }
    return find(window, depth: 12) { (attribute($0, "AXIdentifier") as? String) == id }
}

/// All the text under an element, values and titles, joined.
func text(of element: AXUIElement, depth: Int = 8) -> String {
    var parts = [oneLine(attribute(element, kAXValueAttribute)), oneLine(attribute(element, kAXTitleAttribute))]
    if depth > 0 {
        parts += children(element).map { text(of: $0, depth: depth - 1) }
    }
    return parts.filter { !$0.isEmpty }.joined(separator: " ")
}

func frontmost(_ pid: Int32) -> Bool {
    (attribute(AXUIElementCreateApplication(pid), kAXFrontmostAttribute) as? Bool) == true
}

/// Bring pid to the front, as a click on it would, and wait until it is.
func activate(_ pid: Int32) -> Bool {
    let app = AXUIElementCreateApplication(pid)
    _ = AXUIElementSetAttributeValue(app, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
    if let window = frontWindow(of: pid) {
        _ = AXUIElementPerformAction(window, kAXRaiseAction as CFString)
    }
    for _ in 0..<50 {
        if frontmost(pid) { return true }
        usleep(100_000)
    }
    return false
}

/// One keystroke to pid: down and up, with `flags`, typing `text`.
func key(_ pid: Int32, _ code: CGKeyCode, _ text: String, _ flags: CGEventFlags = []) {
    let source = CGEventSource(stateID: .hidSystemState)
    let units = Array(text.utf16)
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { continue }
        event.flags = flags
        event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
        event.postToPid(pid)
        usleep(30_000)
    }
}

/// The keys `keys` presses: virtual key code and the character, US layout.
let combos: [String: (CGKeyCode, String, CGEventFlags)] = [
    "cmd+w": (13, "w", .maskCommand),
    "cmd+t": (17, "t", .maskCommand),
    "cmd+l": (37, "l", .maskCommand),
    "cmd+r": (15, "r", .maskCommand),
    "cmd+[": (33, "[", .maskCommand),
    "cmd+]": (30, "]", .maskCommand),
    "cmd+.": (47, ".", .maskCommand),
    "return": (36, "\r", []),
]

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
case "ax-close" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    var pressed = 0
    for window in windows(of: pid) {
        if let button = attribute(window, kAXCloseButtonAttribute),
           AXUIElementPerformAction(button as! AXUIElement, kAXPressAction as CFString) == .success {
            pressed += 1
        }
    }
    exit(pressed > 0 ? 0 : 1)
case "ax-tree" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    for window in windows(of: pid) {
        walk(window, depth: 24)
    }
case "front-tree" where args.count == 3:
    guard let pid = Int32(args[2]), let window = frontWindow(of: pid) else { exit(1) }
    walk(window, depth: 24)
case "titles" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    for window in windows(of: pid) {
        print(oneLine(attribute(window, kAXTitleAttribute)))
    }
case "activate" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    exit(activate(pid) ? 0 : 1)
case "type" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard activate(pid) else { print("not frontmost"); exit(1) }
    let (code, character, flags) = combos["cmd+l"]!
    key(pid, code, character, flags)
    usleep(300_000)
    for character in args[3] {
        key(pid, 0, String(character))
    }
    let (enter, newline, _) = combos["return"]!
    key(pid, enter, newline)
case "keys" where args.count == 4:
    guard let pid = Int32(args[2]), let (code, character, flags) = combos[args[3]] else { exit(2) }
    guard activate(pid) else { print("not frontmost"); exit(1) }
    key(pid, code, character, flags)
case "press" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard let element = identified(pid, args[3]) else { exit(1) }
    exit(AXUIElementPerformAction(element, kAXPressAction as CFString) == .success ? 0 : 1)
case "value" where args.count == 4:
    guard let pid = Int32(args[2]), let element = identified(pid, args[3]) else { exit(1) }
    print(oneLine(attribute(element, kAXValueAttribute)))
case "menu" where args.count == 5:
    guard let pid = Int32(args[2]),
          let bar = asElement(attribute(AXUIElementCreateApplication(pid), kAXMenuBarAttribute)) else { exit(1) }
    for top in children(bar) where oneLine(attribute(top, kAXTitleAttribute)) == args[3] {
        for menu in children(top) {
            for item in children(menu) where oneLine(attribute(item, kAXTitleAttribute)) == args[4] {
                exit(AXUIElementPerformAction(item, kAXPressAction as CFString) == .success ? 0 : 1)
            }
        }
    }
    exit(1)
case "press-link" where args.count == 4:
    guard let pid = Int32(args[2]), let window = frontWindow(of: pid) else { exit(1) }
    let link = find(window, depth: 24) {
        (attribute($0, kAXRoleAttribute) as? String) == "AXLink" && text(of: $0).contains(args[3])
    }
    guard let link else { exit(1) }
    exit(AXUIElementPerformAction(link, kAXPressAction as CFString) == .success ? 0 : 1)
default:
    exit(2)
}
