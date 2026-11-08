# Animation audit: Tauri app vs Lookout Native

Built from the original source (`clients/desktop/src`, `clients/react/src`,
`node_modules/{motion-dom,framer-motion,vaul,number-flow}`), not from
`ui-spec.md`. Each row lists the original's trigger, properties and timing,
then the native counterpart and its status.

Status key: **match**, **fixed** (changed in this audit), **deviation**
(deliberate, with the reason), **GPUI** (GPUI can't do it, so the row gives
the closest approximation used).

## Library rules this audit relies on (verified in node_modules)

- When a motion transition is defined without `type` or `ease`, motion uses
  the base `ease: "easeOut"` = `cubic-bezier(0,0,0.58,1)`
  (`motion-dom/.../interfaces/motion-value.mjs:27`). It does **not** use
  easeInOut. `ui-spec.md` §4.C says easeInOut here, which is wrong; §4.D is
  right. With no duration given, the tween runs 300 ms.
- When a per-value transition object lacks a key, motion uses the whole
  object for that key (`get-value-transition.mjs`). Overlay's `scale`
  therefore gets a 300 ms easeOut tween, not the 550/30 default spring.
- If no transition is defined, the per-value defaults apply: opacity and
  colours get 0.3 s `cubic-bezier(.25,.1,.35,1)`, x/y get spring 500/25
  (restSpeed 10), and scale gets spring 550/30.
- AnimatePresence: a key that is re-added while it is still exiting keeps
  its PresenceChild. It animates back to `animate` **from its current
  values** and does not restart from `initial` (`AnimatePresence/index.mjs`).
- A CSS `transition` restarts from the current value with no velocity. When
  it reverses a running transition back to that transition's start value,
  its duration is multiplied by the *reversing shortening factor* (CSS
  Transitions §3).
- NumberFlow composes digit spins with `composite: accumulate`. New digits
  start at 0, spin to their value and fade in. Removed digits are popped out
  of the layout, spin to 0, fade out, and are dropped on `animationsfinish`.
  The mask is opaque over the line box and fades over `2·round(0.125em)` px
  above and below it.

## Inventory

| # | Component · trigger | Original (props · curve · duration/delay) | Native | Status |
|---|---|---|---|---|
| 1 | Button bg layer · press | scale 1→0.96, spring 1500/60 (`Button.tsx:87-92`) | `ui/button.rs` PressState | match (left untouched per owner) |
| 2 | Button bg layer · hover | bg/border via variants on spring 1500/60, with a CSS `background .15s` transition on the same node | spring-driven colour mix | match. Not changed, per the owner's rule to keep the button press as it is. The CSS layer adds a lag of ≤150 ms on top of the spring. |
| 3 | Button disabled | opacity 0.6, instant | `when_some_opacity` | match |
| 4 | Card (clickable) · press | scale 0.99, spring 1500/60 | gallery card press 0.01 | match |
| 5 | ErrorDisplay action/Copy/× · press | outline layer scale 0.96, spring 1500/60 | `primitives.rs` PressScale | match |
| 6 | SessionCard archive · press | bg layer scale 0.99, spring (plus CSS `all .15s`) | `gallery.rs` press 0.01 spring | match. The CSS lag is on a 0.24 px movement and can't be seen. |
| 7 | SessionCard archive · hover | bg/border/icon colour, CSS `.15s ease` | `gallery.rs` | **fixed**: now uses CSS reversal semantics (`set_css`) |
| 8 | Spinner | rotate, 1 s linear, infinite | `spin_angle(1000)` | match |
| 9 | Skeleton shimmer | bg-position 200%→−200%, 1.5 s ease-in-out, infinite | `shimmer_phase` | deviation (README): Tauri only defined the keyframes after the recorder had mounted |
| 10 | Pulse dot (recorder, tray) | opacity 1→.4→1, 1.5 s, ease-in-out per segment | `pulse_opacity(1500)` | match |
| 11 | Route transition · enter | x ±14→0 spring 460/36/0.7; opacity 0→1 160 ms easeOut, delay 40 ms (`App.tsx:1506-1541`) | `main_window.rs` slots | match |
| 12 | Route transition · exit | x 0→∓14 spring; opacity 140 ms easeOut (mode sync) | same | match |
| 13 | Route · re-entering an exiting page | animates back from current x/opacity | used to restart from ±14/0 | **fixed** |
| 14 | Route direction rule | gallery↔other ±1, record↔session ±1 | `direction()` | match |
| 15 | RecordPage picker⇄recorder slide | same variants as #11/#12 | `record.rs` | match |
| 16 | RecordPage · picker re-entered mid-exit | animates from current values | restarted from enter | **fixed** |
| 17 | Settings subpage slide | same variants, `initial={false}` | `settings.rs` layers | match |
| 18 | Settings · subpage re-entered mid-exit | animates from current values | restarted from enter | **fixed** |
| 19 | "Starting {program}…" pill · enter | opacity 0.3 s `[.25,.1,.35,1]`; y 8→0 spring 500/25 | `set_starting` | match |
| 20 | Starting pill · exit | opacity + y over 120 ms **easeOut** (duration-only) | used easeInOut | **fixed** |
| 21 | Starting pill · re-shown mid-exit | re-enters from current values | jumped y to 8 | **fixed** |
| 22 | Overlay panel · enter | y 10→0 spring 420/34/0.7; opacity 160 ms easeOut | `overlay.rs` | match |
| 23 | Overlay panel · enter scale | 0.99→1, **300 ms easeOut tween** | missing | **fixed** / GPUI: the panel fill, border and shadow are painted at the scale; the content stays at 1 (≤2 px at the edges while still fading in) |
| 24 | NamingModal · enter | y 12→0 spring 420/34/0.7; opacity 160 ms easeOut; no scale | `Entrance::Naming` | match |
| 25 | Overlay/NamingModal · exit | none (instant unmount); backdrop instant | 120 ms fade | deviation (README "deliberate improvements") |
| 26 | Header bar close control | bg 100 ms ease-out, :hover/:active | `header_bar.rs` | **fixed**: CSS reversal semantics |
| 27 | Header bar actions (back, gear, +) | bg **and colour** 100 ms ease-out | icons were hard-coded `text_secondary` | **fixed**: icons now take the animated foreground colour (`HeaderBtn::icon`), with CSS reversal semantics |
| 28 | Header bar backdrop dim (Linux) | opacity 1↔.55, 160 ms ease-out | `active` anim | **fixed**: CSS reversal semantics |
| 29 | CSD window shadow · focus | box-shadow focused↔backdrop, 160 ms ease-out | `linux_frame` lerp on `active` | **fixed** (same anim as #28) |
| 30 | CSD snapped | inset/radius instant, shadow →none 160 ms | shadow dropped instantly | match. The shadow lies outside the visible area once the frame collapses. |
| 31 | Linux blur plate | bg-color 200 ms ease-out | Native doesn't implement `LOOKOUT_WINDOW_BLUR` | n/a (feature not ported) |
| 32 | AddMenuPopup (Win/Linux) · enter | scale 0.92→1 spring 1800/56; opacity 100 ms easeOut | `menu.rs` | match |
| 33 | AddMenuPopup · exit | opacity only, 100 ms easeOut | `dismiss` | match |
| 34 | AddMenuPopup rows | bg instant | — | match |
| 35 | macOS add menu (Swift) | alpha 0.15 s + scale 0.85→1 spring 1200/46; dismiss alpha 0.17 s | `Look::Mac` | match |
| 36 | SourcePicker preview tile · enter | opacity 220 ms easeOut; scale 1.08/1.02→1 spring 360/32/0.7 | `source_picker.rs` | match |
| 37 | Preview tile · exit (popLayout) | opacity →0, scale →0.98. The tile is popped at its last box and the others spring to their new slots. | shrank the leaving tile's flex weight and grew the new one from 0 | **fixed**: each tile has an animated box (`LinRect`, spring 420/34/0.75). Leaving tiles keep their box, staying tiles spring to their slot, new tiles appear in place. |
| 38 | Preview tile · layout reflow | `layout` spring 420/34/0.75 | `LinRect` | **fixed** / GPUI: motion stretches the img with a scale transform. Native interpolates the box and re-fits the image each frame, so it is not distorted. |
| 39 | Hover card → tile handoff | shared `layoutId`: on select, the hover card's image flies into the tile | missing | **fixed** (box morph from the card's image rect, layout spring) |
| 40 | Window hover card · enter | opacity 0→1, scale .92→1, spring 420/32/0.7 | single shared anim | match |
| 41 | Hover card · exit | opacity only; scale is kept | used to shrink back to 0.92 | **fixed** |
| 42 | Hover card · hover another window | old card exits while the new one enters (keyed `hover-<id>`), so they crossfade | content swapped instantly | **fixed** (a list of cards) |
| 43 | Empty-preview text | opacity default tween 0.3 s `[.25,.1,.35,1]` | `empty_text` | match |
| 44 | Tab indicator | layoutId spring 350/35 | `indicator_x/w` | match |
| 45 | Tab label weight/colour | instant | — | match |
| 46 | Source row · press | bg scale 0.98 spring 1500/60 | press 0.02 | match |
| 47 | Shift hint · enter/exit | opacity + y 5→0 / →10, spring 900/30 | `hint`, `hint_y` | match |
| 48 | Shift hint · re-shown mid-exit | from current values | jumped to 5 | **fixed** |
| 49 | Settings rows (menu + apps) | bg + scale(.96) 120 ms ease-out, :hover/:active | `pressable_row` | **fixed**: CSS reversal semantics |
| 50 | Filtered-app checkbox | `all .15s ease` | `app_row` | **fixed**: CSS reversal semantics |
| 51 | App icon fade-in | 200 ms ease-out, only when the icon arrives after mount | `icon_fade` | match |
| 52 | Apps skeleton rows | opacity [.45,.9,.45], 1.2 s easeInOut per segment, delay i×50 ms, repeat ∞ | phase came from the process clock, so rows started at a random phase | **fixed**: the clock starts at mount and holds .45 during each row's delay |
| 53 | Sheet (vaul) · open/close | translateY 100%↔0 and overlay fade, 500 ms `cubic-bezier(.32,.72,0,1)` | `sheet.rs` | match |
| 54 | Sheet · drag/over-drag/release | 1:1 drag; upward damped `8(ln(v+1)−2)`; overlay 1−p; close if >0.4 px/ms or ≥25% | same | match |
| 55 | Sheet · press within 500 ms of opening | vaul `shouldDrag` refuses to drag | dragged | **fixed** |
| 56 | ProgramPanel drawer `::after` | bg extends 200% below, so an upward over-drag shows no gap | gap | **fixed** |
| 57 | vaul · close interrupting open (or vice versa) | CSS animation restarts from the underlying value: a visible jump | smooth from current progress | deviation: the jump is a CSS-animation artefact and only shows on sub-500 ms toggles |
| 58 | ProgramPanel body height | spring 420/34/0.7, `initial={false}` | `program_panel.rs` | match |
| 59 | ProgramPanel loading cover | opacity 1→0, 180 ms easeOut | `cover` | match |
| 60 | TipDrawer hero image | opacity 220 ms ease-out on load | `services.rs` fade | match |
| 61 | UpdatePill · enter/exit/colours/ring | spring 450/32; exit 150 ms easeOut; colours .15 s ease; ring 200 ms ease | `ui/update_pill.rs` | match (audited read-only: the file belongs to the updater agent). The ring picks up CSS semantics through ProgressRing (#64). |
| 62 | UpdateDrawer / CaptureErrorDialog "Copied" swaps | timed label swaps, no animation | — | match |
| 63 | EditorOpenPlaceholder | opacity 1↔.6, `.15s ease` | `EditorPlaceholder` | **fixed**: CSS reversal semantics |
| 64 | ProgressRing arc | dashoffset `.25s linear` | `ProgressRing` | **fixed**: CSS reversal semantics |
| 65 | NumberFlow · digit roll | per-digit spin of signed delta (trend-wrapped), 900 ms `linear(…)`, composite accumulate | retargeted from the current position | **fixed**: accumulates like NumberFlow |
| 66 | NumberFlow · edge mask | rigid strip, numerals 1em+2s apart, opaque over the line with 2s px fades | linear crossfade by distance | **fixed** / GPUI: no gradient masks, so the fade zones are drawn as 1 px clipped bands at stepped opacity |
| 67 | NumberFlow · new place | starts at 0, spins to its value, fades in 450 ms ease-out | faded in at its value | **fixed** |
| 68 | NumberFlow · removed place | popped in place, spins to 0, fades out 450 ms, dropped at finish | vanished | **fixed** |
| 69 | NumberFlow · width change | the section slides (translateX) and the mask box resizes over 900 ms | digit box width animates over 900 ms on NumberFlow's curve; digits are right-anchored so they slide, the suffix follows, popped places fade where they stood | **fixed** (after the audit) |
| 70 | NumberFlow · reduced motion | no animation | jump | match |
| 71 | Tray timer component reuse | React reuses NumberFlows by position (at 59:59→1:00:00 the first one rolls 59→1) | keyed by unit | **fixed**: positional keys |
| 72 | MinutesFlow component reuse | same positional reuse at 59 m→1 h | keyed by unit | **fixed** |
| 73 | macOS menu-bar clock | SwiftUI numericText, spring 0.4/0.9 | native tray | match (Swift path unchanged) |
| 74 | SessionDetail rename · paddings/date y | spring 500/30 | `session.rs` | match |
| 75 | SessionDetail rename · input bg/border | **150 ms easeOut** (duration-only) | was easeInOut | **fixed** |
| 76 | SessionDetail rename · input width | `"100%"`↔`"max(100%,300px)"`: motion can't interpolate it and switches immediately | instant `min_w` | match |
| 77 | Rename pencil · enter/exit | opacity + scale 0.8, **150 ms easeOut**, enter gated 600 ms | was easeInOut | **fixed** |
| 78 | Rename pencil · press | CSS `transform .1s ease-in-out` | `pencil_press` | **fixed**: CSS reversal semantics |
| 79 | Editor play overlay | opacity + scale .88, 160 ms `[.25,1,.5,1]`, enter and exit | `play_overlay` | match |
| 80 | Editor "will be removed" ring | opacity 120 ms easeOut | `cut_ring` | match |
| 81 | `.lk-ed-region` hover | bg 140 ms ease-out-quart | `region_hover` | **fixed**: CSS reversal semantics |
| 82 | `.lk-ed-grip` hover | scaleX 1.6, 140 ms | `grip` | **fixed**: CSS reversal semantics |
| 83 | `.lk-ed-playhead` hover/active | scaleX 1.25 / 1.1, 120 ms | `head_scale` | **fixed**: CSS reversal semantics |
| 84 | `.lk-ed-iconbtn` | bg/border/scale(.94), 140 ms | `btn_hover/press` | **fixed**: CSS reversal semantics |
| 85 | Editor reduced motion | all editor transitions 1 ms; grip/playhead hover transforms removed | ignored | **fixed** (`ed_ms`, `ed_hover`) |
| 86 | `.lk-ed-strip` focus ring | box-shadow 180 ms on `:focus-visible` | no keyboard-focus ring on the strip | n/a (focus-visible styling not ported) |
| 87 | `.lk-ed-fade-in` | defined, unused | — | n/a |
| 88 | Editor playhead position | rAF-driven, no easing | per frame | match |
| 89 | Editor window active (Linux header dim) | 160 ms ease-out | `windows/editor.rs` | **fixed**: CSS reversal semantics |
| 90 | Recorder timer | rAF, hard text swap | — | match |
| 91 | Video player controls | videojs skin fade | `ui/video.rs` 150/400 ms easeOut | match (kept as ported) |
| 92 | Reduced motion (global) | motion ignores it; only NumberFlow and the editor CSS respect it | springs snap, fades play | deviation (README) |

**Counts:** 92 rows: 45 match, 40 fixed, 4 deliberate deviations (#9, #25,
#57, #92; all kept as improvements, listed in the README) and 3 n/a (#31,
#86, #87). Rows #23,
#38 and #66 are fixed with a documented GPUI approximation. Row #2 (the
button's CSS colour lag) is counted as a match because the owner asked for
buttons to be left exactly as they are.

## Not checked side by side

The slow-motion side-by-side comparison against the Tauri app was not done.
The native build was launched and checked visually.
