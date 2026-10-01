import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi, afterEach } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { UpdateProvider } from "@/contexts/UpdateContext";
import { we2aiApi, type We2aiBalance } from "@/we2ai/api";
import { We2aiShell } from "@/we2ai/We2aiShell";

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

function mockShellCommands(
  extra: Record<string, (body: any) => JsonBodyType> = {},
) {
  const calls: string[] = [];
  server.use(
    http.post(`${TAURI_ENDPOINT}/*`, async ({ request }) => {
      const command = request.url.slice(`${TAURI_ENDPOINT}/`.length);
      calls.push(command);
      const handler = extra[command];
      if (handler) {
        const body = await request.text();
        return HttpResponse.json(handler(body ? JSON.parse(body) : {}));
      }
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
        return HttpResponse.json(LOGGED_IN);
      }
      if (command === "we2ai_gateway_info") {
        return HttpResponse.json({ baseUrl: "https://api.we2ai.com" });
      }
      if (command === "we2ai_get_balance") {
        return HttpResponse.json({
          balance: 12.48,
          frozenBalance: 0,
          totalRecharged: 0,
        });
      }
      if (command === "we2ai_list_keys") {
        return HttpResponse.json({ keys: [], selectedKeyId: null });
      }
      return HttpResponse.json(null);
    }),
  );
  return calls;
}

describe("We2aiShell balance chip and billing tab", () => {
  afterEach(() => {
    server.resetHandlers();
    vi.restoreAllMocks();
  });

  it("shows the balance with two decimals in the top bar and fetches it after login", async () => {
    const calls = mockShellCommands({
      we2ai_get_balance: () => ({
        balance: 12.48,
        frozenBalance: 0,
        totalRecharged: 200,
      }),
    });

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$12.48"));
    expect(chip).toHaveAttribute("data-low", "false");
    expect(chip).toHaveAccessibleName("可用余额 $12.48，点击前往充值");
    expect(calls).toContain("we2ai_get_balance");
  });

  it("turns the chip orange when the balance is below $1", async () => {
    mockShellCommands({
      we2ai_get_balance: () => ({
        balance: 0.37,
        frozenBalance: 0,
        totalRecharged: 5,
      }),
    });

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$0.37"));
    expect(chip).toHaveAttribute("data-low", "true");
  });

  it("shows a dash without any error toast when the balance can't be loaded", async () => {
    const calls = mockShellCommands();
    server.use(
      http.post(`${TAURI_ENDPOINT}/we2ai_get_balance`, () => {
        calls.push("we2ai_get_balance");
        return HttpResponse.text("down", { status: 500 });
      }),
    );
    vi.spyOn(console, "debug").mockImplementation(() => {});

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    // 先确认请求确实发出并失败，再断言界面（初始状态本来也是 `—`）。
    await waitFor(() => expect(calls).toContain("we2ai_get_balance"));
    expect(chip).toHaveTextContent("—");
    expect(chip).toHaveAccessibleName("余额暂不可用，点击前往充值");
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(document.querySelector("[data-sonner-toast]")).toBeNull();
  });

  it("clicking the chip switches to the billing tab and refreshes the balance", async () => {
    let balanceCalls = 0;
    mockShellCommands({
      we2ai_get_balance: () => {
        balanceCalls += 1;
        return { balance: 12.48, frozenBalance: 1.5, totalRecharged: 200 };
      },
    });
    const user = userEvent.setup();

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$12.48"));
    const tabs = screen.getAllByRole("tab").map((tab) => tab.textContent);
    expect(tabs).toEqual(["模型广场", "充值", "设置"]);
    expect(screen.getByRole("tab", { name: "模型广场" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    const callsBefore = balanceCalls;

    await user.click(chip);

    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "充值" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    await waitFor(() => expect(balanceCalls).toBeGreaterThan(callsBefore));
    const panel = screen.getByRole("tabpanel", { name: "充值" });
    expect(within(panel).getByTestId("billing-frozen")).toHaveTextContent(
      "$1.50",
    );
    expect(
      within(panel).getByRole("button", { name: "去 WE2AI 充值 ↗" }),
    ).toBeInTheDocument();
  });

  it("refreshes the balance when the window regains focus", async () => {
    let balanceCalls = 0;
    mockShellCommands({
      we2ai_get_balance: () => {
        balanceCalls += 1;
        return {
          balance: balanceCalls === 1 ? 5 : 8,
          frozenBalance: 0,
          totalRecharged: 0,
        };
      },
    });

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$5.00"));

    window.dispatchEvent(new Event("focus"));

    await waitFor(() => expect(chip).toHaveTextContent("$8.00"));
  });

  it("the insufficient-balance banner button opens the billing tab and runs the main recharge flow (opens /purchase, starts waiting)", async () => {
    const opened: string[] = [];
    mockShellCommands({
      open_external: (body) => {
        opened.push(body.url);
        return null;
      },
      we2ai_get_balance: () => ({
        balance: 0,
        frozenBalance: 0,
        totalRecharged: 0,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "default",
            groupName: null,
            status: "active",
            maskedKey: "sk-123…cdef",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [],
        callable: false,
        blockedReason: "INSUFFICIENT_BALANCE",
        pricing: null,
      }),
    });
    const user = userEvent.setup();

    renderShell();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("账户余额不足");
    await user.click(within(alert).getByRole("button", { name: "去充值" }));

    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "充值" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    expect(await screen.findByTestId("billing-watch-waiting")).toBeVisible();
    expect(opened).toEqual(["https://api.we2ai.com/purchase"]);
    // 不会因重渲染重复触发。
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(opened).toHaveLength(1);
  });

  it("the balance chip only switches to the billing tab, without opening the browser", async () => {
    const opened: string[] = [];
    mockShellCommands({
      open_external: (body) => {
        opened.push(body.url);
        return null;
      },
    });
    const user = userEvent.setup();

    renderShell();
    await openBillingTab(user);
    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(opened).toEqual([]);
    expect(screen.queryByTestId("billing-watch-waiting")).toBeNull();
  });

  it("other blocked reasons do not show the top-up button", async () => {
    mockShellCommands({
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "default",
            groupName: null,
            status: "active",
            maskedKey: "sk-123…cdef",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [],
        callable: false,
        blockedReason: "API_KEY_EXPIRED",
        pricing: null,
      }),
    });

    renderShell();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("这个 Key 已过期");
    expect(within(alert).queryByRole("button")).toBeNull();
  });

  // ---- 修复轮新增 ----

  function bal(balance: number): We2aiBalance {
    return { balance, frozenBalance: 0, totalRecharged: 0 };
  }

  async function openBillingTab(user: ReturnType<typeof userEvent.setup>) {
    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).not.toHaveTextContent("—"));
    await user.click(chip);
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "充值" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    return chip;
  }

  it("keeps the previous balance on a network error but clears it on other errors", async () => {
    mockShellCommands();
    let mode: "ok" | "network" | "other" = "ok";
    vi.spyOn(we2aiApi, "getBalance").mockImplementation(async () => {
      if (mode === "network") {
        throw { code: "TRANSIENT", message: "offline" };
      }
      if (mode === "other") throw new Error("bad payload");
      return bal(12.48);
    });
    vi.spyOn(console, "debug").mockImplementation(() => {});

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$12.48"));

    mode = "network";
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    expect(chip).toHaveTextContent("$12.48");

    mode = "other";
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => expect(chip).toHaveTextContent("—"));
  });

  it("window focus while the billing tab is hidden and nothing is being watched fetches the balance exactly once", async () => {
    const calls = mockShellCommands({
      we2ai_get_balance: () => ({
        balance: 12.48,
        frozenBalance: 0,
        totalRecharged: 0,
      }),
    });

    renderShell();

    const chip = await screen.findByTestId("balance-chip");
    await waitFor(() => expect(chip).toHaveTextContent("$12.48"));
    const before = calls.filter((c) => c === "we2ai_get_balance").length;

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    // 给可能出现的第二个请求留出时间。
    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(calls.filter((c) => c === "we2ai_get_balance").length).toBe(
      before + 1,
    );
  });

  it("window focus while waiting for a payment fetches the balance once, not once per listener", async () => {
    const calls = mockShellCommands({
      we2ai_get_balance: () => ({
        balance: 12.48,
        frozenBalance: 0,
        totalRecharged: 0,
      }),
    });
    const user = userEvent.setup();

    renderShell();
    await openBillingTab(user);
    await user.click(screen.getByRole("button", { name: "去 WE2AI 充值 ↗" }));
    await screen.findByTestId("billing-watch-waiting");
    await new Promise((resolve) => setTimeout(resolve, 50));
    const before = calls.filter((c) => c === "we2ai_get_balance").length;

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(calls.filter((c) => c === "we2ai_get_balance").length).toBe(
      before + 1,
    );
  });

  it("resets to the marketplace tab on a session change and discards the previous session's in-flight balance response", async () => {
    let session = { ...LOGGED_IN };
    mockShellCommands({ we2ai_session_status: () => session });
    let current = 12.48;
    let holdNext = false;
    const pending: Array<(value: We2aiBalance) => void> = [];
    vi.spyOn(we2aiApi, "getBalance").mockImplementation(() => {
      if (holdNext) {
        holdNext = false;
        return new Promise<We2aiBalance>((resolve) => pending.push(resolve));
      }
      return Promise.resolve(bal(current));
    });
    let announcementsFail = false;
    vi.spyOn(we2aiApi, "listAnnouncements").mockImplementation(async () => {
      if (announcementsFail) throw { code: "TOKEN_REVOKED", message: "x" };
      return [];
    });
    let changed: (() => void) | null = null;
    vi.spyOn(we2aiApi, "onAnnouncementsChanged").mockImplementation(
      async (handler) => {
        changed = handler;
        return () => {};
      },
    );
    vi.spyOn(console, "debug").mockImplementation(() => {});
    const user = userEvent.setup();

    renderShell();
    const chip = await openBillingTab(user);
    await waitFor(() => expect(chip).toHaveTextContent("$12.48"));

    // 旧会话的一次余额请求还在途。
    holdNext = true;
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    expect(pending).toHaveLength(1);

    // 会话换成另一个账号（公告拉取遇到非网络类错误 → 外壳复查会话状态）。
    session = { ...LOGGED_IN, userId: 43 };
    current = 3;
    announcementsFail = true;
    await act(async () => {
      changed?.();
    });

    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "模型广场" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    await waitFor(() =>
      expect(screen.getByTestId("balance-chip")).toHaveTextContent("$3.00"),
    );

    // 旧会话的响应此时才回来，必须被丢弃。
    await act(async () => {
      pending[0](bal(999));
    });
    expect(screen.getByTestId("balance-chip")).toHaveTextContent("$3.00");
  });

  it("clears the insufficient-balance banner in the marketplace as soon as a payment is detected", async () => {
    let balance = 0;
    let callable = false;
    mockShellCommands({
      we2ai_get_balance: () => ({
        balance,
        frozenBalance: 0,
        totalRecharged: 0,
      }),
      we2ai_list_keys: () => ({
        keys: [
          {
            id: 1,
            name: "default",
            groupName: null,
            status: "active",
            maskedKey: "sk-123…cdef",
          },
        ],
        selectedKeyId: 1,
      }),
      we2ai_key_models: () => ({
        models: [],
        callable,
        blockedReason: callable ? null : "INSUFFICIENT_BALANCE",
        pricing: null,
      }),
    });
    const user = userEvent.setup();

    renderShell();
    expect(await screen.findByRole("alert")).toHaveTextContent("账户余额不足");

    // 去充值页开始等待，再回到模型广场（充值页保持挂载，检测继续）。
    await user.click(screen.getByRole("button", { name: "去充值" }));
    await screen.findByTestId("billing-watch-waiting");
    await user.click(screen.getByRole("tab", { name: "模型广场" }));
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "模型广场" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("账户余额不足");

    // 付款完成：服务端余额增加、Key 恢复可用；窗口回前台触发检测。
    balance = 20;
    callable = true;
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("a stale balance response after a session change does not make the old recharge flow open the browser", async () => {
    let session = { ...LOGGED_IN };
    const opened: string[] = [];
    mockShellCommands({
      we2ai_session_status: () => session,
      open_external: (body) => {
        opened.push(body.url);
        return null;
      },
    });
    let mode: "fail" | "hold" | "ok" = "fail";
    const pending: Array<(value: We2aiBalance) => void> = [];
    vi.spyOn(we2aiApi, "getBalance").mockImplementation(() => {
      if (mode === "fail") return Promise.reject(new Error("unavailable"));
      if (mode === "hold") {
        return new Promise<We2aiBalance>((resolve) => pending.push(resolve));
      }
      return Promise.resolve(bal(3));
    });
    let announcementsFail = false;
    vi.spyOn(we2aiApi, "listAnnouncements").mockImplementation(async () => {
      if (announcementsFail) throw { code: "TOKEN_REVOKED", message: "x" };
      return [];
    });
    let changed: (() => void) | null = null;
    vi.spyOn(we2aiApi, "onAnnouncementsChanged").mockImplementation(
      async (handler) => {
        changed = handler;
        return () => {};
      },
    );
    vi.spyOn(console, "debug").mockImplementation(() => {});
    const user = userEvent.setup();

    renderShell();
    const chip = await screen.findByTestId("balance-chip");
    await user.click(chip);
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "充值" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    expect(chip).toHaveTextContent("—");

    // 余额未知：点主按钮会先请求余额取基线，请求一直挂起。
    mode = "hold";
    await user.click(screen.getByRole("button", { name: "去 WE2AI 充值 ↗" }));
    expect(pending).toHaveLength(1);

    // 请求完成前换会话。
    session = { ...LOGGED_IN, userId: 43 };
    mode = "ok";
    announcementsFail = true;
    await act(async () => {
      changed?.();
    });
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "模型广场" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );

    // 旧请求这时才成功返回：不能再打开旧会话的支付页，也不能进入等待。
    await act(async () => {
      pending[0](bal(999));
    });
    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(opened).toEqual([]);
    expect(screen.queryByTestId("billing-watch-waiting")).toBeNull();
  });

  it("identifies the session by region and userId: same region and same emailMasked but a different userId tears down the waiting flow without a false success", async () => {
    let session = { ...LOGGED_IN };
    const opened: string[] = [];
    mockShellCommands({
      we2ai_session_status: () => session,
      open_external: (body) => {
        opened.push(body.url);
        return null;
      },
    });
    let current = 12.48;
    let holdNext = false;
    const pending: Array<(value: We2aiBalance) => void> = [];
    vi.spyOn(we2aiApi, "getBalance").mockImplementation(() => {
      if (holdNext) {
        holdNext = false;
        return new Promise<We2aiBalance>((resolve) => pending.push(resolve));
      }
      return Promise.resolve(bal(current));
    });
    let announcementsFail = false;
    vi.spyOn(we2aiApi, "listAnnouncements").mockImplementation(async () => {
      if (announcementsFail) throw { code: "TOKEN_REVOKED", message: "x" };
      return [];
    });
    let changed: (() => void) | null = null;
    vi.spyOn(we2aiApi, "onAnnouncementsChanged").mockImplementation(
      async (handler) => {
        changed = handler;
        return () => {};
      },
    );
    vi.spyOn(console, "debug").mockImplementation(() => {});
    const user = userEvent.setup();

    renderShell();
    const chip = await openBillingTab(user);
    await waitFor(() => expect(chip).toHaveTextContent("$12.48"));
    await user.click(screen.getByRole("button", { name: "去 WE2AI 充值 ↗" }));
    await screen.findByTestId("billing-watch-waiting");
    expect(opened).toHaveLength(1);

    // 旧账号的一次到账检测请求在途。
    holdNext = true;
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    expect(pending).toHaveLength(1);

    // 活动会话被直接换成同区域、emailMasked 完全相同的另一个账号。
    session = { ...LOGGED_IN, userId: 43 };
    current = 3;
    announcementsFail = true;
    await act(async () => {
      changed?.();
    });
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "模型广场" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    await waitFor(() =>
      expect(screen.getByTestId("balance-chip")).toHaveTextContent("$3.00"),
    );
    // 充值页已重建：旧账号的等待状态消失。
    expect(screen.queryByTestId("billing-watch-waiting")).toBeNull();

    // 旧账号的高余额响应此时才回来：不能误报到账，也不能再打开浏览器。
    await act(async () => {
      pending[0](bal(999));
    });
    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(screen.queryByText(/充值成功/)).toBeNull();
    expect(screen.queryByTestId("billing-watch-success")).toBeNull();
    expect(screen.getByTestId("balance-chip")).toHaveTextContent("$3.00");
    expect(opened).toHaveLength(1);
  });
});
