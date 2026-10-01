import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  we2aiApi,
  type We2aiApiError,
  type We2aiCreatedKey,
  type We2aiManagedKey,
} from "@/we2ai/api";
import { KeyEditDialog } from "@/we2ai/KeyEditDialog";
import { getWe2aiStrings } from "@/we2ai/strings";

// Radix Select 打开时会滚动到选中项，jsdom 没有实现 scrollIntoView。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

const t = getWe2aiStrings("zh");
const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

const GROUPS = [
  { id: 7, name: "默认分组", platform: "anthropic", rate: 1 },
  { id: 8, name: "优惠组", platform: "openai", rate: 0.8 },
];

// 相对时间，不写死日历日期：避免某天之后「默认过期时间已过去」让用例失效。
const DAY_MS = 24 * 60 * 60 * 1000;
const FUTURE_EXPIRES_AT = new Date(Date.now() + 90 * DAY_MS).toISOString();

function makeKey(patch: Partial<We2aiManagedKey> = {}): We2aiManagedKey {
  return {
    id: 5,
    name: "工作",
    status: "active",
    maskedKey: "sk-we2…5555",
    group: { id: 7, name: "默认分组", rate: 1 },
    quota: 10,
    quotaUsed: 3.2,
    expiresAt: FUTURE_EXPIRES_AT,
    lastUsedAt: null,
    ...patch,
  };
}

function created(): We2aiCreatedKey {
  return { key: makeKey({ id: 11 }), plaintext: "sk-secret-plaintext" };
}

function apiError(code: string): We2aiApiError {
  return { code, message: code } as We2aiApiError;
}

const onClose = vi.fn();
const onCreated = vi.fn();
const onUpdated = vi.fn();
const onResultUnknown = vi.fn();
const onSessionMaybeEnded = vi.fn();

async function renderDialog(keyItem: We2aiManagedKey | null) {
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(
      <KeyEditDialog
        t={t}
        keyItem={keyItem}
        onClose={onClose}
        onCreated={onCreated}
        onUpdated={onUpdated}
        onResultUnknown={onResultUnknown}
        onSessionMaybeEnded={onSessionMaybeEnded}
      />,
    );
  });
  // 等分组加载完成（创建时提交按钮在加载期间禁用）。
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: keyItem ? "保存" : "创建" }),
    ).toBeEnabled(),
  );
  return view;
}

describe("KeyEditDialog", () => {
  beforeEach(() => {
    vi.spyOn(we2aiApi, "listKeyGroups").mockResolvedValue(GROUPS);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  describe("create", () => {
    const NAME_ERROR = /名称不能为空，且不超过 100 字节/;

    it("rejects an empty name and a name over 100 escaped UTF-8 bytes without sending a request", async () => {
      const create = vi.spyOn(we2aiApi, "createKey");
      const user = userEvent.setup();
      await renderDialog(null);

      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(await screen.findByText(NAME_ERROR)).toBeVisible();

      // 34 个汉字 = 102 字节。
      fireEvent.change(screen.getByLabelText("名称"), {
        target: { value: "中".repeat(34) },
      });
      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(screen.getByText(NAME_ERROR)).toBeVisible();

      // 96 个 x + `&`（转义后 5 字节）= 101 字节。
      fireEvent.change(screen.getByLabelText("名称"), {
        target: { value: `${"x".repeat(96)}&` },
      });
      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(screen.getByText(NAME_ERROR)).toBeVisible();

      expect(create).not.toHaveBeenCalled();
    });

    it.each([
      ["33 个汉字（99 字节）", "中".repeat(33)],
      ["100 个 ASCII 字符", "x".repeat(100)],
      ["95 个 x + `&`（转义后恰好 100 字节）", `${"x".repeat(95)}&`],
    ])("accepts %s", async (_label, value) => {
      const create = vi
        .spyOn(we2aiApi, "createKey")
        .mockResolvedValue(created());
      const user = userEvent.setup();
      await renderDialog(null);

      fireEvent.change(screen.getByLabelText("名称"), { target: { value } });
      await user.click(screen.getByRole("button", { name: "创建" }));

      await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
      expect(create.mock.calls[0][0].name).toBe(value);
    });

    it("rejects an invalid quota", async () => {
      const create = vi.spyOn(we2aiApi, "createKey");
      const user = userEvent.setup();
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.type(screen.getByLabelText("额度上限（美元）"), "-3");
      await user.click(screen.getByRole("button", { name: "创建" }));

      expect(await screen.findByText("额度需为不小于 0 的数字")).toBeVisible();
      await user.clear(screen.getByLabelText("额度上限（美元）"));
      await user.type(screen.getByLabelText("额度上限（美元）"), "abc");
      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(screen.getByText("额度需为不小于 0 的数字")).toBeVisible();
      expect(create).not.toHaveBeenCalled();
    });

    it("sends the mapped fields with a UUID idempotency key and the first group by default", async () => {
      const result = created();
      const create = vi.spyOn(we2aiApi, "createKey").mockResolvedValue(result);
      const user = userEvent.setup();
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "  我的 Key  ");
      await user.type(screen.getByLabelText("额度上限（美元）"), "10.5");
      await user.click(screen.getByRole("button", { name: "30 天" }));
      await user.click(screen.getByRole("button", { name: "创建" }));

      await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
      const input = create.mock.calls[0][0];
      expect(input.idempotencyKey).toMatch(UUID_RE);
      expect(input).toMatchObject({
        name: "我的 Key",
        groupId: 7,
        quota: 10.5,
        expiresInDays: 30,
      });
      expect(onCreated).toHaveBeenCalledWith(result);
    });

    it("omits quota and expiry for unlimited and never-expiring keys", async () => {
      const create = vi
        .spyOn(we2aiApi, "createKey")
        .mockResolvedValue(created());
      const user = userEvent.setup();
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));

      await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
      const input = create.mock.calls[0][0];
      expect(input).not.toHaveProperty("quota");
      expect(input).not.toHaveProperty("expiresInDays");
    });

    it("lets the user pick another group, listing the rate when it is not 1", async () => {
      const create = vi
        .spyOn(we2aiApi, "createKey")
        .mockResolvedValue(created());
      const user = userEvent.setup();
      await renderDialog(null);

      await user.click(screen.getByRole("combobox", { name: "分组" }));
      expect(
        await screen.findByRole("option", { name: "优惠组 · ×0.8" }),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("option", { name: "默认分组" }),
      ).toBeInTheDocument();
      await user.click(screen.getByRole("option", { name: "优惠组 · ×0.8" }));
      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));

      await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
      expect(create.mock.calls[0][0].groupId).toBe(8);
    });

    it("reuses the idempotency key when the same submission is retried and renews it when the payload changes", async () => {
      const create = vi
        .spyOn(we2aiApi, "createKey")
        .mockRejectedValueOnce(apiError("TRANSIENT"))
        .mockRejectedValueOnce(apiError("TRANSIENT"))
        .mockResolvedValue(created());
      const user = userEvent.setup();
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
        "网络连接失败",
      );
      await user.click(screen.getByRole("button", { name: "创建" }));
      await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
      expect(create.mock.calls[1][0].idempotencyKey).toBe(
        create.mock.calls[0][0].idempotencyKey,
      );

      // 改了表单内容：同一个键配不同载荷会被服务端拒绝，必须换新键。
      await user.type(screen.getByLabelText("名称"), "B");
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "创建" })).toBeEnabled(),
      );
      await user.click(screen.getByRole("button", { name: "创建" }));
      await waitFor(() => expect(create).toHaveBeenCalledTimes(3));
      expect(create.mock.calls[2][0].name).toBe("AB");
      expect(create.mock.calls[2][0].idempotencyKey).toMatch(UUID_RE);
      expect(create.mock.calls[2][0].idempotencyKey).not.toBe(
        create.mock.calls[0][0].idempotencyKey,
      );
    });

    it("generates a fresh idempotency key every time the dialog is opened", async () => {
      const create = vi
        .spyOn(we2aiApi, "createKey")
        .mockResolvedValue(created());
      const user = userEvent.setup();

      const first = await renderDialog(null);
      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));
      await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
      // 卸载后重新打开（同样的表单内容）。
      first.unmount();
      await renderDialog(null);
      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));
      await waitFor(() => expect(create).toHaveBeenCalledTimes(2));

      expect(create.mock.calls[1][0].idempotencyKey).not.toBe(
        create.mock.calls[0][0].idempotencyKey,
      );
    });

    it("turns a custom date into expiresInDays that covers the whole chosen day", async () => {
      vi.useFakeTimers({ toFake: ["Date"] });
      vi.setSystemTime(new Date(2026, 9, 1, 10, 0, 0));
      const create = vi
        .spyOn(we2aiApi, "createKey")
        .mockResolvedValue(created());
      const user = userEvent.setup({ advanceTimers: () => {} });
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "自定义日期" }));
      fireEvent.change(screen.getByLabelText("自定义日期"), {
        target: { value: "2026-10-11" },
      });
      await user.click(screen.getByRole("button", { name: "创建" }));

      await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
      // 10-01 10:00 → 10-11 23:59:59 = 10 天 14 小时，向上取整为 11 天。
      expect(create.mock.calls[0][0].expiresInDays).toBe(11);
    });

    it("requires a date for the custom expiry and rejects past dates", async () => {
      vi.useFakeTimers({ toFake: ["Date"] });
      vi.setSystemTime(new Date(2026, 9, 1, 10, 0, 0));
      const create = vi.spyOn(we2aiApi, "createKey");
      const user = userEvent.setup({ advanceTimers: () => {} });
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "自定义日期" }));
      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(await screen.findByText("请选择到期日期")).toBeVisible();

      fireEvent.change(screen.getByLabelText("自定义日期"), {
        target: { value: "2026-09-30" },
      });
      await user.click(screen.getByRole("button", { name: "创建" }));
      expect(await screen.findByText("到期日期需晚于现在")).toBeVisible();
      expect(create).not.toHaveBeenCalled();
    });

    it("maps server errors to readable messages and asks the shell to re-check the session", async () => {
      vi.spyOn(we2aiApi, "createKey").mockRejectedValue(
        apiError("API_KEY_COUNT_EXCEEDED"),
      );
      const user = userEvent.setup();
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));

      expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
        "Key 数量已达上限",
      );
      expect(onSessionMaybeEnded).toHaveBeenCalled();
      expect(onCreated).not.toHaveBeenCalled();
    });

    it.each(["IDEMPOTENCY_IN_PROGRESS", "IDEMPOTENCY_RETRY_BACKOFF"])(
      "%s shows the in-progress hint and does not trigger a session re-check",
      async (code) => {
        vi.spyOn(we2aiApi, "createKey").mockRejectedValue(apiError(code));
        const user = userEvent.setup();
        await renderDialog(null);

        await user.type(screen.getByLabelText("名称"), "A");
        await user.click(screen.getByRole("button", { name: "创建" }));

        expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
          "正在处理，请稍候重试",
        );
        expect(onSessionMaybeEnded).not.toHaveBeenCalled();
      },
    );

    it("SESSION_CHANGED explains the previous action may have taken effect", async () => {
      vi.spyOn(we2aiApi, "createKey").mockRejectedValue(
        apiError("SESSION_CHANGED"),
      );
      const user = userEvent.setup();
      await renderDialog(null);

      await user.type(screen.getByLabelText("名称"), "A");
      await user.click(screen.getByRole("button", { name: "创建" }));

      expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
        "会话已切换，原操作可能已生效，请刷新后确认结果",
      );
      expect(onSessionMaybeEnded).toHaveBeenCalled();
    });

    it("explains that custom dates are counted in whole days (create only)", async () => {
      const user = userEvent.setup();
      await renderDialog(null);
      expect(
        screen.queryByText(/按天计算，实际到期时间可能晚于所选日期不足 1 天/),
      ).not.toBeInTheDocument();

      await user.click(screen.getByRole("button", { name: "自定义日期" }));

      expect(
        screen.getByText(/按天计算，实际到期时间可能晚于所选日期不足 1 天/),
      ).toBeVisible();
    });

    it("shows a retry when groups fail to load and still lets the user create without a group", async () => {
      vi.spyOn(we2aiApi, "listKeyGroups")
        .mockReset()
        .mockRejectedValueOnce(apiError("TRANSIENT"))
        .mockResolvedValue(GROUPS);
      const user = userEvent.setup();
      await act(async () => {
        render(
          <KeyEditDialog
            t={t}
            keyItem={null}
            onClose={onClose}
            onCreated={onCreated}
            onUpdated={onUpdated}
            onResultUnknown={onResultUnknown}
            onSessionMaybeEnded={onSessionMaybeEnded}
          />,
        );
      });

      expect(await screen.findByText("分组加载失败")).toBeVisible();
      await user.click(screen.getByRole("button", { name: "重试" }));
      await waitFor(() =>
        expect(screen.queryByText("分组加载失败")).not.toBeInTheDocument(),
      );
    });
  });

  describe("edit", () => {
    it("prefills the form from the key", async () => {
      await renderDialog(makeKey());

      expect(screen.getByLabelText("名称")).toHaveValue("工作");
      expect(screen.getByLabelText("额度上限（美元）")).toHaveValue("10");
      expect(screen.getByRole("switch", { name: "启用" })).toBeChecked();
      expect(
        screen.getByLabelText(/重置已用额度（当前已用 \$3\.20）/),
      ).not.toBeChecked();
      // 有过期时间：显示为自定义日期，且不是预设按钮高亮。
      expect(
        screen.getByRole("button", { name: "自定义日期" }),
      ).toHaveAttribute("aria-pressed", "true");
    });

    it("closes without a request when nothing changed", async () => {
      const update = vi.spyOn(we2aiApi, "updateKey");
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(onClose).toHaveBeenCalled());
      expect(update).not.toHaveBeenCalled();
    });

    it("sends only the changed fields", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey({ name: "改名" }));
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.clear(screen.getByLabelText("名称"));
      await user.type(screen.getByLabelText("名称"), "改名");
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
      expect(update).toHaveBeenCalledWith(5, { name: "改名" });
      expect(onUpdated).toHaveBeenCalled();
    });

    it("never clears an unparseable expiry unless 永久 is clicked", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey({ name: "改名" }));
      const user = userEvent.setup();
      await renderDialog(makeKey({ expiresAt: "not-a-date" }));

      await user.clear(screen.getByLabelText("名称"));
      await user.type(screen.getByLabelText("名称"), "改名");
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
      expect(update).toHaveBeenCalledWith(5, { name: "改名" });
    });

    it("clears an unparseable expiry when 永久 is clicked explicitly", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey({ expiresAt: null }));
      const user = userEvent.setup();
      await renderDialog(makeKey({ expiresAt: "not-a-date" }));

      await user.click(screen.getByRole("button", { name: "永久" }));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
      expect(update).toHaveBeenCalledWith(5, { expiresAt: "" });
    });

    it("maps group, quota, status and reset-quota changes", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.click(screen.getByRole("combobox", { name: "分组" }));
      await user.click(
        await screen.findByRole("option", { name: "优惠组 · ×0.8" }),
      );
      await user.clear(screen.getByLabelText("额度上限（美元）"));
      await user.type(screen.getByLabelText("额度上限（美元）"), "25");
      await user.click(screen.getByRole("switch", { name: "启用" }));
      await user.click(screen.getByLabelText(/重置已用额度/));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
      expect(update).toHaveBeenCalledWith(5, {
        groupId: 8,
        quota: 25,
        status: "inactive",
        resetQuota: true,
      });
    });

    it("sends quota 0 when the quota field is cleared (unlimited)", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.clear(screen.getByLabelText("额度上限（美元）"));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledWith(5, { quota: 0 }));
    });

    it("clears the expiry with an empty string when switched to never", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey({ expiresAt: null }));
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.click(screen.getByRole("button", { name: "永久" }));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() =>
        expect(update).toHaveBeenCalledWith(5, { expiresAt: "" }),
      );
    });

    it("sends an RFC3339 timestamp for preset and custom expiry choices", async () => {
      vi.useFakeTimers({ toFake: ["Date"] });
      vi.setSystemTime(new Date(Date.UTC(2026, 9, 1, 0, 0, 0)));
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup({ advanceTimers: () => {} });
      await renderDialog(makeKey({ expiresAt: null }));

      await user.click(screen.getByRole("button", { name: "30 天" }));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
      expect(update).toHaveBeenCalledWith(5, {
        expiresAt: "2026-10-31T00:00:00.000Z",
      });
    });

    it("lets an already expired key be saved with only a name change, without submitting expires_at", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup();
      await renderDialog(
        makeKey({
          name: "旧",
          status: "expired",
          expiresAt: new Date(Date.now() - 3 * DAY_MS).toISOString(),
        }),
      );

      await user.type(screen.getByLabelText("名称"), "新");
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
      expect(update).toHaveBeenCalledWith(5, { name: "旧新" });
      expect(update.mock.calls[0][1]).not.toHaveProperty("expiresAt");
      expect(screen.queryByText("到期日期需晚于现在")).not.toBeInTheDocument();
    });

    it("still rejects a past date when an expired key's date is actively changed", async () => {
      const update = vi.spyOn(we2aiApi, "updateKey");
      const user = userEvent.setup();
      await renderDialog(
        makeKey({
          expiresAt: new Date(Date.now() - 3 * DAY_MS).toISOString(),
        }),
      );

      fireEvent.change(screen.getByLabelText("自定义日期"), {
        target: { value: "2000-01-01" },
      });
      await user.click(screen.getByRole("button", { name: "保存" }));

      expect(await screen.findByText("到期日期需晚于现在")).toBeVisible();
      expect(update).not.toHaveBeenCalled();
    });

    it("an expired key can be extended with a preset", async () => {
      vi.useFakeTimers({ toFake: ["Date"] });
      vi.setSystemTime(new Date(Date.UTC(2026, 9, 1, 0, 0, 0)));
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup({ advanceTimers: () => {} });
      await renderDialog(
        makeKey({
          status: "expired",
          expiresAt: new Date(Date.UTC(2026, 8, 1)).toISOString(),
        }),
      );

      await user.click(screen.getByRole("button", { name: "30 天" }));
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() =>
        expect(update).toHaveBeenCalledWith(5, {
          expiresAt: "2026-10-31T00:00:00.000Z",
        }),
      );
    });

    it("keeps the exact original expiry when the custom date is left untouched", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup();
      await renderDialog(makeKey({ name: "旧" }));

      await user.type(screen.getByLabelText("名称"), "新");
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() =>
        expect(update).toHaveBeenCalledWith(5, { name: "旧新" }),
      );
    });

    it("shows the recovery hint for keys that ran out of quota or expired and only sends status when switched on", async () => {
      const update = vi
        .spyOn(we2aiApi, "updateKey")
        .mockResolvedValue(makeKey());
      const user = userEvent.setup();
      await renderDialog(
        makeKey({ status: "quota_exhausted", quota: 5, quotaUsed: 5 }),
      );

      expect(
        screen.getByText(/提高额度或延长有效期后会自动恢复/),
      ).toBeVisible();
      expect(screen.getByRole("switch", { name: "启用" })).not.toBeChecked();

      // 只改额度，不碰开关：不应发送 status（服务端提高额度时自动恢复）。
      await user.clear(screen.getByLabelText("额度上限（美元）"));
      await user.type(screen.getByLabelText("额度上限（美元）"), "20");
      await user.click(screen.getByRole("button", { name: "保存" }));

      await waitFor(() =>
        expect(update).toHaveBeenCalledWith(5, { quota: 20 }),
      );
    });

    it("keeps the current group as an option even when it is no longer bindable and does not offer 'no group'", async () => {
      vi.spyOn(we2aiApi, "listKeyGroups").mockResolvedValue([GROUPS[1]]);
      const user = userEvent.setup();
      await renderDialog(
        makeKey({ group: { id: 99, name: "旧分组", rate: 1.5 } }),
      );

      await user.click(screen.getByRole("combobox", { name: "分组" }));
      expect(
        await screen.findByRole("option", { name: "旧分组 · ×1.5" }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("option", { name: "不指定分组" }),
      ).not.toBeInTheDocument();
    });

    it.each(["TRANSIENT", "NETWORK_ERROR"])(
      "a reset-quota update that fails with %s has an unknown result: no retry in the dialog, the parent is told",
      async (code) => {
        const update = vi
          .spyOn(we2aiApi, "updateKey")
          .mockRejectedValue(apiError(code));
        const user = userEvent.setup();
        await renderDialog(makeKey());

        await user.click(screen.getByLabelText(/重置已用额度/));
        await user.click(screen.getByRole("button", { name: "保存" }));

        await waitFor(() => expect(onResultUnknown).toHaveBeenCalledTimes(1));
        expect(update).toHaveBeenCalledWith(5, { resetQuota: true });
        expect(update).toHaveBeenCalledTimes(1);
        // 不在弹窗里展示可直接重试的网络错误。
        expect(screen.queryByTestId("key-edit-error")).not.toBeInTheDocument();
      },
    );

    it("a definite failure of a reset-quota update is shown normally, not as unknown", async () => {
      vi.spyOn(we2aiApi, "updateKey").mockRejectedValue(
        apiError("API_KEY_NOT_FOUND"),
      );
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.click(screen.getByLabelText(/重置已用额度/));
      await user.click(screen.getByRole("button", { name: "保存" }));

      expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
        "这个 Key 已不存在",
      );
      expect(onResultUnknown).not.toHaveBeenCalled();
    });

    it("a transient failure of a regular update stays a normal retryable error", async () => {
      vi.spyOn(we2aiApi, "updateKey").mockRejectedValue(apiError("TRANSIENT"));
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.clear(screen.getByLabelText("名称"));
      await user.type(screen.getByLabelText("名称"), "x");
      await user.click(screen.getByRole("button", { name: "保存" }));

      expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
        "网络连接失败",
      );
      expect(onResultUnknown).not.toHaveBeenCalled();
    });

    it("surfaces update errors", async () => {
      vi.spyOn(we2aiApi, "updateKey").mockRejectedValue(
        apiError("GROUP_NOT_ALLOWED"),
      );
      const user = userEvent.setup();
      await renderDialog(makeKey());

      await user.clear(screen.getByLabelText("名称"));
      await user.type(screen.getByLabelText("名称"), "x");
      await user.click(screen.getByRole("button", { name: "保存" }));

      expect(await screen.findByTestId("key-edit-error")).toHaveTextContent(
        "没有权限使用所选分组",
      );
      expect(onUpdated).not.toHaveBeenCalled();
    });
  });
});
