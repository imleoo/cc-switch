import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { UpdateProvider, useUpdate } from "@/contexts/UpdateContext";
import { We2aiShell } from "@/we2ai/We2aiShell";
import { getWe2aiStrings } from "@/we2ai/strings";

const t = getWe2aiStrings("zh");

const TAURI_ENDPOINT = "http://tauri.local";

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: async () => "4.20.4",
}));

// 与 We2aiShellOffline.test.tsx 不同，这里需要按用例控制 `check()` 是
// 成功还是失败（模拟 fork 仓库首次发版前 latest.json 404 的场景），所以
// 用 `vi.fn()` 而不是固定实现，测试内按需 `mockRejectedValue`/`mockResolvedValue`。
const checkMock = vi.fn();
vi.mock("@tauri-apps/plugin-updater", () => ({
  check: (...args: unknown[]) => checkMock(...args),
}));

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
        {/* sonner 的 toast 需要一个挂载的 <Toaster />（真实应用挂在
            main.tsx），这里补上才能断言 toast 文案。 */}
        <Toaster />
      </UpdateProvider>
    </ThemeProvider>,
  );
}

function mockLoggedInShell(extra: Record<string, (body: any) => JsonBodyType> = {}) {
  server.use(
    http.post(`${TAURI_ENDPOINT}/*`, async ({ request }) => {
      const command = request.url.slice(`${TAURI_ENDPOINT}/`.length);
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
      if (command === "we2ai_get_last_region") {
        return HttpResponse.json("international");
      }
      if (command === "we2ai_resume_session") {
        return HttpResponse.json("restored");
      }
      if (command === "we2ai_session_status") {
        return HttpResponse.json({
          loggedIn: true,
          region: "international",
          emailMasked: "u****@we2ai.com",
          keyringDegraded: false,
          indexDegraded: false,
          offlineRetryInSeconds: null,
          localCleanupPending: false,
        });
      }
      if (command === "we2ai_list_keys") {
        return HttpResponse.json({ keys: [], selectedKeyId: null });
      }
      const handler = extra[command];
      if (handler) {
        const body = await request.text();
        return HttpResponse.json(handler(body ? JSON.parse(body) : {}));
      }
      return HttpResponse.json(null);
    }),
  );
}

/** 已登录界面加载完成后切到设置页，找到"检查更新"按钮所在区域。 */
async function goToUpdateSection() {
  await screen.findByRole("tab", { name: t.navMarketplace });
  await userEvent.click(screen.getByRole("tab", { name: t.navSettings }));
  return screen.findByRole("button", { name: t.checkForUpdates });
}

/**
 * 只用来在"已经找到更新"（`hasUpdate=true`，按钮变成"安装并重启"、不再是
 * "检查更新"）之后，仍然能再触发一次 `checkUpdate()`——`We2aiShell` 此时
 * 不渲染检查按钮，这个组件直接调用共享的 `UpdateContext`，复现"先查到
 * 更新、后一次检查失败"这个只能通过重复调用触发的场景（Opus 复核 P9）。
 */
function ManualRecheckTrigger() {
  const update = useUpdate();
  return (
    <button onClick={() => void update.checkUpdate().catch(() => undefined)}>
      manual-recheck
    </button>
  );
}

function renderShellWithManualRecheck() {
  return render(
    <ThemeProvider defaultTheme="system" storageKey="we2ai-test-theme">
      <UpdateProvider>
        <We2aiShell />
        <ManualRecheckTrigger />
        <Toaster />
      </UpdateProvider>
    </ThemeProvider>,
  );
}

describe("We2aiShell update check", () => {
  afterEach(() => {
    server.resetHandlers();
    checkMock.mockReset();
  });

  it("shows a friendly hint instead of the raw error, and logs the raw error to the console", async () => {
    const rawError = new Error(
      "Could not fetch a valid release JSON from the remote",
    );
    checkMock.mockRejectedValue(rawError);
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    mockLoggedInShell();
    renderShell();

    const button = await goToUpdateSection();
    await userEvent.click(button);

    expect(
      await screen.findByText(t.checkFailedHint),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Could not fetch a valid release JSON/),
    ).not.toBeInTheDocument();
    expect(consoleError).toHaveBeenCalledWith(
      expect.stringContaining("check for updates"),
      rawError,
    );
    consoleError.mockRestore();
  });

  it('does not show "up to date" after a failed check; shows a failed status instead', async () => {
    checkMock.mockRejectedValue(new Error("network error"));
    mockLoggedInShell();
    renderShell();

    const button = await goToUpdateSection();
    await userEvent.click(button);

    await screen.findByText(t.checkFailedHint);
    expect(screen.queryByText(t.upToDate)).not.toBeInTheDocument();
    expect(
      screen.getByTestId("we2ai-update-check-failed"),
    ).toHaveTextContent(t.checkFailed);
  });

  it('shows "up to date" when the check succeeds and finds nothing', async () => {
    checkMock.mockResolvedValue(null);
    mockLoggedInShell();
    renderShell();

    const button = await goToUpdateSection();
    await userEvent.click(button);

    await waitFor(() => {
      expect(screen.getByText(t.upToDate)).toBeInTheDocument();
    });
  });

  it("shows the failed status, not the install button or \"up to date\", when a later check fails after an earlier one found an update", async () => {
    checkMock
      .mockResolvedValueOnce({ version: "5.0.0", notes: "", date: "" })
      .mockRejectedValueOnce(new Error("network error"));
    mockLoggedInShell();
    renderShellWithManualRecheck();

    await screen.findByRole("tab", { name: t.navMarketplace });
    await userEvent.click(screen.getByRole("tab", { name: t.navSettings }));

    // 启动后 1 秒的自动检查先找到一个更新版本：按钮变成"安装并重启"。
    await screen.findByRole("button", {
      name: new RegExp(t.installAndRestart),
    });

    // `hasUpdate=true` 时 We2aiShell 不再渲染"检查更新"按钮，用独立的
    // trigger 组件复用同一个 UpdateContext 再触发一次检查，这次失败。
    await userEvent.click(screen.getByRole("button", { name: "manual-recheck" }));
    await waitFor(() => expect(checkMock).toHaveBeenCalledTimes(2));

    expect(
      await screen.findByTestId("we2ai-update-check-failed"),
    ).toHaveTextContent(t.checkFailed);
    expect(
      screen.queryByRole("button", { name: new RegExp(t.installAndRestart) }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(t.upToDate)).not.toBeInTheDocument();
  });

  it("does not show a toast when the automatic startup check fails", async () => {
    checkMock.mockRejectedValue(new Error("network error"));
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    mockLoggedInShell();
    renderShell();

    await screen.findByRole("tab", { name: t.navMarketplace });
    // UpdateContext 的自动检查延迟 1 秒后触发；真实等过这段时间以及一次
    // 检查往返，确认没有弹出任何 toast。
    await new Promise((resolve) => setTimeout(resolve, 1500));
    await waitFor(() => expect(checkMock).toHaveBeenCalled());

    expect(screen.queryByText(t.checkFailedHint)).not.toBeInTheDocument();
    expect(screen.queryByText(t.checkFailed)).not.toBeInTheDocument();
    consoleError.mockRestore();
  });
});
