import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { we2aiApi, type We2aiKeyList, type We2aiKeyView } from "@/we2ai/api";
import { PlaygroundPage } from "@/we2ai/PlaygroundPage";
import { StudioImagePanel } from "@/we2ai/StudioImagePanel";
import { StudioVideoPanel } from "@/we2ai/StudioVideoPanel";
import { getStudioMediaStrings } from "@/we2ai/studioMediaStrings";
import { getWe2aiStrings } from "@/we2ai/strings";

// Radix Select 打开时会滚动到选中项，jsdom 没有实现 scrollIntoView。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

const t = getWe2aiStrings("zh");
const ms = getStudioMediaStrings("zh");

const key = (
  id: number,
  name: string,
  status: We2aiKeyView["status"] = "active",
): We2aiKeyView => ({
  id,
  name,
  groupName: null,
  status,
  maskedKey: `sk-…${id}`,
});

const list = (keys: We2aiKeyView[], selected: number): We2aiKeyList => ({
  keys,
  selectedKeyId: selected,
  rememberedKeyId: null,
  studioSelectedKeyId: selected,
});

const OLD = list([key(5, "旧Key")], 5);
const WITH_NEW = list([key(5, "旧Key"), key(9, "新Key")], 5);

const onSessionMaybeEnded = vi.fn();
const onTerminal = vi.fn();

/** 受控的 active：用按钮切换，模拟外壳切 Tab。 */
function Harness({ initialActive = true }: { initialActive?: boolean }) {
  const [active, setActive] = useState(initialActive);
  return (
    <>
      <button
        data-testid="toggle-active"
        onClick={() => setActive((a) => !a)}
      />
      <PlaygroundPage
        t={t}
        language="zh"
        active={active}
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />
    </>
  );
}

async function renderHarness(initialActive = true) {
  await act(async () => {
    render(<Harness initialActive={initialActive} />);
  });
  await waitFor(() =>
    expect(screen.getByTestId("we2ai-studio-key-select")).toBeInTheDocument(),
  );
}

async function optimize() {
  await userEvent.click(screen.getByTestId("we2ai-studio-mode-image"));
  fireEvent.change(screen.getByTestId("we2ai-studio-image-field-productName"), {
    target: { value: "设备" },
  });
  await waitFor(() =>
    expect(screen.getByTestId("we2ai-studio-image-optimize")).toBeEnabled(),
  );
  await userEvent.click(screen.getByTestId("we2ai-studio-image-optimize"));
}
async function startVideo() {
  vi.spyOn(we2aiApi, "playgroundVideo").mockImplementation(
    () => new Promise(() => {}),
  );
  fireEvent.change(screen.getByTestId("we2ai-studio-video-field-productName"), {
    target: { value: "设备" },
  });
  await userEvent.click(screen.getByTestId("we2ai-studio-video-generate"));
  await screen.findByTestId("we2ai-studio-video-stop");
}

async function openKeyOptions(): Promise<string[]> {
  await userEvent.click(screen.getByTestId("we2ai-studio-key-select"));
  const listbox = await screen.findByRole("listbox");
  const names = within(listbox)
    .getAllByRole("option")
    .map((o) => o.textContent ?? "");
  await userEvent.keyboard("{Escape}");
  return names;
}

describe("工坊 Key / 模型列表刷新（服务端改动客户端收不到事件）", () => {
  let current: We2aiKeyList;
  let models: { id: string; provider: string | null }[];
  let now: number;
  let keysChangedHandler: (() => void) | null;

  beforeEach(() => {
    vi.restoreAllMocks();
    onSessionMaybeEnded.mockClear();
    onTerminal.mockClear();
    current = OLD;
    models = [{ id: "gpt-a", provider: null }];
    now = 1_000_000;
    keysChangedHandler = null;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    vi.spyOn(we2aiApi, "listKeys").mockImplementation(async () => current);
    vi.spyOn(we2aiApi, "playgroundModels").mockImplementation(async () => ({
      models: models.slice(),
      blockedReason: null,
    }));
    vi.spyOn(we2aiApi, "playgroundMediaModels").mockImplementation(
      async (_id, kind) => ({
        models: [
          {
            id: kind === "image" ? "grok-image" : "grok-video",
            family: "grok",
            provider: null,
            capabilities: {
              sizes: [],
              aspectRatios: ["16:9"],
              imageSizes: [],
              durations: [10],
              resolutions: ["720p"],
              maxN: 1,
              supportsReference: false,
              supportsAudio: false,
              minReferenceSide: 0,
            },
          },
        ],
        blockedReason: null,
      }),
    );
    vi.spyOn(we2aiApi, "playgroundChat").mockResolvedValue({
      finishReason: "stop",
      usage: null,
    });
    vi.spyOn(we2aiApi, "playgroundCancel").mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "playgroundChooseKey").mockResolvedValue({
      studioSelectedKeyId: 5,
    });
    vi.spyOn(we2aiApi, "onKeysChanged").mockImplementation(async (fn) => {
      keysChangedHandler = fn;
      return () => {};
    });
    vi.spyOn(we2aiApi, "onPlaygroundChunk").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "onPlaygroundProgress").mockResolvedValue(() => {});
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("切回工坊 Tab 会重新拉，新增的 Key 出现；期间不显示加载态", async () => {
    await renderHarness();
    expect(await openKeyOptions()).toEqual(["旧Key"]);

    await userEvent.click(screen.getByTestId("toggle-active")); // 切走
    current = WITH_NEW; // 服务端新增了 Key，客户端没有事件
    let release!: () => void;
    vi.mocked(we2aiApi.listKeys).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = () => resolve(current);
        }),
    );
    await userEvent.click(screen.getByTestId("toggle-active")); // 切回
    await waitFor(() => expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2));
    // 静默：选择器不被“加载中”替换。
    expect(screen.queryByTestId("we2ai-studio-loading-keys")).toBeNull();
    expect(screen.getByTestId("we2ai-studio-key-select")).toBeInTheDocument();
    await act(async () => release());
    await waitFor(async () => {
      expect(await openKeyOptions()).toEqual(["旧Key", "新Key"]);
    });
  });

  it("首次挂载不会重复拉；非激活时切换不触发", async () => {
    await renderHarness(false);
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(1);
    window.dispatchEvent(new Event("focus"));
    now += 60_000;
    window.dispatchEvent(new Event("focus"));
    await act(async () => {});
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(1);
  });

  it("窗口聚焦刷新：30 秒内节流，超过后再触发；隐藏时不触发", async () => {
    await renderHarness();
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(1);

    window.dispatchEvent(new Event("focus")); // 距挂载拉取不足 30 秒
    await act(async () => {});
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(1);

    now += 31_000;
    const visibility = vi.spyOn(document, "visibilityState", "get");
    visibility.mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    await act(async () => {});
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(1);

    visibility.mockReturnValue("visible");
    window.dispatchEvent(new Event("focus"));
    await waitFor(() => expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2));

    window.dispatchEvent(new Event("focus")); // 刚刷新过，又被节流
    await act(async () => {});
    expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2);
  });

  it("手动刷新重拉优化模型，已有优化模型仍可用时保留", async () => {
    await renderHarness();
    models = [
      { id: "gpt-b", provider: null },
      { id: "gpt-a", provider: null },
    ];
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() =>
      expect(we2aiApi.playgroundModels).toHaveBeenCalledTimes(2),
    );
    await optimize();
    expect(vi.mocked(we2aiApi.playgroundChat).mock.calls[0][0].model).toBe(
      "gpt-a",
    );
  });

  it("优化模型被移除后使用新的可用模型", async () => {
    await renderHarness();
    models = [{ id: "gpt-c", provider: null }];
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await optimize();
    expect(vi.mocked(we2aiApi.playgroundChat).mock.calls[0][0].model).toBe(
      "gpt-c",
    );
  });

  it("选中的 Key 额度耗尽时静默刷新不重建媒体面板、不取消请求", async () => {
    await renderHarness();
    await startVideo();
    current = {
      ...list([key(5, "旧Key", "quota_exhausted")], 5),
      studioSelectedKeyId: null,
    };
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() => expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2));
    expect(screen.getByTestId("we2ai-studio-video-stop")).toBeInTheDocument();
    expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
      "旧Key",
    );
    expect(we2aiApi.playgroundCancel).not.toHaveBeenCalled();
  });

  it("选中的 Key 被删除时回退并取消旧媒体请求", async () => {
    current = WITH_NEW;
    await renderHarness();
    await startVideo();
    current = list([key(9, "新Key")], 9);
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "新Key",
      ),
    );
    await waitFor(() =>
      expect(we2aiApi.playgroundModels).toHaveBeenCalledWith(9),
    );
    expect(we2aiApi.playgroundCancel).toHaveBeenCalled();
    expect(
      screen.getByTestId("we2ai-studio-video-field-productName"),
    ).toHaveValue("");
  });

  it("刷新不会取消正在进行的视频生成", async () => {
    await renderHarness();
    await startVideo();
    current = WITH_NEW;
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() => expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2));
    expect(we2aiApi.playgroundCancel).not.toHaveBeenCalled();
    expect(screen.getByTestId("we2ai-studio-video-stop")).toBeInTheDocument();
  });

  it("刷新失败：保留旧列表，不弹错误；会话级错误仍通知外壳", async () => {
    await renderHarness();
    vi.mocked(we2aiApi.listKeys).mockRejectedValueOnce(new Error("offline"));
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() => expect(we2aiApi.listKeys).toHaveBeenCalledTimes(2));
    await act(async () => {});
    expect(screen.queryByTestId("we2ai-studio-keys-error")).toBeNull();
    expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
      "旧Key",
    );

    vi.mocked(we2aiApi.listKeys).mockRejectedValueOnce({
      code: "SESSION_CHANGED",
      message: "raw",
    });
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() => expect(onSessionMaybeEnded).toHaveBeenCalled());
  });

  it("Key 列表刷新成功后清除之前的会话切换错误并恢复选择器", async () => {
    vi.mocked(we2aiApi.listKeys).mockRejectedValueOnce({
      code: "SESSION_CHANGED",
      message: "会话已切换，操作已放弃",
    });
    await act(async () => {
      render(<Harness />);
    });
    expect(
      await screen.findByTestId("we2ai-studio-keys-error"),
    ).toHaveTextContent("会话已切换，操作已放弃");
    expect(onSessionMaybeEnded).toHaveBeenCalled();

    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() => {
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "旧Key",
      );
      expect(screen.queryByTestId("we2ai-studio-keys-error")).toBeNull();
    });
  });

  it("优化模型加载失败后手动刷新可恢复 AI 优化按钮", async () => {
    vi.mocked(we2aiApi.playgroundModels).mockRejectedValueOnce({
      code: "SESSION_CHANGED",
      message: "会话已切换",
    });
    await renderHarness();
    expect(onSessionMaybeEnded).toHaveBeenCalled();
    expect(screen.getByTestId("we2ai-studio-image-optimize")).toBeDisabled();
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-image-optimize")).toBeEnabled(),
    );
  });

  it("keys-changed 事件路径保持原有行为", async () => {
    await renderHarness();
    current = WITH_NEW;
    await act(async () => keysChangedHandler?.());
    await waitFor(async () => {
      expect(await openKeyOptions()).toEqual(["旧Key", "新Key"]);
    });
  });

  it("刷新同时通知图片 / 视频面板重拉各自的模型", async () => {
    await renderHarness();
    await waitFor(() => {
      expect(we2aiApi.playgroundMediaModels).toHaveBeenCalledWith(5, "image");
      expect(we2aiApi.playgroundMediaModels).toHaveBeenCalledWith(5, "video");
    });
    const before = vi.mocked(we2aiApi.playgroundMediaModels).mock.calls.length;
    await userEvent.click(screen.getByTestId("we2ai-studio-refresh"));
    await waitFor(() =>
      expect(vi.mocked(we2aiApi.playgroundMediaModels).mock.calls.length).toBe(
        before + 2,
      ),
    );
  });
});

describe("图片 / 视频面板：refreshTick 静默重拉模型", () => {
  const caps = {
    sizes: [],
    aspectRatios: ["1:1"],
    imageSizes: [],
    durations: [5],
    resolutions: ["480p"],
    maxN: 1,
    supportsReference: false,
    supportsAudio: false,
    minReferenceSide: 0,
  };
  const m = (id: string, family: string) => ({
    id,
    provider: null,
    family,
    capabilities: caps,
  });

  beforeEach(() => {
    vi.restoreAllMocks();
    vi.spyOn(we2aiApi, "playgroundCancel").mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "onPlaygroundProgress").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "onPlaygroundChunk").mockResolvedValue(() => {});
  });

  async function mountImage(tick: number) {
    return render(
      <StudioImagePanel
        t={t}
        ms={ms}
        language="zh"
        keyId={5}
        textModel={null}
        refreshTick={tick}
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />,
    );
  }

  it("图片：新增模型出现、已选模型保留、刷新期间不显示加载态", async () => {
    const spy = vi.spyOn(we2aiApi, "playgroundMediaModels").mockResolvedValue({
      models: [m("grok-img", "grok"), m("gpt-image", "openai")],
      blockedReason: null,
    });
    let view!: ReturnType<typeof render>;
    await act(async () => {
      view = await mountImage(0);
    });
    const select = (await screen.findByTestId(
      "we2ai-studio-image-model",
    )) as HTMLSelectElement;
    await userEvent.selectOptions(select, "gpt-image"); // 用户改选
    expect(select.value).toBe("gpt-image");

    let release!: () => void;
    spy.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = () =>
            resolve({
              models: [
                m("grok-img", "grok"),
                m("gpt-image", "openai"),
                m("gemini-img", "gemini"),
              ],
              blockedReason: null,
            });
        }),
    );
    view.rerender(
      <StudioImagePanel
        t={t}
        ms={ms}
        language="zh"
        keyId={5}
        textModel={null}
        refreshTick={1}
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />,
    );
    await waitFor(() => expect(spy).toHaveBeenCalledTimes(2));
    expect(screen.queryByTestId("we2ai-studio-image-loading")).toBeNull();
    await act(async () => release());
    await waitFor(() =>
      expect(
        within(screen.getByTestId("we2ai-studio-image-model"))
          .getAllByRole("option")
          .map((o) => o.textContent),
      ).toEqual(["grok-img", "gpt-image", "gemini-img"]),
    );
    expect(
      (screen.getByTestId("we2ai-studio-image-model") as HTMLSelectElement)
        .value,
    ).toBe("gpt-image");
  });

  it("图片：已选模型被移除时回到第一个；刷新失败保留旧列表", async () => {
    const spy = vi.spyOn(we2aiApi, "playgroundMediaModels").mockResolvedValue({
      models: [m("grok-img", "grok"), m("gpt-image", "openai")],
      blockedReason: null,
    });
    let view!: ReturnType<typeof render>;
    await act(async () => {
      view = await mountImage(0);
    });
    const select = (await screen.findByTestId(
      "we2ai-studio-image-model",
    )) as HTMLSelectElement;
    await userEvent.selectOptions(select, "gpt-image");

    spy.mockRejectedValueOnce(new Error("offline"));
    view.rerender(
      <StudioImagePanel
        t={t}
        ms={ms}
        language="zh"
        keyId={5}
        textModel={null}
        refreshTick={1}
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />,
    );
    await waitFor(() => expect(spy).toHaveBeenCalledTimes(2));
    await act(async () => {});
    expect(screen.queryByTestId("we2ai-studio-image-models-error")).toBeNull();
    expect(
      (screen.getByTestId("we2ai-studio-image-model") as HTMLSelectElement)
        .value,
    ).toBe("gpt-image");

    spy.mockResolvedValueOnce({
      models: [m("grok-img", "grok")],
      blockedReason: null,
    });
    view.rerender(
      <StudioImagePanel
        t={t}
        ms={ms}
        language="zh"
        keyId={5}
        textModel={null}
        refreshTick={2}
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />,
    );
    await waitFor(() =>
      expect(
        (screen.getByTestId("we2ai-studio-image-model") as HTMLSelectElement)
          .value,
      ).toBe("grok-img"),
    );
  });

  it("视频：refreshTick 变化触发静默重拉", async () => {
    const spy = vi.spyOn(we2aiApi, "playgroundMediaModels").mockResolvedValue({
      models: [m("grok-video", "grok")],
      blockedReason: null,
    });
    const props = {
      t,
      ms,
      language: "zh" as const,
      keyId: 5,
      textModel: null,
      onSessionMaybeEnded,
      onTerminal,
    };
    let view!: ReturnType<typeof render>;
    await act(async () => {
      view = render(<StudioVideoPanel {...props} refreshTick={0} />);
    });
    await screen.findByTestId("we2ai-studio-video-model");
    view.rerender(<StudioVideoPanel {...props} refreshTick={1} />);
    await waitFor(() => expect(spy).toHaveBeenCalledTimes(2));
    expect(screen.queryByTestId("we2ai-studio-video-loading")).toBeNull();
    // 同一个 tick 重渲染不会再拉。
    view.rerender(<StudioVideoPanel {...props} refreshTick={1} />);
    await act(async () => {});
    expect(spy).toHaveBeenCalledTimes(2);
  });
});
