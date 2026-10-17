import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { we2aiApi, type We2aiKeyList } from "@/we2ai/api";
import { PlaygroundPage } from "@/we2ai/PlaygroundPage";
import { getWe2aiStrings } from "@/we2ai/strings";

if (!Element.prototype.scrollIntoView)
  Element.prototype.scrollIntoView = () => {};
const KEYS: We2aiKeyList = {
  keys: [5, 6].map((id) => ({
    id,
    name: id === 5 ? "工作" : "备用",
    status: "active",
    maskedKey: `sk-…${id}`,
    groupName: null,
  })),
  selectedKeyId: 5,
  rememberedKeyId: null,
  studioSelectedKeyId: 5,
};
const caps = {
  sizes: [],
  aspectRatios: ["16:9"],
  imageSizes: [],
  durations: [10],
  resolutions: ["720p"],
  maxN: 1,
  supportsReference: false,
  supportsAudio: false,
  minReferenceSide: 0,
};
const onSessionMaybeEnded = vi.fn(),
  onTerminal = vi.fn();
async function mount(language: "zh" | "en" = "zh") {
  const view = render(
    <PlaygroundPage
      t={getWe2aiStrings(language)}
      language={language}
      onSessionMaybeEnded={onSessionMaybeEnded}
      onTerminal={onTerminal}
    />,
  );
  await screen.findByTestId("we2ai-studio-key-select");
  return view;
}

describe("PlaygroundPage 图片与视频工坊", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    onSessionMaybeEnded.mockClear();
    onTerminal.mockClear();
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue(KEYS);
    vi.spyOn(we2aiApi, "playgroundChooseKey").mockImplementation(
      async (keyId) => ({ studioSelectedKeyId: keyId }),
    );
    vi.spyOn(we2aiApi, "playgroundModels").mockResolvedValue({
      models: [{ id: "gpt-text", provider: null }],
      blockedReason: null,
    });
    vi.spyOn(we2aiApi, "playgroundMediaModels").mockImplementation(
      async (_id, kind) => ({
        models: [
          {
            id: kind === "image" ? "grok-image" : "grok-video",
            family: "grok",
            provider: null,
            capabilities: caps,
          },
        ],
        blockedReason: null,
      }),
    );
    vi.spyOn(we2aiApi, "onKeysChanged").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "onPlaygroundProgress").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "onPlaygroundChunk").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "playgroundCancel").mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "playgroundChat").mockResolvedValue({
      finishReason: "stop",
      usage: null,
    });
  });
  it("仅有图片和视频，默认视频，提示使用 Codex / WorkBuddy，挂载不订阅文本流或生成文案", async () => {
    await mount();
    expect(screen.getByTestId("we2ai-studio-mode-video")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByTestId("we2ai-studio-mode-image")).toBeInTheDocument();
    for (const id of [
      "we2ai-studio-mode-text",
      "we2ai-studio-text-mode",
      "we2ai-studio-input",
      "we2ai-studio-send",
    ])
      expect(screen.queryByTestId(id)).toBeNull();
    expect(
      screen.getByTestId("we2ai-studio-copywriting-hint"),
    ).toHaveTextContent("文案生成请使用 Codex 或 WorkBuddy");
    expect(we2aiApi.playgroundChat).not.toHaveBeenCalled();
    expect(we2aiApi.onPlaygroundChunk).not.toHaveBeenCalled();
  });
  it("图片模式同样显示引导，输入和模式切换后不取消任务", async () => {
    await mount();
    fireEvent.change(
      screen.getByTestId("we2ai-studio-video-field-productName"),
      { target: { value: "设备" } },
    );
    const video = vi
      .spyOn(we2aiApi, "playgroundVideo")
      .mockImplementation(() => new Promise(() => {}));
    await userEvent.click(screen.getByTestId("we2ai-studio-video-generate"));
    await screen.findByTestId("we2ai-studio-video-stop");
    await userEvent.click(screen.getByTestId("we2ai-studio-mode-image"));
    expect(screen.getByTestId("we2ai-studio-copywriting-hint")).toBeVisible();
    await userEvent.click(screen.getByTestId("we2ai-studio-mode-video"));
    expect(
      screen.getByTestId("we2ai-studio-video-field-productName"),
    ).toHaveValue("设备");
    expect(video).toHaveBeenCalledTimes(1);
    expect(we2aiApi.playgroundCancel).not.toHaveBeenCalled();
  });
  it("英文使用相应的文案工具引导", async () => {
    await mount("en");
    expect(
      screen.getByTestId("we2ai-studio-copywriting-hint"),
    ).toHaveTextContent("Use Codex or WorkBuddy to generate copy.");
  });
  it("图片的 AI 优化提示词继续使用内部文本模型，不保存为文案作品", async () => {
    await mount();
    await userEvent.click(screen.getByTestId("we2ai-studio-mode-image"));
    fireEvent.change(
      screen.getByTestId("we2ai-studio-image-field-productName"),
      { target: { value: "设备" } },
    );
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-image-optimize")).toBeEnabled(),
    );
    await userEvent.click(screen.getByTestId("we2ai-studio-image-optimize"));
    await waitFor(() =>
      expect(we2aiApi.playgroundChat).toHaveBeenCalledTimes(1),
    );
    expect(vi.mocked(we2aiApi.playgroundChat).mock.calls[0][0]).toMatchObject({
      keyId: 5,
      model: "gpt-text",
    });
    expect(
      vi.mocked(we2aiApi.playgroundChat).mock.calls[0][0].saveAsWork,
    ).not.toBe(true);
  });
  it("切换 Key 取消旧 Key 的视频，重新拉取媒体与优化模型", async () => {
    await mount();
    fireEvent.change(
      screen.getByTestId("we2ai-studio-video-field-productName"),
      { target: { value: "设备" } },
    );
    const video = vi
      .spyOn(we2aiApi, "playgroundVideo")
      .mockImplementation(() => new Promise(() => {}));
    await userEvent.click(screen.getByTestId("we2ai-studio-video-generate"));
    await screen.findByTestId("we2ai-studio-video-stop");
    await userEvent.click(screen.getByTestId("we2ai-studio-key-select"));
    await userEvent.click(
      within(await screen.findByRole("listbox")).getByText("备用"),
    );
    await waitFor(() =>
      expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(
        video.mock.calls[0][0].requestId,
      ),
    );
    expect(we2aiApi.playgroundChooseKey).toHaveBeenCalledWith(6);
    await waitFor(() =>
      expect(we2aiApi.playgroundModels).toHaveBeenCalledWith(6),
    );
    expect(
      screen.getByTestId("we2ai-studio-video-field-productName"),
    ).toHaveValue("");
  });
  it("Key 拉取被取代时有限重试，错误可手动恢复", async () => {
    vi.mocked(we2aiApi.listKeys).mockRejectedValueOnce({
      code: "KEY_LIST_SUPERSEDED",
      message: "superseded",
    });
    await mount();
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2);
  });
});
