//! Where the text cursor is in the frontmost editor: line and column,
//! sampled with the rest of the interaction metadata (activity.rs). Only
//! the two numbers leave the machine — never the text, never the image.
//!
//! Editors fall into two kinds, read two ways:
//!
//! - Editors built on the platform's text views (Xcode, TextEdit, BBEdit,
//!   Notepad, Visual Studio…) answer the accessibility APIs: the caret's
//!   offset and the text before it give the line and column exactly
//!   (`ax` on macOS, `uia` on Windows). Scintilla editors (Notepad++) are
//!   asked directly (`scintilla`).
//! - Editors that draw their own text — VS Code and its forks, JetBrains
//!   IDEs, Sublime Text, Zed — either answer nothing or, for the Chromium
//!   based ones, switch into screen-reader mode when asked (VS Code turns
//!   on its accessibility support and changes how it behaves). They are
//!   never queried. Every one of them shows the caret's line and column in
//!   its status bar, so that strip of the window (and only that strip) is
//!   read with the platform's own on-device text recognition (`ocr`):
//!   Vision on macOS, Windows.Media.Ocr on Windows.
//!
//! The "Prefer accessibility" setting ([`set_prefer_accessibility`]) trades
//! that away for VS Code and its forks: Lookout switches their
//! accessibility on and reads the cursor-position item's own label from the
//! status bar (`ax-status` / `uia-status`) — exact, no OCR — at the cost of
//! VS Code noticing a screen reader (it may offer, or switch to, its
//! screen-reader-optimised mode). OCR remains the fallback when the label
//! can't be found.
//!
//! Browsers are left alone too: asking Chromium or Gecko for the focused
//! element turns on their accessibility engines for the whole browser.
//!
//! Linux reads nothing: AT-SPI would have to be switched on for the whole
//! desktop (and flips Electron apps into screen-reader mode the same way),
//! and there is no built-in OCR.

// Linux reads no caret, so the shared parsing goes unused there.
#![cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use regex::Regex;

static PREFER_ACCESSIBILITY: AtomicBool = AtomicBool::new(false);

/// The "Prefer accessibility" setting: read VS Code's (and its forks') line
/// and column through accessibility instead of OCR, switching their
/// accessibility support on to do it. Off by default. Takes effect at the
/// next sample; turning it off hands the apps' accessibility back (macOS —
/// on Windows Chromium keeps it on until the app restarts).
pub fn set_prefer_accessibility(on: bool) {
    PREFER_ACCESSIBILITY.store(on, Ordering::Relaxed);
}

pub(crate) fn prefer_accessibility() -> bool {
    PREFER_ACCESSIBILITY.load(Ordering::Relaxed)
}

/// The id VS Code gives its cursor-position status bar item (the forks
/// keep it). Chromium exposes it as the element's `AXDOMIdentifier`
/// (macOS) and UI Automation `AutomationId` (Windows).
pub(crate) const VSCODE_POSITION_ID: &str = "status.editor.selection";

/// Recognised text below this confidence (0–1) is ignored: a misread digit
/// is worse than no reading.
const MIN_CONFIDENCE: f32 = 0.5;

/// Editors whose line and column are read off their status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Editor {
    /// VS Code and its forks: `Ln 12, Col 5` (localised).
    VsCode,
    /// JetBrains IDEs and Zed: `12:5`.
    LineColon,
    /// Sublime Text: `Line 12, Column 5`.
    Sublime,
    /// Notepad++ (when its Scintilla can't be reached): `Ln : 12  Col : 5`.
    NotepadPlusPlus,
}

/// The status bar strip read for line and column, in logical points (CSS
/// pixels on Windows at 100%). Every supported editor's status bar fits in
/// it (VS Code's is 22, JetBrains' about 30), with room for zoom.
pub(crate) const STRIP_POINTS: f64 = 40.0;

/// Text in the strip's top part is the editor or a panel above the status
/// bar, not the status bar: a match there is ignored.
const STATUS_BAR_FROM: f64 = 0.45;

/// The editor an app id (bundle id, exe name, WM_CLASS) is, if its line and
/// column come from its status bar.
pub(crate) fn ocr_editor(app: &str) -> Option<Editor> {
    let a = app.to_ascii_lowercase();
    let a = a.strip_suffix(".exe").unwrap_or(&a);
    const VSCODE: &[&str] = &[
        // macOS bundle ids
        "com.microsoft.vscode",
        "com.microsoft.vscodeinsiders",
        "com.vscodium",
        "com.vscodium.vscodiuminsiders",
        "com.todesktop.230313mzl4w4u92", // Cursor
        "com.exafunction.windsurf",
        "com.trae.app",
        "com.google.antigravity",
        "com.kiro.app",
        "dev.kiro.desktop",
        // Windows exe names, Linux WM_CLASS
        "code",
        "code - insiders",
        "code-insiders",
        "vscodium",
        "codium",
        "cursor",
        "windsurf",
        "trae",
        "antigravity",
        "kiro",
    ];
    const JETBRAINS_EXES: &[&str] = &[
        "idea", "idea64", "pycharm", "pycharm64", "webstorm", "webstorm64", "clion", "clion64", "rider",
        "rider64", "goland", "goland64", "phpstorm", "phpstorm64", "rubymine", "rubymine64", "rustrover",
        "rustrover64", "datagrip", "datagrip64", "dataspell", "dataspell64", "aqua", "aqua64", "writerside",
        "writerside64", "studio", "studio64", "android studio",
    ];
    if VSCODE.contains(&a) {
        return Some(Editor::VsCode);
    }
    if a.starts_with("com.jetbrains.")
        || a.starts_with("jetbrains-")
        || a == "com.google.android.studio"
        || a == "com.google.android.studio-eap"
        || JETBRAINS_EXES.contains(&a)
        || a.starts_with("dev.zed.zed")
        || a == "zed"
        || a == "zed-editor"
        || a == "dev.zed.zed-preview"
    {
        return Some(Editor::LineColon);
    }
    if a.starts_with("com.sublimetext.") || a == "sublime_text" || a == "sublime-text" {
        return Some(Editor::Sublime);
    }
    if a == "notepad++" {
        return Some(Editor::NotepadPlusPlus);
    }
    None
}

/// Terminals expose their scrollback as one big text area: a "line" there
/// is a place in the scrollback, not in a file, and reading it all back
/// every sample is expensive. (Vim's own ruler isn't read either.)
pub(crate) fn is_terminal(app: &str) -> bool {
    let a = app.to_ascii_lowercase();
    let a = a.strip_suffix(".exe").unwrap_or(&a);
    matches!(
        a,
        "com.apple.terminal"
            | "com.googlecode.iterm2"
            | "windowsterminal"
            | "openconsole"
            | "conhost"
            | "cmd"
            | "powershell"
            | "pwsh"
            | "wsl"
            | "wslhost"
            | "mintty"
    )
}

/// One run of recognised text in the strip. `y` and `x` are its centre,
/// as fractions of the strip (0 = top / left).
#[derive(Clone, Debug)]
pub(crate) struct TextRun {
    pub text: String,
    pub x: f64,
    pub y: f64,
    /// The recogniser's confidence, 0–1 (1 when it doesn't say).
    pub confidence: f32,
}

static VSCODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    // "Ln 12, Col 5" and its translations ("Zeile 12, Spalte 5",
    // "行 12，列 5"): a short word, a number, a comma, a short word, a
    // number. Nothing else in VS Code's status bar has that shape.
    Regex::new(r"(?:^|[^\p{L}\d])\p{L}[\p{L}.]{0,7}\s*(\d{1,7})\s*[,，、]\s*\p{L}[\p{L}.]{0,7}\s*(\d{1,5})").unwrap()
});
static COLON_RE: LazyLock<Regex> = LazyLock::new(|| {
    // "12:5", not part of a time ("10:42:07") or a path ("main.rs:12:5").
    Regex::new(r"(?:^|[^\d:.\w])(\d{1,7})\s?:\s?(\d{1,5})(?:$|[^\d:])").unwrap()
});
static SUBLIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Line\s*(\d{1,7}),\s*Column\s*(\d{1,5})").unwrap());
static NPP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Ln\s*:\s*(\d{1,7})\s+Col\s*:\s*(\d{1,5})").unwrap());

/// Line and column from what the strip says: the lowest match in the
/// status bar part of the strip, rightmost on ties (where the editors put
/// it). 1-based; a 0 is a misread. Nothing when the recogniser wasn't sure,
/// or when the status bar shows two different positions (one is wrong, and
/// there's no telling which).
pub(crate) fn parse(editor: Editor, runs: &[TextRun]) -> Option<(u32, u32)> {
    let re: &Regex = match editor {
        Editor::VsCode => &VSCODE_RE,
        Editor::LineColon => &COLON_RE,
        Editor::Sublime => &SUBLIME_RE,
        Editor::NotepadPlusPlus => &NPP_RE,
    };
    let found: Vec<(&TextRun, (u32, u32))> = runs
        .iter()
        .filter(|r| r.y >= STATUS_BAR_FROM && r.confidence >= MIN_CONFIDENCE)
        .flat_map(|r| {
            re.captures_iter(&r.text).filter_map(move |c| {
                let line: u32 = c.get(1)?.as_str().parse().ok()?;
                let col: u32 = c.get(2)?.as_str().parse().ok()?;
                (line > 0 && col > 0).then_some((r, (line, col)))
            })
        })
        .collect();
    // Same row of text if within a tenth of the strip.
    let same_row = |a: &TextRun, b: &TextRun| (a.y - b.y).abs() <= 0.1;
    let (best, lc) = found.iter().copied().max_by(|(a, _), (b, _)| {
        if same_row(a, b) {
            a.x.total_cmp(&b.x)
        } else {
            a.y.total_cmp(&b.y)
        }
    })?;
    let ambiguous = found.iter().any(|(r, other)| *other != lc && same_row(r, best));
    (!ambiguous).then_some(lc)
}

/// Line and column from an accessibility label ("Ln 12, Col 5") and the
/// labels around it.
pub(crate) fn parse_labels(editor: Editor, labels: &[String]) -> Option<(u32, u32)> {
    let runs: Vec<TextRun> = labels
        .iter()
        .map(|t| TextRun { text: t.clone(), x: 1.0, y: 1.0, confidence: 1.0 })
        .collect();
    parse(editor, &runs)
}

/// The caret's line and column given all the text before it: 1-based, the
/// column in characters. `\r\n`, `\n` and a lone `\r` each end a line.
pub(crate) fn position_after(prefix: &str) -> (u32, u32) {
    let mut line = 1u32;
    let mut col = 1u32;
    let mut chars = prefix.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line = line.saturating_add(1);
                col = 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => {
                line = line.saturating_add(1);
                col = 1;
            }
            _ => col = col.saturating_add(1),
        }
    }
    (line, col)
}

// ── macOS ───────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub(crate) mod platform {
    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::Mutex;

    use lookout_recorder::host::CaretSample;
    use objc2::rc::autoreleasepool;
    use objc2::AllocAnyThread;
    use objc2_core_foundation::{CFString, CGPoint, CGRect, CGSize};
    // The one-window, one-strip grab ScreenCaptureKit has no cheap match for.
    #[allow(deprecated)]
    use objc2_core_graphics::{CGWindowImageOption, CGWindowListCreateImage, CGWindowListOption};
    use objc2_foundation::{NSArray, NSDictionary};
    use objc2_vision::{
        VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
    };

    use super::{
        is_terminal, ocr_editor, parse, parse_labels, position_after, prefer_accessibility, Editor, TextRun,
        STRIP_POINTS, VSCODE_POSITION_ID,
    };
    use crate::activity::Rect;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    /// Longest text before the caret read to count lines (UTF-16 units): about
    /// 4 MB, far past any hand-edited file; a caret deeper than that reads
    /// nothing rather than stall the sampler.
    const MAX_PREFIX: isize = 2_000_000;

    /// `kAXValueCFRangeType`.
    const AX_VALUE_CFRANGE: u32 = 4;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
        fn AXUIElementSetMessagingTimeout(element: *const c_void, timeout_seconds: f32) -> i32;
        fn AXUIElementCopyAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            value: *mut *const c_void,
        ) -> i32;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            parameter: *const c_void,
            value: *mut *const c_void,
        ) -> i32;
        fn AXValueGetValue(value: *const c_void, the_type: u32, out: *mut c_void) -> bool;
        fn AXValueCreate(the_type: u32, value: *const c_void) -> *const c_void;
        fn AXUIElementSetAttributeValue(element: *const c_void, attribute: *const c_void, value: *const c_void) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFRetain(cf: *const c_void) -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        static kCFBooleanTrue: *const c_void;
        static kCFBooleanFalse: *const c_void;
    }

    /// A CF object released when dropped.
    struct Owned(*const c_void);

    // AX elements are plain CF objects, usable from any thread.
    unsafe impl Send for Owned {}

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn copy_attr(element: *const c_void, name: &str) -> Option<Owned> {
        let attr = CFString::from_str(name);
        let mut out: *const c_void = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(element, (&*attr as *const CFString).cast(), &mut out) };
        (err == 0 && !out.is_null()).then_some(Owned(out))
    }

    fn cf_string(value: &Owned) -> String {
        unsafe { &*(value.0 as *const CFString) }.to_string()
    }

    /// Bundles that embed a browser engine: Chromium (Chrome, Edge, Arc,
    /// Brave, every Electron and CEF app) or Gecko. Asking them for the
    /// focused element switches their accessibility engine on.
    fn embeds_browser_engine(bundle: &Path) -> bool {
        let frameworks = bundle.join("Contents/Frameworks");
        if let Ok(entries) = std::fs::read_dir(&frameworks) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.contains("electron") || name.contains("chrom") || name.contains("cef") {
                    return true;
                }
            }
        }
        let macos = bundle.join("Contents/MacOS");
        macos.join("XUL").exists()
            || macos.join("libxul.dylib").exists()
            || bundle.join("Contents/Resources/omni.ja").exists()
    }

    /// `embeds_browser_engine`, remembered per bundle (a directory read per
    /// sample otherwise).
    fn web_engine(bundle: &Path) -> bool {
        static SEEN: Mutex<Vec<(std::path::PathBuf, bool)>> = Mutex::new(Vec::new());
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, v)) = seen.iter().find(|(p, _)| p == bundle) {
            return *v;
        }
        let v = embeds_browser_engine(bundle);
        if seen.len() > 64 {
            seen.clear();
        }
        seen.push((bundle.to_path_buf(), v));
        v
    }

    /// The caret in `pid`'s focused text area, by Accessibility.
    fn ax_caret(pid: i32) -> Option<CaretSample> {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return None;
        }
        // A hung app must not hang the sampler.
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
        let focused = copy_attr(app.0, "AXFocusedUIElement")?;
        // Multi-line text only: a search field's "line 1" says nothing.
        if cf_string(&copy_attr(focused.0, "AXRole")?) != "AXTextArea" {
            return None;
        }
        let selection = copy_attr(focused.0, "AXSelectedTextRange")?;
        let mut range = CFRange::default();
        let ok = unsafe { AXValueGetValue(selection.0, AX_VALUE_CFRANGE, (&mut range as *mut CFRange).cast()) };
        if !ok || range.location < 0 || range.location > MAX_PREFIX {
            return None;
        }
        let (line, column) = if range.location == 0 {
            (1, 1)
        } else {
            let before = CFRange { location: 0, length: range.location };
            let param = Owned(unsafe { AXValueCreate(AX_VALUE_CFRANGE, (&before as *const CFRange).cast()) });
            if param.0.is_null() {
                return None;
            }
            let attr = CFString::from_str("AXStringForRange");
            let mut out: *const c_void = std::ptr::null();
            let err = unsafe {
                AXUIElementCopyParameterizedAttributeValue(
                    focused.0,
                    (&*attr as *const CFString).cast(),
                    param.0,
                    &mut out,
                )
            };
            if err != 0 || out.is_null() {
                return None;
            }
            position_after(&cf_string(&Owned(out)))
        };
        Some(CaretSample { line, column, via: "ax" })
    }

    fn string_attr(element: *const c_void, name: &str) -> Option<String> {
        let v = copy_attr(element, name)?;
        (unsafe { CFGetTypeID(v.0) == CFStringGetTypeID() }).then(|| cf_string(&v))
    }

    fn children(element: *const c_void) -> Vec<Owned> {
        let Some(array) = copy_attr(element, "AXChildren") else {
            return Vec::new();
        };
        let n = unsafe { CFArrayGetCount(array.0) }.clamp(0, 4096);
        (0..n)
            .filter_map(|i| {
                let v = unsafe { CFArrayGetValueAtIndex(array.0, i) };
                (!v.is_null()).then(|| Owned(unsafe { CFRetain(v) }))
            })
            .collect()
    }

    /// An element's own label and its children's (the item's text is often
    /// a child static text).
    fn labels(element: *const c_void) -> Vec<String> {
        let read = |e: *const c_void, out: &mut Vec<String>| {
            for name in ["AXTitle", "AXDescription", "AXValue"] {
                if let Some(s) = string_attr(e, name).filter(|s| !s.is_empty() && s.len() < 200) {
                    out.push(s);
                }
            }
        };
        let mut out = Vec::new();
        read(element, &mut out);
        for child in children(element).iter().take(8) {
            read(child.0, &mut out);
        }
        out
    }

    /// Apps whose accessibility we switched on, to hand back when the
    /// setting goes off.
    static SWITCHED_ON: Mutex<Vec<i32>> = Mutex::new(Vec::new());

    /// The cursor-position item last found: its app, window and element.
    /// The status bar lives as long as the window, so a hit is one
    /// attribute read instead of a tree walk.
    static FOUND: Mutex<Option<(i32, Option<u32>, Owned)>> = Mutex::new(None);

    fn set_bool_attr(element: *const c_void, name: &str, on: bool) -> i32 {
        let attr = CFString::from_str(name);
        let value = unsafe { if on { kCFBooleanTrue } else { kCFBooleanFalse } };
        unsafe { AXUIElementSetAttributeValue(element, (&*attr as *const CFString).cast(), value) }
    }

    /// The setting went off: give every app we switched on its own
    /// accessibility setting back.
    fn hand_back_accessibility() {
        let pids: Vec<i32> = std::mem::take(&mut *SWITCHED_ON.lock().unwrap_or_else(|e| e.into_inner()));
        if pids.is_empty() {
            return;
        }
        *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
        for pid in pids {
            let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
            if !app.0.is_null() {
                unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
                set_bool_attr(app.0, "AXManualAccessibility", false);
            }
        }
    }

    /// Breadth-first search under `root` for the element with this DOM id,
    /// within a node and time budget (VS Code's tree is large).
    fn find_by_dom_id(root: Owned, id: &str) -> Option<Owned> {
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_millis(400);
        let mut queue = VecDeque::from([root]);
        let mut seen = 0usize;
        while let Some(el) = queue.pop_front() {
            seen += 1;
            if seen > 8000 || Instant::now() > deadline {
                return None;
            }
            if string_attr(el.0, "AXDOMIdentifier").as_deref() == Some(id) {
                return Some(el);
            }
            // The status bar comes last in the workbench: look there first.
            let mut kids = children(el.0);
            kids.reverse();
            queue.extend(kids);
        }
        None
    }

    /// VS Code's own line and column, from its cursor-position status bar
    /// item, with its (Electron) accessibility switched on.
    fn ax_vscode_position(pid: i32, window: Option<u32>) -> Option<(u32, u32)> {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return None;
        }
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
        {
            let mut on = SWITCHED_ON.lock().unwrap_or_else(|e| e.into_inner());
            if !on.contains(&pid) {
                // Electron's switch for assistive apps; Chromium builds the
                // tree over the next moments, so the first look may miss.
                set_bool_attr(app.0, "AXManualAccessibility", true);
                on.push(pid);
            }
        }
        let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, el)) = found.as_ref().filter(|(p, w, _)| *p == pid && *w == window) {
            if let Some(lc) = parse_labels(Editor::VsCode, &labels(el.0)) {
                return Some(lc);
            }
        }
        *found = None;
        let root = copy_attr(app.0, "AXFocusedWindow")?;
        let el = find_by_dom_id(root, VSCODE_POSITION_ID)?;
        let lc = parse_labels(Editor::VsCode, &labels(el.0));
        *found = Some((pid, window, el));
        lc
    }

    /// Recognise the text in the bottom strip of window `id` (global
    /// display points). The window's own pixels, even where something
    /// covers it.
    fn ocr_strip(id: u32, rect: Rect) -> Vec<TextRun> {
        if rect.w < 50.0 || rect.h < STRIP_POINTS * 2.0 {
            return Vec::new();
        }
        let strip = CGRect::new(
            CGPoint::new(rect.x, rect.y + rect.h - STRIP_POINTS),
            CGSize::new(rect.w, STRIP_POINTS),
        );
        #[allow(deprecated)]
        let Some(image) = CGWindowListCreateImage(
            strip,
            CGWindowListOption::OptionIncludingWindow,
            id,
            CGWindowImageOption::BoundsIgnoreFraming | CGWindowImageOption::BestResolution,
        ) else {
            return Vec::new();
        };
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        // Numbers and labels, not prose: correction would "fix" them.
        request.setUsesLanguageCorrection(false);
        let handler = unsafe {
            VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &image, &NSDictionary::new())
        };
        let as_request: &VNRequest = &request;
        if handler.performRequests_error(&NSArray::from_slice(&[as_request])).is_err() {
            return Vec::new();
        }
        let Some(results) = request.results() else {
            return Vec::new();
        };
        results
            .iter()
            .filter_map(|obs| {
                let best = obs.topCandidates(1).firstObject()?;
                // Normalised, origin bottom-left.
                let b = unsafe { obs.boundingBox() };
                Some(TextRun {
                    text: best.string().to_string(),
                    x: b.origin.x + b.size.width / 2.0,
                    y: 1.0 - (b.origin.y + b.size.height / 2.0),
                    confidence: best.confidence(),
                })
            })
            .collect()
    }

    /// The caret in the frontmost app: `pid` and its bundle, the focused
    /// window and its frame.
    pub(crate) fn read(
        pid: i32,
        app: &str,
        bundle: Option<&Path>,
        window: Option<u32>,
        rect: Option<Rect>,
    ) -> Option<CaretSample> {
        autoreleasepool(|_| {
            if !prefer_accessibility() {
                hand_back_accessibility();
            }
            if let Some(editor) = ocr_editor(app) {
                if editor == Editor::VsCode && prefer_accessibility() && unsafe { AXIsProcessTrusted() } {
                    if let Some((line, column)) = ax_vscode_position(pid, window) {
                        return Some(CaretSample { line, column, via: "ax-status" });
                    }
                }
                let runs = ocr_strip(window?, rect?);
                let (line, column) = parse(editor, &runs)?;
                return Some(CaretSample { line, column, via: "ocr" });
            }
            if is_terminal(app) || bundle.is_some_and(web_engine) || !unsafe { AXIsProcessTrusted() } {
                return None;
            }
            ax_caret(pid)
        })
    }
}

// ── Windows ─────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub(crate) mod platform {
    use lookout_recorder::host::CaretSample;
    use windows::core::Interface;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationTextPattern, IUIAutomationTextRange,
        TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Paragraph, TreeScope_Descendants,
        UIA_AutomationIdPropertyId, UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_TextPatternId,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClassNameW, GetGUIThreadInfo, GetWindowLongW, GetWindowThreadProcessId, IsIconic,
        SendMessageTimeoutW, WindowFromPoint, GA_ROOT, GUITHREADINFO, GWL_STYLE, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    };

    use super::{
        is_terminal, ocr_editor, parse, parse_labels, position_after, prefer_accessibility, Editor, TextRun,
        STRIP_POINTS, VSCODE_POSITION_ID,
    };

    fn class_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// Chromium (Chrome, Edge, every Electron app) and Gecko windows: asking
    /// them for the focused element switches their accessibility engine on.
    fn embeds_browser_engine(hwnd: HWND) -> bool {
        let class = class_of(hwnd);
        class.starts_with("Chrome_WidgetWin") || class == "MozillaWindowClass"
    }

    /// The window with the keyboard focus inside `fg`'s thread.
    fn focus_in(fg: HWND) -> Option<HWND> {
        let tid = unsafe { GetWindowThreadProcessId(fg, None) };
        let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        unsafe { GetGUIThreadInfo(tid, &mut info) }.ok()?;
        (!info.hwndFocus.is_invalid()).then_some(info.hwndFocus)
    }

    /// Send a message whose arguments and result are plain integers (safe
    /// across processes), giving up on a hung window.
    fn ask(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> Option<isize> {
        let mut result = 0usize;
        let ok = unsafe {
            SendMessageTimeoutW(
                hwnd,
                msg,
                WPARAM(wparam),
                LPARAM(lparam),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                150,
                Some(&mut result),
            )
        };
        (ok.0 != 0).then_some(result as isize)
    }

    /// A Scintilla control (Notepad++, SciTE, Notepad2): its own line and
    /// column, tabs expanded as it shows them.
    fn scintilla_caret(hwnd: HWND) -> Option<CaretSample> {
        const SCI_GETCURRENTPOS: u32 = 2008;
        const SCI_LINEFROMPOSITION: u32 = 2166;
        const SCI_GETCOLUMN: u32 = 2129;
        let pos = ask(hwnd, SCI_GETCURRENTPOS, 0, 0)?;
        let line = ask(hwnd, SCI_LINEFROMPOSITION, pos as usize, 0)?;
        let col = ask(hwnd, SCI_GETCOLUMN, pos as usize, 0)?;
        Some(CaretSample {
            line: u32::try_from(line + 1).ok()?,
            column: u32::try_from(col + 1).ok()?,
            via: "scintilla",
        })
    }

    /// COM for this (pooled) thread while it samples.
    struct ComScope(bool);

    impl ComScope {
        fn enter() -> Self {
            ComScope(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for ComScope {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Longest paragraph count walked to find the caret's line.
    const MAX_PARAGRAPHS: i32 = 500_000;

    /// The caret in the focused text control of `fg`'s process, by UI
    /// Automation: paragraphs are lines (TextUnit_Line would count wrapped
    /// rows).
    fn uia_caret(fg: HWND) -> Option<CaretSample> {
        let _com = ComScope::enter();
        unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
            if let Ok(uia2) = uia.cast::<IUIAutomation2>() {
                // A hung app must not hang the sampler.
                let _ = uia2.SetConnectionTimeout(250);
                let _ = uia2.SetTransactionTimeout(250);
            }
            let element = uia.GetFocusedElement().ok()?;
            let mut pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut pid));
            if element.CurrentProcessId().ok()? as u32 != pid {
                return None;
            }
            let control = element.CurrentControlType().ok()?;
            let pattern: IUIAutomationTextPattern = element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
            if control == UIA_EditControlTypeId {
                // Multi-line edits only: a search box's "line 1" says nothing.
                let hwnd = element.CurrentNativeWindowHandle().unwrap_or_default();
                const ES_MULTILINE: i32 = 0x0004;
                let multiline = if !hwnd.is_invalid() && class_of(hwnd).to_ascii_lowercase().contains("edit") {
                    GetWindowLongW(hwnd, GWL_STYLE) & ES_MULTILINE != 0
                } else {
                    let text = pattern.DocumentRange().ok()?.GetText(65_536).ok()?.to_string();
                    text.contains('\n') || text.contains('\r')
                };
                if !multiline {
                    return None;
                }
            } else if control != UIA_DocumentControlTypeId {
                return None;
            }
            let selection = pattern.GetSelection().ok()?;
            if selection.Length().ok()? < 1 {
                return None;
            }
            let caret: IUIAutomationTextRange = selection.GetElement(0).ok()?;
            // Collapse to where the selection starts.
            caret
                .MoveEndpointByRange(TextPatternRangeEndpoint_End, &caret, TextPatternRangeEndpoint_Start)
                .ok()?;
            let line_start = caret.Clone().ok()?;
            line_start.ExpandToEnclosingUnit(TextUnit_Paragraph).ok()?;
            line_start
                .MoveEndpointByRange(TextPatternRangeEndpoint_End, &line_start, TextPatternRangeEndpoint_Start)
                .ok()?;
            // From the start of a paragraph, moving back to the document's
            // start crosses exactly the paragraphs before it.
            let walker = line_start.Clone().ok()?;
            let moved = walker.Move(TextUnit_Paragraph, -MAX_PARAGRAPHS).ok()?;
            if moved <= -MAX_PARAGRAPHS {
                return None;
            }
            let before = line_start.Clone().ok()?;
            before
                .MoveEndpointByRange(TextPatternRangeEndpoint_End, &caret, TextPatternRangeEndpoint_Start)
                .ok()?;
            let text = before.GetText(100_000).ok()?.to_string();
            let (extra_lines, column) = position_after(&text);
            Some(CaretSample {
                line: (-moved) as u32 + extra_lines,
                column,
                via: "uia",
            })
        }
    }

    /// VS Code's own line and column, from its cursor-position status bar
    /// item. Asking switches Chromium's accessibility on (it stays on until
    /// the app restarts), so the first ask may come back empty.
    fn uia_vscode_position(fg: HWND) -> Option<(u32, u32)> {
        let _com = ComScope::enter();
        unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
            if let Ok(uia2) = uia.cast::<IUIAutomation2>() {
                let _ = uia2.SetConnectionTimeout(250);
                // One search over a large tree.
                let _ = uia2.SetTransactionTimeout(800);
            }
            let root = uia.ElementFromHandle(fg).ok()?;
            let by_id = uia
                .CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(VSCODE_POSITION_ID))
                .ok()?;
            let item = root.FindFirst(TreeScope_Descendants, &by_id).ok()?;
            let mut labels: Vec<String> = item.CurrentName().ok().map(|n| n.to_string()).into_iter().collect();
            let walker = uia.RawViewWalker().ok()?;
            let mut child = walker.GetFirstChildElement(&item).ok();
            for _ in 0..8 {
                let Some(c) = child else { break };
                if let Ok(name) = c.CurrentName() {
                    labels.push(name.to_string());
                }
                child = walker.GetNextSiblingElement(&c).ok();
            }
            parse_labels(Editor::VsCode, &labels)
        }
    }

    /// Recognise the text in the bottom strip of `fg` as it is on screen,
    /// when nothing covers that strip.
    fn ocr_strip(fg: HWND) -> Vec<TextRun> {
        unsafe {
            if IsIconic(fg).as_bool() {
                return Vec::new();
            }
            // The visible frame, without the invisible resize borders.
            let mut r = RECT::default();
            if DwmGetWindowAttribute(
                fg,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut r as *mut RECT).cast(),
                std::mem::size_of::<RECT>() as u32,
            )
            .is_err()
            {
                return Vec::new();
            }
            let scale = GetDpiForWindow(fg).max(96) as f64 / 96.0;
            let strip_h = (STRIP_POINTS * scale).round() as i32;
            let (w, h) = (r.right - r.left, strip_h);
            if w < 50 || r.bottom - r.top < h * 2 {
                return Vec::new();
            }
            let top = r.bottom - h;
            // Read off the screen, so the strip must be this window's.
            for fx in [0.1, 0.5, 0.9] {
                let p = POINT { x: r.left + (w as f64 * fx) as i32, y: top + h / 2 };
                if GetAncestor(WindowFromPoint(p), GA_ROOT) != fg {
                    return Vec::new();
                }
            }
            let Some(bgra) = grab_screen(r.left, top, w, h) else {
                return Vec::new();
            };
            // Windows' recogniser wants text well over 10px tall; a status
            // bar at 100% is about 12.
            let factor = if scale < 1.75 { 2 } else { 1 };
            let (bgra, w, h) = if factor > 1 { upscale(&bgra, w as u32, h as u32, factor) } else { (bgra, w as u32, h as u32) };
            recognise(&bgra, w, h).unwrap_or_default()
        }
    }

    /// The screen's pixels in a rectangle (physical pixels), top-down BGRA.
    unsafe fn grab_screen(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) if !bits.is_null() => {
                let old = SelectObject(mem, bmp.into());
                let copied = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
                let pixels = copied.then(|| {
                    let mut v = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
                    // BitBlt leaves alpha undefined; the recogniser wants it opaque.
                    for a in v.iter_mut().skip(3).step_by(4) {
                        *a = 255;
                    }
                    v
                });
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                pixels
            }
            _ => None,
        };
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        out
    }

    fn upscale(bgra: &[u8], w: u32, h: u32, factor: u32) -> (Vec<u8>, u32, u32) {
        let Some(img) = image::RgbaImage::from_raw(w, h, bgra.to_vec()) else {
            return (bgra.to_vec(), w, h);
        };
        let out = image::imageops::resize(&img, w * factor, h * factor, image::imageops::FilterType::CatmullRom);
        (out.into_raw(), w * factor, h * factor)
    }

    fn recognise(bgra: &[u8], w: u32, h: u32) -> windows::core::Result<Vec<TextRun>> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().or_else(|_| {
            OcrEngine::TryCreateFromLanguage(&windows::Globalization::Language::CreateLanguage(&"en-US".into())?)
        })?;
        let buffer = CryptographicBuffer::CreateFromByteArray(bgra)?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)?;
        let result = engine.RecognizeAsync(&bitmap)?.join()?;
        let mut runs = Vec::new();
        for line in result.Lines()? {
            let text = line.Text()?.to_string();
            let words = line.Words()?;
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for word in &words {
                let b = word.BoundingRect()?;
                x0 = x0.min(b.X);
                y0 = y0.min(b.Y);
                x1 = x1.max(b.X + b.Width);
                y1 = y1.max(b.Y + b.Height);
            }
            if x0 > x1 {
                continue;
            }
            runs.push(TextRun {
                text,
                x: ((x0 + x1) / 2.0) as f64 / w as f64,
                y: ((y0 + y1) / 2.0) as f64 / h as f64,
                // Windows.Media.Ocr gives no confidence.
                confidence: 1.0,
            });
        }
        Ok(runs)
    }

    /// The caret in the foreground window `fg`, whose app is `app`. Runs
    /// with the sampler's per-monitor DPI awareness (physical pixels).
    pub(crate) fn read(fg: HWND, app: &str) -> Option<CaretSample> {
        if let Some(sci) = focus_in(fg).filter(|h| class_of(*h) == "Scintilla") {
            if let Some(c) = scintilla_caret(sci) {
                return Some(c);
            }
        }
        if let Some(editor) = ocr_editor(app) {
            if editor == Editor::VsCode && prefer_accessibility() {
                if let Some((line, column)) = uia_vscode_position(fg) {
                    return Some(CaretSample { line, column, via: "uia-status" });
                }
            }
            let (line, column) = parse(editor, &ocr_strip(fg))?;
            return Some(CaretSample { line, column, via: "ocr" });
        }
        if is_terminal(app) || embeds_browser_engine(fg) {
            return None;
        }
        uia_caret(fg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, x: f64, y: f64) -> TextRun {
        TextRun { text: text.into(), x, y, confidence: 1.0 }
    }

    #[test]
    fn editors_are_recognised_by_app_id_on_every_platform() {
        assert_eq!(ocr_editor("com.microsoft.VSCode"), Some(Editor::VsCode));
        assert_eq!(ocr_editor("Code.exe"), Some(Editor::VsCode));
        assert_eq!(ocr_editor("Cursor.exe"), Some(Editor::VsCode));
        assert_eq!(ocr_editor("com.todesktop.230313mzl4w4u92"), Some(Editor::VsCode));
        assert_eq!(ocr_editor("com.jetbrains.intellij"), Some(Editor::LineColon));
        assert_eq!(ocr_editor("rider64.exe"), Some(Editor::LineColon));
        assert_eq!(ocr_editor("dev.zed.Zed"), Some(Editor::LineColon));
        assert_eq!(ocr_editor("com.sublimetext.4"), Some(Editor::Sublime));
        assert_eq!(ocr_editor("sublime_text.exe"), Some(Editor::Sublime));
        assert_eq!(ocr_editor("com.apple.dt.Xcode"), None, "read by Accessibility");
        assert_eq!(ocr_editor("com.google.Chrome"), None);
        assert!(is_terminal("com.googlecode.iterm2"));
        assert!(is_terminal("WindowsTerminal.exe"));
        assert!(!is_terminal("notepad.exe"));
    }

    #[test]
    fn vs_code_status_bar() {
        let runs = [
            run("const x = foo(12, 5);", 0.3, 0.1),
            run("⊗ 0 ⚠ 2", 0.05, 0.75),
            run("Ln 128, Col 17  Spaces: 4  UTF-8  LF  {} Rust", 0.8, 0.75),
        ];
        assert_eq!(parse(Editor::VsCode, &runs), Some((128, 17)));
        // With a selection, and translated.
        assert_eq!(parse(Editor::VsCode, &[run("Ln 3, Col 9 (14 selected)", 0.8, 0.7)]), Some((3, 9)));
        assert_eq!(parse(Editor::VsCode, &[run("Zeile 40, Spalte 2", 0.8, 0.7)]), Some((40, 2)));
        assert_eq!(parse(Editor::VsCode, &[run("行 7，列 31", 0.8, 0.7)]), Some((7, 31)));
        // Code in the editor above the status bar doesn't count.
        assert_eq!(parse(Editor::VsCode, &[run("Ln 1, Col 1", 0.5, 0.2)]), None);
        // Neither do the problem and sync counters.
        assert_eq!(parse(Editor::VsCode, &[run("⊗ 0 ⚠ 2  ↓0 ↑1", 0.1, 0.7)]), None);
    }

    #[test]
    fn line_colon_status_bar() {
        let runs = [
            run("src > main.rs > fn main", 0.2, 0.7),
            run("42:13", 0.78, 0.72),
            run("LF  UTF-8  4 spaces", 0.9, 0.72),
        ];
        assert_eq!(parse(Editor::LineColon, &runs), Some((42, 13)));
        assert_eq!(parse(Editor::LineColon, &[run("42:13 (5 chars)", 0.8, 0.7)]), Some((42, 13)));
        // Times and paths aren't positions.
        assert_eq!(parse(Editor::LineColon, &[run("Build finished at 10:42:07", 0.3, 0.7)]), None);
        assert_eq!(parse(Editor::LineColon, &[run("main.rs:12:5 warning", 0.3, 0.7)]), None);
        // The lowest match is the status bar; above it is a panel.
        let runs = [run("at 3:4", 0.5, 0.5), run("120:1", 0.8, 0.8)];
        assert_eq!(parse(Editor::LineColon, &runs), Some((120, 1)));
    }

    #[test]
    fn sublime_and_notepad_plus_plus_status_bars() {
        assert_eq!(parse(Editor::Sublime, &[run("Line 9, Column 4; Saved", 0.2, 0.7)]), Some((9, 4)));
        assert_eq!(
            parse(Editor::NotepadPlusPlus, &[run("Ln : 31    Col : 8    Pos : 900", 0.5, 0.7)]),
            Some((31, 8))
        );
    }

    #[test]
    fn unsure_or_conflicting_readings_give_nothing() {
        let unsure = TextRun { confidence: 0.3, ..run("42:13", 0.8, 0.7) };
        assert_eq!(parse(Editor::LineColon, &[unsure]), None);
        // Two positions side by side in the status bar: one is a misread.
        let runs = [run("42:13", 0.6, 0.7), run("42:18", 0.8, 0.7)];
        assert_eq!(parse(Editor::LineColon, &runs), None);
        // The same position twice is fine.
        let runs = [run("42:13", 0.6, 0.7), run("42:13", 0.8, 0.7)];
        assert_eq!(parse(Editor::LineColon, &runs), Some((42, 13)));
    }

    #[test]
    fn accessibility_labels() {
        let labels = ["Ln 12, Col 5".to_string(), "Go to Line/Column".to_string()];
        assert_eq!(parse_labels(Editor::VsCode, &labels), Some((12, 5)));
        assert_eq!(parse_labels(Editor::VsCode, &["Spaces: 4".to_string()]), None);
    }

    #[test]
    fn a_zero_is_a_misread() {
        assert_eq!(parse(Editor::LineColon, &[run("0:5", 0.8, 0.7)]), None);
    }

    #[test]
    fn position_after_counts_lines_and_characters() {
        assert_eq!(position_after(""), (1, 1));
        assert_eq!(position_after("abc"), (1, 4));
        assert_eq!(position_after("a\nbc"), (2, 3));
        assert_eq!(position_after("a\r\nb\rc\n"), (4, 1));
        assert_eq!(position_after("fn main() {\n    let é = 1;"), (2, 15));
    }
}
