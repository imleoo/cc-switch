import { StrictMode } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { we2aiApi, type We2aiKeyModels } from "@/we2ai/api";
import { ModelSquarePage } from "@/we2ai/ModelSquarePage";
import { getWe2aiStrings } from "@/we2ai/strings";

const t = getWe2aiStrings("zh");

// Radix Select 打开时会滚动到选中项，jsdom 没有实现 scrollIntoView。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

const claudeKey = {
  id: 1,
  name: "工作",
  groupName: "Claude 组",
  status: "active",
  maskedKey: "sk-we2…1111",
};
const openaiKey = {
  id: 3,
  name: "个人",
  groupName: "OpenAI 组",
  status: "active",
  maskedKey: "sk-we2…3333",
};

const claudeModels: We2aiKeyModels = {
  models: [
    {
      id: "claude-sonnet-4-5",
      provider: "anthropic",
      tools: ["claude_code", "workbuddy"],
    },
  ],
  callable: true,
  blockedReason: null,
};
const openaiModels: We2aiKeyModels = {
  models: [{ id: "gpt-5", provider: null, tools: ["codex"] }],
  callable: true,
  blockedReason: null,
};

function renderPage(onSessionMaybeEnded = vi.fn()) {
  render(<ModelSquarePage t={t} onSessionMaybeEnded={onSessionMaybeEnded} />);
  return onSessionMaybeEnded;
}

describe("ModelSquarePage", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("auto-selects a single key and renders its models with tool buttons", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
      keys: [claudeKey],
      selectedKeyId: 1,
    });
    const keyModels = vi
      .spyOn(we2aiApi, "keyModels")
      .mockResolvedValue(claudeModels);
    renderPage();

    const card = await screen.findByTestId("we2ai-model-card");
    expect(keyModels).toHaveBeenCalledWith(1);
    expect(screen.getByTestId("we2ai-single-key")).toHaveTextContent(
      "工作 · Claude 组",
    );
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(within(card).getByText("claude-sonnet-4-5")).toBeInTheDocument();
    expect(
      within(card).getByRole("button", { name: "Claude Code" }),
    ).toBeInTheDocument();
    expect(
      within(card).getByRole("button", { name: "WorkBuddy" }),
    ).toBeInTheDocument();
    expect(
      within(card).queryByRole("button", { name: "Codex" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows different models and buttons per key and remembers the choice", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
      keys: [claudeKey, openaiKey],
      selectedKeyId: 1,
    });
    vi.spyOn(we2aiApi, "keyModels").mockImplementation(async (keyId) =>
      keyId === 1 ? claudeModels : openaiModels,
    );
    const selectKey = vi.spyOn(we2aiApi, "selectKey").mockResolvedValue();
    renderPage();

    await screen.findByText("claude-sonnet-4-5");
    await userEvent.click(screen.getByRole("combobox", { name: t.keyLabel }));
    await userEvent.click(
      await screen.findByRole("option", { name: "个人 · OpenAI 组" }),
    );

    await screen.findByText("gpt-5");
    expect(screen.queryByText("claude-sonnet-4-5")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Codex" })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Claude Code" }),
    ).not.toBeInTheDocument();
    expect(selectKey).toHaveBeenCalledWith(3);
  });

  it("ignores a slow model response for a key that is no longer selected", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
      keys: [claudeKey, openaiKey],
      selectedKeyId: 1,
    });
    let resolveFirst: ((value: We2aiKeyModels) => void) | undefined;
    vi.spyOn(we2aiApi, "keyModels").mockImplementation((keyId) =>
      keyId === 1
        ? new Promise((resolve) => {
            resolveFirst = resolve;
          })
        : Promise.resolve(openaiModels),
    );
    vi.spyOn(we2aiApi, "selectKey").mockResolvedValue();
    renderPage();

    await waitFor(() => expect(resolveFirst).toBeDefined());
    await userEvent.click(screen.getByRole("combobox", { name: t.keyLabel }));
    await userEvent.click(
      await screen.findByRole("option", { name: "个人 · OpenAI 组" }),
    );
    await screen.findByText("gpt-5");

    resolveFirst!(claudeModels);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByText("claude-sonnet-4-5")).not.toBeInTheDocument();
    expect(screen.getByText("gpt-5")).toBeInTheDocument();
  });

  it("keeps the latest key list when an earlier list request resolves last", async () => {
    // StrictMode 下挂载时 effect 会执行两次，产生两次并发的 Key 列表请求。
    const resolvers: Array<(value: unknown) => void> = [];
    vi.spyOn(we2aiApi, "listKeys").mockImplementation(
      () =>
        new Promise((resolve) => {
          resolvers.push(resolve as (value: unknown) => void);
        }),
    );
    vi.spyOn(we2aiApi, "keyModels").mockImplementation(async (keyId) =>
      keyId === 1 ? claudeModels : openaiModels,
    );
    render(
      <StrictMode>
        <ModelSquarePage t={t} onSessionMaybeEnded={vi.fn()} />
      </StrictMode>,
    );
    await waitFor(() => expect(resolvers).toHaveLength(2));

    resolvers[1]({ keys: [openaiKey], selectedKeyId: 3 });
    await screen.findByText("gpt-5");
    resolvers[0]({ keys: [claudeKey], selectedKeyId: 1 });
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.getByTestId("we2ai-single-key")).toHaveTextContent("个人");
    expect(screen.queryByText("claude-sonnet-4-5")).not.toBeInTheDocument();
  });

  it("re-fetches models on refresh even when the selected key is unchanged", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
      keys: [claudeKey],
      selectedKeyId: 1,
    });
    const keyModels = vi
      .spyOn(we2aiApi, "keyModels")
      .mockResolvedValueOnce({
        ...claudeModels,
        callable: false,
        blockedReason: "API_KEY_QUOTA_EXHAUSTED",
      })
      .mockResolvedValueOnce(claudeModels);
    renderPage();

    await screen.findByText(t.keyBlockedQuotaExhausted);
    await userEvent.click(screen.getByRole("button", { name: t.refresh }));
    await waitFor(() =>
      expect(
        screen.queryByText(t.keyBlockedQuotaExhausted),
      ).not.toBeInTheDocument(),
    );
    expect(keyModels).toHaveBeenCalledTimes(2);
  });

  it("retries once when the latest key list request is reported as superseded", async () => {
    vi.spyOn(we2aiApi, "listKeys")
      .mockRejectedValueOnce({ code: "KEY_LIST_SUPERSEDED", message: "old" })
      .mockResolvedValueOnce({ keys: [claudeKey], selectedKeyId: 1 });
    vi.spyOn(we2aiApi, "keyModels").mockResolvedValue(claudeModels);
    const onSessionMaybeEnded = renderPage();

    await screen.findByText("claude-sonnet-4-5");
    expect(onSessionMaybeEnded).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("asks the shell to re-check the session on unrecognized errors", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockRejectedValue({
      code: "UNKNOWN_401",
      message: "unauthorized",
    });
    const onSessionMaybeEnded = renderPage();

    await screen.findByRole("alert");
    expect(onSessionMaybeEnded).toHaveBeenCalledTimes(1);
  });

  it("explains why a key cannot be called", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
      keys: [{ ...claudeKey, status: "quota_exhausted" }],
      selectedKeyId: 1,
    });
    vi.spyOn(we2aiApi, "keyModels").mockResolvedValue({
      ...claudeModels,
      callable: false,
      blockedReason: "API_KEY_QUOTA_EXHAUSTED",
    });
    renderPage();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      t.keyBlockedQuotaExhausted,
    );
    expect(screen.getByRole("button", { name: "Claude Code" })).toBeDisabled();
  });

  it("tells the user when the account has no usable keys", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
      keys: [],
      selectedKeyId: null,
    });
    const keyModels = vi.spyOn(we2aiApi, "keyModels");
    renderPage();

    await screen.findByText(t.noKeys);
    expect(keyModels).not.toHaveBeenCalled();
  });

  it("hands session-ending errors back to the shell and offers a retry", async () => {
    const listKeys = vi
      .spyOn(we2aiApi, "listKeys")
      .mockRejectedValueOnce({ code: "TOKEN_REVOKED", message: "revoked" })
      .mockResolvedValueOnce({ keys: [claudeKey], selectedKeyId: 1 });
    vi.spyOn(we2aiApi, "keyModels").mockResolvedValue(claudeModels);
    const onSessionMaybeEnded = renderPage();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      t.errorTokenRevoked,
    );
    expect(onSessionMaybeEnded).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole("button", { name: t.offlineRetry }));
    await screen.findByText("claude-sonnet-4-5");
    expect(listKeys).toHaveBeenCalledTimes(2);
  });

  it("does not treat a network error as the end of the session", async () => {
    vi.spyOn(we2aiApi, "listKeys").mockRejectedValue({
      code: "TRANSIENT",
      message: "offline",
    });
    const onSessionMaybeEnded = renderPage();

    expect(await screen.findByRole("alert")).toHaveTextContent(t.errorNetwork);
    expect(onSessionMaybeEnded).not.toHaveBeenCalled();
  });
});
