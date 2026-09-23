import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi, afterEach } from "vitest";
import { http, HttpResponse } from "msw";
import { server } from "../msw/server";
import { ThemeProvider } from "@/components/theme-provider";
import { UpdateProvider } from "@/contexts/UpdateContext";
import App from "@/App";
import { WE2AI_MODE } from "@/config/we2ai";
import { isWe2aiIpcAllowed } from "@/we2ai/ipcWhitelist";

const TAURI_ENDPOINT = "http://tauri.local";

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: async () => "4.20.4",
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: async () => null,
}));

// jsdom 未实现 matchMedia；ThemeProvider 挂载时无条件调用它来监听系统主题变化。
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

describe("We2aiShell IPC whitelist", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  it("only invokes IPC commands that are on the WE2AI whitelist", async () => {
    // 本仓库始终以 WE2AI_MODE=true 构建（见 src/config/we2ai.ts），这里断言
    // 该假设成立——一旦这个常量被改回运行时可切换，本用例需要重新设计。
    expect(WE2AI_MODE).toBe(true);

    const invokedCommands: string[] = [];
    // We2aiShell 只会用到这几个命令；用通配符兜底记录并返回通用成功响应，
    // 任何超出预期的命令都会被下面的白名单断言捕获。
    server.use(
      http.post(`${TAURI_ENDPOINT}/*`, ({ request }) => {
        const command = request.url.slice(`${TAURI_ENDPOINT}/`.length);
        invokedCommands.push(command);
        if (command === "we2ai_get_settings") {
          return HttpResponse.json({
            language: "zh",
            silentStartup: false,
            useAppWindowControls: false,
            showInTray: true,
            minimizeToTrayOnClose: false,
          });
        }
        if (command === "get_auto_launch_status") {
          return HttpResponse.json(false);
        }
        return HttpResponse.json(null);
      }),
    );

    render(
      <ThemeProvider defaultTheme="system" storageKey="we2ai-test-theme">
        <UpdateProvider>
          <App />
        </UpdateProvider>
      </ThemeProvider>,
    );

    // We2aiShell 品牌顶栏文案，确认 WE2AI_MODE 分支真的渲染了 We2aiShell，
    // 而不是上游的供应商管理界面。
    expect(await screen.findByText("WE2AI")).toBeInTheDocument();

    await waitFor(() => {
      expect(invokedCommands).toContain("we2ai_get_settings");
    });

    for (const command of invokedCommands) {
      expect(
        isWe2aiIpcAllowed(command),
        `command "${command}" invoked by We2aiShell is not in the WE2AI IPC whitelist`,
      ).toBe(true);
    }
  });
});
