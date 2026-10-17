import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { we2aiApi } from "@/we2ai/api";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("video reference IPC", () => {
  it("preserves every reference and its order across the command boundary", async () => {
    vi.mocked(invoke).mockResolvedValue({ video: {} });
    const references = [
      { mime: "image/png" as const, dataBase64: "first" },
      { mime: "image/jpeg" as const, dataBase64: "second" },
    ];
    await we2aiApi.playgroundVideo({
      requestId: "r",
      keyId: 5,
      model: "grok-imagine-video",
      prompt: "p",
      references,
    });
    expect(invoke).toHaveBeenCalledWith("we2ai_playground_video", {
      request: expect.objectContaining({ reference: null, references }),
    });
  });
});
