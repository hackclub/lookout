// Load a built Lookout.prefPane the way System Settings does (NSBundle →
// principal class → loadMainView), let it fetch Lookout's state through the
// real helper, and save each page as a PNG in light and dark, so CI proves
// the bundle loads and shows what it looks like.
//
//   swift packaging/macos/prefpane-check.swift <Lookout.prefPane> <out-dir> [pages...]
//
// Pages: menu filtered-apps linked-programs advanced (default: all).

import AppKit
import PreferencePanes

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write("prefpane-check: \(msg)\n".data(using: .utf8)!)
    exit(1)
}

let args = CommandLine.arguments
guard args.count >= 3 else { fail("usage: prefpane-check.swift <Lookout.prefPane> <out-dir> [pages...]") }
let outDir = URL(fileURLWithPath: args[2], isDirectory: true)
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
let pages = args.count > 3 ? Array(args[3...]) : ["menu", "filtered-apps", "linked-programs", "advanced"]

_ = NSApplication.shared
NSApp.setActivationPolicy(.accessory)
guard let bundle = Bundle(url: URL(fileURLWithPath: args[1])) else { fail("not a bundle") }
guard bundle.load() else { fail("the bundle didn't load") }
guard let cls = bundle.principalClass, cls is NSPreferencePane.Type else { fail("no NSPreferencePane principal class") }

/// `[[cls alloc] initWithBundle:]`: NSPreferencePane's initializer isn't
/// `required`, so Swift can't call it through a metatype.
func makePane() -> NSPreferencePane {
    let allocated = (cls as AnyObject).perform(NSSelectorFromString("alloc"))!.takeUnretainedValue()
    guard let pane = allocated.perform(NSSelectorFromString("initWithBundle:"), with: bundle)?.takeUnretainedValue()
        as? NSPreferencePane else { fail("initWithBundle: failed") }
    return pane
}

func snapshot(_ view: NSView, _ appearance: NSAppearance.Name, to url: URL) {
    let window = NSWindow(contentRect: view.frame, styleMask: [.titled], backing: .buffered, defer: false)
    window.appearance = NSAppearance(named: appearance)
    window.contentView = view
    view.layoutSubtreeIfNeeded()
    // Let the pane ask Lookout for its state and lay out the reply.
    RunLoop.main.run(until: Date().addingTimeInterval(4))
    view.layoutSubtreeIfNeeded()
    guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { fail("no bitmap") }
    view.cacheDisplay(in: view.bounds, to: rep)
    let out = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: rep.pixelsWide, pixelsHigh: rep.pixelsHigh,
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: out)
    // System Settings' window background, which the pane sits on, resolved
    // and filled in the pane's appearance (a dynamic colour otherwise
    // resolves light in this bitmap context).
    window.effectiveAppearance.performAsCurrentDrawingAppearance {
        NSColor.windowBackgroundColor.setFill()
        NSRect(x: 0, y: 0, width: rep.pixelsWide, height: rep.pixelsHigh).fill()
    }
    rep.draw(in: NSRect(x: 0, y: 0, width: rep.pixelsWide, height: rep.pixelsHigh))
    NSGraphicsContext.restoreGraphicsState()
    guard let png = out.representation(using: .png, properties: [:]) else { fail("no PNG") }
    try! png.write(to: url)
    window.contentView = nil
    print("prefpane-check: wrote \(url.path)")
}

for page in pages {
    for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", NSAppearance.Name.darkAqua)] {
        setenv("LOOKOUT_PANE_PAGE", page, 1)
        let pane = makePane()
        let view = pane.loadMainView()
        guard view.frame.width > 0, view.frame.height > 40 else { fail("empty view") }
        pane.willSelect()
        pane.didSelect()
        snapshot(view, appearance, to: outDir.appendingPathComponent("prefpane-\(page)-\(name).png"))
        pane.willUnselect()
        pane.didUnselect()
    }
}
print("prefpane-check: \(String(describing: cls)) loaded and drew \(pages.count * 2) pages")
