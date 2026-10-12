import { render, screen, waitFor } from "@testing-library/react";
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
        case "we2ai_get_balance":
          return HttpResponse.json({
            balance: 12.48,
            frozenBalance: 0,
            totalRecharged: 0,
          });
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
            ],
            selectedKeyId: 5,
            rememberedKeyId: null,
            studioSelectedKeyId: 5,
          });
        case "we2ai_playground_models":
          return HttpResponse.json({
            models: [{ id: "gpt-marketing", provider: null }],
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
          return HttpResponse.json({ studioSelectedKeyId: 5 });
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
});
