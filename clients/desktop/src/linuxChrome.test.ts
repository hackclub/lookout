// @vitest-environment happy-dom
/**
 * The colour maths behind following a GTK theme.
 *
 * These are the parts where being subtly wrong is invisible until someone on
 * Breeze reports that Lookout looks "a bit off": an alpha derived from the
 * wrong channel, a hex that loses its leading zero, a palette that only
 * half-applies when a theme defines some roles and not others.
 */
import { describe, expect, it, beforeEach } from "vitest";
import {
  accentForeground,
  applyThemePalette,
  applyWindowRadius,
  lighten,
  isDarkSurface,
  linuxFontStack,
  withAlpha,
  NO_THEME_COLORS,
  type ThemeColors,
} from "./linuxChrome.js";

describe("withAlpha", () => {
  it("keeps the channels and applies the opacity", () => {
    expect(withAlpha("#ffffff", 0.08)).toBe("rgba(255, 255, 255, 0.08)");
    expect(withAlpha("#242424", 0.7)).toBe("rgba(36, 36, 36, 0.7)");
  });

  it("accepts uppercase and surrounding space, as a theme might emit", () => {
    expect(withAlpha(" #A1B2C3 ", 0.5)).toBe("rgba(161, 178, 195, 0.5)");
  });

  it("refuses anything that isn't a six-digit hex", () => {
    // Shorthand, named colours and rgb() would all silently produce NaN
    // channels if parsed loosely.
    for (const bad of ["#fff", "red", "rgb(1,2,3)", "#12345", ""]) {
      expect(withAlpha(bad, 0.5)).toBeNull();
    }
  });
});

describe("lighten", () => {
  it("moves the colour towards white by the given fraction", () => {
    // Adwaita's popover sits 2.5% above its own base, and this is the pair
    // it ships: matching it exactly is the point of that number.
    expect(lighten("#383838", 0.025)).toBe("#3d3d3d");
    expect(lighten("#000000", 0.5)).toBe("#808080");
  });

  it("pads single-digit channels, so the hex stays six characters", () => {
    // 0x05 must not serialise as "5" and shift every later channel along.
    expect(lighten("#050505", 0)).toBe("#050505");
  });

  it("never overshoots white", () => {
    expect(lighten("#ffffff", 0.9)).toBe("#ffffff");
  });

  it("refuses a malformed colour rather than emitting NaN", () => {
    expect(lighten("nonsense", 0.1)).toBeNull();
  });
});

describe("isDarkSurface", () => {
  it("reads real window backgrounds on the right side of the line", () => {
    expect(isDarkSurface("#242424")).toBe(true);   // Adwaita dark
    expect(isDarkSurface("#2a2e32")).toBe(true);   // Breeze dark
    expect(isDarkSurface("#fafafa")).toBe(false);  // Adwaita light
    expect(isDarkSurface("#f2f2f2")).toBe(false);  // WhiteSur light
  });

  it("can't judge a colour it can't parse", () => {
    expect(isDarkSurface("#fff")).toBeNull();
  });
});

describe("accentForeground", () => {
  it("picks black on a genuinely bright accent", () => {
    // A custom theme's bright yellow — the case that matters now that any
    // GTK theme's accent_bg_color flows through here.
    expect(accentForeground("#f5c211")).toBe("#000000");
  });

  it("still picks white on GNOME's yellow, which was darkened for it", () => {
    // #c88800 looks like the case this guard exists for, and isn't: GNOME
    // chose that dark amber over a bright yellow precisely so white works,
    // and libadwaita pairs it with white. Strict WCAG would prefer black
    // here (6.98 vs 3.01), but matching the desktop beats out-contrasting
    // it — Breeze's blue is the same story. Don't "fix" this.
    expect(accentForeground("#c88800")).toBe("#ffffff");
  });

  it("picks white on the dark end", () => {
    expect(accentForeground("#9141ac")).toBe("#ffffff");
    expect(accentForeground("#3584e4")).toBe("#ffffff");
  });

  it("falls back to white on an unparseable accent", () => {
    expect(accentForeground("#abc")).toBe("#ffffff");
  });
});

describe("linuxFontStack", () => {
  it("puts the desktop's font first and never repeats it", () => {
    const stack = linuxFontStack("Cantarell");
    expect(stack.startsWith('"Cantarell"')).toBe(true);
    expect(stack.match(/"Cantarell"/g)).toHaveLength(1);
  });

  it("falls back to the GNOME families when the desktop won't say", () => {
    expect(linuxFontStack(null)).toBe(
      '"Adwaita Sans", "Cantarell", system-ui, "Geist", sans-serif',
    );
  });
});

describe("applyWindowRadius", () => {
  const read = () => document.documentElement.style.getPropertyValue("--lookout-theme-radius");

  beforeEach(() => {
    document.documentElement.removeAttribute("style");
  });

  it("rounds to whatever the theme rounds its windows to", () => {
    applyWindowRadius(15); // Yaru
    expect(read()).toBe("15px");
  });

  it("squares the window when the theme says zero", () => {
    // A theme built for square windows. Nothing else on this desktop is
    // rounded, so neither is Lookout.
    applyWindowRadius(0);
    expect(read()).toBe("0px");
  });

  it("falls back to the stylesheet when the theme won't say", () => {
    applyWindowRadius(15);
    applyWindowRadius(null);
    expect(read()).toBe("");
  });

  it("never sets the variable the snapped rule owns", () => {
    // Setting --lookout-window-radius inline would out-rank .lookout-snapped
    // and a maximized window would keep its rounded corners.
    applyWindowRadius(15);
    expect(document.documentElement.style.getPropertyValue("--lookout-window-radius")).toBe("");
  });
});

describe("applyThemePalette", () => {
  const root = () => document.documentElement;
  const read = (name: string) => root().style.getPropertyValue(name);

  beforeEach(() => {
    root().removeAttribute("style");
  });

  // Roughly Breeze Dark: a theme that is nothing like Adwaita's greys.
  const breeze: ThemeColors = {
    windowBg: "#2a2e32",
    windowFg: "#eff0f1",
    viewBg: "#1b1e20",
    border: "#4d4d4d",
    popoverBg: "#31363b",
    accent: "#3daee9",
  };

  it("maps the theme's surfaces onto the app's own tokens", () => {
    applyThemePalette(breeze, true);
    expect(read("--color-bg-body")).toBe("#2a2e32");
    expect(read("--color-bg-panel")).toBe("#1b1e20");
    expect(read("--color-text-primary")).toBe("#eff0f1");
    expect(read("--color-popover-bg")).toBe("#31363b");
    expect(read("--color-window-border")).toBe("#4d4d4d");
  });

  it("derives the overlays from the theme's foreground, not from Adwaita", () => {
    applyThemePalette(breeze, true);
    // This is what makes a light theme get dark overlays for free.
    expect(read("--color-bg-surface")).toBe("rgba(239, 240, 241, 0.08)");
    expect(read("--color-border-default")).toBe("rgba(239, 240, 241, 0.12)");
    expect(read("--color-headerbar-control")).toBe("rgba(239, 240, 241, 0.1)");
    expect(read("--color-text-secondary")).toBe("rgba(239, 240, 241, 0.7)");
  });

  it("lifts the popover's top stop above its own base", () => {
    applyThemePalette(breeze, true);
    expect(read("--color-popover-bg-top")).toBe("#363b40");
  });

  it("leaves a role the theme doesn't define on the app's palette", () => {
    applyThemePalette({ ...NO_THEME_COLORS, windowBg: "#2a2e32" }, true);
    expect(read("--color-bg-body")).toBe("#2a2e32");
    // No foreground means no derived overlays rather than overlays derived
    // from a colour we made up.
    expect(read("--color-text-primary")).toBe("");
    expect(read("--color-bg-surface")).toBe("");
    expect(read("--color-border-default")).toBe("");
  });

  it("ignores a palette whose polarity disagrees with the session", () => {
    // Installing WhiteSur-Dark or Breeze-Dark sets gtk-theme without touching
    // GNOME's color-scheme, so a dark theme in a light-scheme session is a
    // real configuration, not a contrived one.
    applyThemePalette(breeze, false);
    for (const name of ["--color-bg-body", "--color-text-primary", "--color-bg-surface"]) {
      expect(read(name)).toBe("");
    }
  });

  it("clears a palette applied earlier when the polarity later disagrees", () => {
    applyThemePalette(breeze, true);
    expect(read("--color-bg-body")).toBe("#2a2e32");
    // A light/dark switch must not leave the dark palette stranded.
    applyThemePalette(breeze, false);
    expect(read("--color-bg-body")).toBe("");
    expect(read("--color-text-secondary")).toBe("");
  });

  it("applies a light theme in a light session", () => {
    // Roughly WhiteSur-Light: the macOS lookalike case, matched correctly.
    applyThemePalette(
      { ...NO_THEME_COLORS, windowBg: "#f2f2f2", windowFg: "#2e3436", accent: "#0a84ff" },
      false,
    );
    expect(read("--color-bg-body")).toBe("#f2f2f2");
    // Overlays come out dark, because the theme's foreground is dark.
    expect(read("--color-border-default")).toBe("rgba(46, 52, 54, 0.12)");
  });

  it("still applies a palette that names no background to judge", () => {
    // Nothing to disagree with, so the theme's other roles are honoured.
    applyThemePalette({ ...NO_THEME_COLORS, border: "#4d4d4d" }, false);
    expect(read("--color-window-border")).toBe("#4d4d4d");
  });

  it("lifts a popover off the window when the theme names none", () => {
    // The case Yaru-dark forces: no popover_bg_color, and theme_base_color is
    // darker than the window so it must not be used. 9% off Adwaita's own
    // #242424 window lands exactly on the #383838 popover Adwaita ships.
    applyThemePalette({ ...NO_THEME_COLORS, windowBg: "#242424" }, true);
    expect(read("--color-popover-bg")).toBe("#383838");

    applyThemePalette({ ...NO_THEME_COLORS, windowBg: "#2c2c2c" }, true);
    // Above its own window, which is the whole point.
    expect(read("--color-popover-bg")).toBe("#3f3f3f");
  });

  it("uses the view colour for a light theme, where it really is elevated", () => {
    // Adwaita light: an #ffffff base over an #f6f5f4 window is the popover
    // Adwaita actually draws.
    applyThemePalette(
      { ...NO_THEME_COLORS, windowBg: "#f6f5f4", viewBg: "#ffffff" },
      false,
    );
    expect(read("--color-popover-bg")).toBe("#ffffff");
  });

  it("refuses the view colour on a dark theme, where it is recessed", () => {
    // Adwaita-dark and Yaru-dark both put the base *below* the window, so
    // taking it would punch a hole where the menu should float.
    for (const [windowBg, viewBg, expected] of [
      ["#353535", "#2d2d2d", "#474747"], // Adwaita-dark
      ["#2c2c2c", "#272727", "#3f3f3f"], // Yaru-dark
    ]) {
      applyThemePalette({ ...NO_THEME_COLORS, windowBg, viewBg }, true);
      expect(read("--color-popover-bg")).toBe(expected);
    }
  });

  it("leaves a light theme's popover level with the window", () => {
    // Deliberate: a near-white window has nothing to lighten into, and
    // Adwaita's own light pair (#fafafa window, #f7f7f7 popover) differs by
    // three parts in 255. The border and shadow do the separating.
    applyThemePalette({ ...NO_THEME_COLORS, windowBg: "#fafafa" }, false);
    expect(read("--color-popover-bg")).toBe("#fafafa");
    // ...and the border is what makes it readable as a separate surface.
    applyThemePalette(
      { ...NO_THEME_COLORS, windowBg: "#fafafa", border: "#d0d0d0" },
      false,
    );
    expect(read("--color-popover-border")).toBe("#d0d0d0");
  });

  it("prefers the theme's own popover colour when it has one", () => {
    applyThemePalette(breeze, true);
    expect(read("--color-popover-bg")).toBe("#31363b");
  });

  it("takes colours away again when a new theme stops defining them", () => {
    applyThemePalette(breeze, true);
    expect(read("--color-bg-body")).not.toBe("");
    // Re-applying an empty palette must not leave the old theme behind.
    applyThemePalette(NO_THEME_COLORS, true);
    for (const name of [
      "--color-bg-body",
      "--color-bg-panel",
      "--color-text-primary",
      "--color-bg-surface",
      "--color-popover-bg",
      "--color-popover-bg-top",
      "--color-window-border",
    ]) {
      expect(read(name)).toBe("");
    }
  });
});
