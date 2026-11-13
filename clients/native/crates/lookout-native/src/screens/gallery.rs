//! Gallery — `Gallery.tsx` + `SessionCard.tsx` + `useGallery`.
//!
//! Sessions come from the persisted token list, painted instantly from the
//! summary cache and refreshed from `POST /api/sessions/batch` (100 tokens
//! per request) on open and whenever the window regains focus.

use gpui_kit::gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Point, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Window, canvas, div, img, px,
};

use crate::app::{self, api_base, store, store_mut};
use crate::images::{self, Remote};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::store::{CachedSummary, SessionSummary};
use crate::theme::{self, Theme, theme};
use crate::ui::button::{BtnSize, Button, PressState, Variant, paint_squircle_bg, press_values};
use crate::ui::icon::Icon;
use crate::ui::primitives::{AspectBox, BadgeSize, PressScale, Skeleton, badge, card, edge_faded};
use crate::ui::text::TextExt;

/// Newest first by `created_at`. The server sends `Date.toISOString()`
/// (fixed-width UTC), so string order is time order; the sort is stable so
/// ties keep the server's order.
fn sort_newest_first(sessions: &mut [SessionSummary]) {
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
}

pub enum GalleryEvent {
    Open(String, String),
    Settings,
    Add(Bounds<Pixels>),
    ContextMenu { token: String, status: String, position: Point<Pixels> },
    Archive(String),
}

pub struct GalleryPage {
    pub sessions: Vec<SessionSummary>,
    pub loading: bool,
    pub error: Option<String>,
    scroll: ScrollHandle,
    restored_scroll: bool,
    add_anchor: Bounds<Pixels>,
    pub banner: Option<AnyElementFactory>,
    generation: u64,
    /// A session card's measured height (they're all the same).
    card_h: Option<f32>,
}

/// Builds the announcement banner each render (owned by the main window).
pub type AnyElementFactory = std::rc::Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

impl EventEmitter<GalleryEvent> for GalleryPage {}

impl GalleryPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe(&images::cache(cx), |_, _, cx| cx.notify()).detach();
        let tokens = store(cx).active_tokens();
        let cache = &store(cx).gallery_cache;
        let mut initial: Vec<SessionSummary> =
            tokens.iter().filter_map(|t| cache.get(t).map(|c| c.summary.clone())).collect();
        sort_newest_first(&mut initial);
        let all_cached = !tokens.is_empty() && initial.len() == tokens.len();
        let mut page = Self {
            sessions: initial,
            loading: !all_cached && !tokens.is_empty(),
            error: None,
            scroll: ScrollHandle::new(),
            restored_scroll: false,
            add_anchor: Bounds::default(),
            banner: None,
            generation: 0,
            card_h: None,
        };
        page.refresh(cx);
        page
    }

    /// Re-fetch every token's summary (`useGallery().refresh()`).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let tokens: Vec<String> = store(cx)
            .active_tokens()
            .into_iter()
            .filter(|t| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()))
            .collect();
        if tokens.is_empty() {
            self.sessions.clear();
            self.loading = false;
            self.error = None;
            cx.notify();
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let core = app::state(cx).core.clone();
        let base = api_base(cx);
        let fut = app::tokio(cx, async move {
            let mut out: Vec<SessionSummary> = Vec::new();
            for chunk in tokens.chunks(100) {
                let v = core.sessions_batch(&base, chunk).await.map_err(|e| e.message)?;
                let list: Vec<SessionSummary> = v
                    .get("sessions")
                    .and_then(|s| serde_json::from_value(s.clone()).ok())
                    .unwrap_or_default();
                out.extend(list);
            }
            // The server sorts within one request only; the token list can be
            // oldest-first (imported from the Tauri app), so order the merge.
            sort_newest_first(&mut out);
            Ok::<_, String>(out)
        });
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(list) => {
                        let now = chrono::Utc::now().timestamp_millis();
                        let st = store_mut(cx);
                        for s in &list {
                            st.gallery_cache
                                .insert(s.token.clone(), CachedSummary { summary: s.clone(), fetched_at: now });
                        }
                        if st.gallery_cache.len() > 500 {
                            let mut v: Vec<_> = st.gallery_cache.iter().map(|(k, c)| (k.clone(), c.fetched_at)).collect();
                            v.sort_by_key(|(_, t)| -t);
                            let keep: std::collections::HashSet<String> = v.into_iter().take(500).map(|(k, _)| k).collect();
                            st.gallery_cache.retain(|k, _| keep.contains(k));
                        }
                        st.save();
                        this.sessions = list;
                        this.error = None;
                    }
                    Err(e) => {
                        log::warn!("Gallery fetch error: {e}");
                        this.error = Some(e);
                    }
                }
                this.loading = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn header(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let entity2 = cx.entity();
        let anchor_probe = cx.entity();
        div()
            .flex()
            .justify_between()
            .items_center()
            .p(px(16.0))
            .pb(px(0.0))
            .flex_none()
            .child(div().ts(20.0).w700().text_color(t.text_primary).child("Your Timelapses"))
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        Button::empty("gallery-settings")
                            .variant(Variant::Ghost)
                            .size(BtnSize::Sm)
                            .fixed(40.0, 40.0)
                            .padding(0.0, 0.0)
                            .font_size(18.0)
                            .tooltip("Settings")
                            .child(Icon::GearSixFill.el(GALLERY_ICON, t.text_secondary))
                            .on_click(move |_, _, cx| entity.update(cx, |_, cx| cx.emit(GalleryEvent::Settings))),
                    )
                    .child(
                        div()
                            .relative()
                            .child(
                                Button::empty("gallery-add")
                                    .variant(Variant::Ghost)
                                    .size(BtnSize::Sm)
                                    .fixed(40.0, 40.0)
                                    .padding(0.0, 0.0)
                                    .font_size(18.0)
                                    .tooltip("Start")
                                    .child(Icon::PlusBold.el(GALLERY_ICON, t.text_secondary))
                                    .on_click(move |_, _, cx| {
                                        entity2.update(cx, |this, cx| cx.emit(GalleryEvent::Add(this.add_anchor)))
                                    }),
                            )
                            .child(
                                canvas(
                                    move |bounds, _, cx| anchor_probe.update(cx, |this, _| this.add_anchor = bounds),
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .inset_0(),
                            ),
                    ),
            )
    }

    /// Where the "+" button is, for the add menu (Linux's header-bar copy
    /// reports its own anchor).
    pub fn add_anchor(&self) -> Bounds<Pixels> {
        self.add_anchor
    }
}

impl Render for GalleryPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let show_header = !crate::theme::header_bar_shown();
        // The Tauri app faded the bottom on macOS and Windows; on Linux the
        // list ends against the window's own rounded edge (App.tsx). That
        // used to coincide with showing the in-page header, which every
        // platform now replaces with the header bar.
        let show_bottom_fade = !crate::theme::is_linux();

        if !self.restored_scroll {
            self.restored_scroll = true;
            let y = store(cx).gallery_scroll;
            if y > 0.0 {
                self.scroll.set_offset(gpui_kit::gpui::point(px(0.0), px(-y)));
            }
        }

        let banner = cx
            .try_global::<crate::windows::main_window::CurrentAnnouncement>()
            .and_then(|a| a.0.clone())
            .map(|a| div().p(px(16.0)).pb(px(0.0)).flex_none().child(crate::services::announcement_banner(&a, &t)));

        // 1. Loading with nothing cached → skeleton.
        if self.loading && self.sessions.is_empty() {
            return div().size_full().child(gallery_skeleton()).into_any_element();
        }

        let mut col = div().size_full().flex().flex_col();
        if show_header {
            col = col.child(self.header(&t, cx));
        }
        col = col.children(banner);

        // 2. Error with nothing to show.
        if let (Some(err), true) = (&self.error, self.sessions.is_empty()) {
            return col
                .child(
                    div().flex_1().flex().flex_col().items_center().justify_center().p(px(24.0)).child(
                        div()
                            .font_family(crate::ui::text::MONO)
                            .ts(11.0)
                            .text_color(t.text_error)
                            .max_h(px(150.0))
                            .child(err.clone()),
                    ),
                )
                .child(div().flex_none().pb(px(16.0)).child(crate::screens::info::hack_club_footer("gallery-hackclub", &t)))
                .into_any_element();
        }

        // 3. Empty.
        if self.sessions.is_empty() {
            return col
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .p(px(24.0))
                        .child(div().mb(px(12.0)).opacity(0.2).child(Icon::LkMonitor.el(48.0, t.text_primary)))
                        .child(div().ts(14.0).text_color(t.text_primary).opacity(0.5).child("No timelapses yet"))
                        .child(
                            div()
                                .ts(12.0)
                                .text_color(t.text_primary)
                                .opacity(0.3)
                                .mt(px(4.0))
                                .child("Start a recording session to see it here."),
                        ),
                )
                .child(div().flex_none().pb(px(16.0)).child(crate::screens::info::hack_club_footer("gallery-hackclub", &t)))
                .into_any_element();
        }

        // 4. List.
        let offset = -f32::from(self.scroll.offset().y);
        let max = f32::from(self.scroll.max_offset().y);
        let show_top = offset > 0.5;
        let show_bottom = show_bottom_fade && offset + 0.5 < max;
        store_mut(cx).gallery_scroll = offset;
        let entity = cx.entity();

        // Only the cards on screen (and a few either side) are built: every
        // card is the same height, so the rest are stand-in spacers. The
        // first built card is measured each frame to keep that height true.
        let n = self.sessions.len();
        let viewport = self.scroll.bounds().size;
        let (view_w, view_h) = if viewport.height > px(0.0) {
            (f32::from(viewport.width), f32::from(viewport.height))
        } else {
            let v = window.viewport_size();
            (f32::from(v.width), f32::from(v.height))
        };
        let card_h = self.card_h.unwrap_or_else(|| LAST_CARD_H.get().unwrap_or_else(|| estimate_card_h(view_w - 2.0 * LIST_PAD)));
        let stride = card_h + CARD_GAP;
        let range = visible_range(n, offset, view_h, stride, LIST_PAD, OVERSCAN);
        self.release_far_thumbnails(&range, cx);
        let mut cards: Vec<AnyElement> = Vec::with_capacity(range.len() + 2);
        if range.start > 0 {
            cards.push(div().flex_none().h(px(range.start as f32 * stride - CARD_GAP)).into_any_element());
        }
        for i in range.clone() {
            let card = session_card(&self.sessions[i], &t, entity.clone(), window, cx).into_any_element();
            if i == range.start {
                let probe = entity.clone();
                cards.push(
                    div()
                        .relative()
                        .child(card)
                        .child(
                            canvas(
                                move |bounds, _, cx| {
                                    let h = f32::from(bounds.size.height);
                                    probe.update(cx, |this, cx| {
                                        if h > 0.0 && this.card_h.is_none_or(|old| (old - h).abs() > 0.5) {
                                            this.card_h = Some(h);
                                            LAST_CARD_H.set(Some(h));
                                            cx.notify();
                                        }
                                    })
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .inset_0(),
                        )
                        .into_any_element(),
                );
            } else {
                cards.push(card);
            }
        }
        if range.end < n {
            cards.push(div().flex_none().h(px((n - range.end) as f32 * stride - CARD_GAP)).into_any_element());
        }
        col.child(
            div()
                .flex_1()
                .relative()
                .min_h(px(0.0))
                // CSS mask-image-style edge fades (fadeMask, 20px).
                .child(edge_faded(
                    div()
                        .id("gallery-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                        .p(px(LIST_PAD))
                        // Cards, then a spacer that soaks up any leftover
                        // height, then the footer: pinned to the bottom when
                        // the list is short, after the last card when it scrolls.
                        .flex()
                        .flex_col()
                        .child(div().flex_none().flex().flex_col().gap(px(CARD_GAP)).children(cards))
                        .child(div().flex_1().min_h(px(40.0)))
                        .child(crate::screens::info::hack_club_footer("gallery-hackclub", &t)),
                    if show_top { 20.0 } else { 0.0 },
                    if show_bottom { 20.0 } else { 0.0 },
                )),
        )
        .into_any_element()
    }
}

/// The scroll list's padding and the space between cards.
const LIST_PAD: f32 = 16.0;
const CARD_GAP: f32 = 12.0;
/// Cards built beyond each edge of the view, so a fast scroll doesn't
/// reveal a gap before the next frame.
const OVERSCAN: usize = 3;
/// Decoded thumbnails kept for cards this many places beyond the built
/// ones; farther ones release their textures and reload from the disk
/// cache when scrolled back to.
const KEEP_DECODED: usize = 12;

thread_local! {
    /// The last measured card height, so a re-opened gallery starts exact.
    static LAST_CARD_H: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// A card's height before one has been measured: the 16:9 thumbnail plus
/// the name and meta lines, inside a 1px border.
fn estimate_card_h(card_w: f32) -> f32 {
    (card_w - 2.0) * 9.0 / 16.0 + 64.0
}

/// The cards to build for a list of `n` cards `stride` apart (height plus
/// gap) that starts `top` into the scroll content, scrolled to `offset`
/// with a `view_h`-tall view: the ones on screen, plus `overscan` a side.
fn visible_range(n: usize, offset: f32, view_h: f32, stride: f32, top: f32, overscan: usize) -> std::ops::Range<usize> {
    if n == 0 || stride <= 0.0 {
        return 0..n;
    }
    let first = ((offset - top) / stride).floor().max(0.0) as usize;
    let last = ((offset + view_h - top) / stride).ceil().max(0.0) as usize;
    let start = first.saturating_sub(overscan).min(n);
    let end = last.saturating_add(overscan).min(n).max(start);
    start..end
}

impl GalleryPage {
    /// Drop decoded thumbnails of cards far outside `built`, so a long
    /// gallery holds a screenful or so of textures, not one per session.
    fn release_far_thumbnails(&self, built: &std::ops::Range<usize>, cx: &mut Context<Self>) {
        let keep = built.start.saturating_sub(KEEP_DECODED)..built.end.saturating_add(KEEP_DECODED);
        let cache = images::cache(cx);
        let far: Vec<String> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(i, _)| !keep.contains(i))
            .filter_map(|(_, s)| s.thumbnail_url.as_deref())
            .filter(|url| cache.read(cx).is_ready(url))
            .map(str::to_string)
            .collect();
        if !far.is_empty() {
            cx.defer(move |cx| {
                cache.update(cx, |c, cx| {
                    for url in &far {
                        c.evict(url, cx);
                    }
                })
            });
        }
    }
}

/// What the Tauri gallery header's gear and + actually render at. The source
/// asks for `size={30}` / `size={28}` (`Gallery.tsx`), but the 40×40 button's
/// inner span is `width: 100%` with the `sm` padding (12px a side) under the
/// global `box-sizing: border-box`, leaving a 14px content box; the SVGs are
/// flex items, so they shrink to it, keeping their aspect ratio.
const GALLERY_ICON: f32 = 14.0;

fn gallery_skeleton() -> impl IntoElement {
    let card = || {
        div()
            .rounded(px(10.0))
            .overflow_hidden()
            .child(Skeleton::new().aspect(16.0 / 9.0).radius(0.0))
            .child(
                div()
                    .p(px(12.0))
                    .child(div().mb(px(4.0)).child(Skeleton::new().w_frac(0.6).h(16.0)))
                    .child(Skeleton::new().w_frac(0.4).h(12.0)),
            )
    };
    div()
        .p(px(16.0))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .mb(px(16.0))
                .child(Skeleton::new().w(180.0).h(24.0))
                .child(Skeleton::new().w(36.0).h(36.0).radius(6.0)),
        )
        .child(div().flex().flex_col().gap(px(12.0)).child(card()).child(card()).child(card()))
}

struct ArchiveHover {
    hovered: bool,
    t: Anim,
}

fn session_card(
    s: &SessionSummary,
    t: &Theme,
    gallery: Entity<GalleryPage>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let token = s.token.clone();
    let status = s.status.clone();
    let thumb: AnyElement = match s.thumbnail_url.as_deref() {
        Some(url) => match images::get(url, cx) {
            Remote::Ready(img_data, _) => img(img_data)
                .size_full()
                .object_fit(gpui_kit::gpui::ObjectFit::Cover)
                .rounded_t(px(9.0))
                .into_any_element(),
            _ => div().into_any_element(),
        },
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::LkMonitor.el(32.0, t.text_quaternary))
            .into_any_element(),
    };
    let date = crate::fmt::short_date(&s.created_at, false);
    let meta = format!("{} · {}", crate::fmt::format_tracked(s.tracked_seconds), date);

    // Archive button (top-left): hover swaps bg/border/icon colour over 0.15s CSS ease.
    let hover = window.use_keyed_state(theme::eid("archive-hover", &token), cx, |_, _| ArchiveHover {
        hovered: false,
        t: Anim::new(0.0),
    });
    if hover.update(cx, |h, _| h.t.tick()) {
        window.request_animation_frame();
    }
    let ht = hover.read(cx).t.get();
    let press = window.use_keyed_state(theme::eid("archive-press", &token), cx, |_, _| PressState::new());
    let (_, ascale) = press_values(&press, window, cx);
    let abg = theme::mix(t.archive_bg, t.archive_hover_bg, ht);
    let aborder = theme::mix(t.archive_border, t.archive_hover_border, ht);
    let aicon = theme::mix(t.archive_icon, t.text_error, ht);
    let (h1, p1, p2, p3) = (hover.clone(), press.clone(), press.clone(), press.clone());
    let g_archive = gallery.clone();
    let tok_archive = token.clone();
    let archive = div()
        .id(theme::eid("archive", &token))
        .absolute()
        .top(px(8.0))
        .left(px(8.0))
        .size(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .child(
            canvas(|_, _, _| (), move |bounds, _, window, _| {
                // Circle: squircle with radius = half size is a circle.
                paint_squircle_bg(bounds, 12.0, 0.0, ascale, abg, aborder, window);
            })
            .absolute()
            .inset_0(),
        )
        .child(Icon::LkArchiveX.el(14.0, aicon))
        .tooltip(|window, cx| crate::ui::tooltip::Tooltip::new("Archive").build(window, cx))
        .on_hover(move |hov, _, cx| {
            h1.update(cx, |h, cx| {
                h.hovered = *hov;
                h.t.set_css(if *hov { 1.0 } else { 0.0 }, 150.0, Ease::CSS_EASE);
                cx.notify();
            })
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
            p1.update(cx, |s, cx| {
                s.press.set(0.01, Curve::Spring(Spring::PRESS));
                cx.notify();
            })
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            p2.update(cx, |s, cx| {
                s.press.set(0.0, Curve::Spring(Spring::PRESS));
                cx.notify();
            })
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
            p3.update(cx, |s, cx| {
                s.press.set(0.0, Curve::Spring(Spring::PRESS));
                cx.notify();
            })
        })
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            let tok = tok_archive.clone();
            g_archive.update(cx, |_, cx| cx.emit(GalleryEvent::Archive(tok)));
        });

    let content = card(t)
        .relative()
        .child(
            div()
                .relative()
                .bg(t.bg_sunken)
                .rounded_t(px(9.0))
                .overflow_hidden()
                .child(AspectBox::new(16.0 / 9.0).child(div().absolute().inset_0().child(thumb)))
                .child(
                    // The web badge sat in an inline span whose 16px line box
                    // pushed the 10px pill ~4px below `top: 8`.
                    div().absolute().top(px(12.0)).right(px(8.0)).child(badge(&s.status, BadgeSize::Sm, true, t)),
                ),
        )
        .child(
            div()
                .p(px(12.0))
                .child(
                    div()
                        .ts(13.0)
                        .w600()
                        .text_color(t.text_primary)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .mb(px(2.0))
                        .child(if s.name.is_empty() { SharedString::from(" ") } else { s.name.clone().into() }),
                )
                .child(div().ts(11.0).text_color(t.text_tertiary).child(meta)),
        )
        .child(archive);

    let (g_open, g_menu) = (gallery.clone(), gallery);
    let (tok_open, st_open) = (token.clone(), status.clone());
    let (tok_menu, st_menu) = (token.clone(), status);
    PressScale::new(theme::eid("card", &token), 0.99, content)
        .on_click(move |_, _, cx| {
            let (t, s) = (tok_open.clone(), st_open.clone());
            g_open.update(cx, |_, cx| cx.emit(GalleryEvent::Open(t, s)));
        })
        .on_right_click(move |pos, _, cx| {
            let (t, s) = (tok_menu.clone(), st_menu.clone());
            g_menu.update(cx, |_, cx| cx.emit(GalleryEvent::ContextMenu { token: t, status: s, position: pos }));
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(token: &str, created_at: &str) -> SessionSummary {
        serde_json::from_value(serde_json::json!({ "token": token, "createdAt": created_at })).unwrap()
    }

    #[test]
    fn visible_range_builds_only_what_is_on_screen() {
        // 100 cards 312px apart under 16px of padding, in a 600px view.
        assert_eq!(visible_range(100, 0.0, 600.0, 312.0, 16.0, 0), 0..2);
        assert_eq!(visible_range(100, 0.0, 600.0, 312.0, 16.0, 3), 0..5);
        // Scrolled to card 50's top edge.
        let y = 16.0 + 50.0 * 312.0;
        assert_eq!(visible_range(100, y, 600.0, 312.0, 16.0, 0), 50..52);
        assert_eq!(visible_range(100, y, 600.0, 312.0, 16.0, 3), 47..55);
        // At the end, and past it (overscroll).
        assert_eq!(visible_range(100, 31_000.0, 600.0, 312.0, 16.0, 3), 96..100);
        assert_eq!(visible_range(100, 99_999.0, 600.0, 312.0, 16.0, 3), 100..100);
        assert_eq!(visible_range(0, 0.0, 600.0, 312.0, 16.0, 3), 0..0);
        // A short list builds everything.
        assert_eq!(visible_range(2, 0.0, 600.0, 312.0, 16.0, 3), 0..2);
    }

    #[test]
    fn merged_chunks_sort_newest_first() {
        // Two batch responses, each newest-first within itself, but the
        // second chunk holds the newer sessions (oldest-first token list).
        let mut list = vec![
            summary("b", "2025-03-01T00:00:00.000Z"),
            summary("a", "2025-01-01T00:00:00.000Z"),
            summary("d", "2026-11-09T12:00:00.000Z"),
            summary("c", "2026-02-01T00:00:00.000Z"),
        ];
        sort_newest_first(&mut list);
        let order: Vec<&str> = list.iter().map(|s| s.token.as_str()).collect();
        assert_eq!(order, ["d", "c", "b", "a"]);
    }
}
