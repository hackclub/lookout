//! `HeaderBar` (`clients/desktop/src/components/HeaderBar.tsx`): the 47px
//! Adwaita-style title bar. The Tauri app drew it only on Linux; Lookout
//! Native uses the same bar on every platform (a deliberate improvement over
//! the old in-page "‹ Back" buttons), changing only the window controls:
//! GNOME's round close, the macOS traffic lights, or Windows caption buttons.
//! See [`HeaderChrome`].
//! Back button (30×30 round action), title/subtitle, a flexible drag
//! spacer, page actions and the 24×24 round close control on whichever
//! edge GNOME's `button-layout` puts it.
//!
//! Hover/active colours transition over 100ms ease-out; the whole bar dims
//! to 55% opacity (160ms ease-out) while the window is in the backdrop.

use std::rc::Rc;

use gpui_kit::gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, WindowControlArea, div, img, px,
};

use gpui_kit::gpui::prelude::FluentBuilder;

use crate::motion::{Anim, Ease};
use crate::theme::{self, theme};
use crate::ui::icon::Icon;
use crate::ui::text::TextExt;

pub const HEADER_BAR_HEIGHT: f32 = 47.0;

thread_local! {
    /// Decoded once (at 2× for HiDPI): a fresh `RenderImage` per frame would
    /// leak an atlas tile each time.
    static APP_ICON: Option<std::sync::Arc<gpui_kit::gpui::RenderImage>> =
        crate::images::decode_fit(crate::assets::APP_ICON_PNG, 40, 40).ok();
}
/// Windows caption cells: full height, touching each other and the window's
/// top-right corner (so the red close reaches the edge, as on Windows), with
/// each glyph centred in its cell. The width is Lookout's button pitch (a
/// 30px round button plus the 4px gap), so minimize→close is the same
/// distance as gear→+.
const WIN_CAPTION_W: f32 = 34.0;

/// Where the traffic lights go, centred on the bar (`TitlebarOptions`).
pub const MAC_TRAFFIC_LIGHT_POSITION: (f32, f32) = (16.0, 16.0);
/// Space between the zoom button and the bar's first item.
const MAC_TRAFFIC_LIGHT_GAP: f32 = 12.0;

/// Width of the traffic-light cluster (close button's left edge to the zoom
/// button's right edge), measured from the window's real buttons at startup
/// (`platform::vibrancy`): macOS 26 spaces them wider than earlier releases,
/// so a fixed number collides on one or the other. The fallback fits macOS 26.
static MAC_TRAFFIC_SPAN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const MAC_TRAFFIC_SPAN_FALLBACK: f32 = 60.0;

pub fn set_mac_traffic_light_span(span: f32) {
    if span.is_finite() && span > 20.0 && span < 200.0 {
        MAC_TRAFFIC_SPAN.store(span.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// The bar's leading inset on macOS: where its first item starts.
pub fn mac_traffic_light_inset() -> f32 {
    let bits = MAC_TRAFFIC_SPAN.load(std::sync::atomic::Ordering::Relaxed);
    let span = if bits == 0 { MAC_TRAFFIC_SPAN_FALLBACK } else { f32::from_bits(bits) };
    MAC_TRAFFIC_LIGHT_POSITION.0 + span + MAC_TRAFFIC_LIGHT_GAP
}

/// Which platform's window chrome the bar sits in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderChrome {
    /// Linux, Lookout drawing its own frame: GNOME's round close control.
    Gnome,
    /// Linux under the window manager's frame, which has its own close.
    GnomeFramed,
    /// macOS: the bar extends under the transparent titlebar and leaves its
    /// leading edge to the traffic lights.
    Mac,
    /// Windows: the native title bar is hidden; minimize and close are the
    /// bar's own round buttons after a divider, spaced like its actions. (The
    /// window isn't resizable, so there's no maximize or Snap flyout to keep.)
    Windows,
}

impl HeaderChrome {
    /// Whether the bar's buttons take Windows caption cells (46px, full
    /// height, side by side) instead of GNOME's round actions.
    pub fn windows_captions() -> bool {
        Self::current(true) == Self::Windows
    }

    pub fn current(lookout_draws_frame: bool) -> Self {
        // Debug builds: `LOOKOUT_HEADER_CHROME=mac|windows` previews another
        // platform's controls (layout only; the buttons act on this window).
        #[cfg(debug_assertions)]
        match std::env::var("LOOKOUT_HEADER_CHROME").as_deref() {
            Ok("mac") => return Self::Mac,
            Ok("windows") => return Self::Windows,
            _ => {}
        }
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if lookout_draws_frame {
            Self::Gnome
        } else {
            Self::GnomeFramed
        }
    }
}

/// The round header-bar buttons. `Control` is the filled close button,
/// `Action` the transparent back/gear/+ buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HeaderBtnKind {
    Control,
    Action,
}

struct HoverState {
    hovered: bool,
    pressed: bool,
    /// Pressed since the pointer arrived: the tooltip stays hidden until it
    /// leaves, like the webview's native `title` tooltips (which hid on click
    /// rather than sitting over the menu the button just opened).
    tip_dismissed: bool,
    hover: Anim,
    press: Anim,
}

#[derive(IntoElement)]
pub struct HeaderBtn {
    id: ElementId,
    kind: HeaderBtnKind,
    content: AnyElement,
    /// An icon drawn in the button's own (transitioning) foreground colour —
    /// Phosphor icons are `currentColor`, so they follow the CSS
    /// `color 100ms ease-out` hover transition.
    icon: Option<(Icon, f32)>,
    on_click: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    tooltip: Option<SharedString>,
    /// Windows' close: system red behind a white glyph on hover/press.
    danger: bool,
    /// Windows caption shape: a 46px-wide, full-height square-cornered cell
    /// flush with the window edge (the OS rounds the window's corner).
    caption: bool,
    /// A text action ([`Self::text`]): a 30px pill sized to its label.
    label: Option<SharedString>,
    /// Destructive text action: red label, red-tinted hover.
    destructive: bool,
    /// Dimmed to 60% and inert (a pending or unavailable action).
    disabled: bool,
}

impl HeaderBtn {
    pub fn new(
        id: impl Into<ElementId>,
        kind: HeaderBtnKind,
        content: impl IntoElement,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            content: content.into_any_element(),
            icon: None,
            on_click: Rc::new(on_click),
            tooltip: None,
            danger: false,
            caption: false,
            label: None,
            destructive: false,
            disabled: false,
        }
    }
    /// A button whose content is `icon` at `size`, tinted with the button's
    /// animated foreground colour.
    pub fn icon(
        id: impl Into<ElementId>,
        kind: HeaderBtnKind,
        icon: Icon,
        size: f32,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        let mut b = Self::new(id, kind, div(), on_click);
        b.icon = Some((icon, size));
        b
    }
    /// A page action with a word instead of a glyph, for actions with no
    /// unambiguous icon ("Archive" as an ✕ would read as close). Same height,
    /// colours and hover/press as the round actions, so it sits in the row
    /// with them; the pill just grows to fit the label.
    pub fn text(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        let mut b = Self::new(id, HeaderBtnKind::Action, div(), on_click);
        b.label = Some(label.into());
        b
    }
    /// Red label on a faint red pill, for actions that end or remove
    /// something. Never a solid red button: the bar stays quiet.
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }
    pub fn tooltip(mut self, t: impl Into<SharedString>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
    /// Hover/press in close red (#F9271E, a touch lighter while pressed) with
    /// the glyph turning white.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    /// Draw as a Windows 11 caption button instead of a round action.
    pub fn caption(mut self) -> Self {
        self.caption = true;
        self
    }
    /// [`Self::caption`] when `on`.
    pub fn caption_if(mut self, on: bool) -> Self {
        self.caption |= on;
        self
    }
}


impl RenderOnce for HeaderBtn {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let st = window.use_keyed_state(self.id.clone(), cx, |_, _| HoverState {
            hovered: false,
            pressed: false,
            tip_dismissed: false,
            hover: Anim::new(0.0),
            press: Anim::new(0.0),
        });
        let moving = st.update(cx, |s, _| s.hover.tick() | s.press.tick());
        if moving {
            window.request_animation_frame();
        }
        let (h, p) = {
            let s = st.read(cx);
            (s.hover.get(), s.press.get())
        };
        let (size, idle, hover, active, fg_idle, fg_hover): (f32, Hsla, Hsla, Hsla, Hsla, Hsla) = match self.kind {
            HeaderBtnKind::Control => (
                24.0,
                t.headerbar_control,
                t.headerbar_control_hover,
                t.border_hover,
                t.text_primary,
                t.text_primary,
            ),
            HeaderBtnKind::Action => (
                30.0,
                theme::transparent(),
                t.headerbar_control,
                t.headerbar_control_hover,
                t.text_secondary,
                t.text_primary,
            ),
        };
        let (fg_idle, fg_hover) = if self.caption { (t.text_primary, t.text_primary) } else { (fg_idle, fg_hover) };
        // Destructive actions carry a faint red wash even at rest, so they
        // read as buttons (and as different from Archive-style actions)
        // without becoming a loud filled red in the bar.
        let (idle, hover, active, fg_idle, fg_hover) = if self.destructive {
            let red = theme::status::danger();
            (red.opacity(0.10), red.opacity(0.16), red.opacity(0.22), red, red)
        } else {
            (idle, hover, active, fg_idle, fg_hover)
        };
        let (hover, active, fg_hover) = if self.danger {
            let red: Hsla = theme::rgba8(0xf9, 0x27, 0x1e, 1.0).into();
            (red, theme::rgba8(0xf9, 0x27, 0x1e, 0.9).into(), theme::white(1.0))
        } else {
            (hover, active, fg_hover)
        };
        let bg = theme::mix(theme::mix(idle, hover, h), active, p);
        let fg = theme::mix(fg_idle, fg_hover, h.max(p));
        let (s1, s2, s3, s4) = (st.clone(), st.clone(), st.clone(), st.clone());
        let f = self.on_click.clone();
        let caption = self.caption;
        let disabled = self.disabled;
        let label = self.label.clone();
        let mut el = div()
            .id(self.id)
            .map(|d| {
                if caption {
                    d.w(px(WIN_CAPTION_W)).h(px(HEADER_BAR_HEIGHT))
                } else if label.is_some() {
                    // The label's side padding matches the gap between a round
                    // button's edge and its glyph, so text and icons share a rhythm.
                    d.h(px(size)).px(px(12.0)).rounded_full()
                } else {
                    d.size(px(size)).rounded_full()
                }
            })
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .text_color(fg)
            .map(|d| match (self.icon, label) {
                (Some((icon, size)), _) => d.child(icon.el(size, fg)),
                (None, Some(l)) => d.child(div().text_size(px(13.0)).line_height(px(16.0)).w600().whitespace_nowrap().child(l)),
                (None, None) => d.child(self.content),
            })
            .when(disabled, |d| d.opacity(0.6))
            .when(!disabled, |d| d.on_hover(move |hov, _, cx| {
                s1.update(cx, |s, cx| {
                    s.hovered = *hov;
                    if !*hov {
                        s.tip_dismissed = false;
                    }
                    s.hover.set_css(if *hov { 1.0 } else { 0.0 }, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
                s2.update(cx, |s, cx| {
                    s.pressed = true;
                    s.tip_dismissed = true;
                    s.press.set_css(1.0, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                s3.update(cx, |s, cx| {
                    s.pressed = false;
                    s.press.set_css(0.0, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                s4.update(cx, |s, cx| {
                    s.pressed = false;
                    s.press.set_css(0.0, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_click(move |ev, window, cx| f(ev, window, cx)));
        let tip_dismissed = st.read(cx).tip_dismissed;
        if let Some(tip) = self.tooltip.filter(|_| !tip_dismissed) {
            el = el.tooltip(move |window, cx| crate::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx));
        }
        el
    }
}

/// Header contents for the current page.
#[derive(Clone, Default)]
pub struct HeaderNav {
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    pub on_back: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    /// Lead with the app icon (the gallery's "Lookout" title).
    pub app_icon: bool,
}

#[derive(IntoElement)]
pub struct HeaderBar {
    nav: HeaderNav,
    actions: Vec<AnyElement>,
    controls_on_right: bool,
    backdrop: f32,
    maximizable: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    chrome: HeaderChrome,
}

impl HeaderBar {
    pub fn new(nav: HeaderNav, controls_on_right: bool, backdrop: f32) -> Self {
        Self { nav, actions: Vec::new(), controls_on_right, backdrop, maximizable: false, on_close: None, chrome: HeaderChrome::Gnome }
    }
    pub fn chrome(mut self, c: HeaderChrome) -> Self {
        self.chrome = c;
        self
    }
    pub fn actions(mut self, a: Vec<AnyElement>) -> Self {
        self.actions = a;
        self
    }
    pub fn maximizable(mut self, on: bool) -> Self {
        self.maximizable = on;
        self
    }
    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(f));
        self
    }
}

struct DragState {
    should_move: bool,
}

impl RenderOnce for HeaderBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let drag = window.use_keyed_state("headerbar-drag", cx, |_, _| DragState { should_move: false });
        let make_close = |id: &'static str| {
            let on_close = self.on_close.clone();
            HeaderBtn::new(
                id,
                HeaderBtnKind::Control,
                Icon::LkHeaderClose.el(14.0, t.text_primary),
                move |_, window, cx| match &on_close {
                    Some(f) => f(window, cx),
                    None => window.remove_window(),
                },
            )
            .tooltip("Close")
        };
        let has_back = self.nav.on_back.is_some();
        let back = self.nav.on_back.clone().map(|f| {
            HeaderBtn::icon("hb-back", HeaderBtnKind::Action, Icon::LkHeaderBack, 16.0, move |_, window, cx| f(window, cx))
                .tooltip("Back")
        });
        let title_size = if self.nav.subtitle.is_some() { 13.0 } else { 14.0 };
        let title_block = div()
            .flex()
            .flex_col()
            .justify_center()
            .min_w(px(0.0))
            .child(
                div()
                    .text_size(px(title_size))
                    .line_height(px(title_size * 1.15))
                    .w700()
                    .text_color(t.text_primary)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.nav.title.clone()),
            )
            .children(self.nav.subtitle.clone().map(|s| {
                div()
                    .text_size(px(11.0))
                    .line_height(px(11.0 * 1.15))
                    .w400()
                    .text_color(t.text_secondary)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(s)
            }));
        let maximizable = self.maximizable;
        let chrome = self.chrome;
        let gnome_close = chrome == HeaderChrome::Gnome;
        let controls_on_right = self.controls_on_right;
        let (d1, d2, d3) = (drag.clone(), drag.clone(), drag.clone());
        let bar = div()
            .id("lookout-headerbar")
            .relative()
            .flex_none()
            .h(px(HEADER_BAR_HEIGHT))
            .flex()
            .items_center()
            .gap(px(8.0))
            .map(|d| {
                let lead = match chrome {
                    HeaderChrome::Mac => mac_traffic_light_inset(),
                    _ if has_back => 8.0,
                    _ => 12.0,
                };
                // Windows' caption cells meet the window's right edge.
                d.pl(px(lead)).pr(px(if chrome == HeaderChrome::Windows { 0.0 } else { 8.0 }))
            })
            .opacity(self.backdrop);
        let bar = match chrome {
            // The OS hit-tests the bar as caption: native drag and Aero Snap.
            HeaderChrome::Windows => bar.window_control_area(WindowControlArea::Drag),
            HeaderChrome::Mac => bar.window_control_area(WindowControlArea::Drag).on_mouse_down(MouseButton::Left, |ev, window, _| {
                if window.default_prevented() {
                    return;
                }
                if ev.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            }),
            HeaderChrome::Gnome | HeaderChrome::GnomeFramed => self.gnome_drag(bar, maximizable, d1, d2, d3),
        };
        let app_icon = (self.nav.app_icon && !has_back).then(|| APP_ICON.with(|i| i.clone())).flatten();
        bar.map(|d| if gnome_close && !controls_on_right { d.child(make_close("hb-close-left")) } else { d })
            .children(back)
            .children(app_icon.map(|i| img(i).size(px(20.0)).rounded(px(5.0)).flex_none()))
            .child(title_block)
            .child(div().flex_1().self_stretch())
            .child({
                let win = chrome == HeaderChrome::Windows;
                let on_close = self.on_close.clone();
                // The divider separates page actions from the window
                // controls, so it goes when there are no actions (subpages).
                let has_actions = !self.actions.is_empty();
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .flex_none()
                    .children(self.actions)
                    .map(|d| if gnome_close && controls_on_right { d.child(make_close("hb-close")) } else { d })
                    .when(win, |d| {
                        d.when(has_actions, |d| d.child(div().w(px(1.0)).h(px(16.0)).mx(px(4.0)).bg(t.border_default))).child(
                            // The two cells touch: no gap between them.
                            div().flex().flex_none().child(
                                HeaderBtn::icon("hb-win-min", HeaderBtnKind::Action, Icon::LkWinMinimize, 10.0, |_, window, _| {
                                    window.minimize_window()
                                })
                                .tooltip("Minimize")
                                .caption(),
                            )
                            .child(
                                HeaderBtn::icon("hb-win-close", HeaderBtnKind::Action, Icon::LkWinClose, 10.0, move |_, window, cx| {
                                    match &on_close {
                                        Some(f) => f(window, cx),
                                        None => window.remove_window(),
                                    }
                                })
                                .tooltip("Close")
                                .caption()
                                .danger(),
                            ),
                        )
                    })
            })
    }
}

impl HeaderBar {
    /// GNOME drag: arm on press, move once the pointer moves with the button
    /// held, so the bar's buttons keep their clicks.
    fn gnome_drag(
        &self,
        bar: gpui_kit::gpui::Stateful<gpui_kit::gpui::Div>,
        maximizable: bool,
        d1: gpui_kit::gpui::Entity<DragState>,
        d2: gpui_kit::gpui::Entity<DragState>,
        d3: gpui_kit::gpui::Entity<DragState>,
    ) -> gpui_kit::gpui::Stateful<gpui_kit::gpui::Div> {
        bar.on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                if window.default_prevented() {
                    return;
                }
                if ev.click_count == 2 {
                    if maximizable {
                        window.zoom_window();
                    }
                    return;
                }
                d1.update(cx, |s, _| s.should_move = true);
            })
            .on_mouse_up(MouseButton::Left, {
                let d2 = d2.clone();
                move |_, _, cx| d2.update(cx, |s, _| s.should_move = false)
            })
            // A release outside the bar must disarm it too: otherwise the
            // next pointer motion anywhere would start a window move that
            // follows the cursor with no button held.
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| d2.update(cx, |s, _| s.should_move = false))
            .on_mouse_move(move |ev, window, cx| {
                let held = ev.pressed_button == Some(MouseButton::Left);
                let go = d3.update(cx, |s, _| std::mem::replace(&mut s.should_move, false)) && held;
                if go {
                    window.start_window_move();
                }
            })
            .on_mouse_down(MouseButton::Right, |ev, window, _| window.show_window_menu(ev.position))
    }
}


