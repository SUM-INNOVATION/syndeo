// What ci/test-app.sh asks of Launch Services and the window server, about a
// running application. Compiled once per test run with swiftc.
//
//   appctl pids <bundle-id>      the pid of each running application with it
//   appctl bundle <pid>          the bundle identifier Launch Services has for pid
//   appctl windows <pid>         one "width height" line per on-screen, ordinary
//                                window pid owns; exits 1 if there are none
//   appctl front-window-id <pid>   the window server's number for pid's front
//                                window, as Accessibility has it (see frontWindow), as
//                                screencapture -l takes it; exits 1 if there is none
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
//   appctl press-near <pid> <text> <id>   press the element with that identifier in
//                                the first of pid's windows (a dialog, a tab) whose
//                                text contains <text>; exits 1 if there is none
//   appctl window-text <pid> <text>   all the text, whole, of the first of pid's
//                                windows whose text contains <text>; exits 1 if none
//   appctl frames <pid> [<text>]   where the parts of pid's front window are, as
//                                "part x y width height" lines, y downwards, a row's
//                                title after a tab: the window, its traffic lights
//                                (lights), the page (AXWebArea), the sidebar's Hide
//                                Sidebar (hide), back, forward, reload, address, its
//                                route and Copy Link (copy), each favorite, folder
//                                and tab row (row), New Tab (new-tab), downloads and
//                                each space; AppKit's tab bar (tab-bar, its tabs
//                                appkit-tab) if it shows, tab-bar-hidden if
//                                Accessibility keeps it but it does not; and an
//                                agent's banner (indicator) and its stop, with a
//                                "covered <part> by ..." line for either that a
//                                click in its middle would not find; exits 1 if
//                                there is no front window, or it does not say <text>
//   appctl tabs <pid>            the front window's tabs, as its sidebar's rows have
//                                them, in order, one title a line, the one in front
//                                (its row selected) marked "* " and the others "  ";
//                                New Tab, the last row, is not a tab
//   appctl rows <pid>            each of the front window's sidebar rows, New Tab
//                                too, a line each: "front|-<TAB>pinned|-<TAB>folder|-
//                                <TAB>icon<TAB>title", the icon being what the row
//                                shows: own (the page's own icon), globe (a page
//                                without one), loading, new-tab, or none
//   appctl tab-bar <pid>         whether AppKit's own tab bar shows in the front
//                                window, as Accessibility has it: "hidden", or exits
//                                1 saying how many tabs it shows
//   appctl new-tab-row <pid>     press the sidebar's New Tab row, as a click would
//   appctl bar <pid>             the command bar's field, while it shows, in
//                                whichever of pid's windows has it: its value; exits
//                                1 if no command bar shows
//   appctl bar-focused <pid>     whether the command bar's field has the keys: says
//                                "focused", or exits 1
//   appctl bar-type <pid> <text>     bring pid to the front without raising any of its
//                                windows, so the command bar keeps the keys, then
//                                type the text and Return; exits 1 if no command bar
//                                shows, or pid is not frontmost
//   appctl bar-type-only <pid> <text>   the same, without Return
//   appctl bar-key <pid> <key>   the same, pressing <key>: escape, return, down or up
//   appctl bar-suggestions <pid>   the command bar's suggestions, in order, a line
//                                each: "kind<TAB>title<TAB>detail", kind being what
//                                its symbol says (Switch to tab, or Favorite)
//   appctl bar-pick <pid> <title>    click the command bar's suggestion with that
//                                title, as a person clicks it; exits 1 if none has it
//   appctl ids <pid> <id>        each element outside the page, in pid's front window,
//                                with that accessibility identifier: "role<TAB>description"
//                                a line; exits 1 if there is none
//   appctl click-web <pid> <text> <keys> <button>   click the first link in the front
//                                window's page whose text contains <text>, with the mouse,
//                                at its middle, holding <keys> (none, or cmd, shift and
//                                option joined by +), with <button> (left or middle)
//   appctl keys <pid> <key> <keys>    bring pid to the front and press <key> (a digit,
//                                t, w, d, l, s, [, ], \, escape, return, down or up)
//                                holding <keys>, as above
//   appctl menu-dump <pid> <menu>    each item of that menu, one a line:
//                                "title<TAB>key<TAB>modifiers<TAB>enabled"
//   appctl id-tree <pid>         each element outside the page in pid's front window
//                                that has an accessibility identifier: "role<TAB>identifier"
//   appctl tab-press <pid> <index>    press the front window's tab <index>, counted from 1,
//                                its row in the sidebar, as a click on it would; exits 1
//                                if there is no such tab
//   appctl menu-enabled <pid> <menu> <item>   "enabled" or "disabled", for that menu
//                                item, read with its menu open, as a person sees it;
//                                exits 1 if there is no such item
//   appctl submenu-items <pid> <menu> <item>   the title of each item in that item's
//                                menu, one a line
//   appctl submenu <pid> <menu> <item> <title>   press that item's menu's item <title>
//   appctl tab-menu <pid> <index> [<title>]   a secondary click on the front window's
//                                tab <index>, counted from 1, on its row in the sidebar,
//                                as a person makes it; print
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

func role(_ element: AXUIElement) -> String { attribute(element, kAXRoleAttribute) as? String ?? "" }
func identifier(_ element: AXUIElement) -> String { attribute(element, "AXIdentifier") as? String ?? "" }
func described(_ element: AXUIElement) -> String { oneLine(attribute(element, kAXDescriptionAttribute)) }

/// Whether `element` says `label`: what Accessibility calls it, or its help tag.
func says(_ element: AXUIElement, _ label: String) -> Bool {
    described(element) == label || oneLine(attribute(element, kAXHelpAttribute)) == label
}

/// A row of a window's sidebar, as Accessibility has it.
struct Row {
    let element: AXUIElement
    let title: String
    /// Its tab is the one in front: the row is selected.
    let active: Bool
    /// It shows a pin.
    let pinned: Bool
    /// The folder it is set in under, or "".
    let folder: String
    /// What its icon shows: see iconKind.
    let icon: String
    var isNewTab: Bool { icon == "new-tab" }
}

/// What a row's icon shows: by the identifier the sidebar gives it, as the
/// tab bar's badge had them, syndeo.tab-loading while its page loads and
/// syndeo.tab-icon for the page's own icon; otherwise by what Accessibility
/// calls it, as the symbols the sidebar draws say what they are, and a
/// page's own icon is called none of them.
func iconKind(_ icon: AXUIElement) -> String {
    switch identifier(icon) {
    case "syndeo.tab-loading": return "loading"
    case "syndeo.tab-icon": return "own"
    default: break
    }
    switch described(icon) {
    case "Loading": return "loading"
    case "Page": return "globe"
    case "New Tab": return "new-tab"
    default: return "own"
    }
}

/// How far in from its row a folder's tab's icon is, at least: the sidebar
/// sets a folder's tabs in under it.
let folderIndent: CGFloat = 19

/// The rows of the sidebar in pid's front window, in order: each tab's, and
/// New Tab, the last. Read from the front window only: another window's
/// sidebar may have no rows.
func sidebarRows(_ pid: Int32) -> [Row] {
    guard let window = frontWindow(of: pid) else { return [] }
    var rows: [Row] = []
    var folder = ""
    for element in all(window, depth: 24, { ["syndeo.sidebar-tab", "syndeo.folder"].contains(identifier($0)) }) {
        if identifier(element) == "syndeo.folder" {
            folder = described(element)
            continue
        }
        let parts = children(element)
        let images = parts.filter { role($0) == "AXImage" }
        let icon = images.first
        var indent: CGFloat = 0
        if let icon, let box = frame(of: icon), let row = frame(of: element) {
            indent = box.minX - row.minX
        }
        // A folder's tabs follow its row, set in; the first that is not ends it.
        if indent < folderIndent { folder = "" }
        var title = described(element)
        if title.isEmpty, let label = parts.first(where: { role($0) == "AXStaticText" }) {
            title = oneLine(attribute(label, kAXValueAttribute))
        }
        rows.append(Row(
            element: element,
            title: title,
            active: (attribute(element, kAXSelectedAttribute) as? Bool) == true,
            pinned: images.dropFirst().contains { identifier($0) == "syndeo.tab-pinned" || described($0) == "Pinned" },
            folder: folder,
            icon: icon.map(iconKind) ?? "none"))
    }
    return rows
}

/// The front window's tabs: its sidebar's rows, New Tab left out.
func tabRows(_ pid: Int32) -> [Row] { sidebarRows(pid).filter { !$0.isNewTab } }

/// The command bar's window and its field, while it shows: a window of its
/// own, which has the keys, so it is looked for first among the focused one.
func commandBar(_ pid: Int32) -> (window: AXUIElement, field: AXUIElement)? {
    let app = AXUIElementCreateApplication(pid)
    var candidates = windows(of: pid)
    if let focused = asElement(attribute(app, kAXFocusedWindowAttribute)) {
        candidates.insert(focused, at: 0)
    }
    for window in candidates {
        if let field = find(window, depth: 10, { identifier($0) == "syndeo.command-bar" }) {
            return (window, field)
        }
    }
    return nil
}

/// Bring pid to the front, as a click on it would, without raising any of its
/// windows: the command bar has the keys, and raising the window under it
/// would take them, and put the bar away.
func frontKeepingKeys(_ pid: Int32) -> Bool {
    _ = AXUIElementSetAttributeValue(AXUIElementCreateApplication(pid), kAXFrontmostAttribute as CFString, kCFBooleanTrue)
    for _ in 0..<50 {
        if frontmost(pid) { return true }
        usleep(100_000)
    }
    return false
}

/// One key to pid, down and up, as the keyboard sends it, nothing held.
func press(_ pid: Int32, _ code: CGKeyCode) {
    let source = CGEventSource(stateID: .hidSystemState)
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { continue }
        event.postToPid(pid)
        usleep(30_000)
    }
}

/// The command bar's suggestions, in order: what each one's symbol says it
/// does, its title and its detail, each a row's parts as Accessibility has
/// them, with the title's element.
func suggestions(_ bar: AXUIElement) -> [(kind: String, title: String, detail: String, element: AXUIElement?)] {
    var found: [(kind: String, title: String, detail: String, element: AXUIElement?)] = []
    for element in all(bar, depth: 12, { ["AXImage", "AXStaticText"].contains(role($0)) }) {
        if role(element) == "AXImage" {
            let kind = described(element)
            if ["Switch to tab", "Favorite"].contains(kind) { found.append((kind, "", "", nil)) }
            continue
        }
        guard var last = found.popLast() else { continue }
        let value = oneLine(attribute(element, kAXValueAttribute))
        if last.element == nil {
            last.title = value
            last.element = element
        } else if last.detail.isEmpty {
            last.detail = value
        }
        found.append(last)
    }
    return found
}

/// Where the parts of `window` are, as "part x y width height" lines.
func layout(_ pid: Int32, _ window: AXUIElement) {
    func line(_ part: String, _ element: AXUIElement?, _ extra: String = "") {
        guard let element, let box = frame(of: element) else { return }
        print("\(part) \(Int(box.minX)) \(Int(box.minY)) \(Int(box.width)) \(Int(box.height))\(extra.isEmpty ? "" : "\t" + extra)")
    }
    let named = [
        "syndeo.back": "back", "syndeo.forward": "forward", "syndeo.reload": "reload",
        "syndeo.address": "address", "syndeo.route": "route", "syndeo.copy": "copy",
        "syndeo.favorite": "favorite", "syndeo.folder": "folder", "syndeo.space": "space",
        "syndeo.agent-indicator": "indicator", "syndeo.agent-stop": "stop",
    ]
    line("window", window)
    for light in [kAXCloseButtonAttribute, kAXMinimizeButtonAttribute, kAXZoomButtonAttribute] {
        line("lights", asElement(attribute(window, light)))
    }
    for element in all(window, depth: 24, { _ in true }) {
        let id = identifier(element)
        if let part = named[id] {
            line(part, element)
            // An agent's banner and its Stop: anything a click on them would
            // find instead is over them.
            if ["indicator", "stop"].contains(part), let box = frame(of: element),
               let found = hit(pid, CGPoint(x: box.midX, y: box.midY)), !within(found, element) {
                print("covered \(part) by \(role(found)) \(identifier(found)) \(described(found))")
            }
        } else if id == "syndeo.sidebar-tab" {
            let icon = children(element).first { role($0) == "AXImage" }
            let isNewTab = icon.map(iconKind) == "new-tab"
            line(isNewTab ? "new-tab" : "row", element, described(element))
        } else if role(element) == "AXWebArea" {
            line("page", element)
        } else if role(element) == "AXTabGroup" {
            guard showing(pid, element) else {
                line("tab-bar-hidden", element)
                continue
            }
            line("tab-bar", element)
            for tab in children(element) where role(tab) == "AXRadioButton" {
                line("appkit-tab", tab, oneLine(attribute(tab, kAXTitleAttribute)))
            }
        } else if role(element) == "AXButton" && says(element, "Hide Sidebar") {
            line("hide", element)
        } else if role(element) == "AXButton" && says(element, "Downloads") {
            line("downloads", element)
        }
    }
}

/// The menu open in pid now, if one is: a context menu, under the application.
/// The menu open in pid now, if one is: a context menu, under the application,
/// or under the view it was opened from, in one of pid's windows; or, if
/// `near` is where it was asked for, the one Accessibility finds just below
/// and right of that point, where a context menu opens.
func openMenu(_ pid: Int32, near point: CGPoint? = nil) -> (AXUIElement, [AXUIElement])? {
    let app = AXUIElementCreateApplication(pid)
    func items(_ menu: AXUIElement) -> [AXUIElement]? {
        // Not one of the menu bar's.
        if let parent = asElement(attribute(menu, kAXParentAttribute)), role(parent) == "AXMenuBarItem" { return nil }
        let found = children(menu).filter { role($0) == "AXMenuItem" }
        return found.isEmpty ? nil : found
    }
    for _ in 0..<30 {
        var menus = all(app, depth: 3, { role($0) == "AXMenu" })
        for window in windows(of: pid) {
            menus += all(window, depth: 24, { role($0) == "AXMenu" })
        }
        if let point {
            for (dx, dy) in [(14.0, 10.0), (40.0, 10.0), (14.0, 30.0)] {
                var at = hit(pid, CGPoint(x: point.x + dx, y: point.y + dy))
                for _ in 0..<4 {
                    guard let now = at else { break }
                    if role(now) == "AXMenu" { menus.append(now); break }
                    at = asElement(attribute(now, kAXParentAttribute))
                }
            }
        }
        for menu in menus {
            if let found = items(menu) { return (menu, found) }
        }
        usleep(100_000)
    }
    return nil
}

/// Whether `element` is `ancestor`, or inside it.
func within(_ element: AXUIElement, _ ancestor: AXUIElement) -> Bool {
    var at: AXUIElement? = element
    for _ in 0..<24 {
        guard let now = at else { return false }
        if CFEqual(now, ancestor) { return true }
        at = asElement(attribute(now, kAXParentAttribute))
    }
    return false
}

/// What Accessibility finds at `point` on screen among pid's windows, as a
/// click there would.
func hit(_ pid: Int32, _ point: CGPoint) -> AXUIElement? {
    var found: AXUIElement?
    guard AXUIElementCopyElementAtPosition(AXUIElementCreateApplication(pid), Float(point.x), Float(point.y), &found) == .success else {
        return nil
    }
    return found
}

/// Whether AppKit's tab bar `bar` shows, as a person sees it: a click in
/// the middle of one of its tabs lands on it. Hidden, the bar keeps its
/// place in Accessibility's tree, its frame and all, while the title bar
/// gives it no room; a click there finds what is under it instead.
func showing(_ pid: Int32, _ bar: AXUIElement) -> Bool {
    let tabs = children(bar).filter { role($0) == "AXRadioButton" }
    for tab in tabs.isEmpty ? [bar] : tabs {
        guard let box = frame(of: tab), box.width > 0, box.height > 0,
              let found = hit(pid, CGPoint(x: box.midX, y: box.midY)) else { continue }
        if within(found, bar) { return true }
    }
    return false
}

/// The virtual key code of `key` on a US keyboard, as the runner has.
let keyCodes: [String: CGKeyCode] = [
    "1": 18, "2": 19, "3": 20, "4": 21, "5": 23, "6": 22, "7": 26, "8": 28, "9": 25, "0": 29,
    "t": 17, "w": 13, "d": 2, "l": 37, "s": 1, "[": 33, "]": 30, "\\": 42, "escape": 53,
    "return": 36, "down": 125, "up": 126,
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
case "front-window-id" where args.count == 3:
    // The on-screen window with the front window's place and size.
    guard let pid = Int32(args[2]), let front = frontWindow(of: pid),
          let place = attribute(front, kAXPositionAttribute), let extent = attribute(front, kAXSizeAttribute),
          CFGetTypeID(place) == AXValueGetTypeID(), CFGetTypeID(extent) == AXValueGetTypeID() else { exit(1) }
    var origin = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(place as! AXValue, .cgPoint, &origin),
          AXValueGetValue(extent as! AXValue, .cgSize, &size) else { exit(1) }
    let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]]) ?? []
    for window in list {
        guard (window[kCGWindowOwnerPID as String] as? Int32) == pid,
              (window[kCGWindowLayer as String] as? Int) == 0,
              let number = window[kCGWindowNumber as String] as? Int,
              let bounds = window[kCGWindowBounds as String] as? NSDictionary,
              let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { continue }
        if abs(rect.minX - origin.x) < 1, abs(rect.minY - origin.y) < 1,
           abs(rect.width - size.width) < 1, abs(rect.height - size.height) < 1 {
            print(number)
            exit(0)
        }
    }
    exit(1)
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
case "press-near" where args.count == 5:
    guard let pid = Int32(args[2]) else { exit(2) }
    for window in windows(of: pid) where text(of: window, depth: 12).contains(args[3]) {
        if let element = find(window, depth: 12, { (attribute($0, "AXIdentifier") as? String) == args[4] }) {
            exit(AXUIElementPerformAction(element, kAXPressAction as CFString) == .success ? 0 : 1)
        }
    }
    exit(1)
case "frames" where args.count == 3 || args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard let window = frontWindow(of: pid) else { print("no front window"); exit(1) }
    if args.count == 4, !text(of: window, depth: 12).contains(args[3]) {
        print("the front window, \(oneLine(attribute(window, kAXTitleAttribute))), does not say \(args[3])")
        exit(1)
    }
    layout(pid, window)
case "window-text" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    for window in windows(of: pid) {
        let all = text(of: window, depth: 12)
        if all.contains(args[3]) { print(all); exit(0) }
    }
    exit(1)
case "tabs" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    for row in tabRows(pid) {
        print("\(row.active ? "* " : "  ")\(row.title)")
    }
case "rows" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    for row in sidebarRows(pid) {
        print([row.active ? "front" : "-", row.pinned ? "pinned" : "-", row.folder.isEmpty ? "-" : row.folder,
               row.icon, row.title].joined(separator: "\t"))
    }
case "tab-bar" where args.count == 3:
    guard let pid = Int32(args[2]), let window = frontWindow(of: pid) else { exit(2) }
    let bars = all(window, depth: 24) { role($0) == "AXTabGroup" }
    let tabs = bars.flatMap { children($0) }.filter { role($0) == "AXRadioButton" }
    if let shown = bars.first(where: { showing(pid, $0) }) {
        print("shown, with \(children(shown).filter { role($0) == "AXRadioButton" }.count) tabs, at \(frame(of: shown).map { "\($0)" } ?? "?")")
        exit(1)
    }
    if bars.isEmpty {
        print("hidden: not there")
    } else {
        let landed = tabs.compactMap { frame(of: $0) }.compactMap { hit(pid, CGPoint(x: $0.midX, y: $0.midY)) }
            .map { "\(role($0)) \(identifier($0))" }
        print("hidden: Accessibility keeps it, with \(tabs.count) tabs, but a click on them lands on: \(landed.joined(separator: ", "))")
    }
case "new-tab-row" where args.count == 3:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard let row = sidebarRows(pid).last(where: { $0.isNewTab }) else { print("no New Tab row"); exit(1) }
    exit(AXUIElementPerformAction(row.element, kAXPressAction as CFString) == .success ? 0 : 1)
case "bar" where args.count == 3:
    guard let pid = Int32(args[2]), let (_, field) = commandBar(pid) else { exit(1) }
    print(oneLine(attribute(field, kAXValueAttribute)))
case "bar-focused" where args.count == 3:
    guard let pid = Int32(args[2]), let (_, field) = commandBar(pid) else { print("no command bar"); exit(1) }
    let focused = asElement(attribute(AXUIElementCreateApplication(pid), kAXFocusedUIElementAttribute))
    if (attribute(field, kAXFocusedAttribute) as? Bool) == true || focused.map({ identifier($0) == "syndeo.command-bar" }) == true {
        print("focused")
        exit(0)
    }
    print("not focused: the keys are with \(focused.map { "\(role($0)) \(identifier($0))" } ?? "nothing")")
    exit(1)
case "bar-type" where args.count == 4, "bar-type-only" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    guard commandBar(pid) != nil else { print("no command bar"); exit(1) }
    guard frontKeepingKeys(pid) else { print("not frontmost"); exit(1) }
    for character in args[3] {
        key(pid, 0, String(character))
    }
    if args[1] == "bar-type" { key(pid, 36, "\r") }
case "bar-key" where args.count == 4:
    guard let pid = Int32(args[2]), let code = keyCodes[args[3]], ["escape", "return", "down", "up"].contains(args[3]) else { exit(2) }
    guard commandBar(pid) != nil else { print("no command bar"); exit(1) }
    guard frontKeepingKeys(pid) else { print("not frontmost"); exit(1) }
    switch args[3] {
    case "escape": key(pid, code, "\u{1b}")
    case "return": key(pid, code, "\r")
    default: press(pid, code)
    }
case "bar-suggestions" where args.count == 3:
    guard let pid = Int32(args[2]), let (bar, _) = commandBar(pid) else { exit(1) }
    for suggestion in suggestions(bar) {
        print("\(suggestion.kind)\t\(suggestion.title)\t\(suggestion.detail)")
    }
case "bar-pick" where args.count == 4:
    guard let pid = Int32(args[2]), let (bar, _) = commandBar(pid) else { print("no command bar"); exit(1) }
    let listed = suggestions(bar)
    guard let chosen = listed.first(where: { $0.title == args[3] }), let element = chosen.element, let box = frame(of: element) else {
        print("no suggestion \(args[3]): \(listed.map { $0.title }.joined(separator: " | "))")
        exit(1)
    }
    click(CGPoint(x: box.midX, y: box.midY), .left, [])
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
    // As a person presses them, through the system's own event stream: the
    // keys held pressed first and let go last, so the application sees a
    // shortcut, not a character.
    guard let pid = Int32(args[2]), let code = keyCodes[args[3]] else { exit(2) }
    guard activate(pid) else { print("not frontmost"); exit(1) }
    let source = CGEventSource(stateID: .hidSystemState)
    let held = flags(args[4])
    var down: CGEventFlags = []
    for (modifier, flag) in modifierKeys(held) {
        down.insert(flag)
        let event = CGEvent(keyboardEventSource: source, virtualKey: modifier, keyDown: true)
        event?.flags = down
        event?.post(tap: .cghidEventTap)
        usleep(50_000)
    }
    for pressed in [true, false] {
        let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: pressed)
        event?.flags = held
        event?.post(tap: .cghidEventTap)
        usleep(50_000)
    }
    for (modifier, flag) in modifierKeys(held).reversed() {
        down.remove(flag)
        let event = CGEvent(keyboardEventSource: source, virtualKey: modifier, keyDown: false)
        event?.flags = down
        event?.post(tap: .cghidEventTap)
        usleep(50_000)
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
case "menu-dump" where args.count == 4:
    guard let pid = Int32(args[2]) else { exit(2) }
    for item in menuItems(pid, args[3]) {
        let modifiers = (attribute(item, kAXMenuItemCmdModifiersAttribute) as? Int).map(String.init) ?? "?"
        let enabled = (attribute(item, kAXEnabledAttribute) as? Bool) == true ? "enabled" : "disabled"
        print("\(oneLine(attribute(item, kAXTitleAttribute)))\t\(oneLine(attribute(item, kAXMenuItemCmdCharAttribute)))\t\(modifiers)\t\(enabled)")
    }
case "id-tree" where args.count == 3:
    guard let pid = Int32(args[2]), let window = frontWindow(of: pid) else { exit(1) }
    for element in all(window, depth: 24, { attribute($0, "AXIdentifier") != nil }) {
        print("\(attribute(element, kAXRoleAttribute) as? String ?? "?")\t\(oneLine(attribute(element, "AXIdentifier")))")
    }
case "tab-press" where args.count == 4:
    guard let pid = Int32(args[2]), let index = Int(args[3]) else { exit(2) }
    let tabs = tabRows(pid)
    guard index >= 1, index <= tabs.count else { print("no tab \(args[3]) of \(tabs.count)"); exit(1) }
    exit(AXUIElementPerformAction(tabs[index - 1].element, kAXPressAction as CFString) == .success ? 0 : 1)
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
    let tabs = tabRows(pid)
    guard index >= 1, index <= tabs.count, let box = frame(of: tabs[index - 1].element) else {
        print("no tab \(args[3]) of \(tabs.count)")
        exit(1)
    }
    // Left of the middle, on its title, away from the row's Close or pin.
    let point = CGPoint(x: box.minX + box.width * 0.4, y: box.midY)
    click(point, .right, [])
    guard let (menu, items) = openMenu(pid, near: point) else { print("no menu opened"); exit(1) }
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
