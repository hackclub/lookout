// The Settings screens (crates/lookout-native/src/screens/settings.rs) as
// pane pages: the menu and its three sub-pages, with the same copy, layout
// and behaviour, plus the page shown once Lookout itself is gone.

import AppKit

enum Sub {
    case menu
    case filteredApps
    case linkedPrograms
    case advanced
}

enum Role {
    case primary, secondary, tertiary, danger, warning

    func color(_ t: Theme) -> NSColor {
        switch self {
        case .primary: return t.textPrimary
        case .secondary: return t.textSecondary
        case .tertiary: return t.textTertiary
        case .danger: return Theme.danger
        case .warning: return Theme.warning
        }
    }
}

struct TextItem {
    var text: String
    var frame: CGRect
    var font: NSFont
    var role: Role
    var lineHeight: CGFloat
    var wrap = false
    var align: NSTextAlignment = .left
}

/// A page: draws its text, positions its controls, reports its height.
class Page: FlippedView {
    unowned let root: PaneRoot
    var texts: [TextItem] = []
    /// Outlined boxes (the custom-server warning).
    var outlines: [(CGRect, Role)] = []
    /// Card surfaces drawn by the page itself, under its text.
    var surfaces: [CGRect] = []
    let pad: CGFloat = 20

    private(set) lazy var backButton: LKButton = {
        let b = LKButton("← Back", .secondary, card: true) { [unowned self] in self.root.back() }
        addSubview(b)
        return b
    }()

    init(root: PaneRoot) {
        self.root = root
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Lay out for `width` with `height` visible; return the content height.
    func build(width: CGFloat, height: CGFloat) -> CGFloat { height }

    /// The model changed.
    func stateChanged() {}

    /// The page became the current one.
    func entered() {}

    func text(_ s: String, _ rect: CGRect, _ font: NSFont, _ role: Role, lh: CGFloat, wrap: Bool = false,
              align: NSTextAlignment = .left) {
        texts.append(TextItem(text: s, frame: rect, font: font, role: role, lineHeight: lh, wrap: wrap, align: align))
    }

    /// `PageChrome`: Back, the title (20 bold) and the description (12 / 1.5).
    func chrome(_ title: String, _ description: String, back: Bool, width: CGFloat, trailing: LKButton? = nil) -> CGFloat {
        var y: CGFloat = 16
        if back {
            backButton.sizeToFit()
            backButton.setFrameOrigin(CGPoint(x: pad, y: y))
            y += LKButton.height + 16
        }
        let inner = width - 2 * pad
        if let b = trailing {
            b.sizeToFit()
            b.setFrameOrigin(CGPoint(x: width - pad - b.frame.width, y: y + (24 - LKButton.height) / 2))
        }
        text(title, CGRect(x: pad, y: y, width: inner - (trailing.map { $0.frame.width + 12 } ?? 0), height: 24),
             Txt.font(20, .bold), .primary, lh: 24)
        y += 24 + 4
        let dh = Txt.height(description, Txt.font(12), width: inner, lineHeight: 18)
        text(description, CGRect(x: pad, y: y, width: inner, height: dh), Txt.font(12), .secondary, lh: 18, wrap: true)
        return y + dh + 16
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        for rect in surfaces {
            paintBox(rect, radius: 10, fill: t.bgSurface, border: t.borderDefault)
        }
        for (rect, role) in outlines {
            paintBox(rect, radius: 8, fill: clearColor, border: role.color(t))
        }
        for item in texts {
            let color = item.role.color(t)
            if item.wrap {
                let a = NSAttributedString(string: item.text,
                                           attributes: Txt.attrs(item.font, color, lineHeight: item.lineHeight,
                                                                 truncate: false, align: item.align))
                a.draw(with: item.frame, options: [.usesLineFragmentOrigin])
            } else {
                Txt.draw(item.text, at: item.frame.origin, width: item.frame.width, font: item.font, color: color,
                         lineHeight: item.lineHeight)
            }
        }
    }
}

// MARK: - Menu

final class MenuPage: Page {
    private let card1 = Card()
    private let card2 = Card()
    private let filtered: MenuRow
    private let linked: MenuRow
    private let advanced: MenuRow
    private let beta: MenuRow
    private let onboarding: MenuRow
    private let about: MenuRow
    private let openButton: LKButton

    override init(root: PaneRoot) {
        filtered = MenuRow(.funnelFill, "Filtered Apps", "") { [unowned root] in root.go(.filteredApps) }
        linked = MenuRow(.linkBold, "Linked Programs", "") { [unowned root] in root.go(.linkedPrograms) }
        advanced = MenuRow(.wrenchFill, "Advanced", "") { [unowned root] in root.go(.advanced) }
        beta = MenuRow(.power, "Beta updates", "Get new versions early. They can be rougher.", .toggle)
        onboarding = MenuRow(.play, "Show onboarding", "") { [unowned root] in root.openLink("lookout://onboarding") }
        about = MenuRow(.infoFill, "About Lookout", "") { [unowned root] in root.openLink("lookout://settings/about") }
        openButton = LKButton("Open Lookout", .secondary, card: true) { [unowned root] in root.openLookout() }
        super.init(root: root)
        beta.onClick = { [unowned self] in
            self.beta.isOn.toggle()
            self.root.setBeta(self.beta.isOn)
        }
        for r in [filtered, linked, advanced, beta] { card1.addSubview(r) }
        for r in [onboarding, about] { card2.addSubview(r) }
        for v in [card1, card2, openButton] { addSubview(v) }
        beta.setOnImmediately(root.state.beta)
        stateChanged()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func stateChanged() {
        let s = root.state
        let n = s.filtered.count
        filtered.detail = n > 0 ? "\(n) app\(n == 1 ? "" : "s") filtered" : "Black out selected apps in captures"
        let l = s.links.count
        linked.detail = "\(l) program\(l == 1 ? "" : "s") linked"
        advanced.detail = s.isDefault
            ? "Developer options"
            : "Custom server: " + s.apiBase.replacingOccurrences(of: "https://", with: "").replacingOccurrences(of: "http://", with: "")
        if !root.writing { beta.isOn = s.beta }
        about.detail = s.version.isEmpty ? "" : "Version \(s.version)"
        needsLayout = true
        root.relayout()
    }

    override func build(width: CGFloat, height: CGFloat) -> CGFloat {
        texts = []
        let s = root.state
        var y = chrome("Settings", "Configure Lookout.", back: false, width: width, trailing: openButton)
        let inner = width - 2 * pad
        func fill(_ card: Card, _ rows: [MenuRow]) -> CGFloat {
            var ry: CGFloat = 4
            for r in [filtered, linked, advanced, beta, onboarding, about] where r.superview === card {
                r.isHidden = !rows.contains(r)
            }
            for r in rows {
                r.frame = CGRect(x: 4, y: ry, width: inner - 8, height: MenuRow.height)
                ry += MenuRow.height
            }
            return ry + 4
        }
        var rows1 = [filtered]
        if !s.links.isEmpty { rows1.append(linked) }
        rows1.append(advanced)
        if s.betaSupported { rows1.append(beta) }
        let h1 = fill(card1, rows1)
        card1.frame = CGRect(x: pad, y: y, width: inner, height: h1)
        y += h1 + 12
        let h2 = fill(card2, [onboarding, about])
        card2.frame = CGRect(x: pad, y: y, width: inner, height: h2)
        y += h2 + 16
        needsDisplay = true
        return y
    }
}

// MARK: - Filtered Apps

final class FilteredAppsPage: Page {
    private let search = LKField(placeholder: "Search apps...")
    private let card = Card()
    private let scroll = NSScrollView()
    private let list = FlippedView()
    private let skeleton = SkeletonRows()
    private let message = MessageView()
    private var rows: [String: AppRow] = [:]
    private var order: [String] = []
    /// What was filtered when the page opened: those stay on top.
    private let pinned: Set<String>
    private var query = ""

    override init(root: PaneRoot) {
        pinned = Set(root.state.filtered)
        super.init(root: root)
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.borderType = .noBorder
        scroll.documentView = list
        card.addSubview(scroll)
        card.addSubview(message)
        for v in [search, card] { addSubview(v) }
        search.onChange = { [unowned self] q in
            self.query = q.lowercased()
            self.rebuildRows()
            self.root.relayout()
        }
        root.loadApps()
        rebuildRows()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func stateChanged() {
        rebuildRows()
        root.relayout()
    }

    private func rebuildRows() {
        guard let apps = root.apps else {
            if skeleton.superview == nil { list.addSubview(skeleton) }
            message.isHidden = true
            return
        }
        skeleton.removeFromSuperview()
        let filtered = Set(root.filtered)
        var all = apps
        for name in filtered where !all.contains(where: { $0.name == name }) {
            all.append(AppEntry(name: name, path: nil, running: false))
        }
        let shown = all.filter { query.isEmpty || $0.name.lowercased().contains(query) }.sorted { a, b in
            func group(_ e: AppEntry) -> Int { pinned.contains(e.name) ? 0 : (e.running ? 1 : 2) }
            let ga = group(a), gb = group(b)
            return ga != gb ? ga < gb : a.name.lowercased() < b.name.lowercased()
        }.prefix(400)
        message.isHidden = !shown.isEmpty
        message.text = query.isEmpty ? "No apps detected" : "No matching apps"
        var keep: [String: AppRow] = [:]
        order = []
        for app in shown {
            let row: AppRow
            if let r = rows[app.name] {
                row = r
                row.checked = filtered.contains(app.name)
            } else {
                row = AppRow(name: app.name, checked: filtered.contains(app.name))
                let name = app.name
                row.onClick = { [unowned self] in self.root.toggleFiltered(name) }
                list.addSubview(row)
                if let path = app.path {
                    root.appIcon(path) { [weak row] img in row?.icon = img }
                }
            }
            keep[app.name] = row
            order.append(app.name)
        }
        for (name, row) in rows where keep[name] == nil { row.removeFromSuperview() }
        rows = keep
    }

    override func build(width: CGFloat, height: CGFloat) -> CGFloat {
        texts = []
        var y = chrome("Filtered Apps", "Selected apps will be blacked out in monitor screen captures.", back: true, width: width)
        let inner = width - 2 * pad
        search.frame = CGRect(x: pad, y: y, width: inner, height: LKField.height)
        y += LKField.height + 12
        card.frame = CGRect(x: pad, y: y, width: inner, height: max(120, height - y - 16))
        scroll.frame = card.bounds.insetBy(dx: 1, dy: 1)
        message.frame = card.bounds
        let w = scroll.contentSize.width
        var ry: CGFloat = 4
        for name in order {
            rows[name]?.frame = CGRect(x: 4, y: ry, width: w - 8, height: AppRow.height)
            ry += AppRow.height
        }
        if skeleton.superview != nil {
            skeleton.frame = CGRect(x: 0, y: 0, width: w, height: 4 + 12 * AppRow.height + 4)
            ry = skeleton.frame.height - 4
        }
        list.frame = CGRect(x: 0, y: 0, width: w, height: max(ry + 4, scroll.contentSize.height))
        needsDisplay = true
        return height
    }
}

/// A centred, tertiary line ("No matching apps").
final class MessageView: FlippedView {
    var text = "" {
        didSet { needsDisplay = true }
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        let w = Txt.width(text, Txt.font(12))
        Txt.draw(text, at: CGPoint(x: (bounds.width - w) / 2, y: 24), width: w + 1, font: Txt.font(12),
                 color: t.textTertiary, lineHeight: Txt.lh(12))
    }
}

// MARK: - Linked Programs

final class LinkedProgramsPage: Page {
    private let card = Card()
    private var rows: [ProgramRow] = []
    private var programs: [String] = []

    override init(root: PaneRoot) {
        super.init(root: root)
        addSubview(card)
        stateChanged()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func stateChanged() {
        let links = root.state.links
        if links.map(\.program) != programs {
            rows.forEach { $0.removeFromSuperview() }
            rows = links.map { link in
                let b = LKButton("Unlink", .secondary, card: true) { [unowned self] in self.confirm(link) }
                let row = ProgramRow(label: link.label, detail: link.detail, unlink: b)
                if let url = link.iconURL {
                    root.remoteImage(url) { [weak row] img in row?.image = img }
                }
                card.addSubview(row)
                return row
            }
            programs = links.map(\.program)
        }
        for (row, program) in zip(rows, programs) {
            row.unlink.title = root.unlinking == program ? "Unlinking…" : "Unlink"
            row.unlink.isEnabled = root.unlinking == nil
        }
        root.relayout()
    }

    private func confirm(_ link: LinkInfo) {
        root.confirm(
            title: "Unlink program",
            message: "Unlink \(link.label) from this device?\n\nStarting a session from the + menu will go through your browser again until you re-link.",
            ok: "Unlink"
        ) { [unowned self] in
            self.root.unlink(link.program)
        }
    }

    override func build(width: CGFloat, height: CGFloat) -> CGFloat {
        texts = []
        var y = chrome("Linked Programs", "Programs linked to this device can start a timelapse straight from the + menu.",
                       back: true, width: width)
        let inner = width - 2 * pad
        var ry: CGFloat = 4
        for row in rows {
            row.frame = CGRect(x: 4, y: ry, width: inner - 8, height: ProgramRow.height)
            ry += ProgramRow.height
        }
        card.frame = CGRect(x: pad, y: y, width: inner, height: ry + 4)
        card.isHidden = rows.isEmpty
        y += card.frame.height + 16
        needsDisplay = true
        return y
    }
}

// MARK: - Advanced

/// `normalizeServerUrl`: https (http only for localhost), reduced to its origin.
func normalizeServerURL(_ input: String) -> String? {
    let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty { return nil }
    let lower = trimmed.lowercased()
    let withScheme = lower.hasPrefix("http://") || lower.hasPrefix("https://") ? trimmed : "https://" + trimmed
    guard let c = URLComponents(string: withScheme), let scheme = c.scheme?.lowercased(),
          let host = c.host?.lowercased(), !host.isEmpty else { return nil }
    let local = host == "localhost" || host == "127.0.0.1"
    guard scheme == "https" || (scheme == "http" && local) else { return nil }
    let defaultPort = scheme == "https" ? 443 : 80
    if let port = c.port, port != defaultPort { return "\(scheme)://\(host):\(port)" }
    return "\(scheme)://\(host)"
}

final class AdvancedPage: Page {
    static let defaultServer = "https://lookout.hackclub.com"
    /// Holds the card's controls; its surface is drawn by the page, under the text.
    private let card = FlippedView()
    private let field = LKField(placeholder: AdvancedPage.defaultServer, mono: true)
    private let save: LKButton
    private let reset: LKButton
    private var error: String?
    private var saving = false

    override init(root: PaneRoot) {
        save = LKButton("Save", .primary)
        reset = LKButton("Reset to default", .secondary)
        super.init(root: root)
        field.text = root.state.apiBase
        field.onChange = { [unowned self] _ in
            self.error = nil
            self.refreshControls()
        }
        field.onSubmit = { [unowned self] in self.submit() }
        save.onClick = { [unowned self] in self.submit() }
        reset.onClick = { [unowned self] in
            self.field.text = AdvancedPage.defaultServer
            self.apply(AdvancedPage.defaultServer)
        }
        for v in [field, save, reset] { card.addSubview(v) }
        addSubview(card)
        refreshControls()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    private var normalized: String? { normalizeServerURL(field.text) }
    private var invalid: Bool {
        !field.text.trimmingCharacters(in: .whitespaces).isEmpty && normalized == nil
    }

    private func refreshControls() {
        let dirty = normalized.map { $0 != root.state.apiBase } ?? false
        save.title = saving ? "Checking…" : "Save"
        save.isEnabled = dirty && !invalid && !saving
        reset.isEnabled = !saving
        field.invalid = error != nil || invalid
        root.relayout()
    }

    override func stateChanged() {
        if !saving, !field.focused { field.text = root.state.apiBase }
        refreshControls()
    }

    private func submit() {
        guard let target = normalized, target != root.state.apiBase, !saving else { return }
        if target != AdvancedPage.defaultServer {
            root.confirm(
                title: "Switch Lookout server",
                message: "Point this app at \(target)?\n\nAll new recordings will upload there instead of the official Lookout server. Only continue if someone from Hack Club asked you to.",
                ok: "Switch"
            ) { [unowned self] in self.apply(target) }
        } else {
            apply(target)
        }
    }

    private func apply(_ target: String) {
        saving = true
        error = nil
        refreshControls()
        root.setServer(target) { [weak self] err in
            guard let self = self else { return }
            self.saving = false
            self.error = err
            if err == nil { self.field.text = self.root.state.apiBase }
            self.refreshControls()
        }
    }

    override func build(width: CGFloat, height: CGFloat) -> CGFloat {
        texts = []
        outlines = []
        surfaces = []
        var y = chrome("Advanced",
                       "Options for testing and development. Please don't touch anything if you don't know what you're doing.",
                       back: true, width: width)
        let inner = width - 2 * pad
        let cw = inner - 32
        // The card's own text is drawn by this page, offset by the card's origin.
        var cy: CGFloat = 16
        let label = CGRect(x: pad + 16, y: y + cy, width: cw, height: Txt.lh(12))
        text("Lookout server", label, Txt.font(12, .medium), .secondary, lh: Txt.lh(12))
        cy += Txt.lh(12) + 4
        field.frame = CGRect(x: 16, y: cy, width: cw, height: LKField.height)
        cy += LKField.height + 4
        let hint = "Please enter a VALID lookout service URL."
        text(hint, CGRect(x: pad + 16, y: y + cy, width: cw, height: 18), Txt.font(12), .tertiary, lh: 18)
        cy += 18
        var errors: [String] = []
        if invalid { errors.append("Enter a valid HTTPS URL (e.g. https://lookout-stage.example.com).") }
        if let e = error { errors.append(e) }
        for e in errors {
            let h = Txt.height(e, Txt.font(12), width: cw, lineHeight: Txt.lh(12))
            text(e, CGRect(x: pad + 16, y: y + cy, width: cw, height: h), Txt.font(12), .danger, lh: Txt.lh(12), wrap: true)
            cy += h
        }
        cy += 12
        save.sizeToFit(minWidth: save.fittingWidth(for: "Checking…"))
        save.setFrameOrigin(CGPoint(x: 16, y: cy))
        reset.sizeToFit()
        reset.setFrameOrigin(CGPoint(x: save.frame.maxX + 8, y: cy))
        reset.isHidden = root.state.isDefault
        cy += LKButton.height + 16
        card.frame = CGRect(x: pad, y: y, width: inner, height: cy)
        surfaces.append(card.frame)
        y += cy
        if !root.state.isDefault {
            y += 12
            let msg = "Using a custom server. Timelapses recorded here won't appear on the default Lookout server."
            let h = Txt.height(msg, Txt.font(12), width: inner - 24 - 2, lineHeight: 18)
            outlines.append((CGRect(x: pad, y: y, width: inner, height: h + 16 + 2), .warning))
            text(msg, CGRect(x: pad + 13, y: y + 9, width: inner - 26, height: h), Txt.font(12), .warning, lh: 18, wrap: true)
            y += h + 18
        }
        needsDisplay = true
        return y + 16
    }
}

// MARK: - Lookout is gone

final class MissingPage: Page {
    private let get: LKButton
    private let remove: LKButton
    private var removed = false

    override init(root: PaneRoot) {
        get = LKButton("Get Lookout", .primary) { [unowned root] in root.openWebsite() }
        remove = LKButton("Remove This Pane", .secondary)
        super.init(root: root)
        remove.onClick = { [unowned self] in
            if self.root.removePane() {
                self.removed = true
                self.remove.isEnabled = false
                self.root.relayout()
            } else {
                NSSound.beep()
            }
        }
        addSubview(get)
        addSubview(remove)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func build(width: CGFloat, height: CGFloat) -> CGFloat {
        texts = []
        var y = chrome("Lookout isn't on this Mac anymore",
                       "Download it again to record timelapses, or remove this pane from System Settings.",
                       back: false, width: width)
        get.sizeToFit()
        get.setFrameOrigin(CGPoint(x: pad, y: y))
        remove.sizeToFit()
        remove.setFrameOrigin(CGPoint(x: get.frame.maxX + 8, y: y))
        y += LKButton.height + 16
        if removed {
            text("Removed. Lookout leaves System Settings the next time it opens.",
                 CGRect(x: pad, y: y, width: width - 2 * pad, height: 18), Txt.font(12), .tertiary, lh: 18)
            y += 18 + 16
        }
        needsDisplay = true
        return y
    }
}
