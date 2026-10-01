import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi, afterEach } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { UpdateProvider } from "@/contexts/UpdateContext";
import { We2aiShell } from "@/we2ai/We2aiShell";

// 全程用真实定时器：We2aiShell 的离线轮询用的是真实 `window.setInterval`，
// 而 MSW 在这套测试基础设施里通过真实 `fetch`（`tests/msw/tauriMocks.ts`）
// 转发 invoke() 调用——`vi.useFakeTimers()` 和真实网络往返的内部定时任务
// 混用容易互相卡住（常见的 fake-timer + fetch 死锁陷阱），不值得为了让
// 1 秒轮询测得更快而引入这层不稳定性。倒计时相关断言改为真实等待略多于
// 1 秒。

const TAURI_ENDPOINT = "http://tauri.local";

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: async () => "4.20.4",
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: async () => null,
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
        {/* sonner 的 toast 需要一个挂载的 <Toaster /> 才会真正渲染进 DOM
            （真实应用里挂在 main.tsx，We2aiShell 本身不含）——logout 相关的
            用例要断言 toast 文案，这里补上。 */}
        <Toaster />
      </UpdateProvider>
    </ThemeProvider>,
  );
}

function mockShellCommands(extra: Record<string, (body: any) => JsonBodyType>) {
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
      const handler = extra[command];
      // 余额与网关地址：已登录壳会在登录后请求，给默认响应避免无关的报错日志。
      if (!handler && command === "we2ai_get_balance") {
        return HttpResponse.json({
          balance: 12.48,
          frozenBalance: 0,
          totalRecharged: 0,
        });
      }
      if (!handler && command === "we2ai_gateway_info") {
        return HttpResponse.json({ baseUrl: "https://api.we2ai.com" });
      }
      if (!handler && command === "we2ai_list_keys") {
        // 已登录视图会挂载模型广场；本文件只测会话横幅，给一个空 Key 列表。
        return HttpResponse.json({ keys: [], selectedKeyId: null });
      }
      if (handler) {
        const body = await request.text();
        return HttpResponse.json(handler(body ? JSON.parse(body) : {}));
      }
      return HttpResponse.json(null);
    }),
  );
}

describe("We2aiShell offline retry banner", () => {
  afterEach(() => {
    server.resetHandlers();
    // 部分用例会覆盖 `document.hidden` 来模拟切到后台标签页，测试结束后
    // 还原，避免影响其他用例（这里覆盖的是实例自身属性，会遮蔽 jsdom
    // Document 原型上的 getter）。
    Object.defineProperty(document, "hidden", {
      value: false,
      configurable: true,
    });
  });

  it("shows the offline banner with a countdown when resume reports offlineRetained, and the countdown ticks down via polling", async () => {
    let sessionStatusCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "offlineRetained",
      we2ai_session_status: () => {
        sessionStatusCalls += 1;
        return {
          loggedIn: true,
          userId: 42,
          region: "international",
          emailMasked: "u****@we2ai.com",
          keyringDegraded: false,
          // 每次轮询倒计时递减，模拟 Rust 侧真实的剩余秒数计算；封底到 0。
          offlineRetryInSeconds: Math.max(0, 3 - sessionStatusCalls),
        };
      },
    });

    renderShell();

    await waitFor(() => {
      expect(screen.getByText(/网络连接不可用/)).toBeInTheDocument();
    });
    expect(screen.getByText(/2 秒后自动重试/)).toBeInTheDocument();

    // We2aiShell 每秒轮询一次 session_status；真实等过一次轮询周期，断言
    // 倒计时确实跟着服务端返回值前进（不是纯前端本地递减的假倒计时）。
    await waitFor(
      () => {
        expect(screen.getByText(/1 秒后自动重试/)).toBeInTheDocument();
      },
      { timeout: 3000 },
    );
  });

  it("clicking the retry button calls we2ai_retry_now and clears the banner on success", async () => {
    let recovered = false;
    mockShellCommands({
      we2ai_resume_session: () => "offlineRetained",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        offlineRetryInSeconds: recovered ? null : 30,
      }),
      we2ai_retry_now: () => {
        recovered = true;
        return "restored";
      },
    });

    const user = userEvent.setup();
    renderShell();

    await waitFor(() => {
      expect(screen.getByText(/网络连接不可用/)).toBeInTheDocument();
    });

    await user.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => {
      expect(screen.queryByText(/网络连接不可用/)).not.toBeInTheDocument();
    });
  });

  // Codex 代码评审第 3 轮低危项 2：标签页切到后台时暂停每秒轮询，避免看
  // 不见界面时仍持续发起 IPC 调用；切回前台立即补一次并恢复轮询。
  it("pauses polling while the document is hidden and resumes once it becomes visible again", async () => {
    let sessionStatusCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "offlineRetained",
      we2ai_session_status: () => {
        sessionStatusCalls += 1;
        return {
          loggedIn: true,
          userId: 42,
          region: "international",
          emailMasked: "u****@we2ai.com",
          keyringDegraded: false,
          offlineRetryInSeconds: 30,
        };
      },
    });

    renderShell();

    await waitFor(() => {
      expect(screen.getByText(/网络连接不可用/)).toBeInTheDocument();
    });

    // 先确认页面可见时轮询确实在跑。
    const callsWhileVisible = sessionStatusCalls;
    await waitFor(
      () => expect(sessionStatusCalls).toBeGreaterThan(callsWhileVisible),
      { timeout: 3000 },
    );

    // 切到后台：`document.hidden` 置 true 并派发 visibilitychange。
    Object.defineProperty(document, "hidden", {
      value: true,
      configurable: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));

    const callsWhileHidden = sessionStatusCalls;
    // 真实等过至少一个轮询周期，确认隐藏期间没有继续发起 IPC 调用。
    await new Promise((resolve) => setTimeout(resolve, 1500));
    expect(sessionStatusCalls).toBe(callsWhileHidden);

    // 切回前台：应当立即补一次查询，而不是等到下一个整秒。
    Object.defineProperty(document, "hidden", {
      value: false,
      configurable: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));

    await waitFor(() => {
      expect(sessionStatusCalls).toBeGreaterThan(callsWhileHidden);
    });
  });

  // Codex 代码评审第 3 轮低危项 2：轮询期间 `we2ai_session_status` 偶发
  // 失败（IPC 抖动），必须保留当前已登录/离线重试中的状态，不能把用户
  // 误踢回登录页。
  it("keeps the current offline state when a poll tick fails instead of falling back to logged out", async () => {
    let sessionStatusCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "offlineRetained",
    });
    server.use(
      http.post(`${TAURI_ENDPOINT}/we2ai_session_status`, () => {
        sessionStatusCalls += 1;
        if (sessionStatusCalls === 1) {
          // 挂载时 `refreshSessionStatus` 的那一次，正常返回，让离线横幅
          // 先展示出来。
          return HttpResponse.json({
            loggedIn: true,
            userId: 42,
            region: "international",
            emailMasked: "u****@we2ai.com",
            keyringDegraded: false,
            offlineRetryInSeconds: 30,
          });
        }
        // 第一次轮询 tick 起模拟一次 IPC 失败。
        return new HttpResponse(null, { status: 500 });
      }),
    );

    renderShell();

    await waitFor(() => {
      expect(screen.getByText(/网络连接不可用/)).toBeInTheDocument();
    });

    // 真实等过至少一次轮询失败的周期。
    await waitFor(() => expect(sessionStatusCalls).toBeGreaterThan(1), {
      timeout: 3000,
    });

    // 轮询失败不应该把界面切回登录页/未登录状态——离线横幅必须还在。
    expect(screen.getByText(/网络连接不可用/)).toBeInTheDocument();
    expect(screen.queryByText("登录 WE2AI")).not.toBeInTheDocument();
  });

  it("does not show the offline banner when resume reports needLogin", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "needLogin",
    });

    renderShell();

    await waitFor(() => {
      expect(screen.getByText("登录 WE2AI")).toBeInTheDocument();
    });
    expect(screen.queryByText(/网络连接不可用/)).not.toBeInTheDocument();
  });
});

// Codex 代码评审第 3 轮高危项 2：会话索引写入失败时（`indexDegraded`），
// 前端要展示"登录状态无法保存"提示，且与钥匙串退化的提示分开、不互相
// 混淆。
describe("We2aiShell session-not-persistable warning", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  it("shows the session-not-persistable warning when indexDegraded is true", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: true,
        offlineRetryInSeconds: null,
      }),
    });

    renderShell();

    await waitFor(() => {
      expect(
        screen.getByText("登录状态无法保存，下次启动需要重新登录"),
      ).toBeInTheDocument();
    });
    // 钥匙串退化的提示不应该同时出现——两者是不同原因，文案不应该混淆。
    expect(screen.queryByText(/系统钥匙串不可用/)).not.toBeInTheDocument();
  });

  it("prefers the keyring-degraded warning when both flags are set", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: true,
        indexDegraded: true,
        offlineRetryInSeconds: null,
      }),
    });

    renderShell();

    await waitFor(() => {
      expect(screen.getByText(/系统钥匙串不可用/)).toBeInTheDocument();
    });
    expect(
      screen.queryByText("登录状态无法保存，下次启动需要重新登录"),
    ).not.toBeInTheDocument();
  });
});

// Codex 代码评审第 5 轮高危项 3：本地清理（索引 + 钥匙串）两项全部失败时，
// `we2ai_logout` 返回 `localCleanupFailed`——前端不能表现成"已经退出"，
// 必须保持已登录界面并提示清理失败、提供重试。
describe("We2aiShell logout restoring official config", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  function loggedInUntilLogout(state: { loggedOut: boolean }) {
    return () => ({
      loggedIn: !state.loggedOut,
      region: state.loggedOut ? null : "international",
      emailMasked: state.loggedOut ? null : "u****@we2ai.com",
      keyringDegraded: false,
      indexDegraded: false,
      offlineRetryInSeconds: null,
      localCleanupPending: false,
    });
  }

  it("restores official config for all three tools only when the checkbox is ticked", async () => {
    const state = { loggedOut: false };
    const restoreCalls: unknown[] = [];
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: loggedInUntilLogout(state),
      we2ai_logout: () => {
        state.loggedOut = true;
        return "revoked";
      },
      we2ai_restore_official: (body: any) => {
        restoreCalls.push(body?.tools);
        return {
          restored: ["/u/.claude/settings.json", "/u/.codex/config.toml"],
          skipped: [],
        };
      },
    });
    const user = userEvent.setup();
    renderShell();

    await user.click(await screen.findByText("登出"));
    await user.click(
      await screen.findByRole("checkbox", { name: /同时恢复工具的官方配置/ }),
    );
    await user.click(screen.getByRole("button", { name: "确认登出" }));

    await screen.findByText("已恢复 2 个工具的官方配置");
    expect(restoreCalls).toEqual([["claude_code", "codex", "workbuddy"]]);
  });

  // Opus 复核高危项 1：只指定过一个工具时，其余两个工具的"未指向
  // WE2AI，无需恢复"落在 unchanged 而不是 skipped——不能弹出警告 toast，
  // 那不是失败，只是正常情况（验收清单 2.11 的前提：警告只列真失败）。
  it("shows no warning toast when only one of the three tools was ever applied", async () => {
    const state = { loggedOut: false };
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: loggedInUntilLogout(state),
      we2ai_logout: () => {
        state.loggedOut = true;
        return "revoked";
      },
      we2ai_restore_official: () => ({
        restored: ["/u/.claude/settings.json"],
        unchanged: [
          "Codex：未指向 WE2AI 或正被其他程序代理接管，无需恢复",
          "WorkBuddy：未指向 WE2AI，无需恢复",
        ],
        skipped: [],
      }),
    });
    const user = userEvent.setup();
    renderShell();

    await user.click(await screen.findByText("登出"));
    await user.click(
      await screen.findByRole("checkbox", { name: /同时恢复工具的官方配置/ }),
    );
    await user.click(screen.getByRole("button", { name: "确认登出" }));

    await screen.findByText("已恢复 1 个工具的官方配置");
    expect(
      screen.queryByText("部分工具的官方配置未恢复"),
    ).not.toBeInTheDocument();
  });

  it("shows a neutral toast (not a warning) when nothing was ever pointed at WE2AI", async () => {
    const state = { loggedOut: false };
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: loggedInUntilLogout(state),
      we2ai_logout: () => {
        state.loggedOut = true;
        return "revoked";
      },
      we2ai_restore_official: () => ({
        restored: [],
        unchanged: [
          "Claude Code：未指向 WE2AI 或正被其他程序代理接管，无需恢复",
          "Codex：未指向 WE2AI 或正被其他程序代理接管，无需恢复",
          "WorkBuddy：未指向 WE2AI，无需恢复",
        ],
        skipped: [],
      }),
    });
    const user = userEvent.setup();
    renderShell();

    await user.click(await screen.findByText("登出"));
    await user.click(
      await screen.findByRole("checkbox", { name: /同时恢复工具的官方配置/ }),
    );
    await user.click(screen.getByRole("button", { name: "确认登出" }));

    await screen.findByText("工具未指向 WE2AI，无需恢复");
    expect(
      screen.queryByText("部分工具的官方配置未恢复"),
    ).not.toBeInTheDocument();
  });

  it("still restores official config when local cleanup failed after logout", async () => {
    let restoreCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_logout: () => "localCleanupFailed",
      we2ai_restore_official: () => {
        restoreCalls += 1;
        return { restored: ["/u/.claude/settings.json"], skipped: [] };
      },
    });
    const user = userEvent.setup();
    renderShell();

    await user.click(await screen.findByText("登出"));
    await user.click(
      await screen.findByRole("checkbox", { name: /同时恢复工具的官方配置/ }),
    );
    await user.click(screen.getByRole("button", { name: "确认登出" }));
    await screen.findByText("已恢复 1 个工具的官方配置");
    expect(restoreCalls).toBe(1);
  });

  it("leaves tool configs alone by default", async () => {
    const state = { loggedOut: false };
    let restoreCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: loggedInUntilLogout(state),
      we2ai_logout: () => {
        state.loggedOut = true;
        return "revoked";
      },
      we2ai_restore_official: () => {
        restoreCalls += 1;
        return { restored: [], skipped: [] };
      },
    });
    const user = userEvent.setup();
    renderShell();

    await user.click(await screen.findByText("登出"));
    await user.click(screen.getByRole("button", { name: "确认登出" }));
    await screen.findByText("登录 WE2AI");
    expect(restoreCalls).toBe(0);
  });
});

describe("We2aiShell logout with local cleanup failure", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  it("keeps the logged-in UI and shows a retry toast when logout reports localCleanupFailed", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
      }),
      we2ai_logout: () => "localCleanupFailed",
    });

    const user = userEvent.setup();
    renderShell();

    await waitFor(() => {
      expect(screen.getByText("登出")).toBeInTheDocument();
    });

    await user.click(screen.getByText("登出"));
    await user.click(await screen.findByRole("button", { name: "确认登出" }));

    // 不能表现成已经退出：账号信息与登出按钮必须还在。
    await waitFor(() => {
      expect(
        screen.getByText("退出未完成：无法清除本机保存的登录凭据"),
      ).toBeInTheDocument();
    });
    expect(screen.getByText("登出")).toBeInTheDocument();
    expect(screen.queryByText("登录 WE2AI")).not.toBeInTheDocument();
  });

  // Codex 代码评审第 6 轮高危项 3：重试必须调用 `we2ai_retry_local_cleanup`，
  // 不能再调 `we2ai_logout`（后端此时处于 LogoutCleanupPending，再登出只会
  // 得到 notLoggedIn，什么也清理不了）。重试成功后退出已登录界面。
  it("retries via we2ai_retry_local_cleanup instead of logging out again", async () => {
    let logoutCalls = 0;
    let retryCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      // 重试成功后前端重新读取真实状态：此时已无会话、无待清理残留。
      we2ai_session_status: () => ({
        loggedIn: retryCalls === 0,
        region: retryCalls === 0 ? "international" : null,
        emailMasked: retryCalls === 0 ? "u****@we2ai.com" : null,
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_logout: () => {
        logoutCalls += 1;
        return "localCleanupFailed";
      },
      we2ai_retry_local_cleanup: () => {
        retryCalls += 1;
        return "localOnly";
      },
    });

    const user = userEvent.setup();
    renderShell();

    await waitFor(() => {
      expect(screen.getByText("登出")).toBeInTheDocument();
    });
    await user.click(screen.getByText("登出"));
    await user.click(await screen.findByRole("button", { name: "确认登出" }));
    await screen.findByText("退出未完成：无法清除本机保存的登录凭据");

    await user.click(await screen.findByRole("button", { name: "重试" }));

    await waitFor(() => {
      expect(retryCalls).toBe(1);
    });
    expect(logoutCalls).toBe(1);
    await waitFor(() => {
      expect(screen.queryByText("登出")).not.toBeInTheDocument();
    });
  });
  // Codex 验收第 5 轮高危项 2：会话终止或登出后本地凭据都没清掉时，即使处于
  // 未登录界面也要提示并提供重试（调用 we2ai_retry_local_cleanup）。
  it("shows a cleanup-pending banner on the login view and retries local cleanup", async () => {
    let retryCalls = 0;
    let pending = true;
    mockShellCommands({
      we2ai_resume_session: () => "needLogin",
      we2ai_session_status: () => ({
        loggedIn: false,
        region: null,
        emailMasked: null,
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: pending,
      }),
      we2ai_retry_local_cleanup: () => {
        retryCalls += 1;
        pending = false;
        return "localOnly";
      },
    });

    const user = userEvent.setup();
    renderShell();

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("本机保存的登录凭据未能完全清除，请重试");
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => {
      expect(retryCalls).toBe(1);
    });
    await waitFor(() => {
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    });
  });
  // 验收第 6 轮高危项 2：待清理残留独立于当前会话——登录着另一区域时也要提示。
  it("shows the cleanup-pending banner while logged into another region", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "domestic_prod",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: true,
      }),
    });

    renderShell();

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("本机保存的登录凭据未能完全清除，请重试");
    expect(screen.getByText("登出")).toBeInTheDocument();
  });
  // 验收第 7 轮高危项 2：A 待清理、登录着 B 时正常登出 B，登录页仍要显示
  // A 的待清理提示——前端按真实摘要渲染，而不是写入固定的"未登录"摘要。
  it("keeps the cleanup-pending banner after logging out of another region", async () => {
    let loggedOut = false;
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: !loggedOut,
        region: loggedOut ? null : "domestic_prod",
        emailMasked: loggedOut ? null : "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: true,
      }),
      we2ai_logout: () => {
        loggedOut = true;
        return "revoked";
      },
    });

    const user = userEvent.setup();
    renderShell();

    await waitFor(() => {
      expect(screen.getByText("登出")).toBeInTheDocument();
    });
    await user.click(screen.getByText("登出"));
    await user.click(await screen.findByRole("button", { name: "确认登出" }));

    await waitFor(() => {
      expect(screen.queryByText("登出")).not.toBeInTheDocument();
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "本机保存的登录凭据未能完全清除，请重试",
    );
  });
});

// Opus 复核低危项 L5：`ModelSquarePage` 的 `onBeforeApplyDialogOpen` 必须真的
// 接到 We2aiShell 的 `refreshToolStatus`，而不是只在 `ModelSquarePage`
// 自身的单元测试里验证过回调"被调用"这件事——这里从 We2aiShell 整体渲染，
// 断言点击工具按钮打开确认弹窗时，`we2ai_tool_status` 确实被再次调用。
describe("We2aiShell apply dialog refreshes tool status", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  it("wires ModelSquarePage's onBeforeApplyDialogOpen to refreshToolStatus", async () => {
    let toolStatusCalls = 0;
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "工作",
            groupName: "Claude 组",
            status: "active",
            maskedKey: "sk-we2…1111",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
          },
        ],
        callable: true,
        blockedReason: null,
      }),
      we2ai_apply_plan: () => ({
        files: ["/home/u/.claude/settings.json"],
        fields: ["model"],
        extraChanges: [],
      }),
      we2ai_tool_status: () => {
        toolStatusCalls += 1;
        return {
          tools: [
            {
              tool: "claude_code",
              installed: true,
              broken: false,
              version: "2.1.0",
              downloadUrl: "https://docs.anthropic.com/en/docs/claude-code/setup",
              managedModel: null,
            },
            {
              tool: "codex",
              installed: false,
              broken: false,
              version: null,
              downloadUrl: "https://github.com/openai/codex/releases",
              managedModel: null,
            },
            {
              tool: "workbuddy",
              installed: false,
              broken: false,
              version: null,
              downloadUrl: "https://www.workbuddy.ai/downloads",
              managedModel: null,
            },
          ],
          ccSwitchRunning: false,
        };
      },
    });

    const user = userEvent.setup();
    renderShell();

    const card = await screen.findByTestId("we2ai-model-card");
    // 登录后已经会拉取一次工具状态（`loggedInIdentity` 变化触发）。
    await waitFor(() => expect(toolStatusCalls).toBeGreaterThanOrEqual(1));
    const callsBeforeOpen = toolStatusCalls;

    await user.click(
      within(card).getByRole("button", { name: /^Claude Code/ }),
    );
    await screen.findByRole("dialog");

    await waitFor(() =>
      expect(toolStatusCalls).toBeGreaterThan(callsBeforeOpen),
    );
  });

  // Codex 验收 Z1：快速检测结果此前直接塞回 `toolStatus`（`prev ? {...} :
  // prev`），首次登录后完整检测仍在等 npm（`toolStatus` 还是 `null`）时，
  // 快速检测查到 CC Switch 正在运行也会被这个分支悄悄丢弃——弹窗既无
  // 并存警告也无"未能完成检测"提示。快速检测结果现在存在独立 state，不
  // 依赖 `toolStatus` 是否已经加载完成。
  it("shows the running banner from the quick check even while the first full tool-status request is still pending", async () => {
    const baseHandlers = {
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "工作",
            groupName: "Claude 组",
            status: "active",
            maskedKey: "sk-we2…1111",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
          },
        ],
        callable: true,
        blockedReason: null,
      }),
      we2ai_apply_plan: () => ({
        files: ["/home/u/.claude/settings.json"],
        fields: ["model"],
        extraChanges: [],
      }),
    };
    mockShellCommands(baseHandlers);
    // 覆盖 `we2ai_tool_status`：模拟"完整检测仍在等 npm 返回"，这次请求
    // 永远不 resolve；覆盖 `we2ai_cc_switch_running_quick`：立即查到
    // "running"。两个 `server.use()` 叠加在 `mockShellCommands` 之上，
    // 对这两个命令的匹配优先级更高。
    server.use(
      http.post(`${TAURI_ENDPOINT}/we2ai_tool_status`, () => new Promise(() => {})),
      http.post(`${TAURI_ENDPOINT}/we2ai_cc_switch_running_quick`, () =>
        HttpResponse.json("running"),
      ),
    );

    const user = userEvent.setup();
    renderShell();

    const card = await screen.findByTestId("we2ai-model-card");
    await user.click(
      within(card).getByRole("button", { name: /^Claude Code/ }),
    );
    const dialog = await screen.findByRole("dialog");

    await waitFor(() =>
      expect(
        within(dialog).getByTestId("we2ai-apply-other-tool-running"),
      ).toBeInTheDocument(),
    );
  });

  // Codex 验收 Z1：快速请求此前没有序号保护——关闭弹窗后立即重开会再触发
  // 一次快速检测，如果前一次（更旧）的请求碰巧更晚才 resolve，会用一个
  // 过期结果覆盖掉后一次（更新）请求已经生效的结果。
  it("keeps only the latest quick check result when the dialog is closed and reopened with out-of-order responses", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "工作",
            groupName: "Claude 组",
            status: "active",
            maskedKey: "sk-we2…1111",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
          },
        ],
        callable: true,
        blockedReason: null,
      }),
      we2ai_apply_plan: () => ({
        files: ["/home/u/.claude/settings.json"],
        fields: ["model"],
        extraChanges: [],
      }),
      we2ai_tool_status: () => ({
        tools: [
          {
            tool: "claude_code",
            installed: true,
            broken: false,
            version: "2.1.0",
            downloadUrl: "https://docs.anthropic.com/en/docs/claude-code/setup",
            managedModel: null,
          },
        ],
        ccSwitchRunning: false,
      }),
    });

    let quickCheckCalls = 0;
    let resolveFirstQuickCheck: (status: string) => void = () => {};
    const firstQuickCheckPromise = new Promise<string>((resolve) => {
      resolveFirstQuickCheck = resolve;
    });
    server.use(
      http.post(`${TAURI_ENDPOINT}/we2ai_cc_switch_running_quick`, async () => {
        quickCheckCalls += 1;
        if (quickCheckCalls === 1) {
          // 第一次（旧）请求：先挂起，等测试手动放行,验证它稍后 resolve
          // 时不会覆盖掉第二次（新）请求已经生效的结果。
          const status = await firstQuickCheckPromise;
          return HttpResponse.json(status);
        }
        return HttpResponse.json("running");
      }),
    );

    const user = userEvent.setup();
    renderShell();
    const card = await screen.findByTestId("we2ai-model-card");

    // 第一次打开：触发第一次（挂起中的）快速检测，随后关闭弹窗。
    await user.click(
      within(card).getByRole("button", { name: /^Claude Code/ }),
    );
    await screen.findByRole("dialog");
    await user.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );

    // 第二次打开：触发第二次（立即返回 "running"）快速检测。
    await user.click(
      within(card).getByRole("button", { name: /^Claude Code/ }),
    );
    const dialog = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(
        within(dialog).getByTestId("we2ai-apply-other-tool-running"),
      ).toBeInTheDocument(),
    );

    // 现在才放行第一次（更旧）的请求，给一个不同的结果（"not_running"）——
    // 它必须被当作过期结果丢弃，不能覆盖掉第二次请求已经生效的 "running"。
    resolveFirstQuickCheck("not_running");
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(
      within(dialog).getByTestId("we2ai-apply-other-tool-running"),
    ).toBeInTheDocument();
  });

  // Codex 验收 W2：快速检测取得确定结果时必须优先于旧的完整报告，而不是
  // 与之 OR 合并——旧的完整报告说"在运行"（`ccSwitchRunning: true`），
  // 但这次最新的快速检测明确查到"没有在运行"，警告不应该继续显示。
  it("does not show the warning when the latest quick check says not_running even if the old full report said true", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "工作",
            groupName: "Claude 组",
            status: "active",
            maskedKey: "sk-we2…1111",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
          },
        ],
        callable: true,
        blockedReason: null,
      }),
      we2ai_apply_plan: () => ({
        files: ["/home/u/.claude/settings.json"],
        fields: ["model"],
        extraChanges: [],
      }),
      // 旧的完整报告：ccSwitchRunning: true。
      we2ai_tool_status: () => ({
        tools: [
          {
            tool: "claude_code",
            installed: true,
            broken: false,
            version: "2.1.0",
            downloadUrl: "https://docs.anthropic.com/en/docs/claude-code/setup",
            managedModel: null,
          },
        ],
        ccSwitchRunning: true,
      }),
    });
    server.use(
      http.post(`${TAURI_ENDPOINT}/we2ai_cc_switch_running_quick`, () =>
        HttpResponse.json("not_running"),
      ),
    );

    const user = userEvent.setup();
    renderShell();
    const card = await screen.findByTestId("we2ai-model-card");
    // 登录后已经会拉取一次完整报告，等它落地成 ccSwitchRunning: true。
    await waitFor(() =>
      expect(screen.queryByTestId("we2ai-model-card")).toBeInTheDocument(),
    );

    await user.click(
      within(card).getByRole("button", { name: /^Claude Code/ }),
    );
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByTestId("we2ai-apply-plan");

    // 快速检测（"not_running"）落地后，警告不应该出现——即使旧的完整
    // 报告仍然是 true。
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(
      within(dialog).queryByTestId("we2ai-apply-other-tool-running"),
    ).not.toBeInTheDocument();
  });

  // Codex 验收 V2：顶栏（`ToolStatusBar`）必须与确认弹窗展示一致的
  // "CC Switch 是否在运行"——复用同一份 `resolveCcSwitchRunning`，不能
  // 只在弹窗里修好、顶栏还是旧的 OR 合并逻辑。
  it("shows the top bar warning based on the quick check result instead of the stale full report", async () => {
    mockShellCommands({
      we2ai_resume_session: () => "restored",
      we2ai_session_status: () => ({
        loggedIn: true,
        userId: 42,
        region: "international",
        emailMasked: "u****@we2ai.com",
        keyringDegraded: false,
        indexDegraded: false,
        offlineRetryInSeconds: null,
        localCleanupPending: false,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "工作",
            groupName: "Claude 组",
            status: "active",
            maskedKey: "sk-we2…1111",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
          },
        ],
        callable: true,
        blockedReason: null,
      }),
      we2ai_apply_plan: () => ({
        files: ["/home/u/.claude/settings.json"],
        fields: ["model"],
        extraChanges: [],
      }),
      // 旧的完整报告：ccSwitchRunning: true。
      we2ai_tool_status: () => ({
        tools: [
          {
            tool: "claude_code",
            installed: true,
            broken: false,
            version: "2.1.0",
            downloadUrl: "https://docs.anthropic.com/en/docs/claude-code/setup",
            managedModel: null,
          },
        ],
        ccSwitchRunning: true,
      }),
    });
    server.use(
      http.post(`${TAURI_ENDPOINT}/we2ai_cc_switch_running_quick`, () =>
        HttpResponse.json("not_running"),
      ),
    );

    const user = userEvent.setup();
    renderShell();
    const card = await screen.findByTestId("we2ai-model-card");
    const statusBar = await screen.findByTestId("we2ai-tool-status");
    // 顶栏最初展示旧的完整报告：ccSwitchRunning: true，警告应该可见。
    await waitFor(() =>
      expect(within(statusBar).getByRole("status")).toBeInTheDocument(),
    );

    // 打开一次确认弹窗触发快速检测（快速结果是 "not_running"），随后
    // 关闭弹窗，只关注顶栏。
    await user.click(
      within(card).getByRole("button", { name: /^Claude Code/ }),
    );
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByTestId("we2ai-apply-plan");
    await user.click(within(dialog).getByRole("button", { name: "取消" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );

    // 顶栏的警告现在应该消失——即使 toolStatus.ccSwitchRunning 仍然是
    // true，最新的快速检测结果（"not_running"）必须优先。
    await waitFor(() =>
      expect(within(statusBar).queryByRole("status")).not.toBeInTheDocument(),
    );
  });
});
