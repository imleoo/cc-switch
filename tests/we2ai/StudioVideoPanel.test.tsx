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
    supportsAudio: true,
    minReferenceSide: 300,
  },
};

const onSessionMaybeEnded = vi.fn();
const onTerminal = vi.fn();
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

  it("上传首帧参考图随请求下发，提示词追加保持外形条款", async () => {
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
    expect(prompt().value).toContain("first frame");
    await userEvent.click(generate());
    await waitFor(() => expect(request?.reference?.mime).toBe("image/png"));
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
    vi.mocked(we2aiApi.playgroundVideo).mockImplementationOnce(
      (r) =>
        new Promise(() => {
          request = r;
        }),
    );
    await userEvent.click(generate());
    await userEvent.click(await screen.findByTestId("we2ai-studio-video-stop"));
    expect(we2aiApi.playgroundCancel).toHaveBeenCalledWith(request?.requestId);
    vi.mocked(we2aiApi.playgroundCancel).mockClear();
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
});
