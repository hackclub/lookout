// Lookout's controls in AppKit: Button (ui/button.rs), the pressable rows
// of the Settings screens (screens/settings.rs: MenuRow, ToggleRow,
// app_row), cards, text fields and the loading skeleton. Each one draws
// itself from the theme and animates with Kit's Anim on the shared clock.

import AppKit

// MARK: - Button

enum ButtonVariant {
    case primary
    case secondary
}

/// `Button` at size Sm: the squircle background springs to 0.96 on press
/// (1500 / 60) while the label stays put; hover blends the fill.
final class LKButton: FlippedView, Animating, HoverSyncing {
    var title: String {
        didSet { needsDisplay = true }
    }
    var variant: ButtonVariant
    /// `card_style`: surface fill and default border, radius 10.
    var card: Bool
    var isEnabled = true {
        didSet {
            alphaValue = isEnabled ? 1 : 0.6
            if !isEnabled {
                pressed = false
                hover.set(0, .spring(.press))
                press.set(0, .spring(.press))
                animate()
            } else if hovered {
                hover.set(1, .spring(.press))
                animate()
            }
        }
    }
    var onClick: (() -> Void)?

    private var hovered = false
    private var pressed = false
    private var hover = Anim(0)
    private var press = Anim(0)
    private var tracking: NSTrackingArea?

    static let height: CGFloat = Txt.lh(12) + 12 + 2
    private let font = Txt.font(12, .semibold)

    init(_ title: String, _ variant: ButtonVariant = .primary, card: Bool = false, onClick: (() -> Void)? = nil) {
        self.title = title
        self.variant = variant
        self.card = card
        self.onClick = onClick
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Width for a label: padding 12 each side plus the transparent 1px border.
    func fittingWidth(for label: String? = nil) -> CGFloat {
        Txt.width(label ?? title, font) + 24 + 2
    }

    func sizeToFit(minWidth: CGFloat = 0) {
        setFrameSize(NSSize(width: max(fittingWidth(), minWidth), height: LKButton.height))
    }

    private func colors(_ t: Theme) -> (NSColor, NSColor, NSColor, NSColor, NSColor) {
        if card {
            let text = variant == .primary ? t.onAccent : t.textSecondary
            return (t.bgSurface, t.borderDefault, t.bgSurface, t.borderDefault, text)
        }
        switch variant {
        case .primary: return (t.accent, clearColor, t.accentHover, clearColor, t.onAccent)
        case .secondary: return (clearColor, t.borderHover, t.bgSurface, t.borderHover, t.textSecondary)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        let (ib, ibo, hb, hbo, text) = colors(t)
        let h = hover.v
        paintSquircle(bounds, radius: card ? 10 : 8, scale: 1 - press.v, fill: mix(ib, hb, h), border: mix(ibo, hbo, h))
        let w = Txt.width(title, font)
        let lh = Txt.lh(12)
        Txt.draw(title, at: CGPoint(x: (bounds.width - w) / 2, y: (bounds.height - lh) / 2), width: w + 1,
                 font: font, color: text, lineHeight: lh)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let t = tracking { removeTrackingArea(t) }
        let t = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(t)
        tracking = t
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseEntered(with event: NSEvent) { enter() }

    override func mouseExited(with event: NSEvent) { leave() }

    private func enter() {
        hovered = true
        guard isEnabled else { return }
        hover.set(1, .spring(.press))
        animate()
    }

    private func leave() {
        hovered = false
        if !pressed {
            hover.set(0, .spring(.press))
            animate()
        }
    }

    func syncHover() {
        let inside = pointerInside(self)
        guard inside != hovered else { return }
        if inside { enter() } else { leave() }
    }

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        pressed = true
        press.set(0.04, .spring(.press))
        hover.set(1, .spring(.press))
        animate()
    }

    override func mouseUp(with event: NSEvent) {
        guard pressed else { return }
        pressed = false
        press.set(0, .spring(.press))
        if !hovered { hover.set(0, .spring(.press)) }
        animate()
        if isEnabled, bounds.contains(convert(event.locationInWindow, from: nil)) {
            onClick?()
        }
    }

    private func animate() {
        needsDisplay = true
        Animator.shared.run(self)
    }

    func animationFrame() -> Bool {
        let a = hover.tick()
        let b = press.tick()
        needsDisplay = true
        return a || b
    }
}

// MARK: - Rows

/// `pressable_row`: hover fills the background (120ms ease-out), press
/// scales the fill to 0.96 (120ms ease-out); the content stays put.
class PressRow: FlippedView, Animating, HoverSyncing {
    var onClick: (() -> Void)?
    private var hovered = false
    private var hover = Anim(0)
    private var press = Anim(0)
    private var pressed = false
    private var tracking: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let t = tracking { removeTrackingArea(t) }
        let t = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(t)
        tracking = t
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseEntered(with event: NSEvent) { setHovered(true) }

    override func mouseExited(with event: NSEvent) { setHovered(false) }

    private func setHovered(_ on: Bool) {
        guard on != hovered else { return }
        hovered = on
        // Only one row in a list is ever lit, even if AppKit loses an exit.
        if on, let siblings = superview?.subviews {
            for case let r as PressRow in siblings where r !== self { r.setHovered(false) }
        }
        hover.setCSS(on ? 1 : 0, 120, .easeOut)
        animate()
    }

    /// AppKit drops `mouseExited` when content scrolls out from under a
    /// still pointer, which left rows lit up after a scroll: ask where the
    /// pointer really is instead.
    func syncHover() { setHovered(pointerInside(self)) }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { hovered = false; hover.jump(0); press.jump(0); pressed = false }
    }

    override func mouseDown(with event: NSEvent) {
        pressed = true
        press.setCSS(1, 120, .easeOut)
        animate()
    }

    override func mouseUp(with event: NSEvent) {
        guard pressed else { return }
        pressed = false
        press.setCSS(0, 120, .easeOut)
        animate()
        if bounds.contains(convert(event.locationInWindow, from: nil)) {
            onClick?()
        }
    }

    func animate() {
        needsDisplay = true
        Animator.shared.run(self)
    }

    /// Subclasses' own animations, ticked with the row's.
    func tickExtra() -> Bool { false }

    func animationFrame() -> Bool {
        let a = hover.tick()
        let b = press.tick()
        let c = tickExtra()
        needsDisplay = true
        return a || b || c
    }

    func drawBackground(_ t: Theme) {
        let p = press.v
        let fill = mix(clearColor, t.bgSelected, max(hover.v, p))
        if fill.alphaComponent > 0.001 {
            fill.setFill()
            roundedRect(bounds, 8, scale: 1 - 0.04 * p).fill()
        }
    }
}

/// The 18px checkbox the toggle and app rows end or start with.
func drawCheckbox(_ rect: CGRect, progress c: CGFloat, on: Bool, offBorder: NSColor, onColor: NSColor, check: NSColor) {
    paintBox(rect, radius: 6, fill: mix(clearColor, onColor, c), border: mix(offBorder, onColor, c), borderWidth: 1.5)
    if on {
        Icon.checkBold.draw(in: CGRect(x: rect.midX - 6, y: rect.midY - 6, width: 12, height: 12), color: check)
    }
}

/// `MenuRow` / `ToggleRow`: a 32px icon tile, title and description, then a
/// caret or a checkbox.
final class MenuRow: PressRow {
    enum Trailing {
        case caret
        case toggle
    }

    let icon: Icon
    let title: String
    var detail: String {
        didSet { needsDisplay = true }
    }
    let trailing: Trailing
    var isOn = false {
        didSet {
            guard isOn != oldValue else { return }
            check.set(isOn ? 1 : 0, .tween(150, .cssEase))
            animate()
        }
    }
    private var check = Anim(0)

    static let height: CGFloat = 12 + 32 + 12

    init(_ icon: Icon, _ title: String, _ detail: String, _ trailing: Trailing = .caret, onClick: (() -> Void)? = nil) {
        self.icon = icon
        self.title = title
        self.detail = detail
        self.trailing = trailing
        super.init(frame: .zero)
        self.onClick = onClick
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Set without animating (first state).
    func setOnImmediately(_ on: Bool) {
        isOn = on
        check.jump(on ? 1 : 0)
    }

    override func tickExtra() -> Bool { check.tick() }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        drawBackground(t)
        let h = bounds.height
        let tile = CGRect(x: 12, y: (h - 32) / 2, width: 32, height: 32)
        paintBox(tile, radius: 8, fill: t.bgSelected, border: nil)
        icon.draw(in: tile.insetBy(dx: 7, dy: 7), color: t.textSecondary)

        let trailW: CGFloat = trailing == .caret ? 16 : 18
        let trailX = bounds.width - 12 - trailW
        if trailing == .caret {
            Icon.caretRight.draw(in: CGRect(x: trailX, y: (h - 16) / 2, width: 16, height: 16), color: t.textTertiary)
        } else {
            drawCheckbox(CGRect(x: trailX, y: (h - 18) / 2, width: 18, height: 18), progress: check.v, on: isOn,
                         offBorder: t.borderHover, onColor: t.textPrimary, check: t.textInverse)
        }

        let x: CGFloat = 12 + 32 + 12
        let w = trailX - 12 - x
        let tl = Txt.lh(13), dl = Txt.lh(12)
        if detail.isEmpty {
            Txt.draw(title, at: CGPoint(x: x, y: (h - tl) / 2), width: w, font: Txt.font(13, .medium),
                     color: t.textPrimary, lineHeight: tl)
        } else {
            let top = (h - tl - dl) / 2
            Txt.draw(title, at: CGPoint(x: x, y: top), width: w, font: Txt.font(13, .medium), color: t.textPrimary, lineHeight: tl)
            Txt.draw(detail, at: CGPoint(x: x, y: top + tl), width: w, font: Txt.font(12), color: t.textTertiary, lineHeight: dl)
        }
    }
}

/// `app_row`: red checkbox, the app's icon (fading in once loaded) and name.
final class AppRow: PressRow {
    let name: String
    var checked: Bool {
        didSet {
            guard checked != oldValue else { return }
            check.setCSS(checked ? 1 : 0, 150, .cssEase)
            animate()
        }
    }
    private(set) var icon: NSImage?
    private var check: Anim
    private var iconFade = Anim(1)

    static let height: CGFloat = 6 + 20 + 6

    init(name: String, checked: Bool) {
        self.name = name
        self.checked = checked
        check = Anim(checked ? 1 : 0)
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Fade in only icons that arrive after the row is on screen.
    func setIcon(_ image: NSImage, fade: Bool) {
        icon = image
        iconFade = fade ? Anim.fromTo(0, 1, .tween(200, .easeOut)) : Anim(1)
        if fade { animate() } else { needsDisplay = true }
    }

    override func tickExtra() -> Bool {
        let a = check.tick()
        let b = iconFade.tick()
        return a || b
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        drawBackground(t)
        let h = bounds.height
        drawCheckbox(CGRect(x: 8, y: (h - 18) / 2, width: 18, height: 18), progress: check.v, on: checked,
                     offBorder: t.borderHover, onColor: Theme.danger, check: rgb(0xffffff))
        let iconRect = CGRect(x: 8 + 18 + 2 + 6, y: (h - 20) / 2, width: 20, height: 20)
        if let icon = icon {
            icon.draw(in: iconRect, from: .zero, operation: .sourceOver, fraction: iconFade.v, respectFlipped: true, hints: nil)
        } else {
            paintBox(iconRect, radius: 6, fill: t.bgSelected, border: nil)
        }
        let x = iconRect.maxX + 6
        let lh = Txt.lh(13)
        Txt.draw(name, at: CGPoint(x: x, y: (h - lh) / 2), width: bounds.width - x - 8,
                 font: Txt.font(13, checked ? .medium : .regular), color: t.textPrimary, lineHeight: lh)
    }
}

/// A linked program: icon tile, name and "Linked <date> · <host>", Unlink.
final class ProgramRow: FlippedView {
    let label: String
    let detail: String
    var image: NSImage? {
        didSet { needsDisplay = true }
    }
    let unlink: LKButton

    static let height: CGFloat = 8 + 32 + 8

    init(label: String, detail: String, unlink: LKButton) {
        self.label = label
        self.detail = detail
        self.unlink = unlink
        super.init(frame: .zero)
        addSubview(unlink)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func layout() {
        super.layout()
        unlink.sizeToFit(minWidth: unlink.fittingWidth(for: "Unlinking…"))
        unlink.setFrameOrigin(CGPoint(x: bounds.width - 12 - unlink.frame.width, y: (bounds.height - LKButton.height) / 2))
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        let h = bounds.height
        let tile = CGRect(x: 12, y: (h - 32) / 2, width: 32, height: 32)
        paintBox(tile, radius: 8, fill: t.bgSelected, border: nil)
        if let image = image {
            NSGraphicsContext.saveGraphicsState()
            NSBezierPath(roundedRect: tile, xRadius: 8, yRadius: 8).addClip()
            image.draw(in: tile, from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
            NSGraphicsContext.restoreGraphicsState()
        } else {
            Icon.linkBold.draw(in: tile.insetBy(dx: 8, dy: 8), color: t.textSecondary)
        }
        let x = tile.maxX + 12
        let w = unlink.frame.minX - 12 - x
        let tl = Txt.lh(13), dl = Txt.lh(12)
        let top = (h - tl - dl) / 2
        Txt.draw(label, at: CGPoint(x: x, y: top), width: w, font: Txt.font(13, .medium), color: t.textPrimary, lineHeight: tl)
        Txt.draw(detail, at: CGPoint(x: x, y: top + tl), width: w, font: Txt.font(12), color: t.textTertiary, lineHeight: dl)
    }
}

// MARK: - Hover sync

/// A control that can re-check whether the pointer is over it.
protocol HoverSyncing: AnyObject {
    func syncHover()
}

/// Whether the pointer is over the visible part of `view`.
func pointerInside(_ view: NSView) -> Bool {
    guard let w = view.window, w.isVisible else { return false }
    // Inside System Settings `mouseLocationOutsideOfEventStream` can be
    // stale, so prefer the event being handled (the scroll itself).
    let loc: NSPoint
    let pointerEvents: [NSEvent.EventType] = [.scrollWheel, .mouseMoved, .mouseEntered, .mouseExited, .leftMouseDragged]
    if let e = NSApp.currentEvent, e.window === w, pointerEvents.contains(e.type) {
        loc = e.locationInWindow
    } else {
        loc = w.convertPoint(fromScreen: NSEvent.mouseLocation)
    }
    let p = view.convert(loc, from: nil)
    return view.visibleRect.contains(p)
}

/// Re-check hover for every control under `view` (after a scroll).
func syncHover(in view: NSView) {
    if let v = view as? HoverSyncing { v.syncHover() }
    for sub in view.subviews where !sub.isHidden { syncHover(in: sub) }
}

// MARK: - Header button

/// The header bar's round back button (ui/header_bar.rs `HeaderBtn`,
/// `Action` kind): 30px circle, transparent at rest, `headerbar_control` on
/// hover and `headerbar_control_hover` while pressed, the glyph going from
/// secondary to primary text (100ms ease-out each).
final class RoundButton: FlippedView, Animating, HoverSyncing {
    let icon: Icon
    var onClick: (() -> Void)?
    private var hovered = false
    private var pressed = false
    private var hover = Anim(0)
    private var press = Anim(0)
    private var tracking: NSTrackingArea?

    static let size: CGFloat = 30

    init(_ icon: Icon, label: String, onClick: (() -> Void)? = nil) {
        self.icon = icon
        self.onClick = onClick
        super.init(frame: CGRect(x: 0, y: 0, width: RoundButton.size, height: RoundButton.size))
        toolTip = label
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(label)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        let h = hover.v, p = press.v
        let bg = mix(mix(clearColor, t.headerbarControl, h), t.headerbarControlHover, p)
        if bg.alphaComponent > 0.001 {
            bg.setFill()
            NSBezierPath(ovalIn: bounds).fill()
        }
        let fg = mix(t.textSecondary, t.textPrimary, max(h, p))
        icon.draw(in: CGRect(x: bounds.midX - 8, y: bounds.midY - 8, width: 16, height: 16), color: fg)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let t = tracking { removeTrackingArea(t) }
        let t = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(t)
        tracking = t
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    private func setHovered(_ on: Bool) {
        guard on != hovered else { return }
        hovered = on
        hover.setCSS(on ? 1 : 0, 100, .easeOut)
        animate()
    }

    override func mouseEntered(with event: NSEvent) { setHovered(true) }
    override func mouseExited(with event: NSEvent) { setHovered(false) }
    func syncHover() { setHovered(pointerInside(self)) }

    override func mouseDown(with event: NSEvent) {
        pressed = true
        press.setCSS(1, 100, .easeOut)
        animate()
    }

    override func mouseUp(with event: NSEvent) {
        guard pressed else { return }
        pressed = false
        press.setCSS(0, 100, .easeOut)
        animate()
        if bounds.contains(convert(event.locationInWindow, from: nil)) { onClick?() }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { hovered = false; pressed = false; hover.jump(0); press.jump(0) }
    }

    private func animate() {
        needsDisplay = true
        Animator.shared.run(self)
    }

    func animationFrame() -> Bool {
        let a = hover.tick()
        let b = press.tick()
        needsDisplay = true
        return a || b
    }
}

// MARK: - Link

/// A plain text link (screens/info.rs `link`): secondary text that
/// brightens to primary on hover.
final class LinkLabel: FlippedView {
    let title: String
    let url: URL
    private var hovered = false {
        didSet { needsDisplay = true }
    }
    private var tracking: NSTrackingArea?
    static let font = Txt.font(12)

    init(_ title: String, _ url: String) {
        self.title = title
        self.url = URL(string: url)!
        super.init(frame: .zero)
        setAccessibilityElement(true)
        setAccessibilityRole(.link)
        setAccessibilityLabel(title)
        toolTip = url
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    var fittingWidth: CGFloat { Txt.width(title, LinkLabel.font) }

    override func accessibilityPerformPress() -> Bool {
        NSWorkspace.shared.open(url)
        return true
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        Txt.draw(title, at: .zero, width: bounds.width + 1, font: LinkLabel.font,
                 color: hovered ? t.textPrimary : t.textSecondary, lineHeight: bounds.height)
    }

    override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let t = tracking { removeTrackingArea(t) }
        let t = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(t)
        tracking = t
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func mouseEntered(with event: NSEvent) { hovered = true }
    override func mouseExited(with event: NSEvent) { hovered = false }
    override func mouseDown(with event: NSEvent) {}

    override func mouseUp(with event: NSEvent) {
        if bounds.contains(convert(event.locationInWindow, from: nil)) { NSWorkspace.shared.open(url) }
    }
}

// MARK: - Containers

/// The rounded, bordered surface Settings groups rows in.
final class Card: FlippedView {
    var padding: CGFloat = 4

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        paintBox(bounds, radius: 10, fill: t.bgSurface, border: t.borderDefault)
    }
}

// MARK: - Text field

private final class FocusField: NSTextField {
    var onFocus: (() -> Void)?

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { onFocus?() }
        return ok
    }
}

/// `field_box` + `TextInput`: surface fill, a border that turns
/// `border_hover` when focused or red when invalid, 12×8 padding.
final class LKField: FlippedView, NSTextFieldDelegate {
    private let field = FocusField()
    private let placeholder: String
    private(set) var focused = false {
        didSet { needsDisplay = true }
    }
    var invalid = false {
        didSet { needsDisplay = true }
    }
    var onChange: ((String) -> Void)?
    var onSubmit: (() -> Void)?

    static let height: CGFloat = 8 + Txt.lh(13) + 8 + 2

    var text: String {
        get { field.stringValue }
        set { field.stringValue = newValue }
    }

    init(placeholder: String, mono: Bool = false) {
        self.placeholder = placeholder
        super.init(frame: .zero)
        field.isBordered = false
        field.isBezeled = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = mono ? Txt.mono(13) : Txt.font(13)
        field.usesSingleLineMode = true
        field.lineBreakMode = .byClipping
        field.cell?.isScrollable = true
        field.cell?.wraps = false
        field.delegate = self
        field.onFocus = { [weak self] in self?.focused = true }
        addSubview(field)
        applyColors()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func focus() {
        window?.makeFirstResponder(field)
    }

    private func applyColors() {
        let t = Theme.of(self)
        field.textColor = t.textPrimary
        field.placeholderAttributedString = NSAttributedString(string: placeholder, attributes: [
            .font: field.font ?? Txt.font(13),
            .foregroundColor: t.textPrimary.withAlphaComponent(t.textPrimary.alphaComponent * 0.45),
        ])
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColors()
    }

    override func layout() {
        super.layout()
        let fh = ceil(field.intrinsicContentSize.height)
        field.frame = CGRect(x: 12, y: (bounds.height - fh) / 2, width: bounds.width - 24, height: fh)
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        let border = invalid ? Theme.danger : (focused ? t.borderHover : t.borderDefault)
        paintBox(bounds, radius: 8, fill: t.bgSurface, border: border)
    }

    override func mouseDown(with event: NSEvent) { focus() }

    func controlTextDidChange(_ obj: Notification) { onChange?(field.stringValue) }

    func controlTextDidEndEditing(_ obj: Notification) { focused = false }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.insertNewline(_:)) {
            onSubmit?()
            return true
        }
        return false
    }
}

// MARK: - Skeleton

/// The Filtered Apps list while it loads: 12 rows pulsing 0.45 → 0.9 → 0.45
/// over 1.2s (ease-in-out), each 50ms behind the one above.
final class SkeletonRows: FlippedView, Animating {
    private let start = nowMs()

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil { Animator.shared.run(self) }
    }

    func animationFrame() -> Bool {
        needsDisplay = true
        return window != nil
    }

    override func draw(_ dirtyRect: NSRect) {
        let t = Theme.of(self)
        for i in 0..<12 {
            let since = nowMs() - start - Double(i) * 50
            let tt = since <= 0 ? 0 : since.truncatingRemainder(dividingBy: 1200) / 1200
            let o = tt < 0.5 ? 0.45 + 0.45 * Ease.easeInOut.apply(tt * 2) : 0.9 - 0.45 * Ease.easeInOut.apply((tt - 0.5) * 2)
            let y = 4 + CGFloat(i) * AppRow.height
            let fill = t.bgSelected.withAlphaComponent(t.bgSelected.alphaComponent * CGFloat(o))
            let mid = y + AppRow.height / 2
            paintBox(CGRect(x: 4 + 8, y: mid - 9, width: 18, height: 18), radius: 6, fill: fill, border: nil)
            // Same columns as AppRow: checkbox (+2), icon, then the name bar
            // at 40-79% of the row's width.
            paintBox(CGRect(x: 4 + 8 + 18 + 2 + 6, y: mid - 10, width: 20, height: 20), radius: 6, fill: fill, border: nil)
            let x: CGFloat = 4 + 8 + 18 + 2 + 6 + 20 + 6
            let w = min((bounds.width - 8) * CGFloat(40 + (i * 23) % 40) / 100, bounds.width - 4 - 8 - x)
            paintBox(CGRect(x: x, y: mid - 6, width: w, height: 12), radius: 6, fill: fill, border: nil)
        }
    }
}
