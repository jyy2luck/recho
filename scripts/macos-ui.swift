// Native UI smoke helper: operates only the PID passed by the test.
import AppKit
import ApplicationServices

let pid = pid_t(CommandLine.arguments[1])!
let action = CommandLine.arguments[2]
let app = AXUIElementCreateApplication(pid)
func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    AXUIElementCopyAttributeValue(element, name as CFString, &value)
    return value
}
func windows() -> [[String: Any]] {
    let all = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    return all.filter { ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) && ($0[kCGWindowLayer as String] as? Int) == 0 }
}
func walk(_ element: AXUIElement, depth: Int = 0) -> [String: Any] {
    var result: [String: Any] = [:]
    for name in ["AXRole", "AXTitle", "AXDescription", "AXValue"] {
        if let value = attribute(element, name) { result[name] = String(describing: value) }
    }
    if depth < 15, let children = attribute(element, "AXChildren") as? [AXUIElement] {
        result["children"] = children.map { walk($0, depth: depth + 1) }
    }
    return result
}
func find(_ element: AXUIElement, title: String, depth: Int = 0) -> AXUIElement? {
    if ["AXTitle", "AXDescription", "AXValue"].contains(where: { (attribute(element, $0) as? String) == title }) { return element }
    if depth < 15, let children = attribute(element, "AXChildren") as? [AXUIElement] {
        for child in children { if let found = find(child, title: title, depth: depth + 1) { return found } }
    }
    return nil
}
func key(_ code: CGKeyCode, flags: CGEventFlags = []) {
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)!
        event.flags = down ? flags : []
        event.post(tap: .cghidEventTap)
    }
    usleep(100_000)
}
func click(_ point: CGPoint, twice: Bool = false) {
    CGWarpMouseCursorPosition(point)
    let move = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)!
    move.flags = []
    move.post(tap: .cghidEventTap)
    for count in 1...(twice ? 2 : 1) {
        for type in [CGEventType.leftMouseDown, .leftMouseUp] {
            let event = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: .left)!
            event.flags = []
            event.setIntegerValueField(.mouseEventClickState, value: Int64(count))
            event.post(tap: .cghidEventTap)
            usleep(40_000)
        }
    }
}
switch action {
case "windows":
    let data = try JSONSerialization.data(withJSONObject: windows(), options: [.prettyPrinted, .sortedKeys])
    print(String(data: data, encoding: .utf8)!)
case "tree":
    let data = try JSONSerialization.data(withJSONObject: walk(app), options: [.prettyPrinted, .sortedKeys])
    print(String(data: data, encoding: .utf8)!)
case "press":
    guard let element = find(app, title: CommandLine.arguments[3]) else { fputs("AX control not found\n", stderr); exit(2) }
    let result = AXUIElementPerformAction(element, "AXPress" as CFString)
    if result != .success { fputs("AXPress failed: \(result.rawValue)\n", stderr); exit(3) }
case "activate":
    NSRunningApplication(processIdentifier: pid)?.activate(options: [])
case "click":
    guard let window = windows().first,
          let bounds = window[kCGWindowBounds as String] as? [String: CGFloat] else { exit(4) }
    let point = CGPoint(x: bounds["X"]! + Double(CommandLine.arguments[3])!, y: bounds["Y"]! + Double(CommandLine.arguments[4])!)
    click(point)
case "click-text", "double-click-text", "hover-text":
    guard let element = find(app, title: CommandLine.arguments[3]),
          let position = attribute(element, "AXPosition"), let size = attribute(element, "AXSize") else { exit(5) }
    var point = CGPoint.zero
    var dimensions = CGSize.zero
    guard AXValueGetValue(position as! AXValue, .cgPoint, &point), AXValueGetValue(size as! AXValue, .cgSize, &dimensions) else { exit(6) }
    let center = CGPoint(x: point.x + dimensions.width / 2, y: point.y + dimensions.height / 2)
    if action == "hover-text" {
        CGWarpMouseCursorPosition(center)
        let move = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: center, mouseButton: .left)!
        move.flags = []
        move.post(tap: .cghidEventTap)
        usleep(100_000)
    } else {
        click(center, twice: action == "double-click-text")
    }
case "key":
    key(CGKeyCode(CommandLine.arguments[3])!, flags: CommandLine.arguments.count > 4 ? .maskCommand : [])
case "paste":
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(CommandLine.arguments[3], forType: .string)
    key(9, flags: .maskCommand)
case "scroll":
    let event = CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: Int32(CommandLine.arguments[3])!, wheel2: 0, wheel3: 0)!
    event.location = CGEvent(source: nil)!.location
    event.flags = []
    event.post(tap: .cghidEventTap)
    usleep(150_000)
case "close":
    guard let window = (attribute(app, "AXWindows") as? [AXUIElement])?.first,
          let button = attribute(window, "AXCloseButton") else { exit(7) }
    let result = AXUIElementPerformAction(button as! AXUIElement, "AXPress" as CFString)
    if result != .success { exit(8) }
default: exit(1)
}
