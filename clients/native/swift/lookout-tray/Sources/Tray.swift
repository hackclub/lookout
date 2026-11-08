// Native macOS menu-bar item for Lookout. Owns an NSStatusItem whose content
// is a SwiftUI view, so the recorded-time digits animate with the real
// `contentTransition(.numericText())` (the system timer's rolling-digit
// effect). Rust talks to this through the @_cdecl functions at the bottom;
// clicks come back through a C callback carrying the item's screen rect
// (logical points, top-left origin) so Rust can position the tray window.

import AppKit
import SwiftUI

public typealias TrayClickCallback = @convention(c) (Double, Double, Double, Double) -> Void

@available(macOS 10.15, *)
final class TrayModel: ObservableObject {
    static let shared = TrayModel()
    @Published var text: String = "0m"
    @Published var paused: Bool = false
    var icon: NSImage?
}

/// Reports the content's natural width up to TrayController, which sets the
/// NSStatusItem length — the status-bar button doesn't size itself from
/// subview constraints, so without this the text truncates to "…".
@available(macOS 10.15, *)
struct TrayWidthKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

@available(macOS 10.15, *)
struct TrayContentView: View {
    @ObservedObject var model = TrayModel.shared

    var body: some View {
        HStack(spacing: 4) {
            if let icon = model.icon {
                Image(nsImage: icon)
                    .renderingMode(.template)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 17, height: 17)
            }
            if model.paused, #available(macOS 11.0, *) {
                Image(systemName: "pause.fill")
                    .font(.system(size: 9))
            }
            timeText
        }
        .padding(.horizontal, 3)
        .fixedSize()
        .background(
            GeometryReader { geo in
                Color.clear.preference(key: TrayWidthKey.self, value: geo.size.width)
            }
        )
        .onPreferenceChange(TrayWidthKey.self) { width in
            TrayController.shared.setLength(width)
        }
    }

    @ViewBuilder
    private var timeText: some View {
        if #available(macOS 13.0, *) {
            Text(model.text)
                .font(.system(size: 13.5).monospacedDigit())
                .contentTransition(.numericText())
        } else {
            Text(model.text)
                .font(.system(size: 13.5))
        }
    }
}

/// NSHostingView swallows mouse events, which would break the status-bar
/// button's target/action (and its click highlight). Punching through hitTest
/// lets the button own the interaction while SwiftUI only draws.
@available(macOS 10.15, *)
final class PassthroughHostingView<Content: View>: NSHostingView<Content> {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

@available(macOS 10.15, *)
final class TrayController: NSObject {
    static let shared = TrayController()
    var item: NSStatusItem?
    var callback: TrayClickCallback?

    func show() {
        guard item == nil else { return }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        self.item = item
        guard let button = item.button else { return }

        let hosting = PassthroughHostingView(rootView: TrayContentView())
        hosting.translatesAutoresizingMaskIntoConstraints = false
        button.addSubview(hosting)
        NSLayoutConstraint.activate([
            hosting.topAnchor.constraint(equalTo: button.topAnchor),
            hosting.bottomAnchor.constraint(equalTo: button.bottomAnchor),
            hosting.leadingAnchor.constraint(equalTo: button.leadingAnchor),
            hosting.trailingAnchor.constraint(equalTo: button.trailingAnchor),
        ])

        button.target = self
        button.action = #selector(clicked(_:))
    }

    /// Called by the SwiftUI view whenever its natural width changes.
    func setLength(_ width: CGFloat) {
        guard width > 0 else { return }
        item?.length = width
    }

    @objc func clicked(_ sender: Any?) {
        guard let button = item?.button, let window = button.window else { return }
        let frame = window.frame
        // AppKit rects are bottom-left origin; the Rust side wants top-left.
        let screenH = NSScreen.screens.first?.frame.height ?? 0
        callback?(
            frame.origin.x,
            screenH - frame.origin.y - frame.height,
            frame.width,
            frame.height
        )
    }

    func hide() {
        if let item = item {
            NSStatusBar.system.removeStatusItem(item)
        }
        item = nil
    }
}

@_cdecl("lookout_tray_set_callback")
public func lookoutTraySetCallback(_ cb: @escaping TrayClickCallback) {
    DispatchQueue.main.async {
        guard #available(macOS 10.15, *) else { return }
        TrayController.shared.callback = cb
    }
}

@_cdecl("lookout_tray_show")
public func lookoutTrayShow(_ text: UnsafePointer<CChar>, _ iconBytes: UnsafePointer<UInt8>?, _ iconLen: Int32) {
    let s = String(cString: text)
    let iconData = iconBytes.map { Data(bytes: $0, count: Int(iconLen)) }
    DispatchQueue.main.async {
        guard #available(macOS 10.15, *) else { return }
        if TrayModel.shared.icon == nil, let data = iconData, let img = NSImage(data: data) {
            img.isTemplate = true
            TrayModel.shared.icon = img
        }
        TrayModel.shared.text = s
        TrayModel.shared.paused = false
        TrayController.shared.show()
        TrayController.shared.item?.button?.toolTip = "Lookout — \(s) recorded"
    }
}

/// paused: -1 keeps the current pause state (the 1s ticker), 0/1 set it.
@_cdecl("lookout_tray_update")
public func lookoutTrayUpdate(_ text: UnsafePointer<CChar>, _ paused: Int32) {
    let s = String(cString: text)
    DispatchQueue.main.async {
        guard #available(macOS 10.15, *) else { return }
        let p = paused < 0 ? TrayModel.shared.paused : (paused != 0)
        if #available(macOS 13.0, *) {
            withAnimation(.spring(response: 0.4, dampingFraction: 0.9)) {
                TrayModel.shared.text = s
                TrayModel.shared.paused = p
            }
        } else {
            TrayModel.shared.text = s
            TrayModel.shared.paused = p
        }
        TrayController.shared.item?.button?.toolTip =
            p ? "Lookout — paused at \(s)" : "Lookout — \(s) recorded"
    }
}

@_cdecl("lookout_tray_hide")
public func lookoutTrayHide() {
    DispatchQueue.main.async {
        guard #available(macOS 10.15, *) else { return }
        TrayController.shared.hide()
    }
}
