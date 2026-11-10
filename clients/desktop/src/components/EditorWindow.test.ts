// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { buildClosePromptMessage } from "./EditorWindow.js";

describe("buildClosePromptMessage", () => {
    it("reports single cut when dirty", () => {
        expect(buildClosePromptMessage(1, 0, true)).toBe(
            "Closing publishes this timelapse with 1 cut applied. This can't be undone.",
        );
    });

    it("reports multiple cuts when dirty", () => {
        expect(buildClosePromptMessage(3, 0, true)).toBe(
            "Closing publishes this timelapse with 3 cuts applied. This can't be undone.",
        );
    });

    it("reports single mask when dirty", () => {
        expect(buildClosePromptMessage(0, 1, true)).toBe(
            "Closing publishes this timelapse with 1 mask applied. This can't be undone.",
        );
    });

    it("reports multiple masks when dirty", () => {
        expect(buildClosePromptMessage(0, 2, true)).toBe(
            "Closing publishes this timelapse with 2 masks applied. This can't be undone.",
        );
    });

    it("reports both cuts and masks when dirty", () => {
        expect(buildClosePromptMessage(2, 3, true)).toBe(
            "Closing publishes this timelapse with 2 cuts and 3 masks applied. This can't be undone.",
        );
        expect(buildClosePromptMessage(1, 1, true)).toBe(
            "Closing publishes this timelapse with 1 cut and 1 mask applied. This can't be undone.",
        );
    });

    it("reports published as recorded when cuts and masks are zero", () => {
        expect(buildClosePromptMessage(0, 0, true)).toBe(
            "Closing publishes this timelapse as recorded. This can't be undone.",
        );
    });

    it("reports published as recorded when not dirty", () => {
        expect(buildClosePromptMessage(2, 2, false)).toBe(
            "Closing publishes this timelapse as recorded. This can't be undone.",
        );
        expect(buildClosePromptMessage(0, 0, false)).toBe(
            "Closing publishes this timelapse as recorded. This can't be undone.",
        );
    });
});
