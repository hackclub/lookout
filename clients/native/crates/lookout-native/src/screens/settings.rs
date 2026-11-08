//! `SettingsPage.tsx` — a menu and three subpages (Filtered Apps, Linked
//! Programs, Advanced) that slide like routes (spring 460/36/0.7, ±14px;
//! enter fades in over 160ms after 40ms, exit fades out over 140ms).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, Context, CursorStyle, ElementId, Entity, EventEmitter, Hsla, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, canvas, div, img, px,
};
use lookout_core::apps::AppEntry;

use crate::app::{self, store, store_mut};
use crate::images::{self, Remote};
use crate::motion::{self, Anim, Curve, Ease, Spring};
use crate::store::DEFAULT_API_BASE;
use crate::theme::{self, Theme, eid, is_linux, is_mac, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::header_bar::HeaderNav;
use crate::ui::icon::Icon;
use crate::ui::input::{InputEvent, TextInput, new_input};
use crate::ui::paint;
use crate::ui::text::{MONO, TextExt};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sub {
    Menu,
    FilteredApps,
    LinkedPrograms,
    Advanced,
}

pub enum SettingsEvent {
    Back,
    Confirm { title: String, message: String, ok: String, action: ConfirmAction },
    Reload,
}

pub enum ConfirmAction {
    Unlink(String),
    SwitchServer(String),
}

struct Layer {
    sub: Sub,
    x: Anim,
    opacity: Anim,
    leaving: bool,
    scroll: ScrollHandle,
}

pub struct SettingsPage {
    sub: Sub,
    layers: Vec<Layer>,
    is_wayland: bool,
    // Filtered apps
    apps: Option<Vec<AppEntry>>,
    icons: HashMap<String, Option<String>>,
    icon_requested: HashSet<String>,
    icon_fade: HashMap<String, Anim>,
    pinned: Vec<String>,
    search: Entity<TextInput>,
    search_focused: bool,
    // Advanced
    server: Entity<TextInput>,
    server_focused: bool,
    server_error: Option<String>,
    saving: bool,
    unlinking: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsPage {}

/// `normalizeServerUrl`.
pub fn normalize_server_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.to_lowercase().starts_with("http://") || trimmed.to_lowercase().starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

impl SettingsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let search = new_input("Search apps...", cx);
        let server = new_input("https://lookout.hackclub.com", cx);
        let current = store(cx).api_base();
        server.update(cx, |i, cx| i.set_text(current, cx));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
            match ev {
                InputEvent::Focus => this.search_focused = true,
                InputEvent::Blur => this.search_focused = false,
                _ => {}
            }
            cx.notify();
        }));
        subs.push(cx.subscribe(&server, |this, _, ev: &InputEvent, cx| {
            match ev {
                InputEvent::Focus => this.server_focused = true,
                InputEvent::Blur => this.server_focused = false,
                InputEvent::Changed => this.server_error = None,
                InputEvent::Submit => this.save_server(cx),
                _ => {}
            }
            cx.notify();
        }));
        subs.push(cx.observe(&images::cache(cx), |_, _, cx| cx.notify()));
        Self {
            sub: Sub::Menu,
            layers: vec![Layer { sub: Sub::Menu, x: Anim::new(0.0), opacity: Anim::new(1.0), leaving: false, scroll: ScrollHandle::new() }],
            is_wayland: std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false),
            apps: None,
            icons: HashMap::new(),
            icon_requested: HashSet::new(),
            icon_fade: HashMap::new(),
            pinned: Vec::new(),
            search,
            search_focused: false,
            server,
            server_focused: false,
            server_error: None,
            saving: false,
            unlinking: None,
            _subs: subs,
        }
    }

    pub fn go(&mut self, sub: Sub, cx: &mut Context<Self>) {
        if sub == self.sub {
            return;
        }
        let dir = if sub == Sub::Menu { -1.0 } else { 1.0 };
        for l in self.layers.iter_mut().filter(|l| !l.leaving) {
            l.leaving = true;
            l.x.set(-14.0 * dir, Curve::Spring(Spring::ROUTE));
            l.opacity.set(0.0, Curve::tween(140.0, Ease::EASE_OUT));
        }
        if let Some(i) = self.layers.iter().position(|l| l.sub == sub) {
            // Still animating out: AnimatePresence re-enters the same child
            // from its current x/opacity rather than from `enter`.
            let mut l = self.layers.remove(i);
            l.leaving = false;
            l.x.set(0.0, Curve::Spring(Spring::ROUTE));
            l.opacity.set_delayed(1.0, Curve::tween(160.0, Ease::EASE_OUT), 40.0);
            self.layers.push(l);
        } else {
            let mut opacity = Anim::new(0.0);
            opacity.set_delayed(1.0, Curve::tween(160.0, Ease::EASE_OUT), 40.0);
            self.layers.push(Layer { sub, x: Anim::from_to(14.0 * dir, 0.0, Curve::Spring(Spring::ROUTE)), opacity, leaving: false, scroll: ScrollHandle::new() });
        }
        self.sub = sub;
        if sub == Sub::FilteredApps {
            self.pinned = store(cx).blacklisted_apps.clone();
            self.load_apps(cx);
        }
        cx.notify();
    }

    /// Back: subpage → menu; menu → gallery.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.sub == Sub::Menu {
            cx.emit(SettingsEvent::Back);
        } else {
            self.go(Sub::Menu, cx);
        }
    }

    pub fn header(&self, me: Entity<Self>) -> HeaderNav {
        let title = match self.sub {
            Sub::Menu => "Settings",
            Sub::FilteredApps => "Filtered Apps",
            Sub::LinkedPrograms => "Linked Programs",
            Sub::Advanced => "Advanced",
        };
        HeaderNav {
            title: title.into(),
            subtitle: None,
            on_back: Some(Rc::new(move |_: &mut Window, cx: &mut App| me.update(cx, |s, cx| s.back(cx)))),
        }
    }

    fn load_apps(&mut self, cx: &mut Context<Self>) {
        if self.apps.is_some() {
            return;
        }
        let fut = app::tokio(cx, async {
            tokio::task::spawn_blocking(lookout_core::apps::list_installed_apps_blocking).await.unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let list = fut.await;
            this.update(cx, |s, cx| {
                s.apps = Some(list);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn request_icon(&mut self, name: &str, path: Option<String>, cx: &mut Context<Self>) {
        if self.icon_requested.contains(name) {
            return;
        }
        self.icon_requested.insert(name.to_string());
        let Some(path) = path else { return };
        let fut = app::tokio(cx, async move {
            tokio::task::spawn_blocking(move || lookout_core::apps::app_icon(path)).await.ok().flatten()
        });
        let name = name.to_string();
        cx.spawn(async move |this, cx| {
            let icon = fut.await;
            this.update(cx, |s, cx| {
                // Fade in only icons that arrive after the row mounted.
                s.icon_fade.insert(name.clone(), Anim::from_to(0.0, 1.0, Curve::tween(200.0, Ease::EASE_OUT)));
                s.icons.insert(name, icon.map(|b64| format!("data:image/png;base64,{b64}")));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_app(&mut self, name: &str, cx: &mut Context<Self>) {
        let st = store_mut(cx);
        if let Some(i) = st.blacklisted_apps.iter().position(|a| a == name) {
            st.blacklisted_apps.remove(i);
        } else {
            st.blacklisted_apps.push(name.to_string());
            st.blacklisted_apps.sort();
        }
        let list = st.blacklisted_apps.clone();
        st.save();
        let _ = app::state(cx).core.set_blacklisted_apps(list);
        cx.notify();
    }

    fn save_server(&mut self, cx: &mut Context<Self>) {
        let raw = self.server.read(cx).text();
        let Some(target) = normalize_server_url(&raw) else { return };
        let current = store(cx).api_base();
        if target == current || self.saving {
            return;
        }
        if target != DEFAULT_API_BASE {
            cx.emit(SettingsEvent::Confirm {
                title: "Switch Lookout server".into(),
                message: format!(
                    "Point this app at {target}?\n\nAll new recordings will upload there instead of the official Lookout server. Only continue if someone from Hack Club asked you to."
                ),
                ok: "Switch".into(),
                action: ConfirmAction::SwitchServer(target),
            });
        } else {
            self.probe_and_save(target, cx);
        }
    }

    pub fn probe_and_save(&mut self, target: String, cx: &mut Context<Self>) {
        self.saving = true;
        self.server_error = None;
        let core = app::state(cx).core.clone();
        let t2 = target.clone();
        let fut = app::tokio(cx, async move { core.programs(&t2, Some(Duration::from_millis(6000))).await });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |s, cx| {
                s.saving = false;
                match res {
                    Ok(_) => {
                        let st = store_mut(cx);
                        st.api_base = if target == DEFAULT_API_BASE { None } else { Some(target) };
                        st.gallery_cache.clear();
                        st.save();
                        cx.emit(SettingsEvent::Reload);
                    }
                    Err(e) => {
                        s.server_error = Some(if e.message.to_lowercase().contains("timed out") {
                            "Could not reach the server (timed out).".into()
                        } else {
                            format!("Could not reach the server: {}", e.message)
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn do_unlink(&mut self, program: String, cx: &mut Context<Self>) {
        self.unlinking = Some(program.clone());
        let task = crate::programs::unlink(program, cx);
        cx.spawn(async move |this, cx| {
            task.await;
            this.update(cx, |s, cx| {
                s.unlinking = None;
                if store(cx).program_links.is_empty() {
                    s.go(Sub::Menu, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ── Pieces ──────────────────────────────────────────────────────────

    /// `PageChrome`: back button (not on Linux), title (not on Linux) and
    /// description.
    fn chrome(&self, title: &str, description: AnyElement, t: &Theme, cx: &mut Context<Self>) -> gpui_kit::gpui::Div {
        let me = cx.entity();
        let mut d = div().flex().flex_col().size_full().max_w(px(480.0)).mx_auto().p(px(16.0));
        if !is_linux() {
            let back = Button::empty(ElementId::Name(format!("settings-back-{title}").into()))
                .variant(Variant::Secondary)
                .size(BtnSize::Sm)
                .card_style(t)
                .on_click(move |_, _, cx| me.update(cx, |s, cx| s.back(cx)));
            let back = if is_mac() {
                back.child("← Back")
            } else {
                back.gap(4.0).child(Icon::CaretLeftBold.el(14.0, t.text_secondary)).child("Back")
            };
            d = d.child(div().flex_none().mb(px(16.0)).child(back));
        }
        d.child(
            div()
                .flex_none()
                .mb(px(16.0))
                .when(!is_linux(), |d| d.child(div().ts(20.0).w700().text_color(t.text_primary).mb(px(4.0)).child(title.to_string())))
                .child(description),
        )
    }

    fn menu(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let n = store(cx).blacklisted_apps.len();
        let links = store(cx).program_links.len();
        let filtered_desc = if self.is_wayland {
            "Not supported on Wayland".to_string()
        } else if n > 0 {
            format!("{n} app{} filtered", if n == 1 { "" } else { "s" })
        } else {
            "Black out selected apps in captures".to_string()
        };
        let adv_desc = if store(cx).is_default_api_base() {
            "Developer options".to_string()
        } else {
            format!("Custom server: {}", store(cx).api_base().trim_start_matches("https://").trim_start_matches("http://"))
        };
        let me = cx.entity();
        let (m1, m2, m3) = (me.clone(), me.clone(), me);
        let mut list = div().rounded(px(10.0)).border_1().border_color(t.border_default).bg(t.bg_surface).p(px(4.0)).child(
            menu_row("menu-filtered", Icon::FunnelFill, "Filtered Apps", filtered_desc, t, move |_, cx| {
                m1.update(cx, |s, cx| s.go(Sub::FilteredApps, cx))
            }),
        );
        if links > 0 {
            list = list.child(menu_row(
                "menu-linked",
                Icon::LinkBold,
                "Linked Programs",
                format!("{links} program{} linked", if links == 1 { "" } else { "s" }),
                t,
                move |_, cx| m2.update(cx, |s, cx| s.go(Sub::LinkedPrograms, cx)),
            ));
        }
        list = list.child(menu_row("menu-advanced", Icon::WrenchFill, "Advanced", adv_desc, t, move |_, cx| {
            m3.update(cx, |s, cx| s.go(Sub::Advanced, cx))
        }));
        if crate::updater::supported() {
            let me = cx.entity();
            list = list.child(ToggleRow {
                id: "menu-beta",
                icon: Icon::LkPower,
                title: "Beta updates",
                description: "Get new versions early. They can be rougher.".into(),
                on: crate::updater::beta_enabled(),
                t: t.clone(),
                on_toggle: Rc::new(move |on, cx| {
                    crate::updater::set_beta(on, cx);
                    me.update(cx, |_, cx| cx.notify());
                }),
            });
        }
        let desc = div().ts_lh(12.0, 1.5).text_color(t.text_secondary).child("Configure Lookout.").into_any_element();
        self.chrome("Settings", desc, t, cx).child(list).into_any_element()
    }

    fn filtered_apps(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let n = store(cx).blacklisted_apps.len();
        let desc = div()
            .ts_lh(12.0, 1.5)
            .text_color(t.text_secondary)
            .child("Selected apps will be blacked out in monitor screen captures.")
            .when(n > 0, |d| {
                d.child(div().text_color(theme::status::warning()).child(format!(" {n} app{} filtered.", if n == 1 { "" } else { "s" })))
            })
            .into_any_element();
        let chrome = self.chrome("Filtered Apps", desc, t, cx);
        if self.is_wayland {
            return chrome
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(t.border_default)
                        .bg(t.bg_surface)
                        .p(px(24.0))
                        .text_color(t.text_tertiary)
                        .ts_lh(12.0, 1.5)
                        .text_center()
                        .child("App filtering is not supported on Wayland."),
                )
                .into_any_element();
        }
        let search = div()
            .flex_none()
            .mb(px(12.0))
            .child(
                crate::ui::input::field_box(&self.search)
                    .w_full()
                    .px(px(12.0))
                    .py(px(8.0))
                    .ts(13.0)
                    .text_color(t.text_primary)
                    .bg(t.bg_surface)
                    .border_1()
                    .border_color(if self.search_focused { t.border_hover } else { t.border_default })
                    .rounded(px(8.0))
                    .child(self.search.clone()),
            );
        let mut list = div()
            .id("apps-list")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .rounded(px(10.0))
            .border_1()
            .border_color(t.border_default)
            .bg(t.bg_surface);
        match self.apps.clone() {
            None => {
                window.request_animation_frame();
                let mut rows = div().p(px(4.0));
                // The rows' clock starts when they mount (motion's `delay`
                // only holds the first keyframe before the first cycle).
                let start = *window.use_keyed_state("apps-skeleton-start", cx, |_, _| motion::now()).read(cx);
                for i in 0..12 {
                    // opacity [0.45, 0.9, 0.45] over 1.2s easeInOut, delay i × 50ms, repeat ∞.
                    let since = motion::now() - start - i as f64 * 50.0;
                    let tt = if since <= 0.0 { 0.0 } else { (since.rem_euclid(1200.0) / 1200.0) as f32 };
                    let o = if tt < 0.5 {
                        0.45 + 0.45 * Ease::EASE_IN_OUT.apply(tt * 2.0)
                    } else {
                        0.9 - 0.45 * Ease::EASE_IN_OUT.apply((tt - 0.5) * 2.0)
                    };
                    let w = 40 + ((i * 23) % 40);
                    rows = rows.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(8.0))
                            .py(px(6.0))
                            .opacity(o)
                            .child(div().size(px(18.0)).rounded(px(6.0)).bg(t.bg_selected).mr(px(2.0)))
                            .child(div().size(px(20.0)).rounded(px(6.0)).bg(t.bg_selected))
                            .child(div().h(px(12.0)).w(gpui_kit::gpui::relative(w as f32 / 100.0)).rounded(px(6.0)).bg(t.bg_selected)),
                    );
                }
                list = list.child(rows);
            }
            Some(apps) => {
                let q = self.search.read(cx).text().to_lowercase();
                let blacklisted: HashSet<String> = store(cx).blacklisted_apps.iter().cloned().collect();
                let mut all: Vec<AppEntry> = apps;
                for name in &blacklisted {
                    if !all.iter().any(|a| &a.name == name) {
                        all.push(AppEntry { name: name.clone(), path: None, running: false });
                    }
                }
                let pinned: HashSet<String> = self.pinned.iter().cloned().collect();
                let mut filtered: Vec<AppEntry> = all.into_iter().filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q)).collect();
                filtered.sort_by(|a, b| {
                    let ga = if pinned.contains(&a.name) { 0 } else if a.running { 1 } else { 2 };
                    let gb = if pinned.contains(&b.name) { 0 } else if b.running { 1 } else { 2 };
                    ga.cmp(&gb).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                if filtered.is_empty() {
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .p(px(24.0))
                            .text_color(t.text_tertiary)
                            .ts(12.0)
                            .child(if q.is_empty() { "No apps detected" } else { "No matching apps" }),
                    );
                } else {
                    let mut rows = div().p(px(4.0));
                    for a in filtered.iter().take(400) {
                        self.request_icon(&a.name, a.path.clone(), cx);
                        let checked = blacklisted.contains(&a.name);
                        let icon = self.icons.get(&a.name).cloned().flatten();
                        let fade = self.icon_fade.get_mut(&a.name).map(|f| {
                            f.tick();
                            f.get()
                        });
                        if fade.map(|f| f < 1.0).unwrap_or(false) {
                            window.request_animation_frame();
                        }
                        let me = cx.entity();
                        let name = a.name.clone();
                        rows = rows.child(app_row(&a.name, checked, icon, fade.unwrap_or(1.0), t, window, cx, move |_, cx| {
                            me.update(cx, |s, cx| s.toggle_app(&name, cx))
                        }));
                    }
                    list = list.child(rows);
                }
            }
        }
        chrome.child(search).child(list).into_any_element()
    }

    fn linked_programs(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let desc = div()
            .ts_lh(12.0, 1.5)
            .text_color(t.text_secondary)
            .child("Programs linked to this device can start a timelapse straight from the + menu.")
            .into_any_element();
        let registry = crate::programs::registry(cx);
        let mut links: Vec<_> = store(cx).program_links.values().cloned().collect();
        links.sort_by(|a, b| a.program.cmp(&b.program));
        let mut list = div().rounded(px(10.0)).border_1().border_color(t.border_default).bg(t.bg_surface).p(px(4.0));
        for link in links {
            let reg = registry.iter().find(|p| p.name == link.program);
            let label = reg
                .and_then(|p| p.display_name.clone())
                .or_else(|| link.display_name.clone())
                .unwrap_or_else(|| link.program.clone());
            let icon_url = reg.and_then(|p| p.icon_url.clone()).or_else(|| link.icon_url.clone());
            let host = url::Url::parse(&link.pair_url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
            let icon: AnyElement = match icon_url.as_deref().map(|u| images::get(u, cx)) {
                Some(Remote::Ready(i, _)) => img(i).size(px(32.0)).into_any_element(),
                _ => Icon::LinkBold.el(16.0, t.text_secondary).into_any_element(),
            };
            let busy = self.unlinking.is_some();
            let me = cx.entity();
            let (program, label2) = (link.program.clone(), label.clone());
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .child(
                        div()
                            .size(px(32.0))
                            .rounded(px(8.0))
                            .bg(t.bg_selected)
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .flex_none()
                            .child(icon),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(div().ts(13.0).w500().text_color(t.text_primary).overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.clone()))
                            .child(
                                div()
                                    .ts(12.0)
                                    .text_color(t.text_tertiary)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(format!("Linked {} · {host}", crate::fmt::numeric_date(&link.paired_at))),
                            ),
                    )
                    .child(
                        Button::new(eid("unlink", &link.program), if self.unlinking.as_deref() == Some(link.program.as_str()) { "Unlinking…" } else { "Unlink" })
                            .variant(Variant::Secondary)
                            .size(BtnSize::Sm)
                            .card_style(t)
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let (program, label) = (program.clone(), label2.clone());
                                me.update(cx, |_, cx| {
                                    cx.emit(SettingsEvent::Confirm {
                                        title: "Unlink program".into(),
                                        message: format!(
                                            "Unlink {label} from this device?\n\nStarting a session from the + menu will go through your browser again until you re-link."
                                        ),
                                        ok: "Unlink".into(),
                                        action: ConfirmAction::Unlink(program),
                                    })
                                })
                            }),
                    ),
            );
        }
        self.chrome("Linked Programs", desc, t, cx).child(list).into_any_element()
    }

    fn advanced(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let desc = div()
            .ts_lh(12.0, 1.5)
            .text_color(t.text_secondary)
            .child("Options for testing and development. Please don't touch anything if you don't know what you're doing.")
            .into_any_element();
        let raw = self.server.read(cx).text();
        let normalized = normalize_server_url(&raw);
        let invalid = !raw.trim().is_empty() && normalized.is_none();
        let current = store(cx).api_base();
        let dirty = normalized.as_deref().map(|n| n != current).unwrap_or(false);
        let is_default = store(cx).is_default_api_base();
        let error = self.server_error.clone();
        let border = if error.is_some() || invalid {
            theme::status::danger()
        } else if self.server_focused {
            t.border_hover
        } else {
            t.border_default
        };
        let (me1, me2) = (cx.entity(), cx.entity());
        let card = div()
            .rounded(px(10.0))
            .border_1()
            .border_color(t.border_default)
            .bg(t.bg_surface)
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .child(div().ts(12.0).w500().text_color(t.text_secondary).mb(px(4.0)).child("Lookout server"))
                    .child(
                        crate::ui::input::field_box(&self.server)
                            .w_full()
                            .px(px(12.0))
                            .py(px(8.0))
                            .ts(13.0)
                            .font_family(MONO)
                            .text_color(t.text_primary)
                            .bg(t.bg_surface)
                            .border_1()
                            .border_color(border)
                            .rounded(px(8.0))
                            .child(self.server.clone()),
                    )
                    .child(div().ts_lh(12.0, 1.5).text_color(t.text_tertiary).mt(px(4.0)).child("Please enter a VALID lookout service URL."))
                    .when(invalid, |d| {
                        d.child(div().ts(12.0).text_color(theme::status::danger()).child("Enter a valid HTTPS URL (e.g. https://lookout-stage.example.com)."))
                    })
                    .when_some(error, |d, e| d.child(div().ts(12.0).text_color(theme::status::danger()).child(e))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        Button::new("server-save", if self.saving { "Checking…" } else { "Save" })
                            .size(BtnSize::Sm)
                            .disabled(!dirty || invalid || self.saving)
                            .on_click(move |_, _, cx| me1.update(cx, |s, cx| s.save_server(cx))),
                    )
                    .when(!is_default, |d| {
                        d.child(
                            Button::new("server-reset", "Reset to default")
                                .variant(Variant::Secondary)
                                .size(BtnSize::Sm)
                                .disabled(self.saving)
                                .on_click(move |_, _, cx| {
                                    me2.update(cx, |s, cx| {
                                        s.server.update(cx, |i, cx| i.set_text(DEFAULT_API_BASE, cx));
                                        s.probe_and_save(DEFAULT_API_BASE.to_string(), cx);
                                    })
                                }),
                        )
                    }),
            );
        self.chrome("Advanced", desc, t, cx)
            .child(card)
            .when(!is_default, |d| {
                d.child(
                    div()
                        .mt(px(12.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme::status::warning())
                        .text_color(theme::status::warning())
                        .ts_lh(12.0, 1.5)
                        .child("Using a custom server. Timelapses recorded here won't appear on the default Lookout server."),
                )
            })
            .into_any_element()
    }
}

/// `.lookout-app-row`: hover fills the background layer (120ms ease-out);
/// press scales the layer to 0.96 (120ms ease-out). Content stays put.
struct RowAnim {
    hover: Anim,
    press: Anim,
}

fn row_states(id: &ElementId, window: &mut Window, cx: &mut App) -> (Entity<RowAnim>, f32, f32) {
    let st = window.use_keyed_state(id.clone(), cx, |_, _| RowAnim { hover: Anim::new(0.0), press: Anim::new(0.0) });
    let moving = st.update(cx, |s, _| s.hover.tick() | s.press.tick());
    if moving {
        window.request_animation_frame();
    }
    let (h, p) = {
        let s = st.read(cx);
        (s.hover.get(), s.press.get())
    };
    (st, h, p)
}


fn pressable_row(id: ElementId, selected_fill: Hsla, window: &mut Window, cx: &mut App) -> gpui_kit::gpui::Stateful<gpui_kit::gpui::Div> {
    let (st, h, p) = row_states(&id, window, cx);
    let fill = theme::mix(theme::transparent(), selected_fill, h.max(p));
    let scale = 1.0 - 0.04 * p;
    let (s1, s2, s3, s4) = (st.clone(), st.clone(), st.clone(), st);
    div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .w_full()
        .rounded(px(8.0))
        .when(!is_linux(), |d| d.cursor(CursorStyle::PointingHand))
        .child(
            canvas(|_, _, _| (), move |bounds, _, window, _| paint::paint_rounded_bg(bounds, 8.0, scale, fill, theme::transparent(), window))
                .absolute()
                .inset_0(),
        )
        .on_hover(move |hov, _, cx| s1.update(cx, |s, cx| {
            s.hover.set_css(if *hov { 1.0 } else { 0.0 }, 120.0, Ease::EASE_OUT);
            cx.notify();
        }))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| s2.update(cx, |s, cx| {
            s.press.set_css(1.0, 120.0, Ease::EASE_OUT);
            cx.notify();
        }))
        .on_mouse_up(MouseButton::Left, move |_, _, cx| s3.update(cx, |s, cx| {
            s.press.set_css(0.0, 120.0, Ease::EASE_OUT);
            cx.notify();
        }))
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| s4.update(cx, |s, cx| {
            s.press.set_css(0.0, 120.0, Ease::EASE_OUT);
            cx.notify();
        }))
}

fn menu_row(
    id: &'static str,
    icon: Icon,
    title: &'static str,
    description: String,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    MenuRow { id, icon, title, description: description.into(), t: t.clone(), on_click: Rc::new(on_click) }
}

#[derive(IntoElement)]
struct MenuRow {
    id: &'static str,
    icon: Icon,
    title: &'static str,
    description: SharedString,
    t: Theme,
    on_click: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl gpui_kit::gpui::RenderOnce for MenuRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = self.t;
        let f = self.on_click;
        pressable_row(self.id.into(), t.bg_selected, window, cx)
            .gap(px(12.0))
            .p(px(12.0))
            .text_color(t.text_primary)
            .child(
                div()
                    .relative()
                    .size(px(32.0))
                    .rounded(px(8.0))
                    .bg(t.bg_selected)
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .child(self.icon.el(18.0, t.text_secondary)),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(div().ts(13.0).w500().child(self.title))
                    .child(div().ts(12.0).text_color(t.text_tertiary).overflow_hidden().whitespace_nowrap().text_ellipsis().child(self.description)),
            )
            .child(div().relative().child(Icon::CaretRightBold.el(16.0, t.text_tertiary)))
            .on_click(move |_, window, cx| f(window, cx))
    }
}

/// A MenuRow whose trailing caret is a checkbox (the "Beta updates" switch).
#[derive(IntoElement)]
struct ToggleRow {
    id: &'static str,
    icon: Icon,
    title: &'static str,
    description: SharedString,
    on: bool,
    t: Theme,
    on_toggle: Rc<dyn Fn(bool, &mut App)>,
}

impl gpui_kit::gpui::RenderOnce for ToggleRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = self.t;
        // The checkbox eases between states over 150ms, like the app rows'.
        let chk = window.use_keyed_state(eid("toggle-chk", self.id), cx, |_, _| Anim::new(if self.on { 1.0 } else { 0.0 }));
        let moving = chk.update(cx, |a, _| {
            let target = if self.on { 1.0 } else { 0.0 };
            if a.target() != target {
                a.set(target, Curve::tween(150.0, Ease::CSS_EASE));
            }
            a.tick()
        });
        if moving {
            window.request_animation_frame();
        }
        let c = chk.read(cx).get();
        let (on, f) = (self.on, self.on_toggle);
        pressable_row(self.id.into(), t.bg_selected, window, cx)
            .gap(px(12.0))
            .p(px(12.0))
            .text_color(t.text_primary)
            .child(
                div()
                    .relative()
                    .size(px(32.0))
                    .rounded(px(8.0))
                    .bg(t.bg_selected)
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .child(self.icon.el(18.0, t.text_secondary)),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(div().ts(13.0).w500().child(self.title))
                    .child(div().ts(12.0).text_color(t.text_tertiary).overflow_hidden().whitespace_nowrap().text_ellipsis().child(self.description)),
            )
            .child(
                div()
                    .relative()
                    .size(px(18.0))
                    .rounded(px(6.0))
                    .border(px(1.5))
                    .border_color(theme::mix(t.border_hover, t.text_primary, c))
                    .bg(theme::mix(theme::transparent(), t.text_primary, c))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .when(on, |d| d.child(Icon::CheckBold.el(12.0, t.text_inverse))),
            )
            .on_click(move |_, _, cx| f(!on, cx))
    }
}

#[allow(clippy::too_many_arguments)]
fn app_row(
    name: &str,
    checked: bool,
    icon: Option<String>,
    icon_opacity: f32,
    t: &Theme,
    window: &mut Window,
    cx: &mut App,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // Checkbox: 150ms CSS `all` transition between unchecked and checked.
    let id = eid("app-row", name);
    let chk = window.use_keyed_state(eid("app-chk", name), cx, |_, _| Anim::new(if checked { 1.0 } else { 0.0 }));
    let moving = chk.update(cx, |a, _| {
        let target = if checked { 1.0 } else { 0.0 };
        if a.target() != target {
            a.set_css(target, 150.0, Ease::CSS_EASE);
        }
        a.tick()
    });
    if moving {
        window.request_animation_frame();
    }
    let c = chk.read(cx).get();
    let red = theme::status::danger();
    let icon_el: AnyElement = match icon.as_deref().map(|u| images::get(u, cx)) {
        Some(Remote::Ready(i, _)) => img(i).size(px(20.0)).opacity(icon_opacity).into_any_element(),
        _ => div().size(px(20.0)).rounded(px(6.0)).bg(t.bg_selected).into_any_element(),
    };
    pressable_row(id, t.bg_selected, window, cx)
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(6.0))
        .ts(13.0)
        .text_color(t.text_primary)
        .child(
            div()
                .relative()
                .size(px(18.0))
                .mr(px(2.0))
                .rounded(px(6.0))
                .border(px(1.5))
                .border_color(theme::mix(t.border_hover, red, c))
                .bg(theme::mix(theme::transparent(), red, c))
                .flex()
                .items_center()
                .justify_center()
                .flex_none()
                .when(checked, |d| d.child(Icon::CheckBold.el(12.0, theme::hex(0xffffff)))),
        )
        .child(div().relative().size(px(20.0)).flex_none().child(icon_el))
        .child(
            div()
                .relative()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .when(checked, |d| d.w500())
                .when(!checked, |d| d.w400())
                .child(name.to_string()),
        )
        .on_click(move |_, window, cx| on_click(window, cx))
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut moving = false;
        for l in &mut self.layers {
            moving |= l.x.tick() | l.opacity.tick();
        }
        self.layers.retain(|l| !(l.leaving && !l.opacity.is_animating()));
        if moving {
            window.request_animation_frame();
        }
        let subs: Vec<(Sub, f32, f32, ScrollHandle)> = self.layers.iter().map(|l| (l.sub, l.x.get(), l.opacity.get(), l.scroll.clone())).collect();
        let mut layers: Vec<AnyElement> = Vec::new();
        for (sub, x, o, scroll) in subs {
            let content = match sub {
                Sub::Menu => self.menu(&t, cx),
                Sub::FilteredApps => self.filtered_apps(&t, window, cx),
                Sub::LinkedPrograms => self.linked_programs(&t, cx),
                Sub::Advanced => self.advanced(&t, cx),
            };
            let scrollable = !matches!(sub, Sub::FilteredApps);
            let mut layer = div()
                .id(ElementId::Name(format!("settings-layer-{sub:?}").into()))
                .absolute()
                .top_0()
                .bottom_0()
                .w_full()
                .left(px(x))
                .opacity(o);
            if scrollable {
                layer = layer.overflow_y_scroll().track_scroll(&scroll);
            }
            layers.push(layer.child(content).into_any_element());
        }
        div().relative().size_full().overflow_hidden().children(layers)
    }
}
