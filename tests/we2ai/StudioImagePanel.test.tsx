import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  we2aiApi,
  type We2aiImageRequest,
  type We2aiMediaCapabilities,
  type We2aiMediaModel,
  type We2aiMediaModels,
  type We2aiPlaygroundChunkEvent,
} from "@/we2ai/api";
import { StudioImagePanel } from "@/we2ai/StudioImagePanel";
import { getStudioMediaStrings } from "@/we2ai/studioMediaStrings";
import { getWe2aiStrings } from "@/we2ai/strings";

const t = getWe2aiStrings("zh");
const ms = getStudioMediaStrings("zh");

const BASE_CAPS: We2aiMediaCapabilities = {
  sizes: [],
  aspectRatios: [],
  imageSizes: [],
  durations: [],
  resolutions: [],
  maxN: 1,
  supportsReference: false,
  supportsAudio: false,
  minReferenceSide: 0,
};

const OPENAI: We2aiMediaModel = {
  id: "gpt-image-2",
  provider: null,
  family: "openai",
  capabilities: {
    ...BASE_CAPS,
    sizes: ["1024x1024", "1536x1024", "1024x1536"],
    maxN: 4,
    supportsReference: true,
  },
};
const GROK: We2aiMediaModel = {
  id: "grok-imagine-image-2.0",
  provider: null,
  family: "grok",
  // 2026-10-14 实测：Grok 图片按 aspect_ratio 出图，带参考图走编辑接口。
  capabilities: {
    ...BASE_CAPS,
    aspectRatios: ["1:1", "16:9", "9:16", "4:3", "3:4"],
    maxN: 4,
    supportsReference: true,
  },
};
const GEMINI: We2aiMediaModel = {
  id: "gemini-3.1-flash-image",
  provider: null,
  family: "gemini",
  capabilities: {
    ...BASE_CAPS,
    aspectRatios: ["1:1", "3:4", "4:3", "9:16", "16:9"],
    imageSizes: ["1K", "2K", "4K"],
    supportsReference: true,
  },
};

const onSessionMaybeEnded = vi.fn();
const onTerminal = vi.fn();

let chunkHandler: ((event: We2aiPlaygroundChunkEvent) => void) | null = null;

async function renderPanel(
  models: We2aiMediaModel[],
  textModel: string | null = "gpt-text",
) {
  const result: We2aiMediaModels = { models, blockedReason: null };
  vi.spyOn(we2aiApi, "playgroundMediaModels").mockResolvedValue(result);
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(
      <StudioImagePanel
        t={t}
        ms={ms}
        language="zh"
        keyId={5}
        textModel={textModel}
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />,
    );
  });
  // 空列表时没有下拉，只等加载结束。
  await waitFor(() =>
    expect(screen.queryByTestId("we2ai-studio-image-loading")).toBeNull(),
  );
  return view;
}

const field = (id: string) =>
  screen.getByTestId(`we2ai-studio-image-field-${id}`);
const prompt = () =>
  screen.getByTestId("we2ai-studio-image-prompt") as HTMLTextAreaElement;
const generate = () => screen.getByTestId("we2ai-studio-image-generate");

describe("StudioImagePanel（功能 27 P2）", () => {
  let request: We2aiImageRequest | null = null;

  beforeEach(() => {
    vi.restoreAllMocks();
    request = null;
    chunkHandler = null;
    onSessionMaybeEnded.mockClear();
    onTerminal.mockClear();
    vi.spyOn(we2aiApi, "playgroundCancel").mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "onPlaygroundChunk").mockImplementation(async (fn) => {
      chunkHandler = fn;
      return () => {
        chunkHandler = null;
      };
    });
    vi.spyOn(we2aiApi, "playgroundImage").mockImplementation(async (r) => {
      request = r;
      return {
        images: [
          {
            path: "/home/u/.we2ai/studio/1-req-1.png",
            mime: "image/png",
            bytes: 12,
            dataUrl: "data:image/png;base64,AAAA",
          },
        ],
      };
    });
  });

  it("未填产品名时不能生成，并提示缺哪一项", async () => {
    await renderPanel([OPENAI]);
    expect(generate()).toBeDisabled();
    expect(screen.getByTestId("we2ai-studio-image-missing")).toHaveTextContent(
      "产品名称",
    );
    await userEvent.type(field("productName"), "VMC-850");
    expect(generate()).toBeEnabled();
    expect(screen.queryByTestId("we2ai-studio-image-missing")).toBeNull();
  });

  it("模板 + 字段自动拼出提示词，生成时按能力表下发默认构图", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "VMC-850");
    expect(prompt().value).toContain("VMC-850");
    expect(prompt().value).toContain("No text, letters, numbers, logos");

    await userEvent.click(generate());
    await waitFor(() => expect(request).not.toBeNull());
    expect(request).toMatchObject({
      keyId: 5,
      model: "gpt-image-2",
      size: "1024x1024",
      n: 1,
    });
    expect(request?.prompt).toContain("VMC-850");
    expect(request?.reference).toBeUndefined();
    expect(
      await screen.findByTestId("we2ai-studio-image-result-0"),
    ).toHaveAttribute("src", "data:image/png;base64,AAAA");
    expect(onTerminal).toHaveBeenCalledTimes(1);
  });

  it("切换模板按该模板的构图下发（车间实景 → 横版）", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "VMC-850");
    await userEvent.click(
      screen.getByTestId("we2ai-studio-image-template-factory-floor"),
    );
    await userEvent.click(generate());
    await waitFor(() => expect(request?.size).toBe("1536x1024"));
  });

  it("手动改过提示词后，再改字段不会覆盖；点恢复才回到模板", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "VMC");
    fireEvent.change(prompt(), { target: { value: "my own prompt" } });
    expect(
      screen.getByTestId("we2ai-studio-image-prompt-dirty"),
    ).toBeInTheDocument();
    await userEvent.type(field("productName"), "-850");
    expect(prompt().value).toBe("my own prompt");
    await userEvent.click(
      screen.getByTestId("we2ai-studio-image-prompt-reset"),
    );
    expect(prompt().value).toContain("VMC-850");
  });

  it("Gemini：展示画幅与分辨率，不展示张数；构图取模板偏好的画幅", async () => {
    await renderPanel([GEMINI]);
    expect(screen.getByTestId("we2ai-studio-image-aspect")).toBeInTheDocument();
    expect(
      screen.getByTestId("we2ai-studio-image-quality"),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("we2ai-studio-image-count")).toBeNull();
    expect(screen.queryByTestId("we2ai-studio-image-size")).toBeNull();
    await userEvent.type(field("productName"), "Pump");
    await userEvent.selectOptions(
      screen.getByTestId("we2ai-studio-image-quality"),
      "2K",
    );
    await userEvent.click(generate());
    await waitFor(() => expect(request).not.toBeNull());
    expect(request).toMatchObject({ aspectRatio: "1:1", imageSize: "2K" });
    expect(request?.n).toBeUndefined();
    expect(request?.size).toBeUndefined();
  });

  it("Grok：按模板构图下发画幅、不下发 size，可选多张", async () => {
    await renderPanel([GROK]);
    expect(screen.queryByTestId("we2ai-studio-image-size")).toBeNull();
    expect(screen.getByTestId("we2ai-studio-image-aspect")).toBeInTheDocument();
    await userEvent.type(field("productName"), "Pump");
    await userEvent.click(
      screen.getByTestId("we2ai-studio-image-template-factory-floor"),
    );
    await userEvent.selectOptions(
      screen.getByTestId("we2ai-studio-image-count"),
      "3",
    );
    await userEvent.click(generate());
    await waitFor(() => expect(request).not.toBeNull());
    expect(request).toMatchObject({
      model: "grok-imagine-image-2.0",
      aspectRatio: "16:9",
      n: 3,
    });
    expect(request?.size).toBeUndefined();
  });

  it("Grok：支持参考图，随请求下发", async () => {
    await renderPanel([GROK]);
    expect(
      screen.queryByTestId("we2ai-studio-image-reference-unsupported"),
    ).toBeNull();
    await userEvent.type(field("productName"), "Pump");
    const png = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "m.png", {
      type: "image/png",
    });
    await userEvent.upload(
      screen.getByTestId("we2ai-studio-image-reference-input"),
      png,
    );
    await screen.findByTestId("we2ai-studio-image-reference-preview");
    await userEvent.click(generate());
    await waitFor(() => expect(request?.reference?.mime).toBe("image/png"));
  });

  it("上传参考图：预览、随请求下发，并在提示词里追加保持外形的条款", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "VMC-850");
    const png = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "m.png", {
      type: "image/png",
    });
    await userEvent.upload(
      screen.getByTestId("we2ai-studio-image-reference-input"),
      png,
    );
    expect(
      await screen.findByTestId("we2ai-studio-image-reference-preview"),
    ).toBeInTheDocument();
    expect(prompt().value).toContain("preserve its shape");
    await userEvent.click(generate());
    await waitFor(() => expect(request?.reference?.mime).toBe("image/png"));
    expect(request?.reference?.dataBase64).toBe("iVBORw==");
  });

  it("参考图类型不对或超过 8 MB 时提示，不进入请求", async () => {
    await renderPanel([OPENAI]);
    const gif = new File(["x"], "a.gif", { type: "image/gif" });
    fireEvent.change(screen.getByTestId("we2ai-studio-image-reference-input"), {
      target: { files: [gif] },
    });
    expect(
      await screen.findByTestId("we2ai-studio-image-reference-error"),
    ).toHaveTextContent(ms.referenceBadType);
    const big = new File([new Uint8Array(8 * 1024 * 1024 + 1)], "b.png", {
      type: "image/png",
    });
    fireEvent.change(screen.getByTestId("we2ai-studio-image-reference-input"), {
      target: { files: [big] },
    });
    await waitFor(() =>
      expect(
        screen.getByTestId("we2ai-studio-image-reference-error"),
      ).toHaveTextContent(ms.referenceTooLarge),
    );
    expect(
      screen.queryByTestId("we2ai-studio-image-reference-preview"),
    ).toBeNull();
  });

  it("网关拒绝 / Key 失效映射为本地化文案，且仍刷新余额", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "Pump");
    vi.mocked(we2aiApi.playgroundImage).mockRejectedValueOnce({
      code: "PLAYGROUND_REJECTED",
      message: "raw",
    });
    await userEvent.click(generate());
    expect(
      await screen.findByTestId("we2ai-studio-image-error"),
    ).toHaveTextContent(ms.errorRejected);
    expect(onTerminal).toHaveBeenCalledTimes(1);

    vi.mocked(we2aiApi.playgroundImage).mockRejectedValueOnce({
      code: "PLAYGROUND_KEY_INVALID",
      message: "raw",
    });
    await userEvent.click(generate());
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-image-error")).toHaveTextContent(
        t.studioErrorKeyInvalid,
      ),
    );
  });

  it("会话级错误通知外壳复查会话", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "Pump");
    vi.mocked(we2aiApi.playgroundImage).mockRejectedValueOnce({
      code: "PLAYGROUND_IDENTITY_CHANGED",
      message: "raw",
    });
    await userEvent.click(generate());
    await waitFor(() => expect(onSessionMaybeEnded).toHaveBeenCalled());
  });

  it("生成中显示停止按钮，点击取消当前请求", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "Pump");
    let release!: () => void;
    vi.mocked(we2aiApi.playgroundImage).mockImplementationOnce(
      (r) =>
        new Promise((_, reject) => {
          request = r;
          release = () =>
            reject({ code: "PLAYGROUND_CANCELLED", message: "x" });
        }),
    );
    await userEvent.click(generate());
    const stop = await screen.findByTestId("we2ai-studio-image-stop");
    await userEvent.click(stop);
    expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(request?.requestId);
    await act(async () => release());
    await waitFor(() => expect(generate()).toBeInTheDocument());
  });

  it("卸载时取消在途请求（换 Key / 登出重建）", async () => {
    const view = await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "Pump");
    vi.mocked(we2aiApi.playgroundImage).mockImplementationOnce(
      (r) =>
        new Promise(() => {
          request = r;
        }),
    );
    await userEvent.click(generate());
    await waitFor(() => expect(request).not.toBeNull());
    expect(we2aiApi.playgroundCancel).not.toHaveBeenCalled();
    view.unmount();
    expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(request?.requestId);
  });

  it("AI 优化提示词：流式写回，用文本模型并带上字段摘要", async () => {
    await renderPanel([OPENAI], "gpt-text");
    await userEvent.type(field("productName"), "VMC-850");
    const original = prompt().value;
    const chat = vi
      .spyOn(we2aiApi, "playgroundChat")
      .mockImplementation(async (r) => {
        chunkHandler?.({ requestId: r.requestId, delta: "A polished " });
        chunkHandler?.({ requestId: r.requestId, delta: "prompt" });
        return { finishReason: "stop", usage: null };
      });
    await userEvent.click(screen.getByTestId("we2ai-studio-image-optimize"));
    await waitFor(() => expect(prompt().value).toBe("A polished prompt"));
    const sent = chat.mock.calls[0][0];
    expect(sent.model).toBe("gpt-text");
    expect(sent.system).toContain("never invent specifications");
    expect(sent.messages[0].content).toContain("产品名称: VMC-850");
    expect(sent.messages[0].content).toContain(original);
    expect(onTerminal).toHaveBeenCalled();
  });

  it("AI 优化失败时恢复原提示词并报错", async () => {
    await renderPanel([OPENAI], "gpt-text");
    await userEvent.type(field("productName"), "VMC-850");
    const original = prompt().value;
    vi.spyOn(we2aiApi, "playgroundChat").mockRejectedValue({
      code: "PLAYGROUND_RATE_LIMITED",
      message: "raw",
    });
    await userEvent.click(screen.getByTestId("we2ai-studio-image-optimize"));
    await waitFor(() => expect(prompt().value).toBe(original));
    expect(screen.getByTestId("we2ai-studio-image-error")).toHaveTextContent(
      t.studioErrorRateLimited,
    );
  });

  it("没有文本模型时 AI 优化置灰", async () => {
    await renderPanel([OPENAI], null);
    await userEvent.type(field("productName"), "VMC-850");
    expect(screen.getByTestId("we2ai-studio-image-optimize")).toBeDisabled();
  });

  it("另存为与在文件夹中显示走受限命令；用户取消另存不报错", async () => {
    await renderPanel([OPENAI]);
    await userEvent.type(field("productName"), "Pump");
    await userEvent.click(generate());
    const saveAs = vi
      .spyOn(we2aiApi, "playgroundMediaSaveAs")
      .mockResolvedValueOnce("/Users/u/a.png");
    const open = vi
      .spyOn(we2aiApi, "playgroundMediaOpen")
      .mockResolvedValue(undefined);
    await userEvent.click(
      await screen.findByTestId("we2ai-studio-image-save-0"),
    );
    expect(saveAs).toHaveBeenCalledWith("/home/u/.we2ai/studio/1-req-1.png");
    expect(
      await screen.findByTestId("we2ai-studio-image-notice"),
    ).toHaveTextContent("/Users/u/a.png");
    await userEvent.click(screen.getByTestId("we2ai-studio-image-reveal-0"));
    expect(open).toHaveBeenCalledWith(
      "/home/u/.we2ai/studio/1-req-1.png",
      true,
    );
    saveAs.mockResolvedValueOnce(null);
    await userEvent.click(screen.getByTestId("we2ai-studio-image-save-0"));
    expect(screen.queryByTestId("we2ai-studio-image-error")).toBeNull();
  });

  it("该 Key 没有图片模型时给出空状态，不能生成", async () => {
    await renderPanel([]);
    expect(
      screen.getByTestId("we2ai-studio-image-models-empty"),
    ).toHaveTextContent(ms.imageModelsEmpty);
    expect(generate()).toBeDisabled();
  });
});
