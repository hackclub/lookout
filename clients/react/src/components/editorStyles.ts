// Scoped stylesheet for the timelapse editor.
//
// The SDK styles with inline objects, which can't express :hover,
// :focus-visible, or reduced-motion. Those states are not optional on a
// direct-manipulation surface — you need to see what you're about to grab —
// so the editor injects one small sheet the same way theme.ts does.

const EASE_OUT_QUART = "cubic-bezier(0.25, 1, 0.5, 1)";

export const EDITOR_STYLE_ID = "lookout-editor-styles";

export function injectEditorStyles(): void {
  if (typeof document === "undefined") return;
  if (document.querySelector(`style[data-${EDITOR_STYLE_ID}]`)) return;

  const style = document.createElement("style");
  style.setAttribute(`data-${EDITOR_STYLE_ID}`, "");
  style.textContent = `
    .lk-ed-strip { transition: box-shadow 180ms ${EASE_OUT_QUART}; }
    .lk-ed-strip:focus-visible {
      outline: none;
      box-shadow: 0 0 0 2px var(--color-bg-body), 0 0 0 4px var(--color-accent);
    }

    .lk-ed-region {
      transition: background-color 140ms ${EASE_OUT_QUART},
                  box-shadow 140ms ${EASE_OUT_QUART};
    }
    .lk-ed-region:hover { background-color: var(--color-cut-fill-hover); }

    .lk-ed-mask-span,
    .lk-ed-blur-span {
      transition: background-color 140ms ${EASE_OUT_QUART},
                  box-shadow 140ms ${EASE_OUT_QUART};
    }
    .lk-ed-mask-span:hover,
    .lk-ed-blur-span:hover {
      background-color: rgba(59, 130, 246, 0.45);
    }

    /* The grab target is deliberately wider than the visible grip: 12px of
       hit area, a 3px bar. Fitts's law on a 1-second-per-minute timeline. */
    .lk-ed-grip { transition: transform 140ms ${EASE_OUT_QUART}; }
    .lk-ed-handle:hover .lk-ed-grip { transform: scaleX(1.6); }

    /* The cap is a grab target, so it acknowledges the pointer — but
       subtly: it marks a position, it shouldn't dominate the timeline.
       The hover is driven from the (larger, invisible) hit area. */
    .lk-ed-playhead { transition: transform 120ms ${EASE_OUT_QUART}; }
    *:hover > .lk-ed-playhead { transform: scaleX(1.25); }
    *:active > .lk-ed-playhead { transform: scaleX(1.1); }

    .lk-ed-iconbtn {
      display: inline-flex; align-items: center; justify-content: center;
      background: transparent; cursor: pointer; padding: 0;
      color: var(--color-text-primary);
      border: 1px solid var(--color-border-default);
      transition: background-color 140ms ${EASE_OUT_QUART},
                  border-color 140ms ${EASE_OUT_QUART},
                  transform 140ms ${EASE_OUT_QUART};
    }
    .lk-ed-iconbtn:hover {
      background: var(--color-bg-surface);
      border-color: var(--color-border-hover);
    }
    .lk-ed-iconbtn:active { transform: scale(0.94); }
    .lk-ed-iconbtn:focus-visible {
      outline: none;
      box-shadow: 0 0 0 2px var(--color-bg-body), 0 0 0 4px var(--color-accent);
    }

    .lk-ed-scroll-track {
      scrollbar-width: none;
      -ms-overflow-style: none;
    }
    .lk-ed-scroll-track::-webkit-scrollbar {
      display: none;
    }

    .lk-ed-fade-in { animation: lk-ed-fade 160ms ${EASE_OUT_QUART} both; }
    @keyframes lk-ed-fade {
      from { opacity: 0; transform: translateY(4px); }
      to   { opacity: 1; transform: none; }
    }

    /* Motion here is all feedback — grip growth, region tint, the scrubber
       card arriving. Under reduced-motion the states still change, they
       just stop moving. */
    @media (prefers-reduced-motion: reduce) {
      .lk-ed-strip, .lk-ed-region, .lk-ed-grip, .lk-ed-iconbtn,
      .lk-ed-playhead {
        transition-duration: 1ms;
      }
      .lk-ed-fade-in { animation-duration: 1ms; }
      .lk-ed-handle:hover .lk-ed-grip { transform: none; }
      *:hover > .lk-ed-playhead { transform: none; }
    }
  `;
  document.head.appendChild(style);
}
