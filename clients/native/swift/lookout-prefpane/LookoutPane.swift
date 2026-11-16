// Lookout's pane in System Settings (a classic NSPreferencePane bundle).
//
// Lookout copies it to ~/Library/PreferencePanes at launch
// (crates/lookout-native/src/platform/pref_pane.rs); packaging/macos/
// prefpane.sh builds it. It is Lookout's own Settings screen, drawn with
// Lookout's tokens, controls and motion (Kit.swift, Controls.swift,
// Pages.swift), and it edits settings for real: every change goes through
// `lookout --settings-pane <json>` (src/settings_pane.rs), which hands it to
// the running app, or applies it to the files when Lookout is closed.

import AppKit
import PreferencePanes

@objc(LookoutPane)
final class LookoutPane: NSPreferencePane {
    private var root: PaneRoot?

    override func loadMainView() -> NSView {
        Icon.bundle = bundle
        let r = PaneRoot(frame: NSRect(x: 0, y: 0, width: PaneRoot.width, height: PaneRoot.height), paneBundle: bundle)
        root = r
        mainView = r
        mainViewDidLoad()
        return r
    }

    override func willSelect() { root?.refresh() }
    override func didSelect() { root?.appeared() }
    override func didUnselect() { root?.disappeared() }
}

// MARK: - Model

struct LinkInfo {
    let program: String
    let label: String
    let detail: String
    let iconURL: URL?
}

struct PaneState {
    var running = false
    var version = ""
    var filtered: [String] = []
    var links: [LinkInfo] = []
    var apiBase = AdvancedPage.defaultServer
    var isDefault = true
    var beta = false
    var betaSupported = true

    init() {}

    init(_ d: [String: Any]) {
        running = d["running"] as? Bool ?? false
        version = d["version"] as? String ?? ""
        filtered = d["filtered"] as? [String] ?? []
        links = (d["links"] as? [[String: Any]] ?? []).map {
            LinkInfo(program: $0["program"] as? String ?? "", label: $0["label"] as? String ?? "",
                     detail: $0["detail"] as? String ?? "", iconURL: ($0["iconUrl"] as? String).flatMap(URL.init(string:)))
        }
        apiBase = d["apiBase"] as? String ?? AdvancedPage.defaultServer
        isDefault = d["isDefault"] as? Bool ?? true
        beta = d["beta"] as? Bool ?? false
        betaSupported = d["betaSupported"] as? Bool ?? true
    }
}

struct AppEntry {
    let name: String
    let path: String?
    let running: Bool
}

/// Runs `lookout --settings-pane <json>` and parses its one-line reply.
/// Writes go one at a time, in order, so the last change wins.
final class Bridge {
    let exe: URL
    private let writes = DispatchQueue(label: "com.hackclub.lookout.prefpane.writes")
    private let reads = DispatchQueue(label: "com.hackclub.lookout.prefpane.reads", attributes: .concurrent)

    init(exe: URL) { self.exe = exe }

    func call(_ req: [String: Any], write: Bool = false, _ done: @escaping ([String: Any]) -> Void) {
        let exe = self.exe
        (write ? writes : reads).async {
            let reply = Bridge.run(exe, req)
            DispatchQueue.main.async { done(reply) }
        }
    }

    private static func run(_ exe: URL, _ req: [String: Any]) -> [String: Any] {
        guard let data = try? JSONSerialization.data(withJSONObject: req),
              let arg = String(data: data, encoding: .utf8) else {
            return ["ok": false, "error": "Couldn't encode the request."]
        }
        let p = Process()
        p.executableURL = exe
        p.arguments = ["--settings-pane", arg]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        p.standardInput = FileHandle.nullDevice
        do {
            try p.run()
        } catch {
            return ["ok": false, "error": "Couldn't run Lookout: \(error.localizedDescription)"]
        }
        let bytes = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        let text = String(decoding: bytes, as: UTF8.self)
        for line in text.split(separator: "\n").reversed() {
            if let obj = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any] {
                return obj
            }
        }
        return ["ok": false, "error": "Lookout didn't answer."]
    }
}

// MARK: - Root

final class PaneRoot: FlippedView, Animating {
    /// System Settings widens its window to fit a pane wider than its own
    /// detail column (the classic 668pt pane did), so ask for that column's
    /// width. Fixed, not stretchy: a stretchy pane keeps whatever width the
    /// window last had.
    static let width: CGFloat = 500
    static let height: CGFloat = 560
    static let appID = "com.hackclub.lookout"

    private let paneBundle: Bundle
    private(set) var state = PaneState()
    private(set) var apps: [AppEntry]?
    /// The filtered list as the user sees it, ahead of the saved one while writes run.
    private(set) var filtered: [String] = []
    private(set) var unlinking: String?
    private var pendingWrites = 0
    var writing: Bool { pendingWrites > 0 }

    private var bridge: Bridge?
    private var loadingApps = false
    private var watcher: DispatchSourceFileSystemObject?
    private var refreshWork: DispatchWorkItem?
    private var keyObserver: NSObjectProtocol?
    private var iconCache: [String: NSImage] = [:]
    private var remoteCache: [URL: NSImage] = [:]
    private let iconQueue = DispatchQueue(label: "com.hackclub.lookout.prefpane.icons")

    // Router: Settings sub-pages slide like routes (spring 460 / 36 / 0.7,
    // ±14px; enter fades in over 160ms after 40ms, exit out over 140ms).
    private struct Layer {
        let sub: Sub
        let scroll: NSScrollView
        let page: Page
        let scrollObserver: NSObjectProtocol
        var x: Anim
        var opacity: Anim
        var leaving: Bool
    }
    private var layers: [Layer] = []
    private var current = Sub.menu
    private var missing: MissingPage?

    init(frame: NSRect, paneBundle: Bundle) {
        self.paneBundle = paneBundle
        super.init(frame: frame)
        wantsLayer = true
        resolveApp()
        if missing == nil {
            push(.menu, animated: false)
            // CI screenshots (packaging/macos/prefpane-check.swift) open a page directly.
            switch ProcessInfo.processInfo.environment["LOOKOUT_PANE_PAGE"] {
            case "filtered-apps": push(.filteredApps, animated: false)
            case "linked-programs": push(.linkedPrograms, animated: false)
            case "advanced": push(.advanced, animated: false)
            default: break
            }
        }
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: Lifecycle

    private var appURL: URL? {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: PaneRoot.appID)
    }

    /// Find Lookout; show the "isn't on this Mac" page when it's gone.
    private func resolveApp() {
        if let app = appURL, let exe = Bundle(url: app)?.executableURL {
            bridge = Bridge(exe: exe)
            if let m = missing {
                m.removeFromSuperview()
                missing = nil
                push(.menu, animated: false)
            }
        } else {
            bridge = nil
            if missing == nil {
                for l in layers { drop(l) }
                layers = []
                let m = MissingPage(root: self)
                addSubview(m)
                missing = m
                relayout()
            }
        }
    }

    func appeared() {
        window?.makeFirstResponder(self)
        if keyObserver == nil, let w = window {
            keyObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didBecomeKeyNotification, object: w, queue: .main
            ) { [weak self] _ in self?.refresh() }
        }
        watch()
        // Have the app list ready before Filtered Apps opens.
        loadApps()
    }

    func disappeared() {
        if let o = keyObserver {
            NotificationCenter.default.removeObserver(o)
            keyObserver = nil
        }
        watcher?.cancel()
        watcher = nil
    }

    /// Lookout's state changes underneath us (its own Settings, a sign-in…):
    /// watch its config directory while the pane is showing.
    private func watch() {
        guard watcher == nil else { return }
        let dir = PaneRoot.home.appendingPathComponent("Library/Application Support/\(PaneRoot.appID)")
        let fd = open(dir.path, O_EVTONLY)
        guard fd >= 0 else { return }
        let src = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .rename, .delete], queue: .main)
        src.setEventHandler { [weak self] in self?.scheduleRefresh() }
        src.setCancelHandler { close(fd) }
        src.resume()
        watcher = src
    }

    /// The real home directory, even if panes are ever hosted in a sandbox.
    static var home: URL {
        if let pw = getpwuid(getuid()), let dir = pw.pointee.pw_dir {
            return URL(fileURLWithPath: String(cString: dir), isDirectory: true)
        }
        return URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
    }

    private func scheduleRefresh() {
        refreshWork?.cancel()
        let w = DispatchWorkItem { [weak self] in self?.refresh() }
        refreshWork = w
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: w)
    }

    func refresh() {
        resolveApp()
        guard let bridge = bridge, !writing else { return }
        bridge.call(["op": "state"]) { [weak self] reply in
            guard let self = self, !self.writing else { return }
            self.apply(reply)
        }
    }

    /// Take a reply's state; returns its error, if any.
    @discardableResult
    private func apply(_ reply: [String: Any]) -> String? {
        if let s = reply["state"] as? [String: Any] {
            state = PaneState(s)
            if !writing { filtered = state.filtered }
            notifyPages()
        }
        return (reply["ok"] as? Bool) == true ? nil : (reply["error"] as? String ?? "Something went wrong.")
    }

    private func notifyPages() {
        for l in layers { l.page.stateChanged() }
        relayout()
    }

    // MARK: Actions

    func loadApps() {
        guard apps == nil, !loadingApps, let bridge = bridge else { return }
        loadingApps = true
        bridge.call(["op": "apps"]) { [weak self] reply in
            guard let self = self else { return }
            self.loadingApps = false
            self.apps = (reply["apps"] as? [[String: Any]] ?? []).compactMap {
                guard let name = $0["name"] as? String else { return nil }
                return AppEntry(name: name, path: $0["path"] as? String, running: $0["running"] as? Bool ?? false)
            }
            self.notifyPages()
        }
    }

    /// A write: the UI already shows it; the reply's state lands once the last write is back.
    private func write(_ req: [String: Any], then: ((String?) -> Void)? = nil) {
        guard let bridge = bridge else { return }
        pendingWrites += 1
        bridge.call(req, write: true) { [weak self] reply in
            guard let self = self else { return }
            self.pendingWrites -= 1
            let ok = (reply["ok"] as? Bool) == true
            if self.pendingWrites == 0 || !ok {
                let err = self.apply(reply)
                if !ok, self.pendingWrites == 0 { self.refresh() }
                then?(err)
                if let err = err, then == nil { self.alert("Couldn't save", err) }
            } else {
                then?(nil)
            }
        }
    }

    func toggleFiltered(_ name: String) {
        if let i = filtered.firstIndex(of: name) {
            filtered.remove(at: i)
        } else {
            filtered.append(name)
            filtered.sort()
        }
        notifyPages()
        write(["op": "setFiltered", "apps": filtered])
    }

    func setBeta(_ on: Bool) {
        write(["op": "setBeta", "on": on])
    }

    func setServer(_ url: String, done: @escaping (String?) -> Void) {
        write(["op": "setServer", "url": url], then: done)
    }

    func unlink(_ program: String) {
        unlinking = program
        notifyPages()
        write(["op": "unlink", "program": program]) { [weak self] _ in
            guard let self = self else { return }
            self.unlinking = nil
            self.notifyPages()
            if self.state.links.isEmpty, self.current == .linkedPrograms { self.go(.menu) }
        }
    }

    func openLink(_ link: String) {
        guard let url = URL(string: link) else { return }
        if let app = appURL {
            // This Lookout, not whichever app last claimed the scheme.
            NSWorkspace.shared.open([url], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration(), completionHandler: nil)
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    func openLookout() {
        guard let app = appURL else { return }
        NSWorkspace.shared.openApplication(at: app, configuration: NSWorkspace.OpenConfiguration(), completionHandler: nil)
    }

    func openWebsite() {
        if let url = URL(string: "https://lookout.hackclub.com") { NSWorkspace.shared.open(url) }
    }

    /// Move this pane to the Trash; System Settings drops it next time it opens.
    func removePane() -> Bool {
        (try? FileManager.default.trashItem(at: paneBundle.bundleURL, resultingItemURL: nil)) != nil
    }

    func cachedIcon(_ path: String) -> NSImage? { iconCache[path] }

    func appIcon(_ path: String, _ done: @escaping (NSImage) -> Void) {
        if let i = iconCache[path] { return done(i) }
        iconQueue.async {
            // Workspace icons carry every size up to 1024px and decode lazily,
            // on the main thread, the first time they're drawn: render the one
            // size the rows use here instead.
            let img = PaneRoot.thumbnail(NSWorkspace.shared.icon(forFile: path), side: 20)
            DispatchQueue.main.async { [weak self] in
                self?.iconCache[path] = img
                done(img)
            }
        }
    }

    /// `image` as a single bitmap of `side` points at 2x.
    static func thumbnail(_ image: NSImage, side: CGFloat) -> NSImage {
        let px = Int(side * 2)
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                                         samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                         colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
              let ctx = NSGraphicsContext(bitmapImageRep: rep) else { return image }
        // The context is laid out in the rep's pixels, so draw at pixel size
        // and only then say it's `side` points (2x).
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = ctx
        ctx.imageInterpolation = .high
        image.draw(in: NSRect(x: 0, y: 0, width: CGFloat(px), height: CGFloat(px)), from: .zero, operation: .copy, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
        rep.size = NSSize(width: side, height: side)
        let out = NSImage(size: rep.size)
        out.addRepresentation(rep)
        return out
    }

    /// Lookout's icon for the header: the pane's own (the app's icon.icns),
    /// else the installed app's.
    private(set) lazy var lookoutIcon: NSImage? = {
        if let i = self.paneBundle.image(forResource: "icon") { return i }
        if let app = self.appURL { return NSWorkspace.shared.icon(forFile: app.path) }
        return nil
    }()

    /// The app's version; before Lookout answers, the pane's (built with it).
    var version: String {
        if !state.version.isEmpty { return state.version }
        return paneBundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
    }

    func remoteImage(_ url: URL, _ done: @escaping (NSImage) -> Void) {
        if let i = remoteCache[url] { return done(i) }
        URLSession.shared.dataTask(with: url) { data, _, _ in
            guard let data = data, let img = NSImage(data: data) else { return }
            DispatchQueue.main.async { [weak self] in
                self?.remoteCache[url] = img
                done(img)
            }
        }.resume()
    }

    /// Lookout's confirms are native alerts on macOS (`native_dialog`, warning level).
    func confirm(title: String, message: String, ok: String, then: @escaping () -> Void) {
        let a = NSAlert()
        a.alertStyle = .warning
        a.messageText = title
        a.informativeText = message
        a.addButton(withTitle: ok)
        a.addButton(withTitle: "Cancel")
        if let w = window {
            a.beginSheetModal(for: w) { if $0 == .alertFirstButtonReturn { then() } }
        } else if a.runModal() == .alertFirstButtonReturn {
            then()
        }
    }

    func alert(_ title: String, _ message: String) {
        let a = NSAlert()
        a.alertStyle = .warning
        a.messageText = title
        a.informativeText = message
        if let w = window { a.beginSheetModal(for: w, completionHandler: nil) } else { a.runModal() }
    }

    // MARK: Routing

    func go(_ sub: Sub) {
        guard sub != current, missing == nil else { return }
        push(sub, animated: true)
    }

    func back() {
        if current != .menu { go(.menu) }
    }

    private func makePage(_ sub: Sub) -> Page {
        switch sub {
        case .menu: return MenuPage(root: self)
        case .filteredApps: return FilteredAppsPage(root: self)
        case .linkedPrograms: return LinkedProgramsPage(root: self)
        case .advanced: return AdvancedPage(root: self)
        }
    }

    private func push(_ sub: Sub, animated: Bool) {
        let dir: Double = sub == .menu ? -1 : 1
        if !animated {
            for l in layers { drop(l) }
            layers = []
        }
        for i in layers.indices where !layers[i].leaving {
            layers[i].leaving = true
            layers[i].x.set(-14 * dir, .spring(.route))
            layers[i].opacity.set(0, .tween(140, .easeOut))
        }
        if let i = layers.firstIndex(where: { $0.sub == sub }) {
            // Still animating out: re-enter from where it is.
            var l = layers.remove(at: i)
            l.leaving = false
            l.x.set(0, .spring(.route))
            l.opacity.set(1, .tween(160, .easeOut), delay: 40)
            layers.append(l)
            addSubview(l.scroll)
        } else {
            let page = makePage(sub)
            let scroll = NSScrollView()
            scroll.drawsBackground = false
            scroll.hasVerticalScroller = true
            scroll.autohidesScrollers = true
            scroll.scrollerStyle = .overlay
            scroll.borderType = .noBorder
            scroll.documentView = page
            addSubview(scroll)
            scroll.contentView.postsBoundsChangedNotifications = true
            let observer = NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification, object: scroll.contentView, queue: .main
            ) { [weak page] _ in
                if let page = page { syncHover(in: page) }
            }
            let x = animated ? Anim.fromTo(14 * dir, 0, .spring(.route)) : Anim(0)
            let o = animated ? Anim.fromTo(0, 1, .tween(160, .easeOut), delay: 40) : Anim(1)
            layers.append(Layer(sub: sub, scroll: scroll, page: page, scrollObserver: observer, x: x, opacity: o, leaving: false))
        }
        current = sub
        relayout()
        layers.last?.page.entered()
        if animated { Animator.shared.run(self) }
        positionLayers()
    }

    func animationFrame() -> Bool {
        var moving = false
        for i in layers.indices {
            let a = layers[i].x.tick()
            let b = layers[i].opacity.tick()
            moving = moving || a || b
        }
        for l in layers where l.leaving && !l.opacity.isAnimating { drop(l) }
        layers.removeAll { $0.leaving && !$0.opacity.isAnimating }
        positionLayers()
        return moving
    }

    private func drop(_ l: Layer) {
        l.scroll.removeFromSuperview()
        NotificationCenter.default.removeObserver(l.scrollObserver)
    }

    private func positionLayers() {
        for l in layers {
            l.scroll.frame = CGRect(x: l.x.v, y: 0, width: bounds.width, height: bounds.height)
            l.scroll.alphaValue = l.opacity.v
        }
    }

    private var relayoutQueued = false

    /// Re-lay out the pages (coalesced to once per run-loop turn).
    func relayout() {
        guard !relayoutQueued else { return }
        relayoutQueued = true
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.relayoutQueued = false
            self.layoutPages()
        }
    }

    private func layoutPages() {
        let w = bounds.width, h = bounds.height
        if let m = missing {
            m.frame = CGRect(x: 0, y: 0, width: w, height: m.build(width: w, height: h))
            m.needsDisplay = true
            return
        }
        for l in layers {
            let contentH = l.page.build(width: w, height: h)
            l.page.frame = CGRect(x: 0, y: 0, width: w, height: max(contentH, h))
            l.page.needsDisplay = true
        }
        positionLayers()
    }

    override func layout() {
        super.layout()
        layoutPages()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        relayout()
    }

    // MARK: Keys

    override var acceptsFirstResponder: Bool { true }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { // Escape: back, like Lookout
            back()
        } else {
            super.keyDown(with: event)
        }
    }

    override func otherMouseDown(with event: NSEvent) {
        if event.buttonNumber == 3 { back() } else { super.otherMouseDown(with: event) }
    }
}
