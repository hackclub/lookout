// Lookout's design system, ported from the GPUI app so the pane looks and
// moves like Lookout: colour tokens (crates/lookout-native/src/theme.rs),
// motion (motion.rs: framer-motion's analytic spring, CSS cubic-bezier, CSS
// transition retargeting), the squircle every button is clipped to
// (ui/paint.rs, figma-squircle smoothing 0.7) and the icons (assets/icons,
// converted to template PDFs by packaging/macos/svg2pdf.py).

import AppKit
import QuartzCore

// MARK: - Colour

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: a)
}

func white(_ a: CGFloat) -> NSColor { NSColor(srgbRed: 1, green: 1, blue: 1, alpha: a) }
func black(_ a: CGFloat) -> NSColor { NSColor(srgbRed: 0, green: 0, blue: 0, alpha: a) }
let clearColor = NSColor(srgbRed: 0, green: 0, blue: 0, alpha: 0)

/// `theme::mix`: premultiplied-alpha interpolation in sRGB.
func mix(_ a: NSColor, _ b: NSColor, _ t: CGFloat) -> NSColor {
    let t = min(max(t, 0), 1)
    if t <= 0 { return a }
    if t >= 1 { return b }
    let ca = a.usingColorSpace(.sRGB) ?? a
    let cb = b.usingColorSpace(.sRGB) ?? b
    let alpha = ca.alphaComponent + (cb.alphaComponent - ca.alphaComponent) * t
    if alpha <= 0.0001 { return clearColor }
    func pm(_ x: CGFloat, _ y: CGFloat) -> CGFloat {
        (x * ca.alphaComponent + (y * cb.alphaComponent - x * ca.alphaComponent) * t) / alpha
    }
    return NSColor(srgbRed: pm(ca.redComponent, cb.redComponent), green: pm(ca.greenComponent, cb.greenComponent),
                   blue: pm(ca.blueComponent, cb.blueComponent), alpha: alpha)
}

/// The tokens the Settings screens use, dark and light (`Theme::dark/light`).
struct Theme {
    let dark: Bool
    let bgSurface: NSColor
    let bgSelected: NSColor
    let textPrimary: NSColor
    let textInverse: NSColor
    let textSecondary: NSColor
    let textTertiary: NSColor
    let borderDefault: NSColor
    let borderHover: NSColor
    let accent = rgb(0x3b82f6)
    /// `oklab_mix_black(accent, 0.88)`.
    let accentHover = rgb(0x306dd0)
    let onAccent = rgb(0xffffff)

    static let danger = rgb(0xef4444)
    static let warning = rgb(0xf59e0b)

    static let darkTheme = Theme(
        dark: true, bgSurface: white(0.08), bgSelected: white(0.12), textPrimary: rgb(0xffffff),
        textInverse: rgb(0x000000), textSecondary: white(0.7), textTertiary: white(0.5),
        borderDefault: white(0.12), borderHover: white(0.22))
    static let lightTheme = Theme(
        dark: false, bgSurface: white(0.5), bgSelected: black(0.1), textPrimary: black(0.85),
        textInverse: rgb(0xffffff), textSecondary: black(0.65), textTertiary: black(0.45),
        borderDefault: black(0.12), borderHover: black(0.22))

    static func of(_ view: NSView) -> Theme {
        view.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? darkTheme : lightTheme
    }
}

// MARK: - Type

/// macOS Lookout draws in SF with CSS `line-height: normal` (1.19 × size).
enum Txt {
    static func font(_ size: CGFloat, _ weight: NSFont.Weight = .regular) -> NSFont {
        .systemFont(ofSize: size, weight: weight)
    }
    static func mono(_ size: CGFloat) -> NSFont {
        NSFont(name: "Menlo", size: size) ?? .monospacedSystemFont(ofSize: size, weight: .regular)
    }
    /// `ts(size)`: the face's normal line height, rounded.
    static func lh(_ size: CGFloat) -> CGFloat { (size * 1.19).rounded() }

    static func attrs(_ font: NSFont, _ color: NSColor, lineHeight: CGFloat? = nil,
                      truncate: Bool = true, align: NSTextAlignment = .left) -> [NSAttributedString.Key: Any] {
        let p = NSMutableParagraphStyle()
        p.lineBreakMode = truncate ? .byTruncatingTail : .byWordWrapping
        p.alignment = align
        if let lh = lineHeight {
            p.minimumLineHeight = lh
            p.maximumLineHeight = lh
        }
        return [.font: font, .foregroundColor: color, .paragraphStyle: p]
    }

    /// Draw one line, vertically centred on its line box at `y`.
    static func draw(_ s: String, at p: CGPoint, width: CGFloat, font: NSFont, color: NSColor, lineHeight: CGFloat) {
        let a = NSAttributedString(string: s, attributes: attrs(font, color))
        let h = a.size().height
        a.draw(with: CGRect(x: p.x, y: p.y + (lineHeight - h) / 2, width: max(0, width), height: h),
               options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    static func width(_ s: String, _ font: NSFont) -> CGFloat {
        ceil(NSAttributedString(string: s, attributes: [.font: font]).size().width)
    }

    /// Height of wrapped text at `width`.
    static func height(_ s: String, _ font: NSFont, width: CGFloat, lineHeight: CGFloat) -> CGFloat {
        let a = NSAttributedString(string: s, attributes: attrs(font, .black, lineHeight: lineHeight, truncate: false))
        let r = a.boundingRect(with: CGSize(width: width, height: 10_000), options: [.usesLineFragmentOrigin])
        return ceil(max(r.height, lineHeight))
    }
}

// MARK: - Motion

/// Milliseconds on the animation clock.
func nowMs() -> Double { CACurrentMediaTime() * 1000 }

func reduceMotion() -> Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

enum Ease {
    case linear
    case bezier(Double, Double, Double, Double)

    static let cssEase = Ease.bezier(0.25, 0.1, 0.25, 1)
    static let easeOut = Ease.bezier(0, 0, 0.58, 1)
    static let easeInOut = Ease.bezier(0.42, 0, 0.58, 1)

    func apply(_ t: Double) -> Double {
        let t = min(max(t, 0), 1)
        switch self {
        case .linear: return t
        case let .bezier(x1, y1, x2, y2): return cubicBezier(x1, y1, x2, y2, t)
        }
    }
}

/// CSS `cubic-bezier()`, solved like WebKit: Newton-Raphson, then bisection.
func cubicBezier(_ x1: Double, _ y1: Double, _ x2: Double, _ y2: Double, _ t: Double) -> Double {
    if t <= 0 { return 0 }
    if t >= 1 { return 1 }
    let cx = 3 * x1, bx = 3 * (x2 - x1) - cx, ax = 1 - cx - bx
    let cy = 3 * y1, by = 3 * (y2 - y1) - cy, ay = 1 - cy - by
    func sx(_ s: Double) -> Double { ((ax * s + bx) * s + cx) * s }
    func sy(_ s: Double) -> Double { ((ay * s + by) * s + cy) * s }
    func dx(_ s: Double) -> Double { (3 * ax * s + 2 * bx) * s + cx }
    var s = t
    for _ in 0..<8 {
        let x = sx(s) - t
        if abs(x) < 1e-7 { return sy(s) }
        let d = dx(s)
        if abs(d) < 1e-6 { break }
        s -= x / d
    }
    var lo = 0.0, hi = 1.0
    s = t
    for _ in 0..<40 {
        let x = sx(s)
        if abs(x - t) < 1e-7 { break }
        if t > x { lo = s } else { hi = s }
        s = (lo + hi) / 2
    }
    return sy(s)
}

/// framer-motion's spring; `damping` is the coefficient, not a ratio.
struct Spring {
    var stiffness: Double
    var damping: Double
    var mass: Double = 1
    var restSpeed: Double? = nil

    /// Every button, card and row press: 1500 / 60.
    static let press = Spring(stiffness: 1500, damping: 60)
    /// Route and Settings sub-page slides: 460 / 36 / 0.7.
    static let route = Spring(stiffness: 460, damping: 36, mass: 0.7)

    /// Position and velocity (units/s) `tMs` after starting at `from` with `v0`.
    func at(from: Double, to: Double, v0: Double, tMs: Double) -> (Double, Double) {
        let k = stiffness, c = damping, m = mass
        let t = max(tMs / 1000, 0)
        let x0 = from - to
        let omega = (k / m).squareRoot()
        let zeta = c / (2 * (k * m).squareRoot())
        var x: Double, v: Double
        if zeta < 1 {
            let wd = omega * (1 - zeta * zeta).squareRoot()
            let a = x0, b = (v0 + zeta * omega * x0) / wd
            let env = exp(-zeta * omega * t)
            let s = sin(wd * t), co = cos(wd * t)
            x = env * (a * co + b * s)
            v = env * ((b * wd - zeta * omega * a) * co - (a * wd + zeta * omega * b) * s)
        } else if abs(zeta - 1) < 1e-9 {
            let env = exp(-omega * t)
            let b = v0 + omega * x0
            x = env * (x0 + b * t)
            v = env * (b - omega * (x0 + b * t))
        } else {
            let r = (zeta * zeta - 1).squareRoot()
            let s1 = -omega * (zeta - r), s2 = -omega * (zeta + r)
            let c2 = (v0 - s1 * x0) / (s2 - s1)
            let c1 = x0 - c2
            x = c1 * exp(s1 * t) + c2 * exp(s2 * t)
            v = c1 * s1 * exp(s1 * t) + c2 * s2 * exp(s2 * t)
        }
        return (to + x, v)
    }
}

enum Curve {
    case spring(Spring)
    case tween(Double, Ease)
}

/// One animated value, retargetable mid-flight (`motion::Anim`).
struct Anim {
    private var from: Double
    private(set) var to: Double
    private var v0 = 0.0
    private var start = 0.0
    private var delay = 0.0
    private var curve = Curve.tween(0, .linear)
    private var settled = true
    private var revStart: Double
    private var revFactor = 1.0

    init(_ value: Double) {
        from = value
        to = value
        revStart = value
    }

    static func fromTo(_ a: Double, _ b: Double, _ curve: Curve, delay: Double = 0) -> Anim {
        var x = Anim(a)
        x.set(b, curve, delay: delay)
        return x
    }

    mutating func set(_ target: Double, _ curve: Curve, delay: Double = 0) {
        if abs(target - to) < .ulpOfOne { return }
        let t = nowMs()
        let (value, velocity) = sample(t)
        from = value
        to = target
        v0 = velocity
        start = t
        self.delay = delay
        self.curve = curve
        settled = false
        revStart = value
        revFactor = 1
        if case .spring = curve, reduceMotion() { jump(target) }
    }

    /// A CSS `transition` retarget, with the reversing shortening factor.
    mutating func setCSS(_ target: Double, _ durationMs: Double, _ ease: Ease) {
        if abs(target - to) < .ulpOfOne { return }
        let t = nowMs()
        let running = isAnimating
        let (value, _) = sample(t)
        var factor = 1.0
        var newRevStart = value
        if running, abs(target - revStart) < 1e-6, case let .tween(d, e) = curve {
            let elapsed = max(t - start - delay, 0)
            let p = d > 0 ? min(elapsed / d, 1) : 1
            factor = min(max(abs(e.apply(p) * revFactor + 1 - revFactor), 0), 1)
            newRevStart = to
        }
        from = value
        to = target
        v0 = 0
        start = t
        delay = 0
        curve = .tween(durationMs * factor, ease)
        settled = false
        revStart = newRevStart
        revFactor = factor
    }

    mutating func jump(_ value: Double) { self = Anim(value) }

    private func sample(_ t: Double) -> (Double, Double) {
        if settled { return (to, 0) }
        let elapsed = t - start - delay
        if elapsed <= 0 { return (from, delay > 0 ? 0 : v0) }
        switch curve {
        case let .spring(sp):
            return sp.at(from: from, to: to, v0: v0, tMs: elapsed)
        case let .tween(d, ease):
            if d <= 0 || elapsed >= d { return (to, 0) }
            let value = from + (to - from) * ease.apply(elapsed / d)
            let v2 = from + (to - from) * ease.apply(min((elapsed + 1) / d, 1))
            return (value, (v2 - value) * 1000)
        }
    }

    var value: Double { sample(nowMs()).0 }
    var v: CGFloat { CGFloat(value) }

    var isAnimating: Bool {
        if settled { return false }
        let elapsed = nowMs() - start - delay
        if elapsed < 0 { return true }
        switch curve {
        case let .tween(d, _):
            return elapsed < d
        case let .spring(sp):
            let (x, v) = sp.at(from: from, to: to, v0: v0, tMs: elapsed)
            let granular = abs(to - from) < 5
            let restSpeed = sp.restSpeed ?? (granular ? 0.01 : 2)
            let restDelta = granular ? 0.005 : 0.5
            return !(abs(v) <= restSpeed && abs(to - x) <= restDelta) && elapsed < 10_000
        }
    }

    /// Settle once finished; returns whether it's still moving.
    mutating func tick() -> Bool {
        let moving = isAnimating
        if !moving && !settled {
            settled = true
            from = to
            v0 = 0
        }
        return moving
    }
}

/// Something that animates: ticked every frame while it says it's moving.
protocol Animating: AnyObject {
    /// Advance; return whether anything is still moving.
    func animationFrame() -> Bool
}

/// One frame clock for the whole pane (120 Hz, in every run-loop mode so it
/// keeps going while a mouse button is held).
final class Animator {
    static let shared = Animator()
    private var clients: [ObjectIdentifier: () -> Bool] = [:]
    private var timer: Timer?

    func run(_ client: Animating) {
        let id = ObjectIdentifier(client)
        clients[id] = { [weak client] in client?.animationFrame() ?? false }
        if timer == nil {
            let t = Timer(timeInterval: 1.0 / 120, repeats: true) { [weak self] _ in self?.frame() }
            RunLoop.main.add(t, forMode: .common)
            timer = t
        }
    }

    private func frame() {
        for (id, f) in clients where !f() {
            clients.removeValue(forKey: id)
        }
        if clients.isEmpty {
            timer?.invalidate()
            timer = nil
        }
    }
}

// MARK: - Squircle

private struct Seg {
    var c1: CGPoint
    var c2: CGPoint
    var to: CGPoint
}

private struct Corner {
    var a, b, c, d, p, arc, r: CGFloat
}

/// figma-squircle `getPathParamsForCorner`.
private func cornerParams(_ radius: CGFloat, _ smoothing: CGFloat, _ budget: CGFloat) -> Corner {
    let r = max(min(radius, budget), 0)
    var s = smoothing
    var p = (1 + s) * r
    if r > 0 {
        s = max(min(s, budget / r - 1), 0)
        p = min(p, budget)
    }
    let rad = { (deg: CGFloat) in deg * .pi / 180 }
    let arcMeasure = 90 * (1 - s)
    let arc = sin(rad(arcMeasure / 2)) * r * CGFloat(2).squareRoot()
    let alpha = (90 - arcMeasure) / 2
    let p3ToP4 = r * tan(rad(alpha / 2))
    let beta = 45 * s
    let c = p3ToP4 * cos(rad(beta))
    let d = c * tan(rad(beta))
    let b = (p - arc - c - d) / 3
    return Corner(a: 2 * b, b: b, c: c, d: d, p: p, arc: arc, r: r)
}

private func arcSeg(_ c: CGPoint, _ r: CGFloat, _ a0: CGFloat, _ a1: CGFloat) -> Seg {
    let k = 4.0 / 3.0 * tan((a1 - a0) / 4)
    let s0 = sin(a0), c0 = cos(a0), s1 = sin(a1), c1 = cos(a1)
    let p3 = CGPoint(x: c.x + r * c1, y: c.y + r * s1)
    return Seg(c1: CGPoint(x: c.x + r * c0 - k * r * s0, y: c.y + r * s0 + k * r * c0),
               c2: CGPoint(x: p3.x + k * r * s1, y: p3.y - k * r * c1), to: p3)
}

private func topRight(_ w: CGFloat, _ k: Corner) -> (CGPoint, [Seg]) {
    let start = CGPoint(x: w - k.p, y: 0)
    if k.r <= 0 { return (start, [Seg(c1: start, c2: CGPoint(x: w, y: 0), to: CGPoint(x: w, y: k.p))]) }
    let e1 = CGPoint(x: start.x + k.a + k.b + k.c, y: k.d)
    let s1 = Seg(c1: CGPoint(x: start.x + k.a, y: 0), c2: CGPoint(x: start.x + k.a + k.b, y: 0), to: e1)
    let centre = CGPoint(x: w - k.r, y: k.r)
    let a0 = atan2(e1.y - centre.y, e1.x - centre.x)
    let e2 = CGPoint(x: e1.x + k.arc, y: e1.y + k.arc)
    let a1 = atan2(e2.y - centre.y, e2.x - centre.x)
    let s2 = arcSeg(centre, k.r, a0, a1)
    let s3 = Seg(c1: CGPoint(x: e2.x + k.d, y: e2.y + k.c), c2: CGPoint(x: e2.x + k.d, y: e2.y + k.b + k.c),
                 to: CGPoint(x: w, y: k.p))
    return (start, [s1, s2, s3])
}

private func reversed(_ start: CGPoint, _ segs: [Seg]) -> (CGPoint, [Seg]) {
    var points = [start]
    for s in segs { points.append(s.to) }
    var out: [Seg] = []
    for i in stride(from: segs.count - 1, through: 0, by: -1) {
        out.append(Seg(c1: segs[i].c2, c2: segs[i].c1, to: points[i]))
    }
    return (points[points.count - 1], out)
}

/// Append a squircle in `rect` (y-down), scaled by `scale` about its centre.
func addSquircle(_ path: NSBezierPath, _ rect: CGRect, radius: CGFloat, smoothing: CGFloat = 0.7, scale: CGFloat = 1) {
    let w = rect.width, h = rect.height
    guard w > 0, h > 0 else { return }
    let k = cornerParams(radius, smoothing, min(w, h) / 2)
    let (trStart, tr) = topRight(w, k)
    func reflect(_ segs: [Seg], _ start: CGPoint, _ f: (CGPoint) -> CGPoint) -> (CGPoint, [Seg]) {
        reversed(f(start), segs.map { Seg(c1: f($0.c1), c2: f($0.c2), to: f($0.to)) })
    }
    let (brStart, br) = reflect(tr, trStart) { CGPoint(x: $0.x, y: h - $0.y) }
    let (blStart, bl) = reflect(br, brStart) { CGPoint(x: w - $0.x, y: $0.y) }
    let (tlStart, tl) = reflect(tr, trStart) { CGPoint(x: w - $0.x, y: $0.y) }
    let cx = w / 2, cy = h / 2
    func xf(_ q: CGPoint) -> CGPoint {
        CGPoint(x: rect.minX + cx + (q.x - cx) * scale, y: rect.minY + cy + (q.y - cy) * scale)
    }
    path.move(to: xf(trStart))
    for (start, segs) in [(trStart, tr), (brStart, br), (blStart, bl), (tlStart, tl)] {
        path.line(to: xf(start))
        for s in segs { path.curve(to: xf(s.to), controlPoint1: xf(s.c1), controlPoint2: xf(s.c2)) }
    }
    path.close()
}

/// `paint_squircle_bg`: fill plus a 1px inner border, scaled about the centre.
func paintSquircle(_ rect: CGRect, radius: CGFloat, scale: CGFloat, fill: NSColor, border: NSColor) {
    let hasBorder = border.alphaComponent > 0.001
    func scaled(_ inner: CGRect) -> CGRect {
        let c = CGPoint(x: rect.midX, y: rect.midY)
        return CGRect(x: c.x + (inner.minX - c.x) * scale, y: c.y + (inner.minY - c.y) * scale,
                      width: inner.width * scale, height: inner.height * scale)
    }
    if fill.alphaComponent > 0.001 {
        let inner = hasBorder ? rect.insetBy(dx: 1, dy: 1) : rect
        let p = NSBezierPath()
        addSquircle(p, scaled(inner), radius: hasBorder ? max(radius - 1, 0) : radius)
        fill.setFill()
        p.fill()
    }
    if hasBorder {
        let p = NSBezierPath()
        p.windingRule = .evenOdd
        addSquircle(p, scaled(rect), radius: radius)
        addSquircle(p, scaled(rect.insetBy(dx: 1, dy: 1)), radius: max(radius - 1, 0))
        border.setFill()
        p.fill()
    }
}

/// A CSS `border-radius` box, optionally scaled about its centre.
func roundedRect(_ rect: CGRect, _ radius: CGFloat, scale: CGFloat = 1) -> NSBezierPath {
    let c = CGPoint(x: rect.midX, y: rect.midY)
    let r = CGRect(x: c.x - rect.width * scale / 2, y: c.y - rect.height * scale / 2,
                   width: rect.width * scale, height: rect.height * scale)
    return NSBezierPath(roundedRect: r, xRadius: radius * scale, yRadius: radius * scale)
}

/// A CSS box: fill and a 1px border inside `rect`.
func paintBox(_ rect: CGRect, radius: CGFloat, fill: NSColor, border: NSColor?, borderWidth: CGFloat = 1) {
    if fill.alphaComponent > 0.001 {
        fill.setFill()
        NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
    }
    if let border = border, border.alphaComponent > 0.001 {
        let half = borderWidth / 2
        let p = NSBezierPath(roundedRect: rect.insetBy(dx: half, dy: half), xRadius: radius - half, yRadius: radius - half)
        p.lineWidth = borderWidth
        border.setStroke()
        p.stroke()
    }
}

// MARK: - Icons

enum Icon: String {
    case funnelFill = "funnel-fill"
    case linkBold = "link-bold"
    case wrenchFill = "wrench-fill"
    case power = "lk-power"
    case play = "lk-play"
    case infoFill = "info-fill"
    case caretRight = "caret-right-bold"
    case checkBold = "check-bold"

    private static var cache: [String: NSImage] = [:]
    static weak var bundle: Bundle?

    private var image: NSImage? {
        if let i = Icon.cache[rawValue] { return i }
        guard let url = Icon.bundle?.url(forResource: rawValue, withExtension: "pdf", subdirectory: "icons"),
              let i = NSImage(contentsOf: url) else { return nil }
        Icon.cache[rawValue] = i
        return i
    }

    /// Draw tinted with `color` (GPUI tints SVGs by their alpha the same way).
    func draw(in rect: CGRect, color: NSColor, alpha: CGFloat = 1) {
        guard let base = image else { return }
        let tinted = NSImage(size: rect.size, flipped: false) { r in
            base.draw(in: r)
            color.set()
            r.fill(using: .sourceAtop)
            return true
        }
        tinted.draw(in: rect, from: .zero, operation: .sourceOver, fraction: alpha, respectFlipped: true, hints: nil)
    }
}

/// A view in web coordinates (y grows down).
class FlippedView: NSView {
    override var isFlipped: Bool { true }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}
