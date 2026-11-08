//! `PermissionScreen.tsx` — the macOS screen-recording and camera gates.
//! Everywhere else permission is assumed (the gate never shows).

use std::rc::Rc;

use gpui_kit::gpui::{AnyElement, App, Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Window, div, px};

use crate::platform::permissions::{self, Kind};
use crate::screens::layout::page_layout;
use crate::theme::theme;
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::icon::Icon;
use crate::ui::primitives::page_icon;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

pub enum PermissionEvent {
    Granted { cache: bool },
}

pub struct PermissionScreen {
    pub kind: Kind,
    checking: bool,
    requested: bool,
}

impl EventEmitter<PermissionEvent> for PermissionScreen {}

impl PermissionScreen {
    pub fn new(kind: Kind, cx: &mut Context<Self>) -> Self {
        let granted = permissions::check(kind);
        if granted {
            let me = cx.entity();
            cx.defer(move |cx| me.update(cx, |_, cx| cx.emit(PermissionEvent::Granted { cache: true })));
        }
        Self { kind, checking: granted, requested: false }
    }
}

impl Render for PermissionScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let screen = self.kind == Kind::Screen;
        if self.checking {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(Spinner::new(SpinnerSize::Md))
                .child(div().ts(14.0).text_color(t.text_secondary).mt(px(12.0)).child(if screen {
                    "Checking screen recording permission..."
                } else {
                    "Checking camera permission..."
                }))
                .into_any_element();
        }
        let (title, subtitle, hint) = if screen {
            (
                "Screen Recording Permission",
                "Lookout needs screen recording access to capture screenshots of your work. Your screen is captured locally and only periodic screenshots are uploaded.",
                "After enabling \"Lookout\" in System Settings > Privacy > Screen Recording, quit and reopen the app. If it still doesn't work, remove Lookout from the list entirely, restart the app, and grant permission again.",
            )
        } else {
            (
                "Camera Permission",
                "Lookout uses your camera to capture periodic photos of your work for timelapses. Photos are taken locally and only periodic snapshots are uploaded.",
                "After enabling \"Lookout\" in System Settings > Privacy > Camera, quit and reopen the app. If it still doesn't work, remove Lookout from the list entirely, restart the app, and grant permission again.",
            )
        };
        let icon = if screen { Icon::LkMonitor } else { Icon::LkCamera };
        let kind = self.kind;
        let (me1, me2) = (cx.entity(), cx.entity());
        let actions: Vec<AnyElement> = vec![
            Button::new("perm-grant", if self.requested { "Opened System Settings" } else { "Grant Permission" })
                .size(BtnSize::Lg)
                .full_width()
                .disabled(self.requested)
                .on_click(move |_, _, cx| {
                    permissions::request(kind, cx);
                    me1.update(cx, |p, cx| {
                        p.requested = true;
                        cx.notify();
                    })
                })
                .into_any_element(),
            Button::new("perm-skip", "Skip")
                .variant(Variant::Secondary)
                .size(BtnSize::Lg)
                .full_width()
                .on_click(move |_, _, cx| me2.update(cx, |_, cx| cx.emit(PermissionEvent::Granted { cache: false })))
                .into_any_element(),
        ];
        page_layout(
            &t,
            None::<Rc<dyn Fn(&mut Window, &mut App)>>,
            Some(page_icon(icon.el(32.0, t.text_tertiary)).into_any_element()),
            Some(title.into()),
            Some(subtitle.into()),
            Some(hint.into()),
            vec![],
            actions,
        )
        .into_any_element()
    }
}
