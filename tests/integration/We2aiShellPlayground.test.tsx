import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { emitTauriEvent } from "../msw/tauriMocks";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { UpdateProvider } from "@/contexts/UpdateContext";
import { isWe2aiIpcAllowed } from "@/we2ai/ipcWhitelist";
import {
  we2aiApi,
  type We2aiStudioWork,
  type We2aiVideoOutcome,
} from "@/we2ai/api";
import { We2aiShell } from "@/we2ai/We2aiShell";

const TAURI_ENDPOINT = "http://tauri.local";

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: async () => "4.20.4",
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: async () => null,
}));

// jsdom 未实现 scrollIntoView（Radix Select 打开时会调用）与 matchMedia。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}
if (typeof window.matchMedia !== "function") {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

function renderShell() {
  return render(
    <ThemeProvider defaultTheme="system" storageKey="we2ai-test-theme">
      <UpdateProvider>
        <We2aiShell />
        <Toaster />
      </UpdateProvider>
    </ThemeProvider>,
  );
}

const LOGGED_IN = {
  loggedIn: true,
  userId: 42,
  region: "international",
  emailMasked: "u****@we2ai.com",
  keyringDegraded: false,
  indexDegraded: false,
  offlineRetryInSeconds: null,
  localCleanupPending: false,
};

interface RecordedCall {
  command: string;
  body: any;
}

function mockShellCommands(
  extra: Record<string, (body: any) => JsonBodyType> = {},
) {
  const calls: RecordedCall[] = [];
  let balanceCalls = 0;
  server.use(
    http.post(`${TAURI_ENDPOINT}/*`, async ({ request }) => {
      const command = request.url.slice(`${TAURI_ENDPOINT}/`.length);
      const text = await request.text();
      const body = text ? JSON.parse(text) : {};
      calls.push({ command, body });
      const handler = extra[command];
      if (handler) return HttpResponse.json(handler(body));
      switch (command) {
        case "we2ai_get_settings":
          return HttpResponse.json({
            language: "zh",
            silentStartup: false,
            useAppWindowControls: false,
            showInTray: true,
            minimizeToTrayOnClose: false,
          });
        case "get_auto_launch_status":
          return HttpResponse.json(false);
        case "we2ai_get_last_region":
          return HttpResponse.json("international");
        case "we2ai_resume_session":
          return HttpResponse.json("restored");
        case "we2ai_session_status":
          return HttpResponse.json(LOGGED_IN);
        case "we2ai_gateway_info":
          return HttpResponse.json({
            baseUrl: "https://api.we2ai.com",
            webUrl: "https://we2ai.com",
          });
        case "we2ai_get_balance": {
          // 递增余额：等待 chip 显示新值才能证明 setBalance 已提交、重渲染完成，
          // 调用计数本身不构成重渲染屏障（Codex 验收修复项 2）。
          balanceCalls += 1;
          return HttpResponse.json({
            balance: 12.48 + balanceCalls,
            frozenBalance: 0,
            totalRecharged: 0,
          });
        }
        case "we2ai_list_keys":
          return HttpResponse.json({
            keys: [
              {
                id: 5,
                name: "工作",
                groupName: null,
                status: "active",
                maskedKey: "sk-we2…5555",
              },
              {
                id: 6,
                name: "备用",
                groupName: null,
                status: "active",
                maskedKey: "sk-we2…6666",
              },
            ],
            selectedKeyId: 5,
            rememberedKeyId: null,
            studioSelectedKeyId: 5,
          });
        case "we2ai_playground_media_models":
          return HttpResponse.json({
            models: ["grok", "grok-second"].map((id) => ({
              id: `${id}-${body.kind}`,
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
            })),
            blockedReason: null,
          });
        case "we2ai_playground_video":
          return HttpResponse.json({
            video: {
              path: "/home/u/.we2ai/studio/generated.mp4",
              mime: "video/mp4",
              bytes: 1234,
              dataUrl: null,
            },
          });
        case "we2ai_playground_models":
          return HttpResponse.json({
            models: [
              { id: "gpt-marketing", provider: null },
              { id: "gpt-second", provider: null },
            ],
            blockedReason: null,
          });
        case "we2ai_playground_chat": {
          const requestId = body?.request?.requestId as string;
          // 响应前先推两个增量（模拟 Rust 流式事件），再返回终态。
          emitTauriEvent("we2ai-playground-chunk", {
            requestId,
            delta: "第一段",
          });
          emitTauriEvent("we2ai-playground-chunk", {
            requestId,
            delta: "第二段",
          });
          return HttpResponse.json({
            finishReason: "stop",
            usage: {
              promptTokens: 10,
              completionTokens: 6,
              totalTokens: 16,
            },
          });
        }
        case "we2ai_playground_choose_key":
          return HttpResponse.json({
            studioSelectedKeyId: body?.keyId ?? 5,
          });
        default:
          return HttpResponse.json(null);
      }
    }),
  );
  return calls;
}

describe("We2aiShell AI marketing studio tab (feature 27)", () => {
  it("视频任务切换模式和顶栏功能后继续，完成后提醒并跳到作品列表", async () => {
    mockShellCommands();
    const user = userEvent.setup();
    const file = {
      path: "/home/u/.we2ai/studio/background.mp4",
      mime: "video/mp4",
      bytes: 1234,
      dataUrl: null,
    };
    const work: We2aiStudioWork = {
      id: "background",
      name: "后台视频",
      kind: "video",
      status: "processing",
      savedAtMs: Date.now(),
      favorite: false,
      model: "grok-imagine-video",
      progress: 35,
      message: null,
      content: null,
      files: [],
    };
    const cancel = vi
      .spyOn(we2aiApi, "playgroundCancel")
      .mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "playgroundMediaModels").mockImplementation(
      async (_id, kind) => ({
        models:
          kind === "video"
            ? [
                {
                  id: "grok-imagine-video",
                  provider: null,
                  family: "grok",
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
              ]
            : [],
        blockedReason: null,
      }),
    );
    let finish!: (outcome: We2aiVideoOutcome) => void;
    const video = vi.spyOn(we2aiApi, "playgroundVideo").mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    vi.spyOn(we2aiApi, "playgroundMediaList").mockImplementation(async () => [
      { ...work },
    ]);
    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    fireEvent.change(
      await screen.findByTestId("we2ai-studio-video-field-productName"),
      { target: { value: "工业设备" } },
    );
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-video-generate")).toBeEnabled(),
    );
    await user.click(screen.getByTestId("we2ai-studio-video-generate"));
    await waitFor(() => expect(video).toHaveBeenCalledTimes(1));
    for (const mode of ["image", "video"])
      await user.click(screen.getByTestId(`we2ai-studio-mode-${mode}`));
    for (const name of ["Key 管理", "充值", "设置", "作品列表"])
      await user.click(screen.getByRole("tab", { name }));
    expect(await screen.findByTestId("we2ai-work-status")).toHaveTextContent(
      "生成中",
    );
    expect(cancel).not.toHaveBeenCalled();
    await user.click(screen.getByRole("tab", { name: "设置" }));
    work.status = "success";
    work.progress = 100;
    work.files = [file];
    await act(async () => {
      finish({ video: file });
    });
    const notice = await screen.findByTestId("we2ai-work-ready");
    expect(notice).toHaveTextContent("作品已生成并保存");
    await user.click(
      within(notice).getByRole("button", { name: "查看作品列表" }),
    );
    expect(screen.getByRole("tab", { name: "作品列表" })).toHaveAttribute(
      "data-state",
      "active",
    );
    expect(await screen.findByTestId("we2ai-work-player")).toBeInTheDocument();
    expect(screen.queryByTestId("we2ai-work-ready")).toBeNull();
    expect(video).toHaveBeenCalledTimes(1);
    expect(cancel).not.toHaveBeenCalled();
  });
  afterEach(() => {
    server.resetHandlers();
    vi.restoreAllMocks();
  });

  it("orders the tabs marketplace | studio | works | keys | billing | settings", async () => {
    mockShellCommands();
    renderShell();
    await screen.findByTestId("balance-chip");
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual([
      "模型广场",
      "AI 营销工坊",
      "作品列表",
      "Key 管理",
      "充值",
      "设置",
    ]);
  });

  it("作品入口读取本机已保存视频，重新进入仍可下载", async () => {
    const calls = mockShellCommands({
      we2ai_playground_media_list: () => [
        {
          id: "saved",
          kind: "video",
          status: "success",
          favorite: false,
          model: "grok",
          progress: 100,
          message: null,
          content: null,
          name: "saved.mp4",
          savedAtMs: 1792162964000,
          files: [
            {
              path: "/home/u/.we2ai/studio/saved.mp4",
              mime: "video/mp4",
              bytes: 8000000,
              dataUrl: null,
            },
          ],
        },
      ],
      we2ai_playground_media_save_as: () => "/downloads/saved.mp4",
    });
    const user = userEvent.setup();
    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(screen.getByRole("tab", { name: "作品列表" }));
    await screen.findByTestId("we2ai-work-player");
    await user.click(screen.getByRole("button", { name: "下载 / 另存为" }));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "/downloads/saved.mp4",
    );
    await user.click(screen.getByRole("tab", { name: "模型广场" }));
    await user.click(screen.getByRole("tab", { name: "作品列表" }));
    await screen.findByTestId("we2ai-work-player");
    expect(
      calls.filter((c) => c.command === "we2ai_playground_media_list"),
    ).toHaveLength(2);
  });

  it("只展示生图和视频，生成视频后刷新余额，全部调用符合 IPC 白名单", async () => {
    const calls = mockShellCommands();
    const user = userEvent.setup();
    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    await screen.findByTestId("we2ai-studio-video-model");
    expect(screen.queryByTestId("we2ai-studio-mode-text")).toBeNull();
    expect(
      screen.getByTestId("we2ai-studio-copywriting-hint"),
    ).toHaveTextContent("Codex 或 WorkBuddy");
    fireEvent.change(
      screen.getByTestId("we2ai-studio-video-field-productName"),
      { target: { value: "智能保温杯" } },
    );
    await user.click(screen.getByTestId("we2ai-studio-video-generate"));
    await screen.findByTestId("we2ai-work-ready");
    const video = calls.find((c) => c.command === "we2ai_playground_video");
    expect(video?.body.request).toMatchObject({
      keyId: 5,
      model: "grok-video",
      duration: 10,
      aspectRatio: "16:9",
    });
    expect(video?.body.request.prompt).toContain("智能保温杯");
    expect(calls.some((c) => c.command === "we2ai_playground_chat")).toBe(
      false,
    );
    await waitFor(() =>
      expect(
        calls.filter((c) => c.command === "we2ai_get_balance").length,
      ).toBeGreaterThanOrEqual(2),
    );
    for (const { command } of calls)
      expect(isWe2aiIpcAllowed(command), command).toBe(true);
  });

  it("切换顶栏功能保留图片 / 视频表单和当前模式", async () => {
    mockShellCommands();
    const user = userEvent.setup();
    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    await screen.findByTestId("we2ai-studio-video-model");
    fireEvent.change(
      screen.getByTestId("we2ai-studio-video-field-productName"),
      { target: { value: "跨 Tab 保留" } },
    );
    await user.click(screen.getByTestId("we2ai-studio-mode-image"));
    await user.click(screen.getByRole("tab", { name: "模型广场" }));
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    expect(screen.getByTestId("we2ai-studio-mode-image")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await user.click(screen.getByTestId("we2ai-studio-mode-video"));
    expect(
      screen.getByTestId("we2ai-studio-video-field-productName"),
    ).toHaveValue("跨 Tab 保留");
  });

  it("服务端新增 Key 后切回工坊刷新选择器，且媒体表单不丢", async () => {
    const keyOf = (id: number, name: string) => ({
      id,
      name,
      groupName: null,
      status: "active",
      maskedKey: `sk-…${id}`,
    });
    let keys = [keyOf(5, "工作")];
    mockShellCommands({
      we2ai_list_keys: () => ({
        keys,
        selectedKeyId: 5,
        rememberedKeyId: null,
        studioSelectedKeyId: 5,
      }),
    });
    const user = userEvent.setup();
    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    await screen.findByTestId("we2ai-studio-video-model");
    fireEvent.change(
      screen.getByTestId("we2ai-studio-video-field-productName"),
      { target: { value: "设备" } },
    );
    keys = [keyOf(5, "工作"), keyOf(9, "后台新增")];
    await user.click(screen.getByRole("tab", { name: "Key 管理" }));
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    await user.click(screen.getByTestId("we2ai-studio-key-select"));
    expect(
      await within(await screen.findByRole("listbox")).findByText("后台新增"),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(
      screen.getByTestId("we2ai-studio-video-field-productName"),
    ).toHaveValue("设备");
  });

  it("外壳重渲染不重置工坊 Key 与视频模型", async () => {
    const calls = mockShellCommands();
    const user = userEvent.setup();
    renderShell();
    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$13.48"));
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    await user.click(await screen.findByTestId("we2ai-studio-key-select"));
    await user.click(
      within(await screen.findByRole("listbox")).getByText("备用"),
    );
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "备用",
      ),
    );
    const model = await screen.findByTestId("we2ai-studio-video-model");
    await user.selectOptions(model, "grok-second-video");
    const keysBefore = calls.filter(
      (c) => c.command === "we2ai_list_keys",
    ).length;
    const mediaBefore = calls.filter(
      (c) => c.command === "we2ai_playground_media_models",
    ).length;
    await act(async () => window.dispatchEvent(new Event("focus")));
    await waitFor(() => expect(chip).toHaveTextContent("$14.48"));
    expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
      "备用",
    );
    expect(screen.getByTestId("we2ai-studio-video-model")).toHaveValue(
      "grok-second-video",
    );
    expect(calls.filter((c) => c.command === "we2ai_list_keys").length).toBe(
      keysBefore,
    );
    expect(
      calls.filter((c) => c.command === "we2ai_playground_media_models").length,
    ).toBe(mediaBefore);
  });

  it("列表刷新读到旧磁盘记忆时保留仍有效的本地 Key 选择", async () => {
    // 竞态（Codex 验收修复项 1）：handleSelectKey 先改本地、再异步把选择写盘；
    // 写盘完成前的一次列表刷新（onKeysChanged 驱动）读到的还是旧记忆
    // （studioSelectedKeyId: 5），不得把用户刚选的 Key 6 弹回 5 并清空会话。
    let keyDeleted = false;
    // 挂载后、第一次刷新 emit 前置 true：之后的响应把 Key 6 改名「备用B」。
    let refreshArmed = false;
    mockShellCommands({
      we2ai_list_keys: () =>
        ({
          keys: keyDeleted
            ? [
                {
                  id: 5,
                  name: "工作",
                  groupName: null,
                  status: "active",
                  maskedKey: "sk-we2…5555",
                },
              ]
            : [
                {
                  id: 5,
                  name: "工作",
                  groupName: null,
                  status: "active",
                  maskedKey: "sk-we2…5555",
                },
                {
                  // 第一阶段把名称改为「备用B」：名称出现在触发器上即证明这次
                  // 响应已被前端处理完（响应提交屏障，Codex 第三轮复审修复项），
                  // 同时触发器仍是 Key 6 即证明未被旧记忆弹回。
                  id: 6,
                  name: refreshArmed ? "备用B" : "备用",
                  groupName: null,
                  status: "active",
                  maskedKey: "sk-we2…6666",
                },
              ],
          selectedKeyId: 5,
          rememberedKeyId: null,
          // 记忆始终停留在 5：模拟 Key 6 的磁盘写入尚未完成（或保存失败）。
          studioSelectedKeyId: 5,
        }) as JsonBodyType,
    });
    const user = userEvent.setup();

    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(await screen.findByRole("tab", { name: "AI 营销工坊" }));
    await screen.findByTestId("we2ai-studio-video-model");

    await user.click(await screen.findByTestId("we2ai-studio-key-select"));
    const keyListbox = await screen.findByRole("listbox");
    await user.click(await within(keyListbox).findByText("备用"));
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "备用",
      ),
    );

    // 触发一次列表刷新：Rust 返回的工坊记忆仍是 5（旧值）。
    refreshArmed = true;
    await act(async () => {
      emitTauriEvent("we2ai-keys-changed", {});
    });
    // 响应提交屏障：等响应驱动的名称变化渲染出来（若实现错误，响应会把选择
    // 弹回 Key 5，触发器只会显示「工作」，这里将超时失败）。
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "备用B",
      ),
    );

    // Key 6 真的被删除：本地选择失效，此时才回退到服务端记忆 5。
    keyDeleted = true;
    await act(async () => {
      emitTauriEvent("we2ai-keys-changed", {});
    });
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "工作",
      ),
    );
  });
});
