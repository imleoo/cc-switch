import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";
import { copyText } from "@/lib/clipboard";
import {
  we2aiApi,
  type We2aiApiError,
  type We2aiCreatedKey,
  type We2aiManagedKey,
} from "@/we2ai/api";
import { KeyManagePage } from "@/we2ai/KeyManagePage";
import { getWe2aiStrings } from "@/we2ai/strings";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}));

vi.mock("@/lib/clipboard", () => ({
  copyText: vi.fn(async () => undefined),
}));

// Radix Select 打开时会滚动到选中项，jsdom 没有实现 scrollIntoView。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

const t = getWe2aiStrings("zh");
const DAY = 24 * 60 * 60 * 1000;
const SECRET = "sk-we2ai-plaintext-should-never-render-1234";

function isoIn(ms: number): string {
  return new Date(Date.now() + ms).toISOString();
}

function makeKey(
  id: number,
  patch: Partial<We2aiManagedKey> = {},
): We2aiManagedKey {
  return {
    id,
    name: `key-${id}`,
    status: "active",
    maskedKey: `sk-we2…${String(id).repeat(4)}`,
    group: { id: 7, name: "默认分组", rate: 1 },
    quota: 0,
    quotaUsed: 0,
    expiresAt: null,
    lastUsedAt: null,
    ...patch,
  };
}

function apiError(code: string): We2aiApiError {
  return { code, message: code } as We2aiApiError;
}

const onSessionMaybeEnded = vi.fn();

async function renderPage(
  props: Partial<React.ComponentProps<typeof KeyManagePage>> = {},
) {
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(
      <KeyManagePage
        t={t}
        onSessionMaybeEnded={onSessionMaybeEnded}
        {...props}
      />,
    );
  });
  return view;
}

/** 捕获页面订阅的 keys-changed 回调，供用例模拟 Rust 事件。 */
function captureKeysChanged() {
  let handler: (() => void) | null = null;
  vi.spyOn(we2aiApi, "onKeysChanged").mockImplementation(async (fn) => {
    handler = fn;
    return () => {
      handler = null;
    };
  });
  return {
    emit: async () => {
      await act(async () => {
        handler?.();
      });
    },
    isSubscribed: () => handler !== null,
  };
}

describe("KeyManagePage", () => {
  beforeEach(() => {
    vi.spyOn(we2aiApi, "onKeysChanged").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "listKeyGroups").mockResolvedValue([]);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  describe("rendering", () => {
    it("renders the four status chips, group chip with rate, quota bar, expiry and last used", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, {
          group: { id: 7, name: "默认分组", rate: 0.8 },
          quota: 10,
          quotaUsed: 3.2,
          expiresAt: isoIn(3 * DAY + 3600_000),
          lastUsedAt: isoIn(-2 * 3600_000),
        }),
        makeKey(2, {
          status: "inactive",
          group: null,
          quota: 0,
          quotaUsed: 1.5,
        }),
        makeKey(3, { status: "quota_exhausted", quota: 5, quotaUsed: 5 }),
        makeKey(4, { status: "expired", expiresAt: isoIn(-DAY) }),
      ]);

      await renderPage();

      expect(await screen.findByTestId("key-row-1")).toBeInTheDocument();
      expect(screen.getByTestId("key-status-1")).toHaveTextContent("正常");
      expect(screen.getByTestId("key-status-1")).toHaveAttribute(
        "data-status",
        "active",
      );
      expect(screen.getByTestId("key-status-2")).toHaveTextContent("已禁用");
      expect(screen.getByTestId("key-status-3")).toHaveTextContent("额度用完");
      expect(screen.getByTestId("key-status-4")).toHaveTextContent("已过期");

      // 倍率 ≠ 1 才显示 ×0.8；无分组显示「无分组」。
      expect(screen.getByTestId("key-group-1")).toHaveTextContent(
        "默认分组 ×0.8",
      );
      expect(screen.getByTestId("key-group-3")).toHaveTextContent("默认分组");
      expect(screen.getByTestId("key-group-3")).not.toHaveTextContent("×");
      expect(screen.getByTestId("key-group-2")).toHaveTextContent("无分组");

      expect(screen.getByTestId("key-quota-1")).toHaveTextContent(
        "$3.20 / $10",
      );
      expect(screen.getByTestId("key-quota-2")).toHaveTextContent(
        "$1.50 / 不限",
      );
      const bar = within(screen.getByTestId("key-row-1")).getByRole(
        "progressbar",
      );
      expect(bar).toHaveAttribute("aria-valuenow", "32");
      expect(
        within(screen.getByTestId("key-row-3")).getByRole("progressbar"),
      ).toHaveAttribute("data-level", "full");

      // 3 天后到期：落在 7 天内，橙色。
      expect(screen.getByTestId("key-expiry-1")).toHaveTextContent(
        "3 天后过期",
      );
      expect(screen.getByTestId("key-expiry-1")).toHaveAttribute(
        "data-soon",
        "true",
      );
      expect(screen.getByTestId("key-expiry-2")).toHaveTextContent("永久");
      expect(screen.getByTestId("key-expiry-2")).toHaveAttribute(
        "data-soon",
        "false",
      );
      expect(
        within(screen.getByTestId("key-row-1")).getByText("2 小时前"),
      ).toBeInTheDocument();
      expect(
        within(screen.getByTestId("key-row-2")).getByText("从未使用"),
      ).toBeInTheDocument();

      // 掩码 Key，且页面里没有任何明文。
      expect(screen.getByText("sk-we2…1111")).toBeInTheDocument();
    });

    it("does not mark expiry orange when it is 7 or more days away", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { expiresAt: isoIn(8 * DAY) }),
      ]);
      await renderPage();
      expect(await screen.findByTestId("key-expiry-1")).toHaveAttribute(
        "data-soon",
        "false",
      );
    });

    it("shows an active key whose expiry has passed as expired and offers no enable toggle", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { status: "active", expiresAt: isoIn(-1000) }),
      ]);
      await renderPage();
      expect(await screen.findByTestId("key-status-1")).toHaveTextContent(
        "已过期",
      );
      expect(
        screen.queryByRole("button", { name: "禁用 key-1" }),
      ).not.toBeInTheDocument();
    });

    it("shows the empty state with the create button", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([]);
      await renderPage();
      expect(await screen.findByTestId("key-manage-empty")).toHaveTextContent(
        "还没有 Key",
      );
      expect(
        screen.getByRole("button", { name: "+ 新建 Key" }),
      ).toBeInTheDocument();
    });

    it("shows a load failure with retry and recovers", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockRejectedValueOnce(apiError("TRANSIENT"))
        .mockResolvedValue([makeKey(1)]);
      const user = userEvent.setup();

      await renderPage();

      const alert = await screen.findByRole("alert");
      expect(alert).toHaveTextContent("网络连接失败");
      // 网络类错误不触发会话复查。
      expect(onSessionMaybeEnded).not.toHaveBeenCalled();

      await user.click(screen.getByRole("button", { name: "重试" }));

      expect(await screen.findByTestId("key-row-1")).toBeInTheDocument();
      expect(list).toHaveBeenCalledTimes(2);
    });

    it("asks the shell to re-check the session on non-network load errors", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockRejectedValue(
        apiError("TOKEN_REVOKED"),
      );
      await renderPage();
      await screen.findByRole("alert");
      expect(onSessionMaybeEnded).toHaveBeenCalled();
    });

    it("retries when a list request is superseded on the Rust side", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockRejectedValueOnce(apiError("KEY_LIST_SUPERSEDED"))
        .mockResolvedValue([makeKey(1)]);
      await renderPage();
      expect(await screen.findByTestId("key-row-1")).toBeInTheDocument();
      expect(list).toHaveBeenCalledTimes(2);
    });
  });

  describe("accessibility and freshness", () => {
    it("labels each quota progress bar with the key name", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "工作", quota: 10, quotaUsed: 5 }),
      ]);
      await renderPage();
      expect(
        await screen.findByRole("progressbar", { name: "额度 工作" }),
      ).toHaveAttribute("aria-valuenow", "50");
    });

    it("refreshes relative times periodically without a reload", async () => {
      vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
      try {
        vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
          makeKey(1, { lastUsedAt: new Date(Date.now()).toISOString() }),
        ]);
        await renderPage();
        expect(
          within(await screen.findByTestId("key-row-1")).getByText("刚刚"),
        ).toBeInTheDocument();

        await act(async () => {
          vi.advanceTimersByTime(5 * 60_000);
        });

        expect(
          within(screen.getByTestId("key-row-1")).getByText("5 分钟前"),
        ).toBeInTheDocument();
      } finally {
        vi.useRealTimers();
      }
    });

    it("shows the in-progress hint for idempotency conflicts without a session re-check", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([makeKey(1)]);
      vi.spyOn(we2aiApi, "updateKey").mockRejectedValue(
        apiError("IDEMPOTENCY_IN_PROGRESS"),
      );
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "禁用 key-1" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrErrInProgress),
      );
      expect(onSessionMaybeEnded).not.toHaveBeenCalled();
    });
  });

  describe("filtering", () => {
    beforeEach(() => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "Work Laptop" }),
        makeKey(2, { name: "home pc", status: "inactive" }),
        makeKey(3, { name: "CI", status: "quota_exhausted", quota: 1 }),
      ]);
    });

    it("filters by name locally, case-insensitively", async () => {
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.type(screen.getByLabelText("搜索 Key 名称"), "  laptop ");

      expect(screen.getByTestId("key-row-1")).toBeInTheDocument();
      expect(screen.queryByTestId("key-row-2")).not.toBeInTheDocument();
      expect(screen.queryByTestId("key-row-3")).not.toBeInTheDocument();

      await user.clear(screen.getByLabelText("搜索 Key 名称"));
      await user.type(screen.getByLabelText("搜索 Key 名称"), "zzz");
      expect(
        await screen.findByTestId("key-manage-no-match"),
      ).toHaveTextContent("没有符合条件的 Key");
    });

    it("filters by status", async () => {
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("combobox", { name: "状态" }));
      await user.click(await screen.findByRole("option", { name: "已禁用" }));

      expect(screen.getByTestId("key-row-2")).toBeInTheDocument();
      expect(screen.queryByTestId("key-row-1")).not.toBeInTheDocument();
      expect(screen.queryByTestId("key-row-3")).not.toBeInTheDocument();
    });
  });

  describe("copy", () => {
    it("copies through the Rust side by key id and never touches the web clipboard or renders plaintext", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1),
        makeKey(2),
      ]);
      const copyKey = vi.spyOn(we2aiApi, "copyKey").mockResolvedValue();
      const writeText = vi.fn(async () => undefined);
      Object.defineProperty(navigator, "clipboard", {
        value: { writeText },
        configurable: true,
      });
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "复制 key-2" }));

      await waitFor(() => expect(copyKey).toHaveBeenCalledWith(2));
      expect(copyKey).toHaveBeenCalledTimes(1);
      expect(toast.success).toHaveBeenCalledWith("已复制");
      // 复制只靠 Rust：前端既没有 copyText，也没有 navigator.clipboard。
      expect(copyText).not.toHaveBeenCalled();
      expect(writeText).not.toHaveBeenCalled();
      expect(document.body.textContent).not.toContain(SECRET);
    });

    it("refreshes the list and asks to retry when the Rust cache no longer has the key", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockResolvedValue([makeKey(1)]);
      vi.spyOn(we2aiApi, "copyKey").mockRejectedValue(
        apiError("KEY_NOT_FOUND"),
      );
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "复制 key-1" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrCopyStale),
      );
      await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });

    it("reports a clipboard failure", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([makeKey(1)]);
      vi.spyOn(we2aiApi, "copyKey").mockRejectedValue(
        apiError("CLIPBOARD_FAILED"),
      );
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "复制 key-1" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrCopyFailed),
      );
      expect(document.body.textContent).not.toContain(SECRET);
    });
  });

  describe("enable / disable", () => {
    it("disables an active key and refreshes the list", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockResolvedValueOnce([makeKey(1, { name: "工作" })])
        .mockResolvedValue([makeKey(1, { name: "工作", status: "inactive" })]);
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey(1, { status: "inactive" }));
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "禁用 工作" }));

      await waitFor(() =>
        expect(update).toHaveBeenCalledWith(1, { status: "inactive" }),
      );
      await waitFor(() =>
        expect(screen.getByTestId("key-status-1")).toHaveTextContent("已禁用"),
      );
      expect(list).toHaveBeenCalledTimes(2);
      expect(toast.success).toHaveBeenCalledWith("已禁用「工作」");
      expect(
        screen.getByRole("button", { name: "启用 工作" }),
      ).toBeInTheDocument();
    });

    it("enables an inactive key", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { status: "inactive" }),
      ]);
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey(1));
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "启用 key-1" }));

      await waitFor(() =>
        expect(update).toHaveBeenCalledWith(1, { status: "active" }),
      );
    });

    it("offers no toggle for keys that ran out of quota", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { status: "quota_exhausted", quota: 5, quotaUsed: 5 }),
      ]);
      await renderPage();
      await screen.findByTestId("key-row-1");
      expect(
        screen.queryByRole("button", { name: /^(启用|禁用) key-1$/ }),
      ).not.toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "编辑 key-1" }),
      ).toBeInTheDocument();
    });

    it("surfaces a failed toggle without changing the list", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([makeKey(1)]);
      vi.spyOn(we2aiApi, "updateKey").mockRejectedValue(
        apiError("API_KEY_NOT_FOUND"),
      );
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "禁用 key-1" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrErrNotFound),
      );
      expect(screen.getByTestId("key-status-1")).toHaveTextContent("正常");
    });
  });

  describe("reset quota with an unknown result", () => {
    it("closes the dialog, warns that the result is unknown and refreshes the list instead of offering a retry", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockResolvedValue([
          makeKey(1, { name: "工作", quota: 10, quotaUsed: 3.2 }),
        ]);
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockRejectedValue(apiError("TRANSIENT"));
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "编辑 工作" }));
      await user.click(await screen.findByLabelText(/重置已用额度/));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(
          "请求结果未知，请刷新列表确认后再操作",
        ),
      );
      expect(update).toHaveBeenCalledTimes(1);
      await waitFor(() =>
        expect(screen.queryByTestId("key-edit-form")).not.toBeInTheDocument(),
      );
      await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
  });

  describe("delete", () => {
    it("requires typing the exact key name before the delete button enables", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "工作" }),
      ]);
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [],
        selectedKeyId: null,
      });
      const del = vi.spyOn(we2aiApi, "deleteKey").mockResolvedValue();
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "删除 工作" }));

      const dialog = await screen.findByTestId("key-delete-dialog");
      const confirm = within(dialog).getByRole("button", { name: "确认删除" });
      await waitFor(() => expect(confirm).toBeDisabled());
      const input = within(dialog).getByLabelText("Key 名称");

      await user.type(input, "工");
      expect(confirm).toBeDisabled();
      await user.type(input, "作");
      await waitFor(() => expect(confirm).toBeEnabled());
      expect(
        within(dialog).queryByTestId("key-delete-in-use"),
      ).not.toBeInTheDocument();

      await user.click(confirm);

      await waitFor(() => expect(del).toHaveBeenCalledWith(1));
      expect(toast.success).toHaveBeenCalledWith("已删除「工作」");
      await waitFor(() =>
        expect(
          screen.queryByTestId("key-delete-dialog"),
        ).not.toBeInTheDocument(),
      );
    });

    it("never calls window.confirm", async () => {
      const confirmSpy = vi.spyOn(window, "confirm");
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([makeKey(1)]);
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [],
        selectedKeyId: null,
      });
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");
      await user.click(screen.getByRole("button", { name: "删除 key-1" }));
      await screen.findByTestId("key-delete-dialog");
      expect(confirmSpy).not.toHaveBeenCalled();
    });

    it("warns in orange when the key is the one currently used by the tools", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "在用" }),
        makeKey(2, { name: "闲置" }),
      ]);
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [],
        selectedKeyId: 1,
        rememberedKeyId: 1,
      });
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "删除 在用" }));
      const warning = await screen.findByTestId("key-delete-in-use");
      expect(warning).toHaveTextContent(
        "Claude Code / Codex 当前使用此 Key，删除后将无法调用",
      );

      await user.click(screen.getByRole("button", { name: "取消" }));
      await waitFor(() =>
        expect(
          screen.queryByTestId("key-delete-dialog"),
        ).not.toBeInTheDocument(),
      );

      await user.click(screen.getByRole("button", { name: "删除 闲置" }));
      await screen.findByTestId("key-delete-dialog");
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "确认删除" })).toBeDisabled(),
      );
      expect(screen.queryByTestId("key-delete-in-use")).not.toBeInTheDocument();
    });

    it("does not warn when there is no remembered selection, even though the default selection falls back to the first key", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "首个" }),
      ]);
      // 无记忆：selectedKeyId 回退到第一个 Key，但 rememberedKeyId 为 null。
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [],
        selectedKeyId: 1,
        rememberedKeyId: null,
      });
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "删除 首个" }));
      const dialog = await screen.findByTestId("key-delete-dialog");
      await user.type(within(dialog).getByLabelText("Key 名称"), "首个");

      await waitFor(() =>
        expect(
          within(dialog).getByRole("button", { name: "确认删除" }),
        ).toBeEnabled(),
      );
      expect(
        within(dialog).queryByTestId("key-delete-in-use"),
      ).not.toBeInTheDocument();
    });

    it("treats a response without rememberedKeyId (older shape) as no memory", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "A" }),
      ]);
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [],
        selectedKeyId: 1,
      });
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "删除 A" }));
      const dialog = await screen.findByTestId("key-delete-dialog");
      await user.type(within(dialog).getByLabelText("Key 名称"), "A");

      await waitFor(() =>
        expect(
          within(dialog).getByRole("button", { name: "确认删除" }),
        ).toBeEnabled(),
      );
      expect(
        within(dialog).queryByTestId("key-delete-in-use"),
      ).not.toBeInTheDocument();
    });

    it("does not block deletion when the in-use check fails", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "A" }),
      ]);
      vi.spyOn(we2aiApi, "listKeys").mockRejectedValue(apiError("TRANSIENT"));
      vi.spyOn(we2aiApi, "deleteKey").mockResolvedValue();
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "删除 A" }));
      const dialog = await screen.findByTestId("key-delete-dialog");
      await user.type(within(dialog).getByLabelText("Key 名称"), "A");

      await waitFor(() =>
        expect(
          within(dialog).getByRole("button", { name: "确认删除" }),
        ).toBeEnabled(),
      );
    });

    it("keeps the dialog open and shows the error when deletion fails", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([
        makeKey(1, { name: "A" }),
      ]);
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [],
        selectedKeyId: null,
      });
      vi.spyOn(we2aiApi, "deleteKey").mockRejectedValue(apiError("TRANSIENT"));
      const user = userEvent.setup();
      await renderPage();
      await screen.findByTestId("key-row-1");

      await user.click(screen.getByRole("button", { name: "删除 A" }));
      const dialog = await screen.findByTestId("key-delete-dialog");
      await user.type(within(dialog).getByLabelText("Key 名称"), "A");
      const confirm = within(dialog).getByRole("button", { name: "确认删除" });
      await waitFor(() => expect(confirm).toBeEnabled());
      await user.click(confirm);

      expect(await screen.findByTestId("key-delete-error")).toHaveTextContent(
        "网络连接失败",
      );
      expect(screen.getByTestId("key-delete-dialog")).toBeInTheDocument();
    });
  });

  describe("created key card", () => {
    const created: We2aiCreatedKey = {
      key: makeKey(9, { name: "新 Key", maskedKey: "sk-we2…9999" }),
      plaintext: SECRET,
    };

    async function createViaDialog() {
      vi.spyOn(we2aiApi, "listKeyGroups").mockResolvedValue([
        { id: 7, name: "默认分组", platform: "anthropic", rate: 1 },
      ]);
      const create = vi.spyOn(we2aiApi, "createKey").mockResolvedValue(created);
      const user = userEvent.setup();
      await screen.findByTestId("key-manage-empty");
      await user.click(screen.getByRole("button", { name: "+ 新建 Key" }));
      await user.type(await screen.findByLabelText("名称"), "新 Key");
      const submit = screen.getByRole("button", { name: "创建" });
      await waitFor(() => expect(submit).toBeEnabled());
      await user.click(submit);
      return { create, user };
    }

    it("shows the plaintext once and clears it when the card is closed", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockResolvedValueOnce([])
        .mockResolvedValue([created.key]);
      await renderPage();

      const { create } = await createViaDialog();

      const card = await screen.findByTestId("key-created-dialog");
      expect(create).toHaveBeenCalledTimes(1);
      expect(
        within(card).getByTestId("key-created-plaintext"),
      ).toHaveTextContent(SECRET);
      expect(card).toHaveTextContent("关闭后这里不再显示完整 Key");
      // 创建成功后列表刷新，列表里只有掩码。
      await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      expect(await screen.findByTestId("key-row-9")).toHaveTextContent(
        "sk-we2…9999",
      );

      // 卡片里的复制同样走 Rust（按新 Key 的 id，创建时明文已并入 Rust 缓存），
      // 前端不回传明文，也不用 copyText。
      const copyKey = vi.spyOn(we2aiApi, "copyKey").mockResolvedValue();
      await userEvent.click(within(card).getByRole("button", { name: "复制" }));
      await waitFor(() => expect(copyKey).toHaveBeenCalledWith(9));
      expect(copyText).not.toHaveBeenCalled();

      await userEvent.click(
        within(card).getByRole("button", { name: "我已保存，关闭" }),
      );

      await waitFor(() =>
        expect(
          screen.queryByTestId("key-created-dialog"),
        ).not.toBeInTheDocument(),
      );
      expect(screen.queryByTestId("key-created-plaintext")).toBeNull();
      expect(document.body.textContent).not.toContain(SECRET);
    });

    it("card copy goes through copyKey and, like the list, refreshes and asks to retry on KEY_NOT_FOUND", async () => {
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockResolvedValueOnce([])
        .mockResolvedValue([created.key]);
      await renderPage();
      await createViaDialog();
      const card = await screen.findByTestId("key-created-dialog");
      await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      vi.spyOn(we2aiApi, "copyKey").mockRejectedValueOnce(
        apiError("KEY_NOT_FOUND"),
      );

      await userEvent.click(within(card).getByRole("button", { name: "复制" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrCopyStale),
      );
      await waitFor(() => expect(list).toHaveBeenCalledTimes(3));
      // 卡片仍在（明文仍可见，用户可再点复制）。
      expect(screen.getByTestId("key-created-dialog")).toBeInTheDocument();
    });

    it("clears the plaintext from the DOM after closing even though the list refreshed", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([]);
      await renderPage();
      await createViaDialog();
      await screen.findByTestId("key-created-dialog");
      expect(document.body.textContent).toContain(SECRET);

      await userEvent.click(
        screen.getByRole("button", { name: "我已保存，关闭" }),
      );

      await waitFor(() =>
        expect(document.body.textContent).not.toContain(SECRET),
      );
    });
  });

  describe("create request from the marketplace empty state", () => {
    it("opens the create dialog on mount when requested and reports the request as handled", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([]);
      const handled = vi.fn();

      await renderPage({
        openCreateRequested: true,
        onCreateRequestHandled: handled,
      });

      expect(await screen.findByTestId("key-edit-form")).toBeInTheDocument();
      expect(screen.getByRole("heading", { name: "新建 Key" })).toBeVisible();
      expect(handled).toHaveBeenCalled();
    });

    it("does not open the dialog when nothing was requested", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([]);
      const handled = vi.fn();

      await renderPage({ onCreateRequestHandled: handled });
      await screen.findByTestId("key-manage-empty");

      expect(screen.queryByTestId("key-edit-form")).not.toBeInTheDocument();
      expect(handled).not.toHaveBeenCalled();
    });

    it("opens the dialog when the request flips to true while mounted, and only once per request", async () => {
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([]);
      const handled = vi.fn();
      const view = await renderPage({
        openCreateRequested: false,
        onCreateRequestHandled: handled,
      });
      await screen.findByTestId("key-manage-empty");

      await act(async () => {
        view.rerender(
          <KeyManagePage
            t={t}
            onSessionMaybeEnded={onSessionMaybeEnded}
            openCreateRequested
            onCreateRequestHandled={handled}
          />,
        );
      });
      expect(await screen.findByTestId("key-edit-form")).toBeInTheDocument();
      expect(handled).toHaveBeenCalledTimes(1);

      // 外壳复位请求后用户取消弹窗：不会再次自动打开。
      await act(async () => {
        view.rerender(
          <KeyManagePage
            t={t}
            onSessionMaybeEnded={onSessionMaybeEnded}
            openCreateRequested={false}
            onCreateRequestHandled={handled}
          />,
        );
      });
      await userEvent.click(screen.getByRole("button", { name: "取消" }));
      await waitFor(() =>
        expect(screen.queryByTestId("key-edit-form")).not.toBeInTheDocument(),
      );
      expect(handled).toHaveBeenCalledTimes(1);
    });
  });

  describe("keys-changed event", () => {
    it("refreshes the list when Rust reports a change", async () => {
      const events = captureKeysChanged();
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockResolvedValueOnce([makeKey(1)])
        .mockResolvedValue([makeKey(1), makeKey(2)]);
      await renderPage();
      await screen.findByTestId("key-row-1");
      expect(screen.queryByTestId("key-row-2")).not.toBeInTheDocument();
      expect(events.isSubscribed()).toBe(true);

      await events.emit();

      expect(await screen.findByTestId("key-row-2")).toBeInTheDocument();
      expect(list).toHaveBeenCalledTimes(2);
    });

    it("merges a refresh request that arrives while a load is in flight into one follow-up load", async () => {
      const events = captureKeysChanged();
      let resolveFirst!: (keys: We2aiManagedKey[]) => void;
      const list = vi
        .spyOn(we2aiApi, "manageListKeys")
        .mockImplementationOnce(
          () => new Promise<We2aiManagedKey[]>((r) => (resolveFirst = r)),
        )
        .mockResolvedValue([makeKey(1), makeKey(2)]);
      await renderPage();
      await events.emit();
      await events.emit();
      // 在途请求还没返回：不会并发发出第二、第三个请求。
      expect(list).toHaveBeenCalledTimes(1);

      await act(async () => {
        resolveFirst([makeKey(1)]);
      });

      expect(await screen.findByTestId("key-row-2")).toBeInTheDocument();
      expect(list).toHaveBeenCalledTimes(2);
    });

    it("unsubscribes on unmount", async () => {
      const events = captureKeysChanged();
      vi.spyOn(we2aiApi, "manageListKeys").mockResolvedValue([makeKey(1)]);
      const view = await renderPage();
      await screen.findByTestId("key-row-1");
      expect(events.isSubscribed()).toBe(true);
      view.unmount();
      expect(events.isSubscribed()).toBe(false);
    });
  });
});
