//! `TextInput` — a single-line text field, adapted from GPUI's input
//! example. It behaves like the webview's `<input>`: IME composition,
//! word/grapheme navigation, selection by mouse and keyboard, clipboard,
//! a caret that blinks on the browser's 500ms rhythm, and horizontal
//! scrolling that keeps the caret visible. Visuals (padding, border,
//! radius, fill) belong to the call site so each field matches its spec.

use std::ops::Range;
use std::time::Duration;

use gpui_kit::gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, Task, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point,
    prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::theme;

actions!(
    text_input,
    [
        Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, WordLeft,
        WordRight, SelectWordLeft, SelectWordRight, DeleteWordLeft, Paste, Cut, Copy, Submit, Cancel,
    ]
);

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("TextInput");
    let primary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let word = if cfg!(target_os = "macos") { "alt" } else { "ctrl" };
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new(leak(format!("{primary}-a")), SelectAll, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new(leak(format!("{word}-left")), WordLeft, ctx),
        KeyBinding::new(leak(format!("{word}-right")), WordRight, ctx),
        KeyBinding::new(leak(format!("{word}-shift-left")), SelectWordLeft, ctx),
        KeyBinding::new(leak(format!("{word}-shift-right")), SelectWordRight, ctx),
        KeyBinding::new(leak(format!("{word}-backspace")), DeleteWordLeft, ctx),
        KeyBinding::new(leak(format!("{primary}-v")), Paste, ctx),
        KeyBinding::new(leak(format!("{primary}-x")), Cut, ctx),
        KeyBinding::new(leak(format!("{primary}-c")), Copy, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("escape", Cancel, ctx),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
        ]);
    }
}

#[derive(Clone, Debug)]
pub enum InputEvent {
    Changed,
    Submit,
    Cancel,
    Focus,
    Blur,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    pub content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    pub disabled: bool,
    pub max_len: usize,
    scroll_x: f32,
    blink_on: bool,
    blink_task: Option<Task<()>>,
    was_focused: bool,
    pub text_color: Option<Hsla>,
    pub placeholder_color: Option<Hsla>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            disabled: false,
            max_len: usize::MAX,
            scroll_x: 0.0,
            blink_on: true,
            blink_task: None,
            was_focused: false,
            text_color: None,
            placeholder_color: None,
        }
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        let len = self.content.len();
        self.selected_range = len..len;
        cx.notify();
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_on = true;
        self.blink_task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let alive = this
                .update(cx, |this, cx| {
                    this.blink_on = !this.blink_on;
                    cx.notify();
                })
                .is_ok();
            if !alive {
                break;
            }
        }));
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.restart_blink(cx);
        cx.emit(InputEvent::Changed);
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.selected_range.start, window, cx)
        }
    }
    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), window, cx);
        } else {
            self.move_to(self.selected_range.end, window, cx)
        }
    }
    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }
    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }
    fn word_left(&mut self, _: &WordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), window, cx);
    }
    fn word_right(&mut self, _: &WordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), window, cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(self.cursor_offset()), window, cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.cursor_offset()), window, cx);
    }
    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.content.len(), window, cx)
    }
    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }
    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }
    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), window, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Cancel);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus_handle, cx);
        window.prevent_default();
        self.is_selecting = true;
        if event.click_count >= 2 {
            self.select_all(&SelectAll, window, cx);
            return;
        }
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), window, cx)
        }
    }
    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.disabled {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, _window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.restart_blink(cx);
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + px(self.scroll_x))
    }

    fn select_to(&mut self, offset: usize, _window: &Window, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.restart_blink(cx);
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }
    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }
    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }
    fn previous_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0)
    }
    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
    fn previous_word(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .rev()
            .find_map(|(idx, w)| (idx < offset && !w.trim().is_empty()).then_some(idx))
            .unwrap_or(0)
    }
    fn next_word(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .find_map(|(idx, w)| {
                let end = idx + w.len();
                (end > offset && !w.trim().is_empty()).then_some(end)
            })
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn set_selected_text_range(&mut self, range_utf16: Range<usize>, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        cx.notify();
    }

    fn text_length_utf16(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let mut new_text = new_text.to_string();
        // `maxLength`: never grow past the limit.
        let remaining = self.max_len.saturating_sub(self.content.chars().count() - self.content[range.clone()].chars().count());
        if new_text.chars().count() > remaining {
            new_text = new_text.chars().take(remaining).collect();
        }
        self.content = (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..]).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.edited(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range = if !new_text.is_empty() { Some(range.start..range.start + new_text.len()) } else { None };
        let insertion_start_utf16 = self.offset_to_utf16(range.start);
        let inserted_length_utf16 = new_text.encode_utf16().count();
        self.selected_range = new_selected_range_utf16
            .map(|selected| {
                self.range_from_utf16(
                    &(insertion_start_utf16 + selected.start.min(inserted_length_utf16)
                        ..insertion_start_utf16 + selected.end.min(inserted_length_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        self.edited(cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let sx = px(self.scroll_x);
        Some(Bounds::from_corners(
            point(bounds.left() + last_layout.x_for_index(range.start) - sx, bounds.top()),
            point(bounds.left() + last_layout.x_for_index(range.end) - sx, bounds.bottom()),
        ))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&p)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.closest_index_for_x(line_point.x + px(self.scroll_x));
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: f32,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui_kit::gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui_kit::gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let t = theme(cx).clone();
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let focused = input.focus_handle.is_focused(window);
        let blink_on = input.blink_on;

        let (display_text, text_color) = if content.is_empty() {
            // Chromium's placeholder is the text colour at ~54% (darkgray-ish).
            (input.placeholder.clone(), input.placeholder_color.unwrap_or(crate::theme::fade(style.color, 0.45)))
        } else {
            (content.clone(), input.text_color.unwrap_or(style.color))
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = input.marked_range.as_ref().filter(|_| !content.is_empty()) {
            vec![
                TextRun { len: marked.start, ..run.clone() },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: display_text.len() - marked.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(display_text, font_size, &runs, None);

        // Keep the caret inside the visible width.
        let width = f32::from(bounds.size.width);
        let cursor_x = if content.is_empty() { 0.0 } else { f32::from(line.x_for_index(cursor)) };
        let mut scroll_x = input.scroll_x;
        if cursor_x - scroll_x > width - 2.0 {
            scroll_x = cursor_x - width + 2.0;
        }
        if cursor_x - scroll_x < 0.0 {
            scroll_x = cursor_x;
        }
        let line_w = f32::from(line.width);
        scroll_x = scroll_x.clamp(0.0, (line_w - width + 2.0).max(0.0));
        let sx = px(scroll_x);

        let (selection, cursor_quad) = if selected_range.is_empty() || content.is_empty() {
            let show = focused && blink_on && !input.disabled;
            (
                None,
                show.then(|| {
                    fill(
                        Bounds::new(point(bounds.left() + px(cursor_x) - sx, bounds.top()), size(px(1.0), bounds.size.height)),
                        t.text_primary,
                    )
                }),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(bounds.left() + line.x_for_index(selected_range.start) - sx, bounds.top()),
                        point(bounds.left() + line.x_for_index(selected_range.end) - sx, bounds.bottom()),
                    ),
                    crate::theme::fade(t.accent, if focused { 0.35 } else { 0.18 }),
                )),
                None,
            )
        };
        PrepaintState { line: Some(line), cursor: cursor_quad, selection, scroll_x }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui_kit::gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let scroll_x = prepaint.scroll_x;
        window.with_content_mask(Some(gpui_kit::gpui::ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection)
            }
            let line = prepaint.line.take().unwrap();
            let origin = point(bounds.origin.x - px(scroll_x), bounds.origin.y);
            line.paint(origin, window.line_height(), gpui_kit::gpui::TextAlign::Left, None, window, cx).ok();
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
            self.input.update(cx, |input, _cx| {
                input.last_layout = Some(line);
                input.last_bounds = Some(bounds);
                input.scroll_x = scroll_x;
            });
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        if focused != self.was_focused {
            self.was_focused = focused;
            if focused {
                self.restart_blink(cx);
                cx.emit(InputEvent::Focus);
            } else {
                self.blink_task = None;
                cx.emit(InputEvent::Blur);
            }
        }
        div()
            .id("text-input")
            .flex()
            .w_full()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(if self.disabled { CursorStyle::Arrow } else { CursorStyle::IBeam })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Whether a text field holds keyboard focus (the router ignores
/// Escape/Backspace then, like the web app's INPUT check).
pub fn any_input_focused(window: &Window, cx: &App) -> bool {
    window.focused(cx).map(|f| REGISTRY.with(|r| r.borrow().iter().any(|h| h.upgrade().map(|h| h == f).unwrap_or(false)))).unwrap_or(false)
}

thread_local! {
    static REGISTRY: std::cell::RefCell<Vec<gpui_kit::gpui::WeakFocusHandle>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Create an input entity and register its focus handle.
pub fn new_input(placeholder: &str, cx: &mut App) -> Entity<TextInput> {
    let placeholder = placeholder.to_string();
    let e = cx.new(|cx| TextInput::new(placeholder, cx));
    let weak = e.read(cx).focus_handle.downgrade();
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|h| h.upgrade().is_some());
        r.push(weak);
    });
    e
}

/// A text field's box: clicking anywhere in it (padding included) focuses
/// the field, like a browser `<input>`.
pub fn field_box(input: &Entity<TextInput>) -> gpui_kit::gpui::Stateful<gpui_kit::gpui::Div> {
    let input = input.clone();
    let id = ElementId::Name(format!("field-{}", input.entity_id()).into());
    div()
        .id(id)
        .flex()
        .items_center()
        .cursor(CursorStyle::IBeam)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if input.read(cx).disabled {
                return;
            }
            let fh = input.read(cx).focus_handle.clone();
            window.focus(&fh, cx);
            // Ancestors that track focus (the window root) focus themselves on
            // mouse-down unless the event was handled.
            window.prevent_default();
        })
}
