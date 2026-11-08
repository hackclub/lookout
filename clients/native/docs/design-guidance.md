# Design guidance for the native GPUI client

A checklist for making the GPUI rewrite of Lookout feel premium, native and fast. It is distilled from three design skills. Each item names its source. Numbers are quoted exactly. Where a source wrote CSS, the item gives the GPUI equivalent. Items marked **[inferred]** are my own GPUI or desktop translation and come from no source.

## Sources and tags

| Tag | Source | Location read |
|---|---|---|
| **IMP:x** | *impeccable* skill, `reference/x.md` (or `SKILL.src.md`) | `/home/anson/.t3/ref/impeccable/skill/` |
| **EMIL:x** | Emil Kowalski's skills, skill `x` (`STD` = `review-animations/STANDARDS.md`, `RCP` = `animate/RECIPES.md`) | `github.com/emilkowalski/skills`, cloned to `/tmp/research/emil-skills` |
| **TAC:x** | *tactile-design* skill (`kosta4a/tactile-design`), `SKILL.md` or `references/x.md` | local copy at `~/home/code/fallen/.claude/skills/tactile-design/` |

Notes on the sources:
- The impeccable repo's own `DESIGN.md` and `PRODUCT.md` describe Impeccable's marketing site (a "kinpaku gold on lacquer" brand). They do not apply here and are not used.
- impeccable has no GPUI or desktop-native reference. Its "native" files cover iOS and Android only, so the desktop items below are adapted from them.
- The tactile skill's own motion section credits Emil Kowalski. Where TAC and EMIL agree, both tags are given.

---

## 0. Ground rules (read first)

- [ ] **Decide whether something should animate before deciding how.** "The gate below exists to produce zero lines of code sometimes." (EMIL:animate)
- [ ] **Frequency sets the motion budget.** (EMIL:emil-design-eng, STD)

  | How often it's seen | Decision |
  |---|---|
  | 100+ times/day (keyboard shortcuts, command palette toggle) | No animation. Ever. |
  | Tens of times/day (hover effects, list navigation) | Remove or drastically reduce |
  | Occasional (modals, drawers, toasts) | Standard animation |
  | Rare/first-time (onboarding, celebrations) | Can add delight |

- [ ] **Never animate keyboard-triggered actions.** "Raycast has no open/close animation. That is the optimal experience." (EMIL:emil-design-eng). For Lookout, global start, stop and pause hotkeys change state instantly. They get no transition.
- [ ] **Name a purpose for every animation:** feedback, spatial consistency, state indication, preventing a jarring change, explanation, or delight (delight is for rare moments only). (EMIL:animate). If you can't name one, don't build it.
- [ ] **Motion explains state, relationship and hierarchy, or creates one authored moment the surface has earned. "Decoration without purpose is animation debt."** (IMP:animate)
- [ ] **Lookout is an "Operate" surface: "motion serves feedback, state, and continuity. Keep routine transitions fast and do not make users wait through page-load choreography."** (IMP:animate). The visual world lends "type, palette, density, and one signature move", and "never supplies the layout, the navigation model, or the controls" (IMP:mode-operate).
- [ ] **Data and figures never move for style.** "Charts that redraw from zero on every mount" and "Numbers that count up on page load" get deleted (EMIL:remove-animations). "Figures never move." (TAC:motion). This covers the elapsed-time counter and storage and upload stats.
- [ ] **Drag and resize are instant. Nothing animates while the user is dragging.** (TAC:motion)

---

## 1. Motion

### 1.1 Easing curves (cubic-bezier numbers)

Define these once as shared tokens, for example `pub const EASE_OUT: (f32, f32, f32, f32)` fed to a `cubic_bezier(x1, y1, x2, y2)` helper. The local `gpui-kit` ref has one at `crates/base/src/animation.rs` that plugs into `Animation::with_easing` [inferred]. "Five hand-typed cubic-beziers that almost match is a consolidation finding." (EMIL:improve-animations)

| Token | cubic-bezier | Use | Source |
|---|---|---|---|
| `EASE_OUT` (the default) | `0.23, 1, 0.32, 1` | Anything the user caused: enter, exit, press, menu, modal, hover colour | EMIL:emil-design-eng, STD; TAC:motion |
| `EASE_IN_OUT` | `0.77, 0, 0.175, 1` | On-screen movement or morphing where both ends stay visible. TAC limits it to "one-off" folds (250ms) | EMIL:STD; TAC:motion |
| `EASE_DRAWER` | `0.32, 0.72, 0, 1` | Sheets and drawers ("iOS-like drawer curve (from Ionic)"). TAC: spring back 400ms, slide out 300ms | EMIL:emil-design-eng; TAC:motion |
| `EASE_MOVE` | `0.22, 1, 0.36, 1` | Long moves of large objects, such as a panel resizing (400–450ms) | TAC:motion |
| `EASE_ARRIVE` | `0.16, 1, 0.3, 1` | "natural deceleration… for confident arrivals" | IMP:animate |
| Material standard (optional) | `0.4, 0, 0.2, 1` | Measured width and height changes: rail width 220ms, row height 140ms. "pick one per product" | TAC:motion |
| `EASE_DRAW` | `0.645, 0.045, 0.355, 1` | Stroke drawing, such as a checkmark (0.6s, then 0.4s) | EMIL:choreograph/RECIPES |
| linear | — | Constant motion only: progress, spinner rotation, shimmer | EMIL:STD; TAC:motion |

Rules:
- [ ] **Never ease-in on UI.** "A dropdown with `ease-in` at 300ms *feels* slower than `ease-out` at the same 300ms." (EMIL:emil-design-eng; TAC:motion)
- [ ] **Built-in easings are too weak.** Don't use GPUI's stock `ease_in_out` or `ease_out_quint` as a stand-in for the strong curve. (EMIL:animate. GPUI mapping [inferred].) "Never invent `cubic-bezier(0.4, 0, 0.2, 1)` because it looks familiar." (EMIL:animate). TAC allows it only as the opt-in measured-size curve above.
- [ ] Decision tree: enter or exit → ease-out; moving or morphing on screen → ease-in-out; hover or colour → `ease`; constant → linear; default → ease-out. (EMIL:STD). TAC overrides one case: hover and colour also use the strong ease-out. Pick one rule for the product and keep to it.
- [ ] **No bounce or elastic curves by reflex.** (IMP:animate)
- [ ] **Mirror the easing on reversible transitions** (use inverse control points for the return path). (EMIL:apple-design)

### 1.2 Durations

| Element | Duration | Source |
|---|---|---|
| Button press feedback | 100–160ms | EMIL:STD |
| Tooltips, small popovers | 125–200ms | EMIL:STD |
| Dropdowns, selects | 150–250ms | EMIL:STD |
| Modals, drawers | 200–500ms | EMIL:STD |
| "The workhorse": press, hover colour, toggle colour, menu open | **150ms** | TAC:motion |
| Content swaps, overlay fade, modal exit, reduced-motion fallback | **200ms** | TAC:motion |
| Modal card entrance | **240ms** | TAC:motion |
| Sheet rising | 320ms | TAC:motion |
| A fade "that carries no news" | 80ms | TAC:motion |
| Small exits (menu or popover closing) | 100–120ms | TAC:motion |
| Springs and long geometry only, "never a plain fade" | 400–500ms | TAC:motion |

impeccable's coarser scale: 100–150ms immediate feedback · 150–300ms routine state change · 300–500ms layout, overlay or view transition · 500–800ms a deliberately authored focal entrance. "Long feedback feels like latency." (IMP:animate)

- [ ] **UI animations stay under 300ms.** "A 180ms dropdown feels more responsive than a 400ms one." (EMIL:emil-design-eng). Over-budget UI motion is reduced to "200–250ms, same curve" (EMIL:remove-animations).
- [ ] **A faster spinner makes loading feel faster** for the same real wait. (EMIL:emil-design-eng)

### 1.3 Enter/exit asymmetry

- [ ] **Exit faster than entrance.** (IMP:animate; EMIL:STD; TAC:motion). TAC pairs:

  | Element | In | Out |
  |---|---|---|
  | Text or panel swap | 250ms | 150ms |
  | Desktop modal | 240ms | 200ms |
  | Desktop side panel | 240ms | 200ms |
  | Menu, dropdown, popover | 150ms | 120ms |

- [ ] **Slow where the user is deciding, fast where the system responds.** Hold-to-confirm runs 2s linear on press and snaps back in 200ms ease-out on release (EMIL:emil-design-eng). This fits a hold-to-discard-recording control.
- [ ] **Exits are "simultaneous, shorter and simpler". Don't reverse-stagger.** (EMIL:choreograph)
- [ ] **Exit along the entry path.** "If something disappears one way, we expect it to emerge from where it came." (EMIL:apple-design, choreograph)
- [ ] **A leaving element gives up its space immediately,** so its siblings close the gap while it fades. (EMIL:choreograph)
- [ ] **Unmount only after the exit has finished, and match timers to the durations.** (TAC:motion)

### 1.4 Springs

GPUI has no built-in spring [inferred]. Write a small critically-damped spring integrator stepped per frame, so values can retarget with their velocity intact.

| Use | Params | Source |
|---|---|---|
| Default UI spring | damping **1.0**, response **0.3–0.4** s (no overshoot) | EMIL:apple-design |
| Move or reposition (e.g. PiP) | damping 1.0, response 0.4 | EMIL:apple-design (Apple) |
| Drawer or sheet | damping 0.8, response 0.3 | EMIL:apple-design (Apple) |
| Momentum or flick only | damping ~0.8 | EMIL:apple-design |
| General Apple-style | `{ duration: 0.5, bounce: 0.2 }`. Physics form: `mass 1, stiffness 100, damping 10` | EMIL:emil-design-eng |
| Segmented toggle's sliding pill | bounce **0.12** / 0.4s | TAC:motion |
| Switch knob | bounce **0.2** / 0.45s | TAC:motion |
| Height reveals, progress fills | bounce 0.2 / 0.35–0.5s | TAC:motion |
| Camera pans | bounce 0 / 0.5s | TAC:motion |
| Rare celebration | bounce up to 0.45 | TAC:motion |

- [ ] **Keep bounce subtle (0.1–0.3), and avoid it in most UI.** (EMIL:emil-design-eng)
- [ ] **Add bounce only "when the gesture itself carried momentum".** (EMIL:apple-design)
- [ ] **Springs stay inside their containers.** Overshoot ≈ travel × e^(−π·(1−bounce)/√(1−(1−bounce)²)). At 0.35, a pill crossing 100px overshoots about 7px, which is past a 4px inset. (TAC:motion)
- [ ] **Smaller elements need more bounce to read as alive.** (EMIL:choreograph)
- [ ] **Split 2D motion into independent X and Y springs.** (EMIL:apple-design)

### 1.5 Interruptibility

- [ ] **"Interruptibility — the single most important principle."** Never lock out input during a transition. (EMIL:apple-design)
- [ ] **Always animate from the current on-screen value, never the target.** On interrupt, read the live value and start from there. (EMIL:apple-design). GPUI's `with_animation` restarts from progress 0 on a new id, which behaves like CSS keyframes. For anything that can be re-triggered (hover, press, popover open/close, toggles), store animated values in entity state and retarget them [inferred].
- [ ] **Transitions retarget and keyframes restart from zero.** Anything a user can fire twice in a second must retarget. (EMIL:animate; TAC:motion)
- [ ] **When a gesture reverses, blend velocity rather than hard-cutting it.** A hard cut feels like a "brick wall". (EMIL:apple-design)
- [ ] **Hand off release velocity** (`relativeVelocity = gestureVelocity / (target − current)`). **Project momentum** with `current + (v/1000)·d/(1−d)`, where d ≈ 0.998 (0.99 for snappier). (EMIL:apple-design)
- [ ] **Momentum dismissal:** dismiss when velocity > ~0.11 px/ms, whatever the distance. (EMIL:emil-design-eng; TAC:SKILL for sheets)
- [ ] **Rubber-band at boundaries:** `(overshoot·dim·0.55)/(dim + 0.55·|overshoot|)`. (EMIL:apple-design)
- [ ] **Each re-trigger stops in-flight animations before starting new ones. Never stack them.** (EMIL:choreograph)
- [ ] **Interrupted gestures:** if the pointer is released outside, capture is lost, or the window loses focus mid-drag, clear the drag state. The next drag must work. (IMP:harden)

### 1.6 Properties, scale, travel, blur

- [ ] **Animate transform and opacity.** Animating width, height, padding, margin, top or left triggers layout. (EMIL:STD; IMP:animate). In GPUI every frame re-runs layout anyway, so the practical rule is: never animate something that reflows siblings. Prefer offsets and opacity inside a fixed-size slot [inferred].
- [ ] **Never scale from 0.** Enter from `0.95–0.97` with opacity 0. (EMIL:STD; TAC:motion). Decorative shapes may start at 0.2–0.6; "UI never does" (TAC:motion).
- [ ] **Popovers scale from their trigger. Modals stay centered.** (EMIL:emil-design-eng)
- [ ] **Travel grows with size:** 4px phone menu · 6px text and label swaps · 8px step blocks · 16px page sections and modal card · 24px toast exit. (TAC:motion)
- [ ] **Blur bridges crossfades:** 2px small text · 4px panels, icons, modal card · 6px headline text · 8px large panel. (TAC:motion). Keep animated blur "under 20px; 2–5px is enough to soften a crossfade" (EMIL:animation-performance). GPUI has no general element blur filter [inferred], so use fade plus travel and skip the blur.
- [ ] **Label swap** (e.g. "Start" → "Recording…"): both states share one cell. The new one rises from +6px, the old one leaves to −6px, with blur(2px), over 200ms ease-out. No animation on mount. (TAC:motion)
- [ ] **Stagger:** 30–80ms between items, total under ~300ms. After about 6 items, the rest share the 6th item's delay. (EMIL:choreograph). TAC uses 40–80ms. "Uniform stagger is forbidden": vary timing by importance (EMIL:choreograph).
- [ ] **One entrance per container.** Never animate a parent's entrance and also stagger its children. (EMIL:choreograph)
- [ ] **Never stagger high-frequency UI** (menus, list rows, tabs). (EMIL:choreograph)
- [ ] **Choreography never blocks interaction.** (EMIL:choreograph)
- [ ] **No animation on first paint** for state settled on load. A pre-placed knob or pill snaps into place. (TAC:motion)

### 1.7 Reduced motion

- [ ] **"Reduced motion means fewer and gentler animations, not disabling all motion."** Keep opacity and colour; remove spatial movement. (IMP:animate; EMIL:emil-design-eng)
- [ ] TAC mapping (TAC:motion):

  | Normal | Reduced |
  |---|---|
  | travel, blur, scale, spin | a 150–200ms opacity crossfade |
  | a loop that carries state (live, waiting) | keeps only its opacity pulse |
  | height changes | instant |
  | a toggle's sliding pill | a 150ms tween |
  | a switch knob | instant |
  | press scale | may stay (it's feedback, not travel) |

- [ ] **Collapse choreography; don't just shorten it.** Drop delays and movement, keep opacity. (EMIL:choreograph)
- [ ] **Also honour reduced transparency** (make translucent surfaces solid) **and increased contrast** (near-solid backgrounds with a defined border). Ease theme changes instead of jumping brightness. (EMIL:apple-design)
- [ ] **Read the OS setting at startup and when it changes** [inferred]. Settings by platform: macOS `NSWorkspace.accessibilityDisplayShouldReduceMotion` and `…ReduceTransparency`; Windows `SPI_GETCLIENTAREAANIMATION`; GNOME `org.gnome.desktop.interface enable-animations`. Also add an in-app override.

---

## 2. Press and hover feedback

- [ ] **Respond on pointer-down, not release.** (EMIL:apple-design). In GPUI, apply pressed styling on `on_mouse_down`, or use `.active()`, and commit on mouse-up [inferred].
- [ ] **Press scale 0.97** with a transition of 100–160ms ease-out (EMIL:STD; TAC: 150ms). The squeeze deepens as targets shrink: **.99** cards and full-width rows · **.97** buttons and pills · **.94** 44px round icon buttons · **.92** 22px icon buttons. (TAC:buttons-and-controls)
- [ ] **Disabled controls cancel both press and hover.** (TAC:buttons-and-controls)
- [ ] **Hover changes colour only: 150ms, nothing moves, fine pointer only.** (TAC:motion). Hover with scale or lift is reduced to "Color/opacity change only, ≤150ms" (EMIL:remove-animations).
- [ ] **Each row fades its own hover fill in place. Never slide one shared highlight between rows** ("it makes the list look as if it is shuffling"). (TAC:motion)
- [ ] **Hover = local ground + 1 step; selected = + 2 steps.** "Hover always stays a step below selected." (TAC:tokens)
- [ ] **Tooltips:** delay the first one. After that, neighbours open instantly with no animation. Tooltip transition 125ms from `scale(0.97)`. (EMIL:emil-design-eng, RCP). TAC: "Icon tooltips: zero delay."
- [ ] **Copy actions show the tick on click, before the clipboard answers.** Hold it 1000ms. No toast. (TAC:motion)
- [ ] **Blocked action:** shake in place `x: [0, -6, 6, -4, 4, 0]` over 420ms, and say why. (TAC:motion)
- [ ] **Loading inside a button:** the label swaps in place to a spinner plus "Saving…". (TAC:SKILL)

---

## 3. Typography

- [ ] **Default to the platform's system font** before a custom face: it ships optical sizing and tracking tables. "Override only with a reason." (EMIL:apple-design). Platform fonts are SF Pro on macOS, Segoe UI Variable on Windows, and the user's interface font on Linux [inferred].
- [ ] **Tracking depends on size.** Tighten display text (about −0.02em) and keep body near 0. (EMIL:apple-design; EMIL ui-cheatsheet). Floor: −0.04em (IMP:craft-floor). Button labels −0.01em (TAC:SKILL).
- [ ] **Leading tracks size inversely:** tight on large headings (e.g. 1.05) and looser on body. (EMIL:apple-design)
- [ ] **TAC scale for dense product UI:** body and controls **13/20** ("13px, not 14") · meta **12/16** · card and dialog titles **16/24** · section titles **20/28** · display **32/40, −0.02em**. Use one weight. Hierarchy is a size step plus an ink step (12, 11, 10), "not bold". (TAC:SKILL). Note that impeccable sets a 16px body floor for *web* (IMP:typeset). A dense desktop tool may go lower: impeccable allows "a dense role, platform convention".
- [ ] **Build hierarchy from weight, size and leading together, not size alone.** (EMIL:apple-design; IMP:typeset)
- [ ] **Use tabular numerals for anything that ticks.** The recording timer, durations and sizes all need them ("Numbers jitter when they change", EMIL ui-cheatsheet; TAC tables use tabular figures).
- [ ] **Monospace only for IDs, code, keys and machine values.** Not "as a costume for 'technical'." (TAC:SKILL; IMP:craft-floor)
- [ ] **Prose measure 45–75ch** (IMP:typeset); 65–75ch at the craft floor (IMP:craft-floor).
- [ ] **Light text on dark surfaces needs slightly more leading, slightly more tracking, and one step more weight** if the face needs it. (IMP:typeset)
- [ ] **Respect the OS text-scaling setting and lay out with it.** (EMIL:apple-design; IMP:typeset)
- [ ] **Copy:** sentence case. Buttons are a verb plus an object. Use a real ellipsis ("Opening…"), not three dots. No Title Case labels. (TAC:layout-and-hierarchy, SKILL)

---

## 4. Colour

- [ ] **Build roles, not a bag of swatches:** canvas and elevated surfaces, primary and secondary text, action, focus and selection, borders, success, warning, error and info. (IMP:colorize)
- [ ] **Use a 12-step neutral with fixed jobs:** 1–2 grounds, 3–5 fills, 6–8 lines, 9–10 faint text, 11 secondary text, 12 strong text. Coloured scales: 3 tint, 6 edge, 9 solid, 10 solid hover, 11 text. (TAC:SKILL)
- [ ] **Hue carries meaning only** (status or identity), as a −3 tint with −11 text. "Hue never ranks importance." (TAC:SKILL). For Lookout, the recording red is a status colour. Don't spend it on decoration (IMP:colorize: "do not spend its color on decoration").
- [ ] **Have one primary (ink-12) action per surface. Destructive is never the primary.** (TAC:SKILL)
- [ ] **Contrast:** body ≥ **4.5:1**, large text ≥ **3:1**, controls, icons and focus indicators ≥ **3:1**. (IMP:colorize). Check every state and both themes. Never rely on colour alone.
- [ ] **On coloured surfaces, tint secondary text from the hue; "never gray".** (IMP:craft-floor)
- [ ] **Compose dark mode on its own terms; never invert it mechanically.** (IMP:colorize). In dark mode, raised surfaces are 1–2 steps lighter with a solid 0.5px outline and a tight black drop. Never a lit top edge, never a glow, and a shadow alone is never the only elevation cue. (TAC:SKILL)
- [ ] **Shadows are crisp:** ring + `0 1px 2px 0 rgb(ink / .12)` + at most one short drop (card `0 3px 4px -2px`, menu `0 4px 8px -4px`, modal `0 6px 8px -4px`). "No big soft blurs." (TAC:SKILL). Shadows need an offset and a soft blur; "A zero-offset colored halo is decoration" (IMP:craft-floor).
- [ ] **Choose light or dark from the use scene, not the category.** (IMP:craft-floor). For a desktop app, follow the OS appearance by default [inferred].
- [ ] **Theme the surfaces you didn't draw:** text selection, caret, scrollbars, focus rings. (IMP:craft-floor)

---

## 5. Spacing and layout

- [ ] **Use a 4px grid:** 4–8px inside a control · 12–16px between items · 24–32px between sections. (TAC:SKILL). "A 4-unit base usually provides the useful middle steps that an 8-only scale misses." (IMP:layout)
- [ ] **Keep groups tight and separations generous, with more space above a heading than below it.** (IMP:craft-floor)
- [ ] **Group by meaning:** "Use proximity before adding containers." (IMP:layout). Don't nest cards (IMP:craft-floor).
- [ ] **Control heights:** 36px default · 32 compact · 28 pills · 24 inline · 22 row icons · 20 badges. Radius 6px for controls and 8px for cards and overlays. Padding 12–14px a side. (TAC:SKILL)
- [ ] **Column widths by page type:** 800px to read · 1200px for data · 440px for a form or flow. (TAC:SKILL)
- [ ] **Put a section's actions at the right of its title, above its data.** Lists stop at 5–6 rows, then "View all". (TAC:SKILL)
- [ ] **Siblings share height, padding, radius, text size and ink.** "An unmatched sibling is a bug." (TAC:SKILL)
- [ ] **Inner radius = outer radius − border.** No double borders. Structural lines ≤ 0.5px. (TAC:SKILL)
- [ ] **Layout stability:** states that swap share one cell sized to the larger. Reserve height for rotating text. Rest positions land on whole pixels. (TAC:motion). "Layout that jumps when data, a label or a loading state arrives" is a never-ship. (TAC:SKILL)

---

## 6. Focus and keyboard

- [ ] **Use one focus-ring token on every control**, drawn at 2px, ≥ 3:1 on every surface. (TAC:buttons-and-controls; IMP:colorize)
- [ ] **Show focus only for keyboard focus.** Fields deepen one step on focus. (TAC:SKILL). In GPUI, use `.focus_visible()` rather than `.focus()` for rings [inferred].
- [ ] **Modals:** focus the first field, trap Tab, lock scroll. Cancel on the left, confirm on the right. (TAC:SKILL). On Windows the convention reverses; see §8 [inferred].
- [ ] **Every function works from the keyboard, in a logical tab order, with focus managed in modals.** (IMP:harden)
- [ ] **When an animation is removed, move focus immediately.** Focus must not wait on motion. (EMIL:remove-animations)
- [ ] **Screen readers:** label every control, keep reading order logical, announce state. (IMP:audit.native). GPUI's accessibility support is limited, so track it as a known gap [inferred].

---

## 7. Empty, error and loading states

- [ ] **Every control needs default, hover, focus, active, disabled, loading, error and success behaviour.** (IMP:polish)
- [ ] **Empty:** a bare centred panel with 44px padding, an 18px icon and two lines: what's missing, then what to do. "No recordings yet." / "Press Start and your first recording will appear here." (TAC:layout-and-hierarchy, adapted copy)
- [ ] **Failed load:** the empty layout, two lines, and one secondary "Try again". (TAC:layout-and-hierarchy). "Errors name the problem and the recovery." (IMP:craft-floor). Don't block the whole interface when one component fails (IMP:harden).
- [ ] **Loading:** use a spinner in the empty layout, or skeletons that keep labels and widths and replace only values. "Never show a wrong label while loading." Be honest: "Uploading…" until it has actually landed. (TAC:layout-and-hierarchy). Never fake progress (IMP:delight).
- [ ] **No spinner or skeleton for operations under 300ms:** "the flash reads as a glitch." (EMIL:remove-animations)
- [ ] **Skeletons pulse between two ink steps over 1–1.4s, alternating, with no gradient sweep.** Status dots for ongoing states share one clock. (TAC:motion)
- [ ] **"Live" states such as the recording indicator** loop slowly: "linear or a breathing ease-in-out" (TAC:motion). Infinite loops on persistent elements otherwise "run once" (EMIL:remove-animations). Nonessential loops "stop when offscreen or hidden" (IMP:animate). Stop animating when the window is hidden or minimised [inferred].
- [ ] **Stress-test with real data:** long names (100+ chars), emoji, CJK, RTL, 1000+ items, "1 members" pluralisation, offline, and double clicks. (IMP:harden; EMIL:break-ui). Free text truncates past 32ch with a tooltip, and missing values show an en dash (TAC:SKILL).
- [ ] **Prevent double submission:** disable while pending, and use optimistic updates with rollback. (IMP:harden)
- [ ] **Delight belongs only at first use, completion and recovery,** "proportional to frequency and consequence". A routine save "should simply feel certain". (IMP:delight). A first successful upload may earn one; the 50th stop does not.

---

## 8. Native feel per OS

The sources cover iOS, Android and Apple's general principles. Desktop specifics marked [inferred] are mine.

**General (all OSes)**
- [ ] **Score yourself on "does this read as a native app or a ported website?"** Flag web-shaped controls, hover-dependent affordances and mixed icon sets. (IMP:audit.native)
- [ ] **"Translate idioms; never transplant them."** Rebuild navigation and controls in each platform's vocabulary, and carry over only the brand layer (palette intent, type accent, motion personality). (IMP:adapt.native)
- [ ] **Familiarity:** "things that look the same must behave the same and live in the same place (close is always top-left on macOS)." (EMIL:apple-design)
- [ ] **Icons are drawn from one library, with one stroke and weight.** No Unicode glyphs or emoji as icons. (IMP:craft-floor)
- [ ] **Check for dropped frames and runtime warnings on every supported OS version.** (IMP:polish)

**macOS**
- [ ] Traffic lights stay top-left, with a transparent or unified titlebar via GPUI `TitlebarOptions { appears_transparent: true, traffic_light_position }` [inferred].
- [ ] **Use materials as layers:** translucent chrome with content underneath. Heavier material marks structure (the sidebar) and lighter marks interactive items. "Never stack a light translucent surface on another." (EMIL:apple-design). Use GPUI `WindowBackgroundAppearance::Blurred` only for sidebar or popover chrome [inferred].
- [ ] **Use scroll edge effects instead of hard 1px dividers under sticky headers.** (EMIL:apple-design)
- [ ] **System transitions:** "dismiss reverses the entrance", and "Custom transitions that fight the navigation model disorient." (IMP:ios, applied to macOS)
- [ ] Menu bar item for record and stop. Cmd-based shortcuts. Respect Reduce Motion, Reduce Transparency and Increase Contrast (EMIL:apple-design). Native fullscreen [inferred].

**Windows**
- [ ] **Test in Windows high contrast mode.** (IMP:harden)
- [ ] [inferred] Caption buttons top-right, with snap-layout hover support on maximise. Mica/Acrylic backdrop only on Windows 11. Segoe UI Variable. Dialog button order puts the primary on the left (OK, then Cancel). Ctrl shortcuts. A tray icon for recording status.

**Linux**
- [ ] [inferred] No source covers Linux. Support client-side decorations on Wayland, with server-side decorations where offered. Honour GNOME `enable-animations` and the font and scaling settings. Don't assume blur or translucency is available. Expect fractional scaling (rest positions land on whole pixels; TAC:motion).

---

## 9. Snappiness and performance

- [ ] **Frame budget: 16.7ms at 60Hz, 8ms at 120Hz. "Design against the smaller number."** (EMIL:animation-performance). ProMotion Macs and 120/144Hz Windows panels are common.
- [ ] **"Never conclude 'it's fine' from a fast machine."** Profile on a mid-range target. (EMIL:animation-performance)
- [ ] **Keep latency off the input path.** "Audit debounces, artificial timers, transition waits." (EMIL:apple-design)
- [ ] **Startup:** no heavy work before first frame. (IMP:audit.native)
- [ ] **Virtualise long lists** (GPUI `uniform_list` / `list`). (IMP:audit.native; EMIL performance cheatsheet)
- [ ] **No main-thread jank:** no synchronous work in scroll or gesture paths. (IMP:audit.native). Recorder, encoding and upload work stay off the UI thread, and the UI only reads snapshots [inferred].
- [ ] **No wasted rendering.** (IMP:audit.native). Only `cx.notify()` the entity that changed. Don't notify every frame for a 1Hz timer; tick once a second [inferred].
- [ ] **Decode thumbnails at display size and cache them.** Never decode full-size frames for thumbnails. (IMP:audit.native)
- [ ] **"Name the cause before changing code."** (EMIL:animation-performance)
- [ ] **Measure on target devices** rather than assuming an effect is cheap. (IMP:animate)

---

## 10. Anti-patterns (never ship)

From EMIL:animate "Never Ship", TAC:SKILL "Never ship", IMP:craft-floor "Refuse":

- [ ] Animation on keyboard shortcuts, typing, drag, resize or data figures (EMIL, TAC)
- [ ] Ease-in on UI; built-in weak ease-out on deliberate motion (EMIL)
- [ ] Scale from 0 (EMIL, TAC)
- [ ] Keyframe-style restart-from-zero on anything that can be re-triggered (EMIL, TAC)
- [ ] UI duration > 300ms with no reason (EMIL)
- [ ] Popover scaling from centre instead of its trigger (EMIL)
- [ ] A transition with no reduced-motion path (EMIL, TAC)
- [ ] Everything entering at once on an occasional surface; uniform or long staggers; staggers on frequent surfaces (EMIL)
- [ ] Hover that moves things; one sliding hover highlight; hover-only information (TAC)
- [ ] More than one primary per surface; destructive styled as primary (TAC)
- [ ] Hierarchy made with bold or colour (TAC)
- [ ] Big soft shadow blurs; glows; lit top edges in dark mode; hard offset shadows (TAC, IMP)
- [ ] Gradient text; glass and blur as decoration; coloured side-stripe borders > 1px; nested cards; identical card grids (IMP:craft-floor)
- [ ] Eyebrow labels over headings ("a ban, not a default") (IMP:craft-floor; TAC)
- [ ] A modal for a task that needs neither interruption nor protected focus (IMP:craft-floor)
- [ ] Spinners and skeletons for sub-300ms work; infinite decorative loops (EMIL:remove-animations)
- [ ] Fake data, wrong labels while loading, layout that jumps (TAC)
- [ ] Generic error copy ("Error occurred") (IMP:harden)
- [ ] Toasts for successful copies (TAC)
- [ ] Sound without consent or ignoring mute (IMP:delight)

---

## 11. Verify

- [ ] **Play every animation at 2–5× duration** and step through it frame by frame. (EMIL:emil-design-eng). Add a debug "slow motion" multiplier to the motion tokens [inferred].
- [ ] **Check at 4–8× zoom, in both themes, with worst-case data and every state.** (TAC:SKILL)
- [ ] **Review with fresh eyes the next day.** (EMIL:emil-design-eng; TAC:motion)
- [ ] **Imagine the 50th use of the day: "If you'd skip it, cut it."** (EMIL:choreograph)
- [ ] **"Removing an animation would lose meaning or authored character, not merely decoration."** (IMP:animate)
