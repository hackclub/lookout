//! `TextMorph`: torph's text morph (github.com/lochie/torph, MIT) for GPUI,
//! driven by an explicit progress so a caller can place it on a timeline.
//!
//! Two modes, as torph picks them:
//!
//! - **Numbers** (the two strings have the same shape — the same length,
//!   digits in the same places, the same symbols between them): place by
//!   place, a changed digit rolls. The new digit drops in from a line above
//!   while fading in over the first quarter; the old one slides down a line
//!   while fading out over 45% ("a digit that has already left is a hole in
//!   the number"). Unchanged places and the symbols between them hold still.
//! - **Text**: the characters are diffed (longest common subsequence).
//!   Characters in both slide from where they were to where they are now;
//!   new ones fade in after a quarter of the morph, growing from 95%;
//!   departing ones fade out over the first quarter, shrinking to 95%.
//!
//! Timing is torph's: the caller runs `progress` 0→1 linearly over its
//! duration (torph uses 400ms); motion is eased `cubic-bezier(0.19, 1,
//! 0.22, 1)` here, fades stay linear. The box eases between the old and new
//! widths. GPUI can't transform text, so each character is its own element,
//! placed from the shaped line's glyph offsets.

use gpui_kit::gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, TextRun, Window, div, px};

use crate::motion::Ease;

/// torph's default ease.
const EASE: Ease = Ease::Bezier(0.19, 1.0, 0.22, 1.0);

#[derive(IntoElement)]
pub struct TextMorph {
    from: SharedString,
    to: SharedString,
    progress: f32,
}

impl TextMorph {
    /// Morph from `from` to `to`, `progress` (0..1, linear in time) of the way.
    pub fn new(from: impl Into<SharedString>, to: impl Into<SharedString>, progress: f32) -> Self {
        Self { from: from.into(), to: to.into(), progress: progress.clamp(0.0, 1.0) }
    }
}

/// Same shape for the number mode: equal length, at least one digit, and
/// every non-digit equal in place.
fn number_shaped(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    a.len() == b.len()
        && a.iter().any(|c| c.is_ascii_digit())
        && a.iter().zip(&b).all(|(x, y)| x.is_ascii_digit() == y.is_ascii_digit() && (x.is_ascii_digit() || x == y))
}

/// Pairs `(i, j)` of characters common to `a` and `b`, in order (LCS).
fn lcs(a: &[char], b: &[char]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

impl RenderOnce for TextMorph {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let fs = style.font_size.to_pixels(window.rem_size());
        let line_h = f32::from(window.line_height());
        let shape = |s: &str| {
            let run = TextRun { len: s.len(), font: style.font(), color: style.color, background_color: None, underline: None, strikethrough: None };
            window.text_system().shape_line(SharedString::from(s.to_string()), fs, &[run], None)
        };
        let (from_line, to_line) = (shape(&self.from), shape(&self.to));
        let xs = |line: &gpui_kit::gpui::ShapedLine, s: &str| -> Vec<(f32, String)> {
            s.char_indices().map(|(i, c)| (f32::from(line.x_for_index(i)), c.to_string())).collect()
        };
        let (from_chars, to_chars) = (xs(&from_line, &self.from), xs(&to_line, &self.to));
        let (wa, wb) = (f32::from(from_line.width), f32::from(to_line.width));

        let p = self.progress;
        let e = EASE.apply(p);
        let reduce = crate::motion::reduce_motion();
        let width = if reduce { wb } else { wa + (wb - wa) * e };
        let glyph = |x: f32, y: f32, o: f32, s: String| {
            div().absolute().left(px(x)).top(px(y)).opacity(o.clamp(0.0, 1.0)).whitespace_nowrap().child(s)
        };
        let mut root = div().relative().w(px(width)).h(px(line_h)).overflow_hidden();

        if p >= 1.0 || self.from == self.to {
            return root.child(glyph(0.0, 0.0, 1.0, self.to.to_string()));
        }
        if p <= 0.0 {
            return root.child(glyph(0.0, 0.0, 1.0, self.from.to_string()));
        }

        if number_shaped(&self.from, &self.to) {
            // Place by place on the new layout; changed digits roll.
            for (k, ((xb, cb), (_, ca))) in to_chars.iter().zip(&from_chars).enumerate() {
                let _ = k;
                if cb == ca {
                    root = root.child(glyph(*xb, 0.0, 1.0, cb.clone()));
                    continue;
                }
                let slide = if reduce { 0.0 } else { line_h };
                // The new digit drops in from a line above...
                root = root.child(glyph(*xb, -slide * (1.0 - e), p / 0.25, cb.clone()));
                // ...as the old one slides down a line and goes.
                root = root.child(glyph(*xb, slide * e, 1.0 - p / 0.45, ca.clone()));
            }
            return root;
        }

        // Text: persisting characters slide; the rest fade out or in.
        let (a, b): (Vec<char>, Vec<char>) = (self.from.chars().collect(), self.to.chars().collect());
        let pairs = lcs(&a, &b);
        let kept_from: Vec<bool> = (0..a.len()).map(|i| pairs.iter().any(|(pi, _)| *pi == i)).collect();
        let kept_to: Vec<bool> = (0..b.len()).map(|j| pairs.iter().any(|(_, pj)| *pj == j)).collect();
        for (i, j) in &pairs {
            let (xa, xb) = (from_chars[*i].0, to_chars[*j].0);
            root = root.child(glyph(xa + (xb - xa) * e, 0.0, 1.0, to_chars[*j].1.clone()));
        }
        for (i, (x, c)) in from_chars.iter().enumerate() {
            if !kept_from[i] {
                root = root.child(glyph(*x, 0.0, 1.0 - p / 0.25, c.clone()));
            }
        }
        for (j, (x, c)) in to_chars.iter().enumerate() {
            if !kept_to[j] {
                root = root.child(glyph(*x, 0.0, (p - 0.25) / 0.5, c.clone()));
            }
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_shape() {
        assert!(number_shaped("3h 05m", "4h 17m"));
        assert!(!number_shaped("3h 05m", "13h 05m"));
        assert!(!number_shaped("Compiling", "Complete"));
    }

    #[test]
    fn lcs_keeps_the_shared_letters() {
        let a: Vec<char> = "Compiling".chars().collect();
        let b: Vec<char> = "Complete".chars().collect();
        let kept: String = lcs(&a, &b).iter().map(|(i, _)| a[*i]).collect();
        assert_eq!(kept, "Compl");
    }
}
