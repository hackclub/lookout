import { describe, expect, it, vi } from "vitest";
import { createLookoutClient } from "./client.js";

describe("uploadToR2 URL validation", () => {
    const client = createLookoutClient({
        baseUrl: "http://localhost:3000",
        token: "tok",
    });

    const dummyBlob = new Blob(["test"], { type: "image/jpeg" });

    it("rejects attacker domains disguised as localhost", async () => {
        await expect(
            client.uploadToR2("http://localhost.attacker.com/upload", dummyBlob),
        ).rejects.toThrow("Invalid upload URL: must be HTTPS or a relative path.");
    });

    it("rejects attacker domains disguised as 127.0.0.1", async () => {
        await expect(
            client.uploadToR2("http://127.0.0.1.attacker.com/upload", dummyBlob),
        ).rejects.toThrow("Invalid upload URL: must be HTTPS or a relative path.");
    });

    it("rejects protocol-relative URLs pointing to external hosts", async () => {
        await expect(
            client.uploadToR2("//localhost.attacker.com/upload", dummyBlob),
        ).rejects.toThrow("Invalid upload URL: must be HTTPS or a relative path.");
    });

    it("rejects plain HTTP non-local URLs", async () => {
        await expect(
            client.uploadToR2("http://example.com/upload", dummyBlob),
        ).rejects.toThrow("Invalid upload URL: must be HTTPS or a relative path.");
    });

    it("rejects non-HTTP schemes", async () => {
        await expect(
            client.uploadToR2("ftp://localhost/upload", dummyBlob),
        ).rejects.toThrow("Invalid upload URL: must be HTTPS or a relative path.");
    });

    it("allows localhost, 127.0.0.1, https, and relative URLs", async () => {
        const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 200 }));

        await expect(client.uploadToR2("http://localhost:3000/upload", dummyBlob)).resolves.toBeUndefined();
        await expect(client.uploadToR2("http://127.0.0.1:3000/upload", dummyBlob)).resolves.toBeUndefined();
        await expect(client.uploadToR2("https://r2.example.com/upload", dummyBlob)).resolves.toBeUndefined();
        await expect(client.uploadToR2("/api/upload", dummyBlob)).resolves.toBeUndefined();

        fetchSpy.mockRestore();
    });
});
