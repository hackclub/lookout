//! Accessibility helpers (VoiceOver / Narrator / Orca via AccessKit).
//!
//! GPUI reports an element to assistive technology only when it has an
//! `.id(...)` *and* a `.role(...)`; plain string children are not exposed,
//! so the owning element carries an `.aria_label(...)`. See the
//! "Accessibility" section of `clients/native/README.md` for conventions.
//!
//! This module adds what GPUI's `aria_*` builders don't cover (disabled,
//! busy, modal, live regions) and the spoken strings shared by screens.

use gpui_kit::gpui::{App, Div, ElementId, Window, InteractiveElement, SharedString, Stateful, StatefulInteractiveElement, accesskit};

pub use gpui_kit::gpui::accesskit::Live;
pub use gpui_kit::gpui::{AccessibleAction, Role, Toggled};

/// `use crate::ui::a11y::prelude::*;` brings the role/label builders and
/// [`A11yExt`] into scope.
pub mod prelude {
    pub use super::{A11yExt, A11yTextExt, AccessibleAction, Live, NodeFlags, Role, Toggled};
    pub use gpui_kit::gpui::prelude::FluentBuilder;
    pub use gpui_kit::gpui::{InteractiveElement, StatefulInteractiveElement};
}

/// Node properties GPUI has no builder for. Applied through the element's
/// synthetic-children hook, which runs while the element's own node is
/// being built, so an element using this can't also add synthetic children
/// (combine them in one closure instead, as `TextInput` does).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeFlags {
    pub disabled: bool,
    pub busy: bool,
    pub modal: bool,
    pub live: Option<Live>,
    /// Out of the tree with everything under it (an offscreen page kept
    /// alive for a transition).
    pub hidden: bool,
}

impl NodeFlags {
    pub fn apply(self, node: &mut accesskit::Node) {
        if self.disabled {
            node.set_disabled();
        }
        if self.busy {
            node.set_busy();
        }
        if self.modal {
            node.set_modal();
        }
        if let Some(live) = self.live {
            node.set_live(live);
        }
        if self.hidden {
            node.set_hidden();
        }
    }
}

pub trait A11yExt: StatefulInteractiveElement + Sized {
    /// Set [`NodeFlags`] on this element's accessibility node.
    fn a11y_flags(self, flags: NodeFlags) -> Self {
        if flags == NodeFlags::default() {
            return self;
        }
        self.a11y_synthetic_children(move |b| flags.apply(b.parent_node()))
    }
    /// Report the element as disabled (dimmed and inert).
    fn aria_disabled(self, on: bool) -> Self {
        self.a11y_flags(NodeFlags { disabled: on, ..Default::default() })
    }
    /// A live region: assistive technology announces changes to the label
    /// without the user moving to it. Keep the label stable between real
    /// state changes (never a ticking clock), or it chatters.
    fn aria_live(self, live: Live) -> Self {
        self.a11y_flags(NodeFlags { live: Some(live), ..Default::default() })
    }
    /// Handle the screen reader's "press" directly. Without this GPUI fakes
    /// a mouse click at the node's centre, which lands on whatever is drawn
    /// there — wrong for a row scrolled half out of view.
    fn on_a11y_click(self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_a11y_action(AccessibleAction::Click, move |_, window, cx| f(window, cx))
    }
}

impl<T: StatefulInteractiveElement + Sized> A11yExt for T {}

/// Expose a plain text `div` (which GPUI otherwise leaves out of the tree).
/// `id` only has to be unique among siblings under the nearest element
/// with an id.
pub trait A11yTextExt {
    /// A static text node: role Label reading `text`.
    fn a11y_text(self, id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div>;
    /// A heading node (`level` 1 = page title).
    fn a11y_heading(self, id: impl Into<ElementId>, text: impl Into<SharedString>, level: usize) -> Stateful<Div>;
    /// A live status line: announced when it appears and whenever `text`
    /// changes. AT-SPI announces name changes and macOS value changes, so
    /// the text goes in both. Only use text that changes on real events.
    fn a11y_status(self, id: impl Into<ElementId>, text: impl Into<SharedString>, live: Live) -> Stateful<Div>;
}

impl A11yTextExt for Div {
    fn a11y_text(self, id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
        // AccessKit names a Label from its value, not its label.
        self.id(id).role(Role::Label).aria_value(text)
    }
    fn a11y_heading(self, id: impl Into<ElementId>, text: impl Into<SharedString>, level: usize) -> Stateful<Div> {
        self.id(id).role(Role::Heading).aria_level(level).aria_label(text)
    }
    fn a11y_status(self, id: impl Into<ElementId>, text: impl Into<SharedString>, live: Live) -> Stateful<Div> {
        let text = text.into();
        self.id(id).role(Role::Status).aria_label(text.clone()).aria_value(text).aria_live(live)
    }
}

/// An action only a context menu offers, made reachable for assistive
/// technology as a synthetic child button. AccessKit's adapters don't map
/// `ShowContextMenu` or custom actions, so this is the one way in.
#[derive(Clone)]
pub struct ExtraAction {
    pub label: SharedString,
    pub description: Option<SharedString>,
    pub run: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
}

/// Builds the synthetic buttons for `actions` under the current node and
/// records their ids in `ids`, for [`register_extra_actions`] to wire up at
/// paint time (the ids only exist once the node is built).
pub fn push_extra_actions(
    b: &mut gpui_kit::gpui::A11ySubtreeBuilder,
    actions: &[ExtraAction],
    ids: &std::cell::RefCell<Vec<accesskit::NodeId>>,
) {
    let bounds = b.parent_node().bounds();
    let mut ids = ids.borrow_mut();
    ids.clear();
    for (i, a) in actions.iter().enumerate() {
        let mut node = accesskit::Node::new(Role::Button);
        node.set_label(a.label.to_string());
        if let Some(d) = &a.description {
            node.set_description(d.to_string());
        }
        node.add_action(AccessibleAction::Click);
        if let Some(r) = bounds {
            node.set_bounds(r);
        }
        let id = b.synthetic_node_id(("extra-action", i));
        if b.push_child(id, node) {
            ids.push(id);
        }
    }
}

/// Route Click on the synthetic buttons from [`push_extra_actions`]. Call
/// during paint.
pub fn register_extra_actions(window: &mut Window, actions: &[ExtraAction], ids: &std::cell::RefCell<Vec<accesskit::NodeId>>) {
    for (id, a) in ids.borrow().iter().zip(actions) {
        let run = a.run.clone();
        window.on_a11y_action(*id, AccessibleAction::Click, move |_, window, cx| run(window, cx));
    }
}

/// A status as it should be spoken: the badge's own label ("Complete"),
/// falling back to the raw status capitalised.
pub fn status_words(status: &str) -> String {
    let (label, _) = crate::theme::status_config(status, &crate::theme::Theme::dark());
    if !label.is_empty() {
        return label.to_string();
    }
    let mut c = status.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 { format!("1 {unit}") } else { format!("{n} {unit}s") }
}

/// A tracked duration in words: "1 hour 12 minutes", "47 minutes",
/// "less than a minute". Screen readers read "1h 12min" letter by letter on
/// some voices, so spoken text spells units out.
pub fn spoken_duration(secs: f64) -> String {
    let total = secs.max(0.0) as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    match (h, m) {
        (0, 0) => "less than a minute".into(),
        (0, m) => plural(m, "minute"),
        (h, 0) => plural(h, "hour"),
        (h, m) => format!("{} {}", plural(h, "hour"), plural(m, "minute")),
    }
}

/// A clock position in words, down to the second: "1 minute 5 seconds",
/// "0 seconds", "1 hour 2 minutes 3 seconds".
pub fn spoken_clock(secs: f64) -> String {
    let total = secs.max(0.0).round() as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(plural(h, "hour"));
    }
    if m > 0 {
        parts.push(plural(m, "minute"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(plural(s, "second"));
    }
    parts.join(" ")
}

/// A gallery card's single spoken name: "Timelapse, Complete, 1 hour 12
/// minutes". The duration is left out when nothing was tracked yet.
pub fn session_card_label(name: &str, status: &str, tracked_secs: f64) -> String {
    let name = if name.trim().is_empty() { "Untitled session" } else { name.trim() };
    let mut s = format!("{name}, {}", status_words(status));
    if tracked_secs >= 1.0 {
        s.push_str(", ");
        s.push_str(&spoken_duration(tracked_secs));
    }
    s
}

/// The editor playhead's spoken value: "1 minute 5 seconds of 3 minutes".
pub fn playhead_value_text(pos_secs: f64, total_secs: f64) -> String {
    format!("{} of {}", spoken_clock(pos_secs), spoken_clock(total_secs))
}

/// Editor spans are in units, one per recorded minute (end exclusive):
/// "minute 3" or "minutes 3 to 5", counted from 1.
pub fn minute_span(start: f64, end: f64) -> String {
    let first = start.max(0.0).floor() as i64 + 1;
    let last = (end.ceil() as i64).max(first);
    if last == first { format!("minute {first}") } else { format!("minutes {first} to {last}") }
}

/// A cut region on the editor timeline: "Cut, minutes 3 to 5".
pub fn cut_label(start: f64, end: f64) -> String {
    format!("Cut, {}", minute_span(start, end))
}

/// A mask's bar on the editor timeline: "Mask 2, minute 4".
pub fn mask_label(number: usize, start: f64, end: f64) -> String {
    format!("Mask {number}, {}", minute_span(start, end))
}

/// The editor playhead's spoken value: "Minute 6 of 47", plus ", cut"
/// when that minute will be removed. `t` is in units (minutes).
pub fn editor_playhead_text(t: f64, n: usize, in_cut: bool) -> String {
    let unit = crate::editor_math::unit_at_time(t, n) + 1;
    let mut s = format!("Minute {unit} of {n}");
    if in_cut {
        s.push_str(", cut");
    }
    s
}

/// The editor toolbar's length summary: "47 minutes after cuts, 3 minutes
/// removed".
pub fn kept_summary(kept_units: usize, removed_units: usize) -> String {
    let mins = |n: usize| if n == 1 { "1 minute".to_string() } else { format!("{n} minutes") };
    let mut s = format!("{} after cuts", mins(kept_units));
    if removed_units > 0 {
        s.push_str(&format!(", {} removed", mins(removed_units)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_spelled_out() {
        assert_eq!(spoken_duration(0.0), "less than a minute");
        assert_eq!(spoken_duration(59.0), "less than a minute");
        assert_eq!(spoken_duration(60.0), "1 minute");
        assert_eq!(spoken_duration(47.0 * 60.0), "47 minutes");
        assert_eq!(spoken_duration(3600.0), "1 hour");
        assert_eq!(spoken_duration(3600.0 * 2.0 + 60.0), "2 hours 1 minute");
        assert_eq!(spoken_duration(3600.0 + 12.0 * 60.0 + 30.0), "1 hour 12 minutes");
        assert_eq!(spoken_duration(-5.0), "less than a minute");
    }

    #[test]
    fn clock_positions_include_seconds() {
        assert_eq!(spoken_clock(0.0), "0 seconds");
        assert_eq!(spoken_clock(1.0), "1 second");
        assert_eq!(spoken_clock(65.0), "1 minute 5 seconds");
        assert_eq!(spoken_clock(120.0), "2 minutes");
        assert_eq!(spoken_clock(3723.4), "1 hour 2 minutes 3 seconds");
    }

    #[test]
    fn card_label_reads_name_status_duration() {
        assert_eq!(session_card_label("Timelapse", "complete", 72.0 * 60.0), "Timelapse, Complete, 1 hour 12 minutes");
        assert_eq!(session_card_label("Fresh", "pending", 0.0), format!("Fresh, {}", status_words("pending")));
        assert_eq!(session_card_label("  ", "complete", 60.0), "Untitled session, Complete, 1 minute");
    }

    #[test]
    fn unknown_status_is_capitalised() {
        assert_eq!(status_words("zzz-unknown"), "Zzz-unknown");
        assert_eq!(status_words(""), "");
    }

    #[test]
    fn video_playhead_value() {
        assert_eq!(playhead_value_text(65.0, 180.0), "1 minute 5 seconds of 3 minutes");
    }

    #[test]
    fn editor_spans_count_minutes_from_one() {
        assert_eq!(minute_span(0.0, 1.0), "minute 1");
        assert_eq!(minute_span(2.0, 5.0), "minutes 3 to 5");
        assert_eq!(minute_span(3.0, 3.5), "minute 4");
        assert_eq!(cut_label(2.0, 5.0), "Cut, minutes 3 to 5");
        assert_eq!(mask_label(2, 3.0, 4.0), "Mask 2, minute 4");
    }

    #[test]
    fn editor_playhead_value() {
        assert_eq!(editor_playhead_text(0.0, 47, false), "Minute 1 of 47");
        assert_eq!(editor_playhead_text(5.4, 47, true), "Minute 6 of 47, cut");
        // Past the end clamps to the last minute.
        assert_eq!(editor_playhead_text(99.0, 47, false), "Minute 47 of 47");
    }

    #[test]
    fn kept_summary_reads_naturally() {
        assert_eq!(kept_summary(47, 0), "47 minutes after cuts");
        assert_eq!(kept_summary(1, 3), "1 minute after cuts, 3 minutes removed");
    }

    #[test]
    fn flags_write_the_node() {
        let mut n = accesskit::Node::new(Role::Button);
        NodeFlags { disabled: true, busy: true, modal: false, live: Some(Live::Polite), hidden: false }.apply(&mut n);
        assert!(n.is_disabled());
        assert!(n.is_busy());
        assert!(!n.is_modal());
        assert_eq!(n.live(), Some(Live::Polite));
    }
}
