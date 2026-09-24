import * as React from "react";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, it, vi, afterEach } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { LoginPage } from "@/we2ai/LoginPage";
import { we2aiApi } from "@/we2ai/api";
import { getWe2aiStrings } from "@/we2ai/strings";
import { isWe2aiIpcAllowed } from "@/we2ai/ipcWhitelist";

const TAURI_ENDPOINT = "http://tauri.local";
const t = getWe2aiStrings("zh");

// Radix Select 在 jsdom 下依赖真实的指针事件与 portal 定位，测试里用简化的
// <select> 替身即可覆盖"切换区域触发对应行为"这条业务逻辑，与项目内其他
// 测试（如 WebdavSyncSection.test.tsx）的做法一致。
vi.mock("@/components/ui/select", () => ({
  Select: ({ value, onValueChange, disabled, children }: any) => (
    <select
      data-testid="we2ai-region-select"
      value={value}
      disabled={disabled}
      onChange={(event) => onValueChange?.(event.target.value)}
    >
      {children}
    </select>
  ),
  SelectTrigger: ({ children }: any) => <>{children}</>,
  SelectValue: () => null,
  SelectContent: ({ children }: any) => <>{children}</>,
  SelectItem: ({ value, children }: any) => (
    <option value={value}>{children}</option>
  ),
}));

const TabsContext = React.createContext<{
  value: string;
  onValueChange?: (value: string) => void;
}>({ value: "email" });

vi.mock("@/components/ui/tabs", () => ({
  Tabs: ({ value, onValueChange, children }: any) => (
    <TabsContext.Provider value={{ value, onValueChange }}>
      {children}
    </TabsContext.Provider>
  ),
  TabsList: ({ children }: any) => <div>{children}</div>,
  TabsTrigger: ({ value, children }: any) => {
    const ctx = React.useContext(TabsContext);
    return (
      <button type="button" onClick={() => ctx.onValueChange?.(value)}>
        {children}
      </button>
    );
  },
  TabsContent: ({ value, children }: any) => {
    const ctx = React.useContext(TabsContext);
    return ctx.value === value ? <div>{children}</div> : null;
  },
}));

function mockInvoke(
  handlers: Record<string, (body: any) => unknown | Promise<unknown>>,
) {
  const invoked: string[] = [];
  server.use(
    http.post(`${TAURI_ENDPOINT}/*`, async ({ request }) => {
      const command = request.url.slice(`${TAURI_ENDPOINT}/`.length);
      invoked.push(command);
      const body = await request.text();
      const parsed = body ? JSON.parse(body) : {};
      const handler = handlers[command];
      if (!handler) {
        throw new Error(`unexpected invoke: ${command}`);
      }
      // `await` 一个非 Promise 的同步返回值是恒等操作，不影响既有用例；
      // 交错测试需要让某个命令的响应"挂起"到测试主动 resolve 为止，
      // handler 因此也可以返回一个 Promise（Codex 代码评审第 3 轮高危项 1
      // 的前端交错用例）。
      const result = await handler(parsed);
      return HttpResponse.json((result ?? null) as JsonBodyType);
    }),
  );
  return invoked;
}

describe("LoginPage", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  it("only shows the phone tab for domestic regions", async () => {
    const onLoginSuccess = vi.fn();
    mockInvoke({
      we2ai_available_regions: () => ["international", "domestic_prod"],
      we2ai_get_last_region: () => "international",
      we2ai_set_last_region: () => null,
      // 切区域会静默尝试恢复该区域的会话（见 LoginPage 的
      // handleRegionChange）；这里没有可恢复的会话，返回 "needLogin"。
      we2ai_resume_session: () => "needLogin",
    });
    render(<LoginPage t={t} onLoginSuccess={onLoginSuccess} />);

    await screen.findByText(t.tabEmailLogin);
    expect(screen.queryByText(t.tabPhoneLogin)).not.toBeInTheDocument();

    fireEvent.change(screen.getByTestId("we2ai-region-select"), {
      target: { value: "domestic_prod" },
    });

    await waitFor(() => {
      expect(screen.getByText(t.tabPhoneLogin)).toBeInTheDocument();
    });
    // "needLogin" 不应该跳过登录表单。
    expect(onLoginSuccess).not.toHaveBeenCalled();
  });

  it("silently resumes the session when switching to a region that already has one", async () => {
    const onLoginSuccess = vi.fn();
    mockInvoke({
      we2ai_available_regions: () => ["international", "domestic_prod"],
      we2ai_get_last_region: () => "international",
      we2ai_set_last_region: () => null,
      we2ai_resume_session: () => "restored",
    });
    render(<LoginPage t={t} onLoginSuccess={onLoginSuccess} />);

    await screen.findByText(t.tabEmailLogin);
    fireEvent.change(screen.getByTestId("we2ai-region-select"), {
      target: { value: "domestic_prod" },
    });

    await waitFor(() => expect(onLoginSuccess).toHaveBeenCalledTimes(1));
  });

  it("logs in with email/password and calls onLoginSuccess", async () => {
    const invoked = mockInvoke({
      we2ai_available_regions: () => ["international"],
      we2ai_get_last_region: () => null,
      we2ai_login_email: (body) => {
        expect(body).toMatchObject({
          region: "international",
          email: "user@we2ai.com",
          password: "pw",
        });
        return { kind: "loggedIn" };
      },
    });
    const onLoginSuccess = vi.fn();
    render(<LoginPage t={t} onLoginSuccess={onLoginSuccess} />);

    fireEvent.change(await screen.findByLabelText(t.emailLabel), {
      target: { value: "user@we2ai.com" },
    });
    fireEvent.change(screen.getByLabelText(t.passwordLabel), {
      target: { value: "pw" },
    });
    fireEvent.click(screen.getByRole("button", { name: t.loginButton }));

    await waitFor(() => expect(onLoginSuccess).toHaveBeenCalledTimes(1));
    for (const command of invoked) {
      expect(isWe2aiIpcAllowed(command)).toBe(true);
    }
  });

  it("shows the 2FA form when the server requires it, then completes login", async () => {
    mockInvoke({
      we2ai_available_regions: () => ["international"],
      we2ai_get_last_region: () => null,
      we2ai_login_email: () => ({
        kind: "requires2fa",
        tempToken: "tmp-123",
        emailMasked: "u****@we2ai.com",
      }),
      we2ai_login_2fa: (body) => {
        expect(body).toMatchObject({
          region: "international",
          tempToken: "tmp-123",
          totpCode: "123456",
        });
        return null;
      },
    });
    const onLoginSuccess = vi.fn();
    render(<LoginPage t={t} onLoginSuccess={onLoginSuccess} />);

    fireEvent.change(await screen.findByLabelText(t.emailLabel), {
      target: { value: "user@we2ai.com" },
    });
    fireEvent.change(screen.getByLabelText(t.passwordLabel), {
      target: { value: "pw" },
    });
    fireEvent.click(screen.getByRole("button", { name: t.loginButton }));

    await screen.findByText(t.twoFaTitle);
    expect(
      screen.getByText("u****@we2ai.com", { exact: false }),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText(t.twoFaCodeLabel), {
      target: { value: "123456" },
    });
    fireEvent.click(screen.getByRole("button", { name: t.twoFaSubmit }));

    await waitFor(() => expect(onLoginSuccess).toHaveBeenCalledTimes(1));
  });

  it("shows the email-page hint for BACKEND_MODE_ACTIVE on the phone tab", async () => {
    mockInvoke({
      we2ai_available_regions: () => ["international", "domestic_prod"],
      we2ai_get_last_region: () => "domestic_prod",
    });
    // Tauri 的真实 invoke() 在命令返回 `Err(structured)` 时会直接用反序列化后
    // 的错误对象 reject（不是字符串化的 Error），但本仓库测试用的 HTTP mock
    // 桥接层（tests/msw/tauriMocks.ts）是为既有 `Result<T, String>` 命令写的，
    // 非 2xx 响应一律被转成 `new Error(rawText)`，丢失了结构化错误的字段。
    // 这里直接 spy 掉 `we2aiApi.sendSmsCode` 注入一个真实形状的
    // `We2aiApiError`，绕开这层保真度损失，单独验证 LoginPage 对结构化错误码
    // 的处理，而不是去改造被很多既有测试依赖的共享 mock 桥接层。
    vi.spyOn(we2aiApi, "sendSmsCode").mockRejectedValue({
      code: "BACKEND_MODE_ACTIVE",
      message: "backend mode active",
    });
    render(<LoginPage t={t} onLoginSuccess={vi.fn()} />);

    fireEvent.click(await screen.findByText(t.tabPhoneLogin));
    fireEvent.change(screen.getByLabelText(t.phoneLabel), {
      target: { value: "13800000000" },
    });
    fireEvent.click(screen.getByRole("button", { name: t.sendCodeButton }));

    await screen.findByText(t.errorBackendModeActivePhoneHint);
  });

  // Codex 代码评审第 3 轮高危项 1：邮箱登录请求在途时，区域切换（Select）
  // 必须被禁用——这是 Rust 侧操作代次校验之外的前端配合措施，减少用户在
  // 真实使用中触发"旧区域登录覆盖新区域会话"这类竞态的机会。
  it("disables the region select while an email login request is in flight", async () => {
    let resolveLogin: ((value: unknown) => void) | undefined;
    mockInvoke({
      we2ai_available_regions: () => ["international", "domestic_prod"],
      we2ai_get_last_region: () => null,
      we2ai_login_email: () =>
        new Promise((resolve) => {
          resolveLogin = resolve;
        }),
    });
    render(<LoginPage t={t} onLoginSuccess={vi.fn()} />);

    fireEvent.change(await screen.findByLabelText(t.emailLabel), {
      target: { value: "user@we2ai.com" },
    });
    fireEvent.change(screen.getByLabelText(t.passwordLabel), {
      target: { value: "pw" },
    });
    fireEvent.click(screen.getByRole("button", { name: t.loginButton }));

    await waitFor(() => {
      expect(screen.getByTestId("we2ai-region-select")).toBeDisabled();
    });

    resolveLogin?.({ kind: "loggedIn" });
    await waitFor(() => {
      expect(screen.getByTestId("we2ai-region-select")).not.toBeDisabled();
    });
  });

  // 反过来：区域切换（恢复会话）请求在途时，登录按钮必须被禁用，避免
  // 用户在这个窗口内又提交一次登录，制造同一类竞态。
  it("disables the login button while a region switch is resuming a session", async () => {
    let resolveResume: ((value: unknown) => void) | undefined;
    mockInvoke({
      we2ai_available_regions: () => ["international", "domestic_prod"],
      we2ai_get_last_region: () => "international",
      we2ai_set_last_region: () => null,
      we2ai_resume_session: () =>
        new Promise((resolve) => {
          resolveResume = resolve;
        }),
    });
    render(<LoginPage t={t} onLoginSuccess={vi.fn()} />);

    await screen.findByText(t.tabEmailLogin);
    fireEvent.change(screen.getByTestId("we2ai-region-select"), {
      target: { value: "domestic_prod" },
    });

    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: t.loginButton }),
      ).toBeDisabled();
    });

    resolveResume?.("needLogin");
    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: t.loginButton }),
      ).not.toBeDisabled();
    });
  });

  it("shows a network error message when the request fails outright", async () => {
    server.use(http.post(`${TAURI_ENDPOINT}/*`, () => HttpResponse.error()));
    render(<LoginPage t={t} onLoginSuccess={vi.fn()} />);

    fireEvent.change(await screen.findByLabelText(t.emailLabel), {
      target: { value: "user@we2ai.com" },
    });
    fireEvent.change(screen.getByLabelText(t.passwordLabel), {
      target: { value: "pw" },
    });
    fireEvent.click(screen.getByRole("button", { name: t.loginButton }));

    await screen.findByText(t.errorNetwork);
  });
});
