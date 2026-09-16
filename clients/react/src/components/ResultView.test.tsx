import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { ResultView } from "./ResultView.js";
import { LookoutProvider } from "../LookoutProvider.js";
import type { LookoutClient } from "../api/client.js";

vi.mock("./VideoPlayer.js", () => ({
    VideoPlayer: ({ src }: { src: string }) => <div data-testid="video-player">{src}</div>,
}));

vi.mock("@squircle-js/react", () => ({
    Squircle: ({ children }: { children: React.ReactNode }) => children,
}));

afterEach(cleanup);

function createMockClient(videoUrl: string): LookoutClient {
    return {
        resolveToken: async () => "token",
        getSession: vi.fn(),
        getUploadUrl: vi.fn(),
        confirmScreenshot: vi.fn(),
        uploadToR2: vi.fn(),
        pause: vi.fn(),
        resume: vi.fn(),
        stop: vi.fn(),
        rename: vi.fn(),
        getStatus: vi.fn(),
        getVideo: vi.fn().mockResolvedValue({ videoUrl }),
        getUnits: vi.fn(),
        setCuts: vi.fn(),
        applyCuts: vi.fn(),
        heartbeatEditing: vi.fn(),
    };
}

describe("<ResultView> URL validation", () => {
    it("rejects attacker domain disguised as localhost", async () => {
        const onComplete = vi.fn();
        const client = createMockClient("http://localhost.attacker.com/video.mp4");

        render(
            <LookoutProvider token="tok" client={client} callbacks={{ onComplete }}>
                <ResultView status="complete" trackedSeconds={120} />
            </LookoutProvider>,
        );

        await waitFor(() => {
            expect(screen.getByText("No video available")).toBeTruthy();
        });
        expect(onComplete).not.toHaveBeenCalled();
    });

    it("rejects attacker domain disguised as 127.0.0.1", async () => {
        const onComplete = vi.fn();
        const client = createMockClient("http://127.0.0.1.attacker.com/video.mp4");

        render(
            <LookoutProvider token="tok" client={client} callbacks={{ onComplete }}>
                <ResultView status="complete" trackedSeconds={120} />
            </LookoutProvider>,
        );

        await waitFor(() => {
            expect(screen.getByText("No video available")).toBeTruthy();
        });
        expect(onComplete).not.toHaveBeenCalled();
    });

    it("accepts valid https URL", async () => {
        const onComplete = vi.fn();
        const client = createMockClient("https://example.com/video.mp4");

        render(
            <LookoutProvider token="tok" client={client} callbacks={{ onComplete }}>
                <ResultView status="complete" trackedSeconds={120} />
            </LookoutProvider>,
        );

        await waitFor(() => {
            expect(screen.getByTestId("video-player")).toBeTruthy();
        });
        expect(onComplete).toHaveBeenCalledWith({ videoUrl: "https://example.com/video.mp4" });
    });

    it("accepts valid localhost URL", async () => {
        const onComplete = vi.fn();
        const client = createMockClient("http://localhost:3000/video.mp4");

        render(
            <LookoutProvider token="tok" client={client} callbacks={{ onComplete }}>
                <ResultView status="complete" trackedSeconds={120} />
            </LookoutProvider>,
        );

        await waitFor(() => {
            expect(screen.getByTestId("video-player")).toBeTruthy();
        });
        expect(onComplete).toHaveBeenCalledWith({ videoUrl: "http://localhost:3000/video.mp4" });
    });
});
