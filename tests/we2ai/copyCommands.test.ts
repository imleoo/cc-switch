import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { we2aiApi } from "@/we2ai/api";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

describe("sample copy IPC payloads", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockClear();
  });

  it("copyPlainText sends only the text to we2ai_copy_text", async () => {
    await we2aiApi.copyPlainText("export X=1");
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("we2ai_copy_text", {
      text: "export X=1",
    });
  });

  it("copyTextWithKey sends only the key id and the text (no placeholder argument)", async () => {
    await we2aiApi.copyTextWithKey(7, "Bearer __WE2AI_API_KEY__");
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("we2ai_copy_text_with_key", {
      id: 7,
      text: "Bearer __WE2AI_API_KEY__",
    });
  });
});
