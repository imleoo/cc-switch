import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { http, HttpResponse, type JsonBodyType } from "msw";
import { server } from "../msw/server";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { UpdateProvider } from "@/contexts/UpdateContext";
import { isWe2aiIpcAllowed } from "@/we2ai/ipcWhitelist";
import { We2aiShell } from "@/we2ai/We2aiShell";

const TAURI_ENDPOINT = "http://tauri.local";
const SECRET = "sk-we2ai-plaintext-never-in-list-1234567890";

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

const MANAGED_KEY = {
  id: 5,
  name: "工作",
  status: "active",
  maskedKey: "sk-we2…5555",
  group: { id: 7, name: "默认分组", rate: 0.8 },
  quota: 10,
  quotaUsed: 3.2,
  expiresAt: null,
  lastUsedAt: null,
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
          return HttpResponse.json({ baseUrl: "https://api.we2ai.com" });
        case "we2ai_get_balance":
          return HttpResponse.json({
            balance: 12.48,
            frozenBalance: 0,
            totalRecharged: 0,
          });
        case "we2ai_list_keys":
          return HttpResponse.json({ keys: [], selectedKeyId: null });
        case "we2ai_manage_list_keys":
          return HttpResponse.json([MANAGED_KEY]);
        case "we2ai_list_key_groups":
          return HttpResponse.json([
            { id: 7, name: "默认分组", platform: "anthropic", rate: 0.8 },
          ]);
        default:
          return HttpResponse.json(null);
      }
    }),
  );
  return calls;
}

async function openKeysTab(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("tab", { name: "Key 管理" }));
}

describe("We2aiShell key management tab", () => {
  afterEach(() => {
    server.resetHandlers();
    vi.restoreAllMocks();
  });

  it("orders the tabs marketplace | keys | billing | settings and loads the key list only when the tab opens", async () => {
    const calls = mockShellCommands();
    const user = userEvent.setup();

    renderShell();

    await screen.findByTestId("balance-chip");
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual([
      "模型广场",
      "Key 管理",
      "充值",
      "设置",
    ]);
    expect(
      calls.filter((c) => c.command === "we2ai_manage_list_keys"),
    ).toHaveLength(0);

    await openKeysTab(user);

    const row = await screen.findByTestId("key-row-5");
    expect(row).toHaveTextContent("工作");
    expect(row).toHaveTextContent("默认分组 ×0.8");
    expect(row).toHaveTextContent("$3.20 / $10");
    expect(screen.getByRole("tab", { name: "Key 管理" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("only invokes IPC commands that are allowed and never puts plaintext in the list response", async () => {
    const calls = mockShellCommands();
    const user = userEvent.setup();

    renderShell();
    await openKeysTab(user);
    await screen.findByTestId("key-row-5");
    await user.click(screen.getByRole("button", { name: "+ 新建 Key" }));
    await screen.findByTestId("key-edit-form");

    for (const { command } of calls) {
      expect(
        isWe2aiIpcAllowed(command),
        `command "${command}" is not in the WE2AI IPC whitelist`,
      ).toBe(true);
    }
    expect(
      calls.filter((c) => c.command === "we2ai_manage_list_keys").length,
    ).toBeGreaterThan(0);
    expect(JSON.stringify(MANAGED_KEY)).not.toContain(SECRET);
    expect(document.body.textContent).not.toContain(SECRET);
  });

  it("sends the documented IPC payloads for create, update, delete and copy", async () => {
    const created = {
      key: { ...MANAGED_KEY, id: 11, name: "新建" },
      plaintext: SECRET,
    };
    const calls = mockShellCommands({
      we2ai_create_key: () => created,
      we2ai_update_key: () => MANAGED_KEY,
      we2ai_delete_key: () => null,
      we2ai_copy_key: () => null,
    });
    const user = userEvent.setup();

    renderShell();
    await openKeysTab(user);
    await screen.findByTestId("key-row-5");

    // 创建
    await user.click(screen.getByRole("button", { name: "+ 新建 Key" }));
    await user.type(await screen.findByLabelText("名称"), "新建");
    const submit = screen.getByRole("button", { name: "创建" });
    await waitFor(() => expect(submit).toBeEnabled());
    await user.click(submit);
    const card = await screen.findByTestId("key-created-dialog");
    expect(within(card).getByTestId("key-created-plaintext")).toHaveTextContent(
      SECRET,
    );
    const createCall = calls.find((c) => c.command === "we2ai_create_key");
    expect(createCall?.body.input).toMatchObject({
      name: "新建",
      groupId: 7,
    });
    expect(createCall?.body.input.idempotencyKey).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
    await user.click(
      within(card).getByRole("button", { name: "我已保存，关闭" }),
    );
    await waitFor(() =>
      expect(document.body.textContent).not.toContain(SECRET),
    );

    // 复制：只传 id，Rust 写剪贴板，明文不经 IPC。
    await user.click(screen.getByRole("button", { name: "复制 工作" }));
    await waitFor(() =>
      expect(calls.find((c) => c.command === "we2ai_copy_key")?.body).toEqual({
        id: 5,
      }),
    );
    expect(document.body.textContent).not.toContain(SECRET);
    expect(
      calls.some((c) => c.command === "copy_text_to_clipboard"),
      "前端不再调用上游 copy_text_to_clipboard",
    ).toBe(false);

    // 启停
    await user.click(screen.getByRole("button", { name: "禁用 工作" }));
    await waitFor(() =>
      expect(calls.find((c) => c.command === "we2ai_update_key")?.body).toEqual(
        {
          id: 5,
          input: { status: "inactive" },
        },
      ),
    );

    // 删除
    await user.click(screen.getByRole("button", { name: "删除 工作" }));
    const dialog = await screen.findByTestId("key-delete-dialog");
    await user.type(within(dialog).getByLabelText("Key 名称"), "工作");
    const confirm = within(dialog).getByRole("button", { name: "确认删除" });
    await waitFor(() => expect(confirm).toBeEnabled());
    await user.click(confirm);
    await waitFor(() =>
      expect(calls.find((c) => c.command === "we2ai_delete_key")?.body).toEqual(
        {
          id: 5,
        },
      ),
    );
    // 判断「是否在用」复用现有的 we2ai_list_keys。
    expect(calls.some((c) => c.command === "we2ai_list_keys")).toBe(true);
  });

  it("the marketplace empty state's create button opens the Key management tab with the create dialog, once", async () => {
    // 默认 mock：we2ai_list_keys 返回空列表 -> 模型广场显示无 Key 空状态。
    const calls = mockShellCommands();
    const user = userEvent.setup();

    renderShell();

    await user.click(await screen.findByRole("button", { name: "去创建 Key" }));

    expect(await screen.findByTestId("key-edit-form")).toBeInTheDocument();
    // 模态弹窗打开时背景被 aria-hidden，需要 hidden: true 才能查到 Tab。
    expect(
      screen.getByRole("tab", { name: "Key 管理", hidden: true }),
    ).toHaveAttribute("aria-selected", "true");
    expect(screen.getAllByTestId("key-edit-form")).toHaveLength(1);
    expect(
      calls.filter((c) => c.command === "we2ai_manage_list_keys"),
    ).toHaveLength(1);

    // 取消弹窗后切走再切回：不会被旧请求再次自动打开。
    await user.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() =>
      expect(screen.queryByTestId("key-edit-form")).not.toBeInTheDocument(),
    );
    await user.click(screen.getByRole("tab", { name: "模型广场" }));
    await user.click(screen.getByRole("tab", { name: "Key 管理" }));
    await screen.findByTestId("key-row-5");
    expect(screen.queryByTestId("key-edit-form")).not.toBeInTheDocument();
  });

  it("does not show the create button once the account has keys", async () => {
    mockShellCommands({
      we2ai_list_keys: () => ({
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
      }),
      we2ai_key_models: () => ({
        models: [],
        callable: true,
        blockedReason: null,
        pricing: null,
      }),
    });

    renderShell();

    await screen.findByTestId("balance-chip");
    await screen.findByTestId("we2ai-single-key");
    expect(
      screen.queryByRole("button", { name: "去创建 Key" }),
    ).not.toBeInTheDocument();
  });
});
