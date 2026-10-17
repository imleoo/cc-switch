import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  we2aiApi,
  type We2aiMediaModel,
  type We2aiVideoProgressEvent,
  type We2aiVideoRequest,
} from "@/we2ai/api";
import * as mediaCommon from "@/we2ai/studioMediaCommon";
import { StudioVideoPanel } from "@/we2ai/StudioVideoPanel";
import { getStudioMediaStrings } from "@/we2ai/studioMediaStrings";
import { getWe2aiStrings } from "@/we2ai/strings";

// 全局 tauri mock 没有 convertFileSrc；视频预览用它把本机路径转成资源协议地址。
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: (path: string) => `asset://localhost${path}`,
}));

const t = getWe2aiStrings("zh");
const ms = getStudioMediaStrings("zh");

const GROK: We2aiMediaModel = {
  id: "grok-imagine-video-1.5",
  provider: null,
  family: "grok",
  capabilities: {
    sizes: [],
    aspectRatios: ["16:9", "9:16", "1:1"],
    imageSizes: [],
    durations: [5, 10],
    resolutions: ["480p", "720p"],
    maxN: 1,
    supportsReference: true,
    maxReferenceImages: 14,
    supportsAudio: false,
    minReferenceSide: 0,
  },
};
const SEEDANCE: We2aiMediaModel = {
  id: "doubao-seedance-2.0",
  provider: null,
  family: "seedance",
  capabilities: {
    sizes: [],
    aspectRatios: ["16:9", "9:16", "1:1", "4:3", "3:4"],
    imageSizes: [],
    durations: [5, 10, 15],
    resolutions: ["480p", "720p"],
    maxN: 1,
    supportsReference: true,
    maxReferenceImages: 9,
    supportsAudio: true,
    minReferenceSide: 300,
  },
};

const onSessionMaybeEnded = vi.fn();
const onTerminal = vi.fn();
const onWorkCompleted = vi.fn();
let progressHandler: ((e: We2aiVideoProgressEvent) => void) | null = null;

async function renderPanel(models: We2aiMediaModel[]) {
  vi.spyOn(we2aiApi, "playgroundMediaModels").mockResolvedValue({
    models,
    blockedReason: null,
  });
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(
      <StudioVideoPanel
        t={t}
        ms={ms}
        language="zh"
        keyId={5}
        textModel="gpt-text"
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
        onWorkCompleted={onWorkCompleted}
      />,
    );
  });
  await waitFor(() =>
    expect(screen.queryByTestId("we2ai-studio-video-loading")).toBeNull(),
  );
  return view;
}

const field = (id: string) =>
  screen.getByTestId(`we2ai-studio-video-field-${id}`);
const prompt = () =>
  screen.getByTestId("we2ai-studio-video-prompt") as HTMLTextAreaElement;
const generate = () => screen.getByTestId("we2ai-studio-video-generate");

describe("StudioVideoPanel（功能 27 P3）", () => {
  let request: We2aiVideoRequest | null = null;

  beforeEach(() => {
    vi.restoreAllMocks();
    request = null;
    progressHandler = null;
    onSessionMaybeEnded.mockClear();
    onTerminal.mockClear();
    onWorkCompleted.mockClear();
    vi.spyOn(we2aiApi, "playgroundCancel").mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "onPlaygroundProgress").mockImplementation(
      async (fn) => {
        progressHandler = fn;
        return () => {
          progressHandler = null;
        };
      },
    );
    vi.spyOn(we2aiApi, "playgroundVideo").mockImplementation(async (r) => {
      request = r;
      return {
        video: {
          path: "/home/u/.we2ai/studio/1-req-1.mp4",
          mime: "video/mp4",
          bytes: 100,
          dataUrl: null,
        },
      };
    });
  });

  it("Grok：英文五镜头提示词，默认 10 秒 / 16:9 / 720p，不展示音频开关", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    expect(prompt().value).toContain("Shot 5 (8-10s)");
    expect(prompt().value).toContain("[VMC-850]");
    expect(screen.queryByTestId("we2ai-studio-video-audio")).toBeNull();
    expect(
      screen.getByTestId("we2ai-studio-video-prompt-bytes"),
    ).toBeInTheDocument();
    await userEvent.click(generate());
    await waitFor(() => expect(request).not.toBeNull());
    expect(request).toMatchObject({
      keyId: 5,
      model: "grok-imagine-video-1.5",
      duration: 10,
      aspectRatio: "16:9",
      resolution: "720p",
    });
    expect(request?.generateAudio).toBeUndefined();
  });

  it("Seedance：中文提示词，可勾选音频，15 秒保持五镜头", async () => {
    await renderPanel([SEEDANCE]);
    await userEvent.type(field("productName"), "VMC-850");
    expect(prompt().value).toContain("镜头1（0-2秒）");
    await userEvent.selectOptions(
      screen.getByTestId("we2ai-studio-video-duration"),
      "15",
    );
    expect(prompt().value).toContain("镜头5（12-15秒）");
    expect(screen.queryByTestId("we2ai-studio-video-prompt-bytes")).toBeNull();
    await userEvent.click(screen.getByTestId("we2ai-studio-video-audio"));
    await userEvent.click(generate());
    await waitFor(() => expect(request).not.toBeNull());
    expect(request).toMatchObject({ duration: 15, generateAudio: true });
  });

  it("5 秒压缩为三镜头，并提示", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    await userEvent.selectOptions(
      screen.getByTestId("we2ai-studio-video-duration"),
      "5",
    );
    expect(prompt().value).toContain("Shot 3 (3-5s)");
    expect(prompt().value).not.toContain("Shot 4");
    expect(screen.getByText(ms.videoShortDurationNote)).toBeInTheDocument();
  });

  it("切换模型族后提示词语言随之切换，并重置参数选项", async () => {
    await renderPanel([GROK, SEEDANCE]);
    await userEvent.type(field("productName"), "VMC-850");
    expect(prompt().value).toContain("Shot 1");
    await userEvent.selectOptions(
      screen.getByTestId("we2ai-studio-video-model"),
      SEEDANCE.id,
    );
    expect(prompt().value).toContain("镜头1");
    expect(screen.getByTestId("we2ai-studio-video-audio")).toBeInTheDocument();
  });

  it("Grok 提示词超过 4096 字节时禁用生成并提示", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    fireEvent.change(prompt(), { target: { value: "机".repeat(1400) } });
    expect(
      screen.getByTestId("we2ai-studio-video-prompt-bytes"),
    ).toHaveTextContent(ms.promptTooLong);
    expect(generate()).toBeDisabled();
    fireEvent.change(prompt(), { target: { value: "short prompt" } });
    expect(generate()).toBeEnabled();
  });

  it("多选后继续添加，按顺序提交所有参考图，移除后更新编号和提示词", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    const input = screen.getByTestId("we2ai-studio-video-reference-input");
    expect(input).toHaveAttribute("multiple");
    const photos = ["front", "side", "factory"].map(
      (name) => new File([name], `${name}.png`, { type: "image/png" }),
    );
    await userEvent.upload(input, photos.slice(0, 2));
    await waitFor(() =>
      expect(
        screen.getAllByTestId("we2ai-studio-video-reference-preview"),
      ).toHaveLength(2),
    );
    await userEvent.upload(input, photos[2]);
    await waitFor(() => expect(prompt().value).toContain("<IMAGE_2>"));
    expect(
      screen.getByTestId("we2ai-studio-video-reference-count"),
    ).toHaveTextContent("3 / 14");
    await userEvent.click(
      screen.getByTestId("we2ai-studio-video-reference-remove-1"),
    );
    expect(prompt().value).toContain("<IMAGE_1>");
    expect(prompt().value).not.toContain("<IMAGE_2>");
    await userEvent.click(generate());
    await waitFor(() => expect(request?.references).toHaveLength(2));
    expect(request?.references?.map((image) => atob(image.dataBase64))).toEqual(
      ["front", "factory"],
    );
    expect(request?.reference).toBeUndefined();
  });

  it("切换到低上限模型时保留图片并阻止生成，移除多余图后可生成", async () => {
    const legacy = {
      ...SEEDANCE,
      id: "seedance-1.0-pro",
      capabilities: { ...SEEDANCE.capabilities, maxReferenceImages: 1 },
    };
    await renderPanel([GROK, legacy]);
    await userEvent.type(field("productName"), "Pump");
    await userEvent.upload(
      screen.getByTestId("we2ai-studio-video-reference-input"),
      [
        new File(["a"], "a.png", { type: "image/png" }),
        new File(["b"], "b.png", { type: "image/png" }),
      ],
    );
    await waitFor(() =>
      expect(
        screen.getAllByTestId("we2ai-studio-video-reference-preview"),
      ).toHaveLength(2),
    );
    await userEvent.selectOptions(
      screen.getByTestId("we2ai-studio-video-model"),
      legacy.id,
    );
    expect(
      screen.getAllByTestId("we2ai-studio-video-reference-preview"),
    ).toHaveLength(2);
    expect(generate()).toBeDisabled();
    expect(
      screen.getByTestId("we2ai-studio-video-reference-error"),
    ).toHaveTextContent("最多支持 1 张");
    await userEvent.click(
      screen.getByTestId("we2ai-studio-video-reference-remove-1"),
    );
    expect(generate()).toBeEnabled();
    expect(prompt().value).toContain("作为第一帧");
  });

  it("超张数、总大小和无效文件整批拒绝，保留已选图片", async () => {
    await renderPanel([
      {
        ...GROK,
        capabilities: { ...GROK.capabilities, maxReferenceImages: 2 },
      },
    ]);
    const input = screen.getByTestId("we2ai-studio-video-reference-input");
    const photo = new File(["a"], "a.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [photo, photo, photo] } });
    expect(
      screen.getByTestId("we2ai-studio-video-reference-error"),
    ).toHaveTextContent("最多支持 2 张");
    expect(
      screen.queryByTestId("we2ai-studio-video-reference-preview"),
    ).toBeNull();
    const large = new File(["a"], "large.png", { type: "image/png" });
    Object.defineProperty(large, "size", { value: 33 * 1024 * 1024 });
    fireEvent.change(input, { target: { files: [large] } });
    expect(
      screen.getByTestId("we2ai-studio-video-reference-error"),
    ).toHaveTextContent("32 MB");
    fireEvent.change(input, { target: { files: [photo] } });
    await waitFor(() =>
      expect(
        screen.getAllByTestId("we2ai-studio-video-reference-preview"),
      ).toHaveLength(1),
    );
    fireEvent.change(input, {
      target: { files: [new File(["bad"], "a.txt", { type: "text/plain" })] },
    });
    await waitFor(() =>
      expect(
        screen.getByTestId("we2ai-studio-video-reference-error"),
      ).toHaveTextContent("PNG"),
    );
    expect(
      screen.getAllByTestId("we2ai-studio-video-reference-preview"),
    ).toHaveLength(1);
  });

  it("图片读取完成前禁用生成与模型切换，编辑过的提示词不被覆盖并提醒核对编号", async () => {
    await renderPanel([GROK, SEEDANCE]);
    await userEvent.type(field("productName"), "Pump");
    fireEvent.change(prompt(), { target: { value: "my custom prompt" } });
    let finish!: (result: mediaCommon.ReferenceReadResult) => void;
    vi.spyOn(mediaCommon, "readReferenceFile").mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    fireEvent.change(screen.getByTestId("we2ai-studio-video-reference-input"), {
      target: { files: [new File(["a"], "a.png", { type: "image/png" })] },
    });
    expect(generate()).toBeDisabled();
    expect(screen.getByTestId("we2ai-studio-video-model")).toBeDisabled();
    await act(async () =>
      finish({
        ok: true,
        reference: { mime: "image/png", dataBase64: "YQ==" },
        previewUrl: "data:image/png;base64,YQ==",
      }),
    );
    expect(prompt().value).toBe("my custom prompt");
    expect(screen.getByText(ms.referencePromptChanged)).toBeInTheDocument();
    await userEvent.click(
      screen.getByTestId("we2ai-studio-video-prompt-reset"),
    );
    expect(prompt().value).toContain("<IMAGE_0>");
    expect(screen.queryByText(ms.referencePromptChanged)).toBeNull();
  });

  it("展示提交 / 生成中 / 下载阶段的进度，只认当前请求，结束后清掉", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    let finish!: () => void;
    vi.mocked(we2aiApi.playgroundVideo).mockImplementationOnce(
      (r) =>
        new Promise((resolve) => {
          request = r;
          finish = () =>
            resolve({
              video: {
                path: "/h/.we2ai/studio/a.mp4",
                mime: "video/mp4",
                bytes: 1,
                dataUrl: null,
              },
            });
        }),
    );
    await userEvent.click(generate());
    const progress = await screen.findByTestId("we2ai-studio-video-progress");
    expect(progress).toHaveTextContent(ms.videoStageSubmitted);
    act(() =>
      progressHandler?.({
        requestId: "someone-else",
        stage: "running",
        elapsedSecs: 99,
        progress: 1,
      }),
    );
    expect(progress).not.toHaveTextContent("99");
    act(() =>
      progressHandler?.({
        requestId: request!.requestId,
        stage: "running",
        elapsedSecs: 20,
        progress: 42,
      }),
    );
    expect(progress).toHaveTextContent("生成中 42% · 已用时 20 秒");
    act(() =>
      progressHandler?.({
        requestId: request!.requestId,
        stage: "downloading",
        elapsedSecs: 61,
        progress: 100,
      }),
    );
    expect(progress).toHaveTextContent(ms.videoStageDownloading);
    expect(
      screen.getByText(new RegExp(ms.videoCancelNote)),
    ).toBeInTheDocument();
    await act(async () => finish());
    await waitFor(() =>
      expect(screen.queryByTestId("we2ai-studio-video-progress")).toBeNull(),
    );
    expect(onTerminal).toHaveBeenCalledTimes(1);
  });

  it("下载无百分比时继续计时，完整下载后显示保存阶段并结束", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    let finish!: () => void;
    vi.mocked(we2aiApi.playgroundVideo).mockImplementationOnce(
      (r) =>
        new Promise((resolve) => {
          request = r;
          finish = () =>
            resolve({
              video: {
                path: "/h/.we2ai/studio/a.mp4",
                mime: "video/mp4",
                bytes: 100,
                dataUrl: null,
              },
            });
        }),
    );
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    try {
      fireEvent.click(generate());
      act(() =>
        progressHandler?.({
          requestId: request!.requestId,
          stage: "downloading",
          elapsedSecs: 107,
          progress: null,
        }),
      );
      const status = screen.getByTestId("we2ai-studio-video-progress");
      expect(status).toHaveTextContent("下载视频中 · 已用时 107 秒");
      expect(status).not.toHaveTextContent("100%");
      await act(async () => vi.advanceTimersByTime(5000));
      expect(status).toHaveTextContent("已用时 112 秒");
      act(() =>
        progressHandler?.({
          requestId: request!.requestId,
          stage: "saving",
          elapsedSecs: 112,
          progress: null,
        }),
      );
      expect(status).toHaveTextContent(ms.videoStageSaving);
      await act(async () => finish());
      expect(screen.queryByTestId("we2ai-studio-video-progress")).toBeNull();
      expect(
        screen.getByTestId("we2ai-studio-video-player"),
      ).toBeInTheDocument();
      expect(onTerminal).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("下载 IPC 不返回时结束等待，不重提任务且迟到结果不重复结算终态", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    let finish!: () => void;
    vi.mocked(we2aiApi.playgroundVideo).mockImplementationOnce(
      (r) =>
        new Promise((resolve) => {
          request = r;
          finish = () =>
            resolve({
              video: {
                path: "/h/.we2ai/studio/a.mp4",
                mime: "video/mp4",
                bytes: 100,
                dataUrl: null,
              },
            });
        }),
    );
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    try {
      fireEvent.click(generate());
      act(() =>
        progressHandler?.({
          requestId: request!.requestId,
          stage: "downloading",
          elapsedSecs: 107,
          progress: null,
        }),
      );
      await act(async () => vi.advanceTimersByTime(305_000));
      expect(screen.queryByTestId("we2ai-studio-video-progress")).toBeNull();
      expect(screen.getByTestId("we2ai-studio-video-error")).toHaveTextContent(
        ms.videoDownloadTimeout,
      );
      expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(
        request!.requestId,
      );
      expect(we2aiApi.playgroundVideo).toHaveBeenCalledTimes(1);
      expect(onTerminal).toHaveBeenCalledTimes(1);
      await act(async () => finish());
      expect(screen.queryByTestId("we2ai-studio-video-player")).toBeNull();
      expect(onTerminal).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("成片：应用内预览用资源协议地址；预览失败回退到系统播放器", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    await userEvent.click(generate());
    const player = await screen.findByTestId("we2ai-studio-video-player");
    expect(player).toHaveAttribute(
      "src",
      "asset://localhost/home/u/.we2ai/studio/1-req-1.mp4",
    );
    fireEvent.error(player);
    expect(
      await screen.findByTestId("we2ai-studio-video-preview-failed"),
    ).toHaveTextContent(ms.videoPreviewFailed);
    const open = vi
      .spyOn(we2aiApi, "playgroundMediaOpen")
      .mockResolvedValue(undefined);
    await userEvent.click(screen.getByTestId("we2ai-studio-video-open"));
    expect(open).toHaveBeenLastCalledWith(
      "/home/u/.we2ai/studio/1-req-1.mp4",
      false,
    );
    await userEvent.click(screen.getByTestId("we2ai-studio-video-reveal"));
    expect(open).toHaveBeenLastCalledWith(
      "/home/u/.we2ai/studio/1-req-1.mp4",
      true,
    );
  });

  it("Seedance 首帧图提示最小边长，Grok 不提示", async () => {
    await renderPanel([SEEDANCE]);
    expect(screen.getByText(/宽高都不能小于 300 像素/)).toBeInTheDocument();
    cleanup();
    await renderPanel([GROK]);
    expect(screen.queryByText(/宽高都不能小于/)).toBeNull();
  });

  it("上传单张参考图也按数组下发，提示词保持产品外形", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    const png = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "m.png", {
      type: "image/png",
    });
    await userEvent.upload(
      screen.getByTestId("we2ai-studio-video-reference-input"),
      png,
    );
    await screen.findByTestId("we2ai-studio-video-reference-preview");
    expect(prompt().value).toContain("References do not pin the first frame");
    await userEvent.click(generate());
    await waitFor(() =>
      expect(request?.references?.[0]?.mime).toBe("image/png"),
    );
  });

  it("失败映射为本地化文案：生成失败 / 超时 / 额度", async () => {
    await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    const cases: [string, string][] = [
      ["PLAYGROUND_MEDIA_FAILED", ms.errorMediaFailed],
      ["PLAYGROUND_MEDIA_TIMEOUT", ms.errorMediaTimeout],
      ["PLAYGROUND_QUOTA_EXHAUSTED", t.studioErrorQuota],
    ];
    for (const [code, text] of cases) {
      vi.mocked(we2aiApi.playgroundVideo).mockRejectedValueOnce({
        code,
        message: "raw",
      });
      await userEvent.click(generate());
      await waitFor(() =>
        expect(
          screen.getByTestId("we2ai-studio-video-error"),
        ).toHaveTextContent(text),
      );
    }
    expect(onTerminal).toHaveBeenCalledTimes(cases.length);
  });

  it("停止只取消当前请求；卸载同样取消", async () => {
    const view = await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    vi.mocked(we2aiApi.playgroundVideo).mockImplementation(
      (r) =>
        new Promise(() => {
          request = r;
        }),
    );
    await userEvent.click(generate());
    await userEvent.click(await screen.findByTestId("we2ai-studio-video-stop"));
    expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(request?.requestId);
    expect(generate()).toBeEnabled();
    expect(screen.queryByTestId("we2ai-studio-video-stop")).toBeNull();
    expect(onTerminal).toHaveBeenCalledTimes(1);
    vi.mocked(we2aiApi.playgroundCancel).mockClear();
    await userEvent.click(generate());
    await screen.findByTestId("we2ai-studio-video-stop");
    view.unmount();
    expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(request?.requestId);
  });

  it("该 Key 没有可识别的视频模型时给出空状态", async () => {
    await renderPanel([]);
    expect(
      screen.getByTestId("we2ai-studio-video-models-empty"),
    ).toHaveTextContent(ms.videoModelsEmpty);
    expect(generate()).toBeDisabled();
  });
  it("换 Key 或登出卸载后的迟到成功不发完成提醒", async () => {
    const view = await renderPanel([GROK]);
    await userEvent.type(field("productName"), "VMC-850");
    let finish!: () => void;
    vi.mocked(we2aiApi.playgroundVideo).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = () =>
            resolve({
              video: {
                path: "/h/.we2ai/studio/late.mp4",
                mime: "video/mp4",
                bytes: 1,
                dataUrl: null,
              },
            });
        }),
    );
    await userEvent.click(generate());
    await screen.findByTestId("we2ai-studio-video-stop");
    view.unmount();
    await act(async () => finish());
    expect(onWorkCompleted).not.toHaveBeenCalled();
  });
});
