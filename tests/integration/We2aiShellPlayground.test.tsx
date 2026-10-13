import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { emitTauriEvent } from "../msw/tauriMocks";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { UpdateProvider } from "@/contexts/UpdateContext";
import { isWe2aiIpcAllowed } from "@/we2ai/ipcWhitelist";
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
  afterEach(() => {
    server.resetHandlers();
    vi.restoreAllMocks();
  });

  it("orders the tabs marketplace | studio | keys | billing | settings", async () => {
    mockShellCommands();
    renderShell();
    await screen.findByTestId("balance-chip");
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual([
      "模型广场",
      "AI 营销工坊",
      "Key 管理",
      "充值",
      "设置",
    ]);
  });

  it("streams a chat round end-to-end and only invokes whitelisted commands", async () => {
    const calls = mockShellCommands();
    const user = userEvent.setup();

    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(await screen.findByRole("tab", { name: "AI 营销工坊" }));

    // 工坊挂载即预热 Key 缓存（方案 M1）：列表与模型拉取完成前发送按钮禁用。
    await screen.findByTestId("we2ai-studio-key-select");
    const modelSelect = await screen.findByTestId("we2ai-studio-model-select");
    expect(modelSelect).toHaveTextContent("gpt-marketing");

    // 点击场景预设 + 输入 + 生成。
    await user.click(await screen.findByTestId("we2ai-studio-preset-product-copy"));
    const input = screen.getByTestId("we2ai-studio-input");
    await user.type(input, "智能保温杯");
    await user.click(screen.getByTestId("we2ai-studio-send"));

    // 流式增量与 usage 渲染。
    const bubble = await screen.findByTestId("we2ai-studio-assistant-message");
    await waitFor(() => {
      expect(bubble).toHaveTextContent("第一段第二段");
    });
    expect(bubble).toHaveTextContent("16 tokens");

    // 请求体：keyId/model/system/messages 契约。
    const chat = calls.find((c) => c.command === "we2ai_playground_chat");
    expect(chat?.body.request).toMatchObject({
      keyId: 5,
      model: "gpt-marketing",
      system: expect.stringContaining("产品文案"),
      messages: [{ role: "user", content: "智能保温杯" }],
    });
    expect(typeof chat?.body.request.requestId).toBe("string");
    // 可选参数未启用时不下发（与 Web Playground 参数语义一致）。
    expect(chat?.body.request.temperature).toBeNull();
    expect(chat?.body.request.topP).toBeNull();
    expect(chat?.body.request.maxTokens).toBeNull();

    // 终态后余额刷新（方案 M7）。
    await waitFor(() => {
      expect(
        calls.filter((c) => c.command === "we2ai_get_balance").length,
      ).toBeGreaterThanOrEqual(2);
    });

    // 全部 invoke 均落在白名单内（含新命令）。
    for (const { command } of calls) {
      expect(
        isWe2aiIpcAllowed(command),
        `command "${command}" is not in the WE2AI IPC whitelist`,
      ).toBe(true);
    }
  });

  it("keeps the conversation when switching tabs (forceMount)", async () => {
    mockShellCommands();
    const user = userEvent.setup();

    renderShell();
    await screen.findByTestId("balance-chip");
    await user.click(await screen.findByRole("tab", { name: "AI 营销工坊" }));
    await screen.findByTestId("we2ai-studio-model-select");

    await user.type(screen.getByTestId("we2ai-studio-input"), "跨 Tab 保留");
    await user.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      expect(
        screen.getByTestId("we2ai-studio-assistant-message"),
      ).toHaveTextContent("第一段第二段");
    });

    // 切到模型广场再切回来：会话不丢（工坊 TabsContent forceMount）。
    await user.click(screen.getByRole("tab", { name: "模型广场" }));
    await user.click(screen.getByRole("tab", { name: "AI 营销工坊" }));
    await waitFor(() => {
      expect(
        screen.getByTestId("we2ai-studio-assistant-message"),
      ).toHaveTextContent("第一段第二段");
    });
    expect(screen.getByTestId("we2ai-studio-user-message")).toHaveTextContent(
      "跨 Tab 保留",
    );
  });

  it("外壳重渲染不重置工坊的 Key 与模型选择", async () => {
    const calls = mockShellCommands();
    const user = userEvent.setup();

    renderShell();
    const chip = await screen.findByTestId("balance-chip");
    // 初始余额 12.48 + 第一次 get_balance 调用（=1）的递增值。
    await waitFor(() => expect(chip).toHaveTextContent("$13.48"));
    await user.click(await screen.findByRole("tab", { name: "AI 营销工坊" }));

    const modelSelect = await screen.findByTestId("we2ai-studio-model-select");
    await waitFor(() => expect(modelSelect).toHaveTextContent("gpt-marketing"));

    // 用户改选第二个模型（Key 不变：切换 Key 本来就会清会话并重置模型）。
    await user.click(modelSelect);
    const listbox = await screen.findByRole("listbox");
    await user.click(await within(listbox).findByText("gpt-second"));
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-model-select")).toHaveTextContent(
        "gpt-second",
      ),
    );
    // 再改选第二个 Key（Codex 验收修复项 2：Key 选择也要锁定）。
    await user.click(await screen.findByTestId("we2ai-studio-key-select"));
    const keyListbox = await screen.findByRole("listbox");
    await user.click(await within(keyListbox).findByText("备用"));
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
        "备用",
      ),
    );
    // 切 Key 会开新会话并重置模型为列表第一个（预期行为），重新改选模型。
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-model-select")).toHaveTextContent(
        "gpt-marketing",
      ),
    );
    await user.click(screen.getByTestId("we2ai-studio-model-select"));
    const listbox2 = await screen.findByRole("listbox");
    await user.click(await within(listbox2).findByText("gpt-second"));
    await waitFor(() =>
      expect(screen.getByTestId("we2ai-studio-model-select")).toHaveTextContent(
        "gpt-second",
      ),
    );

    const listKeysBefore = calls.filter(
      (c) => c.command === "we2ai_list_keys",
    ).length;
    const modelsBefore = calls.filter(
      (c) => c.command === "we2ai_playground_models",
    ).length;

    // 窗口重新聚焦会让外壳刷新余额并重渲染（refreshBalance → setBalance）。
    // 传进工坊的回调必须是稳定引用，否则 loadKeys/loadModels 的 useCallback
    // 会换身份，effect 重跑：重拉列表、把选中模型重置回列表第一个、把选中
    // Key 弹回磁盘记忆。
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    // 等待可观察的状态提交屏障：focus 触发的余额刷新返回了新值并渲染到 chip。
    // （调用计数不构成屏障——请求被记录不代表 setBalance 已提交，Codex 验收修复项 2。）
    await waitFor(() => expect(chip).toHaveTextContent("$14.48"));

    expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
      "备用",
    );
    expect(screen.getByTestId("we2ai-studio-model-select")).toHaveTextContent(
      "gpt-second",
    );
    // 恒定重拉是根因的另一个表征：聚焦一次不该再拉一遍 Key 与模型列表。
    expect(calls.filter((c) => c.command === "we2ai_list_keys").length).toBe(
      listKeysBefore,
    );
    expect(
      calls.filter((c) => c.command === "we2ai_playground_models").length,
    ).toBe(modelsBefore);
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
    await screen.findByTestId("we2ai-studio-model-select");

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
