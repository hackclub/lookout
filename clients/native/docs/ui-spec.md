# Lookout desktop: current UI specification (source of truth for the GPUI rewrite)

Generated 2026-11-07 from `main` @ `ea12dd1`, covering `clients/desktop` (Tauri 2 + React 19), `clients/react` (`@lookout/react`) and `packages/shared`. The goal is a pixel- and motion-identical native port. Every value is copied from source and carries a repo-relative `file:line` reference. Each section declares its own path abbreviations at its top (e.g. `D/` = `clients/desktop/src/`, `R/` = `clients/react/src/`, `ST` = `clients/desktop/src-tauri`).

How styling works: there is no Tailwind and almost no stylesheet. Nearly all styling is React inline `style={{…}}` objects that reference tokens from `clients/react/src/ui/theme.ts`. Those tokens are CSS custom properties that switch between dark and light. Global CSS lives in `clients/desktop/index.html`, in stylesheets injected at runtime (`theme.ts`, `linuxChrome.ts`, vaul, the shimmer keyframes from LookoutProvider) and in `VideoPlayer.css`. Motion comes from `motion/react` (framer-motion 14.1.0, pulled in through `@lookout/react`), vaul 1.1.2, `@number-flow/react` 0.6.2, CSS transitions/keyframes, rAF loops and SwiftUI (macOS menu bar and add menu).

## Contents
1. Design tokens (colors light/dark, fonts, spacing, radii, shadows, blur, global CSS, primitives, third-party animation defaults)
2. Windows (Tauri windows, native macOS panels and menu-bar item, tray, GNOME pill, Waybar)
3. Screens & components
   - 3A. Main window: routing/state machine, HeaderBar, PageLayout, Gallery, RecordPage, SourcePicker, DesktopRecorder, timer, ProgramPanel, PanelPrompt, OpenInProgram, SourceLostPrompt, SessionDetail
   - 3B. Secondary: Sheet, TipDrawer, UpdateDrawer, UpdatePill, NamingModal, CaptureErrorDialog, PermissionScreen, AnnouncementBanner, AddSessionPage, SettingsPage
   - 3C. TrayApp, AddMenuPopup, EditorWindow, TimelapseEditor, VideoPlayer
4. Animations (exhaustive tables by area)
5. Behaviours (platform matrix, menus, keyboard shortcuts, focus, drag regions, Linux CSD, resize handles)
6. IPC contract (Tauri commands, events, plugin APIs, localStorage)

Primitives (Button, Spinner and so on) appear in several sections, because each screen section summarises the ones it uses. Section 1.10 is the canonical definition.

## 1. Design tokens

### 1.1 Where styles live (complete inventory)

There is **no Tailwind, no CSS-in-JS library, and no `.css` file in the desktop app**. Styling is:

| Mechanism | Where | Notes |
|---|---|---|
| Inline `style={{…}}` objects using tokens from `@lookout/react` (`colors`, `spacing`, `radii`, `fontSize`, `fontWeight`) | every component | The dominant mechanism. `colors.*` values are `var(--…)` strings, so they resolve through the CSS variables below. |
| Global `<style>` in HTML | `clients/desktop/index.html:41-128` | @font-face, reset, body font/colour, `pulse` + `spin` keyframes, scrollbar hiding, first-paint backgrounds, Linux CSD frame. |
| Theme variable sheet injected on import | `clients/react/src/ui/theme.ts:3-158` (`<style data-lookout-theme>`) | All `--color-*` variables, dark default + light (media query + `[data-theme="light"]`). |
| SDK keyframes sheet injected by `LookoutProvider` effect | `clients/react/src/LookoutProvider.tsx:91-104` (`<style data-lookout-keyframes>`) | `pulse`, `spin`, **`shimmer`** (the only definition of `shimmer`). Desktop only mounts `LookoutProvider` inside RecordPage (`clients/desktop/src/components/RecordPage.tsx:318`), so `shimmer` only exists once RecordPage has mounted. |
| Linux chrome sheet injected on import (Linux only) | `clients/desktop/src/linuxChrome.ts:132-299`, injected `:330-335` (`<style data-lookout-linux-chrome>`) | Adwaita header-bar/popover vars, cursor overrides, CSD frame, backdrop dimming, blur plate. |
| HeaderBar sheet injected on import | `clients/desktop/src/components/HeaderBar.tsx:28-85`, injected `:91-96` (`<style data-lookout-headerbar>`) | `.lookout-headerbar-control`, `.lookout-headerbar-action` hover/active/focus. |
| Editor sheet | `clients/react/src/components/editorStyles.ts:12-82` (`<style data-lookout-editor-styles>`) | `.lk-ed-*` classes for TimelapseEditor. |
| Inline `<style>` JSX in components | `clients/desktop/src/components/SettingsPage.tsx:285-293` and `:962-971`; `clients/desktop/src/components/TrayApp.tsx:150-161` | App-row hover/press; icon fade keyframes; tray transparency override. (Owned by the component sections; values repeated in the animation table.) |
| Side-effect CSS import | `clients/react/src/components/VideoPlayer.css` (imported by `VideoPlayer.tsx`, inlined by tsup `injectStyle`; `clients/react/src/css.d.ts:1-4`) | video.js skin tweaks. |
| Animation libraries | `motion` 14.1.0 (`motion/react`, framer-motion), `vaul` 1.1.2, `@number-flow/react` 0.6.2, `@squircle-js/react` 1.3.0 (all in root `node_modules/`) | `motion/react` is imported by `clients/react/src/ui/{Button,Card,Overlay,ErrorDisplay}.tsx` and by desktop `App.tsx, RecordPage.tsx, ProgramPanel.tsx, AddMenuPopup.tsx, NamingModal.tsx, UpdatePill.tsx, SourcePicker.tsx, SettingsPage.tsx`. Note: `motion` is not listed in `clients/desktop/package.json`; it comes transitively via `@lookout/react`. |

Global DOM-level behaviour from `clients/desktop/src/main.tsx`: right-click context menu disabled globally (`main.tsx:128`); render crash fallback UI (`main.tsx:74-125`, see 1.9).

### 1.2 Fonts

`@font-face` (`clients/desktop/index.html:42-59`), all `font-style: normal`, `format('woff2')`:

| family | weight | file | size on disk |
|---|---|---|---|
| `'Geist'` | 400 | `/fonts/Geist-Regular.woff2` (`clients/desktop/public/fonts/Geist-Regular.woff2`) | 45196 B |
| `'Geist'` | 500 | `/fonts/Geist-Medium.woff2` | 46416 B |
| `'Geist'` | 600 | `/fonts/Geist-SemiBold.woff2` | 46428 B |

No 700 face is shipped although `fontWeight.bold = 700` exists (synthesised bold if Geist is the active face). `clients/desktop/public/` contains only `fonts/` (these three files).

Font stacks:

| selector | font-family | ref |
|---|---|---|
| `body` (all platforms) | `-apple-system, BlinkMacSystemFont, 'Geist', 'Segoe UI', Roboto, sans-serif` | `index.html:62` |
| `html.os-linux body` | `system-ui, 'Adwaita Sans', Cantarell, sans-serif` (Geist deliberately absent) | `index.html:125-127` |
| TrayApp root | `system-ui, -apple-system, sans-serif` | `clients/desktop/src/components/TrayApp.tsx:166` |
| ErrorDisplay `<pre>`/error spans, ErrorBoundary | `monospace` | `clients/react/src/ui/ErrorDisplay.tsx:26,37,46`; `main.tsx:96` |

**Effective face per platform (important for pixel parity):** macOS resolves `-apple-system` first → **SF Pro (system font), not Geist**. Windows WebView2 ignores `-apple-system`/`BlinkMacSystemFont` → **Geist**. Linux → system UI font (Adwaita Sans / Cantarell / fontconfig sans). So Geist is effectively only rendered on Windows.

Font-size scale (`clients/react/src/ui/theme.ts:203`) — px:

| token | xs | sm | md | lg | xl | xxl | heading | display | timer |
|---|---|---|---|---|---|---|---|---|---|
| px | 11 | 12 | 13 | 14 | 16 | 18 | 20 | 24 | 32 |

Font-weight scale (`theme.ts:204`): `normal 400`, `medium 500`, `semibold 600`, `bold 700`.

No global line-height or letter-spacing is defined anywhere in the token layer (browser default `line-height: normal`, ≈1.15–1.2 depending on face). Per-component line-heights/letter-spacings are documented with each component. NumberFlow forces `line-height:1` on its host (see 1.8).

Text rendering globals: `user-select: none; -webkit-user-select: none; cursor: default; overscroll-behavior: none` on body (`index.html:62`). `body.os-windows { transform: rotate(0.001deg); }` (`index.html:71`) — a compositing hack; the `os-windows` class is added for **both Windows and Linux** UAs (`index.html:134-139`).

### 1.3 Spacing and radii

`spacing` (`theme.ts:201`) px: `xs 4`, `sm 8`, `md 12`, `lg 16`, `xl 20`, `xxl 24`, `xxxl 32`.

`radii` (`theme.ts:202`) px: `sm 6`, `md 8`, `lg 10`. Other radii used by primitives: Overlay panel `14` (`Overlay.tsx:100`), Badge `999` (`Badge.tsx:25`), Linux window `12` (`linuxChrome.ts:39` `WINDOW_RADIUS`; also `index.html:104`). vaul drawer background-scale radius 8 (lib constant, not used: `shouldScaleBackground` defaults false).

Linux CSD geometry (`clients/desktop/src/linuxChrome.ts`): `HEADER_BAR_HEIGHT = 47` (:34, Adwaita 46 + 1px hairline), `WINDOW_RADIUS = 12` (:39), `WINDOW_MARGIN = 40` (:90, transparent frame per side), `RESIZE_BAND = 8` (:100), `SHADOW_PASSTHROUGH = 32` (:106), `FRAME_INSET = SHELL_DRAWS_FRAME ? 0 : 32` (:117). HeaderBar control size 24 (`HeaderBar.tsx:21`), header action 30×30 (`HeaderBar.tsx:58-59`).

### 1.4 Colors — CSS variables (theme.ts)

Selection logic (`clients/react/src/ui/theme.ts:7-156`):
- `:root { … }` = **dark** values, the default/fallback (`:7-68`).
- `@media (prefers-color-scheme: light) { :root:not([data-theme="dark"]) { … } }` = light (`:69-113`).
- `:root[data-theme="light"] { … }` = light, forced (`:114-156`).
- `data-theme="dark"` forces dark by defeating the media query.

`--color-accent-hover` is declared twice in each block: first a fallback `#2f6fd0`, then `color-mix(in oklab, var(--color-accent) 88%, black)` which wins where supported (`:65-66`, `:109-110`, `:153-154`).

| variable | `colors.*` key | dark (default) | ref | light | ref |
|---|---|---|---|---|---|
| `--color-bg-body` | `bg.body` | `#242424` | :20 | `#fafafa` | :71 / :115 |
| `--color-bg-panel` | `bg.panel` | `#1e1e1e` | :21 | `#ffffff` | :72 / :116 |
| `--color-modal-backdrop` | `bg.backdrop` | `rgba(0, 0, 0, 0.55)` | :22 | `rgba(0, 0, 0, 0.35)` | :73 / :117 |
| `--color-bg-surface` | `bg.surface` | `rgba(255, 255, 255, 0.08)` | :23 | `#ffffff` | :74 / :118 |
| `--color-bg-sunken` | `bg.sunken` | `rgba(255, 255, 255, 0.04)` | :24 | `rgba(0, 0, 0, 0.04)` | :75 / :119 |
| `--color-text-primary` | `text.primary` | `#ffffff` | :25 | `rgba(0, 0, 0, 0.85)` | :76 / :120 |
| `--color-text-inverse` | `text.inverse` | `#000000` | :26 | `#ffffff` | :77 / :121 |
| `--color-text-secondary` | `text.secondary` | `rgba(255, 255, 255, 0.7)` | :27 | `rgba(0, 0, 0, 0.65)` | :78 / :122 |
| `--color-text-tertiary` | `text.tertiary` | `rgba(255, 255, 255, 0.5)` | :28 | `rgba(0, 0, 0, 0.45)` | :79 / :123 |
| `--color-text-quaternary` | `text.quaternary` | `rgba(255, 255, 255, 0.28)` | :29 | `rgba(0, 0, 0, 0.25)` | :80 / :124 |
| `--color-text-error` | `text.error` | `#fca5a5` | :30 | `#ef4444` | :81 / :125 |
| `--color-border-default` | `border.default` | `rgba(255, 255, 255, 0.12)` | :31 | `rgba(0, 0, 0, 0.12)` | :82 / :126 |
| `--color-border-hover` | `border.hover` | `rgba(255, 255, 255, 0.22)` | :32 | `rgba(0, 0, 0, 0.22)` | :83 / :127 |
| `--color-bg-selected` | `bg.selected` | `rgba(255, 255, 255, 0.12)` | :33 | `rgba(0, 0, 0, 0.1)` | :84 / :128 |
| `--color-border-selected` | `border.selected` | `rgba(255, 255, 255, 0.3)` | :34 | `rgba(0, 0, 0, 0.3)` | :85 / :129 |
| `--color-icon-selected` | `icon.selected` | `rgba(255, 255, 255, 0.8)` | :35 | `rgba(0, 0, 0, 0.8)` | :86 / :130 |
| `--color-status-neutral` | `status.neutral` | `rgba(255, 255, 255, 0.2)` | :36 | `#000000` | :87 / :131 |
| `--color-spinner-base` | `spinner.base` | `rgba(255, 255, 255, 0.1)` | :37 | `rgba(0, 0, 0, 0.1)` | :88 / :132 |
| `--color-spinner-track` | `spinner.track` | `rgba(255, 255, 255, 0.8)` | :38 | `rgba(0, 0, 0, 0.8)` | :89 / :133 |
| `--color-skeleton-bg` | `skeleton.bg` | `rgba(255, 255, 255, 0.06)` | :39 | `rgba(0, 0, 0, 0.05)` | :90 / :134 |
| `--color-skeleton-shimmer` | `skeleton.shimmer` | `rgba(255, 255, 255, 0.12)` | :40 | `rgba(0, 0, 0, 0.08)` | :91 / :135 |
| `--color-badge-primary-bg` | `badge.primaryBg` | `#22c55e26` (green @ 0x26 ≈ 15%) | :41 | `#22c55e` | :92 / :136 |
| `--color-badge-primary-text` | `badge.primaryText` | `#22c55e` | :42 | `#ffffff` | :93 / :137 |
| `--color-badge-overlay-bg` | `badge.overlayBg` | `rgba(0, 0, 0, 0.7)` | :43 | `#000000` | :94 / :138 |
| `--color-badge-overlay-text` | `badge.overlayText` | `#ffffff` | :44 | `#ffffff` | :95 / :139 |
| `--color-archive-bg` | — (raw var) | `rgba(0, 0, 0, 0.6)` | :45 | `rgba(255, 255, 255, 0.9)` | :96 / :140 |
| `--color-archive-icon` | — | `#ffffff` | :46 | `#000000` | :97 / :141 |
| `--color-archive-border` | — | `rgba(255, 255, 255, 0.1)` | :47 | `rgba(0, 0, 0, 0.1)` | :98 / :142 |
| `--color-archive-hover-bg` | — | `rgba(255, 255, 255, 0.1)` | :48 | `rgba(255, 255, 255, 1)` | :99 / :143 |
| `--color-archive-hover-border` | — | `rgba(255, 255, 255, 0.2)` | :49 | `rgba(0, 0, 0, 0.2)` | :100 / :144 |
| `--color-well` | `editor.well` | `rgba(0, 0, 0, 0.32)` | :53 | `rgba(0, 0, 0, 0.08)` | :101 / :145 |
| `--color-well-border` | `editor.wellBorder` | `rgba(255, 255, 255, 0.1)` | :54 | `rgba(0, 0, 0, 0.1)` | :102 / :146 |
| `--color-cut-fill` | `editor.cutFill` | `rgba(248, 113, 113, 0.26)` | :55 | `rgba(220, 38, 38, 0.20)` | :103 / :147 |
| `--color-cut-fill-hover` | `editor.cutFillHover` | `rgba(248, 113, 113, 0.36)` | :56 | `rgba(220, 38, 38, 0.30)` | :104 / :148 |
| `--color-cut-border` | `editor.cutBorder` | `#f87171` | :57 | `#dc2626` | :105 / :149 |
| `--color-cut-stripe` | `editor.cutStripe` | `rgba(248, 113, 113, 0.13)` | :58 | `rgba(220, 38, 38, 0.12)` | :106 / :150 |
| `--color-track` | `editor.track` | `rgba(255, 255, 255, 0.1)` | :59 | `rgba(0, 0, 0, 0.08)` | :107 / :151 |
| `--color-accent` | `accent.base` | `#3b82f6` | :64 | `#3b82f6` | :108 / :152 |
| `--color-accent-hover` | `accent.hover` | `color-mix(in oklab, var(--color-accent) 88%, black)` (fallback `#2f6fd0`) | :65-66 | same | :109-110 / :153-154 |
| `--color-on-accent` | `accent.on` | `#ffffff` | :67 | `#ffffff` | :111 / :155 |

Note the light block inside the media query has inconsistent indentation on `--color-accent-hover` (:110) — cosmetic only.

Fixed (non-variable) semantic colors, `colors.status` (`theme.ts:192-198`): `success #22c55e`, `info #3b82f6`, `warning #f59e0b`, `danger #ef4444`, `neutral var(--color-status-neutral)`.

`statusConfig` (`theme.ts:207-215`) — label + colour per session status:

| status | label | color |
|---|---|---|
| pending | "Pending" | status.neutral |
| active | "Recording" | status.success |
| paused | "Paused" | status.warning |
| stopped | "Processing" | status.info |
| compiling | "Compiling" | status.info |
| complete | "Complete" | status.success |
| failed | "Failed" | status.danger |

#### Accent override — `setAccentColor(accent, on?)` (`theme.ts:228-238`)
Sets/removes inline `--color-accent` and `--color-on-accent` on `document.documentElement` (`null` removes → default `#3b82f6`/`#ffffff`). Hover is always derived via the `color-mix` 88%-toward-black in OKLab. Desktop callers:
- Linux: `setAccentColor(appearance.accent, accentForeground(appearance.accent))` (`clients/desktop/src/linuxChrome.ts:574`) — accent is the GNOME session accent `#rrggbb` from GSettings (or null).
- `accentForeground(hex)` (`linuxChrome.ts:320-324`): WCAG relative luminance (`:304-312`: sRGB channel `c<=0.04045 ? c/12.92 : ((c+0.055)/1.055)^2.4`, `L = 0.2126R + 0.7152G + 0.0722B`); returns `#000000` if `L > 0.45`, else `#ffffff` (also `#ffffff` for non-6-digit input).
- Other `setAccentColor` call sites in desktop: `App.tsx` imports it (see App section) — the per-program brand accent from ProgramPanel/programLink is applied there.

### 1.5 Linux-only color variables (`linuxChrome.ts:132-163`)

Keyed on `html.os-linux[data-theme=…]` (specificity 0,2,1 to beat `:root[data-theme="light"]`).

| variable | dark (`:133-153`) | light (`:154-163`) |
|---|---|---|
| `--color-headerbar-control` | `rgba(255, 255, 255, 0.1)` | `rgba(0, 0, 0, 0.08)` |
| `--color-headerbar-control-hover` | `rgba(255, 255, 255, 0.18)` | `rgba(0, 0, 0, 0.16)` |
| `--color-popover-bg` | `#383838` | `#f7f7f7` |
| `--color-popover-bg-top` | `#3d3d3d` | `#ffffff` |
| `--color-popover-border` | `rgba(255, 255, 255, 0.14)` | `rgba(0, 0, 0, 0.12)` |
| `--color-popover-hover` | `rgba(255, 255, 255, 0.1)` | `rgba(0, 0, 0, 0.08)` |
| `--color-popover-separator` | `rgba(255, 255, 255, 0.15)` | `rgba(0, 0, 0, 0.12)` |
| `--color-window-border` | `rgba(255, 255, 255, 0.18)` | `rgba(0, 0, 0, 0.18)` |

Plus `--lookout-window-radius: 12px; --lookout-window-margin: 40px;` on `html.os-linux.lookout-csd` (`:191-194`), radius `0px` when `.lookout-snapped` (`:198-200`), and `--lookout-blur-plate: color-mix(in srgb, var(--color-bg-body, #242424) 80%, transparent)` (dark, `:277-279`) / `color-mix(in srgb, var(--color-bg-body, #fafafa) 80%, transparent)` (light, `:280-282`).

### 1.6 Theme detection (who decides light/dark)

1. **Before first paint (Linux only)**: inline script `index.html:7-37` adds `os-linux` to `<html>`, adds `lookout-csd` unless `window.__LOOKOUT_SHELL_DRAWS_FRAME__ === true` (planted by native `js_init_script` in lib.rs), and sets `data-theme` from `matchMedia('(prefers-color-scheme: dark)')`.
2. **First-paint backgrounds** (`index.html:85-89`): `html { background: #242424 }`, `html[data-theme="light"] { #fafafa }`, `@media (prefers-color-scheme: light) { html:not([data-theme="dark"]) { #fafafa } }`. These duplicate theme.ts `--color-bg-body`.
3. **After mount** (`clients/desktop/src/App.tsx:884-915`): 
   - Linux: `matchMedia("(prefers-color-scheme: dark)")` → sets `data-theme` to `dark`/`light` and calls `getCurrentWindow().setTheme(theme)`; re-applies on media `change`.
   - macOS/Windows: `getCurrentWindow().theme()` → sets `data-theme` (or removes if null); (subscription to Tauri theme changes continues after line 915 — see App section).
4. CSS resolves via the `:root` / media / `[data-theme]` blocks above.
5. ProgramPanel reads the effective theme as `data-theme` attribute else media query (`clients/desktop/src/components/ProgramPanel.tsx:39-47`).

### 1.7 Shadows, borders, blur, backdrops

| use | value | ref |
|---|---|---|
| Overlay (modal) panel shadow | `0 24px 64px rgba(0,0,0,0.45)` | `clients/react/src/ui/Overlay.tsx:101` |
| Overlay panel border | `1px solid var(--color-border-default)`, radius 14 | `Overlay.tsx:99-100` |
| Badge overlay ring | `0 0 0 1px rgba(0,0,0,0.1)` | `Badge.tsx:27` |
| Linux CSD window (focused) — first paint (dark) | `0 0 0 1px rgba(255, 255, 255, 0.18), 0 2px 8px rgba(0, 0, 0, 0.1), 0 8px 28px rgba(0, 0, 0, 0.14)` | `index.html:105-108` |
| Linux CSD window first paint (light) | `0 0 0 1px rgba(0, 0, 0, 0.18), 0 2px 8px rgba(0, 0, 0, 0.08), 0 8px 28px rgba(0, 0, 0, 0.1)` | `index.html:112-115` |
| Linux CSD window (focused) — runtime | `0 0 0 1px var(--color-window-border), 0 2px 8px rgba(0, 0, 0, 0.1), 0 8px 28px rgba(0, 0, 0, 0.14)` | `linuxChrome.ts:218-221` |
| Linux CSD window (unfocused, `.lookout-backdrop`) | `0 0 0 1px var(--color-window-border), 0 1px 4px rgba(0, 0, 0, 0.06), 0 4px 16px rgba(0, 0, 0, 0.09)` | `linuxChrome.ts:226-231` |
| Linux snapped | `inset: 0; border-radius: 0; box-shadow: none` | `linuxChrome.ts:235-239` |
| Focus ring (editor strip / iconbtn) | `0 0 0 2px var(--color-bg-body), 0 0 0 4px var(--color-accent)` | `editorStyles.ts:22,59` |
| Focus ring (header controls/actions) | `outline: 2px solid var(--color-accent); outline-offset: 1px` | `HeaderBar.tsx:49-52,81-84` |
| Modal dim fill (desktop `backdropFill`) | Windows: `linear-gradient(to bottom, transparent 0%, var(--color-modal-backdrop) 100%)`; macOS/Linux: `var(--color-modal-backdrop)` | `clients/desktop/src/backdrop.ts:16-18` |
| Linux overlay clipping | `html.os-linux.lookout-csd [data-lookout-overlay] { inset: var(--lookout-window-margin) !important; border-radius: var(--lookout-window-radius); overflow: hidden }`; snapped → `inset: 0 !important; border-radius: 0` | `linuxChrome.ts:246-254` |
| Linux header bar backdrop dim | `.lookout-headerbar` opacity `0.55` when `.lookout-backdrop` | `linuxChrome.ts:257-262` |
| Linux background blur plate | `html.os-linux.lookout-blur { background: var(--lookout-blur-plate) }`, body transparent; with CSD the plate moves to `#root` | `linuxChrome.ts:277-298` |
| Tray (Windows) background | `rgba(26, 26, 26, 0.93)`, radius 0; macOS transparent with `cornerRadius` | `TrayApp.tsx:167-173` |
| Tray transparency override | `:root, html, body, #root { background: transparent !important; background-color: transparent !important }`, `--color-bg-body/--color-bg-panel: transparent !important` | `TrayApp.tsx:150-161` |

Native vibrancy/mica/liquid-glass (window-level, not CSS) is owned by the Windows section (tauri.conf.json/lib.rs, `tauri-plugin-liquid-glass-api`). The theme comment notes that on macOS/Windows the window usually wears vibrancy/mica and `--color-bg-body` only shows when those materials drop out (`theme.ts:8-19`).

Scrollbars: `::-webkit-scrollbar { width: 0px; background: transparent; }` on all platforms (`index.html:67-70`) — scrollbars are invisible everywhere.

Selection colour: no `::selection` rule exists (browser default), and body is `user-select: none` anyway.

### 1.8 Global CSS rules (verbatim intent)

`clients/desktop/index.html`:
- `:60` `*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }`
- `:61` `html, body, #root { height: 100%; overflow: hidden; }`
- `:62` body: font stack above; `background: var(--color-bg-body, transparent)`; `color: var(--color-text-primary, #e5e5e5)`; no select; `cursor: default`; `overscroll-behavior: none`.
- `:63` `@keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }`
- `:64` `@keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }`
- `:98-109` `html.os-linux.lookout-csd { background: transparent }`, `#root { position: fixed; inset: 40px; height: auto; background: #242424; border-radius: 12px; box-shadow: … }` (light variant `:110-116`). Superseded at runtime by `linuxChrome.ts:206-223` (which also adds `overflow: hidden` and the box-shadow transition).
- `:132` hidden `#debug` panel: `position:fixed;bottom:0;left:0;right:0;background:#111;color:#0f0;font-family:monospace;font-size:11px;padding:0 8px 8px;max-height:40vh;overflow:auto;z-index:99999;border-top:1px solid #333;` toggled with the backtick key (`` ` ``) (`:147-155`).

Linux cursor overrides (`linuxChrome.ts:173-183`): `button, a, [role="button"], [style*="cursor: pointer"]` → `cursor: default !important`; `input, textarea, [contenteditable="true"]` → `cursor: text !important`.

### 1.9 Z-indices

No z-index scale token. Values used by primitives/globals: Overlay backdrop `9999` (`Overlay.tsx:72`); debug panel `99999` (`index.html:132`); Button label `1` over squircle bg (`Button.tsx:105`); VideoPlayer error `2` (`VideoPlayer.css:40`). NumberFlow: non-number children `z-index:5`.

### 1.10 Shared primitives (`clients/react/src/ui/*`)

#### Button (`clients/react/src/ui/Button.tsx`)
Props: `variant` (`primary` default | `success` | `danger` | `warning` | `secondary` | `ghost`), `size` (`sm` | `md` default | `lg`), `loading`, `fullWidth`, plus native button attrs. `style.background/border/borderRadius/color` are pulled out and applied to the inner squircle layer (`:43`).

Structure:
```
motion.button  (position:relative; display:inline-flex; center/center; padding:0;
               background:transparent; border:1px solid transparent; fontWeight:600;
               borderRadius: style.borderRadius ?? 8; width: fullWidth?100%:auto;
               cursor: disabled? not-allowed : pointer; opacity: disabled? 0.6 : 1;
               color: style.color ?? variant.color)                       :62-83
 ├─ Squircle(cornerRadius = borderRadius ?? 8, cornerSmoothing 0.7, asChild) :85
 │   └─ motion.div  position:absolute; inset:-1; background/border per variant state;
 │                  CSS transition "opacity 0.15s, background 0.15s, border-color 0.15s" :86-100
 └─ span  position:relative; zIndex:1; inline-flex; center; gap:8; width:100%; + size padding/fontSize :102-116
     ├─ Spinner size sm (16px), color "#000" if warning else "#fff"  (only when loading) :114
     └─ children
```
Squircle: `@squircle-js/react` measures the element with a ResizeObserver and applies `clip-path: path('…')` from `figma-squircle` `getSvgPath` with smoothing 0.7 — the inner background AND its 1px border are clipped to the squircle shape.

Variant colours (`:15-22`):

| variant | idle background | text color | idle border | hover background (`:46-57`) | hover border (`:58-59`) |
|---|---|---|---|---|---|
| primary | `var(--color-accent)` | `var(--color-on-accent)` | `1px solid transparent` | `var(--color-accent-hover)` | same as idle |
| success | `#22c55e` | `#fff` | `1px solid transparent` | `#22c55e` (unchanged) | same |
| danger | `#ef4444` | `#fff` | `1px solid transparent` | `#ef4444` | same |
| warning | `#f59e0b` | `#000` | `1px solid transparent` | `#f59e0b` | same |
| secondary | `transparent` | `var(--color-text-secondary)` | `1px solid var(--color-border-hover)` | `var(--color-bg-surface)` | same |
| ghost | `transparent` | `var(--color-text-secondary)` | `1px solid transparent` | `var(--color-bg-selected)` | `1px solid transparent` |
If caller passes `style.background`/`style.border`, hover uses the same caller value (no hover change).

Sizes (`:24-28`) — padding on the inner span: `sm` `6px 12px` / 12px; `md` `8px 16px` / 13px; `lg` `12px 24px` / 15px (note 15 is not in the fontSize scale). Since outer has 1px transparent border, total height ≈ text line + padding×2 + 2.

States: idle → `hover` variant on pointer hover → `active` variant (scale **0.96**) while pressed (`whileTap`). Disabled or loading: no hover/tap variants, opacity 0.6, `not-allowed` cursor. All variant changes (scale, background, border) are animated by motion with `{ type: "spring", stiffness: 1500, damping: 60 }` (mass default 1.0) (`:92`). Only the inner background layer scales; the label does not.

#### Card (`clients/react/src/ui/Card.tsx`)
Content box (`:15-27`): `background var(--color-bg-surface)`, `border 1px solid var(--color-border-default)`, `borderRadius 10`, `overflow hidden`, `padding` prop, `height 100%`, flex column. If `onClick`: wrapped in `motion.div role=button tabIndex=0`, `cursor:pointer; display:block; height:100%`, `whileTap={{ scale: 0.99 }}`, spring `stiffness 1500, damping 60` (`:43-58`); caller `style` goes on the wrapper instead of the content box. No hover state.

#### Badge (`clients/react/src/ui/Badge.tsx`)
`<span>` pill: sizes `sm` → `fontSize 10` (`fontSize.xs - 1`), padding `2px 8px`; `md` → 12 / `4px 12px`; `lg` → 13 / `6px 16px` (`:14-18`). `fontWeight 600`, color `#fff`, `borderRadius 999`, background = statusConfig colour (unknown status: label = raw status, colour `var(--color-status-neutral)`). `variant="overlay"` (default) adds `boxShadow: 0 0 0 1px rgba(0,0,0,0.1)`.

#### Spinner (`clients/react/src/ui/Spinner.tsx`)
SVG `viewBox 0 0 24 24`, rendered size `sm 16 / md 24 / lg 40` (`:9`). Two circles `cx 12 cy 12 r 10 strokeWidth 2`: base ring `var(--color-spinner-base)`; arc `color ?? var(--color-spinner-track)`, `strokeLinecap round`, `strokeDasharray 62.83`, `strokeDashoffset 47.12` → visible arc = 15.71 units = **90°** (quarter circle). Animation `spin 1s linear infinite`; `flexShrink 0`. Arc starts at 3 o'clock (SVG default; no -90° rotation).

#### ProgressRing (`clients/react/src/ui/ProgressRing.tsx`)
Props: `progress` 0–1 (clamped), `size` 72, `strokeWidth` 5, `showPercent` false, `color`. Wrapper `relative`, `size×size`, `role=progressbar`. SVG rotated `-90deg` (starts at 12 o'clock). Radius `(size − strokeWidth)/2`; track stroke `var(--color-spinner-base)`; progress stroke `color ?? var(--color-accent)`, round cap, dasharray = circumference, dashoffset = `circumference × (1 − p)`, CSS `transition: stroke-dashoffset 0.25s linear` (`:60`). Centre label (if `showPercent`): absolute inset 0 centred, `fontSize 13`, `fontWeight 600`, `color var(--color-text-secondary)`, tabular nums; `<NumberFlow value={min(99, floor(p*100))} />%` — the label **never shows 100**.

#### MinutesFlow (`clients/react/src/ui/MinutesFlow.tsx`)
`safe = max(0, round(minutes))`. `<span style={{color, fontVariantNumeric:"tabular-nums"}}>`: if `< 60` → `<NumberFlow value={m}/>m`; else `<NumberFlow value={h}/>h <NumberFlow value={m}/>m` (space after `h`). Default NumberFlow timing (1.8 below).

#### Overlay (`clients/react/src/ui/Overlay.tsx`)
Portal to `document.body`. Backdrop div `data-lookout-overlay`: `position fixed; inset 0; background var(--color-modal-backdrop); flex centre; padding 20; zIndex 9999` (`:59-73`); `pointerdown` on the backdrop itself → `onDismiss`. Panel `motion.div role=dialog aria-modal tabIndex=-1`: `width` default `"min(1100px, 94vw)"`, optional `height`, `maxWidth/maxHeight 100%`, `minHeight 0`, flex column, `background var(--color-bg-panel)`, `border 1px solid var(--color-border-default)`, `borderRadius 14`, shadow `0 24px 64px rgba(0,0,0,0.45)`, `outline none`, `overflow hidden` (`:90-104`).
Enter: `initial {opacity:0, y:10, scale:0.99}` → `animate {opacity:1, y:0, scale:1}`; `y` spring `stiffness 420, damping 34, mass 0.7`; `opacity` `{duration: 0.16, ease: "easeOut"}`; `scale` has no explicit transition → motion default for scale: spring `stiffness 550, damping 30, restSpeed 10` (see 1.11). Backdrop has no animation (appears instantly). **No exit animation** (no AnimatePresence) — unmount is instant. On mount focuses panel and sets `body.style.overflow = hidden` (restored on unmount). Escape → `onDismiss` (window keydown).

#### ErrorDisplay (`clients/react/src/ui/ErrorDisplay.tsx`)
- `page` (`:22-33`): column centred, padding 24, text-align centre. `h2` title (default "Error") `fontSize 20, fontWeight 700, color #ef4444, marginBottom 8`. `<pre>` `fontSize 11, monospace, pre-wrap, break-all, maxWidth 400, maxHeight 150, overflowY auto, color var(--color-text-error), marginBottom 16`. Row gap 8: `Button primary md` with `action.label`, `Button secondary md` "Copy Error".
- `inline` (`:35-39`): `<pre>` 11px monospace, error colour, maxHeight 150, scroll.
- `banner` (default, `:42-101`): row `padding 12px 16px; marginBottom 12; background rgba(239,68,68,0.15); border 1px solid #ef4444; borderRadius 8; color var(--color-text-error); fontSize 13; align flex-start; gap 8`. Text column `flex 1; fontSize 11; pre-wrap; break-all; maxHeight 120; overflowY auto`: optional `<strong>` title (block, marginBottom 4), monospace error, optional reassurance at `opacity 0.8` preceded by two newlines. Buttons: action (label) and "Copy": transparent, `color error, fontSize 11, lineHeight 1, padding 2px 8px, radius 6, nowrap`, with an absolutely-positioned 1px `var(--color-text-error)` outline layer that scales to **0.96** on tap (spring 1500/60). Dismiss "×" (`&times;`): `fontSize 16, lineHeight 1, padding 0`, tap layer `inset -2, radius 50%`, transparent.

#### PageContainer (`clients/react/src/ui/PageContainer.tsx`)
`maxWidth` 640 default, `margin 0 auto`, `padding 16`; `centered` adds flex column centre/centre, `minHeight 100%`, `flex 1`.

#### Skeleton (`clients/react/src/ui/Skeleton.tsx`)
`width` default 100%, `height` default 20 (unless `aspectRatio`), `borderRadius` default 8. `backgroundColor var(--color-skeleton-bg)`, `backgroundImage linear-gradient(90deg, transparent 0%, var(--color-skeleton-shimmer) 50%, transparent 100%)`, `backgroundSize 200% 100%`, `animation shimmer 1.5s ease-in-out infinite`. Presets: `GallerySkeleton` (`:28-48`), `SessionDetailSkeleton` (`:50-62`), `RecordPageSkeleton` (`:64-80`: maxWidth 480, padding 16; header row two blocks 80×32 & 100×32 radius 6; 16/9 block radius 10; three 48-high radius-8 rows gap 8; final 48-high radius-10 block marginTop 16).

#### ErrorBoundary fallback (desktop, `clients/desktop/src/main.tsx:93-121`)
Container `padding 24, monospace, transparent bg, color var(--color-text-primary, #e5e5e5), minHeight 100%`. Card `maxWidth 500, margin 0 auto, border 1px solid #f44, radius 8, padding 20`. `h2` "Something went wrong" `#f44, margin 0 0 12px 0, fontSize 18`. `<pre>` `#faa, 12px, maxHeight 300, marginBottom 16`. Buttons (gap 8) `padding 8px 16px; background #333; border 1px solid #555; radius 4; fontSize 13`: "Copy Error Report"/"Copied!" (color `#0f0`; reverts after 2000 ms, `:90`), "Try Again" (color `#ff0`). Footer `p` `#888, 11px, marginTop 12`: "Press ` to open the debug log. Copy and send to the developer."

### 1.11 Animation libraries — exact defaults

#### motion (framer-motion) 14.1.0 — `node_modules/motion`, `node_modules/framer-motion`, `node_modules/motion-dom` (all 14.1.0)
Spring defaults (`node_modules/motion-dom/dist/es/animation/generators/spring.mjs:6-29`): `stiffness 100, damping 10, mass 1.0`, duration-based defaults `duration 800ms, bounce 0.3, visualDuration 0.3s`; rest thresholds `restSpeed {granular 0.01, default 2}`, `restDelta {granular 0.005, default 0.5}` (granular when |delta| < 5, `:252-262`); `minDuration 0.01s, maxDuration 10s, minDamping 0.05`. Named ease `"easeOut"` = `cubic-bezier(0, 0, 0.58, 1)`. Per-value default transition when a transition object omits a key (`node_modules/motion-dom/dist/es/animation/utils/default-transitions.mjs:3-38`): `scale*` → critically-damped spring `{stiffness 550, damping target===0 ? 2*sqrt(550) : 30, restSpeed 10}`; other transform keys (x, y, rotate) → `{spring, stiffness 500, damping 25, restSpeed 10}`; non-transform values (opacity, colors) → `{keyframes, ease [0.25, 0.1, 0.35, 1], duration 0.3}`; >2 keyframes → `{keyframes, duration 0.8}`. So Overlay's `scale` 0.99→1 uses stiffness 550 / damping 30 / mass 1.
Springs used by primitives (convert to ζ = c / (2√(k·m))):
- Press spring `stiffness 1500, damping 60, mass 1` → ζ ≈ 0.775, ω₀ ≈ 38.7 rad/s; settles ≈ 120–150 ms. Used by Button, Card, ErrorDisplay buttons.
- Overlay `y` spring `stiffness 420, damping 34, mass 0.7` → ζ ≈ 0.99 (≈critically damped), ω₀ ≈ 24.5 rad/s.

#### vaul 1.1.2 — `node_modules/vaul/dist/index.mjs`
Used by desktop `Sheet.tsx`, `TipDrawer.tsx`, `UpdateDrawer.tsx`, `ProgramPanel.tsx`.
- `TRANSITIONS = { DURATION: 0.5, EASE: [0.32, 0.72, 0, 1] }` (`:437-445`) → every vaul motion is **500 ms `cubic-bezier(0.32, 0.72, 0, 1)`**.
- Constants (`:446-452`): `VELOCITY_THRESHOLD 0.4` (px/ms), `CLOSE_THRESHOLD 0.25` (fraction of drawer height), `SCROLL_LOCK_TIMEOUT 100` ms, `BORDER_RADIUS 8`, `NESTED_DISPLACEMENT 16`, `WINDOW_TOP_OFFSET 26`, `DRAG_CLASS 'vaul-dragging'`; `KEYBOARD_BUFFER 24` (`:91`); `LONG_HANDLE_PRESS_TIMEOUT 250`, `DOUBLE_TAP_TIMEOUT 120` (`:1535-1536`).
- Root defaults (`:879`): `shouldScaleBackground=false`, `setBackgroundColorOnScale=true`, `closeThreshold=0.25`, `scrollLockTimeout=100`, `dismissible=true`, `handleOnly=false`, `modal=true`, `direction='bottom'`, `defaultOpen=false`, `disablePreventScroll=true`, `snapToSequentialPoint=false`, `repositionInputs=true`, `autoFocus=false`, `noBodyStyles=false`.
- Injected CSS (`:62`, single `__insertCSS` string):
  - `[data-vaul-drawer]{touch-action:none;will-change:transform;transition:transform .5s cubic-bezier(.32, .72, 0, 1);animation-duration:.5s;animation-timing-function:cubic-bezier(0.32,0.72,0,1)}`
  - no-snap-points, `data-state=open` → `animation-name: slideFrom{Bottom|Top|Left|Right}`; `data-state=closed` → `slideTo…`.
  - `@keyframes slideFromBottom{from{transform:translate3d(0,var(--initial-transform,100%),0)}to{transform:translate3d(0,0,0)}}`; `slideToBottom{to{transform:translate3d(0,var(--initial-transform,100%),0)}}`; Top uses `calc(var(--initial-transform,100%) * -1)`; Left/Right analogous on X.
  - Overlay: `[data-vaul-overlay][data-vaul-snap-points=false]{animation-duration:.5s;animation-timing-function:cubic-bezier(0.32,0.72,0,1)}`, open → `fadeIn`, closed → `fadeOut`; `@keyframes fadeIn{from{opacity:0}to{opacity:1}}`, `@keyframes fadeOut{to{opacity:0}}`.
  - `[data-vaul-animate=false]{animation:none!important}`.
  - Drawer `::after` extension: `content:'';position:absolute;background:inherit` covering 200% beyond the open edge (bottom drawer: `top:100%; height:200%`) so over-drag never reveals a gap (not when `data-vaul-custom-container=true`).
  - Handle `[data-vaul-handle]{display:block;position:relative;opacity:.7;background:#e2e2e4;margin:auto;height:5px;width:32px;border-radius:1rem}`, `:hover/:active{opacity:1}`; hit area `max(100%,2.75rem)` square.
  - `@media (hover:hover) and (pointer:fine){[data-vaul-drawer]{user-select:none}}`.
- Drag (`:1060-1104`): during drag `transition: none` on drawer and overlay; drawer `translate3d(0, d, 0)` following the pointer 1:1 toward close; dragging the opposite way (past open) is dampened: `dampenValue(v) = 8 * (Math.log(v + 1) - 2)` (`:413-415`), translate `min(-dampened, 0)`. Overlay opacity follows `1 − percentageDragged` (percentage = dragged / drawer height).
- Release (`:1225-1275`): `velocity = |distMoved| / timeTaken` (px/ms). Moved toward open → reset. `velocity > 0.4` → close. Else if `|swipe| ≥ visibleHeight × closeThreshold (0.25)` → close. Else reset. Reset (`:1185-1214`): drawer `translate3d(0,0,0)` with `transform 0.5s cubic-bezier(0.32,0.72,0,1)`, overlay opacity `1` with `opacity 0.5s …`. Velocity > 0.05 sets `justReleased` for 200 ms (suppresses focus).
- Close completes and unmounts after `TRANSITIONS.DURATION * 1000` = **500 ms** (`:752`, `:891`, `:1183`).
- Background scaling (unused unless a caller passes `shouldScaleBackground`): wrapper `scale((innerWidth − 26)/innerWidth) translate3d(0, calc(env(safe-area-inset-top) + 14px), 0)`, radius 8, origin top.

#### NumberFlow — `@number-flow/react` 0.6.2 wrapping `number-flow` 0.6.2 (`node_modules/number-flow/dist/lite.mjs:129-141`)
Defaults (no desktop/react caller overrides them — verify per component):
- `transformTiming: { duration: 900, easing: "linear(0,.005,.019,.039,.066,.096,.129,.165,.202,.24,.278,.316,.354,.39,.426,.461,.494,.526,.557,.586,.614,.64,.665,.689,.711,.731,.751,.769,.786,.802,.817,.831,.844,.856,.867,.877,.887,.896,.904,.912,.919,.925,.931,.937,.942,.947,.951,.955,.959,.962,.965,.968,.971,.973,.976,.978,.98,.981,.983,.984,.986,.987,.988,.989,.99,.991,.992,.992,.993,.994,.994,.995,.995,.996,.996,.9963,.9967,.9969,.9972,.9975,.9977,.9979,.9981,.9982,.9984,.9985,.9987,.9988,.9989,1)" }` (a spring approximated as 100-ish evenly spaced samples, no overshoot).
- `spinTiming: undefined` → falls back to `transformTiming` (`lite.mjs:334`).
- `opacityTiming: { duration: 450, easing: "ease-out" }`.
- `animated: true`, `respectMotionPreference: true` (disabled under `prefers-reduced-motion: reduce`), `trend: (oldValue, newValue) => Math.sign(newValue − oldValue)` (digits roll up when increasing, down when decreasing), `plugins: undefined`, `digits: undefined`.
- Rendering (`node_modules/number-flow/dist/ssr-DvIINv8w.mjs`, shadow-DOM styles): host `display:inline-block; direction:ltr; white-space:nowrap; isolation:isolate; line-height:1`. Each digit is a vertical strip of 0–9; current digit translated by `translateY(clamp(-100%, offset*100%, 100%))` (shortest path, wrapping). Edge mask: vertical fade `--number-flow-mask-height` default `0.25em` (rounded to whole px, half applied as padding), horizontal fade `--number-flow-mask-width` default `0.5em`; corners use radial gradients `#000 0, transparent 71%`. Width changes animate via `scaleX` on `.number` with counter-scale on `.number__inner`. Entering/exiting digits and symbols fade with `opacityTiming`; symbols use `mix-blend-mode: plus-lighter`.

#### @squircle-js/react 1.3.0
`Squircle asChild` → ResizeObserver + `clip-path: path('…')` from `figma-squircle` (`getSvgPath({width,height,cornerRadius,cornerSmoothing})`). Used only by Button (smoothing 0.7). Within desktop components also check for direct `Squircle` imports (component sections).

### 1.12 Other CSS (VideoPlayer, editor)

`clients/react/src/components/VideoPlayer.css`: hides `.media-button--mute` and `.media-slider--volume` (`:1-4`); hides PiP on `platform-windows|mac|linux` (`:7-11`); hides fullscreen on `platform-linux` (`:14-16`); `--media-border-radius: 8px; --media-video-border-radius: 8px` (`:18-25`). Error overlay `.lookout-video-player__error` (`:27-41`): absolute inset 0, column centre, gap 8, padding 24, `background rgba(11, 13, 18, 0.92)`, `color #f4f5f7`, radius 8, z 2; `h3` 18px/600 (`:43-47`); `p` maxWidth 480, 14px, line-height 1.5, `#c8ccd4` (`:49-55`); `__error-note` 13px `#9aa0aa` (`:57-60`); links `#6ea8ff` underlined (`:62-65`).

`clients/react/src/components/editorStyles.ts` tokens: `EASE_OUT_QUART = cubic-bezier(0.25, 1, 0.5, 1)` (`:8`); classes `.lk-ed-strip`, `.lk-ed-region` (hover → `var(--color-cut-fill-hover)`), `.lk-ed-grip` (hover handle → `scaleX(1.6)`; 12px hit / 3px bar per comment `:31-32`), `.lk-ed-playhead` (`scaleX(1.25)` hover / `1.1` active), `.lk-ed-iconbtn` (inline-flex centre, transparent, `1px solid var(--color-border-default)`, hover bg `var(--color-bg-surface)` + border `var(--color-border-hover)`, active `scale(0.94)`), `.lk-ed-fade-in`; reduced-motion block `:71-79` collapses durations to 1ms and removes hover transforms. Full timings in the animation table.

### 1.13 Linux CSD state classes on `<html>` (summary)

| class | set by | effect |
|---|---|---|
| `os-linux` | `index.html:20`; also `linuxChrome.ts:544` (in `applyLinuxChrome`, `:539`) (`classList.add("os-linux")`) | enables Linux-only rules |
| `lookout-csd` | `index.html:30` unless shell draws frame; `linuxChrome.ts:553` add / `:559` remove | transparent 40px frame, rounded #root, shadow |
| `lookout-snapped` | `useWindowFrameState` in linuxChrome.ts | radius 0, inset 0, no shadow |
| `lookout-backdrop` | `useBackdropState` (`linuxChrome.ts:371-410`), from Tauri window focus | dimmer shadow, header bar opacity 0.55 |
| `lookout-blur` | `useBackgroundBlur` (`linuxChrome.ts:469`) only after compositor confirms `ext-background-effect-v1` blur; re-sync debounced 120 ms (`setTimeout(…, 120)`, `:492`) | 80% translucent plate, fades in 200 ms |
| `data-theme` | index.html (Linux), App.tsx | light/dark |
| body `os-windows` | `index.html:134-139` (Windows **and** Linux UAs) | `transform: rotate(0.001deg)` |

## 2. Windows

All paths are relative to the repo root. `ST` = `clients/desktop/src-tauri`, `DS` = `clients/desktop/src`.

### 2.0 How a webview decides what to render

Every Tauri webview loads the same bundle (`clients/desktop/index.html` → `DS/main.tsx` → `<App/>`). `App()` branches on `window.location.hash` (`DS/App.tsx:166-179`):

| Test (in order) | Renders | Window |
|---|---|---|
| `window.location.hash.includes("tray")` | `<TrayApp/>` (`DS/components/TrayApp.tsx`) | `tray` |
| `hash.match(/^#\/?editor\?token=([0-9a-fA-F]{64})/)` | `<EditorWindow token={m[1]}/>` | `editor-<token[0..8]>` |
| otherwise | `<MainWindowApp/>` (permission gates, vibrancy, deep links, hash router) | `main` |

`main.tsx` wraps everything in an `ErrorBoundary` (`DS/main.tsx:74-125`) and globally disables the context menu: `document.addEventListener("contextmenu", e => e.preventDefault())` (`DS/main.tsx:128`).

ErrorBoundary crash screen (all windows), exact inline styles (`DS/main.tsx:94-121`):
- outer: `padding:24, fontFamily:"monospace", background:"transparent", color:"var(--color-text-primary, #e5e5e5)", minHeight:"100%"`
- card: `maxWidth:500, margin:"0 auto", border:"1px solid #f44", borderRadius:8, padding:20`
- h2 "Something went wrong": `color:#f44, margin:"0 0 12px 0", fontSize:18`
- `<pre>` stack: `color:#faa, fontSize:12, whiteSpace:pre-wrap, wordBreak:break-all, maxHeight:300, overflowY:auto, marginBottom:16`
- buttons row `display:flex, gap:8`; both buttons `padding:"8px 16px", background:#333, border:"1px solid #555", borderRadius:4, cursor:pointer, fontSize:13`; first `color:#0f0` text "Copy Error Report" → "Copied!" for 2000 ms after copy (`:90`); second `color:#ff0` text "Try Again" (resets state).
- footer p: `color:#888, fontSize:11, marginTop:12` — "Press ` to open the debug log. Copy and send to the developer."

---

### 2.1 `main` window

Declared in `ST/tauri.conf.json:16-33` (no explicit `label` ⇒ Tauri default label `"main"`).

| Property | Value | Source |
|---|---|---|
| title | `"Lookout"` (then JS sets `Lookout v<version>` on macOS/Windows, `"Lookout"` on Linux) | `ST/tauri.conf.json:18`, `DS/App.tsx:957-963` |
| width / minWidth / maxWidth | 480 / 480 / 480 | `ST/tauri.conf.json:19-21` |
| height / minHeight / maxHeight | 640 / 640 / 640 | `ST/tauri.conf.json:22-24` |
| resizable | false | `:25` |
| center | true | `:26` |
| fullscreen | false | `:27` |
| maximizable | false (also `set_maximizable(false)`, `set_fullscreen(false)` at runtime) | `:28`, `ST/src/lib.rs:1340-1341` |
| transparent | true | `:29` |
| titleBarStyle | `"Overlay"` (macOS: content under titlebar, traffic lights drawn over webview) | `:30` |
| hiddenTitle | true (macOS title text hidden) | `:31` |
| decorations | default (true) on macOS/Windows; **set to false at runtime on Linux** | `ST/src/lib.rs:1349-1350` |
| always-on-top / skip-taskbar | defaults (false) | — |
| visible on start | default (true) | — |
| `macOSPrivateApi` | true (needed for transparent window) | `ST/tauri.conf.json:15` |

**Linux runtime sizing** (`ST/src/lib.rs:1343-1426`): decorations off; size is recomputed from the config (not read back from GTK) as `content + 2*margin` where `margin = 0.0` if `desktop_appearance::shell_draws_window_frame()` else `40.0` (`:1393-1398`) ⇒ normally **560 × 720** logical px; then `set_min_size(target)`, `set_max_size(target)`, `set_size(target)`, `center()` in that order (`:1409-1413`). The webview then draws a 480×640 rounded "visible window" inset 40px inside (see §5 Linux chrome).

**Background / material**
- macOS: JS invokes `enable_vibrancy` on mount (`DS/App.tsx:977-988`); Rust applies `NSVisualEffectMaterial::Sidebar`, state `Active`, corner radius **16.0 on macOS ≥ 26 (Tahoe), else 10.0** (`ST/src/lib.rs:402-412`). On success JS sets `html`, `body`, `#root` inline `background: transparent` (`DS/App.tsx:981-984`). Cleanup invokes `disable_vibrancy` (`clear_vibrancy`).
- Windows: same command applies **Mica** (`apply_mica(&window, None)`) (`ST/src/lib.rs:414-418`); cleanup `clear_mica`.
- Linux: vibrancy skipped (`DS/App.tsx:989-995`); `#root` painted with `--color-bg-body`, html/body transparent. Optional compositor blur (`LOOKOUT_WINDOW_BLUR=1`, `ext-background-effect-v1`) — see §5.
- First paint before theme CSS loads: `html {background:#242424}`, light `#fafafa` (`clients/desktop/index.html:83-87`).

**Window-level chrome inside the webview** (`DS/App.tsx:1370-1398`):
- macOS: a 32px-high draggable strip at top: `data-tauri-drag-region`, class `titlebar`, `height:32, flexShrink:0, width:100%, zIndex:9999, background:transparent, cursor:default, display:flex, alignItems:center, justifyContent:flex-end, paddingRight:6` holding `<UpdatePill>` (only when `onMainPage`).
- Windows: no strip; `<UpdatePill origin="bottom">` floated `position:absolute, bottom:8, left:8, zIndex:9999`.
- Linux: `<HeaderBar title subtitle appearance actions onBack/>` (47px, see HeaderBar spec) as the titlebar.

**Show / hide / close**
- Shown at launch; never hidden by the app.
- Focused by: deep links (`handle_deep_link_urls` → `set_focus`, `ST/src/lib.rs:287-289`), macOS menu "Start Timelapse" (`:1272-1274`), tray "stop"/"resume-when-source-lost"/Linux "Open Lookout" (`focus_main_window`: `unminimize`, `show`, `set_focus`, `ST/src/tray.rs:175-181`), editor window closing (`DS/components/EditorWindow.tsx:104-110`), JS `getCurrentWindow().setFocus()` after deep-link handling (`DS/App.tsx:761`, `:820`).
- On `WindowEvent::Destroyed`: `core.recorder_abort()` and `tray::hide()` (`ST/src/lib.rs:1440-1447`).
- On `RunEvent::ExitRequested` with an active capture loop: `prevent_exit`, send pause to server, then `exit(0)` (`ST/src/lib.rs:1455-1479`).
- Close button: OS close (macOS/Windows); Linux HeaderBar close → `getCurrentWindow().close()` (`DS/components/HeaderBar.tsx:156-159`).
- Single instance: second launch forwards `lookout://` args and focuses main (`ST/src/lib.rs:1066-1081`).
- Theme: `data-theme` on `<html>` from `getCurrentWindow().theme()` + `onThemeChanged` (macOS/Windows), or `matchMedia('(prefers-color-scheme: dark)')` + `setTheme(theme)` on Linux (`DS/App.tsx:884-929`).

---

### 2.2 `tray` window (tray popup; macOS + Windows only in practice)

Created lazily on first tray-icon click by `toggle_tray_window` (`ST/src/tray.rs:183-228`).

| Property | Value | Source |
|---|---|---|
| label | `"tray"` | `ST/src/tray.rs:195` |
| URL | `WebviewUrl::App("#/tray")` → `<TrayApp/>` | `:195`, `DS/App.tsx:167-170` |
| title | `"Tray"` | `:196` |
| inner size | 300 × 50 | `:197` |
| resizable | false | `:198` |
| decorations | false | `:199` |
| always_on_top | true | `:200` |
| transparent | true | `:201` |
| skip_taskbar | true | `:202` |
| visible | false at build, shown after positioning | `:203` |
| visible_on_all_workspaces | true (macOS only) | `:205-208` |

**Positioning** (`position_and_show_window`, `ST/src/tray.rs:230-283`), all in logical px using the current monitor's scale factor (fallback primary monitor):
- `x = tray.x + tray.width/2 − window.width/2` (horizontally centred under the icon)
- `y = tray.y + tray.height` (directly under the icon)
- if `y + window.height > monitor.y + monitor.height` → `y = tray.y − window.height` (flip above; Windows bottom taskbar)
- if `x + window.width > monitor.x + monitor.width` → `x = monitor.x + monitor.width − window.width`
- if `x < monitor.x` → `x = monitor.x`
- then `set_position`, `show()`, `set_focus()`; emits `"tray-opened"` (unit payload) to the window (`:189`, `:225`).

**Toggle**: click while visible → `hide()` (`:185-186`). **Hide on blur**: `WindowEvent::Focused(false)` and visible → `hide()` (`:213-222`). Any real `tray_action` (not `"ready"`) hides it (`ST/src/tray.rs:361-365`). `tray::hide()` closes it when the recording ends (`:350-352`).

**Material**: TrayApp forces `html/body/#root` `background: transparent !important` and `--color-bg-body/--color-bg-panel: transparent !important` (`DS/components/TrayApp.tsx:49-57`, `:150-161`). macOS: `setLiquidGlassEffect({ cornerRadius: isTahoe ? 16 : 10, tintColor: "#00000000", variant: GlassMaterialVariant.Sidebar })` from `tauri-plugin-liquid-glass-api` (`:70-84`), `isTahoe = macOS major ≥ 26` parsed from UA `/Mac OS X (\d+)[._](\d+)/` (`:64-67`). Windows/Linux UA: container `backgroundColor: rgba(26, 26, 26, 0.93)`, `borderRadius: 0` (`:167-168`). macOS: container `borderRadius: cornerRadius` (10 or 16), transparent bg.

On Linux the tray click event never fires (`ST/src/tray.rs:115-118`), so the popup is unreachable; Linux uses the tray menu (§5.4).

---

### 2.3 `editor-<token8>` window (timelapse editor)

Created from JS in `openEditorWindow(token)` (`DS/components/EditorWindow.tsx:47-90`) via `new WebviewWindow(label, {...})`.

| Property | Value | Source |
|---|---|---|
| label | `` `editor-${token.slice(0, 8)}` `` | `:25-27` |
| url | `` `${window.location.pathname}#/editor?token=${token}` `` → `<EditorWindow token/>` | `:64`, `DS/App.tsx:174-177` |
| title | `"Edit timelapse"`; later `` `Edit — ${session.name}` `` once fetched | `:65`, `:298-302` |
| width × height | `940 + pad` × `660 + pad` | `:66-67` |
| minWidth × minHeight | `620 + pad` × `480 + pad` | `:71-72` |
| `pad` | `WINDOW_MARGIN*2 = 80` on Linux when Lookout draws its frame, else 0 | `:62` |
| resizable | true | `:73` |
| center | true | `:74` |
| transparent | true | `:77` |
| titleBarStyle | `"overlay"` on macOS only (title **visible**, OS-drawn & centred) | `:81` |
| decorations | false on Linux only | `:85` |
| capability glob | `"editor-*"` | `ST/capabilities/default.json:6-9` |

- If a window with that label already exists → `setFocus()` + emit `lookout-editor-opened {token}` and return (`:49-55`). After creation also emits `lookout-editor-opened {token}` (`:89`). Error: `win.once("tauri://error")` logs.
- Vibrancy: invokes `enable_vibrancy` (non-Linux); on success html/body/#root transparent; on failure they get `var(--color-bg-body)` (`:259-285`).
- Linux: `useDesktopAppearance({undecorated:true})`, `useWindowFrameState()`, `useBackdropState()`, `useBackgroundBlur()` (`:250-254`); renders `<HeaderBar title={sessionName ?? "Edit timelapse"} subtitle={sessionName ? "Editing timelapse" : undefined} maximizable/>` + `<WindowResizeHandles/>` (`:396-406`).
- macOS: top drag strip `data-tauri-drag-region`, `flex:"0 0 auto", height:30, cursor:default` (`:386-391`).
- Body padding: `0 ${spacing.lg}px ${spacing.lg}px` on macOS/Linux, `spacing.lg` all sides on Windows (`:410-419`).
- **Close**: `onCloseRequested` → `preventDefault`, native `confirm()` dialog (title "Finish timelapse?", kind "warning") with message `Closing publishes this timelapse with ${removed} cut${removed===1?"":"s"} applied. This can't be undone.` if dirty & cuts > 0, else `Closing publishes this timelapse as recorded. This can't be undone.` (`:346-369`). OK → `setCuts`, `applyCuts`, emit `lookout-edited {token,status,redirectUrl,panelUrl,panelResolved}`, then `closeEditorWindow()`: focus `main` first, then `close()`, falling back to `destroy()` (`:102-120`).
- Main window, while an editor is open (`useEditorWindowOpen`, `:195-230`): listens for `lookout-editor-opened` (sets token) and `lookout-edited` (clears); polls `WebviewWindow.getByLabel` every **1000 ms** to clear when closed. Main shows `<EditorOpenPlaceholder>` and Linux header `{title:"Lookout", subtitle:"Editing in another window"}` (`DS/App.tsx:1116-1117`, `:1279`).

---

### 2.4 macOS add menu panel (native, not a Tauri window)

`show_add_menu` command (`ST/src/native_menu.rs:176-191`) → Swift `AddMenuController.show` (`ST/swift/lookout-tray/Sources/AddMenu.swift:230-298`). macOS only; elsewhere the command returns `Err("native add menu is only implemented on macOS")` and Windows/Linux use the DOM `<AddMenuPopup>` (rendered in the main window, `DS/App.tsx:1400-1412`).

- `NSPanel` subclass `AddMenuPanel` (`canBecomeKey = true`), styleMask `[.borderless, .nonactivatingPanel]`, backing `.buffered`, `isOpaque=false`, `backgroundColor=.clear`, `hasShadow=false` (shadow drawn in SwiftUI), `level=.popUpMenu`, `isReleasedWhenClosed=false`, `collectionBehavior=[.transient, .ignoresCycle]` (`AddMenu.swift:245-258`).
- Content: `NSHostingView(AddMenuView)`; panel size = `hosting.fittingSize` which includes a transparent **28pt** overshoot margin on all sides (`addMenuOvershootMargin`, `:17`).
- Frame (`:305-324`): visible menu = size − 2·28. `gap = 6`. `x = parent.x + anchor.x + anchor.width − menu.width` (right edges aligned); `y = (parent.maxY − (anchor.y + anchor.height)) − gap − menu.height` (just below the button). Clamp `x` to `[visible.minX + 8, visible.maxX − menu.width − 8]`; if `y < visible.minY + 8` → flip above: `y = (parent.maxY − anchor.y) + gap`. Final frame expanded by the 28pt margin.
- Anchor comes from the JS + button's rect (CSS px, viewport-relative = window coords because the webview fills the window under the Overlay titlebar) (`ST/src/native_menu.rs:1-6`).
- Dismissal: `windowDidResignKey` → finish(nil) (`:326-328`); Escape (keyCode 53) → nil; ↓ (125) / ↑ (126) move selection (wraps, skips separators); Return (36) / keypad Enter (76) activate (`:262-280`). Only one menu at a time; a new `show` tears the old one down without firing its callback (`:233`; Rust side: replacing the oneshot sender resolves the previous await to `None`, `native_menu.rs:117-119`).
- Result: command resolves to the chosen `id` string or `null`.
- Layout & animation: see §2.6 and B-anim.

---

### 2.5 macOS menu-bar item (native NSStatusItem, SwiftUI)

Rust `tray::show` on macOS → `native_tray::show` → Swift `lookout_tray_show` (`ST/src/tray.rs:49-53`, `ST/src/native_tray.rs:35-44`). Icon: `ST/icons/timelapse_template.png` as a template image (`native_tray.rs:38`, `Tray.swift:151-153`).

`TrayContentView` (`ST/swift/lookout-tray/Sources/Tray.swift:33-74`):
```
HStack(spacing: 4)
├─ Image(nsImage: icon).renderingMode(.template).resizable().aspectRatio(.fit).frame(17×17)   [if icon]
├─ Image(systemName: "pause.fill").font(.system(size: 9))                                    [if paused, macOS ≥ 11]
└─ Text(model.text)
     macOS ≥ 13: .font(.system(size: 13.5).monospacedDigit()).contentTransition(.numericText())
     else:       .font(.system(size: 13.5))
.padding(.horizontal, 3).fixedSize()
```
- Width is measured with a `GeometryReader` → `TrayWidthKey` preference → `TrayController.setLength(width)` sets `NSStatusItem.length` (ignored if ≤ 0) (`:53-60`, `:111-114`). Item created with `NSStatusItem.variableLength` (`:92`).
- Hosting view is `PassthroughHostingView` (`hitTest` returns nil) pinned to the status button's 4 edges, so the button keeps native click highlight (`:80-82`, `:96-104`).
- Click → callback with the status button **window frame** converted to top-left origin: `(x, screenH − y − h, w, h)` where `screenH = NSScreen.screens.first.frame.height` (`:116-127`) → Rust `toggle_tray_window` (§2.2).
- Model defaults: `text = "0m"`, `paused = false` (`:16-17`). `lookout_tray_show` sets text, `paused=false`, tooltip `"Lookout — \(s) recorded"` (`:155-158`).
- `lookout_tray_update(text, paused)`: `paused < 0` keeps current (`-1` from 1 s ticker), else sets; on macOS ≥ 13 both text and paused are set inside `withAnimation(.spring(response: 0.4, dampingFraction: 0.9))` (`:168-177`). Tooltip: paused `"Lookout — paused at \(s)"`, else `"Lookout — \(s) recorded"` (`:178-179`).
- Hide: `NSStatusBar.system.removeStatusItem` (`:129-134`).
- Time text format (Rust, `ST/crates/lookout-recorder/src/ticker.rs:38-48`): `h>0 ? "{h}:{mm}:{ss}" : "{mm}:{ss}"` (e.g. `05:07`, `1:02:03`). Ticker writes once per second only when `(text, paused)` changed (`ticker.rs:53-56`).
- Visibility follows recorder state: shown when a `RecorderState` exists whose phase is not `LoadError`/`Finished`; hidden otherwise; a `control_mode` change triggers an immediate `update` so the glyph flips without waiting for the ticker (`ST/src/lib.rs:146-183`).

### 2.6 macOS add menu SwiftUI content

`AddMenuView` (`AddMenu.swift:168-212`):
```
VStack(alignment: .leading, spacing: 1)
  for each entry:
    separator → Divider().padding(.vertical, 4).padding(.horizontal, 10)
    else      → AddMenuRow(entry, highlighted: selection == i)
                  .onHover { inside → selection = i ; leaving the selected row → selection = nil }
                  .onTapGesture { onActivate(entry.id) }
.padding(6)
.frame(minWidth: 220, maxWidth: 320, alignment: .leading)
container:
  .background(NSVisualEffectView material .menu, blendingMode .behindWindow, state .active)
  .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
  .overlay(RoundedRectangle(cornerRadius: 14, .continuous).strokeBorder(Color.primary.opacity(0.12), lineWidth: 1))
.shadow(color: Color.black.opacity(0.28), radius: 16, x: 0, y: 6)
.scaleEffect(appeared ? 1 : 0.85, anchor: .topTrailing)
.padding(28)
```
`AddMenuRow` (`:104-165`):
```
HStack(spacing: 9)
├─ icon
│    cached NSImage for iconUrl → resizable, .fit, clipShape(RoundedRectangle(cornerRadius: 5, .continuous)), frame 18×18
│    uncached iconUrl         → AsyncImage(url) (placeholder = symbolIcon), frame 18×18
│    no iconUrl               → symbolIcon.frame(width: 18)
│    symbolIcon = Image(systemName: symbol).font(.system(size: 13, weight: .medium))
│                 .foregroundColor(highlighted ? .primary : .secondary)
├─ Text(label).font(.system(size: 13.5, weight: .medium)).foregroundColor(.primary).lineLimit(1)
└─ Spacer(minLength: 0)
.padding(.horizontal, 9).padding(.vertical, 7)
.frame(maxWidth: .infinity, alignment: .leading)
.background(RoundedRectangle(cornerRadius: 9, .continuous).fill(highlighted ? Color.primary.opacity(0.09) : .clear))
.contentShape(RoundedRectangle(cornerRadius: 9, .continuous))
```
Entries sent by JS (`DS/App.tsx:665-676`): one per program `{ id: "program:<name>", label: displayName || name, symbol: "arrow.up.forward.app", iconUrl }`, then `{ separator: true }`, then `{ id: "create-new", label: "Start from link", symbol: "plus" }`. Icons are prefetched into `AddMenuIconCache` via `prefetch_add_menu_icons` when the program registry loads (`DS/App.tsx:521-529`, `AddMenu.swift:32-53`).

---

### 2.7 Non-macOS tray icon (tauri `TrayIconBuilder`)

`show_tauri_tray` (`ST/src/tray.rs:70-150`), id `"timelapse_tray"`:
- Icon: Linux `ST/icons/timelapse_linux.png` (red timelapse glyph); Windows `ST/icons/32x32.png` (`:80-83`).
- Title (shown on Linux panels; Windows ignores titles): recording `"{time}"`, paused `"⏸ {time}"` (`:288-300`).
- Tooltip: recording `"Lookout — {time} recorded"`, paused `"Lookout — paused at {time}"` (`:288-300`).
- Left-click **up** → `toggle_tray_window` (Windows) (`:95-107`).
- Linux menu: see §5.4. On Linux, when the GNOME extension pill is attached the tray icon is not created at all (`:59-63`).

### 2.8 GNOME Shell panel pill (Linux/GNOME only)

Extension `lookout-indicator@hackclub.com` (v3 / "1.2.0", shell 45–50) installed+enabled automatically (`ST/src/lib.rs:1182-1186`, `ST/src/gnome_indicator.rs:300-387`; `ST/gnome-extension/metadata.json`). State exported on D-Bus `com.hackclub.lookout.Indicator` (+ legacy `com.hackclub.Lookout`), path `/com/hackclub/Lookout/Indicator` (`gnome_indicator.rs:44-62`).
- `PanelMenu.Button(0.5, 'Lookout')` with style classes `screen-recording-indicator` (shell's red recorder pill) + `lookout-indicator` (`ST/gnome-extension/extension.js:46-56`).
- Content: `St.BoxLayout.lookout-indicator-box` (spacing `0.35em`) → `St.Icon` (app icon `icons/lookout.png`; paused → `media-playback-pause-symbolic`, `icon-size: 1.09em`) + `St.Label` (time, y-centered) (`extension.js:63-78`, `stylesheet.css:5-14`).
- Panel button h-padding `-natural-hpadding: 10px; -minimum-hpadding: 10px` (`stylesheet.css:21-24`).
- Paused: removes `screen-recording-indicator`, adds `lookout-indicator-paused`: `transition-duration:150ms; border:3px solid transparent; border-radius:999px; font-weight:bold; color:#1c1917; box-shadow: inset 0 0 0 100px #f59e0b`; hover/focus `#fbaf24`; active/checked `#fdbc45` (`stylesheet.css:31-48`, `extension.js:103-111`).
- Menu: "Pause recording"/"Resume recording" toggle, separator, "Stop recording", "Open Lookout" (`extension.js:80-99`).

### 2.9 Waybar module (optional, user-installed)

`ST/waybar-module/lookout-waybar.py` emits JSON `{text: time, class: state ("recording"|"paused"), tooltip: "Lookout"}` (`lookout-waybar.py:40-43`). Suggested format icons: recording `<span size='75%' rise='1800'>●</span>`, paused `⏸` (`README.md`). GtkMenu (`lookout-menu.xml`): "Lookout" (insensitive), separator, "Pause / resume recording", "Stop recording", separator, "Open Lookout".

### 2.10 Linux desktop entry

`ST/linux/lookout.desktop`: Tauri template with `Exec={{exec}} %u` (so `lookout://` deep links pass the URL), `StartupWMClass={{exec}}`, `Terminal=false`, `Type=Application`.

### 2.11 Capabilities (what a native port must allow)

`ST/capabilities/default.json`: windows `main`, `tray`, `editor-*`; permissions include `core:window:allow-start-dragging`, `allow-set-title`, `allow-close`, `allow-start-resize-dragging`, `allow-toggle-maximize`, `allow-is-maximized`, `allow-outer-size`, `allow-current-monitor`, `allow-is-focused`, `allow-set-focus`, `allow-get-all-windows`, `allow-destroy`, `core:webview:allow-create-webview-window`, `allow-get-all-webviews`, event listen/emit/unlisten, `liquid-glass:default`, shell open, process, dialog, deep-link, macos-permissions, updater, notification, http (`http://localhost:*`, `https://**`).

### 2.12 Custom URI scheme used by windows

`lookout-preview://localhost/...` (Windows: `http://lookout-preview.localhost/...`, `DS/hooks/useScreenPreview.ts:106-107`) serves live JPEG source previews; query `maxWidth` (default 854), `maxHeight` (480), `jpegQuality` (85); response headers `Content-Type: image/jpeg`, `Cache-Control: no-store, no-cache, must-revalidate, max-age=0`, `Access-Control-Allow-Origin: *`; 400 on bad URL, 500 + error text on capture failure (`ST/src/lib.rs:1014-1063`). Preview frames are only wanted while `main` is focused (`wants_preview_frames`, `ST/src/lib.rs:222-228`).

## 3A. Main window: routing, state machine, and main screens

All paths are relative to the repo root. `D/` = `clients/desktop/src/`, `R/` = `clients/react/src/`.
Token values are resolved from `R/ui/theme.ts` (dark is the default in `:root`; light applies via `@media (prefers-color-scheme: light) :root:not([data-theme="dark"])` or `:root[data-theme="light"]`). Each token is written as `token (dark / light)`.

### 3A.0 Token quick reference (as used below)

| Token | CSS var | Dark | Light | Ref |
|---|---|---|---|---|
| colors.bg.body | --color-bg-body | #242424 | #fafafa | R/ui/theme.ts:20,71 |
| colors.bg.panel | --color-bg-panel | #1e1e1e | #ffffff | :21,72 |
| colors.bg.backdrop | --color-modal-backdrop | rgba(0,0,0,0.55) | rgba(0,0,0,0.35) | :22,73 |
| colors.bg.surface | --color-bg-surface | rgba(255,255,255,0.08) | #ffffff | :23,74 |
| colors.bg.sunken | --color-bg-sunken | rgba(255,255,255,0.04) | rgba(0,0,0,0.04) | :24,75 |
| colors.bg.selected | --color-bg-selected | rgba(255,255,255,0.12) | rgba(0,0,0,0.1) | :33,84 |
| colors.text.primary | --color-text-primary | #ffffff | rgba(0,0,0,0.85) | :25,76 |
| colors.text.secondary | --color-text-secondary | rgba(255,255,255,0.7) | rgba(0,0,0,0.65) | :27,78 |
| colors.text.tertiary | --color-text-tertiary | rgba(255,255,255,0.5) | rgba(0,0,0,0.45) | :28,79 |
| colors.text.quaternary | --color-text-quaternary | rgba(255,255,255,0.28) | rgba(0,0,0,0.25) | :29,80 |
| colors.text.error | --color-text-error | #fca5a5 | #ef4444 | :30,81 |
| colors.border.default | --color-border-default | rgba(255,255,255,0.12) | rgba(0,0,0,0.12) | :31,82 |
| colors.border.hover | --color-border-hover | rgba(255,255,255,0.22) | rgba(0,0,0,0.22) | :32,83 |
| colors.border.selected | --color-border-selected | rgba(255,255,255,0.3) | rgba(0,0,0,0.3) | :34,85 |
| colors.icon.selected | --color-icon-selected | rgba(255,255,255,0.8) | rgba(0,0,0,0.8) | :35,86 |
| colors.spinner.base / track | --color-spinner-base / -track | rgba(255,255,255,0.1) / rgba(255,255,255,0.8) | rgba(0,0,0,0.1) / rgba(0,0,0,0.8) | :37-38,88-89 |
| colors.skeleton.bg / shimmer | --color-skeleton-bg / -shimmer | rgba(255,255,255,0.06) / rgba(255,255,255,0.12) | rgba(0,0,0,0.05) / rgba(0,0,0,0.08) | :39-40,90-91 |
| colors.badge.primaryBg / primaryText | | #22c55e26 / #22c55e | #22c55e / #ffffff | :41-42,92-93 |
| colors.badge.overlayBg / overlayText | | rgba(0,0,0,0.7) / #ffffff | #000000 / #ffffff | :43-44,94-95 |
| colors.accent.base / hover / on | --color-accent / -hover / --color-on-accent | #3b82f6 / `color-mix(in oklab, var(--color-accent) 88%, black)` (fallback #2f6fd0) / #ffffff | same | :64-67,108-111 |
| colors.status.success/info/warning/danger | literal | #22c55e / #3b82f6 / #f59e0b / #ef4444 | same | :193-196 |
| colors.status.neutral | --color-status-neutral | rgba(255,255,255,0.2) | #000000 | :36,87 |
| archive vars | --color-archive-bg/-icon/-border/-hover-bg/-hover-border | rgba(0,0,0,0.6) / #ffffff / rgba(255,255,255,0.1) / rgba(255,255,255,0.1) / rgba(255,255,255,0.2) | rgba(255,255,255,0.9) / #000000 / rgba(0,0,0,0.1) / rgba(255,255,255,1) / rgba(0,0,0,0.2) | :45-49,96-100 |
| spacing | xs 4, sm 8, md 12, lg 16, xl 20, xxl 24, xxxl 32 | | | :201 |
| radii | sm 6, md 8, lg 10 | | | :202 |
| fontSize | xs 11, sm 12, md 13, lg 14, xl 16, xxl 18, heading 20, display 24, timer 32 | | | :203 |
| fontWeight | normal 400, medium 500, semibold 600, bold 700 | | | :204 |
| statusConfig | pending "Pending" neutral; active "Recording" success; paused "Paused" warning; stopped "Processing" info; compiling "Compiling" info; complete "Complete" success; failed "Failed" danger | | | :207-215 |

Linux-only header-bar vars (from `D/linuxChrome.ts:134-135,155-156`): `--color-headerbar-control` dark rgba(255,255,255,0.1) / light rgba(0,0,0,0.08); `--color-headerbar-control-hover` dark rgba(255,255,255,0.18) / light rgba(0,0,0,0.16). `HEADER_BAR_HEIGHT = 47` (`D/linuxChrome.ts:34`).

Body font: `-apple-system, BlinkMacSystemFont, 'Geist', 'Segoe UI', Roboto, sans-serif` (`clients/desktop/index.html:62`) — so macOS renders SF Pro; Geist applies on Windows/Linux (Linux may override with session UI font via linuxChrome). Body is `user-select: none; cursor: default; overscroll-behavior: none` (index.html:62). Scrollbars hidden: `::-webkit-scrollbar { width: 0px; background: transparent }` (index.html:67-70). Right-click context menu disabled globally (`D/main.tsx:128`).

---

### 3A.1 Entry and window-level routing (`D/main.tsx`, `D/App.tsx`)

- `main.tsx` renders `<ErrorBoundary><App/></ErrorBoundary>` into `#root` (`D/main.tsx:130-134`).
- `App()` (`D/App.tsx:166-179`) decides by `window.location.hash`:
  1. `hash.includes("tray")` → `<TrayApp/>` (tray popup window) (`:167-170`).
  2. `hash` matches `/^#\/?editor\?token=([0-9a-fA-F]{64})/` → `<EditorWindow token=…/>` (`:174-177`).
  3. Otherwise → `<MainWindowApp/>`.

#### ErrorBoundary crash screen (`D/main.tsx:74-125`)
Box tree:
```
div  padding 24, font-family monospace, background transparent, color var(--color-text-primary, #e5e5e5), min-height 100%
└ div  max-width 500, margin 0 auto, border 1px solid #f44, radius 8, padding 20
  ├ h2 "Something went wrong"  color #f44, margin 0 0 12px 0, font-size 18
  ├ pre {error stack}  color #faa, font-size 12, white-space pre-wrap, word-break break-all, max-height 300, overflow-y auto, margin-bottom 16
  ├ div flex gap 8
  │  ├ button "Copy Error Report" | "Copied!" (for 2000 ms after copy, setTimeout D/main.tsx:90)  padding 8px 16px, bg #333, color #0f0, border 1px solid #555, radius 4, font-size 13, cursor pointer
  │  └ button "Try Again"  same but color #ff0; resets error
  └ p "Press ` to open the debug log. Copy and send to the developer."  color #888, font-size 11, margin-top 12
```
Copy writes `REACT CRASH:\n${error}\n\n--- LOG ---\n${getReport()}` to clipboard (`:87`).

---

### 3A.2 Hash router (`R/hooks/useHashRouter.ts`)

Routes (`:3-10`): `gallery` (`#/`), `add` (`#/add`), `settings` (`#/settings`), `tray` (`#/tray`), `record` (`#/record?token=T`), `session` (`#/session?token=T`), `editor` (`#/editor?token=T`). Unknown/empty hash → gallery; record/session/editor without token → gallery (`:12-28`). `navigate(r)` sets `window.location.hash` (`:100-102`); route state follows `hashchange` (`:52-56`).

Global keyboard (`:59-98`): on `keydown` of `Escape` or `Backspace`, unless target is INPUT/TEXTAREA/contentEditable or `e.defaultPrevented`, and current page ≠ gallery → `preventDefault()`, `history.back()`, then after **50 ms** (`setTimeout`, `:87-91`) if still not gallery, force `location.hash = "#/"`.

---

### 3A.3 MainWindowApp state machine (`D/App.tsx:181-1546`)

#### 3A.3.1 State
| State | Init | Ref |
|---|---|---|
| screenPermGranted | `!isMacOS \|\| localStorage[permCacheKey("screen")]==="1"` | :185-187 |
| cameraPermGranted | `!isMacOS \|\| localStorage[permCacheKey("camera")]==="1"` | :188-190 |
| (bg recheck) on macOS, if cached "1" but `checkScreenRecordingPermission()`/`checkCameraPermission()` false → remove cache, regate | | :192-210 |
| isWayland | `invoke("is_wayland")` | :211, :504-506 |
| route | useHashRouter | :212 |
| editorWindowToken | `useEditorWindowOpen()` (token of open editor window or null) | :232 |
| editNonce | bumped on `EDITED_EVENT` → remounts SessionDetail | :237, :372 |
| panel `{token,url,fallbackUrl}` | ProgramPanel currently shown | :256-260 |
| panelNonce | bumped on panel resolution | :263 |
| tipOpen / tipOverride | TipDrawer | :465-468 |
| addMenu `{items, anchor}` | Win/Linux DOM add popup | :541 |
| startingProgram (label) | "Starting X…" pill | :545 |
| panelPrompt / openIn / sessionProgram | per-session-view program data | :1018-1036 |
| appVersion | `getVersion()` | :956-964 |
| pageNav `{owner, nav, routeKey}` | header contributions from pages | :1258-1266 |

#### 3A.3.2 Content selection (`:1114-1241`)
```
if !screenPermGranted            → <PermissionScreen key="screen" type="screen" onGranted=…/>
else if !cameraPermGranted       → <PermissionScreen key="camera" type="camera" onGranted=…/>
else if editorWindowToken        → <EditorOpenPlaceholder token=…/>
else switch route.page:
  gallery  → <Gallery sessions loading error onSessionClick=openSession onArchive=archiveSession
              onSessionContextMenu=handleSessionContextMenu onAdd=handleAdd onSettings=→settings
              banner={announcement ? <AnnouncementBanner/> : undefined}
              showHeader={!isLinux} showBottomFade={!isLinux}/>          (:1120-1143)
  settings → <SettingsPage onBack=toGallery isWayland/>                  (:1144-1150)
  add      → <AddSessionPage onBack=toGallery onOpenProgram=openProgram
              onStart={token → addToken; handleDeepLink(["lookout://session/?token="+token])}/>  (:1151-1161)
  record   → <RecordPage key=token token onBack=toGalleryRefreshed
              onViewSession={t → addToken(t); navigate session} onSourcePicker=handleSourcePicker/> (:1162-1174)
  session  → <SessionDetail key=`${token}:${editNonce}` token apiBaseUrl client=detailClient
              onEdit={openEditorWindow(token)} onRecordingFinished={maybeOpenPanel(...)}
              onComplete={handleCompleted(...)} titleAction={<OpenInProgram/> if openIn.token===token}
              belowVideo={<PanelPrompt/> if panelPrompt && panel?.token !== panelPrompt.token}
              onBack=toGalleryRefreshed showBack={!isLinux} onArchive=confirm→archive→gallery/>  (:1175-1227)
```
`toGallery` = navigate gallery; `toGalleryRefreshed` = `gallery.refresh()` then navigate gallery (`:1009-1013`).

#### 3A.3.3 Header (Linux header bar) title derivation (`:1273-1299`)
- `appTitle = { title: "Lookout", subtitle: appVersion ? "v"+appVersion : undefined }`.
- Permission gate showing → appTitle.
- Editor window open → `{ title: "Lookout", subtitle: "Editing in another window" }`.
- gallery → `{ title: "Lookout" }` (no subtitle).
- settings → `{ title: "Settings", onBack: toGallery }`.
- add → `{ title: "New Timelapse", onBack: toGallery }`.
- record → `{ title: "Recording", onBack: toGalleryRefreshed }`.
- session → `{ title: "Timelapse", onBack: toGalleryRefreshed }`.
- Page contributions (via `usePublishHeaderNav`, `D/headerNav.tsx`) override: `title = nav.title ?? route.title`; `subtitle = nav.title ? nav.subtitle : (nav.subtitle ?? route.subtitle)`; `onBack = nav.onBack ?? route.onBack`. A contribution counts only while its `routeKey` (`${page}:${token??""}`) equals the current one (`:1254-1271`), so an outgoing page that is still animating out cannot leave its back button in the header. Publishing is a no-op on non-Linux (`D/headerNav.tsx:65`). `onBack` is read via ref, so only title/subtitle/hasBack changes republish (`:58-76`).

Window title: `setTitle(isLinux ? "Lookout" : "Lookout v"+version)` (`:961`).

#### 3A.3.4 Chrome layout (render tree, `:1368-1545`)
```
HeaderNavProvider
└ div  display flex, column, height 100%, position relative                          (:1370)
  ├ [isLinux] <HeaderBar title subtitle appearance actions=headerActions onBack/>      (:1374-1382)
  ├ [isMacOS] div.titlebar data-tauri-drag-region                                      (:1384-1390)
  │    height 32, flex-shrink 0, width 100%, z-index 9999, bg transparent, cursor default,
  │    flex, align center, justify flex-end, padding-right 6, box-sizing border-box
  │    └ [onMainPage] <UpdatePill phase onRestart/>
  ├ [Windows] div position absolute, bottom 8, left 8, z-index 9999                   (:1395-1397)
  │    └ [onMainPage] <UpdatePill … origin="bottom"/>
  ├ <AnimatePresence>{addMenu && <AddMenuPopup items anchor onSelect/>}</AnimatePresence>  (:1401-1412)
  ├ [panel] <ProgramPanel key=panel.token …/>                                          (:1415-1446)
  ├ <TipDrawer tip={tipOverride ?? tip} open={tipOpen} onClose/>                       (:1450)
  ├ <UpdateDrawer update={linuxUpdate} open={Boolean(linuxUpdate) && onMainPage && !tipOpen} onClose=dismissLinuxUpdate/>  (:1454-1458)
  ├ <AnimatePresence>{startingProgram && StartingPill}</AnimatePresence>               (:1462-1498)
  └ div  flex 1, overflow hidden, flex column, position relative                       (:1499-1505)
     └ <AnimatePresence mode="sync" initial={false} custom={routeDirection}>
        └ motion.div key=routeKey (route transition, see C-anim)                      (:1507-1540)
           style: position absolute, inset 0, height 100%,
                  overflow-y: route.page==="gallery" ? "hidden" : "auto"
           └ mainView
```
- `onMainPage = route.page==="gallery" && screenPermGranted && cameraPermGranted && !editorWindowToken` (`:1304-1305`).
- `headerActions` (Linux) = `[onMainPage && <UpdatePill/>]` + gallery actions (`:1336-1341`). Gallery actions (only on main page, `:1307-1331`):
  - `button.lookout-headerbar-action` title/aria "Settings" → navigate settings. Icon **GearSixIcon size 15 weight "fill"**.
  - `button.lookout-headerbar-action` title/aria "Start" → `handleAdd(rect of button)`. Icon **PlusIcon size 17 weight "bold"**.
- `routeKey = editorWindowToken ? "editor-open:"+token : page+":"+(token??"")` (`:1364-1366`).
- `routeDirection` (`:1344-1354`): gallery→non-gallery = +1; non-gallery→gallery = −1; record→session = +1; session→record = −1; anything else = +1.

**Starting pill** (`:1463-1497`): outer `motion.div` position fixed, bottom 20, left 0, right 0, flex, justify center, z-index 10001, pointer-events none. Inner div: flex, align center, gap 8, padding `8px 14px`, radius radii.lg (10), border `1px solid colors.border.default`, background colors.bg.panel, color colors.text.secondary, font-size fontSize.sm (12), box-shadow `0 4px 18px rgba(0, 0, 0, 0.18)`. Children: `<Spinner size="sm"/>` (16px) + `<span>Starting {label}…</span>` (U+2026 ellipsis).

#### 3A.3.5 Flows / transitions
Text flow diagram:
```
[launch] ─ macOS perms cached? ─no→ PermissionScreen(screen) ─granted→ PermissionScreen(camera) ─granted→ ┐
                     └─yes───────────────────────────────────────────────────────────────────────────────┘
                                                                                                         ▼
                                                     ┌──────────── Gallery (#/) ◄──────────────────────────┐
     gear / header gear ───────────────────────────► │ Settings (#/settings) ──back──────────────────────►│
     + (programs empty OR native menu fails) ──────► │ AddSessionPage (#/add) ──back─────────────────────►│
     + (programs, macOS) → native NSPanel menu → choice:                                                   │
          "create-new" → #/add ; "program:<name>" → openProgram (instant start / pairing / browser)        │
     + (programs, Win/Linux) → AddMenuPopup (DOM) → same choices                                           │
     card click: status pending|active|paused → #/record?token ; else → #/session?token                    │
     card right-click: native menu [Open, (Open in Editor if complete), ─, Archive]                       │
     deep link lookout://session/?token=T: addToken; if recording another token → pause it;               │
          status stopped|compiling|complete|failed → #/session else → #/record; focus window              │
     deep link lookout://pair?code&state → completePairing → openProgram(linked)                           │
                                                                                                           │
  RecordPage: loading skeleton → (error) "Session Error" | (finished) "Session Already X" → View Timelapse │
             → SourcePicker ⇄ DesktopRecorder (Change source / source lost)                               │
             → Stop → NamingModal → Save | Edit & save (→ editor window) → #/session                       │
  SessionDetail: onRecordingFinished → maybe ProgramPanel sheet; onComplete → redirect hook (once)  ──back─┘
  Editor window open → main window content replaced by EditorOpenPlaceholder (slides like a route)
```
Details:
- `handleAdd(anchor)` (`:636-690`): if DOM add menu open → close (toggle). Else refresh registry in background; if no programs → `#/add`. Non-macOS → set `addMenu` with items `programs.map → {id:"program:"+name, label: displayName||name, iconUrl, fallbackIcon: <ArrowSquareOutIcon size={15} weight="bold"/>}`, `{separator:true}`, `{id:"create-new", label:"Start from link", fallbackIcon:<PlusIcon size={15} weight="bold"/>}` (`:651-664`). macOS → `invoke("show_add_menu", {entries, anchor})` with entries `{id, label, symbol:"arrow.up.forward.app", iconUrl}`, separator, `{id:"create-new", label:"Start from link", symbol:"plus"}` (`:667-681`); on failure → `#/add`.
- `handleMenuChoice` (`:620-634`): null → nothing; `create-new` → `#/add`; `program:X` → `openProgram`; outcome "failed" → `#/add`.
- `openProgram` (`:595-617`): linkable+paired → `startViaLink`; linkable unpaired → `beginPairing` (browser) ; else `invoke("open_external_url", {url: newSessionUrl})`.
- `startViaLink` (`:554-586`) sets `startingProgram=label` (shows pill) for its whole duration; on `unauthorized`/`origin-changed` re-pairs; fallback opens browser.
- Archive (`:706-718`): Tauri `confirm("Are you sure you want to archive this session?", { title: "Archive Session", kind: "warning" })`.
- Session context menu (`:722-744`): Tauri `Menu` popup: `MenuItem "Open"`, `MenuItem "Open in Editor"` (only status complete), `PredefinedMenuItem Separator`, `MenuItem "Archive"`.
- Pairing failure dialog (`:780-783`): `message("Linking failed — you can retry from the + menu.", { title: "Lookout", kind: "error" })`.
- Cold-start deep links: poll `invoke("get_cold_start_urls")` up to 10 times, 500 ms apart (`:853-866`). Warm: `listen("lookout-deep-link", string[])` (`:835`). Dev `?token=` query param also routed (`:875-882`). Dedupe on last URL (`:754`).
- `listen("lookout-navigate")` payload `"/add"` → `#/add` (`:943-947`).
- Theme: Linux uses `matchMedia("(prefers-color-scheme: dark)")` → sets `data-theme` and `getCurrentWindow().setTheme(theme)`, re-applied on change (`:894-913`). Others: `getCurrentWindow().theme()` + `onThemeChanged` → `data-theme` attr (`:914-923`).
- Vibrancy (`:967-1004`): non-Linux `invoke("enable_vibrancy")`; on success html/body/#root background → transparent. Cleanup `invoke("disable_vibrancy")`.
- Program panel open logic: `maybeOpenPanel(token, redirectUrl, panelUrl, panelResolved)` opens only if `!panelResolved && isPanelUrlAcceptable(panelUrl) && getPanelState(token)==="pending"` (`:330-348`). `handleCompleted` fires redirect hook only if no acceptable panel URL; redirect fires once per token (`:244-251`, `:356-362`).
- Post-edit (`EDITED_EVENT` listener, `:364-438`): bump editNonce, refresh gallery, maybe open panel; if payload.status==="complete" → handleCompleted; else if no panel opened → poll `getStatus` starting delay 2500 ms, ×1.5 each round, capped 15000 ms, deadline 35 min (`REDIRECT_POLL_MAX_MS = 35*60_000`, `:100`).
- Session view per-route fetch (`:1037-1111`): `getSession()`; `panelPrompt` set when status ∈ {stopped, compiling, complete} and not held (`editHoldUntil` && status≠failed) and `shouldOfferPanel`. `openIn` set when `viewUrl` matches `/^https?:\/\//i`, label = registry displayName || raw program || "program".
- ProgramPanel `onDone` → `setPanelState(token,"done")`, close, panelNonce++, gallery refresh, `openTip("panel-done", token)`; `onDismiss` → `setPanelState(token,"dismissed")`, close, panelNonce++ (`:1429-1444`).
- Tip moments: `"source"` when RecordPage reports the picker is up (`:498-501`); `"panel-done"` above.
- Program label for URL: match registry by origin of newSessionUrl/pairUrl/startUrl; else hostname without `www.`; unparsable → "This program" (`:271-292`).

---

### 3A.4 HeaderBar (Linux only) (`D/components/HeaderBar.tsx`)

Props: `title`, `subtitle?`, `appearance` (`controlsOnRight` from GSettings), `actions?`, `onBack?`, `onClose?`, `maximizable=false` (`:231-262`).

Box tree (`:349-381`):
```
div.lookout-headerbar  data-tauri-drag-region
  position relative, flex-shrink 0, height 47 (HEADER_BAR_HEIGHT), display flex, align-items center,
  gap 8 (spacing.sm), padding: onBack ? "0 8px" : "0 8px 0 12px", box-sizing border-box,
  background transparent (no fill, no divider), cursor default, z-index 10
  onDoubleClick: maximizable ? toggleMaximize : none
  ├ [!controlsOnRight] closeButton
  ├ [onBack] backButton  (button.lookout-headerbar-action, aria/title "Back", flex-shrink 0)
  │    └ svg 16×16 viewBox 0 0 16 16, stroke currentColor, stroke-width 1.7, round caps/joins, path "M10 3L5 8l5 5"
  ├ titleBlock  data-tauri-drag-region, flex column, justify center, min-width 0, gap 0, pointer-events none
  │   ├ span {title}  font-size subtitle ? 13 : 14, weight 700, line-height 1.15, color text.primary, nowrap, ellipsis
  │   └ [subtitle] span {subtitle} font-size 11, weight 400, line-height 1.15, color text.secondary, nowrap, ellipsis
  ├ div data-tauri-drag-region  flex 1, align-self stretch  (drag spacer)
  └ pageActions div  flex, align center, gap 4 (spacing.xs), flex-shrink 0
      ├ {actions}
      └ [controlsOnRight] closeButton
closeButton = button.lookout-headerbar-control aria/title "Close" → onClose ?? getCurrentWindow().close()
   └ svg 14×14 viewBox 0 0 16 16, stroke currentColor, stroke-width 1.6, round cap, path "M4 4l8 8M12 4l-8 8"
```
Injected stylesheet (`style[data-lookout-headerbar]`, injected at import time to avoid first-frame flash, `:145-213`):
| Selector | Rules |
|---|---|
| `.lookout-headerbar-control` | 24×24, border none, radius 50%, padding 0, inline-flex centered, cursor default, color var(--color-text-primary), background var(--color-headerbar-control), `transition: background 100ms ease-out` |
| `:hover` | background var(--color-headerbar-control-hover) |
| `:active` | background var(--color-border-hover) |
| `:focus-visible` | outline 2px solid var(--color-accent), outline-offset 1px |
| `.lookout-headerbar-action` | 30×30, border none, radius 50%, padding 0, inline-flex centered, cursor default, color var(--color-text-secondary), background transparent, `transition: background 100ms ease-out, color 100ms ease-out` |
| `:hover` | background var(--color-headerbar-control), color var(--color-text-primary) |
| `:active` | background var(--color-headerbar-control-hover) |
| `:focus-visible` | outline 2px solid var(--color-accent), outline-offset 1px |
Additionally (`D/linuxChrome.ts:257-261`): `html.os-linux .lookout-headerbar { transition: opacity 160ms ease-out }`, `html.os-linux.lookout-backdrop .lookout-headerbar { opacity: 0.55 }` (window unfocused = GTK :backdrop).

---

### 3A.5 PageLayout (`D/components/PageLayout.tsx`)

Exports `cardButtonStyle = { background: colors.bg.surface, border: 1px solid colors.border.default, borderRadius: radii.lg (10) }` (`:13-17`) — used to restyle secondary Buttons as "card" buttons.

Props: `onBack?, icon?, title?, subtitle?, hint?, actions?, children?`. Publishes `{onBack}` to header nav (`:40`).
```
div  flex column, height 100%, max-width 480, margin 0 auto, padding 16, box-sizing border-box     (:43-46)
├ [onBack && !isLinux] div flex-shrink 0                                                             (:48-63)
│   └ Button variant secondary size sm style=cardButtonStyle
│       macOS: <span>← Back</span> (&larr;)
│       Windows: inline-flex, align center, gap 4: svg 14×14 viewBox 0 0 24 24 stroke currentColor width 2 round, path "m15 18-6-6 6-6" + <span>Back</span>
├ div  flex 1, flex column, justify center, align center, text-align center, padding "0 24px"        (:66-70)
│   ├ [icon] div 72×72, radius 50%, background `${colors.text.quaternary}20` (string concat onto a var() — invalid CSS, so effectively NO background), flex centered, margin-bottom 20, flex-shrink 0  (:71-80)
│   ├ [title] h2 font-size 20, weight 700, color text.primary, margin 0, margin-bottom 8             (:81-88)
│   ├ [subtitle] p font-size 13, color text.secondary, margin 0, line-height 1.6, margin-bottom 16 if hint  (:89-97)
│   ├ [hint] p font-size 12, color text.tertiary, margin 0, line-height 1.5                           (:98-105)
│   └ {children}
└ [actions] div flex-shrink 0, flex column, gap 12                                                    (:110-114)
```
Note: `${colors.text.quaternary}20` evaluates to `var(--color-text-quaternary)20`, which is invalid; the circle renders transparent. Same pattern in SourcePicker "Minimized" badge (`${colors.text.secondary}26`) and DesktopRecorder stale banner (`colors.status.warning + "1a"` — this one IS valid since status.warning is literal `#f59e0b` → `#f59e0b1a`).

---

### 3A.6 Shared primitives used on these screens (from `@lookout/react`)

#### Button (`R/ui/Button.tsx`)
- Outer `motion.button`: position relative, font-weight 600, radius `style.borderRadius ?? 8`, cursor `not-allowed` if disabled else pointer, **opacity 0.6 when disabled/loading**, inline-flex centered, width 100% if fullWidth, padding 0, color variant color, background transparent, border 1px solid transparent, plus caller's other styles (`:61-82`). `whileHover="hover"`, `whileTap="active"` (only when not disabled), `initial="idle"`.
- Background layer: `<Squircle cornerRadius={borderRadius ?? 8} cornerSmoothing={0.7} asChild>` (`@squircle-js/react`) wrapping `motion.div` position absolute inset −1, with variants (`:85-101`):
  - idle `{ scale:1, background: idleBg, border: idleBorder }`
  - hover `{ scale:1, background: hoverBg, border: hoverBorder }`
  - active `{ scale:0.96, background: hoverBg, border: hoverBorder }`
  - transition `{ type:"spring", stiffness:1500, damping:60 }` + CSS `transition: "opacity 0.15s, background 0.15s, border-color 0.15s"`.
  - Note: caller `style.background/border/borderRadius/color` are extracted and applied to this layer (`:43`).
- Variants (`:15-22`): primary bg accent.base / color accent.on; success bg #22c55e / #fff; danger bg #ef4444 / #fff; warning bg #f59e0b / #000; secondary bg transparent / color text.secondary / border `1px solid colors.border.hover`; ghost transparent / text.secondary / border transparent. All non-secondary borders `1px solid transparent`.
- Hover bg (`:46-57`): caller background if given; ghost → colors.bg.selected; secondary → colors.bg.surface; primary → colors.accent.hover; others unchanged. Hover border: caller border; ghost → transparent; else variant border.
- Label span (`:102-113`): position relative, z 1, inline-flex centered, gap 8, width 100%, size padding/font: sm `6px 12px`/12; md `8px 16px`/13; lg `12px 24px`/15.
- Loading: prepends `<Spinner size="sm" color={variant==="warning" ? "#000" : "#fff"}/>` (`:114`).

#### Spinner (`R/ui/Spinner.tsx`)
SVG `s×s` (sm 16, md 24, lg 40), viewBox 0 0 24 24, `animation: spin 1s linear infinite`, flex-shrink 0. Base circle r 10 stroke spinner.base width 2; arc circle r 10 stroke `color || spinner.track` width 2 round cap, `stroke-dasharray 62.83`, `stroke-dashoffset 47.12` (≈ quarter arc).

#### Skeleton (`R/ui/Skeleton.tsx:12-25`)
`width ?? 100%`, `height ?? (aspectRatio ? undefined : 20)`, radius default 8, bg skeleton.bg, `background-image: linear-gradient(90deg, transparent 0%, skeleton.shimmer 50%, transparent 100%)`, `background-size: 200% 100%`, `animation: shimmer 1.5s ease-in-out infinite`. **The `shimmer` keyframes are only injected by `LookoutProvider` (`R/LookoutProvider.tsx:93-104`), which in the desktop app mounts only inside RecordPage's recorder branch (`D/components/RecordPage.tsx:318`). index.html defines only `pulse` and `spin` (`clients/desktop/index.html:63-64`). So skeletons are static (no shimmer) until the first recorder has mounted in that webview session; after that the injected style persists.**

GallerySkeleton (`R/ui/Skeleton.tsx:28-48`): padding 16; header row flex space-between, align center, mb 16: Skeleton 180×24, Skeleton 36×36 radius 6; grid `repeat(auto-fill, minmax(240px, 1fr))` gap 12 with 3 cards: wrapper radius 10 overflow hidden → Skeleton aspect 16/9 radius 0, then div padding 12 bg surface: Skeleton 60%×16 mb 4, Skeleton 40%×12.
SessionDetailSkeleton (`:50-62`): Skeleton 16/9 radius 10 mb 16; 50%×18 mb 4; 30%×12 mb 16; row flex gap 16 center: two Skeleton h64 radius 8 flex 1.

#### ErrorDisplay (`R/ui/ErrorDisplay.tsx`)
- `page`: div flex column centered, padding 24, text-align center → h2 {title||"Error"} 20/700 color #ef4444 mb 8 → pre {error} 11 monospace pre-wrap break-all, max-width 400, max-height 150, overflow-y auto, color text.error, mb 16 → row flex gap 8 center: [action] Button primary md {action.label}; [onCopy] Button secondary md "Copy Error" (`:22-33`).
- `inline`: pre 11 monospace, color text.error, max-height 150, scroll (`:35-39`).
- `banner`: div padding `12px 16px`, mb 12, bg `rgba(239,68,68,0.15)`, border `1px solid #ef4444`, radius 8, color text.error, font 13, flex align flex-start gap 8 (`:43`). Inner text block flex 1, font 11, pre-wrap, break-all, max-height 120, scroll: [title] strong block mb 4; span monospace {error}; [reassurance] span opacity 0.8 `"\n\n"+reassurance`. Buttons: [action] motion.button bg transparent, no border, color text.error, font 11, line-height 1, padding `2px 8px`, radius 6, nowrap; inner motion.div absolute inset 0 radius 6 border `1px solid text.error` with tap scale 0.96 spring 1500/60; label. [onCopy] same, label "Copy". [onDismiss] "×" font 16, tap scale 0.96.

#### PageContainer (`R/ui/PageContainer.tsx`)
max-width 640 default, margin 0 auto, padding 16; centered → flex column, align/justify center, min-height 100%, flex 1.

#### Overlay (`R/ui/Overlay.tsx`)
Portal to body. Backdrop div `data-lookout-overlay`, position fixed inset 0, bg colors.bg.backdrop, flex centered, padding 20, z-index 9999; pointerdown on backdrop itself → onDismiss (if given). Panel `motion.div` role dialog, aria-modal, tabIndex −1 (focused on mount), width default `min(1100px, 94vw)`, max-width/height 100%, min-height 0, flex column, bg colors.bg.panel, border `1px solid border.default`, **radius 14**, box-shadow `0 24px 64px rgba(0,0,0,0.45)`, outline none, overflow hidden (`:78-105`). Escape → onDismiss when provided (`:47-54`). Sets `body.style.overflow="hidden"` while mounted. Enter animation only (no exit — unmounts instantly; see C-anim). On Linux CSD, `[data-lookout-overlay]` is inset to the visible window by linuxChrome.ts.

#### Card (`R/ui/Card.tsx`)
Inner div bg colors.bg.surface, border 1px border.default, radius 10, overflow hidden, padding prop, height 100%, flex column. If `onClick`: wrapped in `motion.div` role button tabIndex 0, cursor pointer, display block, height 100%, `whileTap={{ scale: 0.99 }}`, spring 1500/60 (`:42-58`).

#### Badge (`R/ui/Badge.tsx`)
span: size sm font 10 (`fontSize.xs - 1`), padding `2px 8px`; md 12, `4px 12px`; lg 13, `6px 16px`; weight 600; color #fff; radius 999; background statusConfig color; overlay variant adds `box-shadow: 0 0 0 1px rgba(0,0,0,0.1)`. Label from statusConfig (unknown → raw status, neutral color).

#### ProgressRing (`R/ui/ProgressRing.tsx`)
SVG rotated −90°; track circle stroke spinner.base; progress circle stroke `color ?? accent.base`, round cap, dasharray = circumference, dashoffset = circumference×(1−p), `transition: stroke-dashoffset 0.25s linear`. Optional centred NumberFlow percent (min 99) 13/600 text.secondary tabular-nums.

#### MinutesFlow (`R/ui/MinutesFlow.tsx`)
`<span style={{color, fontVariantNumeric:"tabular-nums"}}>`: `<NumberFlow value={m}/>m` or `<NumberFlow value={h}/>h <NumberFlow value={m}/>m`. NumberFlow uses all library defaults (no props).

---

### 3A.7 Gallery (main page) (`R/components/Gallery.tsx`, `R/components/SessionCard.tsx`)

Desktop passes `showHeader={!isLinux}`, `showBottomFade={!isLinux}`, no `onRefresh` (so error state shows no Retry button).

States:
1. `loading && sessions.length===0` → GallerySkeleton (`:138-140`).
2. `error && sessions.length===0` → column: [header] + [banner wrapper padding 16, pb 0] + centered (flex 1, padding 24) `<ErrorDisplay variant="inline"/>` (+ "Retry" button only if onRefresh) (`:142-157`).
3. empty → column: [header] + [banner] + centered flex 1 padding 24: `p` mb 12 containing monitor svg 48×48 (viewBox 0 0 24 24, stroke text.primary width 1.5, opacity 0.2; rect x2 y3 w20 h14 rx2, lines 8,21→16,21 and 12,17→12,21); `p "No timelapses yet"` 14, color text.primary, opacity 0.5, centered; `p "Start a recording session to see it here."` 12, text.primary, opacity 0.3, mt 4 (`:159-179`).
4. list → column height 100%: [header]; [banner: div padding 16 pb 0]; scroll div flex 1, overflow-y auto, padding 16, `mask-image` fade (`:181-208`): `linear-gradient(to bottom, {top ? "transparent 0%, black 20px" : "black 0%, black 20px"}, {bottom && showBottomFade ? "black calc(100% - 20px), transparent 100%" : "black calc(100% - 20px), black 100%"})`. Top mask when scrollTop>0; bottom when `ceil(scrollTop+clientHeight) < scrollHeight` (`:107-136`). Grid `repeat(auto-fill, minmax(240px, 1fr))` gap 12 (in the 480-wide window this is 1 column). Scroll position is cached module-globally and restored on remount (`:87`, `:116-121`). Sessions are shown in server batch response order.

GalleryHeader (`:56-84`) (non-Linux): div flex space-between align center, padding 16, pb 0, flex-shrink 0 → h2 "Your Timelapses" 20/700 text.primary → right group flex gap 4: Button ghost sm title/aria "Settings" style addButtonStyle → **GearSixIcon size 30 weight fill**; Button ghost sm title/aria "Start" → **PlusIcon size 28 weight bold**; onClick passes the button's bounding rect as anchor. `addButtonStyle` = radius 8, font 18, 40×40, padding 0, inline-flex centered (`:45-54`). (Note: Button's inner span still applies sm padding `6px 12px`.)

SessionCard (`R/components/SessionCard.tsx`): Card (clickable, tap scale 0.99) with style position relative.
```
Card
├ thumb div  position relative, aspect 16/9, bg sunken, overflow hidden
│  ├ img thumbnailUrl  100%×100% object-fit cover, block, loading lazy, alt "Timelapse thumbnail"
│  │   or placeholder: centered monitor svg 32×32 stroke text.quaternary width 1.5
│  └ span position absolute top 8 right 8 → <Badge status variant="overlay"/> (sm)
├ info div padding 12
│  ├ name div 13/600 text.primary, ellipsis nowrap, mb 2
│  └ meta div 11 text.tertiary: `${formatTrackedTime(trackedSeconds)} · ${dateStr}`
└ [onArchive] motion.button "Archive" (title)  position absolute top 8 left 8, 24×24, radius 50%, bg transparent,
     color var(--color-archive-icon,#fff), border none, font 14, flex centered, padding 0, z 10; stops propagation
     ├ bg motion.div absolute inset 0 radius 50%, bg var(--color-archive-bg), backdrop-filter blur(4px),
     │   border 1px solid var(--color-archive-border), CSS transition "all 0.15s"; tap scale 0.99 spring 1500/60
     │   hover (JS): bg → --color-archive-hover-bg, border → --color-archive-hover-border, parent color → var(--color-text-error)
     └ icon div relative z1, transition "color 0.15s": svg 14×14 X (lines 18,6→6,18; 6,6→18,18) stroke currentColor width 2.5 round
```
`dateStr = toLocaleDateString(undefined, {month:"short", day:"numeric", year: different-year ? "numeric" : undefined})`. `formatTrackedTime`: `${h}h ${m}min` / `${h}h` / `${m}min` / `"< 1min"` (`R/hooks/useSessionTimer.ts:230-237`).

Data (`R/hooks/useGallery.ts`): cached in localStorage `lookout:gallery-cache:v2`; skeleton only when not all tokens cached; batches of 100 tokens; re-fetch on `visibilitychange` → visible. Tokens from `lookout-tokens` (non-archived).

---

### 3A.8 RecordPage (`D/components/RecordPage.tsx`)

State: `captureSource: CaptureSource[]|null`, `captureFlowDirection` (+1 on select, −1 on change source), `stopping`, `sessionCheck: "loading"|"ok"|"finished"|"error"`, `sessionStatus`, `checkError`, `isPrompting` (`:39-45`). Status check via `client.getStatus()`; status ∈ {stopped, compiling, complete, failed} → finished (`:59-75`). Re-checked on "change source". `releaseScreencast` (`invoke("release_screencast")`) on stop, on change source, and on unmount (`:93-100`). Calls `onSourcePicker()` whenever picker is visible and check is ok (`:142-145`).

**Loading** (`:162-180`):
```
div flex column height 100%
├ div max-width 480, margin 0 auto, padding 16, width 100%, flex-shrink 0, flex space-between align center
│  ├ Skeleton 80×32 radius 8
│  ├ Skeleton 180×24 radius 8 margin 0 auto
│  └ div width 80 (balance)
└ div max-width 480, margin 0 auto, padding 16, padding-top 0, flex 1, width 100%
   ├ Skeleton aspect 2/1 radius 10 mb 16
   ├ Skeleton h36 radius 8 mb 12
   ├ 3× Skeleton h48 radius 8 mb 4
   └ Skeleton h48 radius 10 mt 16
```
**Error** (`:182-197`): `PageLayout onBack icon=… title="Session Error" subtitle={checkError || "Unknown error"}`. Icon svg 32×32 viewBox 0 0 24 24 stroke #ef4444 width 1.5 round: circle r10 + X lines (15,9→9,15; 9,9→15,15).
**Finished** (`:199-218`): label = complete→"Complete", compiling→"Compiling", failed→"Failed", else "Stopped". `PageLayout title="Session Already {label}" subtitle="This session is no longer recordable."` icon svg 32×32 stroke text.tertiary width 1.5 polyline "20 6 9 17 4 12" (check). actions: `Button primary lg fullWidth "View Timelapse"` → onViewSession.
**OK** (`:220-331`): `div position relative, height 100%, overflow hidden` → `AnimatePresence mode="sync" initial={false} custom={captureFlowDirection}` switching between two `motion.div`s (same slide variants as route transition; see C-anim):

Picker branch (key "source-picker", style absolute inset 0, flex column, height 100%):
```
header div  max-width 480, margin 0 auto, padding 16, padding-bottom 0, flex space-between align center,
            flex-shrink 0, width 100%, position relative                                    (:251)
├ [!isLinux] Button secondary sm style={...cardButtonStyle, zIndex 1}
│    macOS "← Gallery"; Windows: svg chevron 14 (path "m15 18-6-6 6-6", stroke 2) + "Gallery", gap 4
├ h2  position absolute, left/right = (sessionStatus !== "pending" ? 100 : 0), text-align center,
│     font 14, weight 700, text.primary, margin 0, pointer-events none                       (:266)
│     text: sessionStatus !== "pending" ? "Select source" : "What should Lookout capture?"
└ [sessionStatus !== "pending"] Button danger md loading={stopping} style z1 "Stop Session" → isPrompting=true
div flex 1, min-height 0 → <SourcePicker onSelect submitLabel/>
   submitLabel = status active|paused ? "Resume Session" : "Start Capture"                   (:278)
[isPrompting] <NamingModal loading={stopping} onConfirm=stop(name,false) onEditAndSave=stop(name,true) onResume=close/>
```
Stopping from picker: `invoke("session_finish", {token, apiBaseUrl, name, edit})`; if edit → open editor window; release screencast; navigate session (`:102-124`).

Recorder branch (key `recorder:${type:id,…}`, absolute inset 0): `<LookoutProvider token apiBaseUrl client><DesktopRecorder …/></LookoutProvider>`.

---

### 3A.9 SourcePicker (`D/components/SourcePicker.tsx`)

Props: `onSelect(source|source[])`, `submitLabel="Start Capture"`.
State: sources (monitors/windows), cameras (MediaDeviceInfo), error, dialogError, tab (`"screens"|"windows"|"cameras"|"cast"`, default screens), selected[], hoveredWindow, hoverAspectRatio (16/9), isWayland, showTopMask/showBottomMask, cameraPreviewStream (`:137-156`).

Data:
- `refresh()` (`:258-305`): `invoke("is_wayland")`; Wayland → force tab "cast" (unless cameras), sources `{monitors:[],windows:[]}`. Else `invoke("list_capture_sources")` → `{monitors: MonitorInfo[], windows: WindowInfo[]}` (fields `:20-38`). Auto-select: if remembered monitor ids (`localStorage["lookout-last-monitor-selection"]`, JSON number[]) all still present → those; else primary (or first). On failure → `error` and `dialogError`.
- Cameras: `enumerateCameras()` on mount + `devicechange` (`:232-256`).
- Camera preview: when exactly one camera selected → `getUserMedia({video:{deviceId:{exact}}, audio:false})` (`:171-222`).
- Wayland cast: `invoke("request_screencast")` → `{node_id}[]` replaces selection with pipewire sources; `invoke("add_screencast")` appends (`:348-373`). Cancelled-by-user portal errors are ignored (`isCancelledCaptureError`).
- On submit: saves monitor ids to localStorage (if any) and calls onSelect(selected) (`:877-880`).

Selection (`:315-334`): click → `[src]`; shift-click → toggle within same type, different type replaces. Cameras: click only, single.

**Error state** (`:385-406`): div flex column centered, min-height 200, padding 24 → `ErrorDisplay variant="page" title="Failed to detect displays" error action={{label:"Retry"}} onCopy` → div mt 12: `Button ghost sm "Troubleshoot"` (opens CaptureErrorDialog) → errorDialog.
**Loading** (`:408-421`): div flex column centered, min-height 200, padding 24, gap 12 → Spinner md (24) → p "Detecting displays..." 14 text.secondary centered.
**CaptureErrorDialog** (`:376-383`): `fallbackTitle = error ? "Couldn't detect your displays" : "Couldn't start the capture"`, onRetry (clears + refresh), onDismiss.

**Main** (`:443-890`):
```
div  max-width 480, margin 0 auto, padding 16, flex column, height 100%, max-height 100%
├ div flex-shrink 0
│ ├ Preview box  position relative, radius 10, overflow hidden, bg sunken, border 1px border.default, mb 16, aspect-ratio 2/1  (:455-459)
│ │  ├ [camera selected && stream] video autoPlay muted playsInline 100%×100% object-fit contain, transform scaleX(-1) (mirrored)
│ │  └ else LayoutGroup id "preview-handoff":
│ │     ├ row div 100%×100%, flex row, overflow hidden, absolute inset 0, gap = selected>1 ? 4 : 0
│ │     │   AnimatePresence mode="popLayout" initial={false}:
│ │     │   ├ selected (non-camera) → PreviewImage per source (key type:id), isMulti = selected>1,
│ │     │   │   layoutId `hover-to-main-preview-${id}` when it's a window currently hovered, fallbackUrl = hover preview url
│ │     │   └ none → motion.div key "preview-empty" 100% centered → p 13 text.quaternary centered:
│ │     │        isCameraSelected ? "Starting camera preview..." : "Select a source below"
│ │     └ AnimatePresence: [tab windows && hovered && not already selected] hover card motion.div   (:504-542)
│ │          absolute right 12, top 20, width 240, max-height 180, aspect-ratio hoverAspectRatio (w/h clamped 0.5..3),
│ │          radius 8, overflow hidden, bg surface, border 1px border.default, box-shadow "0 10px 30px rgba(0,0,0,0.22)",
│ │          pointer-events none, z 2
│ │          ├ hoverPreviewUrl → motion.img layoutId `hover-to-main-preview-${id}` 100% contain, alt "Window hover preview"
│ │          └ else centered p "Loading preview..." 11 text.tertiary
│ └ [hasTabs] Tabs (LayoutGroup)                                                         (:548-584)
│     div flex, gap 4, mb 12, bg colors.bg.surface, radius 8, padding 4, position relative, align stretch
│     per tab: button flex 1, padding "6px 8px", font 12, weight (active 600 : 500), bg transparent,
│              color (active text.primary : text.secondary), border none, radius 6, cursor pointer, relative, flex centered
│       ├ [active] motion.div layoutId "activeTabIndicator" absolute inset 0 radius 6, bg colors.border.default, z 0
│       └ span relative z1 inline-block: `${label} ${count !== undefined ? `(${count})` : ''}`
│     Tabs: Wayland → [Cast]; else [Screens (n)] + [Windows (n) if any]; + [Cameras (n) if any].
│     hasTabs = hasWindows || hasCameras || isWayland (single-Screens case shows no tab bar).
├ list area div flex 1, position relative, min-height 0, flex column                    (:588)
│ ├ scroll div  flex column, gap 4, flex 1, min-height 0, overflow-y auto, padding-bottom 4,
│ │   mask-image/-webkit-mask-image: linear-gradient(to bottom, {top ? "transparent 0%, black 12px" : "black 0%, black 12px"},
│ │                 {bottom ? "black calc(100% - 12px), transparent 100%" : "black calc(100% - 12px), black 100%"})   (:589-603)
│ │  ├ [tab cast] div flex column centered, height 100%, gap 12, padding 20                (:604-650)
│ │  │   ├ p "Wayland requires explicit permission to capture screens or windows." text.secondary centered 13
│ │  │   ├ [pipewire selected] list div width 100%, flex column, gap 4, margin "8px 0"
│ │  │   │    rows: flex align center gap 12, padding 12, bg bg.selected, border 1px border.selected, radius 8, width 100%
│ │  │   │      radio (selected style, see below) + span "Cast Stream {i+1}" 13/600 text.primary
│ │  │   └ pipewire selected ? row flex gap 4 width 100%: Button secondary md flex 1 "Select new sets of windows" (request_screencast)
│ │  │                                           + Button secondary md style {width 40, padding 0, flex-shrink 0} "+" (add_screencast)
│ │  │                       : Button secondary md "+ Add new Screen/Window"
│ │  ├ [tab screens OR (!wayland && no windows && no cameras)] monitor rows                (:652-712)
│ │  ├ [tab windows] window rows (onMouseEnter → hoveredWindow + aspect; onMouseLeave → null); minimized rows opacity 0.5 (:714-783)
│ │  └ [tab cameras] camera rows (click selects single)                                    (:785-833)
│ └ AnimatePresence: [(screens && monitors>1) || (windows && windows>1)] shift hint pill motion.div   (:836-867)
│      absolute bottom 0 (`'-0px'`), right 16, bg rgba(0,0,0,0.7), backdrop-filter blur(8px), color #fff,
│      padding "6px 10px", radius 8, font 11, flex align center, pointer-events none,
│      box-shadow "0 4px 12px rgba(0,0,0,0.15)", z 10
│      content: kbd "shift" (border 1px solid rgba(255,255,255,0.3), bg rgba(255,255,255,0.1), radius 4, padding "2px 4px",
│               margin "0 4px 0 0", font inherit) + "+" + kbd "click" (margin "0 6px 0 4px") + "to select multiple"
└ div flex-shrink 0 → [selected.length>0] Button primary lg fullWidth style mt 16 {submitLabel}     (:871-886)
```
Source row (shared shape, `:657-710`):
```
motion.button  whileTap="active" initial="idle"; flex align center gap 12, padding 12, bg transparent, border none,
               outline none, user-select none, radius 8, cursor pointer, text-align left, width 100%, position relative
├ bg motion.div  absolute inset 0, radius 8, z 0, variants idle scale 1 / active scale 0.98, spring 1500/60
│   background: selected ? bg.selected : bg.surface; border 1px (selected ? border.selected : border.default)
└ content div flex align center gap 12 width 100% relative z1
   ├ radio 18×18 radius 50%, border 2px solid (selected ? icon.selected : text.quaternary), flex-shrink 0, centered
   │   └ [selected] dot 8×8 radius 50% bg icon.selected
   └ text col flex 1, flex column, gap 2, min-width 0
      ├ title span 13/600 text.primary, flex align center gap 8
      │    monitor: {m.name} + [isPrimary] badge "Primary" (font 10, 600, color badge.primaryText, bg badge.primaryBg, padding "1px 6px", radius 6)
      │    window: {appName || title} + [isMinimized] badge "Minimized" (font 10, 500, color text.secondary, bg invalid `${var}26`→none, padding "1px 6px", radius 6)
      │    camera: {label || `Camera ${index+1}`} (no subtitle row)
      └ subtitle span 11 text.tertiary ellipsis nowrap
           monitor: `${width}x${height}` + (scaleFactor>1 ? ` @ ${scaleFactor}x` : "")
           window: (title && appName ? title + " — " : "") + `${width}x${height}`   (U+2014 em dash)
```
PreviewImage (`:84-134`): uses `useScreenPreview(source, 1)` (1 fps, live). No URL → `motion.div layout` flex 1 centered → Spinner sm. Else `motion.img layout` (+layoutId) 100%×100%, object-fit `isMulti ? cover : contain`, multi: flex 1 position relative; single: position absolute inset 0; alt "Preview".

Scroll masks recomputed on scroll, on resize, and 10 ms after source load (`setTimeout(handleScroll, 10)`, `:272,:281`).

---

### 3A.10 DesktopRecorder (`D/components/DesktopRecorder.tsx`)

A pure view over core `RecorderState` (`D/recorder.ts:78-98`). Lifecycle: waits for `useRecorderState().ready` and `camera.ready`, then `invoke("recorder_open", {token, apiBaseUrl, sources})`; unmount → `invoke("recorder_close")` (`:130-138`). Navigates to session when `phase==="finished"` (`:183-185`). Sets capture guard from `state.capturing`. Preview: `listen("capture-preview-frame", {previewBase64, previewWidth, previewHeight})` → JPEG blob URL (`:157-180`). Clock: `useRecorderClock(state.clock)` (rAF loop; see C-anim and 3A.11).

Hidden camera `<video autoPlay muted playsInline>` (absolute 0×0 opacity 0) is in the DOM in every state when source is a camera (`:258-266`).

**Loading** (`!state || phase==="loading"`, `:269-288`):
```
div flex column, height 100%, max-width 480, margin 0 auto, padding 16, border-box
├ div flex-shrink 0, mb 16 → Skeleton h56 radius 8
├ div flex 1 → Skeleton aspect 16/9 radius 10
└ div flex-shrink 0, flex gap 12, mt 16 → 2× Skeleton h48 radius 10 flex 1
```
**loadError** (`:290-302`): `PageContainer centered` → `ErrorDisplay variant="page" title="Session Error" error={sessionError || "Unknown error"} action={{label: isMacOS ? "← Gallery" : "Back to Gallery", onClick: onBack}}`.
**finished** (`:305-312`): PageContainer centered → Spinner md.

**Active** (`:325-541`):
```
div flex column, height 100%, max-width 480, margin 0 auto, padding 16, border-box
├ Status card  flex-shrink 0, flex align center space-between, padding "12px 20px", bg bg.surface, radius 8,
│              border 1px border.default, mb 16                                                  (:332-369)
│  ├ left flex align center gap 8
│  │  ├ [controlMode recording] dot 10×10 radius 50% bg #ef4444, animation "pulse 1.5s ease-in-out infinite", flex-shrink 0
│  │  ├ [controlMode paused] span color text.tertiary, inline-flex: svg 14×14 fill currentColor, rects (x7 y5 w4 h14 rx1), (x13 y5 w4 h14 rx1)
│  │  └ timer span  font 32 (fontSize.timer), weight 700, font-variant-numeric tabular-nums, text.primary: formatTime(displaySeconds)
│  └ right span 13 text.secondary: `${captureCount} ${captureCount === 1 ? "capture" : "captures"}`
├ Preview area  flex 1, min-height 0, flex, direction (camera ? column : row),
│               gap = (!camera && sources>1 && !previewUrl) ? 4 : 0, mb 16                         (:375-413)
│  ├ previewUrl (core frames) → div relative radius 10 overflow hidden bg sunken border 1px border.default flex 1 min 0
│  │     img 100% contain block alt "Screen preview";
│  │     label span absolute bottom 6 right 6, font 11, color badge.overlayText, bg badge.overlayBg, padding "2px 6px", radius 6:
│  │       (windowFocused && state.capturing) ? "Preview" : "Latest capture"
│  └ else per source RecorderPreviewItem (live = windowFocused && !camera): useScreenPreview(src, 1 fps, live)
│        no url → div flex 1 min 0 radius 10 bg sunken border, aspect 16/9 if single, centered Spinner sm
│        url → same frame; img object-fit (multi ? cover : contain); [single] label "Preview" | "Latest capture"
├ [stale && !captureError] warning div padding "8px 12px", bg "#f59e0b1a", border 1px solid #f59e0b, radius 8, mb 12,
│      font 12, color #f59e0b: "Screenshots haven't been captured in a while. Your recording may not be saving."   (:415-427)
├ [captureError] ErrorDisplay banner error reassurance="Your earlier progress is saved. Restart the app if this keeps happening."
│      action {label "Details"} → CaptureErrorDialog; onCopy → clipboard getReport(error)              (:429-437)
├ [actionError && !captureError] ErrorDisplay banner reassurance="The recording is unchanged — try again." onCopy  (:439-446)
├ [captureErrorDetail] <CaptureErrorDialog error fallbackTitle="The recording hit a capture error" onDismiss/>   (:448-454)
├ Buttons row flex-shrink 0, flex gap 12                                                          (:457-511)
│  recording: Button warning lg fullWidth flex 1 "Pause" loading={phase==="pausing"} disabled={stopLoading||prompting}
│             Button danger lg fullWidth flex 1 "Stop" loading={stopLoading} disabled={pauseLoading||stopLoading||prompting}
│  paused:    Button success lg fullWidth flex 1 "Resume" loading={phase==="resuming"} disabled={stopLoading||prompting||!!sourceLost}
│             Button danger lg "Stop" loading={stopLoading} disabled={resumeLoading||stopLoading||prompting}
├ [prompting] <NamingModal tooShort={state.tooShortToCompile} loading={phase==="stopFinishing"} onConfirm onEditAndSave onResume/>
└ [sourceLost && !prompting && !captureErrorDetail] <SourceLostPrompt loss onReselect onStop onDetails? stopping/>
```
`stopLoading = phase ∈ {stopPausing, stopFinishing}`; `prompting = phase ∈ {stopPrompting, stopFinishing}` (`D/recorder.ts:152-154`); `captureStale = state.stale && !captureError` (`:323`).
Actions: Pause → `recorder_pause`; Resume → `recorder_resume`; Stop → `recorder_begin_stop`; modal "keep recording" → if sourceLost → back to picker else `recorder_cancel_stop`; confirm → `recorder_finish_stop {name, edit}` → if outcome.edit open editor window (`:209-253`).
Phases: `loading | loadError | recording | pausing | paused | resuming | stopPausing | stopPrompting | stopFinishing | finished`; controlMode `recording | paused` (`D/recorder.ts:30-42`).
Recorder error messages (`D/recorder.ts:123-138`): noRecording "No recording is open."; invalidPhase "That doesn't apply right now ({phase})."; sourceLost "The capture source is gone — pick one again."; api/other → message.

---

### 3A.11 The timer

- `formatTime(s)` (`R/hooks/useSessionTimer.ts:219-227`): `h>0 ? "h:mm:ss" : "m:ss"` (minutes not padded when h=0, e.g. `0:07`, `12:30`, `1:02:03`).
- `deriveDisplaySeconds(base, anchorAt, isActive, now)` (`R/hooks/useSessionTimer.ts:55-64`): `!isActive ? base : base + min(MAX_INTERPOLATION_S, max(0, floor((now - anchorAt)/1000)))`.
- `MAX_INTERPOLATION_S = floor(SCREENSHOT_INTERVAL_MS/1000) + TIMER_INTERPOLATION_SLACK_S = 60 + 15 = 75` (`R/hooks/useSessionTimer.ts:18-19`, `packages/shared/src/constants.ts:25,188`).
- `deriveRecorderDisplaySeconds` applies hold: `clockNow = holdAtUnixMs != null ? min(now, holdAtUnixMs) : now` (`D/recorder.ts:146-149`). Note the core's `maxInterpolationSeconds` field is passed through but the JS derivation uses the constant 75.
- `useRecorderClock` (`D/recorder.ts:192-228`): requestAnimationFrame loop while `running && hold == null`; re-renders only when the integer second changes; a held/stopped clock renders once. No digit animation — plain text swap (tabular nums).

---

### 3A.12 ProgramPanel (`D/components/ProgramPanel.tsx`, logic `D/programPanel.ts`)

vaul `Drawer` sheet hosting a sandboxed iframe of the session's `panelUrl`.

Constants (`D/programPanel.ts`): `PANEL_SANDBOX = "allow-scripts allow-forms allow-same-origin allow-popups"` (:66); `PANEL_MIN_HEIGHT = 220` (:78); `PANEL_SANITY_MAX_HEIGHT = 6000` (:84); `PANEL_HEIGHT_SLACK = 2` (:97); `PANEL_LOAD_TIMEOUT_MS = 12_000` (:99); `PANEL_READY_GRACE_MS = 4000` (:113). State persisted in localStorage `lookout-panel-state` as `{token: "dismissed"|"done"}` ("pending" = absent) (:31, :211-240).
URL acceptability (:125-152): reject same origin/host as app, `tauri.localhost`, `*.localhost`; accept https; http only for `localhost`/`127.0.0.1`.
Messages (postMessage from iframe, origin AND source checked, :172-205): `lookout:ready`, `lookout:resize {height}` (clamped 220..6000, rounded), `lookout:done`, `lookout:cancel`.
iframe src gets `?lookout_theme=light|dark` from `data-theme` or media query (`ProgramPanel.tsx:44-48, :91-99`).

Box tree (`:260-549`):
```
clip layer div (portal container)  position fixed, top/left/right/bottom = #root's inset from window (measured, on resize),
   border-radius = #root's computed border-top-left-radius, overflow hidden, z 9998, transform translateZ(0), pointer-events none
└ Drawer.Root open onOpenChange(false → requestClose("dismiss")) onAnimationEnd(isOpen → if !isOpen && closing: onDone|onDismiss)
   └ Drawer.Portal container=clip
      ├ Drawer.Overlay  position absolute inset 0, background backdropFill, z 1, pointer-events auto   (:298-312)
      │    backdropFill = Windows: "linear-gradient(to bottom, transparent 0%, var(--color-modal-backdrop) 100%)"; else colors.bg.backdrop (D/backdrop.ts:15-17)
      └ Drawer.Content aria-label "{programLabel} panel"  absolute left 0 right 0 bottom 0, z 2, pointer-events auto,
           flex column, outline none, bg colors.bg.panel, border-top-left/right radius 14, border 1px border.default,
           border-bottom: (inset from window ? 1px : "none"), overflow hidden                          (:313-334)
         ├ header div padding "8px 12px", flex-shrink 0
         │  ├ grabber 36×4 radius 2 bg colors.border.hover, margin 0 auto
         │  └ row flex align center gap 8, mt 8
         │     ├ [iconUrl && !failed] img 18×18 object-fit contain radius 5, alt "" (onError hides)
         │     ├ Drawer.Title {programLabel} 13/600 text.primary, margin 0, flex-shrink 0
         │     ├ span origin without "http(s)://"  flex 1, min-width 0, font 11, text.quaternary, ellipsis, nowrap, text-align right
         │     └ close button aria/title "Close" 22×22 inline-flex centered, padding 0, radius 50%, border none,
         │         bg colors.bg.selected, color text.secondary, cursor pointer → XIcon size 12 weight bold
         ├ body motion.div data-vaul-no-drag, initial false, animate {height: height + 2}, spring 420/34/0.7,
         │    position relative, flex-shrink 0, overflow-y auto, overflow-x hidden, scrollbar-width none        (:431-455)
         │  ├ broken → absolute inset 0, flex column centered, gap 12, padding 20, text-align center:
         │  │     WarningCircleIcon size 26 weight fill color text.tertiary;
         │  │     div 12 text.secondary line-height 1.5: "{programLabel} couldn't load here. You can finish up in your browser instead."
         │  │     Button primary sm "Open in browser" → open_external_url(fallbackUrl || url) then close (dismiss)
         │  └ else iframe data-vaul-no-drag src=frameSrc title "{programLabel} panel" sandbox=PANEL_SANDBOX
         │        block, width 100%, height height+2, border none, overflow hidden, bg transparent, color-scheme normal
         │        + loading cover motion.div absolute inset 0, flex centered, bg colors.bg.panel,
         │          animate opacity revealed ? 0 : 1 (0.18s easeOut), pointer-events revealed ? none : auto → Spinner md
         └ spacer div height 10 (radii.lg), flex-shrink 0
```
`revealed = ready || graceElapsed(4000 ms after mount)`; `broken` after 12 000 ms without iframe load, or on iframe error, or invalid origin. All closes (done, cancel message, X, swipe, Escape, overlay click) go through `requestClose` → `open=false` → vaul exit animation → `onAnimationEnd(false)` → host callback.

---

### 3A.13 PanelPrompt (`D/components/PanelPrompt.tsx`)
Rendered in SessionDetail's `belowVideo` slot.
```
div flex align flex-start, gap 12, padding 12, mb 16, radius 10, border 1px border.default, bg bg.surface
├ icon box 28×28 radius 8 bg bg.selected flex centered flex-shrink 0 color text.secondary → InfoIcon size 16 weight fill
└ div flex 1 min-width 0
   ├ div 13/500 text.primary: "{programLabel} needs a few details"
   ├ div 12 text.tertiary line-height 1.5 mt 2: "You closed this earlier. Your timelapse is saved either way, but {programLabel} asked for some information before it can finish up."
   └ div mt 12 → Button secondary sm "Continue" → onOpen (reopens ProgramPanel)
```

### 3A.14 OpenInProgram (`D/components/OpenInProgram.tsx`)
Rendered in SessionDetail's `titleAction` slot (right end of the name row).
`button title "Open in {label}"` inline-flex align center gap 4, max-width 180, padding "4px 4px", radius 6, border none, bg transparent, color text.secondary, font inherit 11/500, cursor pointer, nowrap → span ellipsis "Open in {programLabel}" + **ArrowSquareOutIcon size 12 weight bold** flex-shrink 0. Click → `invoke("open_external_url", {url})`. No hover/active styling.

### 3A.15 SourceLostPrompt (`D/components/SourceLostPrompt.tsx`)
`Overlay label={title} width="min(420px, 92vw)"` (no onDismiss — cannot be dismissed by Escape/backdrop).
title = `loss.reason === "revoked" ? "Screen sharing was stopped" : "Lost the screen being captured"`.
```
div flex column padding 24 gap 16
├ div
│  ├ h2 {title} margin 0, 18/600 text.primary
│  └ p margin "8px 0 0 0", 14, line-height 1.5, text.secondary:
│     "{loss.message} Pick a source again to carry on recording — everything captured so far is saved, and the session keeps its time."
└ div flex column gap 8
   ├ Button primary lg fullWidth "Select source again" disabled={stopping} → onReselect
   └ row flex gap 8
      ├ Button secondary md flex 1 "Stop recording" loading={stopping} → onStop (recorder_begin_stop)
      └ [onDetails] Button ghost md flex-shrink 0 "Details" disabled={stopping}
```

---

### 3A.16 SessionDetail (route `session`) (`R/components/SessionDetail.tsx`)

Desktop props: `onEdit` (opens editor window, so inline editor Overlay is never used on desktop), `showBack={!isLinux}`, `belowVideo`, `titleAction`, `onArchive`.
Data: `getSession()` once (name, createdAt); `getStatus()` on mount and every **3000 ms** while status ∈ {stopped, compiling} (`:305-314`); fetches `getVideo()` once complete. `onRecordingFinished` once per mount when status ∈ {stopped, compiling, complete} and not held; `onComplete` once on observed transition stopped|compiling → complete (`:253-303`).

```
div padding 16
├ header row flex space-between align center mb 16
│  ├ [onBack && showBack] Button secondary sm style cardButtonStyle "← Back"  (&larr; Back on every platform here)
│  └ div flex gap 8 → [onArchive] Button secondary sm style cardButtonStyle "Archive"
├ [error] ErrorDisplay banner title "Error"
├ [!status && !error] SessionDetailSkeleton
├ [status && inHold && !editing] HoldPanel (Card padding 16, mb 16)                                      (:25-124)
│   row flex align center gap 16: [!editable] ProgressRing size 48 stroke 4 (progress) ;
│     text: title 14/700 text.primary: editable ? "Ready to review" : "Preparing your timelapse…"
│           body 13 text.secondary mt 4: editable ? "Cut out anything you don't want to share, then save. Nothing is published until you do."
│                                                 : "Your recording is compiling. You'll be able to trim it in a moment."
│           + " " + span text.tertiary "If you close Lookout, it publishes as recorded."
│   buttons flex gap 8 mt 16: Button primary md "Edit & save" disabled={!editable||publishing};
│                             Button secondary md "Publish as recorded" loading={publishing} → client.applyCuts()
│   progress: real `status.progress` when present (monotonic max), else estimate `1 - exp(-2.2 * elapsed/30000)` ticked every 250 ms (R/hooks/buildProgress.ts:36-39)
│   holds an edit lease (heartbeat every 30 s, EDIT_HEARTBEAT_SECONDS) while not publishing
└ [status]
   ├ [!inHold] div mb 16 radius 10 overflow hidden → ProcessingState                                      (:399-407)
   │    complete+videoUrl → 16/9 box bg sunken → VideoPlayer (@videojs/react skin; mute/volume hidden; PiP hidden; fullscreen hidden on Linux; radius 8)
   │    failed → 16/9 box flex column centered gap 12 bg sunken: p "Compilation failed" 14/700 #ef4444; p "It will be retried automatically." 13 text.secondary
   │    else → Spinner lg (40) + p 14/600 text.primary (complete ? "Loading video..." : "Compiling your timelapse...") + p 13 text.secondary "Tracked time: {formatTrackedTime}"
   │    VideoPlayer playback error overlay: "Can't play this timelapse", detail text, "Your timelapse saved successfully. You can download the file and play it in another app." (R/components/VideoPlayer.tsx:107-118; CSS VideoPlayer.css:27-65: bg rgba(11,13,18,0.92), color #f4f5f7, h3 18/600, p 14 lh 1.5 #c8ccd4, note 13 #9aa0aa, link #6ea8ff)
   ├ {belowVideo}  (PanelPrompt)
   ├ [sessionInfo] name block mb 16                                                                     (:412-562)
   │  ├ row flex align center gap 4 height 32
   │  │  ├ form display grid align center margin 0 (submit → client.rename(trimmed) if changed; alert "Failed to rename session." (HttpError) / "Error renaming session.")
   │  │  │  ├ sizer motion.span grid-area 1/1, visibility hidden, white-space pre, 16/700, padding animates "0 16px" (renaming) ↔ "0 8px 0 0"
   │  │  │  └ motion.input grid-area 1/1, readOnly unless renaming, size 1, min-width 0, height 32, 16/700 text.primary,
   │  │  │      border 1px solid (animated color), radius 8, outline none, cursor text|default, transform-origin left center
   │  │  │      animate: padding "0 8px"|"0", width "max(100%, 300px)"|"100%", bg bg.surface|transparent, border border.selected|transparent
   │  │  │      double-click → start rename; Escape → cancel; blur → cancel; Enter submits
   │  │  ├ icon slot 24×24 flex: AnimatePresence [!isRenamingAnim] motion.button title "Rename session" (pencil svg 12×12 fill currentColor
   │  │  │    path "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04c.39-.39.39-1.02 0-1.41l-2.34-2.34c-.39-.39-1.02-.39-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z"),
   │  │  │    bg none, border none, padding 0, margin −5, color text.tertiary; mousedown → transform scale(0.9), up/leave → none, CSS transition "transform 0.1s ease-in-out"
   │  │  └ [titleAction] div margin-left auto flex align center → OpenInProgram
   │  └ date motion.div 11 text.tertiary mt 2, y −4 (static, animates y to −4 when not renaming, 0 when renaming):
   │       toLocaleDateString(undefined, {month:"short", day:"numeric", year:"numeric"})
   └ stats row flex gap 16 justify center
      ├ Card padding "12px 24px" flex 1 flex column align center gap 4:
      │    span 18/700 text.primary height 32 flex align center {formatTrackedTime}; span 11 text.tertiary "Tracked time"
      └ Card same: div h32 → Badge status inline lg; span 11 text.tertiary "Status"
```
isRenamingAnim: true immediately on rename start; false 600 ms after rename ends (`setTimeout`, `:208-220`) so the pencil reappears after layout settles.

---

### 3A.17 Hooks: UI-relevant behaviour

- **useScreenPreview(source, targetFps=20, live=true)** (`D/hooks/useScreenPreview.ts`): fetches `lookout-preview://localhost/{type}/{id}?maxWidth=854&maxHeight=480&jpegQuality=65&t={now}` (Windows: `http://lookout-preview.localhost/...`) (:217-219); interval `max(16, floor(1000/fps))` ms minus fetch time (:182, :207-214); preloads into `Image` before swapping (no blink); module-global per-source cache shared by all subscribers, old blob URLs revoked; parks while `document.hidden` until `visibilitychange`; `live=false` → fetch at most one frame (only if nothing cached). Callers: SourcePicker PreviewImage (1 fps), hover card (5 fps, live=false → single frame), RecorderPreviewItem (1 fps, live = focused && !camera).
- **useWindowFocus** (`D/hooks/useWindowFocus.ts`): `document.hasFocus()` + window focus/blur.
- **useRecorderNotices(capturing)** (`D/hooks/useRecorderNotices.ts`): requests notification permission when capture starts (also at app launch, `D/App.tsx:452-454`); `listen("recorder-notice", {title, body})` → native notification via `@tauri-apps/plugin-notification` `sendNotification`.
- **useCameraBackend(active)** (`D/hooks/useCameraBackend.ts`): listens `camera-start-request {requestId, deviceId}` → waits one rAF, starts stream into hidden video, waits for dimensions, `invoke("camera_request_done", {requestId, error})`; `camera-frame-request {requestId, deviceId}` → JPEG frame → `invoke("camera_frame_delivered", {requestId, base64, width, height, error})` (error string "no camera frame — the stream isn't ready or has ended"); `camera-stop` → stop stream. `ready` gates `recorder_open`.
- **enumerateCameras** (`D/hooks/useCameraCapture.ts:17-50`): if all labels empty, briefly opens getUserMedia to unlock labels; fallback label `Camera {n}` in UI.
- **useBlacklistedApps** (`D/hooks/useBlacklistedApps.ts`): localStorage `lookout-blacklisted-apps` (string[], sorted on add); syncs via `invoke("set_blacklisted_apps", {apps})` on mount and every change. Mounted in MainWindowApp (`D/App.tsx:441`) so the list reaches Rust at launch.

---

### 3A.18 Strings index (this part)
"Something went wrong", "Copy Error Report", "Copied!", "Try Again", "Press ` to open the debug log. Copy and send to the developer.", "Start from link", "Are you sure you want to archive this session?", "Archive Session", "Open", "Open in Editor", "Archive", "Linking failed — you can retry from the + menu.", "Lookout", "Lookout v{version}", "v{version}", "Editing in another window", "Settings", "New Timelapse", "Recording", "Timelapse", "Start", "Starting {label}…", "This program", "program", "Close", "Back", "← Back", "← Gallery", "Gallery", "Session Error", "Unknown error", "Session Already {Complete|Compiling|Failed|Stopped}", "This session is no longer recordable.", "View Timelapse", "Select source", "What should Lookout capture?", "Stop Session", "Resume Session", "Start Capture", "Failed to detect displays", "Retry", "Troubleshoot", "Detecting displays...", "Couldn't detect your displays", "Couldn't start the capture", "Starting camera preview...", "Select a source below", "Loading preview...", "Window hover preview", "Preview", "Screens", "Windows", "Cameras", "Cast", "Wayland requires explicit permission to capture screens or windows.", "Cast Stream {n}", "Select new sets of windows", "+", "+ Add new Screen/Window", "Primary", "Minimized", "Camera {n}", "shift", "click", "to select multiple", "Latest capture", "Screen preview", "capture"/"captures", "Screenshots haven't been captured in a while. Your recording may not be saving.", "Your earlier progress is saved. Restart the app if this keeps happening.", "Details", "The recording is unchanged — try again.", "The recording hit a capture error", "Pause", "Stop", "Resume", "← Gallery"/"Back to Gallery", "Copy Error", "Copy", "×", "{programLabel} panel", "{programLabel} couldn't load here. You can finish up in your browser instead.", "Open in browser", "{programLabel} needs a few details", "You closed this earlier. Your timelapse is saved either way, but {programLabel} asked for some information before it can finish up.", "Continue", "Open in {programLabel}", "Screen sharing was stopped", "Lost the screen being captured", "{message} Pick a source again to carry on recording — everything captured so far is saved, and the session keeps its time.", "Select source again", "Stop recording", "Your Timelapses", "No timelapses yet", "Start a recording session to see it here.", "Timelapse thumbnail", "{tracked} · {date}", "Error", "Ready to review", "Preparing your timelapse…", "Cut out anything you don't want to share, then save. Nothing is published until you do.", "Your recording is compiling. You'll be able to trim it in a moment.", "If you close Lookout, it publishes as recorded.", "Edit & save", "Publish as recorded", "Compilation failed", "It will be retried automatically.", "Loading video...", "Compiling your timelapse...", "Tracked time: {t}", "No video available", "Rename session", "Failed to rename session.", "Error renaming session.", "Tracked time", "Status", "Can't play this timelapse", status labels (Pending, Recording, Paused, Processing, Compiling, Complete, Failed), recorder error messages (3A.10).

## 3B. Secondary screens & components

Path legend for this part (all repo-relative):
`C/` = `clients/desktop/src/components/`, `S/` = `clients/desktop/src/`, `H/` = `clients/desktop/src/hooks/`, `UI/` = `clients/react/src/ui/`.

### 3B.0 Token cheat-sheet used below (resolved from `UI/theme.ts`)

Values are written `token (dark / light)`. Dark is the default `:root` block (`UI/theme.ts:7-68`); light applies under `@media (prefers-color-scheme: light) :root:not([data-theme="dark"])` (`UI/theme.ts:69-113`) or `:root[data-theme="light"]` (`UI/theme.ts:114-156`).

| Token | Dark | Light | Ref |
|---|---|---|---|
| `colors.bg.panel` | `#1e1e1e` | `#ffffff` | theme.ts:21 / :72 |
| `colors.bg.backdrop` | `rgba(0,0,0,0.55)` | `rgba(0,0,0,0.35)` | :22 / :73 |
| `colors.bg.surface` | `rgba(255,255,255,0.08)` | `#ffffff` | :23 / :74 |
| `colors.bg.sunken` | `rgba(255,255,255,0.04)` | `rgba(0,0,0,0.04)` | :24 / :75 |
| `colors.bg.selected` | `rgba(255,255,255,0.12)` | `rgba(0,0,0,0.1)` | :33 / :84 |
| `colors.text.primary` | `#ffffff` | `rgba(0,0,0,0.85)` | :25 / :76 |
| `colors.text.inverse` | `#000000` | `#ffffff` | :26 / :77 |
| `colors.text.secondary` | `rgba(255,255,255,0.7)` | `rgba(0,0,0,0.65)` | :27 / :78 |
| `colors.text.tertiary` | `rgba(255,255,255,0.5)` | `rgba(0,0,0,0.45)` | :28 / :79 |
| `colors.text.quaternary` | `rgba(255,255,255,0.28)` | `rgba(0,0,0,0.25)` | :29 / :80 |
| `colors.text.error` | `#fca5a5` | `#ef4444` | :30 / :81 |
| `colors.border.default` | `rgba(255,255,255,0.12)` | `rgba(0,0,0,0.12)` | :31 / :82 |
| `colors.border.hover` | `rgba(255,255,255,0.22)` | `rgba(0,0,0,0.22)` | :32 / :83 |
| `colors.badge.overlayBg` | `rgba(0,0,0,0.7)` | `#000000` | :43 / :94 |
| `colors.badge.overlayText` | `#ffffff` | `#ffffff` | :44 / :95 |
| `colors.accent.base` | `#3b82f6` | `#3b82f6` | :64 / :108 |
| `colors.accent.hover` | `color-mix(in oklab, var(--color-accent) 88%, black)` (fallback `#2f6fd0`) | same | :65-66 |
| `colors.accent.on` | `#ffffff` | `#ffffff` | :67 |
| `colors.spinner.base` / `.track` | `rgba(255,255,255,0.1)` / `rgba(255,255,255,0.8)` | `rgba(0,0,0,0.1)` / `rgba(0,0,0,0.8)` | :37-38 / :88-89 |
| `colors.status.success/info/warning/danger` | `#22c55e` / `#3b82f6` / `#f59e0b` / `#ef4444` (theme-independent) | | :192-197 |

`spacing` xs 4, sm 8, md 12, lg 16, xl 20, xxl 24, xxxl 32 (`UI/theme.ts:201`). `radii` sm 6, md 8, lg 10 (`:202`). `fontSize` xs 11, sm 12, md 13, lg 14, xl 16, xxl 18, heading 20, display 24, timer 32 (`:203`). `fontWeight` normal 400, medium 500, semibold 600, bold 700 (`:204`).

`backdropFill` (`S/backdrop.ts:15-17`): on Windows `linear-gradient(to bottom, transparent 0%, ${colors.bg.backdrop} 100%)`; macOS/Linux flat `colors.bg.backdrop`.

#### Shared primitives these screens use (summary; full spec belongs to the primitives section)

- **Button** (`UI/Button.tsx`). Outer `motion.button`: `position:relative; fontWeight:600; borderRadius: style.borderRadius ?? 8; cursor: disabled?"not-allowed":"pointer"; opacity: disabled?0.6:1; display:inline-flex; align/justify center; width: fullWidth?"100%":auto; padding:0; border:1px solid transparent; background transparent` (`:67-82`). Inner background layer = `Squircle cornerRadius=(borderRadius ?? 8) cornerSmoothing=0.7`, `position:absolute; inset:-1` (`:85-100`), animated between variants idle/hover/active with `transition {type:"spring", stiffness:1500, damping:60}` (`:92`), `active` = `scale:0.96` (`:90`) and CSS `transition: "opacity 0.15s, background 0.15s, border-color 0.15s"` (`:98`). Label `span`: `display:inline-flex; gap:8; width:100%` + size padding (`:102-113`). Sizes: sm `6px 12px`/12px, md `8px 16px`/13px, lg `12px 24px`/15px (`:24-28`). Variants (`:15-22`): primary bg accent / text accent.on; success `#22c55e` / `#fff`; danger `#ef4444` / `#fff`; warning `#f59e0b` / `#000`; secondary transparent / text.secondary / border `1px solid border.hover`; ghost transparent / text.secondary / transparent border. Hover bg (`:46-57`): ghost→bg.selected, secondary→bg.surface, primary→accent.hover, others unchanged. **If caller passes `style.background`/`border`, they become BOTH idle and hover values** (so `cardButtonStyle` buttons have no hover color change, only the 0.96 press). Loading: `Spinner size="sm"` (16px) colored `#000` for warning else `#fff`, before children (`:114`). Hover/tap variants are disabled when `disabled||loading` (`:63-64`).
- **Card** (`UI/Card.tsx:134-146`): bg.surface, `1px solid border.default`, radius 10, overflow hidden, `padding` prop, `height:100%`, flex column; caller style merged (when not clickable).
- **Overlay** (`UI/Overlay.tsx`): portal to body; backdrop div `data-lookout-overlay`, `position:fixed; inset:0; background: colors.bg.backdrop; flex center; padding:20; zIndex:9999` (`:238-252`); pointer-down on backdrop itself → `onDismiss` (`:253-255`); Escape keydown → `onDismiss` (`:226-233`); body `overflow:hidden` while mounted, panel focused on mount (`:216-224`). Panel `motion.div role=dialog aria-modal`: `width` prop, `maxWidth/maxHeight:100%`, flex column, bg.panel, `1px solid border.default`, `borderRadius:14`, `boxShadow: 0 24px 64px rgba(0,0,0,0.45)`, overflow hidden (`:269-283`). Enter animation see D-anim. No exit animation (unmounts instantly). NOTE: Overlay uses the flat token, NOT `backdropFill` (no Windows gradient).
- **Spinner** (`UI/Spinner.tsx`): svg sizes sm 16 / md 24 / lg 40; two r=10 circles strokeWidth 2: base `spinner.base`, arc `color || spinner.track`, `strokeDasharray 62.83`, `strokeDashoffset 47.12` (a 25% arc), round cap; CSS `animation: spin 1s linear infinite` (`:313-316`) with `@keyframes spin { from rotate(0deg) to rotate(360deg) }` (`clients/desktop/index.html:64`).
- **PageContainer** (`UI/PageContainer.tsx:350-363`): `maxWidth` default 640, `margin:0 auto; padding:16`; `centered` → flex column, center/center, `minHeight:100%`, `flex:1`.
- **PageLayout** (`C/PageLayout.tsx`) — desktop page scaffold used by AddSessionPage & PermissionScreen:
  ```
  div  flex column; height 100%; maxWidth 480; margin 0 auto; padding 16; border-box      (:43-46)
  ├─ [onBack && !isLinux] div flexShrink 0                                             (:48-49)
  │    Button secondary sm style=cardButtonStyle                                        (:50)
  │      macOS: <span>"← Back"</span> (&larr;)                                          (:51-52)
  │      else : span inline-flex center gap 4: svg 14x14 chevron path "m15 18-6-6 6-6",
  │             stroke currentColor width 2 round caps/joins + <span>"Back"</span>      (:54-59)
  ├─ div flex 1; flex column; justify center; align center; textAlign center; padding 0 24  (:66-70)
  │   ├─ [icon] div 72x72 circle; background `${colors.text.quaternary}20`; flex center;
  │   │         marginBottom 20; flexShrink 0                                           (:72-79)
  │   ├─ [title] h2 fontSize 20; weight 700; text.primary; margin 0; marginBottom 8     (:82-87)
  │   ├─ [subtitle] p 13px; text.secondary; margin 0; lineHeight 1.6; + marginBottom 16 if hint (:90-96)
  │   ├─ [hint] p 12px; text.tertiary; margin 0; lineHeight 1.5                        (:99-104)
  │   └─ children
  └─ [actions] div flexShrink 0; flex column; gap 12                                    (:111-113)
  ```
  `cardButtonStyle` = `{ background: colors.bg.surface, border: 1px solid colors.border.default, borderRadius: 10 }` (`:13-17`). On Linux, `usePublishHeaderNav(onBack ? {onBack} : null)` moves Back into the HeaderBar (`:40`).
  QUIRK: the icon circle's background is `"var(--color-text-quaternary)" + "20"` → invalid CSS, so the browser drops it; **the circle renders with no fill** (transparent) (`C/PageLayout.tsx:74`). Replicate as transparent.

---

### 3B.1 Sheet (`C/Sheet.tsx`) — vaul bottom-sheet chrome

Constants: `SHEET_INSET = spacing.xxl = 24` (`:8`), `SHEET_RADIUS = 24` (`:9`), `SHEET_CLOSE_SIZE = 34` (`:10`).
Props: `open`, `onClose`, `ariaLabel`, `hero?: ReactNode`, `children` (`:12-19`).

**Frame measuring** (`:31-55`): on layout and on `window.resize`, measures `#root` bounding rect and its computed `border-top-left-radius`; frame = `{top,left,right,bottom}` insets from the viewport (rounded, ≥0) + radius. This aligns the sheet to the app's own visual frame (on Linux CSD the window has a transparent shadow margin around `#root`).

**Presence** (`:63-74`): `present` becomes true when `open`; after `open` turns false it stays mounted for **600 ms** (`setTimeout 600`, comment "vaul's exit is 500ms") then unmounts. Returns `null` when not present.

**Tree**
```
Fragment
├─ div (clip layer)  position fixed; top/left/right/bottom = frame insets (0 if unknown);
│                    borderRadius = frame.radius; overflow hidden; zIndex 9998;
│                    transform translateZ(0) (makes it the containing block); pointerEvents none   (:81-95)
└─ [clip ready] Drawer.Root open={open} onOpenChange(next => !next && onClose())                   (:97-102)
     (all other vaul props default: direction "bottom", dismissible true, modal true, no snapPoints,
      shouldScaleBackground false, closeThreshold 0.25, handleOnly false)
   └─ Drawer.Portal container={clip}                                                              (:103)
      ├─ Drawer.Overlay  position absolute; inset 0; background backdropFill; zIndex 1;
      │                  pointerEvents auto  (NOT tagged data-lookout-overlay)                      (:104-114)
      └─ Drawer.Content aria-label={ariaLabel}  position absolute; left 24; right 24; bottom 0;
         │              paddingBottom 24; border-box; zIndex 2; pointerEvents auto; flex column;
         │              outline none; background transparent                                       (:119-135)
         └─ div (sheet)  position relative; flex column; background colors.bg.panel;
            │            border 1px solid colors.border.default; borderRadius 24; overflow hidden;
            │            boxShadow "0 24px 64px rgba(0, 0, 0, 0.45)"                               (:136-147)
            ├─ {hero}                                                                              (:148)
            ├─ button (close) aria-label "Close"; data-vaul-no-drag; position absolute; top 20;
            │         right 18 (spacing.lg + 2); 34x34; flex center; borderRadius 50%; border none;
            │         cursor pointer; padding 0; background colors.badge.overlayBg;
            │         color colors.badge.overlayText; zIndex 1                                     (:149-172)
            │    └─ XIcon (phosphor `X`) size 15, weight "bold"                                     (:173)
            └─ div (body) padding 24; flex column; alignItems flex-start; flexShrink 0               (:175-185)
                 └─ {children}
```
Close button has no hover/press styling. The sheet is height-to-content (no max-height). Dismissal paths: X button, overlay click, Escape (vaul/Radix Dialog default), drag down past 25% of height or fling velocity > 0.4 px/ms (vaul defaults, see D-anim). Drag is blocked on elements with `data-vaul-no-drag`.

### 3B.2 TipDrawer (`C/TipDrawer.tsx`, `S/tip.ts`, `H/useTip.ts`)

Props: `tip: Tip|null`, `open`, `onClose` (`:17-21`). Returns `null` if `!tip` (`:38`). Mounted permanently by App: `<TipDrawer tip={tipOverride ?? tip} open={tipOpen} onClose={() => setTipOpen(false)} />` (`S/App.tsx:1450`).

`Tip` shape (`S/tip.ts:16-26`): `{ id: string; badge: string|null; title: string; body: string; imageUrl: string|null; program: string|null; trigger: "any"|"deep_link"; requireUnlinked: boolean }`.

Constants: `HERO_HEIGHT = 210` (`:15`).

Tree (inside Sheet with `ariaLabel = tip.title`):
```
hero (only if heroUrl = imageUrl && !heroBroken):
  div height 210; flexShrink 0; background colors.bg.sunken; position relative               (:51-58)
  └─ img src=heroUrl alt="" display block; 100%x100%; objectFit cover;
         opacity heroLoaded?1:0; transition "opacity 220ms ease-out"; pointerEvents none;
         userSelect none. onLoad→loaded; onError→heroBroken (sheet becomes text-only)         (:59-77)
body children:
  [tip.badge] span inline-block; padding "3px 8px"; marginBottom 10; borderRadius 4;
       background colors.text.primary; color colors.text.inverse; fontSize 12; weight 600;
       letterSpacing -0.4px; lineHeight 1.15  → {tip.badge}                                   (:82-98)
  Drawer.Title  margin 0; fontSize 18; weight 600; letterSpacing -0.36px; text.primary;
       lineHeight 1.25; paddingRight = heroUrl ? 0 : 42 (34 + 8, to clear the X)  → {tip.title} (:100-112, :42)
  Drawer.Description margin "8px 0 0"; fontSize 14; text.secondary; lineHeight 1.45 → {tip.body} (:113-122)
```
Hero load state resets in a layout effect when the URL changes; if the image is already `complete` (preloaded), it is shown immediately with no fade (`:33-36`).

**Data/eligibility** — `useTip` (`H/useTip.ts`): fetches `fetchTip(API_BASE, {client:"lookout-desktop", version})` at mount and every `15*60_000` ms (`:12, :64-70`); stores `data.tip ?? null`; preloads + `decode()`s the hero image into a module-level map kept for the app's life (`:22-33`). Failures keep the previous value.
When it opens (App, `S/App.tsx:469-501, :1436`): `openTip(moment, token)` for moment `"source"` (RecordPage reports the SourcePicker is visible) and `"panel-done"` (ProgramPanel completion). Requires `shouldShowTip` (`S/tip.ts:95-144`): has tip AND token AND (tip.program → if trigger `"deep_link"`: `localStorage["lookout-deeplink-sessions"][program] === token`; if `requireUnlinked`: program not linked) AND moment record for tip.id either absent or bound to the same token AND moment not already used. On open, `markTipMoment` writes `localStorage["lookout-tip-moments"][tip.id] = {token, moments[]}` (`:147-155`).
Debug: `window.__tip.show(overrides?)`/`hide()`/`reset()`/`state(token?)` (`S/tip.ts:188-230`); sample tip strings: badge `"NEW!"`, title `"Sample tip title that runs about this long"`, body `"Sample body copy, roughly the length a real tip runs to, so the sheet is the height it will actually be in front of a user."` (`:162-171`). Return strings: `` `[tip] showing ${"published"|"sample"} tip — this does NOT consume a moment` ``, `"[tip] hidden"`, `"[tip] cleared impressions and deep-link records — reopen a timelapse through its deep link to replay the real flow"` (`:197-206`).

### 3B.3 UpdateDrawer (`C/UpdateDrawer.tsx`, `H/useLinuxUpdate.ts`, `S/updateCommand.ts`) — Linux only

Props `update: LinuxUpdate|null`, `open`, `onClose` (`:9-13`); `null` if no update (`:23`). App: `open={Boolean(linuxUpdate) && onMainPage && !tipOpen}`, `onClose={dismissLinuxUpdate}` (`S/App.tsx:1454-1458`) — i.e. it auto-opens on the main page, never over the tip sheet; closing persists dismissal of that version.

`LinuxUpdate` (`H/useLinuxUpdate.ts:14-21`): `{version, command: string|null, fallback: string|null, releasesUrl}`.

Tree (Sheet, `ariaLabel = "Lookout ${version} is available"`, no hero) (`:45`):
```
Drawer.Title  margin 0; 18px; weight 600; letterSpacing -0.36px; text.primary; lineHeight 1.25;
              paddingRight 42  → "A new version of Lookout ({version}) is out."               (:46-59)
Drawer.Description margin "8px 0 0"; 14px; text.secondary; lineHeight 1.45
   → command ? "On Linux, Lookout updates through your package manager. Run this to get it:" : fallback  (:60-71)
if command:
  div data-vaul-no-drag; flex; alignItems center; gap 8; width 100%; marginTop 16;
      padding "8px 12px"; background bg.sunken; border 1px solid border.default; radius 8      (:74-91)
  ├─ code flex 1; minWidth 0; 12px; text.primary;
  │       fontFamily "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
  │       whiteSpace pre-wrap; overflowWrap anywhere; lineHeight 1.5; user-select text; cursor text  (:92-112)
  └─ Button secondary sm data-vaul-no-drag style {flexShrink 0, gap 6}                          (:113-122)
        copied ? [CheckIcon 13 bold] "Copied" : [CopyIcon 13 (regular)] "Copy"
else:
  Button secondary sm data-vaul-no-drag style {marginTop 16} → "Open downloads"                  (:125-133)
```
Copy: `navigator.clipboard.writeText(command)`, `copied` for **1600 ms** (`:25-34`). Open downloads → `invoke("open_external_url", {url: releasesUrl})` (`:36-42`).

Commands/fallbacks (`S/updateCommand.ts:21-60`):
| manager | enrolled | command | fallback |
|---|---|---|---|
| apt | yes | `sudo apt update && sudo apt install lookout` | — |
| apt | no | — | "This copy was installed before Lookout had an apt repository. Download the latest .deb once, and every update after it arrives through apt." |
| rpm | yes | `sudo dnf upgrade lookout` | — |
| rpm | no | — | "This copy was installed before Lookout had a dnf repository. Download the latest .rpm once, and every update after it arrives through dnf." |
| pacman | yes | `sudo pacman -Syu` | — |
| pacman | no | — | "Add the Lookout repository to get updates, or download the latest package." |
| unknown | – | — | "Download the latest version to update." |

Check logic (`H/useLinuxUpdate.ts`): only if `isLinux`; fetch `https://github.com/hackclub/lookout/releases/latest/download/latest.json` (`cache:"no-store"`), compare `.version` vs `getVersion()`; skip if `localStorage["lookout_update_nag_dismissed"] === latest`; then `invoke<LinuxInstall>("linux_install_kind")` → `{manager:"apt"|"rpm"|"pacman"|"unknown", enrolled:boolean}`; repeat every 6 h (`:8-12, :45-72`). `dismiss()` writes the version to that key and clears state (`:74-77`). Releases URL `https://github.com/hackclub/lookout/releases/latest`.

### 3B.4 UpdatePill (`C/UpdatePill.tsx`, `H/useAppUpdate.ts`)

Props: `phase: UpdatePhase`, `onRestart`, `origin: "top"|"bottom"` (default `"top"`) (`:7-12`).
`UpdatePhase` (`H/useAppUpdate.ts:28-32`): `{state:"idle"} | {state:"downloading",version,progress(0-100 int)} | {state:"ready",version} | {state:"installing",version}`.

Placement (App, only `onMainPage`):
- macOS: inside the 32px-tall drag titlebar div, `justifyContent:flex-end; paddingRight:6` (`S/App.tsx:1383-1390`), origin top.
- Linux: in HeaderBar actions, before gallery actions (`S/App.tsx:1336-1341`), origin top.
- Windows: floating `position:absolute; bottom:8; left:8; zIndex:9999`, `origin="bottom"` (`S/App.tsx:1391-1397`).

Tree: `AnimatePresence` → when `state !== "idle"` a `motion.button` (`:62-64`):
```
motion.button  display inline-flex; alignItems center; gap 6; height 22; padding "0 10px";
               borderRadius 999; border 1px solid (solid ? transparent : border.default);
               background: solid ? (clickable&&hovered ? color-mix(in srgb, text.primary 19%, transparent)
                                                       : color-mix(in srgb, text.primary 16%, transparent))
                                 : colors.bg.surface;
               color: solid ? text.primary : text.secondary; fontSize 11; weight 500;
               fontFamily inherit; whiteSpace nowrap; cursor default;
               transition "background 0.15s ease, color 0.15s ease, border-color 0.15s ease";
               -webkit-app-region no-drag                                                     (:80-101)
   disabled = state !== "ready"; onClick onRestart; title tooltip (:73-79):
     downloading "Downloading v{version}" | installing "Installing v{version}" | ready "Restart to update to v{version}"
   solid = ready || installing (:59)
 downloading:
   ProgressRing (svg 12x12, viewBox 0 0 12 12, display block, flexShrink 0):
     track circle cx6 cy6 r5 stroke border.default width 1.5;
     progress circle r5 stroke currentColor width 1.5 round cap; dasharray c=2π·5≈31.416;
     dashoffset c·(1-progress/100); transform rotate(-90 6 6);
     style transition "stroke-dashoffset 0.2s ease"                                         (:15-36)
   span "Downloading Update…" + " " + NumberFlow value={progress} suffix="%" style {fontVariantNumeric: tabular-nums} (:106-113)
 ready/installing:
   PowerIcon: svg 11x11 viewBox 0 0 24 24, stroke currentColor width 2.5 round; paths "M12 2v10" and
     "M18.4 6.6a9 9 0 1 1-12.77.04"; display block; flexShrink 0; marginTop 0.5               (:38-45)
   span installing "Installing Update…" | ready "Restart to Complete Update"                  (:118-122)
```
Hover only changes background when `ready` (16% → 19% mix). Click on ready → `restart()` → `update.install()` then `relaunch()` (`H/useAppUpdate.ts:176-219, :361-370`).

Update engine behaviours (UI-relevant, `H/useAppUpdate.ts`): checks at launch and every 30 min (`:15, :350`); download progress = round(downloaded/total·100) (`:284-288`); skips first check if `localStorage["lookout_last_update_ts"]` is < 60 s old (`:10, :14, :309-314`); pending-update marker `localStorage["lookout_pending_update"] = {version, attempts, notified}` (`:12, :34-71`); auto-installs a previously-skipped version on launch (not Linux, max 2 attempts, held while capture active, retry every 30 s) (`:17-24, :226-265`). Failure dialog (Tauri `ask`, `kind:"warning"`) (`:84-102`):
- title `"Update couldn't be installed"`, okLabel `"Open Releases Page"`, cancelLabel `"Not Now"`;
- message `"Lookout couldn't install version {version}.\n\nYour sessions and settings are safe — the app will keep working on this version. You can install the update yourself from the releases page."` + optional `"\n\nDetails: {detail}"`; OK → `open_external_url` `https://github.com/hackclub/lookout/releases/latest`.
Dev-only `window.__updatePillDemo()` (`:147-166`): version `"9.9.9"`, every 80 ms progress += 1 + rand·4 until ≥100 → ready; clicking pill in demo → idle.

### 3B.5 NamingModal (`C/NamingModal.tsx`)

Props: `loading`, `onConfirm(name|null)`, `onEditAndSave(name|null)`, `onResume()`, `tooShort?=false` (`:6-20`). State `name` (""), `choice: "stop"|"edit"|null`. Rendered by DesktopRecorder when `prompting` (`C/DesktopRecorder.tsx:514-520`, `loading = phase==="stopFinishing"`, passes tooShort) and RecordPage (`C/RecordPage.tsx:282-287`, `loading=stopping`, no tooShort).
Focus: unless tooShort, `setTimeout(() => input.focus(), 50)` on mount (`:27-31`). No Escape handling, backdrop click does nothing.

```
div data-lookout-overlay  position fixed; inset 0; background backdropFill; flex center; zIndex 9999; padding 20   (:34-39)
└─ motion.div  width 100%; maxWidth 400   (enter animation, see D-anim)                                        (:40-48)
   └─ Card padding 24; style {width 100%, height auto, background var(--color-bg-panel), textAlign center,
          boxShadow "0 10px 40px rgba(0,0,0,0.5)"}  (+ Card's 1px border.default, radius 10)                 (:49)
      ├─ h3 16px; weight 700; text.primary; margin 0; marginBottom 8
      │     tooShort ? "Nothing to save yet" : "Name your timelapse"                                         (:50-52)
      ├─ p 13px; text.secondary; margin 0; marginBottom 16
      │     tooShort ? "A timelapse needs a minute of recording. Stop now and this session ends without one."
      │              : "Give it a name, or skip to use the default."                                         (:53-57)
      ├─ [!tooShort] input text; placeholder "My timelapse"; maxLength 255; disabled=loading;
      │     width 100%; padding "12px 16px"; 14px; weight 500; text.primary; bg.sunken;
      │     border 1px solid border.default; radius 8; outline none; marginBottom 16; opacity loading?0.5:1;
      │     Enter (when !loading) → onConfirm(name)                                                         (:58-83)
      └─ tooShort:
           div flex column gap 8                                                                            (:87)
           ├─ Button success lg fullWidth disabled=loading → "Keep recording" → onResume                    (:88-96)
           └─ Button ghost md fullWidth loading=(loading&&choice==="stop") disabled=(loading&&choice!=="stop")
                 → "Stop anyway" → choice="stop"; onConfirm(name)                                           (:97-109)
         else:
           div flex column gap 8                                                                            (:112)
           ├─ Button primary lg fullWidth loading=(loading&&choice==="edit") disabled=(loading&&choice!=="edit")
           │     → "Edit & Save" → choice="edit"; onEditAndSave(name)                                       (:113-125)
           └─ div flex gap 12                                                                               (:126)
              ├─ Button secondary lg fullWidth loading/disabled per choice "stop" → "Save & Stop" → onConfirm(name) (:127-139)
              └─ Button success lg fullWidth disabled=loading → "Resume" → onResume                         (:140-148)
```
Loading state: the clicked button shows Spinner (white, 16px) + label; others go to 0.6 opacity / not-allowed. No exit animation (parent unmounts it).

### 3B.6 CaptureErrorDialog (`C/CaptureErrorDialog.tsx`, `S/captureDiagnosis.ts`)

Props: `error` (raw text), `fallbackTitle?`, `onRetry?`, `onDismiss` (`:26-34`). Callers:
- SourcePicker: `fallbackTitle = error ? "Couldn't detect your displays" : "Couldn't start the capture"`, with `onRetry` (clears + refresh) (`C/SourcePicker.tsx:377-383`).
- DesktopRecorder: `fallbackTitle="The recording hit a capture error"`, no retry; opened from the capture-error banner's "Details" action (`C/DesktopRecorder.tsx:429-453`).

On mount: `invoke<CaptureEnvironment>("capture_environment")` (`:69-82`); `probed` flips true when done (success or failure). Diagnosis recomputed each render from `(error, env)`.
`title = diagnosis?.title ?? fallbackTitle ?? "Screen capture failed"` (`:85`).

```
Overlay label=title width "min(460px, 92vw)" onDismiss  (Esc / backdrop click close)            (:88)
└─ div flex column; minHeight 0; overflowY auto; padding 24; gap 16                              (:89)
   ├─ div
   │  ├─ h2 margin 0; 18px; weight 600; text.primary → {title}                                    (:91-93)
   │  └─ p margin "8px 0 0 0"; 14px; lineHeight 1.5; text.secondary →
   │       diagnosis?.summary ?? (probed
   │         ? "Something in the capture path failed and we couldn't place it. The details below are worth sending along."
   │         : "Checking what this system has installed…")                                        (:94-99)
   ├─ [diagnosis] ol margin 0; paddingLeft 20; flex column; gap 8;
   │     li 14px; lineHeight 1.5; text.primary → each step (default decimal markers)              (:102-110)
   ├─ [diagnosis.command] div flex; alignItems center; gap 8; bg.sunken; 1px border.default; radius 8;
   │     padding "8px 8px 8px 12px"                                                                (:113-117)
   │     ├─ code flex 1; minWidth 0; fontFamily monospace; 13px; text.primary; overflowX auto; whiteSpace pre (:118-123)
   │     └─ Button secondary sm flexShrink 0 → copiedCommand ? "Copied" : "Copy"                   (:124-126)
   ├─ details (closed by default)
   │  ├─ summary cursor pointer; 13px; text.tertiary; userSelect none → "Technical details"      (:131-133)
   │  └─ pre margin "8px 0 0 0"; padding 12; bg.sunken; radius 8; monospace; 11px; lineHeight 1.5;
   │         color text.error; pre-wrap; break-word; maxHeight 160; overflowY auto → {error}      (:134-142)
   └─ div flex gap 8                                                                              (:145)
      ├─ [onRetry] Button primary md flex 1 → "Try again"                                         (:146-150)
      ├─ Button secondary md flex 1 → copiedReport ? "Copied" : "Copy report"                      (:151-153)
      └─ Button ghost md flexShrink 0 → "Close" → onDismiss                                       (:154-156)
```
Copy buttons: clipboard write, confirmed state for `COPIED_MS = 1600` (`:37-57`); clipboard failure is silent. Report text = `buildCaptureReport` (`S/captureDiagnosis.ts:347-374`): optional `Diagnosis: {title}`, `Suggested fix: {command}`, blank; then `--- CAPTURE ENVIRONMENT ---`, `os:`, `session:`, `desktop:`, `distro:`, `portal installed:`, `screencast available:`, `portal backends:` (or `none found`), `pipewire running:`, `pipewiresrc element:`, `clip encoder:` (or `none (recording one JPEG per minute)`), blank; then `getReport(raw)` (logger). Booleans → `yes`/`no`/`unknown`.

`CaptureEnvironment` (`S/captureDiagnosis.ts:21-40`): `{os, sessionType|null, desktop|null, distroId|null, distroIdLike|null, distroName|null, portalInstalled|null, screencastAvailable|null, portalBackends: string[], pipewireRunning|null, pipewireGstElement|null, clipEncoder|null}`.

**Diagnoses (verbatim, in match order)** — `diagnoseCaptureError` (`S/captureDiagnosis.ts:204-320`). Linux = `env.os === "linux"` or UA contains linux.
Regexes (`:198-202`): `PORTAL_MISSING=/not provided by any \.service files|ServiceUnknown|NameHasNoOwner|Failed to connect to screencast portal|org\.freedesktop\.portal\.Desktop/i`; `BACKEND_MISSING=/No such interface|UnknownMethod|UnknownObject|NoReply|Failed to (select sources|start screencast|create screencast session)/i`; `GST_MISSING=/no element ["']?pipewiresrc|Failed to create pipeline|Failed to init gstreamer|Failed to load plugin/i`; `NO_FRAMES=/Failed to pull sample within timeout|Sample has no buffer/i`; `X11_ENUMERATION=/Failed to list (monitors|windows)|XOpenDisplay|Can't open display/i`. Cancel detection (no dialog): `/cancell?ed|response error: cancelled|user dismissed/i` (`:194-196`).

Package-family map (`:60-85`): apt ← debian, ubuntu, linuxmint, pop, elementary, zorin, raspbian; dnf ← fedora, rhel, centos, rocky, almalinux, nobara; pacman ← arch, manjaro, endeavouros, cachyos, garuda; zypper ← opensuse, opensuse-tumbleweed, opensuse-leap, sles; apk ← alpine; nix ← nixos. Install command forms: `sudo apt install X`, `sudo dnf install X`, `sudo pacman -S X`, `sudo zypper install X`, `sudo apk add X`; nix → none (`:97-112`).
Backend by `env.desktop` (`:119-138`): /gnome|cinnamon|budgie|unity|pantheon/ → `xdg-desktop-portal-gnome` "GNOME"; /kde|plasma/ → `xdg-desktop-portal-kde` "KDE Plasma"; /hyprland/ → `xdg-desktop-portal-hyprland` "Hyprland"; /sway|wlroots|river|wayfire|labwc/ → `xdg-desktop-portal-wlr` "wlroots compositors"; /cosmic/ → `xdg-desktop-portal-cosmic` "COSMIC".
`BACKEND_MENU` (`:141-144`): "Install the portal backend for your desktop: xdg-desktop-portal-gnome (GNOME, Cinnamon, Budgie), xdg-desktop-portal-kde (KDE Plasma), xdg-desktop-portal-hyprland (Hyprland), or xdg-desktop-portal-wlr (sway and other wlroots compositors)."
`backendSteps` (`:146-184`):
- nix: ["Add the portal to your NixOS configuration:", "xdg.portal = { enable = true; extraPortals = [ pkgs.{pkg} ]; };" (pkg default xdg-desktop-portal-gnome), "Rebuild with `sudo nixos-rebuild switch`, then log out and back in.", "Try casting again."]
- unknown backend: [BACKEND_MENU, "Log out and back in so the new backend registers.", "Try casting again."]
- known: [command ? "Install {pkg} — the portal backend for {label}." : "Install {pkg} (the portal backend for {label}) with your package manager.", "Log out and back in so the new backend registers with D-Bus.", "Try casting again."], command = install of pkg.

1. Portal missing — `env.portalInstalled === false` OR (not true AND PORTAL_MISSING AND NOT BACKEND_MISSING) (`:212-236`): title "Screen capture needs the desktop portal"; summary "Wayland only lets an app record what you explicitly hand it, through a system component called the XDG desktop portal. It isn't installed here, so there's nothing to ask permission with."; packages `xdg-desktop-portal {backendPkg}` or `xdg-desktop-portal`; steps if command: [backend ? "Install xdg-desktop-portal and {pkg} (the backend for {label})." : "Install xdg-desktop-portal.", (no backend → BACKEND_MENU), "Log out and back in so the portal registers with D-Bus.", "Try casting again."]; else ["Install xdg-desktop-portal with your package manager.", ...backendSteps.steps].
2. Backend missing — `screencastAvailable === false` OR BACKEND_MISSING (`:238-252`): title known ? "No screen-sharing backend for {label}" : "No screen-sharing backend installed"; summary "The desktop portal is running, but nothing on this system implements screen sharing for it. The portal itself is just a front door — the backend that matches your desktop does the actual capturing." + (backends? " Installed backends: {a, b}."); steps/command = backendSteps.
3. PipeWire down — `pipewireRunning === false` (`:257-269`): title "PipeWire isn't running"; summary "Screen capture on Wayland streams through PipeWire, and its socket isn't there — the service is stopped or not installed."; steps ["Check the service: `systemctl --user status pipewire`", "Start it: `systemctl --user enable --now pipewire pipewire-pulse`", "If the command isn't found, install the `pipewire` package first."]; command `systemctl --user enable --now pipewire pipewire-pulse`. (Backticks are literal characters in the rendered text.)
4. GStreamer plugin — `pipewireGstElement === false` OR GST_MISSING (`:271-293`): packages apt `gstreamer1.0-pipewire`, dnf `pipewire-gstreamer`, pacman `gst-plugin-pipewire`, zypper `gstreamer-plugin-pipewire`, apk `gst-plugin-pipewire`; title "PipeWire's GStreamer plugin is missing"; summary "You granted the capture, but the app can't read the video stream: the piece that bridges PipeWire to GStreamer ships as its own package and isn't installed."; steps [command ? "Install the PipeWire GStreamer plugin." : "Install the PipeWire GStreamer plugin (gstreamer1.0-pipewire, pipewire-gstreamer, or gst-plugin-pipewire, depending on your distro).", "Restart Lookout.", "Try casting again."].
5. No frames — NO_FRAMES (`:295-307`): title "The capture started but no frames arrived"; summary "The stream was set up and then stayed empty. This usually means the selected window was closed or minimised, or the compositor dropped the share."; steps ["Make sure the window or screen you picked is still open and not minimised.", "Pick the source again from the Cast tab to start a fresh share.", "If it keeps happening, capture a whole screen instead of a single window."]; no command.
6. X11 enumeration on Wayland — X11_ENUMERATION AND sessionType wayland (`:309-317`): title "Screens can't be listed on Wayland"; summary "Wayland deliberately hides the list of windows and monitors from apps. Use the Cast tab, which asks your desktop for permission and lets you choose the source there."; steps ["Switch to the Cast tab.", "Choose the screen or window you want to record."].
7. Non-Linux (`:323-336`): only macOS (UA darwin|mac) with /not authorized|screen recording|TCC|permission/i: title "Screen Recording permission is off"; summary "macOS blocks screen capture until Lookout is allowed in System Settings."; steps ["Open System Settings → Privacy & Security → Screen & System Audio Recording.", "Turn on Lookout.", "Quit and reopen Lookout — macOS only applies the change on relaunch."].
Otherwise `null` → fallback title + generic summary.

### 3B.7 PermissionScreen (`C/PermissionScreen.tsx`) — macOS gating

Gates in App: screen first, then camera, each `key`-ed so state resets (`S/App.tsx:1235-1241`). Initial state skips the gate on non-macOS, or when `localStorage["lookout-perm-{type}"] === "1"`; background re-check on macOS drops the cache and re-gates if revoked (`S/App.tsx:184-208`). `permCacheKey(type) = "lookout-perm-${type}"` (`:20`).

Flow: status `"checking"` → `config.check()` (tauri-plugin-macos-permissions `checkScreenRecordingPermission`/`checkCameraPermission`); granted → cache "1", `onGranted()`; denied → prompt; exception (non-macOS) → treat as granted (`:77-101`). "Grant Permission" → `request*Permission()` (opens System Settings) and sets `requested` (`:103-108`). "Skip" → `onGranted()` without caching.

Checking state (`:110-119`):
```
PageContainer centered (maxWidth 640, padding 16, flex column center, minHeight 100%)
├─ Spinner md (24px)
└─ p 14px; text.secondary; marginTop 12 → checkingLabel
```
Prompt state = `PageLayout` (no back) with (`:121-136`):
- icon (32x32 svg, viewBox 0 0 24 24, fill none, stroke `colors.text.tertiary`, width 1.5, round caps/joins), inside the 72px circle:
  - screen (`:33-37`): `rect x2 y3 w20 h14 rx2`, `line 8,21→16,21`, `line 12,17→12,21` (monitor)
  - camera (`:48-51`): path `M23 19a2 2 0 01-2 2H3a2 2 0 01-2-2V8a2 2 0 012-2h4l2-3h6l2 3h4a2 2 0 012 2z` + `circle cx12 cy13 r4`
- actions: Button primary lg fullWidth disabled=requested → requested ? "Opened System Settings" : "Grant Permission"; Button secondary lg fullWidth → "Skip".

Strings (`:31-59`):
| | screen | camera |
|---|---|---|
| title | "Screen Recording Permission" | "Camera Permission" |
| subtitle | "Lookout needs screen recording access to capture screenshots of your work. Your screen is captured locally and only periodic screenshots are uploaded." | "Lookout uses your camera to capture periodic photos of your work for timelapses. Photos are taken locally and only periodic snapshots are uploaded." |
| hint | "After enabling \"Lookout\" in System Settings > Privacy > Screen Recording, quit and reopen the app. If it still doesn't work, remove Lookout from the list entirely, restart the app, and grant permission again." | "After enabling \"Lookout\" in System Settings > Privacy > Camera, quit and reopen the app. If it still doesn't work, remove Lookout from the list entirely, restart the app, and grant permission again." |
| checkingLabel | "Checking screen recording permission..." | "Checking camera permission..." |

### 3B.8 AnnouncementBanner (`C/AnnouncementBanner.tsx`, `H/useAnnouncement.ts`)

`Announcement` = `{level: "info"|"success"|"warning"|"danger", message, url: string|null}` (`H/useAnnouncement.ts:12-16`). Polled from `fetchAnnouncement(API_BASE, {client:"lookout-desktop", version})` at mount and every 15 min; failures keep last value (`:24-58`). Passed to Gallery as `banner` when non-null (`S/App.tsx:1133`).

Level color (`:5-10`): info `#3b82f6`, success `#22c55e`, warning `#f59e0b`, danger `#ef4444`.
```
div flex; alignItems center; justifyContent space-between; gap 12; padding "8px 12px";
    background = color + "1a" (≈10% alpha, e.g. #3b82f61a); border 1px solid {color}; radius 8   (:29-40)
├─ span 12px; text.primary; minWidth 0; lineHeight 1.4 → {message}                              (:41-50)
└─ [url] Button secondary sm flexShrink 0 → "Open" → invoke("open_external_url", {url})          (:51-55, :19-26)
```
No dismiss, no animation.

### 3B.9 AddSessionPage (`C/AddSessionPage.tsx`)

Route `add` (`S/App.tsx:1151-1160`): `onBack → gallery`, `onOpenProgram = App.openProgram`, `onStart(token)` → add token and route the synthetic deep link `lookout://session/?token={token}`.
Data: `fetchPrograms(API_BASE)` → `{programs: Program[]}`; `Program = {name, displayName?, newSessionUrl, iconUrl?, pairUrl?, startUrl?}` (`:24-35`); failure → empty list (`:60-77`). Label = `displayName || name`.

State: `programsLoading`, `launched` (label of program currently opening; auto-cleared after **15 000 ms**, `:85-89`), `link`, `error`, `loading`.

```
PageLayout onBack title "Start a recording" subtitle "Pick a program to start a timelapse"            (:156-159)
 children:
  div width 100%; marginTop 16; flex column; gap 12                                                     (:206-214)
   ├─ programsLoading → Spinner md (24px)
   ├─ hasPrograms →
   │   div flex column gap 8                                                                            (:219-225)
   │     per program: Button primary lg fullWidth loading=(launched===label)
   │        disabled=(loading || (launched!==null && launched!==label))                                 (:227-234)
   │        [iconUrl] img 18x18; radius 5; marginRight 8; verticalAlign -4  + label                     (:236-249)
   │   launched ? p(note, color text.secondary): "Opened " <strong>{launched}</strong> " in your browser — finish there and you'll be sent back here to start recording."  (:253-257)
   │            : p(note): "Not all programs support this. You may need to start the session on the program's website."   (:259-262)
   └─ else → p(note): "No programs available right now. Open Lookout from a Hack Club site, or paste a link below."  (:266-269)
 actions (bottom, flex column gap 12):
  [error] p(note, color #ef4444) → {error}                                                               (:162-164)
  input text placeholder "Paste a lookout:// link here"; disabled=loading; width 100%; padding "12px 16px";
        13px; weight 500; text.primary; bg.sunken; border 1px solid (error ? #ef4444 : border.default);
        radius 10; outline none; height 48; opacity loading?0.5:1; Enter (if !loading) → handleStart;
        typing clears error                                                                              (:166-192)
  Button primary lg fullWidth disabled=(!link.trim()||loading) loading=loading → "Start from link"       (:193-202)
note style: 12px; text.tertiary; margin 0; textAlign center; lineHeight 1.5                              (:145-151)
```
Error strings: "Couldn't open your browser. Try the link option below." (`:100, :111`); "Couldn't finish linking - try again from the program's page." (`:129`); "Couldn't find a valid session token or pair link in that URL." (`:137`).
handleStart (`:116-143`): if `parsePairCallback(trimmed)` → `emit("lookout-deep-link", [trimmed])`; else `extractToken` → `onStart(token)`.
handleOpenProgram (`:91-114`): `onOpenProgram(program)` → `"failed"` shows error and clears launched; `"browser"` keeps spinner; `"started"` → App navigates away. Fallback without delegate: `invoke("open_external_url", {url: newSessionUrl})`.

### 3B.10 SettingsPage (`C/SettingsPage.tsx`)

Props `onBack` (→ gallery), `isWayland?` (`:40-43`). Subpages: `"menu" | "filtered-apps" | "linked-programs" | "advanced"` (`:45`). Settings are NOT a modal; the whole page is a route (`S/App.tsx:1144-1150`).

Outer: `div position relative; height 100%; overflow hidden` (`:351`) → `AnimatePresence mode="sync" initial={false} custom={direction}` → `motion.div key={subpage} position absolute; inset 0; overflowY auto` (`:354-383`). Direction = −1 when going to menu, +1 when going into a subpage (`:348`). Animation in D-anim.

**PageChrome** (`:93-169`) — shared scaffold. `usePublishHeaderNav({title, onBack})` (Linux header bar shows title + back).
```
div flex column; height 100%; maxWidth 480; margin 0 auto; padding 16; border-box               (:111-121)
├─ [!isLinux] div flexShrink 0; marginBottom 16
│    Button secondary sm style=cardButtonStyle: macOS "← Back"; else span inline-flex gap 4:
│      CaretLeftIcon size 14 weight bold + "Back"                                                (:123-136)
├─ div flexShrink 0; marginBottom 16                                                             (:140)
│   ├─ [!isLinux] h2 20px; weight 700; text.primary; margin 0; marginBottom 4 → {title}          (:141-153)
│   └─ p 12px; text.secondary; margin 0; lineHeight 1.5 → {description}                          (:154-163)
└─ children
```

**MenuRow** (`:172-242`):
```
button.lookout-app-row  position relative; flex; alignItems center; gap 12; width 100%; padding "12px 12px";
                        transparent; no border; radius 8; cursor pointer; textAlign left; color text.primary
├─ div.lookout-app-row-bg (absolute inset 0, radius 8; hover/press styles below)
├─ div (icon tile) relative z1; 32x32; radius 8; background bg.selected; flex center; flexShrink 0; color text.secondary
├─ div flex 1; minWidth 0; relative z1
│   ├─ div 13px; weight 500 → title
│   └─ div 12px; text.tertiary; ellipsis nowrap → description
└─ CaretRightIcon size 16 weight bold; relative z1; color text.tertiary; flexShrink 0
```
Row CSS injected via `<style>` (`:285-294`, again `:962-972`): `.lookout-app-row-bg {position:absolute; inset:0; background:transparent; transition: background 0.12s ease-out, transform 0.12s ease-out}`, `:hover .bg {background: bg.selected}`, `:active .bg {transform: scale(0.96); background: bg.selected}`.

**Menu** (`:278-338`): PageChrome title "Settings", description "Configure Lookout.". Container `radius 10; border 1px solid border.default; background bg.surface; padding 4`:
1. FunnelIcon 18 fill — "Filtered Apps" — description: isWayland ? "Not supported on Wayland" : count>0 ? "{n} app{s} filtered" : "Black out selected apps in captures" → subpage filtered-apps. (Clickable on Wayland too.)
2. Only if links.length > 0: LinkIcon 18 bold — "Linked Programs" — "{n} program{s} linked" → linked-programs.
3. WrenchIcon 18 fill — "Advanced" — `isDefaultApiBase()` ? "Developer options" : "Custom server: {apiBase without https?://}" → advanced.

**Filtered Apps** (`:740-1085`). Title "Filtered Apps"; description: "Selected apps will be blacked out in monitor screen captures." + (count>0: span color `#f59e0b` " {n} app{s} filtered.").
- Wayland: box `flex 1; flex center; radius 10; border 1px border.default; bg.surface; padding 24; text.tertiary; 12px; center; lineHeight 1.5` → "App filtering is not supported on Wayland." (`:939-957`)
- Else:
  - search input (wrapper `flexShrink 0; marginBottom 12`): placeholder "Search apps..."; width 100%; padding "8px 12px"; 13px; text.primary; bg.surface; border 1px border.default (focus → border.hover, blur → default); radius 8; outline none (`:974-998`). Filter = case-insensitive substring.
  - list `flex 1; overflowY auto; radius 10; border 1px border.default; bg.surface` (`:1001-1009`):
    - loading: `padding 4`, 12 skeleton rows (`motion.div` flex, alignItems center, gap 6, padding "6px 8px"): 18x18 radius 6 bg.selected (marginRight 2), 20x20 radius 6 bg.selected, bar height 12 width `${40 + ((i*23)%40)}%` radius 6 bg.selected; pulsing opacity (D-anim) (`:1010-1059`).
    - empty: `flex center; padding 24; text.tertiary; 12px` → searchQuery ? "No matching apps" : "No apps detected" (`:1060-1072`).
    - list `padding 4`: rows ordered pinned (blacklisted at page open — snapshot, not live), then running apps, then others; each group alphabetical (`localeCompare`) (`:784-787, :838-842, :1074-1078`).
  - Row (`:846-921`): `button.lookout-app-row` flex, alignItems center, gap 6, width 100%, padding "6px 8px", radius 8, 13px, text.primary; children: bg layer; checkbox 18x18, marginRight 2, radius 6, border `1.5px solid` (checked `#ef4444` : border.hover), background (checked `#ef4444` : transparent), flex center, `transition: all 0.15s` (`:875-895`), CheckIcon 12 bold color `#fff` when checked; icon box 20x20 → `AppIcon`; name span block ellipsis nowrap, weight checked 500 : 400. Click → `toggleApp(name)`.
  - AppIcon (`:60-90`): if base64 PNG → `img 20x20 objectFit contain draggable=false`, animation `lookout-icon-fade-in 0.2s ease-out` only if icon arrived after mount; else placeholder `20x20 radius 6 bg.selected`.
  - Data: `invoke<AppEntry[]>("list_installed_apps")` once → `{name, path?, running?}`; icons lazily via IntersectionObserver (`rootMargin: "200px"`) → `invoke<string|null>("get_app_icon", {path})` (base64 PNG) (`:766-830`). Blacklisted-but-uninstalled names are merged in.
  - Persistence: `useBlacklistedApps` (`H/useBlacklistedApps.ts`): `localStorage["lookout-blacklisted-apps"]` (JSON string[] sorted); on every change `invoke("set_blacklisted_apps", {apps})` (`:4, :28-34`).

**Linked Programs** (`:435-556`). Title "Linked Programs", description "Programs linked to this device can start a timelapse straight from the + menu.". Registry fetched for display names/icons (cosmetic). Container `radius 10; border 1px border.default; bg.surface; padding 4`; row per link `flex; alignItems center; gap 12; padding "8px 12px"`:
- ProgramIcon (`:395-427`): 32x32 radius 8 bg.selected flex center color text.secondary overflow hidden; img 32x32 contain (onError → fallback) else LinkIcon 16 bold.
- text: label (13px, 500, text.primary, ellipsis) = registry displayName || link.displayName || link.program; sub (12px, text.tertiary, ellipsis) "Linked {toLocaleDateString(pairedAt)} · {hostname(pairUrl)}".
- Button secondary sm style=cardButtonStyle disabled=(busy) → busy ? "Unlinking…" : "Unlink".
Unlink → Tauri `confirm("Unlink {label} from this device?\n\nStarting a session from the + menu will go through your browser again until you re-link.", {title: "Unlink program", kind: "warning"})` → `unlinkProgram(program)` → refresh; if it was the last link, return to menu (`:466-483`).

**Advanced** (`:560-736`). Title "Advanced", description "Options for testing and development. Please don't touch anything if you don't know what you're doing.". Card `radius 10; border 1px border.default; bg.surface; padding 16; flex column; gap 12`:
- label div 12px weight 500 text.secondary marginBottom 4 → "Lookout server"
- input text (spellCheck/autoCapitalize/autoCorrect off), placeholder `https://lookout.hackclub.com`; width 100%; padding "8px 12px"; 13px; text.primary; bg.surface; border 1px (error ? #ef4444 : border.default) (focus → border.hover); radius 8; fontFamily monospace (`:610-671`). Initial value = current API base.
- helper div 12px text.tertiary marginTop 4 lineHeight 1.5 → "Please enter a VALID lookout service URL."
- invalid (non-empty and `normalizeServerUrl` null): div 12px #ef4444 → "Enter a valid HTTPS URL (e.g. https://lookout-stage.example.com)."
- error: div 12px #ef4444 → "Could not reach the server (timed out)." or "Could not reach the server: {msg}" (msg may be "server responded {status}").
- row flex gap 8: Button primary sm disabled=(!dirty||invalid||saving) → saving ? "Checking…" : "Save"; if current ≠ default: Button secondary sm disabled=saving → "Reset to default".
- if current ≠ default, below card: div marginTop 12; padding "8px 12px"; radius 8; border 1px solid #f59e0b; transparent; color #f59e0b; 12px; lineHeight 1.5 → "Using a custom server. Timelapses recorded here won't appear on the default Lookout server."
Save flow (`:571-608`): non-default target → Tauri `confirm("Point this app at {target}?\n\nAll new recordings will upload there instead of the official Lookout server. Only continue if someone from Hack Club asked you to.", {title: "Switch Lookout server", kind: "warning"})`; probe `fetchPrograms(target, 6000)`; persist `localStorage["lookout-api-base"]` (removed when default) and `window.location.reload()`. `normalizeServerUrl` (`S/serverConfig.ts:27-43`): trim; prepend `https://` if no scheme; must be https, or http only for localhost/127.0.0.1; returns origin. Default `https://lookout.hackclub.com`.

## 3C. Tray popup, Add menu (DOM), Editor window, Timelapse editor

All file:line references are repo-relative. Token names resolve per `clients/react/src/ui/theme.ts`:
`spacing = { xs: 4, sm: 8, md: 12, lg: 16, xl: 20, xxl: 24, xxxl: 32 }` (theme.ts:201),
`radii = { sm: 6, md: 8, lg: 10 }` (theme.ts:202),
`fontSize = { xs: 11, sm: 12, md: 13, lg: 14, xl: 16, xxl: 18, heading: 20, display: 24, timer: 32 }` (theme.ts:203),
`fontWeight = { normal: 400, medium: 500, semibold: 600, bold: 700 }` (theme.ts:204).
Colour tokens are CSS variables (dark default theme.ts:7-68, light theme.ts:69-156). Where a component uses a colour token the dark / light values are given as `dark | light`.

Libraries in play in this section (versions from node_modules):
- `motion` 14.1.0 (`import { motion, AnimatePresence } from "motion/react"`) — framer-motion successor. Used by AddMenuPopup, TimelapseEditor, Button, ErrorDisplay.
- `@number-flow/react` / `number-flow` 0.6.2 — rolling digits (TrayApp timer, MinutesFlow, ProgressRing percent).
- `@squircle-js/react` ^1.3.0 — Button background is a squircle (`cornerSmoothing={0.7}`), clients/react/src/ui/Button.tsx:85.

---

### 3C.1 Shared primitives used by these screens (summary; exact values)

#### Button (`clients/react/src/ui/Button.tsx`)
Props: `variant` = `primary` (default) | `success` | `danger` | `warning` | `secondary` | `ghost`; `size` = `sm` | `md` (default) | `lg`; `loading`; `fullWidth`; `disabled` (Button.tsx:8-13,30-39).

Structure (Button.tsx:61-117):
```
motion.button  (position:relative; inline-flex; center/center; padding 0; bg transparent; border 1px solid transparent;
                fontWeight 600; borderRadius radii.md=8 (or style.borderRadius); width 100% if fullWidth;
                cursor pointer | not-allowed when disabled; opacity 1 | 0.6 when disabled-or-loading)
├─ Squircle(cornerRadius = borderRadius ?? 8, cornerSmoothing 0.7, asChild)
│   └─ motion.div  position:absolute; inset:-1; background = variant bg; border = variant border;
│                  CSS transition "opacity 0.15s, background 0.15s, border-color 0.15s"  (Button.tsx:98)
│                  variants idle/hover/active (see animations)
└─ span  position:relative; zIndex 1; inline-flex; center/center; gap 8; width 100%; + size padding/fontSize
    ├─ Spinner size "sm" (16px) colour "#000" for warning, "#fff" otherwise — only when loading (Button.tsx:114)
    └─ children
```
Variant colours (Button.tsx:15-22):
| variant | background | text | border |
|---|---|---|---|
| primary | `var(--color-accent)` #3b82f6 | `var(--color-on-accent)` #ffffff | 1px solid transparent |
| success | #22c55e | #fff | 1px solid transparent |
| danger | #ef4444 | #fff | 1px solid transparent |
| warning | #f59e0b | #000 | 1px solid transparent |
| secondary | transparent | text.secondary `rgba(255,255,255,0.7) \| rgba(0,0,0,0.65)` | 1px solid border.hover `rgba(255,255,255,0.22) \| rgba(0,0,0,0.22)` |
| ghost | transparent | text.secondary | 1px solid transparent |

Hover background (Button.tsx:46-57): ghost → bg.selected `rgba(255,255,255,0.12) | rgba(0,0,0,0.1)`; secondary → bg.surface `rgba(255,255,255,0.08) | #ffffff`; primary → `var(--color-accent-hover)` = `color-mix(in oklab, var(--color-accent) 88%, black)` (fallback `#2f6fd0`) (theme.ts:65-66); others → unchanged. Hover border: ghost stays transparent, others unchanged (Button.tsx:58-59).
Sizes (Button.tsx:24-28): sm `padding 6px 12px; fontSize 12`; md `8px 16px; 13`; lg `12px 24px; 15`.
Disabled/loading: no hover/tap variants (whileHover/whileTap undefined), opacity 0.6, cursor not-allowed (Button.tsx:63-72).

#### Spinner (`clients/react/src/ui/Spinner.tsx`)
Sizes sm 16 / md 24 / lg 40 (Spinner.tsx:9). SVG viewBox 0 0 24 24; base circle r=10 stroke `colors.spinner.base` (`rgba(255,255,255,0.1) | rgba(0,0,0,0.1)`) strokeWidth 2; arc circle r=10 stroke `color || colors.spinner.track` (`rgba(255,255,255,0.8) | rgba(0,0,0,0.8)`) strokeWidth 2, round cap, `strokeDasharray 62.83`, `strokeDashoffset 47.12` (= a quarter arc) (Spinner.tsx:26-44). Animation `spin 1s linear infinite` (Spinner.tsx:24).

#### ProgressRing (`clients/react/src/ui/ProgressRing.tsx`)
Defaults `size 72`, `strokeWidth 5`, `showPercent false` (ProgressRing.tsx:21-27). radius = (size−stroke)/2 = 33.5; circumference = 2πr; dashoffset = C·(1−clamped). SVG rotated `-90deg` (starts at 12 o'clock). Track circle stroke `colors.spinner.base`; progress circle stroke `color ?? var(--color-accent)`, round cap, `transition: stroke-dashoffset 0.25s linear` (ProgressRing.tsx:39-60). Percent label (when showPercent): absolute inset 0, centred, fontSize 13, weight 600, `colors.text.secondary`, tabular-nums; content `<NumberFlow value={min(99, floor(clamped*100))}/>%` (ProgressRing.tsx:62-77) — never shows 100.

#### MinutesFlow (`clients/react/src/ui/MinutesFlow.tsx`)
`safe = max(0, round(minutes))`; if h==0 renders `<NumberFlow m/>m`; else `<NumberFlow h/>h <NumberFlow m/>m` (note the literal space after "h"). Span style `{ color, fontVariantNumeric: "tabular-nums" }` (MinutesFlow.tsx:21-39).

#### ErrorDisplay banner (`clients/react/src/ui/ErrorDisplay.tsx:41-101`)
Banner box: `padding 12px 16px; marginBottom 12; background rgba(239,68,68,0.15); border 1px solid #ef4444; borderRadius 8; color text.error (#fca5a5 | #ef4444); fontSize 13; flex; alignItems flex-start; gap 8`. Inner text column: `flex 1; fontSize 11; white-space pre-wrap; word-break break-all; maxHeight 120; overflowY auto`; title as `<strong>` block with marginBottom 4; error text in `fontFamily: monospace`; optional reassurance appended with `"\n\n"` at opacity 0.8. Optional action/Copy buttons: transparent, colour text.error, fontSize 11, lineHeight 1, padding `2px 8px`, radius 6, 1px text.error border drawn by an inner motion.div that springs to scale 0.96 on tap (stiffness 1500, damping 60). Dismiss button: "×" (`&times;`) fontSize 16.

---

### 3C.2 TrayApp — tray popup window (`clients/desktop/src/components/TrayApp.tsx`)

Rendered when `window.location.hash.includes("tray")` (clients/desktop/src/App.tsx:167-170). Window creation/size/position is in the Rust side (see section 2). On macOS a native SwiftUI tray (src-tauri/swift) also exists; this is the webview version.

**Setup side effects**
- Forces `background: transparent !important` on `<html>`, `<body>`, `#root` (TrayApp.tsx:50-57), removed on unmount (TrayApp.tsx:88-90).
- Also injects a `<style>` block (TrayApp.tsx:150-161):
  ```css
  :root, html, body, #root { background: transparent !important; background-color: transparent !important; }
  :root { --color-bg-body: transparent !important; --color-bg-panel: transparent !important; }
  ```
- `isWindows` = UA contains `"windows"` **or `"linux"`** (TrayApp.tsx:60-62) — i.e. Linux uses the Windows fallback styling.
- macOS version from UA `Mac OS X (\d+)[._](\d+)`; `isTahoe = major >= 26` → `cornerRadius = 16`, else `10` (TrayApp.tsx:64-67). Initial state cornerRadius 10 (TrayApp.tsx:46).
- Native glass via `tauri-plugin-liquid-glass-api`: `isGlassSupported()` then `setLiquidGlassEffect({ cornerRadius: isTahoe ? 16 : 10, tintColor: "#00000000", variant: GlassMaterialVariant.Sidebar })` (TrayApp.tsx:70-84). Errors only logged.

**State sync** (TrayApp.tsx:94-131)
- `invoke("recorder_state")` → `RecorderState | null` on mount, on `tray-opened` event, and on `window` `focus`.
- `listen("recorder-state", e => setState(e.payload))` for live updates.
- `controlMode = state?.controlMode ?? "recording"`; `captureCount = state?.captureCount ?? 0` (TrayApp.tsx:145-146).
- `liveSeconds = useRecorderClock(state?.clock ?? null)` (TrayApp.tsx:43) — rAF loop that re-derives display seconds from the clock anchor every frame while `running && holdAtUnixMs == null`, only calling setState when the integer changes (clients/desktop/src/recorder.ts:192-228).

**Actions**: Pause → `invoke("tray_action", { action: "pause" })`; Resume → `{ action: "resume" }`; Stop → `{ action: "stop" }` (TrayApp.tsx:133-143). No confirmation, no local optimistic state.

**Box tree** (TrayApp.tsx:162-219)
```
div.root  display:flex; alignItems:center; justifyContent:space-between;
          padding 8px 12px (spacing.sm spacing.md); width 100%; height 100%; box-sizing border-box;
          fontFamily "system-ui, -apple-system, sans-serif"   ← NOT Geist
          backgroundColor: isWindows ? "rgba(26, 26, 26, 0.93)" : "transparent"
          borderRadius / WebkitBorderRadius: isWindows ? 0 : cornerRadius (10 or 16)
          border none; overflow hidden
├─ div (left group)  flex; alignItems center; gap 12
│   ├─ div  flex; alignItems center; gap 8
│   │   ├─ [controlMode === "recording"] div  8×8, borderRadius 50%, background #ef4444,
│   │   │      animation "pulse 1.5s ease-in-out infinite", flexShrink 0          (TrayApp.tsx:176-181)
│   │   ├─ [controlMode === "paused"] span  color text.tertiary (rgba(255,255,255,0.5) | rgba(0,0,0,0.45)),
│   │   │      lineHeight 1, inline-flex center; contains inline SVG 12×12 viewBox 0 0 24 24 fill currentColor:
│   │   │      <rect x=7 y=5 w=4 h=14 rx=1/>, <rect x=13 y=5 w=4 h=14 rx=1/>   (pause glyph, hand-drawn, not phosphor)
│   │   └─ span  fontSize 13; fontWeight 700; tabular-nums; color text.primary (#ffffff | rgba(0,0,0,0.85)); display flex
│   │        └─ TrayTimer
│   └─ span  fontSize 12; color text.secondary — text: `${captureCount} ${captureCount === 1 ? "shot" : "shots"}`
└─ div (buttons)  flex; gap 4
    ├─ recording → Button variant "warning" size "sm": "Pause"
    │  else      → Button variant "success" size "sm": "Resume"
    └─ Button variant "danger" size "sm": "Stop"
```
Note: in the transparent tray, text.primary resolves via the OS colour scheme; the Windows/Linux background is a fixed dark `rgba(26,26,26,0.93)` regardless of theme.

**TrayTimer** (TrayApp.tsx:9-32): h = floor(s/3600), m = floor((s%3600)/60), s = s%60.
- h > 0: `<NumberFlow h/>` `:` `<NumberFlow m minIntDigits 2/>` `:` `<NumberFlow s minIntDigits 2/>`
- else: `<NumberFlow m minIntDigits 2/>` `:` `<NumberFlow s minIntDigits 2/>` → e.g. "05:09"
- Wrapper span `inline-flex; alignItems center`. Each colon span: `margin 0 1px; position relative; top -1px` (colon nudged up 1px).
- NumberFlow uses library defaults (see animations file): digits roll vertically with 900ms spring-like `linear(...)` easing; trend = sign(new−old) so digits roll upward when increasing.

States: no explicit loading/error/empty state — with `state === null` it shows recording dot, "00:00", "0 shots", Pause + Stop.

---

### 3C.3 AddMenuPopup — "+" menu for Windows/Linux (`clients/desktop/src/components/AddMenuPopup.tsx`)

DOM replica of macOS `AddMenu.swift` (comment AddMenuPopup.tsx:6-18). On macOS the native panel is used instead via `invoke("show_add_menu", { entries, anchor })` (App.tsx:666-683).

**Wiring** (App.tsx:636-689, 1400-1411)
- `handleAdd(anchor)` from the header "+" button. If popup open → close (toggle). If no programs → `navigate({ page: "add" })`. Programs are refetched behind the menu each open.
- Items (non-mac, App.tsx:651-662):
  - one per program: `{ id: "program:<name>", label: displayName || name, iconUrl, fallbackIcon: <ArrowSquareOutIcon size={15} weight="bold" /> }`
  - `{ separator: true }`
  - `{ id: "create-new", label: "Start from link", fallbackIcon: <PlusIcon size={15} weight="bold" /> }`
- (mac native entries use SF Symbols `"arrow.up.forward.app"` and `"plus"`, App.tsx:666-677.)
- Rendered inside `<AnimatePresence>` at App root, outside the route-transition wrapper so `position: fixed` is viewport-relative (App.tsx:1400-1411). `onSelect` → `setAddMenu(null)` then `handleMenuChoice(choice)`: `null` = dismissed; `"create-new"` → add page; `program:<name>` → `openProgram` (fallback to add page on "failed") (App.tsx:619-634).

**Constants** (AddMenuPopup.tsx:44-51): `GAP = 6` (button→menu), `EDGE = 8` (viewport margin), `TAIL_W = 20`, `TAIL_H = 10`, `TAIL_INSET = 14`.

**Positioning** (AddMenuPopup.tsx:118-123, 186-206, 220-228)
- `flipped` (useLayoutEffect): `below = anchor.y + anchor.height + GAP`; flip above when `below + el.offsetHeight > innerHeight − EDGE`.
- `alignedRight = max(EDGE, innerWidth − (anchor.x + anchor.width))` (right edges aligned).
- `centreFromAligned = innerWidth − (anchor.x + anchor.width/2) − alignedRight`; `shift = max(0, TAIL_INSET + TAIL_W/2 − centreFromAligned)`; `right = max(EDGE, alignedRight − shift)`.
- `tailRight = max(TAIL_INSET, innerWidth − (anchor.x + anchor.width/2) − right − TAIL_W/2)`; `tailCentreFromRight = tailRight + TAIL_W/2`.
- Panel style: `position fixed; right`; not flipped → `top = anchor.y + anchor.height + GAP + (isLinux ? TAIL_H : 0)`; flipped → `bottom = innerHeight − anchor.y + GAP + (isLinux ? TAIL_H : 0)`. `zIndex 10000; minWidth 220; maxWidth min(320, innerWidth − right − EDGE); padding 6; box-sizing border-box; flex column; gap 1`.
- `transformOrigin`: Linux → `calc(100% - ${tailCentreFromRight}px) top|bottom`; others → `"top right"` / `"bottom right"` when flipped (AddMenuPopup.tsx:264-268).

**Panel material**
| | Linux (AddMenuPopup.tsx:231-248) | Windows/other (AddMenuPopup.tsx:249-258) |
|---|---|---|
| borderRadius | 12 | 14 |
| border | `1px solid var(--color-popover-border)` | `1px solid color-mix(in srgb, var(--color-text-primary) 12%, transparent)` |
| background | `linear-gradient(to bottom, var(--color-popover-bg-top), var(--color-popover-bg))` | `colors.bg.panel` = `#1e1e1e \| #ffffff` (solid, no blur) |
| boxShadow | `0 1px 3px rgba(0, 0, 0, 0.08), 0 4px 12px rgba(0, 0, 0, 0.1), 0 10px 28px rgba(0, 0, 0, 0.12)` | `0 6px 32px rgba(0, 0, 0, 0.28)` |

Linux popover vars (clients/desktop/src/linuxChrome.ts:141-161), dark | light:
`--color-popover-bg #383838 | #f7f7f7`, `--color-popover-bg-top #3d3d3d | #ffffff`, `--color-popover-border rgba(255,255,255,0.14) | rgba(0,0,0,0.12)`, `--color-popover-hover rgba(255,255,255,0.1) | rgba(0,0,0,0.08)`, `--color-popover-separator rgba(255,255,255,0.15) | rgba(0,0,0,0.12)`.

**Linux tail** (AddMenuPopup.tsx:274-319): `<svg width=20 height=10 viewBox="0 0 20 10">` absolutely positioned `right: tailRight`, `top: -10` (or `bottom: -10` + `transform: scaleY(-1)` when flipped), display block.
- Fill path: `M0 10 L10 0.5 L20 10 Z` fill `var(--color-popover-bg-top)` (covers the panel's top border under the tail).
- Stroke path (open polyline, no base): `M0.5 9 L10 1 L19.5 9` fill none, stroke `var(--color-popover-border)`, strokeWidth 1, strokeLinejoin round.

**Row** (AddMenuPopup.tsx:53-106), role="menuitem"
```
div  flex; alignItems center; gap (Linux 12 | else 9);
     minHeight (Linux 32 | none); padding (Linux "0 12px" | "7px 9px"); borderRadius (Linux 6 | 9);
     background: highlighted ? (Linux var(--color-popover-hover) | color-mix(in srgb, var(--color-text-primary) 9%, transparent)) : transparent;
     cursor default; userSelect none
├─ span (icon slot) 18×18 inline-flex centred, flexShrink 0, color: highlighted ? text.primary : text.secondary
│   └─ img (iconUrl, 18×18, objectFit contain, borderRadius 5, display block, draggable false)
│      — on error → fallbackIcon (phosphor, size 15, weight bold) shown instead
└─ span (label) fontSize (Linux 14.5 | 13.5); fontWeight (Linux 400 | 500); color text.primary; nowrap; ellipsis
```
No hover transition — highlight flips instantly.
**Separator** (AddMenuPopup.tsx:321-329): `height 1; background (Linux var(--color-popover-separator) | border.default rgba(255,255,255,0.12)|rgba(0,0,0,0.12)); margin (Linux "5px 0" | "4px 10px")`.

**Interaction**
- Hover row → `selection = i`; leaving → clears only if still that row (AddMenuPopup.tsx:335-336). Click → `onSelect(item.id ?? null)`.
- Click-away: capture-phase `pointerdown` on document outside the panel AND outside the anchor rect → `onSelect(null)`. Window `blur` → `onSelect(null)` (AddMenuPopup.tsx:126-145).
- Keyboard (capture-phase `keydown` on document, AddMenuPopup.tsx:148-184): `Escape` → dismiss; `ArrowDown/ArrowUp` → move selection among non-separator items, wraps; from no selection Down picks first, Up picks last; `Enter` → activate current selection (no-op when nothing selected). All preventDefault.
- No initial selection (state null).

---

### 3C.4 Editor window (`clients/desktop/src/components/EditorWindow.tsx`)

**Opening** — `openEditorWindow(token)` (EditorWindow.tsx:49-91), called from App (session detail "Edit", App.tsx:732, 1182), RecordPage.tsx:120, DesktopRecorder.tsx:239 (when stop outcome `edit` is true).
- Label: `editor-${token.slice(0, 8)}` (EditorWindow.tsx:25-27). If exists → `setFocus()` + emit `lookout-editor-opened` `{ token }` and return.
- `new WebviewWindow(label, { url: "<pathname>#/editor?token=<token>", title: "Edit timelapse", width: 940 + pad, height: 660 + pad, minWidth: 620 + pad, minHeight: 480 + pad, resizable: true, center: true, transparent: true, macOS: titleBarStyle "overlay", Linux: decorations: false })`; `pad = WINDOW_MARGIN * 2` on Linux when the shell does not draw the frame, else 0 (EditorWindow.tsx:62-86). On `tauri://error` → console.error only.
- Emits `lookout-editor-opened` `{ token }` afterwards (EditorWindow.tsx:90).
- Routed in App.tsx:174-177 by hash regex `^#\/?editor\?token=([0-9a-fA-F]{64})`, skipping all main-window machinery.

**Events** (EditorWindow.tsx:18,22): `EDITED_EVENT = "lookout-edited"`, payload `{ token, status: string|null, redirectUrl: string|null, panelUrl: string|null, panelResolved: boolean }`; `EDITOR_OPENED_EVENT = "lookout-editor-opened"`, payload `{ token }`.

**Root view** `EditorWindow({ token })` (EditorWindow.tsx:246-446)
- `useDesktopAppearance({ undecorated: true })`, `useWindowFrameState()`, `useBackdropState()`, `useBackgroundBlur()` (Linux chrome — section 5).
- Vibrancy (non-Linux): `invoke("enable_vibrancy")` → on success set html/body/#root background `transparent`; on failure set them to `var(--color-bg-body)` (#242424 | #fafafa). On unmount, if applied, `invoke("disable_vibrancy")` (EditorWindow.tsx:260-286).
- Client: `createTauriLookoutClient({ baseUrl: getApiBase(), token })` (EditorWindow.tsx:289-291). `client.getSession()` → if name: `sessionName = name`, window title → `` `Edit — ${name}` `` (em dash) (EditorWindow.tsx:295-307).
- Close handling (EditorWindow.tsx:347-370): `onCloseRequested` → unless finished, `preventDefault`, then Tauri `confirm()` dialog:
  - title: **"Finish timelapse?"**, kind `"warning"`
  - body if dirty && removed > 0: `` `Closing publishes this timelapse with ${removed} cut${removed === 1 ? "" : "s"} applied. This can't be undone.` ``
  - else: **"Closing publishes this timelapse as recorded. This can't be undone."**
  - OK → `finishAndClose()`: `client.setCuts(cuts)` then `client.applyCuts()` (errors swallowed+logged), emit `lookout-edited` with result fields, then `closeEditorWindow()` (EditorWindow.tsx:318-345).
- `closeEditorWindow()` (EditorWindow.tsx:101-121): focus `main` window first, then `getCurrentWindow().close()`, fallback `destroy()`.
- `onApplied` from the editor's Save: mark finished, emit `lookout-edited`, close (EditorWindow.tsx:429-441).

**Box tree** (EditorWindow.tsx:372-445)
```
div  height 100%; minHeight 0; box-sizing border-box; color text.primary; flex column
├─ [macOS] div data-tauri-drag-region  flex 0 0 auto; height 30; cursor default   (clears traffic lights; OS draws title)
├─ [Linux] HeaderBar title = sessionName ?? "Edit timelapse"; subtitle = sessionName ? "Editing timelapse" : undefined;
│          appearance; maximizable   + <WindowResizeHandles/>
│  [Windows] nothing (native decorations)
└─ div (body)  flex 1 1 auto; minHeight 0; flex column;
               padding: (macOS or Linux) "0 16px 16px" ; Windows 16 all round
    └─ TimelapseEditor (token, apiBaseUrl=getApiBase(), client, onCutsChange, onApplied)
```

**EditorOpenPlaceholder** (main window, while an editor is open; EditorWindow.tsx:128-186; shown at App.tsx:1117)
```
div role=button tabIndex 0  height 100%; flex column; center/center; gap 12; padding 24; textAlign center;
                            cursor pointer; userSelect none; opacity focusing ? 0.6 : 1; transition "opacity 0.15s ease"
├─ svg 40×40 viewBox 0 0 24 24 fill none stroke text.tertiary strokeWidth 1.5 round caps/joins:
│    <rect x=2 y=4 w=20 h=16 rx=2/>, <path d="M2 9h20"/>, <path d="M9 14h6"/>
├─ div fontSize 14; weight 700; text.primary — "Edit your timelapse in the edit window."
└─ div fontSize 12; text.tertiary — "Click here to bring it to the front."
```
Click → `focusing=true`, `focusEditorWindow(token)` then `focusing=false`. Enter/Space → focus (no dim).
`useEditorWindowOpen()` (EditorWindow.tsx:195-230): token set on `lookout-editor-opened`, cleared on `lookout-edited`; while set, polls every 1000 ms `WebviewWindow.getByLabel(label)` and clears if gone.

---

### 3C.5 TimelapseEditor (`clients/react/src/components/TimelapseEditor.tsx`)

Model: 1 video second = 1 capture unit = 1 real-world minute (TimelapseEditor.tsx:116-121; `SCREENSHOT_INTERVAL_MS = 60_000`, packages/shared/src/constants.ts:25). Injects the scoped stylesheet `injectEditorStyles()` on mount (TimelapseEditor.tsx:141; editorStyles.ts).

**Constants** (TimelapseEditor.tsx:71-102)
| name | value | meaning |
|---|---|---|
| `STRIP_HEIGHT` | 56 | filmstrip/track height px |
| `hatch(period)` | `repeating-linear-gradient(45deg, var(--color-cut-stripe) 0 ${p/2}px, transparent ${p/2}px ${p}px)`; used with period 10 | cut hatch |
| `RULER_HEIGHT` | 22 | ruler lane height |
| `HEAD_W` / `HEAD_H` | 9 / 13 | playhead cap pill |
| `HEAD_HIT` | 22 | invisible grab square around cap |
| `FILMSTRIP_MAX_TILES` | 48 | |
| `MAX_TILE_FAULTS` | 2 | bad tiles before source is abandoned |
| `pixelRatio()` | `min(2, devicePixelRatio || 1)` | canvas DPR |
| default tileWidth | `round(56 * 16/9)` = 100 | before first frame source (TimelapseEditor.tsx:420) |
| click-vs-drag threshold | `0.34` unit | TimelapseEditor.tsx:611 |
| keyboard seek step | 1 unit, Shift = 10 | TimelapseEditor.tsx:756 |
| seek clamp | `[0, unitCount − 0.05]` | TimelapseEditor.tsx:568 |
| handle seek nudge | `+0.02` | TimelapseEditor.tsx:641,648,651 |
| filmstrip debounce | 220 ms | TimelapseEditor.tsx:542 |
| min tile width | 24 px | TimelapseEditor.tsx:463 |
| thumbnail encoding | `canvas.toDataURL("image/jpeg", 0.82)`; published progressively every 3rd tile (`i % 3 === 2`) | TimelapseEditor.tsx:505-506 |

Shared/editor-math constants:
- `rulerStep(unitCount, trackWidth, minLabelPx = 88)`: smallest of `NICE_STEPS = [1, 2, 5, 10, 15, 20, 30, 60, 120, 180, 360, 720]` with `step ≥ 88 / (trackWidth/unitCount)`; returns 1 if empty (clients/react/src/hooks/editorMath.ts:174-191). Ticks every `step/2`, `major` on multiples of `step` (editorMath.ts:194-208).
- `elapsedLabel(u, total)`: total < 60 → `"${m}m"`; else `"${h}:${mm}"` (editorMath.ts:165-170).
- `unitClockLabel(unit)`: `toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })` (editorMath.ts:148-154).
- `unitAtTime(t, n) = clamp(floor(t), 0, n−1)` (editorMath.ts:17-19).
- `gapIndices`: gap marker where consecutive captures differ by > `1.5 × 60_000` ms (editorMath.ts:125-133).
- Regions are integer unit ranges `[startUnit, endUnit)`, min width 1; `normalizeRegions` sorts and merges overlapping **or adjacent** (`r.start <= last.end`) (editorMath.ts:85-100).
- `regionsToCuts` end = min(next kept capture's timestamp, lastCut + 60s) (editorMath.ts:27-56).
- packages/shared/src/cuts.ts: `MAX_CUT_INTERVALS = 120` (:24), `MAX_USER_RECOMPILES = 5` (:28), `EDIT_LEASE_SECONDS = 120` (:50), `EDIT_HEARTBEAT_SECONDS = 30` (:54), `EDIT_HOLD_MAX_MINUTES = 120` (:61), `EDIT_WINDOW_DAYS = 7` (:66), `CUT_BOUNDS_SLACK_MS = 5 * 60_000` (:70). `countCutUnits` (:189) drives the kept/removed footer.
- Edit lease: `useEditLease(client, !saving)` → `client.heartbeatEditing()` immediately then every 30 s; returns false once server says `held: false` (clients/react/src/hooks/useEditLease.ts:20-50). When false and not saving → load error "This timelapse was already published, so it can no longer be edited." (TimelapseEditor.tsx:322-328).
- Filmstrip frame sources (clients/react/src/hooks/filmstripFrames.ts): `FLAT_SPREAD = 2` (:34), `NOISE_STEP = 55` (:46), `SEEK_TIMEOUT_MS = 4000` (:49), `FAULT_SAMPLE_BUDGET = 24_000` (:53). `prefersDecoderFrames()` true for WebKitGTK (AppleWebKit + Linux, not Chrome/Chromium/Android/Mac OS X) (:241-249) → opener order `[decoder, CORS <video>, blob <video>]`, else `[CORS video, blob video, decoder]` (TimelapseEditor.tsx:457-459). Tile i samples `t = clamp(((i+0.5)·tileW)/trackW, ≤1) × duration`, clamped to `duration − 0.05` (:488-489). tileW = `max(24, round(56 × aspect))`; tiles = `min(48, max(1, ceil(trackW / tileW)))` (:463-468). All sources failing → plain track (no thumbnails).

**Loading flow** (TimelapseEditor.tsx:191-263)
1. `client.getUnits()`. If `editable && originalVideoUrl` → data ready; `regions = cutsToRegions(res.cuts, res.units)`.
2. If `editableReason` is `"preparing"` or `"no_original"` → `preparingUnits = expectedUnits ?? 0`; poll `client.getStatus()` every **2000 ms**. `status.progress` (number) → real progress anchor. `status.editable` → reload units. `status === "complete"` → fail("published"); `"failed"` → fail("failed"). Poll errors are ignored (keep polling).
3. Any other reason → fail. Thrown error → message string.
Fail messages (TimelapseEditor.tsx:195-204):
- published: **"This timelapse has already been published, so it can't be edited."**
- failed: **"This timelapse couldn't be compiled, so there's nothing to edit."**
- recompiles_exhausted: **"This timelapse has reached its edit limit."**
- other: **"This timelapse isn't available for editing."**

Build progress (TimelapseEditor.tsx:272-315; clients/react/src/hooks/buildProgress.ts): ticks every **200 ms**; value = `min(PROGRESS_CAP=0.95, max(prev, estimated, fromReal))`, monotonic.
- `compileEstimateMs(units) = 6000 + max(0, units) × 90` (buildProgress.ts:2,14,17-19).
- `estimateBuildProgress(elapsed, est) = 1 − exp(−2.2 × elapsed/est)` (buildProgress.ts:36-39).
- `interpolateBuildProgress(anchor, since, est)`: `from = clamp(anchor, 0, 0.95)`; `budget = min(0.95 − from, 2000/est)`; `from + budget × (1 − exp(−2.2 × since/2000))` (buildProgress.ts:72-85). `PROGRESS_POLL_MS = 2000` (:42).

**Render states**

(a) Load error (TimelapseEditor.tsx:828-841):
```
div padding 20; maxWidth 520
├─ ErrorDisplay variant "banner" title "Can't edit" error=<message>
└─ [onCancel] div marginTop 12 → Button secondary sm "Close"     (EditorWindow does not pass onCancel → never shown on desktop)
```

(b) Loading / preparing (TimelapseEditor.tsx:843-892):
```
div height 100%; minHeight 260; flex column; center/center; gap 16; textAlign center; padding 20
├─ preparingUnits !== null → ProgressRing progress=buildProgress showPercent (72px, stroke 5)
│  else                    → Spinner size "lg" (40px)
└─ div
    ├─ div fontSize 16; weight 600; text.primary; letterSpacing -0.01em — "Preparing your timelapse"
    └─ div fontSize 13; text.secondary; marginTop 4; maxWidth 340; lineHeight 1.5 —
         preparingUnits > 0 ? `Stitching ${n} minute${n === 1 ? "" : "s"} of footage.` : "oooooooooooo"
```
(The literal string `"oooooooooooo"` is what renders before the unit count is known — copied verbatim, TimelapseEditor.tsx:887.)

(c) Ready (TimelapseEditor.tsx:894-1380):
```
div root  height 100%; minHeight 0; flex column; gap 12
├─ STAGE div  flex 1 1 auto; minHeight 0; flex; center/center; position relative;
│             background editor.well (rgba(0,0,0,0.32) | rgba(0,0,0,0.08));
│             border 1px solid editor.wellBorder (rgba(255,255,255,0.1) | rgba(0,0,0,0.1)); borderRadius 12; overflow hidden
│   ├─ <video src=originalVideoUrl playsInline muted>  maxWidth 100%; maxHeight 100%; display block; cursor pointer
│   │     onClick togglePlay; onPlay/onPause → playing state     (raw <video>, NOT VideoPlayer, no native controls)
│   ├─ AnimatePresence [!playing] motion.div  absolute inset 0; flex centred; pointerEvents none
│   │     └─ div 56×56 circle; background rgba(0,0,0,0.55); backdropFilter blur(12px); flex centred
│   │          └─ svg 20×20 viewBox 0 0 24 24 fill #fff: <path d="M8 5v14l11-7z"/>  (play triangle)
│   └─ AnimatePresence [inCutNow] motion.div  absolute inset 0; borderRadius 12;
│         boxShadow inset 0 0 0 3px editor.cutBorder (#f87171 | #dc2626); backgroundColor rgba(220, 38, 38, 0.10); pointerEvents none
│         └─ span absolute top 12 right 12; background cutBorder; color #fff; fontSize 11; weight 600;
│              padding 3px 8px; borderRadius 6; letterSpacing 0.01em — "Will be removed"
└─ DOCK div  flex 0 0 auto; flex column; gap 8
    ├─ TRANSPORT div  flex; alignItems center; gap 8
    │   ├─ button.lk-ed-iconbtn  30×30; borderRadius 8; aria-label playing ? "Pause" : "Play"; onClick togglePlay
    │   │     (class: inline-flex centred; bg transparent; 1px solid border.default; color text.primary; padding 0)
    │   │     icon svg 12×12 fill currentColor: playing → <path d="M6 4h4v16H6zM14 4h4v16h-4z"/> ; else <path d="M8 5v14l11-7z"/>
    │   ├─ div fontSize 13; text.primary; tabular-nums; letterSpacing -0.01em
    │   │     `${elapsedLabel(currentUnit, n)}` + span(text.tertiary): " of " + elapsedLabel(n, n) + " · recorded at " + unitClockLabel(units[currentUnit])
    │   │     e.g. "12m of 47m · recorded at 3:42 PM"
    │   └─ div flex 1 (spacer)
    ├─ TIMELINE div  position relative; userSelect none; touchAction none;
    │               onPointerMove / onPointerUp / onPointerCancel(=up)
    │   ├─ [n>0] PLAYHEAD container  absolute; left pct(min(time, n)); top 0; bottom 0; width 0; zIndex 3; pointerEvents none
    │   │   ├─ hit div  absolute; top 22−13 = 9; left −11; 22×22; flex; justify center; align flex-start;
    │   │   │           cursor ew-resize; pointerEvents auto; onPointerDown = ruler scrub
    │   │   │   └─ div.lk-ed-playhead  9×13; borderRadius 4.5; background text.primary; boxShadow 0 1px 3px rgba(0,0,0,0.45)
    │   │   └─ stem div  absolute; top 22−4 = 18; bottom 0; left −1; width 2; background text.primary; boxShadow 0 0 0 0.5px rgba(0,0,0,0.35)
    │   ├─ RULER div  relative; height 22; cursor ew-resize; onPointerDown scrub
    │   │   └─ per tick: div absolute left `${unit/n*100}%` top 0 bottom 0
    │   │       ├─ [major] span label: absolute top 0; left 0 if unit==0; right 0 if unit>=n; else translateX(-50%);
    │   │       │     fontSize 11; text.tertiary; tabular-nums; nowrap; pointerEvents none — elapsedLabel(unit, n)
    │   │       └─ tick div: absolute bottom 0; left (0 if first | −1 if last | −0.5); width 1;
    │   │             height major 7 / minor 4; background major text.tertiary / minor text.quaternary (rgba(255,255,255,0.28) | rgba(0,0,0,0.25))
    │   └─ STRIP div.lk-ed-strip  tabIndex 0; role group; aria-label "Timelapse timeline. Drag to remove a stretch of time.";
    │        relative; height 56; borderRadius 8; overflow hidden; cursor crosshair;
    │        background editor.track (rgba(255,255,255,0.1) | rgba(0,0,0,0.08)); border 1px solid border.default
    │       ├─ tiles div absolute inset 0; flex; pointerEvents none → img × N: width tileWidth; height 100%; flex 0 0 auto; objectFit fill
    │       ├─ gap markers (title "Recording paused here"): absolute left pct(i); top/bottom 0; width 2; marginLeft −1;
    │       │     background repeating-linear-gradient(180deg, var(--color-text-quaternary) 0 3px, transparent 3px 6px); pointerEvents none
    │       └─ regions (raw, un-normalized list) div.lk-ed-region  absolute; left pct(start); width pct(end−start); top/bottom 0;
    │             borderRadius 6; backgroundColor editor.cutFill (rgba(248,113,113,0.26) | rgba(220,38,38,0.20));
    │             backgroundImage hatch(10) (stripe rgba(248,113,113,0.13) | rgba(220,38,38,0.12), 5px on/5px off at 45°);
    │             boxShadow selected ? inset 0 0 0 2px cutBorder : inset 0 0 0 1px cutBorder; cursor grab; box-sizing border-box
    │             └─ 2 handles div.lk-ed-handle  absolute top/bottom 0; width 12; left −6 (start) / right −6 (end);
    │                  cursor ew-resize; flex centred
    │                  └─ div.lk-ed-grip 3×22; borderRadius 2; background cutBorder
    ├─ ACTIONS div  flex; alignItems center; gap 12; flexWrap wrap; marginTop 4
    │   ├─ div minWidth 0; fontSize 14; text.primary; weight 600; letterSpacing -0.01em
    │   │     <MinutesFlow keptUnits/> " kept"
    │   │     [removed>0] span(color cutBorder, weight 500): " · " <MinutesFlow removedUnits color=cutBorder/> " removed"
    │   ├─ div flex 1; minWidth 12 (spacer)
    │   ├─ [selected !== null] Button secondary sm "Remove cut"
    │   ├─ [normalized.length > 0] Button secondary sm "Clear all"
    │   └─ Button primary sm onClick save; loading=saving; disabled=allCut;
    │        title allCut ? "You can't remove the entire timelapse" : undefined; label saving ? "Saving…" : "Save"
    └─ [saveError] ErrorDisplay banner title "Couldn't save your edits"
```
`allCut = n > 0 && keptUnits === 0` (TimelapseEditor.tsx:803). `inCutNow = regionAtTime(time, normalized) !== null` (:811).

**Pointer interactions** (TimelapseEditor.tsx:553-704)
- `unitFromEvent`: `frac = clamp((clientX − strip.left) / strip.width, 0, 1)`; `unitF = frac × n` (always relative to the STRIP's rect, even for ruler events).
- All drags `setPointerCapture`, `preventDefault`, `stopPropagation`. Disabled while `saving`.
- Ruler / playhead pointerdown → `scrub`: seek immediately and on every move.
- Strip pointerdown on open track → `maybe` (records `downUnitF`). On move, if travel ≥ 0.34 units → creates region `[floor(min), max(ceil(max), floor(min)+1))`, selects it, and converts into a region drag with mode `"end"` if moving right else `"start"`, anchor `floor(downUnitF)`. On up without travel → seek to the press point and deselect.
- Region body pointerdown → select + `move` drag with `grabOffset = unitF − start`; move: `start = clamp(round(unitF − grabOffset), 0, n − width)`. No seek while moving.
- Start handle → `start = clamp(round(unitF), 0, end − 1)`; seeks to `start + 0.02` live.
- End handle → if `round(unitF) <= anchor` the region flips: `start = max(0, rounded)`, `end = anchor + 1`, seek `start + 0.02`; else `end = min(n, max(start + 1, rounded))`, seek `min(n − 0.05, end + 0.02)`.
- Pointer up after region drag → `normalizeRegions` (merging) and re-select the region containing the dragged region's start (TimelapseEditor.tsx:671-686).
- Video click → toggle play.

**Playback** — rAF loop reads `video.currentTime` every frame into `time` (TimelapseEditor.tsx:331-339). On `timeupdate` while playing, entering a cut region jumps to `region.endUnit`; if the region runs to the end → pause and seek to `region.startUnit` (TimelapseEditor.tsx:342-358). Play from inside a region first jumps to its end (TimelapseEditor.tsx:706-716).

**Keyboard** (window `keydown`, capture phase; ignored if target is INPUT/TEXTAREA or meta/ctrl held; TimelapseEditor.tsx:734-765)
| key | action |
|---|---|
| Space, `k` | toggle play |
| `x`, `c` | "cut here": add region `[at, at+1)` at `unitAtTime(currentTime)`, normalize, select containing region |
| Delete, Backspace | remove selected region (raw index), deselect |
| Escape | deselect |
| ArrowLeft / ArrowRight | seek −/+1 unit; with Shift ±10 |
All call preventDefault (prevents the desktop router's Backspace-goes-back).

**Focus**: `.lk-ed-strip:focus-visible` and `.lk-ed-iconbtn:focus-visible` → `outline none; box-shadow 0 0 0 2px var(--color-bg-body), 0 0 0 4px var(--color-accent)` (editorStyles.ts:20-23, 57-60).
**Hover**: `.lk-ed-region:hover` background-color → cut-fill-hover (rgba(248,113,113,0.36) | rgba(220,38,38,0.30)); `.lk-ed-handle:hover .lk-ed-grip` → scaleX(1.6); `*:hover > .lk-ed-playhead` → scaleX(1.25); `*:active > .lk-ed-playhead` → scaleX(1.1); `.lk-ed-iconbtn:hover` → bg bg.surface, border border.hover; `.lk-ed-iconbtn:active` → scale(0.94) (editorStyles.ts:29-56).
`.lk-ed-fade-in` class is defined (editorStyles.ts:62-66) but is not applied anywhere in TimelapseEditor.tsx.

**Save** (TimelapseEditor.tsx:768-781): `saving=true`; cuts = `regionsToCuts(normalizeRegions(regions), units)` → `client.setCuts(cuts)` → `client.applyCuts()` → `onApplied(result)`; error → `saveError` message, `saving=false`. `onCutsChange(serializedCuts, dirty)` fires whenever the serialized list changes, `dirty = JSON.stringify(serialized) !== JSON.stringify(data.cuts ?? [])` (TimelapseEditor.tsx:816-825).

---

### 3C.6 VideoPlayer (`clients/react/src/components/VideoPlayer.tsx` + `VideoPlayer.css`)
Not used by the editor (editor uses a bare `<video>`); used by session detail views. Recorded here as requested.
- Wrapper `div.lookout-video-player.platform-{linux|windows|mac}` (UA sniff; note `"win"` match, VideoPlayer.tsx:24-33): width/height 100%; flex; relative; `--media-border-radius: 8px; --media-video-border-radius: 8px` (VideoPlayer.tsx:87-100).
- `@videojs/react` `createPlayer({ features: videoFeatures })`, `<VideoSkin style={{width:"100%",height:"100%"}}><Video src muted playsInline autoPlay={false}/></VideoSkin>`; imports `@videojs/react/video/skin.css` (VideoPlayer.tsx:2-5, 16, 102-106). Skin animations come from the video.js v10 skin CSS (third-party).
- CSS (VideoPlayer.css): hide `.media-button--mute`, `.media-slider--volume` always (:1-4); hide `.media-button--pip` on all three platforms (:7-11); hide `.media-button--fullscreen` on Linux (:14-16); `--media-border-radius: 8px` on wrapper and all descendants (:18-25).
- Error overlay (on `<video>` error, VideoPlayer.tsx:43-64): `div.lookout-video-player__error` role alert — absolute inset 0; flex column centred; gap 8; padding 24; textAlign center; background rgba(11, 13, 18, 0.92); color #f4f5f7; radius 8; zIndex 2 (VideoPlayer.css:27-41). `h3` 18px/600 margin 0 (:43-47); `p` 14px, lineHeight 1.5, color #c8ccd4, maxWidth 480 (:49-55); note 13px #9aa0aa (:57-60); link #6ea8ff underline (:62-65).
  Strings: h3 **"Can't play this timelapse"**; detail by code — 3/4: **"Your system can't decode this video (H.264). On Linux, install gst-plugins-bad / OpenH264 to enable playback."**; 2: **"Network error while loading the video. Check your connection and retry."**; other: `el.error.message` or **"Playback failed for an unknown reason."**; note: **"Your timelapse saved successfully. You can [download the file] and play it in another app."** (link `href=src target=_blank`).

## 4. Animations

This section collects every motion in the app. Sections 4.A–4.E are complete per-area tables. Every row gives the element, property, duration, exact easing, delay, keyframes, trigger and `file:line`. The library defaults behind them are in section 1.11 (motion springs, vaul, NumberFlow, squircle).

### 4.0 Motion primitives cheat-sheet (the ones reused everywhere)
| Primitive | Exact definition | Source |
|---|---|---|
| Button press | `whileTap` → `scale: 0.96` (+ hover background/border), `transition: { type: "spring", stiffness: 1500, damping: 60 }` (mass 1; ζ≈0.775, settles ≈120–150 ms); background clipped to a squircle with `cornerSmoothing 0.7` | `clients/react/src/ui/Button.tsx:64,85,90,92` |
| motion fallback for unspecified keys | `scale` → spring stiffness 550 / damping 30 (critical 2√550 when target 0); x/y/rotate → spring 500/25; opacity/colors → `cubic-bezier(0.25,0.1,0.35,1)` 0.3s; named `"easeOut"` = `cubic-bezier(0,0,0.58,1)` | `node_modules/motion-dom/dist/es/animation/utils/default-transitions.mjs:3-38` |
| vaul drawers (Sheet, TipDrawer, UpdateDrawer, ProgramPanel) | open/close `transform` 0.5s `cubic-bezier(0.32, 0.72, 0, 1)`; overlay fade same curve; close if velocity > 0.4 px/ms or dragged ≥ 25% of height; unmount after 500 ms | `node_modules/vaul/dist/index.mjs:437-452,62` |
| NumberFlow digits | transform 900 ms `linear(…)` spring curve (full list in 1.11), opacity 450 ms `ease-out`, trend = sign(new − old) | `node_modules/number-flow/dist/lite.mjs:129-141` |
| macOS menu-bar clock digits | `.contentTransition(.numericText())` inside `withAnimation(.spring(response: 0.4, dampingFraction: 0.9))` | `clients/desktop/src-tauri/swift/lookout-tray/Sources/Tray.swift:68,170` |

Known quirks to reproduce, or to decide on deliberately (details in the tables):
- Overlay and NamingModal animate in but close instantly, with no exit animation.
- Button runs a CSS `background 0.15s` transition alongside the motion spring on the same layer.
- The `shimmer` skeleton keyframes are only injected once `LookoutProvider` mounts, which in the desktop app happens only inside RecordPage. Until then skeletons are static.
- The recording timer is not NumberFlow-animated. It is a rAF loop with interpolation (see 3A.11).

### 4.A Animations — design-system primitives, global CSS and third-party libraries

| name / element | property | duration | easing (exact) | delay | keyframes / values | trigger | file:line |
|---|---|---|---|---|---|---|---|
| `@keyframes pulse` (global) | opacity | per user | per user | — | `0%, 100% { opacity: 1 } 50% { opacity: 0.4 }` | any `animation: pulse …` | clients/desktop/index.html:63; dup clients/react/src/LookoutProvider.tsx:99 |
| `@keyframes spin` (global) | transform | per user | per user | — | `from rotate(0deg) to rotate(360deg)` | any `animation: spin …` | clients/desktop/index.html:64; dup LookoutProvider.tsx:100 |
| `@keyframes shimmer` | background-position | per user | per user | — | `0% { background-position: 200% 0 } 100% { background-position: -200% 0 }` | Skeleton | clients/react/src/LookoutProvider.tsx:101 (only defined once a LookoutProvider has mounted) |
| Spinner svg | transform (spin) | 1s | linear | 0 | spin, infinite; 90° arc (dasharray 62.83 / offset 47.12) | always while mounted | clients/react/src/ui/Spinner.tsx:24 |
| Skeleton | background-position (shimmer) | 1.5s | ease-in-out | 0 | shimmer, infinite; gradient `transparent → shimmer → transparent`, size 200% 100% | always | clients/react/src/ui/Skeleton.tsx:20-22 |
| ProgressRing progress arc | stroke-dashoffset | 0.25s | linear | 0 | `circumference × (1 − progress)` | progress prop change (~200 ms ticks) | clients/react/src/ui/ProgressRing.tsx:60 |
| ProgressRing percent | NumberFlow digits | 900ms transform / 450ms opacity | NumberFlow default linear() spring / ease-out | 0 | value `min(99, floor(p*100))` | value change | ProgressRing.tsx:77 |
| MinutesFlow h/m numerals | NumberFlow digits | 900ms / 450ms | NumberFlow defaults | 0 | `Xm` or `Xh Ym` | value change (continuous while dragging cut region) | clients/react/src/ui/MinutesFlow.tsx:30,36-37 |
| Button inner bg layer | scale | spring | `{type:"spring", stiffness:1500, damping:60}` mass 1 | 0 | idle 1 → hover 1 → active **0.96** | pointer down (`whileTap`); disabled/loading → none | clients/react/src/ui/Button.tsx:87-92 |
| Button inner bg layer | background, border (motion variants) | spring | same spring 1500/60 (motion interpolates colour) | 0 | idle → hover colours per variant (see tokens 1.10) | hover (`whileHover`) | Button.tsx:46-59,87-92 |
| Button inner bg layer | CSS `transition` | 0.15s | ease (CSS default) | 0 | `opacity 0.15s, background 0.15s, border-color 0.15s` (largely overridden by motion's per-frame inline writes) | — | Button.tsx:98 |
| Button disabled | opacity | instant | — | — | 1 → 0.6 | `disabled || loading` | Button.tsx:72 |
| Card (clickable) | scale | spring | stiffness 1500, damping 60 | 0 | 1 → **0.99** | tap | clients/react/src/ui/Card.tsx:48-49 |
| ErrorDisplay action/Copy/× button outline layer | scale | spring | stiffness 1500, damping 60 | 0 | 1 → 0.96 | tap | clients/react/src/ui/ErrorDisplay.tsx:56-58,73-75,90-92 |
| Overlay panel enter | opacity | 0.16s | motion `"easeOut"` = cubic-bezier(0, 0, 0.58, 1) | 0 | 0 → 1 | mount | clients/react/src/ui/Overlay.tsx:84-88 |
| Overlay panel enter | y (translateY) | spring | stiffness 420, damping 34, mass 0.7 | 0 | 10px → 0 | mount | Overlay.tsx:84-87 |
| Overlay panel enter | scale | spring (motion default for scale) | stiffness 550, damping 30, restSpeed 10 (`node_modules/motion-dom/dist/es/animation/utils/default-transitions.mjs:9-14`) | 0 | 0.99 → 1 | mount | Overlay.tsx:84-85 |
| Overlay backdrop | — | none | — | — | appears/disappears instantly; panel has **no exit animation** | mount/unmount | Overlay.tsx:59-73 |
| Linux CSD #root | box-shadow | 160ms | ease-out | 0 | focused ↔ backdrop shadow sets (tokens 1.7) | window focus change (`.lookout-backdrop`) | clients/desktop/src/linuxChrome.ts:222,226-231 |
| Linux header bar | opacity | 160ms | ease-out | 0 | 1 ↔ 0.55 | window focus change | linuxChrome.ts:257-262 |
| Linux blur plate (no CSD) | background-color | 200ms | ease-out | 0 | opaque body colour → 80% `color-mix` plate | `.lookout-blur` added after compositor confirms | linuxChrome.ts:283-288 |
| Linux blur plate (CSD) | background-color, box-shadow | 200ms / 160ms | ease-out / ease-out | 0 | as above on #root | same | linuxChrome.ts:295-298 |
| Linux blur re-sync | debounce | 120ms | — | — | `setTimeout(sync, 120)` | resize/class change | linuxChrome.ts:492 |
| `.lookout-headerbar-control` (close btn) | background | 100ms | ease-out | 0 | `--color-headerbar-control` → hover `--color-headerbar-control-hover` → active `--color-border-hover` | hover/active | clients/desktop/src/components/HeaderBar.tsx:41-48 |
| `.lookout-headerbar-action` | background, color | 100ms each | ease-out | 0 | transparent/`text-secondary` → hover `--color-headerbar-control`/`text-primary` → active `--color-headerbar-control-hover` | hover/active | HeaderBar.tsx:72-80 |
| `.lookout-app-row-bg` (SettingsPage rows) | background, transform | 0.12s each | ease-out | 0 | hover bg `var(--color-bg-selected)`; active `scale(0.96)` + selected bg | hover/active | clients/desktop/src/components/SettingsPage.tsx:286-292 and :964-970 |
| `@keyframes lookout-icon-fade-in` | opacity | (set by user element) | — | — | `from { opacity: 0 } to { opacity: 1 }` | app icon load | SettingsPage.tsx:963 |
| `.lk-ed-strip` | box-shadow | 180ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | focus-visible ring `0 0 0 2px var(--color-bg-body), 0 0 0 4px var(--color-accent)` | focus-visible | clients/react/src/components/editorStyles.ts:19-23 |
| `.lk-ed-region` | background-color, box-shadow | 140ms each | cubic-bezier(0.25, 1, 0.5, 1) | 0 | hover bg `var(--color-cut-fill-hover)` | hover | editorStyles.ts:25-29 |
| `.lk-ed-grip` | transform | 140ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | `scaleX(1.6)` when `.lk-ed-handle:hover` | hover | editorStyles.ts:33-34 |
| `.lk-ed-playhead` | transform | 120ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | parent hover `scaleX(1.25)`, parent active `scaleX(1.1)` | hover/active | editorStyles.ts:39-41 |
| `.lk-ed-iconbtn` | background-color, border-color, transform | 140ms each | cubic-bezier(0.25, 1, 0.5, 1) | 0 | hover bg `--color-bg-surface`, border `--color-border-hover`; active `scale(0.94)` | hover/active | editorStyles.ts:43-56 |
| `@keyframes lk-ed-fade` / `.lk-ed-fade-in` | opacity, transform | 160ms | cubic-bezier(0.25, 1, 0.5, 1), fill `both` | 0 | `from { opacity: 0; transform: translateY(4px) } to { opacity: 1; transform: none }` | mount | editorStyles.ts:62-66 |
| editor reduced motion | all above | 1ms | — | — | hover transforms removed | `prefers-reduced-motion: reduce` | editorStyles.ts:71-79 |
| ErrorBoundary "Copied!" | label swap | 2000ms | — | — | "Copied!" → "Copy Error Report" | after copy | clients/desktop/src/main.tsx:90 |
| **vaul drawer** open | transform (CSS animation) | 0.5s | cubic-bezier(0.32, 0.72, 0, 1) | 0 | `slideFromBottom: from translate3d(0, var(--initial-transform,100%), 0) to translate3d(0,0,0)` (Top/Left/Right analogues) | `data-state=open` | node_modules/vaul/dist/index.mjs:62 |
| vaul drawer close | transform (CSS animation) | 0.5s | cubic-bezier(0.32, 0.72, 0, 1) | 0 | `slideToBottom: to translate3d(0, var(--initial-transform,100%), 0)`; unmount after 500 ms | `data-state=closed` | index.mjs:62, :752, :891, :1183 |
| vaul drawer base | transform transition | 0.5s | cubic-bezier(.32, .72, 0, 1) | 0 | used for snap/reset | release | index.mjs:62, :1189-1192 |
| vaul overlay | opacity (CSS animation) | 0.5s | cubic-bezier(0.32, 0.72, 0, 1) | 0 | `fadeIn {from{opacity:0} to{opacity:1}}`, `fadeOut {to{opacity:0}}` | open/close | index.mjs:62 |
| vaul drag | transform/opacity | live (transition none) | — | — | drawer follows pointer 1:1 toward close; overdrag dampened `8 * (ln(v + 1) − 2)` (index.mjs:413-415); overlay opacity `1 − dragged/height` | pointer drag | index.mjs:1060-1104 |
| vaul release | close decision | — | — | — | close if velocity > 0.4 px/ms, or swipe ≥ 0.25 × visible height; else reset with 0.5s cubic-bezier(0.32,0.72,0,1) | pointer up | index.mjs:446-447, :1225-1275 |
| vaul handle | opacity | instant | — | — | 0.7 → 1 on hover/active; 32×5, `#e2e2e4`, radius 1rem | hover | index.mjs:62 |
| vaul background scale (unused unless `shouldScaleBackground`) | transform, border-radius | 0.5s | cubic-bezier(0.32,0.72,0,1) | 0 | `scale((W−26)/W) translate3d(0, calc(env(safe-area-inset-top) + 14px), 0)`, radius 8 | open | index.mjs:720-736 |
| **NumberFlow** digit roll (default) | transform | 900ms | `linear(0,.005,.019,.039,.066,.096,.129,.165,.202,.24,.278,.316,.354,.39,.426,.461,.494,.526,.557,.586,.614,.64,.665,.689,.711,.731,.751,.769,.786,.802,.817,.831,.844,.856,.867,.877,.887,.896,.904,.912,.919,.925,.931,.937,.942,.947,.951,.955,.959,.962,.965,.968,.971,.973,.976,.978,.98,.981,.983,.984,.986,.987,.988,.989,.99,.991,.992,.992,.993,.994,.994,.995,.995,.996,.996,.9963,.9967,.9969,.9972,.9975,.9977,.9979,.9981,.9982,.9984,.9985,.9987,.9988,.9989,1)` | 0 | each digit strip translates Y by shortest wrap; direction = `Math.sign(new − old)` | value change | node_modules/number-flow/dist/lite.mjs:130-138 |
| NumberFlow spin | transform | = transformTiming (spinTiming undefined) | same | 0 | — | value change | lite.mjs:135, :334 |
| NumberFlow enter/exit digits & width | opacity; scaleX width | 450ms opacity; 900ms width | ease-out; transform linear() | 0 | entering/leaving digits fade; width change via `.number` scaleX + counter-scale | digit count change | lite.mjs:136; ssr-DvIINv8w.mjs (styles) |
| NumberFlow mask | static | — | — | — | vertical fade `0.25em` (`--number-flow-mask-height`), horizontal `0.5em` (`--number-flow-mask-width`) | — | ssr-DvIINv8w.mjs |
| NumberFlow reduced motion | — | — | — | — | animation disabled when `prefers-reduced-motion: reduce` (`respectMotionPreference: true`) | — | lite.mjs:139 |
| motion library defaults (reference) | spring | — | `stiffness 100, damping 10, mass 1.0`; rest `restSpeed 0.01/2`, `restDelta 0.005/0.5` (granular if |Δ|<5) | — | — | any `type:"spring"` without explicit params | node_modules/motion-dom/dist/es/animation/generators/spring.mjs:6-29 |
| motion per-value defaults (reference) | — | 0.3s (non-transform) | non-transform `[0.25, 0.1, 0.35, 1]`; x/y/rotate spring 500/25 restSpeed 10; scale spring 550/30 restSpeed 10 | — | — | omitted keys in a transition | node_modules/motion-dom/dist/es/animation/utils/default-transitions.mjs:3-38 |

### 4.B Animations — native surfaces, window chrome, Linux CSD

`ST` = `clients/desktop/src-tauri`, `DS` = `clients/desktop/src`.

| Name / element | Property | Duration | Easing (exact) | Delay | Keyframes / values | Trigger | Source |
|---|---|---|---|---|---|---|---|
| macOS menu-bar time text | text content (rolling digits) | spring | `.spring(response: 0.4, dampingFraction: 0.9)` wrapping the model write; text uses `.contentTransition(.numericText())` with `.monospacedDigit()` font 13.5pt | 0 | each changed digit rolls vertically (SwiftUI numericText: direction inferred from value increase/decrease) | every `lookout_tray_update` (≤1/s from ticker, or immediate on pause/resume), macOS ≥ 13 only; < 13 snaps | `ST/swift/lookout-tray/Sources/Tray.swift:65-68`, `:169-173` |
| macOS menu-bar pause glyph | insertion/removal of `Image(systemName:"pause.fill")` (9pt) in HStack | same spring | `.spring(response: 0.4, dampingFraction: 0.9)` (default SwiftUI opacity transition for the inserted view; HStack re-layout animated) | 0 | absent ↔ present | `paused` flips inside the same `withAnimation` | `Tray.swift:45-48`, `:170-173` |
| macOS menu-bar item width | `NSStatusItem.length` | none (set directly from preference) | — | — | follows measured SwiftUI width | width change | `Tray.swift:53-60`, `:111-114` |
| macOS add-menu panel fade-in | `NSPanel.alphaValue` 0 → 1 | 0.15 s | `NSAnimationContext` default timing (`ctx.duration = 0.15`, no explicit timingFunction ⇒ AppKit default ease-in-ease-out) | next main-queue tick after `makeKeyAndOrderFront` (first frame commits at alpha 0, scale 0.85) | 0 → 1 | `show_add_menu` | `ST/swift/lookout-tray/Sources/AddMenu.swift:282-292` |
| macOS add-menu "fling" | `scaleEffect` 0.85 → 1, anchor `.topTrailing` (shadow scales with it) | spring | `.interpolatingSpring(stiffness: 1200, damping: 46, initialVelocity: 0)` (mass 1 ⇒ ζ = 46 / (2·√1200) ≈ 0.664, slight overshoot; panel has 28pt transparent margin for the overshoot) | same tick as fade | `appeared` false → true | `show_add_menu` | `AddMenu.swift:199`, `:293-296`, `:17` |
| macOS add-menu dismiss | `NSPanel.alphaValue` → 0 then `orderOut` | 0.17 s | AppKit default (`ctx.duration = 0.17`) | 0 | 1 → 0 (no scale-out) | select, Escape, resign key, replaced | `AddMenu.swift:343-357` |
| macOS add-menu row highlight | background fill `Color.primary.opacity(0.09)` ↔ clear; symbol color `.secondary` ↔ `.primary` | instant (no animation modifier) | — | — | — | hover / arrow keys | `AddMenu.swift:120-123`, `:162`, `:181-187` |
| GNOME pill paused state | panel-button fill/colours | 150 ms (`transition-duration: 150ms`) | St default | 0 | red recorder pill ↔ amber `inset 0 0 0 100px #f59e0b`; hover/focus `#fbaf24`; active/checked `#fdbc45` | pause/resume, hover | `ST/gnome-extension/stylesheet.css:31-48` |
| Linux visible window shadow | `box-shadow` of `#root` | 160 ms | `ease-out` | 0 | focused `0 0 0 1px var(--color-window-border), 0 2px 8px rgba(0,0,0,0.1), 0 8px 28px rgba(0,0,0,0.14)` ↔ backdrop `0 0 0 1px var(--color-window-border), 0 1px 4px rgba(0,0,0,0.06), 0 4px 16px rgba(0,0,0,0.09)` ↔ snapped `none` | window focus change (`.lookout-backdrop`), snap (`.lookout-snapped`) | `DS/linuxChrome.ts:218-239` |
| Linux snapped frame collapse | `inset` 40px → 0, `border-radius` 12 → 0 | instant (not in transition list) | — | applied after 100 ms debounce of resize | — | WM tiles/maximizes | `DS/linuxChrome.ts:198-200`, `:235-239`; `DS/components/WindowResizeHandles.tsx:182-185` |
| Linux header bar backdrop dim | `opacity` 1 ↔ 0.55 | 160 ms | `ease-out` | 0 | — | window focus change | `DS/linuxChrome.ts:257-262` |
| Linux blur plate fade | `background-color` (html, or `#root` when CSD) opaque → `color-mix(in srgb, var(--color-bg-body) 80%, transparent)` | 200 ms | `ease-out` | after native confirms blur; resync debounced 120 ms on resize | — | `.lookout-blur` added | `DS/linuxChrome.ts:283-298`, `:490-493` |
| Global keyframes `pulse` | `opacity` | defined by user | — | — | `0%, 100% { opacity: 1 } 50% { opacity: 0.4 }` | used by e.g. tray recording dot | `clients/desktop/index.html:63` |
| Global keyframes `spin` | `transform` | defined by user | — | — | `from { rotate(0deg) } to { rotate(360deg) }` | spinners | `clients/desktop/index.html:64` |
| Tray popup recording dot | `animation: pulse 1.5s ease-in-out infinite` | 1.5 s, infinite | `ease-in-out` | 0 | pulse keyframes above; dot 8×8, `border-radius:50%`, `colors.status.danger` | `controlMode === "recording"` | `DS/components/TrayApp.tsx:176-181` |
| Tray popup timer digits | NumberFlow per unit (`h`, `mm`, `ss`) | NumberFlow defaults (see NumberFlow section of spec) | NumberFlow defaults | — | `minimumIntegerDigits: 2` for m/s; colons `margin: 0 1px; position:relative; top:-1px` | `useRecorderClock` tick | `DS/components/TrayApp.tsx:9-32` |
| ErrorBoundary "Copied!" | label swap | 2000 ms timeout | — | — | "Copy Error Report" → "Copied!" → back | copy click | `DS/main.tsx:86-91` |
| Hash-router back fallback | — (timing) | 50 ms timeout | — | — | if hash still not gallery → `#/` | Escape/Backspace | `clients/react/src/hooks/useHashRouter.ts:87-91` |
| NamingModal / StopChoiceModal autofocus | — (timing) | 50 ms timeout | — | — | focus input | open | `DS/components/NamingModal.tsx:30`, `clients/react/src/components/StopChoiceModal.tsx:45` |
| Editor-open poll | — (timing) | 1000 ms interval | — | — | clears placeholder when editor window gone | editor open | `DS/components/EditorWindow.tsx:216-227` |
| Window-frame sync debounce | — | 100 ms trailing | — | — | `sync_window_frame` | `onResized` | `DS/components/WindowResizeHandles.tsx:182-185` |
| Blur sync debounce | — | 120 ms trailing | — | — | `sync_background_blur` | `onResized` / class change | `DS/linuxChrome.ts:490-493` |
| Native main-thread hops timeout | — | 500 ms | — | — | `recv_timeout` | `sync_window_frame`, `sync_background_blur` | `ST/src/window_shape.rs:84`, `ST/src/background_blur.rs:148` |
| Tray ticker | — | 1 s | — | — | writes tray text only when `(text, paused)` changed | recording | `ST/crates/lookout-recorder/src/ticker.rs:50-58` |
| Tray popup show/hide | — | instant (no animation) | — | — | `show()+set_focus()` / `hide()` | tray click, blur, action | `ST/src/tray.rs:183-283` |
| Main/editor/tray windows | — | no window open/close animations beyond OS defaults | — | — | — | — | `ST/tauri.conf.json`, `ST/src/tray.rs`, `DS/components/EditorWindow.tsx` |
| UpdatePill (seen in titlebar) | `background, color, border-color` | 0.15 s | `ease` | 0 | — | phase change / hover | `DS/components/UpdatePill.tsx:99` (full spec belongs to UpdatePill section) |

### 4.C Animations — main window, routing and main screens

Library: `motion` / `framer-motion` / `motion-dom` **v14.1.0** (`node_modules/motion/package.json`). Motion defaults (from `node_modules/motion-dom/dist/es/animation/utils/default-transitions.mjs` and `generators/spring.mjs`, `motion-utils/dist/es/easing/ease.mjs`):
- No transition given: transform props x/y/rotate → spring `{stiffness 500, damping 25, restSpeed 10}` (mass 1); `scale*` → spring `{stiffness 550, damping 30 (or 2·√550≈46.9 when target is 0), restSpeed 10}`; everything else (opacity, colors, height, padding…) → tween `duration 0.3s, ease cubic-bezier(0.25, 0.1, 0.35, 1)`; >2 keyframes → duration 0.8s.
- Explicit `{duration}` without `ease`: JS keyframes generator default ease `easeInOut` = cubic-bezier(0.42, 0, 0.58, 1) (`motion-dom/.../generators/keyframes.mjs:10`); WAAPI-accelerated path (opacity/transform on WebKit) defaults to `easeOut` (`motion-dom/.../waapi/start-waapi-animation.mjs:3`). Treat `{duration: X}` as easeInOut unless noted.
- Named eases: `easeOut` = cubic-bezier(0, 0, 0.58, 1); `easeIn` = cubic-bezier(0.42, 0, 1, 1); `easeInOut` = cubic-bezier(0.42, 0, 0.58, 1).
- Spring with only stiffness/damping: mass defaults 1.0; restSpeed default 2, restDelta 0.5 (granular for small ranges: 0.01 / 0.005).
- `AnimatePresence mode="sync"`: entering and exiting children animate simultaneously (overlapped). `mode="popLayout"`: exiting element is popped out of layout (absolutely positioned) so siblings reflow immediately with layout animations. `initial={false}`: first mount does not play `initial`.

| Name / element | Property | Duration / physics | Easing (exact) | Delay | Keyframes / values | Trigger | file:line |
|---|---|---|---|---|---|---|---|
| Route transition — enter (main window content, also editor placeholder) | x | spring stiffness 460, damping 36, mass 0.7 | spring | 0 | from x = direction>0 ? 14 : −14 → 0 (px) | route key change (`page:token` or `editor-open:token`); AnimatePresence mode "sync", initial false | D/App.tsx:1506-1522 |
| Route transition — enter | opacity | 0.16 s | easeOut cubic-bezier(0,0,0.58,1) | 0.04 s | 0 → 1 | same | D/App.tsx:1520 |
| Route transition — exit | x | spring 460 / 36 / 0.7 | spring | 0 | 0 → direction>0 ? −14 : 14 | same (outgoing page, overlaps incoming) | D/App.tsx:1523-1527 |
| Route transition — exit | opacity | 0.14 s | easeOut | 0 | 1 → 0 | same | D/App.tsx:1528 |
| Route direction rule | — | — | — | — | gallery→other +1; other→gallery −1; record→session +1; session→record −1; else +1 | — | D/App.tsx:1344-1354 |
| RecordPage picker ⇄ recorder slide (enter) | x / opacity | x spring 460/36/0.7; opacity 0.16 s | opacity easeOut | opacity 0.04 s | x ±14→0; opacity 0→1; direction +1 on source select, −1 on change source | captureSource set/cleared; AnimatePresence mode "sync" initial false | D/components/RecordPage.tsx:222-248, 291-315 |
| RecordPage picker ⇄ recorder slide (exit) | x / opacity | x spring 460/36/0.7; opacity 0.14 s | easeOut | 0 | x 0→∓14; opacity 1→0 | same | D/components/RecordPage.tsx:240-247, 307-314 |
| "Starting {program}…" pill — enter | opacity, y | opacity: default tween 0.3 s; y: default spring 500/25 | opacity cubic-bezier(0.25,0.1,0.35,1); y spring | 0 | opacity 0→1; y 8→0 | `startingProgram` set (instant-start in flight) | D/App.tsx:1462-1467 |
| "Starting {program}…" pill — exit | opacity, y | 0.12 s (both) | easeInOut cubic-bezier(0.42,0,0.58,1) (explicit duration, no ease) | 0 | opacity →0; y →8 | `startingProgram` cleared | D/App.tsx:1467 |
| AddMenuPopup presence wrapper | — | (popup's own variants; see AddMenuPopup part) | — | — | AnimatePresence lets it exit-animate | `addMenu` state toggles | D/App.tsx:1401-1412 |
| Header bar control (close) hover | background | 100 ms | ease-out (CSS keyword = cubic-bezier(0,0,0.58,1)) | 0 | headerbar-control → headerbar-control-hover; :active → border-hover | :hover/:active | D/components/HeaderBar.tsx:158-165 |
| Header bar action (back/gear/+) hover | background, color | 100 ms each | ease-out | 0 | transparent → headerbar-control; text.secondary → text.primary; :active bg headerbar-control-hover | :hover/:active | D/components/HeaderBar.tsx:189-197 |
| Header bar backdrop dim (Linux) | opacity | 160 ms | ease-out | 0 | 1 → 0.55 when window unfocused (`html.lookout-backdrop`) | window focus loss | D/linuxChrome.ts:257-261 |
| Button background layer — hover/press | scale, background, border | spring stiffness 1500, damping 60 (mass 1); plus CSS `transition: opacity 0.15s, background 0.15s, border-color 0.15s` (CSS default ease = cubic-bezier(0.25,0.1,0.25,1)) | spring + CSS ease | 0 | idle scale 1 / hover scale 1 + hover bg / active (tap) scale 0.96 + hover bg | whileHover / whileTap (disabled: none) | R/ui/Button.tsx:62-99 |
| Button squircle shape | static | — | — | — | `@squircle-js/react` cornerSmoothing 0.7, cornerRadius = radius (default 8) | — | R/ui/Button.tsx:85 |
| Spinner rotation | transform | 1 s, infinite | linear | 0 | `@keyframes spin { from {rotate(0deg)} to {rotate(360deg)} }` | always | R/ui/Spinner.tsx:24; clients/desktop/index.html:64 |
| Recording dot pulse | opacity | 1.5 s, infinite | ease-in-out (cubic-bezier(0.42,0,0.58,1)) | 0 | `@keyframes pulse { 0%,100% {opacity 1} 50% {opacity 0.4} }` | controlMode "recording" | D/components/DesktopRecorder.tsx:342-345; clients/desktop/index.html:63 |
| Skeleton shimmer | background-position | 1.5 s, infinite | ease-in-out | 0 | `@keyframes shimmer { 0% {background-position: 200% 0} 100% {background-position: -200% 0} }` on `linear-gradient(90deg, transparent 0%, shimmer 50%, transparent 100%)` size 200% 100%. **Keyframes only exist after a LookoutProvider mounted (RecordPage recorder branch); before that skeletons are static.** | always (while skeleton shown) | R/ui/Skeleton.tsx:22; R/LookoutProvider.tsx:93-104 |
| Recorder timer tick | text | requestAnimationFrame loop; re-render only when integer second changes | none (hard text swap, tabular-nums) | — | display = base + min(75, floor((min(now, hold) − anchor)/1000)) | clock running & no hold | D/recorder.ts:192-228 |
| SourcePicker preview image — enter | opacity | 0.22 s | easeOut | 0 | 0 → 1 | new selected source image mounts | D/components/SourcePicker.tsx:114-118 |
| SourcePicker preview image — enter | scale | spring stiffness 360, damping 32, mass 0.7 | spring | 0 | window source 1.08 → 1; others 1.02 → 1 | same | D/components/SourcePicker.tsx:114,119 |
| SourcePicker preview image — exit | opacity, scale | opacity 0.22 s easeOut; scale spring 360/32/0.7 | — | 0 | opacity →0; scale →0.98 | source deselected; AnimatePresence mode "popLayout" initial false | D/components/SourcePicker.tsx:116, 471 |
| SourcePicker preview image — layout | layout (position/size when multi-select tiles reflow) | spring stiffness 420, damping 34, mass 0.75 | spring | 0 | FLIP between tile rects | selection count changes | D/components/SourcePicker.tsx:110,120 |
| SourcePicker preview spinner placeholder | layout | default layout transition (spring 500/25) | — | — | — | — | D/components/SourcePicker.tsx:102 |
| SourcePicker empty-preview text | opacity | default tween 0.3 s | cubic-bezier(0.25,0.1,0.35,1) | 0 | enter 0→1, exit →0 | selection becomes empty / non-empty | D/components/SourcePicker.tsx:489-499 |
| Window hover preview card — enter | opacity, scale | spring stiffness 420, damping 32, mass 0.7 (both props) | spring | 0 | opacity 0→1; scale 0.92→1 | hovering a window row (windows tab) that is not selected | D/components/SourcePicker.tsx:505-511 |
| Window hover preview card — exit | opacity | spring 420/32/0.7 | spring | 0 | → 0 | mouse leave / select | D/components/SourcePicker.tsx:510 |
| Hover card → main preview handoff | layout (shared layoutId) | inherits PreviewImage layout spring 420/34/0.75 | spring | 0 | `layoutId hover-to-main-preview-{id}` morphs hover-card image into the main preview tile on click | select hovered window | D/components/SourcePicker.tsx:476, 532; LayoutGroup id "preview-handoff" :469 |
| Source tab active indicator | layout (position/size) | spring stiffness 350, damping 35 | spring | 0 | `layoutId activeTabIndicator` slides between tabs | tab change | D/components/SourcePicker.tsx:567-575 |
| Source tab label weight/color | font-weight, color | instant | — | — | 500→600; text.secondary→text.primary | tab change | D/components/SourcePicker.tsx:558-560 |
| Source row press | scale (bg layer) | spring stiffness 1500, damping 60 | spring | 0 | 1 → 0.98 | whileTap | D/components/SourcePicker.tsx:671-673, 742-744, 804-806 |
| Source row selection | background/border | instant | — | — | surface/default → selected/selected | click | D/components/SourcePicker.tsx:676-677 |
| Shift-click hint pill — enter | opacity, y | spring stiffness 900, damping 30 | spring | 0 | opacity 0→1; y 5→0 | screens tab with >1 monitor, or windows tab with >1 window | D/components/SourcePicker.tsx:836-842 |
| Shift-click hint pill — exit | opacity, y | spring 900/30 | spring | 0 | opacity →0; y →10 | condition false | D/components/SourcePicker.tsx:840 |
| Source list / gallery scroll fade masks | mask-image | instant (toggled per scroll event) | — | — | 12px (picker) / 20px (gallery) transparent→black edge fades | scroll / resize / 10 ms after load (setTimeout) | D/components/SourcePicker.tsx:600-601, 272, 281; R/components/Gallery.tsx:130-136 |
| Preview polling loop | image swap | `max(16, floor(1000/fps))` ms cadence (1 fps → 1000 ms) minus fetch time | — | — | preloaded via `new Image()` then swapped (no fade) | live && document visible | D/hooks/useScreenPreview.ts:182-257 |
| Card press (gallery session card) | scale | spring stiffness 1500, damping 60 | spring | 0 | 1 → 0.99 | whileTap | R/ui/Card.tsx:48-49 |
| Session card archive button press | scale (bg layer) | spring 1500/60 | spring | 0 | 1 → 0.99 | whileTap | R/components/SessionCard.tsx:307-309 |
| Session card archive button hover | all (bg, border) on bg layer; color on icon wrapper | 0.15 s | CSS `ease` cubic-bezier(0.25,0.1,0.25,1) | 0 | bg --color-archive-bg → --color-archive-hover-bg; border → hover-border; icon color → var(--color-text-error) | JS mouseenter/leave | R/components/SessionCard.tsx:317-334 |
| ErrorDisplay banner buttons press | scale | spring 1500/60 | spring | 0 | 1 → 0.96 | whileTap | R/ui/ErrorDisplay.tsx:56-58, 73-75, 90-92 |
| Overlay (modal) panel — enter only | y | spring stiffness 420, damping 34, mass 0.7 | spring | 0 | 10 → 0 | mount | R/ui/Overlay.tsx:84-88 |
| Overlay panel — enter only | opacity | 0.16 s | easeOut | 0 | 0 → 1 | mount | R/ui/Overlay.tsx:88 |
| Overlay panel — enter only | scale | default scale spring stiffness 550, damping 30, restSpeed 10 (no explicit scale transition) | spring | 0 | 0.99 → 1 | mount | R/ui/Overlay.tsx:84-85 |
| Overlay — exit / backdrop | — | none | — | — | Backdrop appears/disappears instantly; panel unmounts instantly (not in AnimatePresence) | unmount | R/ui/Overlay.tsx:58-77 |
| ProgramPanel sheet open/close | transform translateY | vaul defaults (see vaul section): 0.5 s, cubic-bezier(0.32, 0.72, 0, 1) | — | — | slides from bottom; overlay fades | open prop true/false; host callback fires in `onAnimationEnd(false)` after exit | D/components/ProgramPanel.tsx:281-296 |
| ProgramPanel body height | height | spring stiffness 420, damping 34, mass 0.7; `initial={false}` | spring | 0 | → panel-reported height (clamped 220..6000) + 2 | `lookout:resize` message | D/components/ProgramPanel.tsx:431-439 |
| ProgramPanel loading cover crossfade | opacity | 0.18 s | easeOut | 0 | 1 → 0 (initial 1) | `lookout:ready` or 4000 ms grace timer | D/components/ProgramPanel.tsx:521-524; D/programPanel.ts:113 |
| ProgramPanel load deadline | state | setTimeout 12 000 ms | — | — | → broken state (no animation; content swaps) | iframe hasn't loaded | D/components/ProgramPanel.tsx:235-242; D/programPanel.ts:99 |
| ProgressRing (HoldPanel) | stroke-dashoffset | 0.25 s | linear | 0 | circumference×(1−p); estimate ticks every 250 ms: p = 1 − e^(−2.2·t/30000) | progress change | R/ui/ProgressRing.tsx:57; R/components/SessionDetail.tsx:57-73; R/hooks/buildProgress.ts:36-39 |
| SessionDetail rename — sizer padding | padding | spring stiffness 500, damping 30 | spring | 0 | "0 8px 0 0" ↔ "0 16px" | rename start/end | R/components/SessionDetail.tsx:440-444 |
| SessionDetail rename — input | padding, width | spring 500/30 | spring | 0 | padding "0" ↔ "0 8px"; width "100%" ↔ "max(100%, 300px)" | rename start/end | R/components/SessionDetail.tsx:477-485 |
| SessionDetail rename — input | backgroundColor, borderColor | 0.15 s | easeInOut (explicit duration) | 0 | transparent ↔ bg.surface; transparent ↔ border.selected | rename start/end | R/components/SessionDetail.tsx:480-487 |
| SessionDetail rename pencil icon — enter/exit | opacity, scale | 0.15 s | easeInOut (explicit duration) | enter is gated by a 600 ms setTimeout after rename ends | opacity 0↔1; scale 0.8↔1 | rename start (exit immediately) / end (+600 ms enter) | R/components/SessionDetail.tsx:208-220, 509-515 |
| SessionDetail rename pencil press | transform | 0.1 s | ease-in-out | 0 | scale(0.9) on mousedown; none on up/leave | mouse | R/components/SessionDetail.tsx:521-533 |
| SessionDetail date line | y | spring 500/30 | spring | 0 | −4 (not renaming) ↔ 0 (renaming) | rename start/end | R/components/SessionDetail.tsx:552-557 |
| SessionDetail status poll | state | setInterval 3000 ms | — | — | — | status stopped/compiling | R/components/SessionDetail.tsx:310-314 |
| Router back fallback | navigation | setTimeout 50 ms | — | — | force `#/` if history.back didn't reach gallery | Esc/Backspace | R/hooks/useHashRouter.ts:87-91 |
| Cold-start deep-link poll | — | 10 attempts, setTimeout 500 ms between | — | — | — | launch | D/App.tsx:853-866 |
| Post-edit redirect poll | — | first 2500 ms, ×1.5 per retry, cap 15 000 ms, deadline 35 min | — | — | — | EDITED_EVENT with status ≠ complete | D/App.tsx:412-430 |
| ErrorBoundary "Copied!" label | text | setTimeout 2000 ms | — | — | "Copied!" → "Copy Error Report" | copy click | D/main.tsx:88-91 |
| Camera start wait | — | one requestAnimationFrame before attaching stream | — | — | — | camera-start-request | D/hooks/useCameraBackend.ts:130 |

### 4.D Animations — secondary screens

Paths: `C/` = `clients/desktop/src/components/`, `H/` = `clients/desktop/src/hooks/`, `UI/` = `clients/react/src/ui/`.

**Library defaults used to resolve unspecified values** (verified in node_modules):
- motion 14.1.0 (`motion/react`, imported although not listed in clients/desktop/package.json — it is hoisted from @lookout/react). Named eases: `"easeOut"` = cubic-bezier(0, 0, 0.58, 1), `"easeInOut"` = cubic-bezier(0.42, 0, 0.58, 1) (`node_modules/motion-utils/dist/es/easing/ease.mjs:4-5`). A tween with only `duration` gets ease `"easeOut"` (`node_modules/motion-dom/dist/es/animation/interfaces/motion-value.mjs:27`); a tween with no duration gets 300 ms (`.../generators/keyframes.mjs:10`). Values with NO transition defined at all: opacity/colors → keyframes 0.3 s, ease cubic-bezier(0.25, 0.1, 0.35, 1); x/y → spring stiffness 500 damping 25 restSpeed 10; scale → spring stiffness 550 damping 30 (2·√550 when target 0) (`.../utils/default-transitions.mjs`). If a per-value transition object exists but lacks the key, the WHOLE transition object is used for that key (`.../utils/get-value-transition.mjs:3-9`), i.e. it is "defined" → tween 300 ms easeOut. Springs default mass 1.
- vaul 1.1.2 (`node_modules/vaul/dist/index.mjs`): `TRANSITIONS = {DURATION: 0.5, EASE: [0.32, 0.72, 0, 1]}` (`:438-439`); `VELOCITY_THRESHOLD 0.4` (px/ms), `CLOSE_THRESHOLD 0.25`, `SCROLL_LOCK_TIMEOUT 100`; overdrag dampening `8 * (ln(v + 1) - 2)` (`:413-415`); overlay opacity during drag = `1 - percentageDragged` (`:1080-1084`); release resets with `transform 0.5s cubic-bezier(0.32,0.72,0,1)` + overlay `opacity 0.5s` same curve (`:1189-1196`); close on release if velocity > 0.4 (`:1258`) or swipe ≥ 25 % of drawer height (`:1268`). Injected CSS (verbatim keyframes): `[data-vaul-drawer]{transition:transform .5s cubic-bezier(.32,.72,0,1);animation-duration:.5s;animation-timing-function:cubic-bezier(0.32,0.72,0,1)}`; bottom open → `@keyframes slideFromBottom{from{transform:translate3d(0,var(--initial-transform,100%),0)}to{transform:translate3d(0,0,0)}}`; bottom closed → `@keyframes slideToBottom{to{transform:translate3d(0,var(--initial-transform,100%),0)}}`; overlay `[data-vaul-overlay]` open → `@keyframes fadeIn{from{opacity:0}to{opacity:1}}`, closed → `@keyframes fadeOut{to{opacity:0}}`, both .5s cubic-bezier(0.32,0.72,0,1).
- @number-flow/react 0.6.2 defaults (`node_modules/number-flow/dist/lite.mjs:129-140`): `transformTiming {duration: 900, easing: linear(0,.005,.019,.039,.066,.096,.129,.165,.202,.24,.278,.316,.354,.39,.426,.461,.494,.526,.557,.586,.614,.64,.665,.689,.711,.731,.751,.769,.786,.802,.817,.831,.844,.856,.867,.877,.887,.896,.904,.912,.919,.925,.931,.937,.942,.947,.951,.955,.959,.962,.965,.968,.971,.973,.976,.978,.98,.981,.983,.984,.986,.987,.988,.989,.99,.991,.992,.992,.993,.994,.994,.995,.995,.996,.996,.9963,.9967,.9969,.9972,.9975,.9977,.9979,.9981,.9982,.9984,.9985,.9987,.9988,.9989,1)}`, `spinTiming` = transformTiming, `opacityTiming {duration: 450, easing: "ease-out"}`, `trend: sign(new-old)`, `respectMotionPreference: true`.

| Name / element | Property | Duration | Easing (exact) | Delay | Keyframes / values | Trigger | File:line |
|---|---|---|---|---|---|---|---|
| Sheet drawer enter | transform | 500 ms | cubic-bezier(0.32, 0.72, 0, 1) | 0 | translate3d(0,100%,0) → translate3d(0,0,0) (100% = Drawer.Content height incl. its 24px bottom padding) | `open` → true | C/Sheet.tsx:97-135; vaul CSS slideFromBottom |
| Sheet drawer exit | transform | 500 ms | cubic-bezier(0.32, 0.72, 0, 1) | 0 | → translate3d(0,100%,0) | `open` → false (X, overlay click, Esc, drag release) | vaul CSS slideToBottom |
| Sheet overlay enter / exit | opacity | 500 ms | cubic-bezier(0.32, 0.72, 0, 1) | 0 | 0→1 / →0 | with drawer | vaul CSS fadeIn/fadeOut; C/Sheet.tsx:104-114 |
| Sheet drag | transform translateY, overlay opacity | live | — | — | follows pointer; upward overdrag dampened 8·(ln(v+1)−2); overlay opacity 1−dragged% | pointer drag on sheet (not on `data-vaul-no-drag`) | vaul index.mjs:1037-1084 |
| Sheet drag release (snap back) | transform, overlay opacity | 500 ms | cubic-bezier(0.32,0.72,0,1) | 0 | → translate3d(0,0,0); overlay → 1 | release below thresholds | vaul index.mjs:1189-1196 |
| Sheet drag release (dismiss) | — | 500 ms | as exit | 0 | close if velocity > 0.4 px/ms or dragged ≥ 25 % height | release | vaul index.mjs:1258,1268 |
| Sheet clip-layer unmount | mount state | 600 ms timer | — | — | stays mounted 600 ms after close so exit can play | `open` false | C/Sheet.tsx:63-72 |
| TipDrawer hero image fade | opacity | 220 ms | ease-out (CSS = cubic-bezier(0,0,0.58,1)) | 0 | 0 → 1 (instant 1 if already `complete`) | img `load` | C/TipDrawer.tsx:70-71, :33-36 |
| UpdateDrawer Copy → Copied | label/icon swap | 1600 ms hold | — | — | Copy(CopyIcon) → Copied(CheckIcon bold) → back | click Copy | C/UpdateDrawer.tsx:25-34 |
| UpdatePill enter | opacity, y, scale | spring | spring stiffness 450, damping 32, mass 1 | 0 | opacity 0→1, y −8→0 (origin top) or +8→0 (origin bottom), scale 0.95→1 | phase leaves `idle` | C/UpdatePill.tsx:56, :65-66, :68 |
| UpdatePill exit | opacity, y, scale | 150 ms | easeOut cubic-bezier(0, 0, 0.58, 1) (tween, duration-only) | 0 | → opacity 0, y ±8, scale 0.95 | phase → `idle` | C/UpdatePill.tsx:67 |
| UpdatePill colors | background, color, border-color | 150 ms | ease (cubic-bezier(0.25,0.1,0.25,1)) | 0 | outlined (bg.surface, border.default, text.secondary) ↔ solid (color-mix text.primary 16 %, transparent border, text.primary); hover on ready 16 %→19 % | phase change / hover | C/UpdatePill.tsx:86-99 |
| UpdatePill progress ring | stroke-dashoffset | 200 ms | ease | 0 | 31.416·(1−p/100) | progress update | C/UpdatePill.tsx:32 |
| UpdatePill percent | digits (NumberFlow) | transform 900 ms / opacity 450 ms | transform linear(...) spring curve above; opacity ease-out | 0 | rolling digits, suffix "%", tabular-nums | progress change | C/UpdatePill.tsx:108-112 |
| UpdatePill demo ticker | progress | interval 80 ms | — | — | += 1 + rand·4 until ≥100 → ready | `__updatePillDemo()` (dev) | H/useAppUpdate.ts:147-161 |
| NamingModal panel enter | y | spring | spring stiffness 420, damping 34, mass 0.7 | 0 | 12 → 0 | mount | C/NamingModal.tsx:41-46 |
| NamingModal panel enter | opacity | 160 ms | easeOut cubic-bezier(0, 0, 0.58, 1) | 0 | 0 → 1 | mount | C/NamingModal.tsx:41-45 |
| NamingModal backdrop | — | none | — | — | appears instantly (no fade); no exit animation | mount/unmount | C/NamingModal.tsx:34-39 |
| NamingModal input focus | focus() | — | — | 50 ms setTimeout | — | mount (not tooShort) | C/NamingModal.tsx:27-31 |
| Overlay panel enter (CaptureErrorDialog) | y | spring | stiffness 420, damping 34, mass 0.7 | 0 | 10 → 0 | mount | UI/Overlay.tsx:263-267 |
| Overlay panel enter | opacity | 160 ms | easeOut cubic-bezier(0,0,0.58,1) | 0 | 0 → 1 | mount | UI/Overlay.tsx:263-268 |
| Overlay panel enter | scale | 300 ms | easeOut cubic-bezier(0,0,0.58,1) (falls back to the whole transition object → tween defaults) | 0 | 0.99 → 1 | mount | UI/Overlay.tsx:263-268 |
| Overlay backdrop | — | none | — | — | instant in/out; no exit animation | mount/unmount | UI/Overlay.tsx:238-252 |
| CaptureErrorDialog Copy/Copy report | label swap | 1600 ms hold | — | — | "Copy"→"Copied", "Copy report"→"Copied" | click | C/CaptureErrorDialog.tsx:37-57 |
| Settings subpage enter | x | spring | stiffness 460, damping 36, mass 0.7 | 0 | x +14 → 0 (drilling in) or −14 → 0 (back to menu) | subpage change | C/SettingsPage.tsx:362-367 |
| Settings subpage enter | opacity | 160 ms | easeOut cubic-bezier(0,0,0.58,1) | 40 ms | 0 → 1 | subpage change | C/SettingsPage.tsx:368 |
| Settings subpage exit | x | spring | stiffness 460, damping 36, mass 0.7 | 0 | 0 → −14 (drilling in) or +14 (back) | subpage change | C/SettingsPage.tsx:371-375 |
| Settings subpage exit | opacity | 140 ms | easeOut cubic-bezier(0,0,0.58,1) | 0 | 1 → 0 | subpage change | C/SettingsPage.tsx:376 |
| (Settings) AnimatePresence | mode | — | — | — | `mode="sync"` (enter & exit overlap, both absolute inset 0), `initial={false}` (no animation on first mount) | — | C/SettingsPage.tsx:354 |
| Settings/App row hover bg | background | 120 ms | ease-out | 0 | transparent → bg.selected | :hover | C/SettingsPage.tsx:290-292, :968-970 |
| Settings/App row press | transform, background | 120 ms | ease-out | 0 | bg layer scale 1 → 0.96 + bg.selected (only the background layer scales, not the content) | :active | C/SettingsPage.tsx:293, :971 |
| Filtered-app checkbox | all (border-color, background) | 150 ms | ease (CSS default) | 0 | unchecked (transparent, 1.5px border.hover) ↔ checked (#ef4444 fill + border) | toggle | C/SettingsPage.tsx:889 |
| Filtered-app icon fade-in | opacity | 200 ms | ease-out | 0 | `@keyframes lookout-icon-fade-in { from { opacity: 0; } to { opacity: 1; } }` — only when icon arrives after row mount | icon loaded | C/SettingsPage.tsx:73-76, :963 |
| Filtered-apps skeleton rows (12) | opacity | 1.2 s per cycle, repeat ∞ | easeInOut cubic-bezier(0.42, 0, 0.58, 1) (applied per segment) | i × 50 ms (i = 0…11) | [0.45, 0.9, 0.45] (keyframe times evenly spaced 0, 0.5, 1) | while list loading | C/SettingsPage.tsx:1014-1022 |
| Advanced/search input focus border | border-color | instant | — | — | border.default → border.hover on focus; back on blur (or danger if error) | focus/blur (JS) | C/SettingsPage.tsx:663-670, :991-996 |
| Button (all screens here) press | scale | spring | stiffness 1500, damping 60 | 0 | 1 → 0.96 on tap, background layer only (squircle) | whileTap | UI/Button.tsx:90-92 |
| Button hover/idle bg | background, border | spring (motion variants) + CSS 150 ms | spring 1500/60; CSS `opacity 0.15s, background 0.15s, border-color 0.15s` | 0 | idle ↔ hover bg (see Button spec) | whileHover | UI/Button.tsx:87-98 |
| Button loading spinner / Spinner | rotate | 1 s, infinite | linear | 0 | `@keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }` | loading | UI/Spinner.tsx:314; clients/desktop/index.html:64 |
| AddSessionPage launched spinner timeout | state | 15 000 ms | — | — | program button loading → re-enabled | after open program | C/AddSessionPage.tsx:85-89 |
| useTip / useAnnouncement poll | fetch | every 15 min | — | — | — | interval | H/useTip.ts:12; H/useAnnouncement.ts:10 |
| useLinuxUpdate poll | fetch | every 6 h | — | — | — | interval | H/useLinuxUpdate.ts:10 |
| useAppUpdate poll / held retry | check / install retry | 30 min / 30 s | — | — | — | interval | H/useAppUpdate.ts:15,17 |
| PermissionScreen / AnnouncementBanner / AddSessionPage / CaptureErrorDialog details | — | none | — | — | no own animations (only Button/Spinner/Overlay above; `<details>` toggles instantly) | — | — |

### 4.E Animations — Tray popup, Add menu, Editor window, Timelapse editor

Library defaults that apply below (verified in node_modules):
- **motion 14.1.0**: when a per-value transition is given without `ease`, motion uses `ease: "easeOut"` = `cubic-bezier(0, 0, 0.58, 1)` (node_modules/motion-dom/dist/es/animation/interfaces/motion-value.mjs:24-27; easeOut def node_modules/motion-utils/dist/es/easing/ease.mjs:4). Springs given only stiffness/damping use mass 1. `AnimatePresence` keeps the exiting element mounted until its `exit` finishes.
- **NumberFlow 0.6.2 defaults** (node_modules/number-flow/dist/lite.mjs:129-141): `transformTiming = { duration: 900, easing: "linear(0,.005,.019,.039,.066,.096,.129,.165,.202,.24,.278,.316,.354,.39,.426,.461,.494,.526,.557,.586,.614,.64,.665,.689,.711,.731,.751,.769,.786,.802,.817,.831,.844,.856,.867,.877,.887,.896,.904,.912,.919,.925,.931,.937,.942,.947,.951,.955,.959,.962,.965,.968,.971,.973,.976,.978,.98,.981,.983,.984,.986,.987,.988,.989,.99,.991,.992,.992,.993,.994,.994,.995,.995,.996,.996,.9963,.9967,.9969,.9972,.9975,.9977,.9979,.9981,.9982,.9984,.9985,.9987,.9988,.9989,1)" }`, `spinTiming = undefined` (falls back to transformTiming), `opacityTiming = { duration: 450, easing: "ease-out" }`, `trend = (old, new) => Math.sign(new - old)` (digits roll up when increasing, down when decreasing), `respectMotionPreference: true` (no animation under prefers-reduced-motion), animates only when document visible. No component in this section overrides these.

| name / element | property | duration | easing (exact) | delay | keyframes / values | trigger | file:line |
|---|---|---|---|---|---|---|---|
| Tray recording dot | opacity | 1.5s, infinite | ease-in-out | 0 | `@keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }` | always while `controlMode === "recording"` | clients/desktop/src/components/TrayApp.tsx:179; keyframes clients/desktop/index.html:63 |
| Tray timer digits (h, mm, ss) | NumberFlow digit roll (translateY) + opacity of entering/leaving digits | 900ms transform / 450ms opacity | transform: NumberFlow `linear(...)` above; opacity: ease-out | 0 | per-digit vertical roll, direction = sign(delta) | value change (every second while running) | TrayApp.tsx:17-29 |
| Tray timer source | integer seconds re-derived per frame | rAF loop | — | — | `deriveRecorderDisplaySeconds(clock, Date.now())`; setState only on change; loop stops when `!running` or `holdAtUnixMs != null` | clock change | clients/desktop/src/recorder.ts:192-228 |
| Button background layer (Tray Pause/Resume/Stop, editor Save/Remove cut/Clear all) | scale | spring | `type: "spring", stiffness: 1500, damping: 60` | 0 | idle `scale 1` → hover `scale 1` → active (pressed) `scale 0.96` | whileHover / whileTap (disabled: none) | clients/react/src/ui/Button.tsx:63-64, 86-92 |
| Button background layer | background, border | spring (same as above, motion variants) **and** CSS transition `opacity 0.15s, background 0.15s, border-color 0.15s` (CSS default easing `ease`) | spring 1500/60 + CSS ease | 0 | idle bg → hover bg (primary → accent-hover `color-mix(in oklab, accent 88%, black)`; secondary → bg.surface; ghost → bg.selected; success/danger/warning unchanged) | hover / press | Button.tsx:46-59, 87-98 |
| Button loading spinner | rotate | 1s, infinite | linear | 0 | `@keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }` | `loading` (editor Save while saving) | clients/react/src/ui/Spinner.tsx:24; clients/desktop/index.html:64 |
| AddMenuPopup panel enter | scale | spring | `type: "spring", stiffness: 1800, damping: 56` | 0 | `0.92 → 1`; origin top right (bottom right if flipped); Linux origin `calc(100% - tailCentreFromRight px) top|bottom` | mount (inside AnimatePresence) | clients/desktop/src/components/AddMenuPopup.tsx:212-218, 264-268 |
| AddMenuPopup panel enter | opacity | 0.1s | easeOut `cubic-bezier(0, 0, 0.58, 1)` (motion default when ease omitted) | 0 | `0 → 1` | mount | AddMenuPopup.tsx:212-219 |
| AddMenuPopup panel exit | opacity only (no scale) | 0.1s | `"easeOut"` = `cubic-bezier(0, 0, 0.58, 1)` | 0 | `1 → 0` | unmount via AnimatePresence (select, Escape, click-away, blur, + toggle) | AddMenuPopup.tsx:215; AnimatePresence clients/desktop/src/App.tsx:1402-1411 |
| AddMenuPopup row highlight | background | instant (no transition) | — | — | transparent ↔ hover tint | hover / arrow keys | AddMenuPopup.tsx:77-81 |
| EditorOpenPlaceholder | opacity | 0.15s | ease | 0 | `1 → 0.6` while focusing editor, back to 1 | click | clients/desktop/src/components/EditorWindow.tsx:153-154 |
| Editor play overlay (56px circle) enter | opacity, scale | 0.16s | `[0.25, 1, 0.5, 1]` = cubic-bezier(0.25, 1, 0.5, 1) (ease-out-quart) | 0 | opacity 0→1, scale 0.88→1 | video paused (`!playing`) | clients/react/src/components/TimelapseEditor.tsx:938-944 |
| Editor play overlay exit | opacity, scale | 0.16s | cubic-bezier(0.25, 1, 0.5, 1) | 0 | opacity 1→0, scale 1→0.88 | video plays | TimelapseEditor.tsx:943-944 |
| Editor "Will be removed" stage ring + badge enter/exit | opacity | 0.12s | easeOut `cubic-bezier(0, 0, 0.58, 1)` (motion default) | 0 | 0 ↔ 1 | playhead enters/leaves a normalized cut region | TimelapseEditor.tsx:975-981 |
| Editor playhead position | left % | per frame (rAF) | none (direct) | — | `left = (min(time, n)/n)*100%`, time = video.currentTime each frame | rAF loop always running while mounted | TimelapseEditor.tsx:331-339, 1075 |
| `.lk-ed-strip` focus ring | box-shadow | 180ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | none → `0 0 0 2px var(--color-bg-body), 0 0 0 4px var(--color-accent)` | :focus-visible | clients/react/src/components/editorStyles.ts:8, 19-23 |
| `.lk-ed-region` (cut region) | background-color, box-shadow | 140ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | bg cut-fill → cut-fill-hover on :hover; box-shadow inset 1px → inset 2px cutBorder when selected | hover / selection | editorStyles.ts:25-29; TimelapseEditor.tsx:1262-1266 |
| `.lk-ed-grip` (trim handle bar 3×22) | transform | 140ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | `scaleX(1) → scaleX(1.6)` | `.lk-ed-handle:hover` (12px hit area) | editorStyles.ts:33-34 |
| `.lk-ed-playhead` (9×13 cap) | transform | 120ms | cubic-bezier(0.25, 1, 0.5, 1) | 0 | `scaleX(1.25)` when parent (22px hit box) hovered; `scaleX(1.1)` when parent :active | hover / press | editorStyles.ts:39-41 |
| `.lk-ed-iconbtn` (play/pause 30×30) | background-color, border-color, transform | 140ms each | cubic-bezier(0.25, 1, 0.5, 1) | 0 | hover: bg transparent→bg.surface, border default→hover; :active `scale(0.94)`; focus-visible ring (box-shadow, not transitioned on this class) | hover / press / focus | editorStyles.ts:43-60 |
| `.lk-ed-fade-in` (defined, unused) | opacity, transform | 160ms | cubic-bezier(0.25, 1, 0.5, 1), fill `both` | 0 | `@keyframes lk-ed-fade { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: none; } }` | (no element uses the class) | editorStyles.ts:62-66 |
| Reduced motion (editor) | transition/animation duration | 1ms | — | — | strip/region/grip/iconbtn/playhead transitions → 1ms; fade-in → 1ms; grip & playhead hover transforms disabled | `prefers-reduced-motion: reduce` | editorStyles.ts:71-79 |
| Kept / removed minutes (MinutesFlow) | NumberFlow digit roll | 900 / 450ms | NumberFlow defaults | 0 | `<h>h <m>m` digits roll | regions change (live during drags) | clients/react/src/ui/MinutesFlow.tsx:21-39; TimelapseEditor.tsx:1328-1332 |
| ProgressRing arc (preparing) | stroke-dashoffset | 0.25s | linear | 0 | `C × (1 − progress)` | progress state (updated every 200ms) | clients/react/src/ui/ProgressRing.tsx:57-59 |
| ProgressRing percent | NumberFlow digit roll | 900 / 450ms | NumberFlow defaults | 0 | `min(99, floor(p*100))` + "%" | progress change | ProgressRing.tsx:76 |
| Build-progress estimator | numeric value | tick every 200ms (setInterval) | `1 − exp(−2.2·t/est)`; between real anchors `from + budget·(1 − exp(−2.2·since/2000))`; cap 0.95; monotonic | — | est = 6000 + 90·units ms | while `preparingUnits !== null` | TimelapseEditor.tsx:277-315; clients/react/src/hooks/buildProgress.ts:2-85 |
| Preparing spinner (no unit count yet) | rotate | 1s infinite | linear | 0 | `spin` keyframes; 40px | loading before first /units response or preparing unknown | TimelapseEditor.tsx:861; Spinner.tsx:24 |
| ErrorDisplay banner buttons (Copy / action / ×) | scale | spring stiffness 1500, damping 60 | spring | 0 | 1 → 0.96 on tap | press | clients/react/src/ui/ErrorDisplay.tsx:56-99 |
| Filmstrip tile appearance | (no animation) tiles appear in batches | — | — | debounce 220ms after resize/data | tiles published every 3rd tile (`i % 3 === 2`), then full set | data load / strip resize | TimelapseEditor.tsx:430, 506, 531, 542 |

Timer / polling sequencing (non-visual but affects what is shown and when):
| what | interval / timeout | file:line |
|---|---|---|
| Editor `/status` poll while preparing | `setTimeout` 2000ms, repeating | TimelapseEditor.tsx:219, 248 |
| Edit-lease heartbeat | immediate, then `setInterval` 30_000ms (`EDIT_HEARTBEAT_SECONDS`) | clients/react/src/hooks/useEditLease.ts:41-42; packages/shared/src/cuts.ts:54 |
| Editor-window-open poll (main window) | `setInterval` 1000ms | clients/desktop/src/components/EditorWindow.tsx:219-222 |
| Filmstrip regeneration debounce | 220ms | TimelapseEditor.tsx:542 |
| Filmstrip `<video>` seek timeout | 4000ms | clients/react/src/hooks/filmstripFrames.ts:49, 212 |

## 5. Behaviours

`ST` = `clients/desktop/src-tauri`, `DS` = `clients/desktop/src`, `RS` = `clients/react/src`.

### 5.1 Platform detection

- `DS/platform.ts:9-13`: `isMacOS = ua.includes("Mac")`; `isWindows = ua.includes("Windows")`; `isLinux = !isMacOS && !isWindows && /\bLinux\b/.test(ua) && !/Android/i.test(ua)`. Several components re-derive `isMacOS` inline with `navigator.userAgent.includes("Mac")` (App.tsx:182, PageLayout.tsx:36, RecordPage.tsx:38, DesktopRecorder.tsx:111, EditorWindow.tsx:57/247, SettingsPage.tsx:126).
- `clients/desktop/index.html:16-35` (inline, before first paint): on Linux UA adds `html.os-linux`; adds `html.lookout-csd` unless `window.__LOOKOUT_SHELL_DRAWS_FRAME__ === true`; sets `data-theme` from `prefers-color-scheme`.
- `clients/desktop/index.html:128-133`: adds `body.os-windows` when UA contains `windows` **or `linux`**; CSS `body.os-windows { transform: rotate(0.001deg); }` (`index.html:71`, a compositing hack).
- `TrayApp` treats Windows **and Linux** UA as "isWindows" for its opaque fallback (`DS/components/TrayApp.tsx:60-62`).
- `window.__LOOKOUT_SHELL_DRAWS_FRAME__` is injected at document start by a Rust plugin `lookout-window-frame` via `js_init_script` (`ST/src/lib.rs:1092-1107`), value = `desktop_appearance::shell_draws_window_frame()`.

### 5.2 Platform differences matrix

| Concern | macOS | Windows | Linux | Source |
|---|---|---|---|---|
| Main window decorations | Overlay titlebar, hidden title, native traffic lights | native titlebar | **undecorated**, app-drawn HeaderBar (47px) | tauri.conf.json:30-31, lib.rs:1349-1350 |
| Window material | NSVisualEffect `Sidebar`, radius 16 (≥26) / 10 | Mica | opaque `--color-bg-body` (optional Wayland blur) | lib.rs:397-424, App.tsx:977-995 |
| Window size | 480×640 | 480×640 | 560×720 surface (40px transparent frame) with 480×640 visible window; 480×640 if shell draws frame | lib.rs:1376-1413 |
| Drag region | 32px top strip `data-tauri-drag-region` | none in webview (native titlebar) | HeaderBar root + title block + flexible spacer | App.tsx:1383-1390, HeaderBar.tsx:183,234,262 |
| Update pill | in 32px titlebar strip, right-aligned (`paddingRight:6`) | floating bottom-left (`bottom:8,left:8`), `origin="bottom"` | in header bar actions | App.tsx:1383-1398 |
| Back buttons | in-content `Button secondary sm` labelled `← Back` / `← Gallery` (HTML `&larr;`) | in-content with 14px chevron SVG (`path d="m15 18-6-6 6-6"`, stroke 2) or Phosphor `CaretLeftIcon size=14 weight=bold` + "Back"/"Gallery" | **none in content**; HeaderBar back button | PageLayout.tsx:48-64, RecordPage.tsx:252-265, SettingsPage.tsx:122-136 |
| Gallery header (title, gear, +) | in content | in content | moved to HeaderBar; `showHeader={!isLinux}`, `showBottomFade={!isLinux}` | App.tsx:1134-1141 |
| Session detail back | shown | shown | `showBack={!isLinux}` | App.tsx:1217 |
| Settings H2 heading | shown | shown | hidden (HeaderBar title) | SettingsPage.tsx:141 |
| "+" add menu | native NSPanel (SwiftUI) via `show_add_menu` | DOM `<AddMenuPopup>` | DOM `<AddMenuPopup>` with Adwaita popover look (tail, 12px gap, 32px rows, 14.5px text, 6px radius) | App.tsx:650-689, AddMenuPopup.tsx:71-101, 224-327 |
| Add-menu icon prefetch | `invoke("prefetch_add_menu_icons",{urls})` | `new Image().src = url` | same as Windows | App.tsx:524-529 |
| Recording indicator | NSStatusItem SwiftUI pill with `numericText` | tray icon + tooltip; click → tray popup window | GNOME pill (extension) or StatusNotifier icon with title + menu | tray.rs:45-68 |
| Tray popup window | yes, liquid glass Sidebar | yes, `rgba(26,26,26,0.93)` square | unreachable (no click events) | TrayApp.tsx:70-84,167-172; tray.rs:115-118 |
| Modal backdrop fill | `colors.bg.backdrop` flat | `linear-gradient(to bottom, transparent 0%, ${colors.bg.backdrop} 100%)` | flat; overlay inset to visible window (`[data-lookout-overlay]`) | backdrop.ts:15-17, linuxChrome.ts:246-254 |
| Window title text | `Lookout v<ver>` (hidden) | `Lookout v<ver>` | `Lookout` | App.tsx:961 |
| Permission gates (screen/camera) | yes (cached in localStorage `permCacheKey`) | skipped | skipped | App.tsx:182-210 |
| Cursor over controls | `pointer` where set | `pointer` | forced `default !important` on buttons/links/`[role=button]`/`[style*="cursor: pointer"]`; `text` on inputs | linuxChrome.ts:173-183 |
| Body font | `-apple-system, BlinkMacSystemFont, 'Geist', 'Segoe UI', Roboto, sans-serif` | same (Geist/Segoe UI) | `system-ui, 'Adwaita Sans', Cantarell, sans-serif` | index.html:62, 125-127 |
| Accent colour | app accent | app accent | GNOME `accent-color` mapped to Adwaita hex, applied via `setAccentColor(hex, accentForeground(hex))` | desktop_appearance.rs:77-91, linuxChrome.ts:573-575 |
| Preview URL scheme | `lookout-preview://localhost` | `http://lookout-preview.localhost` | `lookout-preview://localhost` | useScreenPreview.ts:106-107 |
| WebView camera/mic prompt | macOS permission plugin | auto-granted (WebView2 PermissionRequested handler) | n/a | windows_permissions.rs |
| App auto-update install | auto | auto | not auto (`canAutoInstall = armed && !isLinux`; Linux uses `useLinuxUpdate`) | useAppUpdate.ts:235, useLinuxUpdate.ts:42 |
| Deep-link registration | Apple Events | `register_all()` at startup | only AppImage/debug register; packages ship .desktop with `%u` | lib.rs:1283-1310 |
| Source picker | screens/windows/cameras | same | Wayland: portal flow, tabs forced (`hasTabs = hasWindows || hasCameras || isWayland`) | SourcePicker.tsx:425-429 |
| DesktopRecorder back label | `"← Gallery"` | `"Back to Gallery"` | `"Back to Gallery"` | DesktopRecorder.tsx:298 |

### 5.3 Native app menu (macOS only; `ST/src/lib.rs:1188-1293`)

| Menu | Items (in order) |
|---|---|
| **Lookout** | About Lookout (AboutMetadata: name "Lookout", version from config, authors ["Hack Club"], copyright "© 2026 Hack Club, A 501(c)(3) nonprofit project for student makers.", license "MIT", website "https://fallout.hackclub.com", website label "Hack Club Fallout") · separator · Services · separator · Hide Lookout (⌘H) · Hide Others · Show All · separator · Quit Lookout (⌘Q) |
| **File** | Start Timelapse — id `start_timelapse`, accelerator `CmdOrControl+N` → emits `lookout-navigate` payload `"/add"` and focuses `main` |
| **Edit** | Undo · Redo · separator · Cut · Copy · Paste · Select All (predefined, standard accelerators) |
| **Window** | Minimize · Maximize · separator · Close Window |
| **Help** | Fallout Docs → `https://fallout.hackclub.com/docs` · How to Timelapse? → `https://fallout.hackclub.com/docs/project-resources/how-to-timelapse` · GitHub Repo → `https://github.com/hackclub/lookout/` |

Frontend handler: `listen<string>("lookout-navigate")`, payload `"/add"` → `navigate({page:"add"})` (`DS/App.tsx:940-951`). Windows/Linux have no app menu.

### 5.4 Tray / indicator menus

- **Linux StatusNotifier menu** (`ST/src/tray.rs:119-145`): `[ "Pause recording" | "Resume recording" (id tray_toggle_pause), separator, "Stop recording" (tray_stop), "Open Lookout" (tray_open) ]`. Label of the toggle flips on every `update` (`:322-330`). Actions → `tray_action("pause"|"resume"|"stop")`; Open → `focus_main_window`.
- **GNOME pill menu**: identical labels (`ST/gnome-extension/extension.js:80-99`); D-Bus methods `Pause/Resume/Stop/Open` (`extension.js:221-223`).
- **Windows**: no menu; left-click toggles the tray popup window.
- **macOS**: no menu; click toggles the tray popup window.
- `tray_action` semantics (`ST/src/tray.rs:358-411`): hides popup for any action except `"ready"`; `"ready"` → emits `recorder-state` with current state; `"pause"` → `recorder_pause`; `"resume"` → if `source_lost` is set, **focus main window instead** of resuming, else `recorder_resume`; `"stop"` → focus main window then `recorder_begin_stop` (naming prompt is in main); unknown → `Err("unknown tray action …")`.

### 5.5 Keyboard shortcuts (complete list)

| Key | Scope | Effect | Source |
|---|---|---|---|
| `` ` `` (backtick) | every window, global `keydown` | Toggle `#debug` log panel (fixed bottom, `background:#111; color:#0f0; font: 11px monospace; padding:0 8px 8px; max-height:40vh; z-index:99999; border-top:1px solid #333`) | index.html:127, 144-151 |
| `Escape` / `Backspace` | main window, not in INPUT/TEXTAREA/contentEditable, not `defaultPrevented`, route ≠ gallery | `history.back()`; after **50 ms**, if still not gallery, `location.hash = "#/"` | RS/hooks/useHashRouter.ts:58-97 |
| `Escape` | any `<Overlay>` with `onDismiss` | dismiss | RS/ui/Overlay.tsx:47-54 |
| `Escape` / `↑` / `↓` / `Enter` | DOM AddMenuPopup (document capture phase, `preventDefault`) | dismiss / move selection (wrap, skip separators; from none: ↓→first, ↑→last) / activate | DS/components/AddMenuPopup.tsx:146-184 |
| `Escape` / `↑` / `↓` / `Return` / keypad `Enter` | macOS native add menu (NSEvent local monitor, keyCodes 53/126/125/36/76) | same as above | AddMenu.swift:262-280 |
| `Enter` | AddSessionPage link input (not while loading) | start | DS/components/AddSessionPage.tsx:173-175 |
| `Enter` | NamingModal input (not while loading) | confirm name | DS/components/NamingModal.tsx:63-64 |
| `Enter` | StopChoiceModal input (not while loading) | confirm | RS/components/StopChoiceModal.tsx:96-97 |
| `Escape` | SessionDetail rename input | cancel rename + blur | RS/components/SessionDetail.tsx:470-473 |
| `Enter` / `Space` | EditorOpenPlaceholder (`role=button tabIndex=0`) | focus editor window | DS/components/EditorWindow.tsx:134-141 |
| `Space` / `k` | TimelapseEditor (window, capture phase, ignored in INPUT/TEXTAREA and with ⌘/Ctrl) | play/pause | RS/components/TimelapseEditor.tsx:735-764 |
| `x` / `c` | TimelapseEditor | cut at playhead | same |
| `Delete` / `Backspace` | TimelapseEditor | delete selected region | same |
| `Escape` | TimelapseEditor | clear selection | same |
| `←` / `→` (`Shift` = ×10) | TimelapseEditor | seek −/+1 s (10 s with Shift) | same |
| `⌘N` / `Ctrl+N` | macOS menu accelerator | Start Timelapse → add page | lib.rs:1224 |
| `⌘H`, `⌘Q`, Edit/Window menu accelerators | macOS predefined menu items | standard | lib.rs:1199-1258 |

No global (OS-level) shortcuts are registered.

### 5.6 Focus handling

- `NamingModal`: `setTimeout(() => inputRef.current?.focus(), 50)` on open (`DS/components/NamingModal.tsx:30`).
- `StopChoiceModal`: same 50 ms focus when `withName` (`RS/components/StopChoiceModal.tsx:45`).
- `Overlay`: focuses its panel (`tabIndex={-1}`) on mount and sets `document.body.style.overflow = "hidden"` (restored on unmount) (`RS/ui/Overlay.tsx:37-45`, `:83`).
- `SessionDetail` rename: focuses input when renaming starts; blur on Enter; `onBlur` ends renaming (`RS/components/SessionDetail.tsx:212, 432, 463`).
- `Card`: `role="button" tabIndex={0}` (`RS/ui/Card.tsx:45-47`); TimelapseEditor strip `tabIndex={0}` (`TimelapseEditor.tsx:1183`).
- `:focus-visible` styles exist for `.lookout-headerbar-control`, `.lookout-headerbar-action` (`DS/components/HeaderBar.tsx:49`, `:81`) and `.lk-ed-strip`, `.lk-ed-iconbtn` (`RS/components/editorStyles.ts:20`, `:57`) — see component specs.
- SettingsPage inputs use inline `onFocus`/`onBlur` to restyle borders (`DS/components/SettingsPage.tsx:663-668`, `:991-996`).
- Window focus tracking:
  - `useWindowFocus()` — `document.hasFocus()` + window `focus`/`blur` listeners (`DS/hooks/useWindowFocus.ts`), gates live preview polling.
  - `useBackdropState()` (Linux) — asks the **Tauri window** (`isFocused()` + `onFocusChanged`), not the document, because dragging by the header bar steals DOM focus; toggles `html.lookout-backdrop` when unfocused (`DS/linuxChrome.ts:371-410`).
  - `useDesktopAppearance()` (Linux) re-reads `desktop_appearance` every time the window regains focus (`DS/linuxChrome.ts:588-625`).
  - TrayApp re-syncs `recorder_state` on window `focus` and on `tray-opened` (`DS/components/TrayApp.tsx:112-124`).
  - DOM AddMenuPopup dismisses on window `blur` and on any `pointerdown` (capture) outside itself **except** inside the + button's rect (so the button's own click toggles) (`DS/components/AddMenuPopup.tsx:126-144`).
  - Rust: tray window hides on `Focused(false)`; preview frames only produced while `main` is focused (`ST/src/lib.rs:222-228`).
  - Main window refocus after deep link (`DS/App.tsx:761, 820`); editor close focuses `main` first (`EditorWindow.tsx:104-110`).

### 5.7 Window dragging regions

- macOS main: `div[data-tauri-drag-region].titlebar`, 32px tall, full width, z 9999 (`DS/App.tsx:1383-1390`). UpdatePill inside opts out with `WebkitAppRegion: "no-drag"` (`DS/components/UpdatePill.tsx:100`).
- macOS editor: 30px strip (`DS/components/EditorWindow.tsx:386-391`).
- Linux (main + editor) HeaderBar: the bar root, the title block (which has `pointerEvents:none`, so drags pass to the bar), and a `flex:1; alignSelf:stretch` spacer are all `data-tauri-drag-region` (`DS/components/HeaderBar.tsx:183, 234, 262`). Double-click toggles maximize only when `maximizable` (editor) (`:235-237`).
- Windows: OS titlebar.
- Capability `core:window:allow-start-dragging` required.

### 5.8 Linux client-side decorations (`DS/linuxChrome.ts`, `clients/desktop/index.html`)

Constants (`DS/linuxChrome.ts:34-117`):
| Name | Value |
|---|---|
| `HEADER_BAR_HEIGHT` | 47 (Adwaita 46 + 1px hairline) |
| `WINDOW_RADIUS` | 12 |
| `WINDOW_MARGIN` | 40 (transparent frame per side) |
| `RESIZE_BAND` | 8 (inner band of frame that still takes input) |
| `SHADOW_PASSTHROUGH` | `WINDOW_MARGIN − RESIZE_BAND` = 32 |
| `FRAME_INSET` | `SHELL_DRAWS_FRAME ? 0 : 32` |
| `DEFAULT_APPEARANCE` | `{accent: null, controlsOnRight: true}` |

First-paint CSS (`index.html:83-127`): `html{background:#242424}`, `html[data-theme=light]{#fafafa}`, `@media (prefers-color-scheme: light){html:not([data-theme=dark]){#fafafa}}`; `html.os-linux.lookout-csd{background:transparent}`; `html.os-linux.lookout-csd #root{position:fixed; inset:40px; height:auto; background:#242424; border-radius:12px; box-shadow: 0 0 0 1px rgba(255,255,255,0.18), 0 2px 8px rgba(0,0,0,0.1), 0 8px 28px rgba(0,0,0,0.14)}`; light: `background:#fafafa; box-shadow: 0 0 0 1px rgba(0,0,0,0.18), 0 2px 8px rgba(0,0,0,0.08), 0 8px 28px rgba(0,0,0,0.1)`. (Note: the injected sheet below uses `0.1/0.14` alphas in both themes, so after injection light mode has slightly stronger shadow than first paint.)

Injected stylesheet `style[data-lookout-linux-chrome]` appended to `<head>` at module import (`DS/linuxChrome.ts:132-299`, `:330-335`):

Adwaita variables
| Variable | dark (`html.os-linux[data-theme="dark"]`) | light |
|---|---|---|
| `--color-headerbar-control` | `rgba(255,255,255,0.1)` | `rgba(0,0,0,0.08)` |
| `--color-headerbar-control-hover` | `rgba(255,255,255,0.18)` | `rgba(0,0,0,0.16)` |
| `--color-popover-bg` | `#383838` | `#f7f7f7` |
| `--color-popover-bg-top` | `#3d3d3d` | `#ffffff` |
| `--color-popover-border` | `rgba(255,255,255,0.14)` | `rgba(0,0,0,0.12)` |
| `--color-popover-hover` | `rgba(255,255,255,0.1)` | `rgba(0,0,0,0.08)` |
| `--color-popover-separator` | `rgba(255,255,255,0.15)` | `rgba(0,0,0,0.12)` |
| `--color-window-border` | `rgba(255,255,255,0.18)` | `rgba(0,0,0,0.18)` |

Rules
- Cursor: `html.os-linux button, a, [role="button"], [style*="cursor: pointer"] { cursor: default !important }`; `input, textarea, [contenteditable="true"] { cursor: text !important }` (`:173-183`).
- `html.os-linux.lookout-csd, … body { background: transparent }`; `--lookout-window-radius: 12px; --lookout-window-margin: 40px` (`:188-194`); `.lookout-snapped` → `--lookout-window-radius: 0px` (`:198-200`).
- Visible window `html.os-linux.lookout-csd #root { position:fixed; inset:var(--lookout-window-margin); height:auto; background:var(--color-bg-body); border-radius:var(--lookout-window-radius); overflow:hidden; box-shadow: 0 0 0 1px var(--color-window-border), 0 2px 8px rgba(0,0,0,0.1), 0 8px 28px rgba(0,0,0,0.14); transition: box-shadow 160ms ease-out }` (`:206-223`). Shadow reach 8+28 = 36 < 40.
- Unfocused (`.lookout-backdrop #root`): `box-shadow: 0 0 0 1px var(--color-window-border), 0 1px 4px rgba(0,0,0,0.06), 0 4px 16px rgba(0,0,0,0.09)` (`:226-231`).
- Snapped/maximized (`.lookout-snapped #root`): `inset:0; border-radius:0; box-shadow:none` (`:235-239`).
- Overlays: `html.os-linux.lookout-csd [data-lookout-overlay] { inset: var(--lookout-window-margin) !important; border-radius: var(--lookout-window-radius); overflow:hidden }`; snapped → `inset:0 !important; border-radius:0` (`:246-254`).
- Header bar dim: `html.os-linux .lookout-headerbar { transition: opacity 160ms ease-out }`; `.lookout-backdrop .lookout-headerbar { opacity: 0.55 }` (`:257-262`).
- Blur plate (only when `.lookout-blur` present): `--lookout-blur-plate: color-mix(in srgb, var(--color-bg-body, #242424) 80%, transparent)` (light fallback `#fafafa`); `html.os-linux.lookout-blur { background: var(--lookout-blur-plate); transition: background-color 200ms ease-out }`; body transparent; with `.lookout-csd` html transparent and `#root` carries the plate with `transition: background-color 200ms ease-out, box-shadow 160ms ease-out` (`:277-298`).

Logic
- `accentForeground(hex)`: WCAG relative luminance > 0.45 → `#000000` else `#ffffff` (invalid hex → `#ffffff`) (`:304-324`). GNOME accent names → hex: blue `#3584e4`, teal `#2190a4`, green `#3a944a`, yellow `#c88800`, orange `#ed5b00`, red `#e62d42`, pink `#d56199`, purple `#9141ac`, slate `#6f8396` (`ST/src/desktop_appearance.rs:77-91`).
- `controlsOnRight` = close not in the leading part of `org.gnome.desktop.wm.preferences button-layout` (format `leading:trailing`, comma lists; no colon = all trailing) (`desktop_appearance.rs:46-50`, `:125-160`). HeaderBar draws only a Close button, on that edge.
- `shell_draws_window_frame()` (cached once): env `LOOKOUT_WINDOW_FRAME` (yes/no vocabulary via `env_flag`) wins (inverted); else self-decorating sessions `hyprland, niri, sway, river, dwl, i3, bspwm, awesome, xmonad, herbstluftwm, spectrwm, dwm, leftwm, qtile` from `XDG_CURRENT_DESKTOP`/`XDG_SESSION_DESKTOP`/`DESKTOP_SESSION` or env markers `HYPRLAND_INSTANCE_SIGNATURE, SWAYSOCK, NIRI_SOCKET, I3SOCK`; else an enabled GNOME extension whose UUID starts with `rounded-window-corners@`, `rounded-corners-effect@`, `rounded-windows@`, `p7-borders@` (unless `disable-user-extensions`) (`desktop_appearance.rs:171-260`, `:383-453`). True ⇒ no margin/border/radius/shadow/input shape.
- `useWindowFrameState()` (`DS/components/WindowResizeHandles.tsx:164-202`): on mount and on every `onResized` (debounced **100 ms** trailing) invoke `sync_window_frame({inset: FRAME_INSET})`; Rust applies a GTK input shape = rect inset by `inset` (passes clicks through the outer 32px shadow ring; 0 clears it) and returns whether GDK state is TILED/LEFT/RIGHT/TOP/BOTTOM_TILED/MAXIMIZED/FULLSCREEN (`ST/src/window_shape.rs:33-130`, 500 ms timeout). Fallback if null: `isMaximized()` or outer size within `max(2, ceil(scaleFactor))` px of the monitor work area on either axis. Toggles `html.lookout-snapped`.
- `useBackgroundBlur()` (`DS/linuxChrome.ts:469-527`): sync on mount, on resize (debounced **120 ms**), and on `lookout-csd`/`lookout-snapped` class changes (MutationObserver); `inset = framed ? 40 : 0`, radius = computed `--lookout-window-radius` (or 0 if not framed) → `sync_background_blur`; toggles `html.lookout-blur` only if Rust returns true. Rust is a no-op unless `LOOKOUT_WINDOW_BLUR` is truthy; it builds a rounded region as per-row rectangles (`ST/src/background_blur.rs:59-110`).
- `applyLinuxChrome({undecorated})` adds `os-linux`, adds/removes `lookout-csd`, calls `syncWindowFrame(FRAME_INSET)`, reads `desktop_appearance`, applies accent (`DS/linuxChrome.ts:539-578`).

### 5.9 Resize handles (`DS/components/WindowResizeHandles.tsx`)

Mounted only on Linux in the **editor** window (main is fixed size). Hidden entirely while `html.lookout-snapped` (MutationObserver) (`:204-219`). 8 `position:fixed; zIndex:10000` divs, `aria-hidden`; `GRAB = 8`, `INSET = FRAME_INSET` (32, or 0), `CORNER = 8 + 6 = 14` (`:93-101`):

| Direction | cursor | geometry |
|---|---|---|
| North | `n-resize` | top:INSET, left:INSET, right:INSET, height:8 |
| South | `s-resize` | bottom:INSET, left:INSET, right:INSET, height:8 |
| West | `w-resize` | top:INSET, bottom:INSET, left:INSET, width:8 |
| East | `e-resize` | top:INSET, bottom:INSET, right:INSET, width:8 |
| NorthWest | `nw-resize` | top:INSET, left:INSET, 14×14 |
| NorthEast | `ne-resize` | top:INSET, right:INSET, 14×14 |
| SouthWest | `sw-resize` | bottom:INSET, left:INSET, 14×14 |
| SouthEast | `se-resize` | bottom:INSET, right:INSET, 14×14 |

`onMouseDown`: left button only (`e.button === 0`), `preventDefault()`, `getCurrentWindow().startResizeDragging(direction)` (compositor-driven resize) (`:227-235`). Since INSET=32 and the frame is 40, handles cover the 8px band just outside the visible window (corners reach 6px into the content).

### 5.10 Miscellaneous global behaviours

- Context menu disabled everywhere (`DS/main.tsx:128`).
- `body { user-select:none; -webkit-user-select:none; cursor:default; overscroll-behavior:none }`, `html, body, #root { height:100%; overflow:hidden }`, scrollbars hidden `::-webkit-scrollbar { width:0; background:transparent }` (`index.html:61-70`).
- Deep links: `lookout://` scheme (`ST/tauri.conf.json:109-115`); all entry points funnel to `handle_deep_link_urls` → stash for `get_cold_start_urls` + emit `lookout-deep-link` (array of URL strings) + focus main (`ST/src/lib.rs:266-290`). Debug helper `window.__simulateDeepLink(url)` emits the same event (`DS/main.tsx:26-28`).
- Window close during recording → recording aborted, tray hidden (`ST/src/lib.rs:1440-1447`); app exit with an active capture loop pauses the session first (`:1455-1479`).
- Fetch shim: external `http(s)` (non-`.localhost`) requests go through `@tauri-apps/plugin-http`; preview-frame URLs aren't logged (`DS/main.tsx:35-64`).

## 6. IPC contract: Tauri commands, events, plugin APIs, persisted state

All paths are relative to the repo root. `desk/` = `clients/desktop/`, `rs/` = `clients/desktop/src-tauri/`.

The native app has to reproduce everything below. Some of it is in-process now (the frontend and the core run in one Rust process), and some still crosses a process boundary (D-Bus, Swift FFI, the OS). Most commands are thin wrappers over `lookout_core::Core` (`rs/crates/lookout-core`). The GPUI app can call `Core` directly, but it must keep the same semantics, errors and payload shapes listed here.

### 6.0 Invocation plumbing

- **Two `invoke` wrappers.**
  - `desk/src/logger.ts:240` `invoke<T>(cmd, args)` logs `[ipc] → cmd args` and `[ipc] ← cmd ok|FAILED: msg`. It redacts the arg keys `value`, `deviceToken`, `token`, `verifier` and `code` (`logger.ts:225-238`). Almost every call site uses this wrapper.
  - `desk/src/api/tauriClient.ts:73` `call<T>(command, args)` logs `[api] <cmd> <apiBaseUrl> token <first 8 chars>…`. On failure it converts the Rust `ApiError {status, message}` into `HttpError(status, message)` when `status != null`, otherwise into a plain `Error(message)` (`tauriClient.ts:36-61`). The shared hooks rely on `instanceof HttpError && status === 409` checks.
  - `desk/src/programLink.ts:33,365` uses the raw `@tauri-apps/api/core` invoke for `secret_set`, so the device token is never logged.
- **Arg naming.** JS passes camelCase args and Tauri maps them to snake_case Rust params (`apiBaseUrl` → `api_base_url`, `requestId` → `request_id`, `bytesBase64` → `bytes_base64`, `timeoutMs` → `timeout_ms`, `capturedAt` → `captured_at`, `uploadUrl` → `upload_url`, `contentType` → `content_type`).
- **Error shapes crossing IPC.**
  - `ApiError`: `{ status: number|null, message: string }` (`rs/crates/lookout-protocol/src/error.rs:19`, camelCase).
  - `RecorderError`: tagged on `kind`, camelCase (`rs/crates/lookout-session/src/state.rs:167`): `{kind:"noRecording"} | {kind:"invalidPhase", phase} | {kind:"sourceLost"} | {kind:"api", status, message} | {kind:"other", message}`. `desk/src/recorder.ts:123-138` turns these into the UI strings:
    - noRecording → "No recording is open."
    - invalidPhase → "That doesn't apply right now (${phase})."
    - sourceLost → "The capture source is gone — pick one again."
    - api / other → `message`
  - Everything else rejects with a plain `String`.
- **Registered handlers.** `generate_handler![]` is at `rs/src/lib.rs:1113-1167` and registers 55 commands. Every registered command is listed in 6.1. Only one is **not called from JS**:
  - `get_blacklisted_apps` (`lib.rs:351`). The frontend keeps the list in localStorage and pushes it with `set_blacklisted_apps`.
- **Capabilities.** `rs/capabilities/default.json` applies to windows `main`, `tray` and `editor-*`.
  - Window permissions: start-dragging, set-title, close, start-resize-dragging, toggle-maximize, is-maximized, outer-size, current-monitor, is-focused, set-focus, get-all-windows, destroy, create-webview-window.
  - Event: listen, emit, unlisten.
  - Plugins: `shell:allow-open`, process, dialog, deep-link, macos-permissions, updater, notification, liquid-glass.
  - `http:default` is allowed for `http://localhost:*` and `https://**`.

### 6.1 Commands (Rust handler ⇄ frontend)

Each entry gives:
- the command name
- the Rust handler location
- args as Rust types, with the JS arg names
- the return type, converted to its JSON shape
- the errors it can return
- the frontend call site(s) and why they call it

#### 6.1.1 Recorder (the recording state machine lives in the core)

Every recorder command returns the full `RecorderState` (see 6.3). The same state is also pushed on the `recorder-state` event.

| Command | Rust | Args | Returns | Err | Called from / why |
|---|---|---|---|---|---|
| `recorder_open` | `lib.rs:569` | `token: String`, `apiBaseUrl: String`, `sources: Vec<PickedSource>` | `RecorderState` | `RecorderError` (`Other{message}` when `RecordingSource::from_picked` fails: "no capture source selected" / "a camera can't be combined with other sources") | `desk/src/recorder.ts:233-234` `recorder.open`. Used by DesktopRecorder to start recording on the picked sources |
| `recorder_close` | `lib.rs:586` | — | `()` | String | `recorder.ts:235`. DesktopRecorder unmount `DesktopRecorder.tsx:136` |
| `recorder_pause` | `lib.rs:592` | — | `RecorderState` | RecorderError | `recorder.ts:236` |
| `recorder_resume` | `lib.rs:597` | — | `RecorderState` | RecorderError (`sourceLost` if there is no source) | `recorder.ts:237` |
| `recorder_begin_stop` | `lib.rs:602` | — | `RecorderState` (phase → `stopPausing` → `stopPrompting`) | RecorderError | `recorder.ts:238`. Opens the naming prompt |
| `recorder_cancel_stop` | `lib.rs:607` | — | `RecorderState` | RecorderError | `recorder.ts:239`. "Keep recording" |
| `recorder_finish_stop` | `lib.rs:612` | `name: Option<String>`, `edit: bool` | `StopOutcome {status: string, edit: boolean}` (`state.rs:138`) | RecorderError | `recorder.ts:240-241`. Naming modal submit, or "edit & save" (`edit=true` → open the editor window) |
| `recorder_state` | `lib.rs:623` | — | `RecorderState \| null` | — | `recorder.ts:172` (initial read in `useRecorderState`, after the listener is attached; `setState(prev => prev ?? current)`), `recorder.ts:242`, `TrayApp.tsx:100` (on mount, on `tray-opened`, on window `focus`) |
| `session_finish` | `lib.rs:630` | `token`, `apiBaseUrl`, `name: Option<String>`, `edit: bool` | `FinishOutcome {status: string, trackedSeconds: number\|null, totalActiveSeconds: number\|null}` (`state.rs:149`). TS types it as `{status}` | `ApiError` | `recorder.ts:247-254` `finishSession`. The SourcePicker's Stop button, used when no recording is open |
| `tray_action` | `rs/src/tray.rs:358` | `action: String` ∈ `"ready"\|"pause"\|"resume"\|"stop"` | `()` | `"unknown tray action \"x\""` | `TrayApp.tsx:134,138,142`. Also called in-process from `tray.rs:172` (Linux tray menu) and `gnome_indicator.rs:169-177` (D-Bus) |

`tray_action` behaviour (`tray.rs:359-411`):
1. Any action other than `ready` first hides the `tray` window.
2. `ready` → emits `recorder-state` with the current state.
3. `pause` → spawns `core.recorder_pause()`.
4. `resume` → if `state.source_lost` is set, it skips the resume and calls `focus_main_window` (unminimize + show + set_focus on `main`). Otherwise it spawns `recorder_resume`.
5. `stop` → `focus_main_window`, then `recorder_begin_stop` (the naming prompt is in the main window).

#### 6.1.2 Camera (the core asks the webview; the webview answers)

| Command | Rust | Args | Returns | Called from |
|---|---|---|---|---|
| `camera_request_done` | `lib.rs:648` | `requestId: u64`, `error: Option<String>` | `()` (logs if the id is stray) | `desk/src/hooks/useCameraBackend.ts:73`, after handling `camera-start-request` (getUserMedia → video ready, or the error message) |
| `camera_frame_delivered` | `lib.rs:661` | `requestId: u64`, `base64: Option<String>` (JPEG), `width: Option<u32>`, `height: Option<u32>`, `error: Option<String>` | `()`. `(None, None)` → error "no frame" | `useCameraBackend.ts:76-88`. On failure it sends `error: "no camera frame — the stream isn't ready or has ended"` |

In GPUI this becomes a native camera capture, so the request/response round-trip disappears. The core's `Frontend::camera_start`, `camera_grab_frame` and `camera_stop` (`lib.rs:236-260`) must be implemented directly.

#### 6.1.3 Capture sources and screencast

| Command | Rust | Args | Returns | Called from / why |
|---|---|---|---|---|
| `list_capture_sources` | `lib.rs:383` | — | `CaptureSourceList {monitors: MonitorInfo[], windows: WindowInfo[]}` (`rs/crates/lookout-core/src/sources.rs:62`) | `SourcePicker.tsx:276` |
| `is_wayland` | `lib.rs:445` | — | `bool` (`WAYLAND_DISPLAY` is set) | `SourcePicker.tsx:261` (on Wayland: only the cast/cameras tabs), `App.tsx:505` (the filtered-apps feature is hidden on Wayland) |
| `request_screencast` | `lib.rs:537` | — | `StreamInfo[]` = `[{node_id: u32}]`. **Not camelCase** (`screencast.rs:9`) | `SourcePicker.tsx:350`. XDG portal pick; the result replaces the selection with `{type:"pipewire", id: node_id}` |
| `add_screencast` | `lib.rs:542` | — | `StreamInfo[]` | `SourcePicker.tsx:361`. Adds another portal session and appends to the selection |
| `release_screencast` | `lib.rs:553` | — | `()` | `RecordPage.tsx:94` (on stop, and on RecordPage unmount via `useEffect(() => releaseScreencast)` at `RecordPage.tsx:100`). Closes portal sessions. Pause deliberately does NOT release |
| `capture_environment` | `lib.rs:391` | — | `CaptureEnvironment` (see 6.3) | `CaptureErrorDialog.tsx:71`. Probed only once the dialog opens |
| `set_blacklisted_apps` | `lib.rs:346` | `apps: Vec<String>` | `()` | `desk/src/hooks/useBlacklistedApps.ts:30`. On mount and on every list change. The list is also saved to localStorage |
| `get_blacklisted_apps` | `lib.rs:352` | — | `string[]` | **unused by JS** |
| `list_installed_apps` | `lib.rs:368` (spawn_blocking) | — | `AppEntry[]` = `{name: string, path: string\|null, running: boolean}`. **No serde rename**, but the fields are already single words (`apps.rs:14`) | `SettingsPage.tsx:768` (Filtered Apps page) |
| `get_app_icon` | `lib.rs:378` | `path: String` (`AppEntry.path`) | `string \| null` (base64 PNG; stored per app name in `appIcons`) | `SettingsPage.tsx:805`, lazily per row through an IntersectionObserver with `rootMargin: "200px"`, deduplicated by app name |

Custom URI scheme **`lookout-preview`** (`lib.rs:976-~1060`; this is a protocol handler, not a command):
- The URL is `lookout-preview://localhost/<monitor|window|pipewire>/<id>?maxWidth=&maxHeight=&jpegQuality=`. On Windows it is `http://lookout-preview.localhost/...`.
- The handler returns a JPEG. It applies blacklist redaction and uses the PipeWire fds on Linux.
- Defaults: 854×480, quality 85.
- Bad path → 400.
- Caller: `desk/src/hooks/useScreenPreview.ts:105-113`. It polls with `maxWidth=854&maxHeight=480&jpegQuality=65&t=<now>`, using fetch → blob → objectURL, preloads an `Image`, then swaps. The rate is `useScreenPreview(source, targetFps = 20, live = true)` (`useScreenPreview.ts:27-31`). Callers: `DesktopRecorder.tsx:62` (1 fps), `SourcePicker.tsx:97` (1 fps, the selected source tile), `SourcePicker.tsx:165` (5 fps, `live=false`, the hovered window).

#### 6.1.4 Window chrome and appearance

| Command | Rust | Args | Returns | Called from / why |
|---|---|---|---|---|
| `enable_vibrancy` | `lib.rs:396` | (window injected) | `()` | `App.tsx:978` (main, not on Linux), `EditorWindow.tsx:269`. On success it sets html/body/#root `background: transparent` |
| `disable_vibrancy` | `lib.rs:426` | — | `()` | `App.tsx:998`, `EditorWindow.tsx:284` (cleanup) |
| `sync_window_frame` | `rs/src/window_shape.rs:55` | `inset: i32` | `bool` (true = frame collapsed, i.e. WM-sized/tiled/maximized). Always `false` off Linux | `desk/src/linuxChrome.ts:351-363` `syncWindowFrame`, called with `FRAME_INSET` (`linuxChrome.ts:117`: `SHELL_DRAWS_FRAME ? 0 : WINDOW_MARGIN(40) - RESIZE_BAND`). Runs at init (`:557`) and on every resize (`useWindowFrameState`, `WindowResizeHandles.tsx:~110-140`) |
| `sync_background_blur` | `rs/src/background_blur.rs:112` | `inset: i32`, `radius: i32` | `bool` (the compositor honours `ext-background-effect-v1`). `false` off Linux | `linuxChrome.ts:420-430`. Called from `useBackgroundBlur` with `inset = framed ? WINDOW_MARGIN(40) : 0` and `radius = framed ? --lookout-window-radius : 0` (`linuxChrome.ts:484-485`) |
| `desktop_appearance` | `rs/src/desktop_appearance.rs:457` | — | `DesktopAppearance {accent: string\|null /* "#3584e4" */, controlsOnRight: boolean}` (camelCase, `:53-62`). The non-Linux fallback is `{accent:null, controlsOnRight:true}` | `linuxChrome.ts:564`. On success `setAccentColor(accent, accentForeground(accent))` from @lookout/react. Re-read when the window regains focus (`linuxChrome.ts:606`) |

What `enable_vibrancy` does on each platform (`lib.rs:396-424`):
- **macOS:** `apply_vibrancy(NSVisualEffectMaterial::Sidebar, NSVisualEffectState::Active, radius)`. The radius is **16.0 on macOS major ≥ 26 (Tahoe), else 10.0**.
- **Windows:** `apply_mica(window, None)`.
- **Linux:** no-op.

`disable_vibrancy` calls `clear_vibrancy` on macOS and `clear_mica` on Windows (`lib.rs:426-443`).

Injected global (not a command): plugin `lookout-window-frame` `js_init_script` sets `window.__LOOKOUT_SHELL_DRAWS_FRAME__ = <bool>` at document start (`lib.rs:1099-1107`). The value comes from `desktop_appearance::shell_draws_window_frame()`. It is read in `index.html` (decides the `lookout-csd` class) and in `linuxChrome.ts:66`.

#### 6.1.5 Add menu (macOS native popup)

| Command | Rust | Args | Returns | Called from |
|---|---|---|---|---|
| `show_add_menu` | `rs/src/native_menu.rs:118` | `entries: AddMenuEntry[]`, `anchor: AddMenuAnchor` | `string \| null` (chosen id, or null when dismissed). Off macOS it errors with "native add menu is only implemented on macOS" | `App.tsx:681`. On error it falls back to `navigate({page:"add"})` |
| `prefetch_add_menu_icons` | `native_menu.rs:104` | `urls: string[]` | `()` (no-op off macOS) | `App.tsx:526` after the program registry fetch (off macOS the code does `new Image().src = url` instead) |

Argument shapes:
- `AddMenuEntry` (`native_menu.rs:10-20`) = `{id?: string, label?: string, symbol?: string /* SF Symbol */, iconUrl?: string, separator?: boolean}`.
- `AddMenuAnchor` = `{x, y, width, height}` in CSS px, viewport-relative. It comes from the "+" button's `getBoundingClientRect()` (`App.tsx:1322-1323`).

The entries App sends (`App.tsx:667-677`):
1. One entry per program: `{id:"program:<name>", label: displayName||name, symbol:"arrow.up.forward.app", iconUrl}`.
2. `{separator:true}`.
3. `{id:"create-new", label:"Start from link", symbol:"plus"}`.

How a choice is handled (`App.tsx:638-652`):
- `null` → ignored.
- `"create-new"` → the add page.
- `program:<name>` → `openProgram`; if that returns `"failed"` → the add page.

Only one menu can be open at a time. A new `show` call cancels the pending one, which resolves `None` (`native_menu.rs:58-60`).

The FFI is `lookout_add_menu_show(items_json, ns_window, x, y, w, h, cb)` and `lookout_add_menu_prefetch_icons(urls_json)`, implemented in Swift `rs/swift/lookout-tray/Sources/AddMenu.swift`.

#### 6.1.6 Misc OS

| Command | Rust | Args | Returns | Called from / why |
|---|---|---|---|---|
| `open_external_url` | `lib.rs:466` | `url: String` | `()`. Err "only http(s) URLs are allowed" for any other scheme. Uses `tauri_plugin_opener` | `App.tsx:248` (redirect hook, fired once per token via `redirectFiredRef`), `App.tsx:575,609` (program `newSessionUrl`), `AddSessionPage.tsx:108`, `OpenInProgram.tsx:29`, `ProgramPanel.tsx:248`, `AnnouncementBanner.tsx:22`, `UpdateDrawer.tsx:38` (`update.releasesUrl`), `useAppUpdate.ts:98` (`RELEASES_URL`), `programLink.ts:322` (the pairing consent page) |
| `get_cold_start_urls` | `lib.rs:333` | — | `string[]`. Takes and clears the stash | `App.tsx:857`. Polled up to 10× at 500 ms intervals on mount (`App.tsx:846-872`) |
| `linux_install_kind` | `lib.rs:490` | — | `LinuxInstall {manager: "apt"\|"rpm"\|"pacman"\|"unknown", enrolled: bool}` (`lib.rs:477-487`) | `desk/src/hooks/useLinuxUpdate.ts:57`. Builds the update command text via `updateCommand.ts:21 instructionsFor` |
| `secret_set` | `rs/src/secret_store.rs:96` | `key: String`, `value: String` | `()` | `programLink.ts:365` (raw invoke). Key `program-device-token:<program>` (`programLink.ts:270`) |
| `secret_get` | `secret_store.rs:104` | `key` | `string \| null` | `programLink.ts:393,454` |
| `secret_delete` | `secret_store.rs:110` | `key` | `()` | `programLink.ts:406` (unlink) |

How `linux_install_kind` decides:
- `manager` is the first package manager that owns the exe: `dpkg -S`, then `rpm -qf`, then `pacman -Qo`.
- `enrolled` checks for the matching repo file:
  - apt: `/etc/apt/sources.list.d/lookout.sources`
  - rpm: `/etc/yum.repos.d/lookout.repo`
  - pacman: a `[lookout]` section in `/etc/pacman.conf`
- Off Linux it always returns `{unknown, false}`.

#### 6.1.7 Server API (proxied through `lookout_core::api`; JSON returned as-is)

Every command below lives in `lib.rs` and is wrapped by `desk/src/api/tauriClient.ts`.
- **Session-scoped** commands take `token: String` and `apiBaseUrl: String`. The client builds both with `session()` (`tauriClient.ts:115`).
- **Return / error:** each returns `serde_json::Value` (TS types from `packages/shared/src/types.ts`) or `ApiError`.

| Command | Rust line | Extra args | TS return type (shared) | HTTP (core) | TS wrapper line |
|---|---|---|---|---|---|
| `api_session_get` | 706 | — | `SessionResponse` (`types.ts:91`) | GET session | `tauriClient.ts:120-122` |
| `api_session_upload_url` | 808 | `capturedAt: string\|null`, `format: string\|null` | `UploadUrlResponse` (`types.ts:236`) | | `:124-130` |
| `api_session_confirm_screenshot` | 826 | `body: ConfirmScreenshotRequest` (`types.ts:273`) | `ConfirmScreenshotResponse` (`:287`) | | `:132-137` |
| `api_upload_to_r2` | 842 | `uploadUrl`, `bytesBase64` (blob read by FileReader as a data URL, prefix stripped), `contentType` (default `"image/jpeg"`). No token | `void` | PUT to R2 | `:139-145` |
| `api_session_pause` | 724 | — | `PauseResponse` (`:295`) | | `:147-149` |
| `api_session_resume` | 733 | — | `ResumeResponse` (`:300`) | | `:151-153` |
| `api_session_stop` | 742 | `edit: bool` (`opts?.edit === true`) | `StopResponse` (`:326`) | | `:155-160` |
| `api_session_rename` | 752 | `name: string` | `RenameSessionResponse` (`:399`) | | `:162-167` |
| `api_session_status` | 715 | — | `StatusResponse` (`:335`) | | `:169-171` |
| `api_session_video` | 762 | — | `VideoResponse` (`:370`) | | `:173-175` |
| `api_session_units` | 771 | — | `UnitsResponse` (`:163`) | | `:177-179` |
| `api_session_set_cuts` | 780 | `cuts: CutInterval[]` (`packages/shared/src/cuts.ts:17`) | `SetCutsResponse` (`:202`) | | `:181-186` |
| `api_session_apply_cuts` | 790 | — | `ApplyCutsResponse` (`:216`) | | `:188-190` |
| `api_session_heartbeat_editing` | 799 | — | `EditHeartbeatResponse` (`:316`) | | `:192-194` |
| `api_programs` | 856 | `apiBaseUrl`, `timeoutMs: number\|null` (core default 30 s; Settings probes with `6_000`, `SettingsPage.tsx:~588`) | `{programs: Program[]}` with `Program = {name, displayName?, newSessionUrl, iconUrl?, pairUrl?, startUrl?}` (`tauriClient.ts:200-213`) | GET `/api/programs` | `:217-222` |
| `api_announcement` | 871 | `apiBaseUrl`, `client: string\|null`, `version: string\|null` | `{announcement: {level:"info"\|"success"\|"warning"\|"danger", message, url\|null} \| null}` | GET `/api/announcement` | `:234-243` |
| `api_tip` | 884 | same as above | `{tip: Tip \| null}` (`desk/src/tip.ts`) | GET `/api/tip` | `:247-256` |
| `api_sessions_batch` | 897 | `apiBaseUrl`, `tokens: string[]` (≤100) | `BatchSessionsResponse` (`types.ts:407`) | POST `/api/sessions/batch` | `:259-264` |

The server base defaults to `DEFAULT_API_BASE = "https://lookout.hackclub.com"` (`desk/src/serverConfig.ts:17`), and is overridable through localStorage `lookout-api-base`.

### 6.2 Events

#### 6.2.1 Rust → webview

Core events are emitted through `TauriFrontend::emit` (`lib.rs:197-221`), using names from `CoreEvent::name()` (`rs/crates/lookout-recorder/src/events.rs:105-117`). All of them are app-wide (`app.emit`).

| Event | Payload (JSON) | Emitted at | Listened at | Notes |
|---|---|---|---|---|
| `recorder-state` | `RecorderState \| null` (null = recording closed) | `lib.rs:214-217` (from the core on every change; also calls `sync_tray`), `tray.rs:370` (`tray_action("ready")`) | `desk/src/recorder.ts:169` (`useRecorderState`), `TrayApp.tsx:108` | The single source of truth for recorder UI |
| `recorder-notice` | `{title: string, body: string}` (`state.rs:159`) | `lib.rs:218` (core: `lookout-recorder/src/recorder.rs:306`) | `desk/src/hooks/useRecorderNotices.ts:68` → `sendNotification({title, body})` | Native OS notification. Permission is requested once capture starts |
| `capture-preview-frame` | `{previewBase64: string /*JPEG*/, previewWidth: u32, previewHeight: u32}` (`events.rs:52-58`) | `lib.rs:212` | `DesktopRecorder.tsx:159` → base64 → Blob → objectURL (revokes the previous one) | Live preview frames come only while the `main` window is focused (`wants_preview_frames`, `lib.rs:223-229`), plus every uploaded capture |
| `capture-progress` | `string` (e.g. "capturing screen...", "uploading camera frame WxH") | `lib.rs:203` | `desk/src/logger.ts:95` (logs `[capture] …` only) | Debug only |
| `capture-tracked-seconds` | `i64` | `lib.rs:204` | **none** | |
| `capture-session-terminated` | `{status: string}` | `lib.rs:205` | **none** (folded into RecorderState) | |
| `capture-tick-error` | `{message: string}` | `lib.rs:210` | **none** | |
| `capture-source-lost` | `{message: string}` | `lib.rs:211` | **none** (RecorderState.sourceLost is used instead) | |
| `screencast-revoked` | `{nodeIds: u32[]}` | `lib.rs:213` (from `lookout-core/src/screencast.rs:258`) | **none** | |
| `capture-tick-result` | — | **never emitted**: suppressed at `lib.rs:206-209` | — | |
| `camera-start-request` | `{requestId: u64, deviceId: string}` (`lib.rs:78-83`) | `lib.rs:112` via `camera_start` (`:236-239`) | `useCameraBackend.ts:96` | Answered with `camera_request_done` |
| `camera-frame-request` | `{requestId, deviceId}` | `lib.rs:112` via `camera_grab_frame` (`:241-250`) | `useCameraBackend.ts:97` | Answered with `camera_frame_delivered` |
| `camera-stop` | `null` (unit) | `lib.rs:258` | `useCameraBackend.ts:98` → `stopStream()` | |
| `lookout-deep-link` | `string[]` (`url::Url` values serialized as strings) | `lib.rs:283` `handle_deep_link_urls` (cold start, single-instance, Apple Events). Also stashes the URLs for `get_cold_start_urls` and focuses `main` | `App.tsx:835` → `handleDeepLinkRef.current(urls)` | Also emitted from JS (see 6.2.2) |
| `lookout-navigate` | `string` (`"/add"` only) | `lib.rs:1271` (macOS menu File → "Start Timelapse", `CmdOrControl+N`; also focuses `main`) | `App.tsx:943` → `navigate({page:"add"})` | |
| `tray-opened` | `null` | `tray.rs:189,225` (`window.emit` on the `tray` window when it is shown) | `TrayApp.tsx:113` → re-reads `recorder_state` | |

#### 6.2.2 Webview → webview (JS `emit`, broadcast to all windows)

| Event | Payload | Emitted at | Listened at |
|---|---|---|---|
| `lookout-deep-link` | `string[]` | `AddSessionPage.tsx:127` (a pasted link is routed through the deep-link handler as `[trimmed]`), `main.tsx:27` (`window.__simulateDeepLink(url)` debug helper) | `App.tsx:835` |
| `lookout-editor-opened` (`EDITOR_OPENED_EVENT`, `EditorWindow.tsx:22`) | `{token: string}` | `EditorWindow.tsx:54` (focusing an existing editor), `:90` (after creating one) | `EditorWindow.tsx:202` (`useEditorWindowOpen` in main sets `token` → shows `EditorOpenPlaceholder`) |
| `lookout-edited` (`EDITED_EVENT`, `EditorWindow.tsx:18`) | `{token: string, status: string\|null, redirectUrl: string\|null, panelUrl: string\|null, panelResolved: boolean}` | `EditorWindow.tsx:332` (publish-on-close after `setCuts` + `applyCuts`), `:433` (`onApplied` from TimelapseEditor) | `App.tsx:368` (bumps `editNonce`, refreshes the gallery, then runs the panel/redirect handoff for `token`), `EditorWindow.tsx:208` (clears the "editor open" placeholder token) |

#### 6.2.3 Window/webview lifecycle listeners (Tauri window API)

| API | Where | Purpose |
|---|---|---|
| `getCurrentWindow().onThemeChanged` | `App.tsx:918` (not on Linux) | Sets/removes `data-theme` on `<html>`. On Linux, `matchMedia('(prefers-color-scheme: dark)')` + `setTheme(theme)` is used instead (`App.tsx:894-913`) |
| `getCurrentWindow().theme()` | `App.tsx:915` | Initial theme |
| `onFocusChanged` | `linuxChrome.ts:390` (focused/backdrop state → CSS), `linuxChrome.ts:607` (re-read `desktop_appearance` on focus) | Linux chrome |
| `onResized` | `linuxChrome.ts:514` (blur region re-sync), `WindowResizeHandles.tsx:133` (frame collapse via `sync_window_frame` + `isFlushWithWorkArea`) | Both are throttled with a setTimeout `schedule` |
| `onCloseRequested` | `EditorWindow.tsx:350` | `preventDefault`, then a native `confirm` ("Finish timelapse?"), then publish, then close |
| `win.once("tauri://error")` | `EditorWindow.tsx:87` | Logs if editor window creation fails |
| DOM `window` `focus` | `TrayApp.tsx:~119` | Re-sync `recorder_state` |
| DOM `contextmenu` | `main.tsx:128` | `preventDefault` everywhere (no webview context menu) |

### 6.3 Payload types (Rust source of truth; camelCase on the wire)

`RecorderState` (`rs/crates/lookout-session/src/state.rs:95-135`, `rename_all=camelCase`):
```
{ token: string,
  source: {type:"screen", sources: Array<{type:"monitor"|"window"|"pipewire", id:u32}>} | {type:"camera", deviceId:string},
  phase: "loading"|"loadError"|"recording"|"pausing"|"paused"|"resuming"|"stopPausing"|"stopPrompting"|"stopFinishing"|"finished",
  sessionStatus: string|null, sessionName: string, sessionError: string|null, actionError: string|null,
  controlMode: "recording"|"paused",          // pinned pre-action reading during transitions
  capturing: boolean,                          // updater must not install while true
  clock: { baseSeconds:i64, anchorAtUnixMs:i64, running:bool, holdAtUnixMs:i64|null, maxInterpolationSeconds:i64 },
  displaySeconds: i64, trackedSeconds: i64, tooShortToCompile: bool, captureCount: u32,
  captureError: string|null,
  sourceLost: {reason:"revoked"|"failed", message:string, detail:string|null} | null,
  stale: bool,                                 // no capture for STALE_CAPTURE_SECS = 180
  lastCaptureAtUnixMs: i64|null,
  pendingUploads: u32 }                        // NOT in the TS interface (desk/src/recorder.ts:78-98)
```
- The TS mirror is `desk/src/recorder.ts:19-117`.
- Constants (`state.rs:10-20`): `MAX_CAPTURE_WIDTH=1920`, `MAX_CAPTURE_HEIGHT=1080`, `CAPTURE_JPEG_QUALITY=85`, `MIN_STOPPABLE_TRACKED_SECONDS=60`, `STALE_CAPTURE_SECS=180`.
- The clock is derived in `rs/crates/lookout-session/src/clock.rs:91-101` and mirrored in TS at `recorder.ts:146-149`:
  - `display = running ? base + clamp(floor((nowOrHold - anchor)/1000), 0, MAX_INTERPOLATION_SECS) : base`, where `nowOrHold = min(now, holdAtUnixMs)` when a hold is set.
  - `MAX_INTERPOLATION_SECS = CAPTURE_INTERVAL_SECS + TIMER_INTERPOLATION_SLACK_SECS` (`clock.rs:63-64`).
- Timer surfaces tick with requestAnimationFrame while `running && hold == null`, and re-render only when the derived second changes (`recorder.ts:192-228`).

`PickedSource` (input to `recorder_open`; `rs/crates/lookout-session/src/source.rs:23-30`, tag `type`, lowercase): `{type:"monitor", id:u32} | {type:"window", id:u32} | {type:"pipewire", id:u32} | {type:"camera", id:string /*deviceId*/}`. A camera must be the only source.

`MonitorInfo` (`sources.rs:38-47`, camelCase): `{id, name, width, height, isPrimary, isBuiltin, scaleFactor: f32}`.

`WindowInfo` (`sources.rs:50-58`, camelCase): `{id, appName, title, width, height, isMinimized, isFocused}`.

`CaptureEnvironment` (`rs/crates/lookout-core/src/capture_diagnostics.rs:19-60`, camelCase):
```
{ os: "linux"|"macos"|"windows",
  sessionType, desktop, distroId, distroIdLike, distroName: string|null,
  portalInstalled, screencastAvailable: bool|null,
  portalBackends: string[],
  pipewireRunning, pipewireGstElement: bool|null,
  clipEncoder: string|null }
```
The TS mirror is `desk/src/captureDiagnosis.ts:21`.

### 6.4 Tauri plugin and JS-side native APIs used (call sites)

| API | Call sites | Detail |
|---|---|---|
| `@tauri-apps/api/window` `getCurrentWindow().close()` | `HeaderBar.tsx:158` (Linux close button, unless `onClose` is given), `EditorWindow.tsx:112` | |
| `.destroy()` | `EditorWindow.tsx:116` | Fallback when close fails |
| `.toggleMaximize()` | `HeaderBar.tsx:237` | Header bar double-click when `maximizable` (editor) |
| `.startResizeDragging(direction)` | `WindowResizeHandles.tsx:175` | Linux undecorated windows |
| `.isMaximized()`, `.outerSize()`, `currentMonitor()` (`workArea.size`, `scaleFactor`) | `WindowResizeHandles.tsx:75-90` | `isFlushWithWorkArea`: slack = `max(2, ceil(scaleFactor))` physical px |
| `.isFocused()` | `linuxChrome.ts:387` | |
| `.setFocus()` | `App.tsx:761,820` (deep link), `EditorWindow.tsx:41,53,106` | |
| `.setTitle()` | `App.tsx:961` (`"Lookout"` on Linux, else `` `Lookout v${version}` ``), `EditorWindow.tsx:301` (`` `Edit — ${name}` ``) | |
| `.setTheme(theme)` | `App.tsx:905` | Linux only |
| `data-tauri-drag-region` (attribute → start-dragging) | `HeaderBar.tsx:183,234,262`, `App.tsx:1385`, `EditorWindow.tsx:388` | Window drag regions |
| `@tauri-apps/api/webviewWindow` `new WebviewWindow(label, opts)`, `WebviewWindow.getByLabel` | `EditorWindow.tsx:30-91` | Label `editorWindowLabel(token)` (an `editor-*` label); url `` `${pathname}#/editor?token=${token}` ``; title "Edit timelapse"; `940+pad × 660+pad`, min `620+pad × 480+pad`, where `pad = (Linux && !SHELL_DRAWS_FRAME) ? WINDOW_MARGIN*2 (=80) : 0`; resizable, center, transparent; macOS `titleBarStyle:"overlay"`; Linux `decorations:false` |
| `@tauri-apps/api/menu` `Menu.new`, `MenuItem.new`, `PredefinedMenuItem.new({item:"Separator"})`, `menu.popup()` | `App.tsx:722-742` | Gallery card right-click: "Open"; "Open in Editor" (only if status `complete`); separator; "Archive" |
| `@tauri-apps/api/app` `getVersion()` | `App.tsx:21,959`, `useAppUpdate.ts`, `useAnnouncement.ts`, `useTip.ts`, `useLinuxUpdate.ts`, `logger.ts` | |
| `@tauri-apps/api/event` `listen` / `emit` | see 6.2 | |
| `@tauri-apps/plugin-dialog` `confirm(msg, {title, kind})` | See the list after this table | All are native dialogs |
| `@tauri-apps/plugin-dialog` `message` | `App.tsx:780` | "Linking failed — you can retry from the + menu." (`{title:"Lookout", kind:"error"}`) |
| `@tauri-apps/plugin-dialog` `ask` | `useAppUpdate.ts:86-97` | Title "Update couldn't be installed", kind warning, okLabel "Open Releases Page", cancelLabel "Not Now". Body text: "Lookout couldn't install version ${version}.\n\nYour sessions and settings are safe — the app will keep working on this version. You can install the update yourself from the releases page." plus an optional "\n\nDetails: …" |
| `@tauri-apps/plugin-updater` `check()`, `update.downloadAndInstall/download` (progress events `Started{contentLength}`, `Progress{chunkLength}`, `Finished`), `update.install()` | `useAppUpdate.ts:191-215,124` | Check on launch, then every `CHECK_INTERVAL_MS = 30 min`. Skips the first check if `LAST_UPDATE_KEY` is < 60 s old. Auto-installs a previously skipped update (Windows/macOS only, at most 2 attempts). Holds while capture is active, re-testing every 30 s. On Windows, install runs NSIS `/S` and exits the process |
| `@tauri-apps/plugin-process` `relaunch()` | `useAppUpdate.ts:125` | After install, on macOS/Linux |
| `@tauri-apps/plugin-notification` `isPermissionGranted`, `requestPermission`, `sendNotification({title, body})` | `useRecorderNotices.ts:15-51`, `App.tsx` (`ensureNotificationPermission` import) | |
| `@tauri-apps/plugin-http` `fetch` | `main.tsx:30-63` | Replaces `window.fetch` for external `http(s)` URLs not on `*.localhost`. Remaining uses: R2 video/thumbnail media, program icons, the GitHub `latest.json` (`useLinuxUpdate.ts:8,49`; every 6 h), program pair/start endpoints (`programLink.ts:226-235`, 15 s timeouts) |
| `tauri-plugin-macos-permissions-api` `checkScreenRecordingPermission`, `requestScreenRecordingPermission`, `checkCameraPermission`, `requestCameraPermission` | `PermissionScreen.tsx:3-6,43-58`, `App.tsx:35-36,196-203` | Background re-verification of the cached grant. When the plugin is unavailable (non-macOS), permission is assumed granted |
| `tauri-plugin-liquid-glass-api` `isGlassSupported`, `setLiquidGlassEffect({cornerRadius: isTahoe?16:10, tintColor:"#00000000", variant: GlassMaterialVariant.Sidebar})` | `TrayApp.tsx:4,70-84` | Tray popup. Tahoe = macOS major ≥ 26, parsed from the UA. html/body/#root are forced to `background: transparent !important` |
| `@tauri-apps/plugin-deep-link` | **not imported by JS**. Deep links arrive through the custom `lookout-deep-link` event + `get_cold_start_urls` | |
| `@tauri-apps/plugin-shell` | **not imported by JS** (only the `shell:allow-open` capability). External URLs go through the `open_external_url` command (opener plugin, Rust) | |

Native `confirm` dialogs:
- `App.tsx:708` and `:1219`: "Are you sure you want to archive this session?" (`{title:"Archive Session", kind:"warning"}`)
- `SettingsPage.tsx:468`: "Unlink ${label} from this device?\n\nStarting a session from the + menu will go through your browser again until you re-link." (`{title:"Unlink program", kind:"warning"}`)
- `SettingsPage.tsx:576`: "Point this app at ${target}?\n\nAll new recordings will upload there instead of the official Lookout server. Only continue if someone from Hack Club asked you to." (`{title:"Switch Lookout server", kind:"warning"}`)
- `EditorWindow.tsx:354`, title "Finish timelapse?", kind warning. The message depends on whether cuts were made:
  - with cuts: "Closing publishes this timelapse with N cut(s) applied. This can't be undone."
  - without: "Closing publishes this timelapse as recorded. This can't be undone."

Window globals installed by JS (debug):
- `window.__simulateDeepLink(url)` (`main.tsx:26`)
- `window.__tip.show()` / `.hide()` (`tip.ts:184-206`)
- `window.__dbgRender` (`logger.ts:258`)
- The backtick key toggles the debug log panel (`index.html` + `logger.ts`)

Non-webview native surfaces that drive the same actions:
- **macOS app menu** (`lib.rs:1192-1290`):
  - Lookout menu: About (authors "Hack Club", website `https://fallout.hackclub.com`), Services, Hide, Hide Others, Show All, Quit.
  - File menu: "Start Timelapse" `CmdOrControl+N` → `lookout-navigate "/add"`.
  - Edit and Window menus.
  - Help menu: "Fallout Docs" (`https://fallout.hackclub.com/docs`), "How to Timelapse?" (`…/docs/project-resources/how-to-timelapse`), "GitHub Repo".
- **Linux tray menu** (`tray.rs:155-173`): the toggle item becomes pause or resume depending on `TRAY_PAUSED`; Stop → stop; Open → focus main.
- **GNOME pill** (D-Bus `com.hackclub.Lookout.Indicator` at `/com/hackclub/Lookout/Indicator`, `gnome_indicator.rs:62,142-195`):
  - Methods: `GetState() -> (active: bool, time: String, paused: bool)`, `Attach`, `Detach`, `Pause`, `Resume`, `Stop`, `Open`.
  - Signal: `StateChanged`.
- **macOS Swift status item:** FFI `lookout_tray_set_callback(cb(x, y, w, h))` and `lookout_tray_show(text, icon, len)` (`rs/src/native_tray.rs:15-17`). A click toggles the `tray` webview window (`tray.rs:183-226`: 300×50, undecorated, transparent, always-on-top, skip-taskbar, hidden on blur, visible on all workspaces on macOS).

### 6.5 localStorage keys (frontend-persisted state the native app must migrate/replicate)

| Key | Value shape | Purpose | Defined / used |
|---|---|---|---|
| `lookout-tokens` | `TokenEntry[]` = `{token: string, addedAt: string /*ISO*/, label?: string, archived: boolean}[]` | The user's sessions (gallery source of truth); archive flag | `clients/react/src/hooks/useTokenStore.ts:3,25,34`. Corrupt JSON is cleared at boot (`desk/src/main.tsx:66-72`) |
| `lookout:gallery-cache:v2` | `Record<token, {summary: SessionSummary, fetchedAt: number}>`, newest 500 | Instant gallery paint | `clients/react/src/hooks/useGallery.ts:28-50` |
| `lookout-api-base` | origin string (absent = `https://lookout.hackclub.com`) | Server override (Settings) | `desk/src/serverConfig.ts:19,48,72-74` |
| `lookout-blacklisted-apps` | `string[]` (sorted app names) | Filtered apps; pushed to Rust with `set_blacklisted_apps` | `desk/src/hooks/useBlacklistedApps.ts:4-19` |
| `lookout-last-monitor-selection` | `number[]` (monitor ids) | Preselect monitors in SourcePicker | `SourcePicker.tsx:58-80` |
| `lookout-perm-screen`, `lookout-perm-camera` | `"1"` | macOS permission grant cache; skips the PermissionScreen gate (re-verified in the background, removed if revoked) | `PermissionScreen.tsx:19,86`, `App.tsx:186-203` |
| `lookout-program-links` | `ProgramLink[]` = `{program, origin, pairUrl, startUrl, pairedAt /*ISO*/, deviceLabel /* "Lookout Desktop (macOS\|Windows\|Linux)" */, displayName?, iconUrl?}` | Paired-program metadata. The credential is in the secret store under `program-device-token:<program>` | `desk/src/programLink.ts:48-77,249-259` |
| `lookout-pending-pairing` | `{program, state, verifier, origin, pairUrl, startUrl, createdAt: epochMs, displayName?, iconUrl?}` | In-flight pairing (PKCE); TTL 10 min | `programLink.ts:62-78,278-340` |
| `lookout-panel-state` | `Record<token, "dismissed"\|"done">` (`"pending"` = absent) | Program panel per-session state | `desk/src/programPanel.ts:31,211-239` |
| `lookout-deeplink-sessions` | `Record<program, token>` (newest deep-linked token per program) | Tip targeting | `desk/src/tip.ts:31,63-74` |
| `lookout-tip-moments` | `Record<string, TipMomentRecord>`, where `TipMomentRecord = {token, moments: ("source"\|"panel-done")[]}` | Tip shown-once bookkeeping | `tip.ts:32-38,76-78` |
| `lookout_last_update_ts` | stringified epoch ms | Relaunch-loop guard (skips the check within 60 s) | `useAppUpdate.ts:10,190,309-312` |
| `lookout_pending_update` | `{version: string, attempts: number, notified: boolean}` | Downloaded-but-not-installed update marker | `useAppUpdate.ts:12,40-75` |
| `lookout_update_nag_dismissed` | version string | Linux update drawer dismissed for this version | `useLinuxUpdate.ts:12,55,75` |

The Rust-side secret store (`rs/src/secret_store.rs`) is a JSON map file in the app data dir, guarded by `WRITE_LOCK`. Its only keys are `program-device-token:<program>`.

### 6.6 Surprises / gotchas for the rewrite

- `StreamInfo` serializes as `node_id` (snake_case). This is the only snake_case payload.
- `AppEntry` has no rename attribute either (its fields are single words anyway).
- The TS `RecorderState` interface lacks `pendingUploads`, which Rust sends.
- `session_finish` returns `FinishOutcome {status, trackedSeconds, totalActiveSeconds}`, but TS types it as `{status}`.
- Five core events (`capture-tracked-seconds`, `capture-session-terminated`, `capture-tick-error`, `capture-source-lost`, `screencast-revoked`) are emitted with no JS listener. `capture-tick-result` is never emitted. Everything the UI needs is folded into `recorder-state`.
- `get_blacklisted_apps` is registered but never called. The blacklist's source of truth is localStorage, pushed to Rust on every mount.
- Deep links never use the deep-link plugin's JS API. They come through a custom event plus a cold-start poll (10 × 500 ms).
- Live preview runs on two paths:
  - (a) the `lookout-preview://` URI-scheme pull, at 1 fps for the selected source, 5 fps for a hovered window, and 1 fps in the recorder;
  - (b) the `capture-preview-frame` push, only while `main` is focused.
- The camera is captured by the webview (getUserMedia). The core requests frames over events and gets them back through commands. GPUI needs a native camera backend instead.
