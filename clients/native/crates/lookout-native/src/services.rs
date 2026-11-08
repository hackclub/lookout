//! Background services the main window polls: the tip (`useTip`, 15 min),
//! the announcement (`useAnnouncement`, 15 min) and, on Linux, the release
//! check (`useLinuxUpdate`, 6 h) — plus the views they feed: the TipDrawer
//! and UpdateDrawer sheet contents and the gallery's AnnouncementBanner.

use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, AsyncApp, ClipboardItem, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, img, px,
};
use serde::Deserialize;

use crate::app::{self, api_base, store, store_mut};
use crate::images::{self, Remote};
use crate::motion::{Anim, Curve, Ease};
use crate::theme::{self, Theme, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::icon::Icon;
use crate::ui::text::{MONO, TextExt};

// ── Tip ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tip {
    pub id: String,
    #[serde(default)]
    pub badge: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default = "default_trigger")]
    pub trigger: String,
    #[serde(default)]
    pub require_unlinked: bool,
}

fn default_trigger() -> String {
    "any".into()
}

pub async fn fetch_tip(cx: &AsyncApp) -> Option<Option<Tip>> {
    let (core, base) = cx.update(|cx| (app::state(cx).core.clone(), api_base(cx)));
    let v = app::tokio_async(cx, async move { core.tip(&base, Some("lookout-desktop"), Some(app::VERSION)).await }).await.ok()?;
    Some(v.get("tip").and_then(|t| serde_json::from_value(t.clone()).ok()))
}

/// `shouldShowTip(tip, moment, token)`.
pub fn should_show_tip(tip: &Tip, moment: &str, token: &str, cx: &App) -> bool {
    let st = store(cx);
    if let Some(program) = &tip.program {
        if tip.trigger == "deep_link" && st.deeplink_sessions.get(program).map(String::as_str) != Some(token) {
            return false;
        }
        if tip.require_unlinked && st.program_links.contains_key(program) {
            return false;
        }
    }
    if let Some(rec) = st.tip_moments.get(&tip.id) {
        if rec.token != token || rec.moments.iter().any(|m| m == moment) {
            return false;
        }
    }
    true
}

pub fn mark_tip_moment(tip: &Tip, moment: &str, token: &str, cx: &mut App) {
    let st = store_mut(cx);
    let rec = st.tip_moments.entry(tip.id.clone()).or_default();
    if rec.token != token {
        rec.token = token.to_string();
        rec.moments.clear();
    }
    if !rec.moments.iter().any(|m| m == moment) {
        rec.moments.push(moment.to_string());
    }
    st.save();
}

pub const HERO_HEIGHT: f32 = 210.0;

/// The tip sheet's hero image (fades in over 220ms ease-out on load; an
/// image that was already loaded shows at once).
pub struct TipHero {
    pub url: String,
    fade: Option<Anim>,
}

impl TipHero {
    pub fn new(url: String, cx: &mut Context<Self>) -> Self {
        cx.observe(&images::cache(cx), |_, _, cx| cx.notify()).detach();
        Self { url, fade: None }
    }
}

impl Render for TipHero {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        // GPUI clips to rectangles, so the hero rounds its own top corners to
        // sit inside the sheet's 24px radius (inner radius = 24 − 1px border).
        let mut d = div().h(px(HERO_HEIGHT)).flex_none().bg(t.bg_sunken).relative().rounded_t(px(23.0));
        if let Remote::Ready(image, _) = images::get(&self.url, cx) {
            let fade = self.fade.get_or_insert_with(|| Anim::from_to(0.0, 1.0, Curve::tween(220.0, Ease::EASE_OUT)));
            if fade.tick() {
                window.request_animation_frame();
            }
            d = d.child(img(image).size_full().object_fit_cover().rounded_t(px(23.0)).opacity(fade.get()));
        }
        d
    }
}

trait CoverFit {
    fn object_fit_cover(self) -> Self;
}
impl CoverFit for gpui_kit::gpui::Img {
    fn object_fit_cover(self) -> Self {
        use gpui_kit::gpui::StyledImage;
        self.object_fit(gpui_kit::gpui::ObjectFit::Cover)
    }
}

pub struct TipBody {
    pub tip: Tip,
    pub has_hero: bool,
}

impl Render for TipBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .flex()
            .flex_col()
            .items_start()
            .when_some(self.tip.badge.clone(), |d, b| {
                d.child(
                    div()
                        .px(px(8.0))
                        .py(px(3.0))
                        .mb(px(10.0))
                        .rounded(px(4.0))
                        .bg(t.text_primary)
                        .text_color(t.text_inverse)
                        .text_size(px(12.0))
                        .line_height(px(12.0 * 1.15))
                        .w600()
                        .child(b),
                )
            })
            .child(
                div()
                    .text_size(px(18.0))
                    .line_height(px(18.0 * 1.25))
                    .w600()
                    .text_color(t.text_primary)
                    .when(!self.has_hero, |d| d.pr(px(42.0)))
                    .child(self.tip.title.clone()),
            )
            .child(div().mt(px(8.0)).ts_lh(14.0, 1.45).text_color(t.text_secondary).child(self.tip.body.clone()))
    }
}

// ── Announcement ────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Announcement {
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub url: Option<String>,
}

pub async fn fetch_announcement(cx: &AsyncApp) -> Option<Option<Announcement>> {
    let (core, base) = cx.update(|cx| (app::state(cx).core.clone(), api_base(cx)));
    let v = app::tokio_async(cx, async move { core.announcement(&base, Some("lookout-desktop"), Some(app::VERSION)).await })
        .await
        .ok()?;
    Some(v.get("announcement").and_then(|a| serde_json::from_value(a.clone()).ok()))
}

/// `AnnouncementBanner`: the level colour at 10% fill with a 1px border.
pub fn announcement_banner(a: &Announcement, t: &Theme) -> AnyElement {
    let color = match a.level.as_str() {
        "success" => theme::hex(0x22c55e),
        "warning" => theme::hex(0xf59e0b),
        "danger" => theme::hex(0xef4444),
        _ => theme::hex(0x3b82f6),
    };
    let url = a.url.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(8.0))
        .bg(theme::fade(color, 0x1a as f32 / 255.0))
        .border_1()
        .border_color(color)
        .rounded(px(8.0))
        .child(div().ts_lh(12.0, 1.4).text_color(t.text_primary).min_w(px(0.0)).child(a.message.clone()))
        .when_some(url, |d, url| {
            d.child(
                Button::new("announcement-open", "Open")
                    .variant(Variant::Secondary)
                    .size(BtnSize::Sm)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        })
        .into_any_element()
}

// ── Linux update ────────────────────────────────────────────────────────

pub const RELEASES_URL: &str = "https://github.com/hackclub/lookout/releases/latest";
const MANIFEST_URL: &str = "https://github.com/hackclub/lookout/releases/latest/download/latest.json";

#[derive(Clone, Debug, PartialEq)]
pub struct LinuxUpdate {
    pub version: String,
    pub command: Option<String>,
    pub fallback: Option<String>,
}

pub fn compare_versions(a: &str, b: &str) -> i64 {
    let pa: Vec<i64> = a.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let pb: Vec<i64> = b.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect();
    for i in 0..3 {
        let d = pa.get(i).copied().unwrap_or(0) - pb.get(i).copied().unwrap_or(0);
        if d != 0 {
            return d;
        }
    }
    0
}

/// `linux_install_kind`: which package manager owns this binary, and whether
/// its Lookout repository is set up.
fn linux_install_kind() -> (&'static str, bool) {
    let exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let owns = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd).args(args).arg(&exe).output().map(|o| o.status.success()).unwrap_or(false)
    };
    if owns("dpkg", &["-S"]) {
        return ("apt", std::path::Path::new("/etc/apt/sources.list.d/lookout.sources").exists());
    }
    if owns("rpm", &["-qf"]) {
        return ("rpm", std::path::Path::new("/etc/yum.repos.d/lookout.repo").exists());
    }
    if owns("pacman", &["-Qo"]) {
        let enrolled = std::fs::read_to_string("/etc/pacman.conf").map(|c| c.contains("[lookout]")).unwrap_or(false);
        return ("pacman", enrolled);
    }
    ("unknown", false)
}

/// `instructionsFor` (updateCommand.ts).
fn instructions(manager: &str, enrolled: bool) -> (Option<String>, Option<String>) {
    match (manager, enrolled) {
        ("apt", true) => (Some("sudo apt update && sudo apt install lookout".into()), None),
        ("apt", false) => (None, Some("This copy was installed before Lookout had an apt repository. Download the latest .deb once, and every update after it arrives through apt.".into())),
        ("rpm", true) => (Some("sudo dnf upgrade lookout".into()), None),
        ("rpm", false) => (None, Some("This copy was installed before Lookout had a dnf repository. Download the latest .rpm once, and every update after it arrives through dnf.".into())),
        ("pacman", true) => (Some("sudo pacman -Syu".into()), None),
        ("pacman", false) => (None, Some("Add the Lookout repository to get updates, or download the latest package.".into())),
        _ => (None, Some("Download the latest version to update.".into())),
    }
}

pub async fn check_linux_update(cx: &AsyncApp) -> Option<LinuxUpdate> {
    // An AppImage updates itself (updater/); only packaged installs get this.
    if !cfg!(target_os = "linux") || crate::updater::supported() {
        return None;
    }
    let http = cx.update(|cx| app::state(cx).http.clone());
    let latest = app::tokio_async(cx, async move {
        let res = http.get(MANIFEST_URL).timeout(Duration::from_secs(20)).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let v: serde_json::Value = res.json().await.ok()?;
        v.get("version").and_then(|v| v.as_str()).map(|s| s.trim_start_matches('v').to_string())
    })
    .await?;
    if compare_versions(&latest, app::VERSION) <= 0 {
        return None;
    }
    if cx.update(|cx| store(cx).update_nag_dismissed.as_deref() == Some(latest.as_str())) {
        return None;
    }
    let (manager, enrolled) = app::tokio_async(cx, async { tokio::task::spawn_blocking(linux_install_kind).await.unwrap_or(("unknown", false)) }).await;
    let (command, fallback) = instructions(manager, enrolled);
    Some(LinuxUpdate { version: latest, command, fallback })
}

pub struct UpdateBody {
    pub update: LinuxUpdate,
    copied: bool,
}

impl UpdateBody {
    pub fn new(update: LinuxUpdate) -> Self {
        Self { update, copied: false }
    }
}

impl Render for UpdateBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let u = self.update.clone();
        let me = cx.entity();
        div()
            .flex()
            .flex_col()
            .items_start()
            .w_full()
            .child(
                div()
                    .text_size(px(18.0))
                    .line_height(px(18.0 * 1.25))
                    .w600()
                    .text_color(t.text_primary)
                    .pr(px(42.0))
                    .child(format!("A new version of Lookout ({}) is out.", u.version)),
            )
            .child(div().mt(px(8.0)).ts_lh(14.0, 1.45).text_color(t.text_secondary).child(if u.command.is_some() {
                "On Linux, Lookout updates through your package manager. Run this to get it:".to_string()
            } else {
                u.fallback.clone().unwrap_or_default()
            }))
            .map(|d| match u.command.clone() {
                Some(cmd) => d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .w_full()
                        .mt(px(16.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .bg(t.bg_sunken)
                        .border_1()
                        .border_color(t.border_default)
                        .rounded(px(8.0))
                        .child(div().flex_1().min_w(px(0.0)).ts_lh(12.0, 1.5).font_family(MONO).text_color(t.text_primary).child(cmd.clone()))
                        .child(
                            Button::empty("update-copy")
                                .variant(Variant::Secondary)
                                .size(BtnSize::Sm)
                                .gap(6.0)
                                .child(if self.copied {
                                    Icon::CheckBold.el(13.0, t.text_secondary)
                                } else {
                                    Icon::CopyRegular.el(13.0, t.text_secondary)
                                })
                                .child(if self.copied { "Copied" } else { "Copy" })
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(cmd.clone()));
                                    me.update(cx, |b, cx| {
                                        b.copied = true;
                                        cx.spawn(async move |this, cx| {
                                            cx.background_executor().timer(Duration::from_millis(1600)).await;
                                            this.update(cx, |b, cx| {
                                                b.copied = false;
                                                cx.notify();
                                            })
                                            .ok();
                                        })
                                        .detach();
                                        cx.notify();
                                    })
                                }),
                        ),
                ),
                None => d.child(
                    div().mt(px(16.0)).child(
                        Button::new("update-downloads", "Open downloads")
                            .variant(Variant::Secondary)
                            .size(BtnSize::Sm)
                            .on_click(|_, _, cx| cx.open_url(RELEASES_URL)),
                    ),
                ),
            })
    }
}

#[allow(unused)]
fn _u(_: Entity<UpdateBody>, _: SharedString) {}
