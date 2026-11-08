// Raycast-style popup menu for the gallery's "+" button. A borderless
// NSPanel with a SwiftUI list over an NSVisualEffectView, anchored under the
// button — native chrome (blur, shadow, key handling) without the stock
// NSMenu look. Rust calls lookout_add_menu_show with the items as JSON and
// the button rect in window coordinates (logical points, top-left origin);
// the selected item id comes back through a C callback, nil on dismissal.

import AppKit
import SwiftUI

public typealias AddMenuCallback = @convention(c) (UnsafePointer<CChar>?) -> Void

/// Transparent padding around the menu inside its panel, so the spring's
/// overshoot (scale > 1) and the SwiftUI-drawn drop shadow have room to draw
/// instead of clipping at the window edge. The positioning math subtracts it
/// back out.
private let addMenuOvershootMargin: CGFloat = 28

struct AddMenuEntry: Decodable {
    var id: String?
    var label: String?
    var symbol: String?
    var iconUrl: String?
    var separator: Bool?
    var isSeparator: Bool { separator == true }
}

/// In-memory icon store, warmed via lookout_add_menu_prefetch_icons when the
/// frontend loads the program registry — AsyncImage alone re-fetches on every
/// open, which showed the fallback symbol for ~0.5s each time. Main-thread
/// access only.
final class AddMenuIconCache {
    static let shared = AddMenuIconCache()
    private var images: [String: NSImage] = [:]
    private var inflight: Set<String> = []

    func image(for url: String) -> NSImage? { images[url] }

    func prefetch(_ urls: [String]) {
        for u in urls where images[u] == nil && !inflight.contains(u) {
            guard let url = URL(string: u) else { continue }
            inflight.insert(u)
            URLSession.shared.dataTask(with: url) { data, _, _ in
                DispatchQueue.main.async {
                    self.inflight.remove(u)
                    if let data, let img = NSImage(data: data) {
                        self.images[u] = img
                    }
                }
            }.resume()
        }
    }
}

@available(macOS 12.0, *)
final class AddMenuModel: ObservableObject {
    let entries: [AddMenuEntry]
    @Published var selection: Int?
    /// Drives the fling-in scale. Set by the controller once the panel is on
    /// screen — SwiftUI's own onAppear fires a commit later than orderFront,
    /// which read as a frozen frame before the spring started.
    @Published var appeared = false
    let onActivate: (String?) -> Void

    init(entries: [AddMenuEntry], onActivate: @escaping (String?) -> Void) {
        self.entries = entries
        self.onActivate = onActivate
    }

    private var selectable: [Int] {
        entries.indices.filter { !entries[$0].isSeparator }
    }

    func moveSelection(_ delta: Int) {
        let indices = selectable
        guard !indices.isEmpty else { return }
        guard let current = selection, let pos = indices.firstIndex(of: current) else {
            selection = delta > 0 ? indices.first : indices.last
            return
        }
        let next = (pos + delta + indices.count) % indices.count
        selection = indices[next]
    }

    func activateSelection() {
        guard let i = selection, !entries[i].isSeparator else { return }
        onActivate(entries[i].id)
    }
}

@available(macOS 12.0, *)
private struct MenuEffectBackground: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .menu
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {}
}

@available(macOS 12.0, *)
private struct AddMenuRow: View {
    let entry: AddMenuEntry
    let highlighted: Bool

    var body: some View {
        HStack(spacing: 9) {
            icon
            Text(entry.label ?? "")
                .font(.system(size: 13.5, weight: .medium))
                .foregroundColor(.primary)
                .lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(highlighted ? Color.primary.opacity(0.09) : Color.clear)
        )
        .contentShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
    }

    /// Program logo when the entry carries one; SF Symbol as the fallback
    /// (and as the placeholder while the image loads or if it fails).
    @ViewBuilder
    private var icon: some View {
        if let iconUrl = entry.iconUrl, let cached = AddMenuIconCache.shared.image(for: iconUrl) {
            Image(nsImage: cached)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
                .frame(width: 18, height: 18)
        } else if let iconUrl = entry.iconUrl, let url = URL(string: iconUrl) {
            // Cold miss (first open before the prefetch landed) — load in
            // place, symbol as the placeholder.
            AsyncImage(url: url) { phase in
                if let image = phase.image {
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
                } else {
                    symbolIcon
                }
            }
            .frame(width: 18, height: 18)
        } else {
            symbolIcon
                .frame(width: 18)
        }
    }

    @ViewBuilder
    private var symbolIcon: some View {
        if let symbol = entry.symbol {
            Image(systemName: symbol)
                .font(.system(size: 13, weight: .medium))
                .foregroundColor(highlighted ? .primary : .secondary)
        }
    }
}

@available(macOS 12.0, *)
struct AddMenuView: View {
    @ObservedObject var model: AddMenuModel

    var body: some View {
        container(
            VStack(alignment: .leading, spacing: 1) {
                ForEach(Array(model.entries.enumerated()), id: \.offset) { i, entry in
                    if entry.isSeparator {
                        Divider()
                            .padding(.vertical, 4)
                            .padding(.horizontal, 10)
                    } else {
                        AddMenuRow(entry: entry, highlighted: model.selection == i)
                            .onHover { inside in
                                if inside {
                                    model.selection = i
                                } else if model.selection == i {
                                    model.selection = nil
                                }
                            }
                            .onTapGesture { model.onActivate(entry.id) }
                    }
                }
            }
            .padding(6)
            .frame(minWidth: 220, maxWidth: 320, alignment: .leading)
        )
        // Drawn here rather than by the window (hasShadow) — AppKit snapshots
        // the window shadow once, mid-fling, leaving a stale outline at the
        // wrong scale. This one tracks the animation.
        .shadow(color: Color.black.opacity(0.28), radius: 16, x: 0, y: 6)
        .scaleEffect(model.appeared ? 1 : 0.85, anchor: .topTrailing)
        .padding(addMenuOvershootMargin)
    }

    private func container<V: View>(_ content: V) -> some View {
        content
            .background(MenuEffectBackground())
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.12), lineWidth: 1)
            )
    }
}

/// Borderless windows refuse key status by default; the menu needs it for
/// Escape/arrow keys and to learn when the user clicks away (resignKey).
@available(macOS 12.0, *)
private final class AddMenuPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}

@available(macOS 12.0, *)
final class AddMenuController: NSObject, NSWindowDelegate {
    static let shared = AddMenuController()

    private var panel: NSPanel?
    private var callback: AddMenuCallback?
    private var keyMonitor: Any?
    private var finished = true

    func show(entries: [AddMenuEntry], parent: NSWindow, anchor: NSRect, cb: @escaping AddMenuCallback) {
        // A stale panel here means Rust already abandoned its callback — just
        // tear the old one down without firing anything.
        closePanel()
        finished = false
        callback = cb

        let model = AddMenuModel(entries: entries) { [weak self] id in
            self?.finish(id)
        }
        let hosting = NSHostingView(rootView: AddMenuView(model: model))
        // fittingSize includes the transparent overshoot margin on all sides;
        // the menu's own width is clamped by the SwiftUI frame modifier.
        let size = hosting.fittingSize

        let panel = AddMenuPanel(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false // shadow is drawn in SwiftUI; see AddMenuView
        panel.level = .popUpMenu
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.transient, .ignoresCycle]
        panel.contentView = hosting
        panel.delegate = self
        panel.setFrame(frame(for: size, parent: parent, anchor: anchor), display: false)
        self.panel = panel

        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, let panel = self.panel, event.window === panel else { return event }
            switch event.keyCode {
            case 53: // escape
                self.finish(nil)
                return nil
            case 125: // down
                model.moveSelection(1)
                return nil
            case 126: // up
                model.moveSelection(-1)
                return nil
            case 36, 76: // return, keypad enter
                model.activateSelection()
                return nil
            default:
                return event
            }
        }

        panel.alphaValue = 0
        panel.makeKeyAndOrderFront(nil)
        // Kick fade and fling on the next tick, after the first frame (at
        // 0.85 scale, alpha 0) has committed — starting them together is what
        // makes the pop read as one motion.
        DispatchQueue.main.async { [weak self] in
            guard let self, self.panel === panel else { return }
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.15
                panel.animator().alphaValue = 1
            }
            // Damping scales with √stiffness to keep the same slight overshoot.
            withAnimation(.interpolatingSpring(stiffness: 1200, damping: 46, initialVelocity: 0)) {
                model.appeared = true
            }
        }
    }

    /// Screen frame for the menu: right edge aligned with the button, dropped
    /// just below it, clamped to the visible screen (flips above the button
    /// when there's no room underneath). The anchor rect is in window
    /// coordinates with a top-left origin; the webview spans the whole window
    /// (Overlay titlebar), so window-relative math is enough.
    private func frame(for size: NSSize, parent: NSWindow, anchor: NSRect) -> NSRect {
        let margin = addMenuOvershootMargin
        // The visible menu box, excluding the transparent overshoot margin.
        let menu = NSSize(width: size.width - 2 * margin, height: size.height - 2 * margin)
        let wf = parent.frame
        let gap: CGFloat = 6
        var x = wf.origin.x + anchor.origin.x + anchor.width - menu.width
        let anchorBottomY = wf.maxY - (anchor.origin.y + anchor.height)
        var y = anchorBottomY - gap - menu.height

        if let visible = (parent.screen ?? NSScreen.main)?.visibleFrame {
            x = max(visible.minX + 8, min(x, visible.maxX - menu.width - 8))
            if y < visible.minY + 8 {
                let anchorTopY = wf.maxY - anchor.origin.y
                y = anchorTopY + gap
            }
        }
        // Expand back out so the window carries the margin on every side.
        return NSRect(x: x - margin, y: y - margin, width: size.width, height: size.height)
    }

    func windowDidResignKey(_ notification: Notification) {
        finish(nil)
    }

    private func finish(_ id: String?) {
        guard !finished else { return }
        finished = true
        let cb = callback
        callback = nil
        if let id {
            id.withCString { cb?($0) }
        } else {
            cb?(nil)
        }
        closePanel()
    }

    private func closePanel() {
        if let monitor = keyMonitor {
            NSEvent.removeMonitor(monitor)
            keyMonitor = nil
        }
        guard let panel = panel else { return }
        self.panel = nil
        panel.delegate = nil
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.17
            panel.animator().alphaValue = 0
        }, completionHandler: {
            panel.orderOut(nil)
        })
    }
}

@_cdecl("lookout_add_menu_prefetch_icons")
public func lookoutAddMenuPrefetchIcons(_ urlsJson: UnsafePointer<CChar>) {
    let json = String(cString: urlsJson)
    DispatchQueue.main.async {
        guard let data = json.data(using: .utf8),
              let urls = try? JSONDecoder().decode([String].self, from: data)
        else { return }
        AddMenuIconCache.shared.prefetch(urls)
    }
}

@_cdecl("lookout_add_menu_show")
public func lookoutAddMenuShow(
    _ itemsJson: UnsafePointer<CChar>,
    _ windowPtr: UnsafeMutableRawPointer,
    _ x: Double,
    _ y: Double,
    _ w: Double,
    _ h: Double,
    _ cb: @escaping AddMenuCallback
) {
    let json = String(cString: itemsJson)
    DispatchQueue.main.async {
        guard #available(macOS 12.0, *),
              let data = json.data(using: .utf8),
              let entries = try? JSONDecoder().decode([AddMenuEntry].self, from: data),
              !entries.isEmpty
        else {
            cb(nil)
            return
        }
        let window = Unmanaged<NSWindow>.fromOpaque(windowPtr).takeUnretainedValue()
        AddMenuController.shared.show(
            entries: entries,
            parent: window,
            anchor: NSRect(x: x, y: y, width: w, height: h),
            cb: cb
        )
    }
}
