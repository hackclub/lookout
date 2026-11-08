//! The installer window: Figma "Fallen" frame 494:13 (742×545, a white to
//! mint ground, app icon and title top-left, the green download badge
//! cropped bottom-left, the Hack Club flag footer). The pages and their
//! wording are the NSIS installer's (Choose Install Location → Installing →
//! Completing Lookout Setup), drawn in the tactile style with Lookout's blue
//! as the primary and its squircle press spring on every button.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::gpui::{
    App, Bounds, Context, FocusHandle, Hsla, IntoElement, KeyDownEvent, ParentElement, PathPromptOptions, Render,
    SharedString, Styled, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowKind, WindowOptions, div, img, linear_color_stop, linear_gradient, point, prelude::*, px, relative, size,
};

use super::ops::{self, Ops, Progress};
use crate::motion::{Anim, Ease};
use crate::theme::{self, Theme, hex, hexa, is_mac, is_windows};
use crate::ui::button::Button;
use crate::ui::header_bar::{HeaderBtn, HeaderBtnKind};
use crate::ui::icon::Icon;
use crate::ui::text::TextExt;

const W: f32 = 742.0;
const H: f32 = 545.0;
/// The frame's title inset; the page content lines up with the logo.
const EDGE: f32 = 39.0;
/// The footer's left edge in the frame.
const FOOTER_X: f32 = 180.0;
/// Where the page starts, under the title row.
const PAGE_TOP: f32 = 124.0;

/// The tactile skill's strong ease-out, for everything the user caused.
const STRONG: Ease = Ease::Bezier(0.23, 1.0, 0.32, 1.0);

// Radix sand, the tactile defaults.
fn ink12() -> Hsla {
    hex(0x21201c)
}
fn ink11() -> Hsla {
    hex(0x63635e)
}
fn ink10() -> Hsla {
    hex(0x82827c)
}
fn red9() -> Hsla {
    hex(0xe5484d)
}
fn red10() -> Hsla {
    hex(0xdc3e42)
}
fn red11() -> Hsla {
    hex(0xce2c31)
}
/// Ink at an alpha: wells, rings and drops borrow from the ground beneath.
fn ink_a(a: f32) -> Hsla {
    hexa(0x21201c, a)
}
/// Lookout's blue (the light theme's accent) and its hover.
fn blue() -> Hsla {
    Theme::light().accent
}
fn blue_hover() -> Hsla {
    Theme::light().accent_hover
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Install,
    Uninstall,
}

#[derive(Clone, Debug)]
enum Phase {
    Ready,
    Working { detail: String },
    Done { exe: Option<PathBuf> },
    Failed(String),
}

pub fn run(flow: Flow, ops: Ops) {
    let app = gpui_kit::platform::application().with_assets(crate::assets::Assets);
    app.run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(crate::assets::FONTS.iter().map(|b| Cow::Borrowed(*b)).collect())
            .expect("load bundled fonts");
        let families = cx.text_system().all_font_names().into_iter().map(Into::into).collect::<Vec<_>>();
        crate::ui::text::init_family(&families);
        // The frame is a light design; the caption buttons read the theme.
        cx.set_global(Theme::light());
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let title: SharedString = match flow {
            Flow::Install => "Lookout Setup".into(),
            Flow::Uninstall => "Lookout Uninstall".into(),
        };
        let bounds = Bounds::centered(None, size(px(W), px(H)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title),
                // Windows and macOS: the frame runs to the top edge; we draw
                // Windows' caption buttons, macOS keeps its traffic lights.
                appears_transparent: is_windows() || is_mac(),
                traffic_light_position: is_mac().then(|| point(px(16.0), px(16.0))),
            }),
            window_background: WindowBackgroundAppearance::Opaque,
            is_resizable: false,
            is_minimizable: true,
            kind: WindowKind::Normal,
            app_id: Some(crate::store::APP_ID.into()),
            icon: crate::platform::window_icon(),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| cx.new(|cx| InstallerView::new(flow, ops, window, cx)))
            .expect("open installer window");
        cx.activate(true);
    });
}

struct InstallerView {
    flow: Flow,
    ops: Ops,
    dir: PathBuf,
    /// Writing to `dir` takes admin rights: shield on the button, UAC on click.
    admin: bool,
    phase: Phase,
    /// The finish page's "Open Lookout" box.
    open_after: bool,
    focus: FocusHandle,
    /// The progress bar's fill, eased toward each reported fraction.
    bar: Anim,
    /// 0→1 each time the page changes: it fades and rises in.
    content: Anim,
    /// The badge's one-time entrance.
    badge: Anim,
    /// What the install put in place, kept across a WebView2 install.
    installed: Option<PathBuf>,
    /// WebView2 is still missing after the install (its per-user install
    /// didn't take): the finish page offers to install it with admin rights.
    wv2_offer: bool,
    /// The admin WebView2 install is what's running.
    wv2_running: bool,
    /// Why the last admin WebView2 install didn't work.
    wv2_error: Option<String>,
}

impl InstallerView {
    fn new(flow: Flow, ops: Ops, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // An existing install is updated where it is, if this user can write there.
        let dir = ops.preferred_dir();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut content = Anim::new(0.0);
        content.set_css(1.0, 320.0, STRONG);
        let mut badge = Anim::new(0.0);
        badge.set_delayed(1.0, crate::motion::Curve::tween(520.0, STRONG), 80.0);
        let admin = ops.needs_admin(&dir);
        Self {
            flow,
            ops,
            dir,
            admin,
            phase: Phase::Ready,
            open_after: true,
            focus,
            bar: Anim::new(0.0),
            content,
            badge,
            installed: None,
            wv2_offer: false,
            wv2_running: false,
            wv2_error: None,
        }
    }

    fn set_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        let turn = std::mem::discriminant(&phase) != std::mem::discriminant(&self.phase);
        self.phase = phase;
        if turn {
            self.content.jump(0.0);
            self.content.set_css(1.0, 240.0, STRONG);
        }
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (ops, flow, dir) = (self.ops, self.flow, self.dir.clone());
        self.wv2_running = false;
        self.run_job(cx, move |report| match flow {
            Flow::Install => ops.install(&dir, report).map(Some),
            Flow::Uninstall => ops.uninstall(report).map(|_| None),
        });
    }

    /// The finish page's "Install WebView2": through UAC, for everyone.
    fn start_webview2(&mut self, cx: &mut Context<Self>) {
        let ops = self.ops;
        self.wv2_running = true;
        self.wv2_error = None;
        self.run_job(cx, move |report| ops.install_webview2_admin(report).map(|_| None));
    }

    /// Runs `job` on a worker thread, showing its progress, then the page for
    /// its outcome.
    fn run_job(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(&dyn Fn(Progress)) -> Result<Option<PathBuf>, String> + Send + 'static,
    ) {
        if matches!(self.phase, Phase::Working { .. }) {
            return;
        }
        self.bar.jump(0.0);
        self.set_phase(Phase::Working { detail: String::new() }, cx);

        let (tx, rx) = flume::unbounded::<Result<Progress, Result<Option<PathBuf>, String>>>();
        std::thread::Builder::new()
            .name("lookout-installer".into())
            .spawn(move || {
                let report = |p: Progress| {
                    let _ = tx.send(Ok(p));
                };
                let result = job(&report);
                let _ = tx.send(Err(result));
            })
            .expect("installer thread");

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv_async().await {
                match msg {
                    Ok(p) => {
                        let alive = this
                            .update(cx, |v, cx| {
                                v.bar.set_css(p.fraction, 240.0, STRONG);
                                v.phase = Phase::Working { detail: p.step };
                                cx.notify();
                            })
                            .is_ok();
                        if !alive {
                            return;
                        }
                    }
                    Err(result) => {
                        if result.is_ok() {
                            // Let the bar land at the end before the page turns.
                            let _ = this.update(cx, |v, cx| {
                                v.bar.set_css(1.0, 240.0, STRONG);
                                cx.notify();
                            });
                            cx.background_executor().timer(Duration::from_millis(450)).await;
                        }
                        let _ = this.update(cx, |v, cx| v.job_done(result, cx));
                        return;
                    }
                }
            }
        })
        .detach();
    }

    fn job_done(&mut self, result: Result<Option<PathBuf>, String>, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.wv2_running) {
            // Lookout itself is installed either way: back to the finish page.
            match result {
                Ok(_) => self.wv2_offer = self.ops.webview2_missing(),
                Err(e) => self.wv2_error = Some(e),
            }
            return self.set_phase(Phase::Done { exe: self.installed.clone() }, cx);
        }
        match result {
            Ok(exe) => {
                self.installed = exe.clone();
                self.wv2_offer = self.flow == Flow::Install && self.ops.webview2_missing();
                self.set_phase(Phase::Done { exe }, cx)
            }
            Err(e) => self.set_phase(Phase::Failed(e), cx),
        }
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select the folder to install Lookout in:".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    // NSIS appends the product folder unless it's already there.
                    let dir = if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("lookout")) { p } else { p.join("Lookout") };
                    let _ = this.update(cx, |v, cx| {
                        v.admin = v.ops.needs_admin(&dir);
                        v.dir = dir;
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if let Phase::Done { exe: Some(exe) } = &self.phase {
            if self.open_after {
                self.ops.launch(exe);
            }
        }
        cx.quit();
    }

    /// The page's default button (also Enter).
    fn primary(&mut self, cx: &mut Context<Self>) {
        match self.phase {
            Phase::Ready | Phase::Failed(_) => self.start(cx),
            Phase::Working { .. } => {}
            Phase::Done { .. } => self.finish(cx),
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "enter" => self.primary(cx),
            "escape" if !matches!(self.phase, Phase::Working { .. }) => cx.quit(),
            _ => {}
        }
    }

    /// NSIS's page header: `(title, subtitle)`.
    fn header(&self) -> (&'static str, &'static str) {
        if self.wv2_running {
            return ("Installing WebView2", "Please wait while Microsoft Edge WebView2 is being installed.");
        }
        match (self.flow, &self.phase) {
            (Flow::Install, Phase::Ready) => ("Choose Install Location", "Choose the folder in which to install Lookout."),
            (Flow::Install, Phase::Working { .. }) => ("Installing", "Please wait while Lookout is being installed."),
            (Flow::Install, Phase::Done { .. }) => ("Completing Lookout Setup", "Lookout has been installed on your computer."),
            (Flow::Install, Phase::Failed(_)) => ("Installation Aborted", "Setup was not completed successfully."),
            (Flow::Uninstall, Phase::Ready) => ("Uninstall Lookout", "Remove Lookout from your computer."),
            (Flow::Uninstall, Phase::Working { .. }) => ("Uninstalling", "Please wait while Lookout is being uninstalled."),
            (Flow::Uninstall, Phase::Done { .. }) => ("Uninstallation Complete", "Uninstall was completed successfully."),
            (Flow::Uninstall, Phase::Failed(_)) => ("Uninstallation Aborted", "Uninstall was not completed successfully."),
        }
    }
}

// ── controls ─────────────────────────────────────────────────────────────

/// Tactile accent primary: Lookout blue, white label, a ring in a darker
/// blue and the crisp drop.
fn primary_btn(id: &'static str, label: impl Into<SharedString>) -> Button {
    style_primary(Button::new(id, label))
}

fn style_primary(b: Button) -> Button {
    b
        .bg(blue())
        .hover_bg(blue_hover())
        .border(theme::oklab_mix_black(blue(), 0.82))
        .drop(hexa(0x1d4ed8, 0.35))
        .text_color(hex(0xffffff))
        .padding(8.5, 20.0)
        .font_size(13.0)
        .medium()
        .radius(8.0)
}

fn secondary_btn(id: &'static str, label: impl Into<SharedString>) -> Button {
    secondary_btn_style(Button::new(id, label))
}

fn secondary_btn_style(b: Button) -> Button {
    b
        .bg(hex(0xffffff))
        .hover_bg(hex(0xfafaf9))
        .border(ink_a(0.10))
        .drop(ink_a(0.12))
        .text_color(ink12())
        .padding(8.5, 14.0)
        .font_size(13.0)
        .medium()
        .radius(8.0)
}

/// Ghost: no fill, ring or drop until hovered (Cancel, Close).
fn ghost_btn(id: &'static str, label: impl Into<SharedString>) -> Button {
    Button::new(id, label)
        .bg(theme::transparent())
        .hover_bg(ink_a(0.06))
        .border(theme::transparent())
        .text_color(ink12())
        .padding(8.5, 14.0)
        .font_size(13.0)
        .medium()
        .radius(8.0)
}

/// `label` with the UAC shield in front when the action needs admin rights
/// (Windows' convention for a button that will raise a UAC prompt).
fn shielded(id: &'static str, label: &'static str, shield: bool, style: fn(Button) -> Button) -> Button {
    if !shield {
        return style(Button::new(id, label));
    }
    style(Button::empty(id)).gap(6.0).child(super::shield::shield_img()).child(label)
}

fn danger_btn(id: &'static str, label: impl Into<SharedString>) -> Button {
    style_danger(Button::new(id, label))
}

fn style_danger(b: Button) -> Button {
    b
        .bg(red9())
        .hover_bg(red10())
        .border(theme::oklab_mix_black(red9(), 0.85))
        .drop(hexa(0x641723, 0.3))
        .text_color(hex(0xffffff))
        .padding(8.5, 20.0)
        .font_size(13.0)
        .medium()
        .radius(8.0)
}

/// A tactile checkbox: raised white box, blue with a tick when on.
fn checkbox(on: bool) -> impl IntoElement {
    div()
        .size(px(16.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .map(|d| {
            if on {
                d.bg(blue()).border_1().border_color(theme::oklab_mix_black(blue(), 0.82))
            } else {
                d.bg(hex(0xffffff)).border_1().border_color(ink_a(0.16))
            }
        })
        .shadow(vec![crate::theme::sh(0.0, 1.0, 2.0, 0.0, ink_a(0.12))])
        .when(on, |d| d.child(Icon::CheckBold.el(10.0, hex(0xffffff))))
}

/// Paragraph text: 13/20, ink-11.
fn para(text: impl Into<SharedString>) -> gpui_kit::gpui::Div {
    div().ts_lh(13.0, 20.0 / 13.0).text_color(ink11()).child(text.into())
}

/// Meta text: 12/16, ink-10.
fn meta(text: impl Into<SharedString>) -> gpui_kit::gpui::Div {
    div().ts_lh(12.0, 16.0 / 12.0).text_color(ink10()).child(text.into())
}

/// The folder as a borderless well.
fn well(path: SharedString) -> gpui_kit::gpui::Stateful<gpui_kit::gpui::Div> {
    div()
        .id("dir-well")
        .flex_1()
        .min_w(px(0.0))
        .h(px(36.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded(px(8.0))
        .bg(ink_a(0.05))
        .ts_lh(13.0, 20.0 / 13.0)
        .text_color(ink12())
        .child(div().min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(path))
}

// ── render ───────────────────────────────────────────────────────────────

impl Render for InstallerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let moving = [self.bar.tick(), self.content.tick(), self.badge.tick()].iter().any(|m| *m);
        if moving {
            window.request_animation_frame();
        }
        let reduce = crate::motion::reduce_motion();
        let c = self.content.get();
        let rise = if reduce { 0.0 } else { 8.0 * (1.0 - c) };
        let b = self.badge.get();
        let badge_rise = if reduce { 0.0 } else { 28.0 * (1.0 - b) };
        let working = matches!(self.phase, Phase::Working { .. });
        let path: SharedString = self.dir.to_string_lossy().to_string().into();

        // ── page ──
        let (title, subtitle) = self.header();
        let mut page = div()
            .absolute()
            .left(px(EDGE))
            .right(px(EDGE))
            .top(px(PAGE_TOP + rise))
            .opacity(c)
            .flex()
            .flex_col()
            .child(div().ts_lh(20.0, 1.4).text_color(ink12()).child(title))
            .child(para(subtitle).mt(px(2.0)));

        page = match (&self.phase, self.flow) {
            (Phase::Ready, Flow::Install) => {
                let required = ops::space_required().map(ops::fmt_size);
                let available = ops::space_available(&self.dir).map(ops::fmt_size);
                page.child(para(
                    "Setup will install Lookout in the following folder. To install in a different folder, click Browse and select another folder. Click Install to start the installation.",
                ).mt(px(18.0)).max_w(px(600.0)))
                .child(
                    div()
                        .mt(px(18.0))
                        .flex()
                        .items_end()
                        .justify_between()
                        .child(meta("Destination Folder"))
                        .child(
                            div()
                                .flex()
                                .gap(px(16.0))
                                .children(required.map(|s| meta(format!("Space required: {s}"))))
                                .children(available.map(|s| meta(format!("Space available: {s}")))),
                        ),
                )
                .child(
                    div()
                        .mt(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(well(path).cursor_pointer().hover(|s| s.bg(ink_a(0.07))).on_click(cx.listener(|v, _, _, cx| v.browse(cx))))
                        .child(secondary_btn("browse", "Browse…").on_click(cx.listener(|v, _, _, cx| v.browse(cx)))),
                )
            }
            (Phase::Ready, Flow::Uninstall) => page
                .child(
                    para("Lookout will be uninstalled from the following folder. Click Uninstall to start the uninstallation.")
                        .mt(px(18.0))
                        .max_w(px(600.0)),
                )
                .child(meta("Uninstalling from:").mt(px(18.0)))
                .child(div().mt(px(6.0)).flex().child(well(path))),
            (Phase::Working { detail }, _) => {
                let f = self.bar.get().clamp(0.0, 1.0);
                page.child(
                    div()
                        .mt(px(28.0))
                        .flex()
                        .justify_between()
                        .gap(px(16.0))
                        .child(meta(detail.clone()).min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_ellipsis())
                        .child(meta(format!("{}%", (f * 100.0).round() as u32)).flex_none()),
                )
                .child(
                    // The track is a well; the fill is raised blue.
                    div()
                        .mt(px(8.0))
                        .w_full()
                        .h(px(10.0))
                        .p(px(2.0))
                        .rounded_full()
                        .bg(ink_a(0.06))
                        .child(
                            div()
                                .h_full()
                                .w(relative(f))
                                .min_w(px(6.0))
                                .rounded_full()
                                .bg(blue())
                                .shadow(vec![crate::theme::sh(0.0, 1.0, 1.0, 0.0, hexa(0x1d4ed8, 0.35))]),
                        ),
                )
            }
            (Phase::Done { .. }, Flow::Install) => page
                .when(self.wv2_offer, |page| {
                    page.child(
                        para(
                            "Lookout requires Microsoft Edge WebView2. Click Install WebView2 to install it.",
                        )
                        .mt(px(18.0))
                        .max_w(px(600.0)),
                    )
                    .children(self.wv2_error.clone().map(|e| {
                        div().mt(px(8.0)).max_w(px(600.0)).ts_lh(13.0, 20.0 / 13.0).text_color(red11()).child(e)
                    }))
                })
                .child(para("Click Finish to close Setup.").mt(px(18.0)))
                .child(
                div()
                    .id("open-after")
                    .mt(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .child(checkbox(self.open_after))
                    .child(div().ts_lh(13.0, 20.0 / 13.0).text_color(ink12()).child("Open Lookout"))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.open_after = !v.open_after;
                        cx.notify();
                    })),
            ),
            (Phase::Done { .. }, Flow::Uninstall) => page,
            (Phase::Failed(e), _) => page.child(
                div().mt(px(18.0)).max_w(px(600.0)).ts_lh(13.0, 20.0 / 13.0).text_color(red11()).child(e.clone()),
            ),
        };

        // ── buttons (NSIS order: Cancel left of the default) ──
        let mut actions = div().absolute().right(px(EDGE)).bottom(px(36.0)).flex().items_center().gap(px(8.0)).opacity(c);
        actions = match (&self.phase, self.flow) {
            (Phase::Ready, Flow::Install) => actions
                .child(ghost_btn("cancel", "Cancel").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                .child(shielded("primary", "Install", self.admin, style_primary).on_click(cx.listener(|v, _, _, cx| v.primary(cx)))),
            (Phase::Ready, Flow::Uninstall) => actions
                .child(ghost_btn("cancel", "Cancel").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                .child(shielded("primary", "Uninstall", self.admin, style_danger).on_click(cx.listener(|v, _, _, cx| v.primary(cx)))),
            (Phase::Working { .. }, _) => actions,
            (Phase::Done { .. }, Flow::Install) => actions
                .when(self.wv2_offer, |a| {
                    a.child(
                        shielded("webview2", "Install WebView2", true, secondary_btn_style)
                            .on_click(cx.listener(|v, _, _, cx| v.start_webview2(cx))),
                    )
                })
                .child(primary_btn("primary", "Finish").on_click(cx.listener(|v, _, _, cx| v.finish(cx)))),
            (Phase::Done { .. }, Flow::Uninstall) => {
                actions.child(primary_btn("primary", "Close").on_click(cx.listener(|_, _, _, cx| cx.quit())))
            }
            (Phase::Failed(_), _) => actions
                .child(ghost_btn("close", "Close").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                .child(shielded("primary", "Retry", self.admin, style_primary).on_click(cx.listener(|v, _, _, cx| v.primary(cx)))),
        };

        // ── frame ──
        let caption = is_windows() || cfg!(debug_assertions) && std::env::var("LOOKOUT_HEADER_CHROME").as_deref() == Ok("windows");
        div()
            .id("installer")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .size_full()
            .overflow_hidden()
            .font_family(SharedString::from("Geist"))
            .font_weight(gpui_kit::gpui::FontWeight::MEDIUM)
            .bg(hex(0xffffff))
            // Figma: white to 21.154%, then to #CDFFDE at the bottom.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .top(px(H * 0.21154))
                    .bg(linear_gradient(180.0, linear_color_stop(hex(0xffffff), 0.0), linear_color_stop(hex(0xcdffde), 1.0))),
            )
            // The download badge, cropped by the window's bottom-left corner.
            .child(
                img("installer/download-badge.svg")
                    .absolute()
                    .left(px(-95.0))
                    .top(px(311.0 + badge_rise))
                    .size(px(267.0))
                    .opacity(b),
            )
            // Drag strip (the OS treats it as caption on Windows).
            .child(
                div().id("drag").absolute().top_0().left_0().right_0().h(px(32.0)).window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .absolute()
                    .left(px(EDGE))
                    .top(px(43.0))
                    .flex()
                    .items_center()
                    .gap(px(11.0))
                    .child(img("installer/app-icon.png").size(px(46.0)).rounded(px(11.0)).flex_none())
                    .child(
                        div().text_size(px(32.0)).line_height(px(40.0)).text_color(hex(0x000000)).whitespace_nowrap().child(
                            match self.flow {
                                Flow::Install => "Lookout Installer",
                                Flow::Uninstall => "Lookout Uninstaller",
                            },
                        ),
                    ),
            )
            .child(page)
            .child(actions)
            .child(
                div()
                    .absolute()
                    .left(px(FOOTER_X))
                    .top(px(484.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(img("installer/hack-club-flag.svg").w(px(73.333)).h(px(24.444)).ml(px(2.22)))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(15.0))
                            .font_weight(gpui_kit::gpui::FontWeight::NORMAL)
                            .text_color(hex(0x000000))
                            .child("© Hack Club, a 501(c)(3) nonprofit"),
                    ),
            )
            .when(caption, |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .flex()
                        .child(
                            HeaderBtn::icon("min", HeaderBtnKind::Action, Icon::LkWinMinimize, 10.0, |_, window, _| {
                                window.minimize_window()
                            })
                            .tooltip("Minimize")
                            .caption(),
                        )
                        .child(
                            HeaderBtn::icon("close", HeaderBtnKind::Action, Icon::LkWinClose, 10.0, move |_, _, cx| {
                                if !working {
                                    cx.quit()
                                }
                            })
                            .tooltip("Close")
                            .caption()
                            .danger()
                            .disabled(working),
                        ),
                )
            })
    }
}
