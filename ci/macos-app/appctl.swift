// What ci/test-app.sh asks of Launch Services and the window server, about a
// running application. Compiled once per test run with swiftc.
//
//   appctl pids <bundle-id>      the pid of each running application with it
//   appctl bundle <pid>          the bundle identifier Launch Services has for pid
//   appctl windows <pid>         one "width height" line per on-screen, ordinary
//                                window pid owns; exits 1 if there are none
//   appctl quit <bundle-id>      ask each running application with it to quit,
//                                as the Quit menu item would; exits 1 if none was asked
//   appctl stop <bundle-id>      quit each running application with it and wait,
//                                up to 10 s, until its process is gone; then force
//                                any left and wait 10 s more. Says "quit", "forced"
//                                or "still running"; exits 1 only for the last
//   appctl ax                    whether this process is trusted for Accessibility
//   appctl ax-close <pid>        press the close button of each of pid's windows, as a
//                                person would; exits 1 if there was none to press
//   appctl ax-tree <pid>         each accessibility element in pid's windows, one
//                                "role<TAB>text<TAB>title" line each
//   appctl front-tree <pid>      the same, for pid's front window only
//   appctl titles <pid>          the title of each window Accessibility has for pid
//   appctl activate <pid>        bring pid to the front; exits 1 if it is not frontmost
//   appctl type <pid> <text>     bring pid to the front, then type the text and
//                                Return, as keystrokes; exits 1 if pid is not frontmost
//   appctl type-only <pid> <text>     the same, without Return
//   appctl tab <pid>             bring pid to the front and press Tab
//   appctl press <pid> <id>      press the element with that accessibility identifier
//                                in pid's front window; exits 1 if there is none
//   appctl value <pid> <id>      that element's value
//   appctl enabled <pid> <id>    "enabled" or "disabled", for that element
//   appctl describe <pid> <id>   that element's description and help (its
//                                tooltip), "description<TAB>help"
//   appctl menu <pid> <menu> <item>   press a menu item, as a click on it would;
//                                exits 1 if there is no such item
//   appctl menu-items <pid> <menu>    the title of each item in that menu, one a line
//   appctl menu-key <pid> <menu> <key>   press the one enabled item in that menu
//                                whose key equivalent is exactly Command and <key>,
//                                whatever its title, and print its title; exits 1,
//                                listing them, unless there is exactly one
//   appctl shortcut <pid> <menu> <item>   the item's key equivalent, as
//                                "<character> <modifiers>": 0 is Command alone
//   appctl press-web <pid> <text>     press the first link or button in the front
//                                window's page whose text contains <text>, as a click
//                                would; exits 1 if there is none
//   appctl apps                  the bundle identifier of each running application
//                                that has a Dock icon, sorted, one a line
//   appctl pick <pid> <id> <title>    open the pop-up button with that identifier in
//                                pid's front window and choose its item <title>, as
//                                a person would; exits 1 if there is no such item
//   appctl popup-items <pid> <id>     open that pop-up, print each item's title, one a
//                                line, and close it again choosing nothing
//   appctl tabs <pid>            the front window's tabs, as its tab bar has them, in
//                                order, one title a line, the one in front marked "* "
//                                and the others "  "; nothing for a window with no
//                                tab bar showing
//   appctl ids <pid> <id>        each element outside the page, in pid's front window,
//                                with that accessibility identifier: "role<TAB>description"
//                                a line; exits 1 if there is none
//   appctl click-web <pid> <text> <keys> <button>   click the first link in the front
//                                window's page whose text contains <text>, with the mouse,
//                                at its middle, holding <keys> (none, or cmd, shift and
//                                option joined by +), with <button> (left or middle)
//   appctl keys <pid> <key> <keys>    bring pid to the front and press <key> (a digit,
//                                a letter, [, ], \ or escape) holding <keys>, as above
//   appctl menu-enabled <pid> <menu> <item>   "enabled" or "disabled", for that menu
//                                item, read with its menu open, as a person sees it;
//                                exits 1 if there is no such item
//   appctl submenu-items <pid> <menu> <item>   the title of each item in that item's
//                                menu, one a line
//   appctl submenu <pid> <menu> <item> <title>   press that item's menu's item <title>
//   appctl tab-menu <pid> <index> [<title>]   a secondary click on the front window's
//                                tab <index>, counted from 1, as a person makes it; print
//                                each item of the menu it opens, one a line, then press
//                                <title>, or close it choosing nothing; exits 1 if no menu
//                                opened, or there is no such item
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

/// One keystroke to pid, down and up, typing `text`.
func key(_ pid: Int32, _ code: CGKeyCode, _ text: String) {
    let source = CGEventSource(stateID: .hidSystemState)
    let units = Array(text.utf16)
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { continue }
        event.flags = []
        event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
        event.postToPid(pid)
        usleep(30_000)
    }
}

/// The element's frame on screen, top-left origin, as CGEvent has the screen.
func frame(of element: AXUIElement) -> CGRect? {
    guard let position = attribute(element, kAXPositionAttribute), let size = attribute(element, kAXSizeAttribute),
          CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
    var origin = CGPoint.zero, extent = CGSize.zero
    guard AXValueGetValue(position as! AXValue, .cgPoint, &origin), AXValueGetValue(size as! AXValue, .cgSize, &extent) else { return nil }
    return CGRect(origin: origin, size: extent)
}

/// Keys held, from "none" or "cmd+shift+option" in any order.
func flags(_ words: String) -> CGEventFlags {
    var held: CGEventFlags = []
    for word in words.split(separator: "+") {
        switch word {
        case "cmd": held.insert(.maskCommand)
        case "shift": held.insert(.maskShift)
        case "option": held.insert(.maskAlternate)
        case "ctrl": held.insert(.maskControl)
        default: break
        }
    }
    return held
}

/// The modifier keys in `held`, each with its virtual key code.
func modifierKeys(_ held: CGEventFlags) -> [(CGKeyCode, CGEventFlags)] {
    [(55, .maskCommand), (56, .maskShift), (58, .maskAlternate), (59, .maskControl)].filter { held.contains($0.1) }
}

/// A click at `point`, as a person makes it: the pointer moved there, the
/// keys in `held` pressed, the button down and up, and the keys let go.
func click(_ point: CGPoint, _ button: CGMouseButton, _ held: CGEventFlags) {
    let source = CGEventSource(stateID: .hidSystemState)
    CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)?
        .post(tap: .cghidEventTap)
    usleep(150_000)
    var down: CGEventFlags = []
    for (code, flag) in modifierKeys(held) {
        down.insert(flag)
        let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: true)
        event?.flags = down
        event?.post(tap: .cghidEventTap)
        usleep(50_000)
    }
    defer {
        for (code, flag) in modifierKeys(held).reversed() {
            down.remove(flag)
            let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false)
            event?.flags = down
            event?.post(tap: .cghidEventTap)
            usleep(50_000)
        }
    }
    let (pressed, released): (CGEventType, CGEventType) = switch button {
    case .left: (.leftMouseDown, .leftMouseUp)
    case .right: (.rightMouseDown, .rightMouseUp)
    default: (.otherMouseDown, .otherMouseUp)
    }
    for type in [pressed, released] {
        guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point, mouseButton: button) else { continue }
        event.flags = held
        event.setIntegerValueField(.mouseEventClickState, value: 1)
        event.post(tap: .cghidEventTap)
        usleep(80_000)
    }
}

/// Each element under `element` matching, not looking inside pages.
func all(_ element: AXUIElement, depth: Int, _ matches: (AXUIElement) -> Bool) -> [AXUIElement] {
    var found = matches(element) ? [element] : []
    guard depth > 0, (attribute(element, kAXRoleAttribute) as? String) != "AXWebArea" else { return found }
    for child in children(element) {
        found += all(child, depth: depth - 1, matches)
    }
    return found
}

/// The front window's tab bar's tabs, in order.
func tabButtons(_ pid: Int32) -> [AXUIElement] {
    guard let window = frontWindow(of: pid),
          let bar = all(window, depth: 6, { (attribute($0, kAXRoleAttribute) as? String) == "AXTabGroup" }).first else { return [] }
    return children(bar).filter { (attribute($0, kAXRoleAttribute) as? String) == "AXRadioButton" }
}

/// The menu open in pid now, if one is: a context menu, under the application.
func openMenu(_ pid: Int32) -> (AXUIElement, [AXUIElement])? {
    let app = AXUIElementCreateApplication(pid)
    for _ in 0..<30 {
        for menu in all(app, depth: 3, { (attribute($0, kAXRoleAttribute) as? String) == "AXMenu" }) {
            // Not one of the menu bar's.
            if let parent = asElement(attribute(menu, kAXParentAttribute)),
               (attribute(parent, kAXRoleAttribute) as? String) == "AXMenuBarItem" { continue }
            let items = children(menu).filter { (attribute($0, kAXRoleAttribute) as? String) == "AXMenuItem" }
            if !items.isEmpty { return (menu, items) }
        }
        usleep(100_000)
    }
    return nil
}

/// The virtual key code of `key` on a US keyboard, as the runner has.
let keyCodes: [String: CGKeyCode] = [
    "1": 18, "2": 19, "3": 20, "4": 21, "5": 23, "6": 22, "7": 26, "8": 28, "9": 25, "0": 29,
    "t": 17, "w": 13, "d": 2, "l": 37, "[": 33, "]": 30, "\\": 42, "escape": 53,
]

/// The items of the menu bar item `menu`.
func menuItems(_ pid: Int32, _ menu: String) -> [AXUIElement] {
    guard let bar = asElement(attribute(AXUIElementCreateApplication(pid), kAXMenuBarAttribute)) else { return [] }
    return children(bar)
        .filter { oneLine(attribute($0, kAXTitleAttribute)) == menu }
        .flatMap { children($0) }
        .flatMap { children($0) }
}

/// The menu bar item `menu`'s item `title`.
func menuItem(_ pid: Int32, _ menu: String, _ title: String) -> AXUIElement? {
    menuItems(pid, menu).first { oneLine(attribute($0, kAXTitleAttribute)) == title }
}

/// Open the pop-up button with identifier `id` in pid's front window, and
/// wait for its items.
func openPopUp(_ pid: Int32, _ id: String) -> (AXUIElement, [AXUIElement])? {
    guard let popup = identified(pid, id),
          AXUIElementPerformAction(popup, kAXPressAction as CFString) == .success else { return nil }
    let app = AXUIElementCreateApplication(pid)
    for _ in 0..<30 {
        // Its menu, while open: under the button, or under the application.
        for root in [popup, app] {
            if let menu = find(root, depth: 3, { (attribute($0, kAXRoleAttribute) as? String) == "AXMenu" }) {
                let items = children(menu).filter { (attribute($0, kAXRoleAttribute) as? String) == "AXMenuItem" }
                if !items.isEmpty { return (menu, items) }
            }
        }
        usleep(100_000)
    }
    return nil
}

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
case "stop" where args.count == 3:
    let apps = NSRunningApplication.runningApplications(withBundleIdentifier: args[2])
    let pids = apps.map { $0.processIdentifier }
    // Gone when the kernel has no such process: not what this process's copy
    // of NSRunningApplication says, which needs a run loop to change.
    func gone() -> Bool { pids.allSatisfy { kill($0, 0) != 0 && errno == ESRCH } }
    func wait() { for _ in 0..<100 where !gone() { usleep(100_000) } }
    for app in apps { _ = app.terminate() }
    wait()
    if gone() { print("quit"); exit(0) }
    for app in apps { _ = app.forceTerminate() }
    wait()
    if gone() { print("forced"); exit(0) }
    print("still running")
    exit(1)
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
    for character in args[3] {
        key(pid, 0, String(character))
    }
    key(pid, 36, "\r")
case "type-only" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard activate(pid) else { print("not frontmost"); exit(1) }
    for character in args[3] {
        key(pid, 0, String(character))
    }
case "tab" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard activate(pid) else { print("not frontmost"); exit(1) }
    key(pid, 48, "\t")
case "press" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard let element = identified(pid, args[3]) else { exit(1) }
    exit(AXUIElementPerformAction(element, kAXPressAction as CFString) == .success ? 0 : 1)
case "value" where args.count == 4:
    guard let pid = Int32(args[2]), let element = identified(pid, args[3]) else { exit(1) }
    print(oneLine(attribute(element, kAXValueAttribute)))
case "enabled" where args.count == 4:
    guard let pid = Int32(args[2]), let element = identified(pid, args[3]) else { exit(1) }
    print((attribute(element, kAXEnabledAttribute) as? Bool) == true ? "enabled" : "disabled")
case "describe" where args.count == 4:
    guard let pid = Int32(args[2]), let element = identified(pid, args[3]) else { exit(1) }
    print("\(oneLine(attribute(element, kAXDescriptionAttribute)))\t\(oneLine(attribute(element, kAXHelpAttribute)))")
case "menu" where args.count == 5:
    guard let pid = Int32(args[2]), let item = menuItem(pid, args[3], args[4]) else { exit(1) }
    exit(AXUIElementPerformAction(item, kAXPressAction as CFString) == .success ? 0 : 1)
case "menu-items" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    for item in menuItems(pid, args[3]) {
        print(oneLine(attribute(item, kAXTitleAttribute)))
    }
case "menu-key" where args.count == 5:
    guard let pid = Int32(args[2]) else { exit(2) }
    let matching = menuItems(pid, args[3]).filter {
        oneLine(attribute($0, kAXMenuItemCmdCharAttribute)).uppercased() == args[4].uppercased()
            && (attribute($0, kAXMenuItemCmdModifiersAttribute) as? Int) == 0
            && (attribute($0, kAXEnabledAttribute) as? Bool) == true
    }
    let titles = matching.map { oneLine(attribute($0, kAXTitleAttribute)) }
    guard matching.count == 1 else {
        print("\(matching.count) enabled items with Command-\(args[4]): \(titles.joined(separator: " | "))")
        exit(1)
    }
    print(titles[0])
    exit(AXUIElementPerformAction(matching[0], kAXPressAction as CFString) == .success ? 0 : 1)
case "shortcut" where args.count == 5:
    guard let pid = Int32(args[2]), let item = menuItem(pid, args[3], args[4]) else { exit(1) }
    let character = oneLine(attribute(item, kAXMenuItemCmdCharAttribute))
    let modifiers = (attribute(item, kAXMenuItemCmdModifiersAttribute) as? Int).map(String.init) ?? "?"
    print("\(character) \(modifiers)")
case "press-web" where args.count == 4:
    guard let pid = Int32(args[2]), let window = frontWindow(of: pid),
          let page = find(window, depth: 12, { (attribute($0, kAXRoleAttribute) as? String) == "AXWebArea" }) else { exit(1) }
    let control = find(page, depth: 24) {
        ["AXLink", "AXButton"].contains(attribute($0, kAXRoleAttribute) as? String ?? "") && text(of: $0).contains(args[3])
    }
    guard let control else { exit(1) }
    exit(AXUIElementPerformAction(control, kAXPressAction as CFString) == .success ? 0 : 1)
case "apps" where args.count == 2:
    let ids = NSWorkspace.shared.runningApplications
        .filter { $0.activationPolicy == .regular }
        .compactMap { $0.bundleIdentifier }
    print(Set(ids).sorted().joined(separator: "\n"))
case "pick" where args.count == 5:
    guard let pid = Int32(args[2]), activate(pid), let (menu, items) = openPopUp(pid, args[3]) else { exit(1) }
    guard let item = items.first(where: { oneLine(attribute($0, kAXTitleAttribute)) == args[4] }) else {
        _ = AXUIElementPerformAction(menu, kAXCancelAction as CFString)
        exit(1)
    }
    exit(AXUIElementPerformAction(item, kAXPressAction as CFString) == .success ? 0 : 1)
case "popup-items" where args.count == 4:
    guard let pid = Int32(args[2]), activate(pid), let (menu, items) = openPopUp(pid, args[3]) else { exit(1) }
    for item in items {
        print(oneLine(attribute(item, kAXTitleAttribute)))
    }
    // Closed by Cancel, or by Escape if the menu does not take Cancel.
    if AXUIElementPerformAction(menu, kAXCancelAction as CFString) != .success {
        key(pid, 53, "\u{1b}")
    }
case "tabs" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    for tab in tabButtons(pid) {
        let front = (attribute(tab, kAXValueAttribute) as? Int) == 1
        print("\(front ? "* " : "  ")\(oneLine(attribute(tab, kAXTitleAttribute)))")
    }
case "ids" where args.count == 4:
    guard let pid = Int32(args[2]), let window = frontWindow(of: pid) else { exit(2) }
    let found = all(window, depth: 24) { (attribute($0, "AXIdentifier") as? String) == args[3] }
    for element in found {
        print("\(attribute(element, kAXRoleAttribute) as? String ?? "?")\t\(oneLine(attribute(element, kAXDescriptionAttribute)))")
    }
    exit(found.isEmpty ? 1 : 0)
case "click-web" where args.count == 6:
    guard let pid = Int32(args[2]), activate(pid), let window = frontWindow(of: pid),
          let page = find(window, depth: 12, { (attribute($0, kAXRoleAttribute) as? String) == "AXWebArea" }) else { exit(1) }
    let link = find(page, depth: 24) {
        (attribute($0, kAXRoleAttribute) as? String) == "AXLink" && text(of: $0).contains(args[3])
    }
    guard let link, let box = frame(of: link) else { exit(1) }
    let button: CGMouseButton = args[5] == "middle" ? .center : .left
    click(CGPoint(x: box.midX, y: box.midY), button, flags(args[4]))
case "keys" where args.count == 5:
    guard let pid = Int32(args[2]), let code = keyCodes[args[3]] else { exit(2) }
    guard activate(pid) else { print("not frontmost"); exit(1) }
    let source = CGEventSource(stateID: .hidSystemState)
    let typed = args[3] == "escape" ? "\u{1b}" : args[3]
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { continue }
        event.flags = flags(args[4])
        event.keyboardSetUnicodeString(stringLength: typed.utf16.count, unicodeString: Array(typed.utf16))
        event.postToPid(pid)
        usleep(30_000)
    }
case "menu-enabled" where args.count == 5:
    // Opened first, as a person opens it, so AppKit asks the application
    // which of its items it offers now; then closed again.
    guard let pid = Int32(args[2]), activate(pid),
          let bar = asElement(attribute(AXUIElementCreateApplication(pid), kAXMenuBarAttribute)),
          let top = children(bar).first(where: { oneLine(attribute($0, kAXTitleAttribute)) == args[3] }) else { exit(1) }
    _ = AXUIElementPerformAction(top, kAXPressAction as CFString)
    usleep(400_000)
    let item = menuItem(pid, args[3], args[4])
    let enabled = item.map { (attribute($0, kAXEnabledAttribute) as? Bool) == true }
    let closed = children(top).first.map { AXUIElementPerformAction($0, kAXCancelAction as CFString) == .success } ?? false
    if !closed {
        key(pid, 53, "\u{1b}")
    }
    guard let enabled else { exit(1) }
    print(enabled ? "enabled" : "disabled")
case "submenu-items" where args.count == 5:
    guard let pid = Int32(args[2]), let item = menuItem(pid, args[3], args[4]) else { exit(1) }
    for entry in children(item).flatMap({ children($0) }) {
        print(oneLine(attribute(entry, kAXTitleAttribute)))
    }
case "submenu" where args.count == 6:
    guard let pid = Int32(args[2]), let item = menuItem(pid, args[3], args[4]) else { exit(1) }
    guard let entry = children(item).flatMap({ children($0) })
        .first(where: { oneLine(attribute($0, kAXTitleAttribute)) == args[5] }) else { exit(1) }
    exit(AXUIElementPerformAction(entry, kAXPressAction as CFString) == .success ? 0 : 1)
case "tab-menu" where args.count == 4 || args.count == 5:
    guard let pid = Int32(args[2]), let index = Int(args[3]), activate(pid) else { exit(1) }
    let tabs = tabButtons(pid)
    guard index >= 1, index <= tabs.count, let box = frame(of: tabs[index - 1]) else {
        print("no tab \(args[3]) of \(tabs.count)")
        exit(1)
    }
    // Left of the middle, away from the tab's close button and its badge.
    click(CGPoint(x: box.minX + box.width * 0.4, y: box.midY), .right, [])
    guard let (menu, items) = openMenu(pid) else { print("no menu opened"); exit(1) }
    for item in items {
        print(oneLine(attribute(item, kAXTitleAttribute)))
    }
    if args.count == 5 {
        guard let item = items.first(where: { oneLine(attribute($0, kAXTitleAttribute)) == args[4] }) else {
            _ = AXUIElementPerformAction(menu, kAXCancelAction as CFString)
            exit(1)
        }
        exit(AXUIElementPerformAction(item, kAXPressAction as CFString) == .success ? 0 : 1)
    }
    if AXUIElementPerformAction(menu, kAXCancelAction as CFString) != .success {
        key(pid, 53, "\u{1b}")
    }
default:
    exit(2)
}
