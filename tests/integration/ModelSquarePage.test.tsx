import { StrictMode } from "react";
import { act, render, screen, waitFor, within } from "@testing-library/react";
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
      price: null,
    },
  ],
  callable: true,
  blockedReason: null,
  pricing: null,
};
const openaiModels: We2aiKeyModels = {
  models: [{ id: "gpt-5", provider: null, tools: ["codex"], price: null }],
  callable: true,
  blockedReason: null,
  pricing: null,
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

  describe("grouped discount pricing (B1 pricing extension)", () => {
    async function renderWithModels(modelsResponse: We2aiKeyModels) {
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      vi.spyOn(we2aiApi, "keyModels").mockResolvedValue(modelsResponse);
      renderPage();
      return screen.findByTestId("we2ai-model-card");
    }

    it("shows the discounted price and CNY conversion, with a strikethrough base price", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
            price: {
              billingMode: "token",
              input: 3,
              output: 15,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: null,
              baseInput: 6,
              baseOutput: 30,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: null,
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 0.5,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 0.5,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-value-input"),
      ).toHaveTextContent("¥21.60 / $3.00");
      expect(
        within(card).getByTestId("we2ai-price-strikethrough-input"),
      ).toHaveTextContent("¥43.20 / $6.00");
      expect(
        within(card).getByTestId("we2ai-price-value-output"),
      ).toHaveTextContent("¥108.00 / $15.00");
      expect(within(card).getByText(t.priceInput)).toBeInTheDocument();
      expect(within(card).getByText(t.priceOutput)).toBeInTheDocument();
      expect(
        within(card).queryByTestId("we2ai-price-row-cacheRead"),
      ).not.toBeInTheDocument();
      expect(within(card).getByText(t.priceFootnote)).toBeInTheDocument();
      expect(
        within(card).queryByTestId("we2ai-price-peak-active"),
      ).not.toBeInTheDocument();
    });

    it("hides the strikethrough price when the effective multiplier is 1", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
            price: {
              billingMode: "token",
              input: 3,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: null,
              baseInput: 3,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: null,
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 1,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-value-input"),
      ).toHaveTextContent("¥21.60 / $3.00");
      expect(
        within(card).queryByTestId("we2ai-price-strikethrough-input"),
      ).not.toBeInTheDocument();
    });

    it("labels the peak-hour price when peakActive is true", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
            price: {
              billingMode: "token",
              input: 5,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: null,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: null,
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1.5,
          peakActive: true,
          effectiveMultiplier: 1.5,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-peak-active"),
      ).toHaveTextContent(t.pricePeakActive);
    });

    // Opus 复核 Q2 正向对照：非 token 计费但没有自己 multiplier 的模型，
    // 说明它跟 token 类模型一样是按顶层倍率计价的，高峰标注应该照常显示。
    it("still labels the peak-hour price for a non-token model that has no multiplier of its own", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "gpt-5-image",
            provider: "openai",
            tools: ["codex"],
            price: {
              billingMode: "image",
              input: null,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: 0.04,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: "request",
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1.5,
          peakActive: true,
          effectiveMultiplier: 1.5,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-peak-active"),
      ).toHaveTextContent(t.pricePeakActive);
    });

    it("shows a single per-request row with the per-request unit label", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "gpt-5-image",
            provider: "openai",
            tools: ["codex"],
            price: {
              billingMode: "per_request",
              input: null,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: 0.04,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: "request",
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 1,
          unit: "usd_per_request",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-value-perRequest"),
      ).toHaveTextContent("¥0.288 / $0.04");
      expect(within(card).getByText(t.priceUnitPerRequest)).toBeInTheDocument();
      expect(
        within(card).queryByText(t.priceUnitPerMillionTokens),
      ).not.toBeInTheDocument();
    });

    // v3 契约：视频类模型按秒计费，单位显示"每秒"而不是"每次"。
    it("shows the per-second unit for a video model billed by the second", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "video-gen-1",
            provider: "openai",
            tools: ["codex"],
            price: {
              billingMode: "video",
              input: null,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: 0.5,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: "second",
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 1,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(within(card).getByText(t.priceUnitPerSecond)).toBeInTheDocument();
      expect(
        within(card).queryByText(t.priceUnitPerRequest),
      ).not.toBeInTheDocument();
      // Opus 复核 R1：标签用"按秒"，不能沿用"按次"（那样会读成"按次 …
      // 每秒"这种自相矛盾的组合）。
      expect(within(card).getByText(t.pricePerSecond)).toBeInTheDocument();
      expect(
        within(card).queryByText(t.pricePerRequest),
      ).not.toBeInTheDocument();
    });

    // v3 契约：未识别的 per_request_unit 不展示按次这一行（宁可"暂无
    // 定价"，也不能展示错误单位）。
    it("shows \"no pricing\" instead of a wrong unit when perRequestUnit is unrecognized", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "mystery-model",
            provider: "openai",
            tools: ["codex"],
            price: {
              billingMode: "per_request",
              input: null,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: 0.5,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: null,
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 1,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(within(card).getByText(t.priceUnavailable)).toBeInTheDocument();
    });

    // 追加需求：peak_active 且该模型自己的 multiplier 与顶层 effectiveMultiplier
    // 数值相同（在容差内），说明它确实叠加了分组高峰——即使 billingMode
    // 不是 "token"，也要显示"高峰价"（这正是取代 Q2 里 billingMode 判定的
    // 场景：普通 per_request 模型一样可能叠加分组高峰）。
    it("labels the peak-hour price for a per_request model whose own multiplier matches the top-level one", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "gpt-5-image",
            provider: "openai",
            tools: ["codex"],
            price: {
              billingMode: "per_request",
              input: null,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: 0.04,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: 1.5,
              perRequestUnit: "request",
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1.5,
          peakActive: true,
          effectiveMultiplier: 1.5,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-peak-active"),
      ).toHaveTextContent(t.pricePeakActive);
    });

    it('shows "no pricing available" when the model has no price data', async () => {
      const card = await renderWithModels({
        models: [
          { id: "claude-sonnet-4-5", provider: "anthropic", tools: [], price: null },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 1,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(within(card).getByText(t.priceUnavailable)).toBeInTheDocument();
    });

    it("shows no price area at all when the key has no pricing", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: [],
            price: null,
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: null,
      });

      expect(
        within(card).queryByTestId("we2ai-model-price"),
      ).not.toBeInTheDocument();
    });

    // Opus 复核 P5/v2 契约：image 类模型自带独立倍率（0.5），与顶层因分组
    // 高峰而升高的倍率（2）不同——加价标注必须用模型自己的倍率，而不是
    // 被顶层的高峰倍率误判成"×2 倍率"。
    it("labels the model's own multiplier badge (not the top-level one) for a surcharged image model", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "image-gen-1",
            provider: "openai",
            tools: ["codex"],
            price: {
              billingMode: "image",
              input: null,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: 0.02,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: 0.01,
              multiplier: 2,
              perRequestUnit: "request",
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 4,
          peakActive: true,
          effectiveMultiplier: 4,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-multiplier-badge"),
      ).toHaveTextContent("×2");
      expect(
        within(card).queryByTestId("we2ai-price-strikethrough-perRequest"),
      ).not.toBeInTheDocument();
      expect(within(card).getByText(t.pricePerRequest)).toBeInTheDocument();
      // Opus 复核 Q2：这个模型有自己独立的 multiplier（image 类不叠加分组
      // 高峰），即使顶层 `pricing.peakActive` 是 true，也不该跟着标"高峰
      // 价"——那只对按顶层倍率计价的模型（token 类，或没有自己 multiplier
      // 的模型）成立。
      expect(
        within(card).queryByTestId("we2ai-price-peak-active"),
      ).not.toBeInTheDocument();
    });

    // Opus 复核 P7：划线原价用 <del>，且带屏幕阅读器专用前缀。
    it("marks the strikethrough price with <del> and screen-reader-only prefixes", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
            price: {
              billingMode: "token",
              input: 3,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: null,
              baseInput: 6,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: null,
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 0.5,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 0.5,
          unit: "usd_per_1m_tokens",
        },
      });

      const strikethrough = within(card).getByTestId(
        "we2ai-price-strikethrough-input",
      );
      expect(strikethrough.tagName).toBe("DEL");
      expect(strikethrough).toHaveTextContent(t.priceOriginalSrLabel);
      expect(
        within(card).getByTestId("we2ai-price-value-input"),
      ).toHaveTextContent(t.priceDiscountedSrLabel);
    });

    // Opus 复核 P3：价格区页脚展示这批数据的拉取时间。
    it("shows the fetch time in the footer once models have loaded", async () => {
      const card = await renderWithModels({
        models: [
          {
            id: "claude-sonnet-4-5",
            provider: "anthropic",
            tools: ["claude_code"],
            price: {
              billingMode: "token",
              input: 3,
              output: null,
              cacheRead: null,
              cacheWrite: null,
              cacheWrite1h: null,
              perRequest: null,
              baseInput: null,
              baseOutput: null,
              baseCacheRead: null,
              baseCacheWrite: null,
              baseCacheWrite1h: null,
              basePerRequest: null,
              multiplier: null,
              perRequestUnit: null,
            },
          },
        ],
        callable: true,
        blockedReason: null,
        pricing: {
          cnyRate: 7.2,
          rateMultiplier: 1,
          peakMultiplier: 1,
          peakActive: false,
          effectiveMultiplier: 1,
          unit: "usd_per_1m_tokens",
        },
      });

      expect(
        within(card).getByTestId("we2ai-price-fetched-at"),
      ).toBeInTheDocument();
    });
  });

  // Opus 复核 P3：价格会随高峰/峰谷边界变化，验证过期时静默重拉，不打断
  // 已展示的内容；用假定时器而不是真实等待，加速这几个跨越 60 秒/5 分钟
  // 的场景。
  describe("model price freshness (fake timers)", () => {
    afterEach(() => {
      vi.useRealTimers();
      Object.defineProperty(document, "hidden", {
        value: false,
        configurable: true,
      });
    });

    it("does not refetch on focus before 60 seconds have passed, but does after", async () => {
      vi.useFakeTimers();
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      const keyModels = vi
        .spyOn(we2aiApi, "keyModels")
        .mockResolvedValue(claudeModels);

      render(<ModelSquarePage t={t} onSessionMaybeEnded={vi.fn()} />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(keyModels).toHaveBeenCalledTimes(1);
      expect(screen.getByText("claude-sonnet-4-5")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
        window.dispatchEvent(new Event("focus"));
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(keyModels).toHaveBeenCalledTimes(1);

      // 累计 61 秒后再次获得焦点：静默重拉一次，已展示的模型不被打断。
      await act(async () => {
        await vi.advanceTimersByTimeAsync(31_000);
        window.dispatchEvent(new Event("focus"));
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(keyModels).toHaveBeenCalledTimes(2);
      expect(screen.getByText("claude-sonnet-4-5")).toBeInTheDocument();
    });

    it("refetches every 5 minutes while the page stays visible", async () => {
      vi.useFakeTimers();
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      const keyModels = vi
        .spyOn(we2aiApi, "keyModels")
        .mockResolvedValue(claudeModels);

      render(<ModelSquarePage t={t} onSessionMaybeEnded={vi.fn()} />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(keyModels).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5 * 60_000);
      });
      expect(keyModels).toHaveBeenCalledTimes(2);
    });

    // Opus 复核第 2 轮 Q6：静默刷新遇到会话终止类错误也要通知外壳复查
    // 会话状态；纯网络错误（TRANSIENT/NETWORK_ERROR）不算——那只是这一次
    // 静默刷新恰好失败，不代表会话已经失效。
    it("calls onSessionMaybeEnded when a silent refresh fails with a session-ending error (Q6)", async () => {
      vi.useFakeTimers();
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      vi.spyOn(we2aiApi, "keyModels")
        .mockResolvedValueOnce(claudeModels)
        .mockRejectedValue({ code: "TOKEN_REVOKED", message: "revoked" });
      const onSessionMaybeEnded = vi.fn();

      render(
        <ModelSquarePage t={t} onSessionMaybeEnded={onSessionMaybeEnded} />,
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(onSessionMaybeEnded).not.toHaveBeenCalled();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5 * 60_000);
      });
      expect(onSessionMaybeEnded).toHaveBeenCalledTimes(1);
    });

    it("does not call onSessionMaybeEnded when a silent refresh fails with a network error (Q6)", async () => {
      vi.useFakeTimers();
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      vi.spyOn(we2aiApi, "keyModels")
        .mockResolvedValueOnce(claudeModels)
        .mockRejectedValue({ code: "TRANSIENT", message: "offline" });
      const onSessionMaybeEnded = vi.fn();

      render(
        <ModelSquarePage t={t} onSessionMaybeEnded={onSessionMaybeEnded} />,
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5 * 60_000);
      });
      expect(onSessionMaybeEnded).not.toHaveBeenCalled();
    });

    it("does not refetch on the periodic timer while the document is hidden", async () => {
      vi.useFakeTimers();
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      const keyModels = vi
        .spyOn(we2aiApi, "keyModels")
        .mockResolvedValue(claudeModels);

      render(<ModelSquarePage t={t} onSessionMaybeEnded={vi.fn()} />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(keyModels).toHaveBeenCalledTimes(1);

      Object.defineProperty(document, "hidden", {
        value: true,
        configurable: true,
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5 * 60_000);
      });
      expect(keyModels).toHaveBeenCalledTimes(1);
    });
  });

  // Opus 复核第 2 轮 Q1：静默刷新如果在前台请求完成前抢先把请求序号往前
  // 推一格，前台请求 resolve 时序号对不上，`loadingModels` 会永远卡在
  // `true`（"重试"按钮也跟着一直禁用）。这里不需要假定时器——用手动控制
  // 的 Promise 模拟"前台请求仍未返回"，配合真实 `focus` 事件即可复现。
  describe("foreground request in flight is not starved by a silent refresh (Q1)", () => {
    it("keeps loading correctly when focus fires while the initial foreground request is still pending", async () => {
      vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      let resolveModels: ((value: We2aiKeyModels) => void) | undefined;
      const keyModels = vi
        .spyOn(we2aiApi, "keyModels")
        .mockImplementation(
          () =>
            new Promise((resolve) => {
              resolveModels = resolve;
            }),
        );

      renderPage();

      await waitFor(() => expect(resolveModels).toBeDefined());
      expect(screen.getByText(t.loadingModels)).toBeInTheDocument();

      // 前台请求（初次加载）仍未完成时触发 focus：静默刷新必须被跳过——
      // 不应该再调用一次 `keyModels`，也不应该把请求序号抢走。
      window.dispatchEvent(new Event("focus"));
      await new Promise((r) => setTimeout(r, 0));
      expect(keyModels).toHaveBeenCalledTimes(1);

      resolveModels!(claudeModels);
      await screen.findByText("claude-sonnet-4-5");

      // loading 状态必须被正确清除，不能因为序号被静默刷新抢走而卡死。
      expect(screen.queryByText(t.loadingModels)).not.toBeInTheDocument();
      expect(keyModels).toHaveBeenCalledTimes(1);
    });
  });

  // Opus 复核第 3 轮 R3：`selectedKeyId` 变回 `null`（唯一途径是 Key 列表
  // 变空——Rust 侧 `choose_selected` 保证列表非空时必有选中项）那个
  // effect 分支会让当前请求的序号作废，如果这时恰好有一个前台请求仍在
  // 进行中，它的 finally 就再也跑不到，`loadingModels`/`loadingModelsRef`
  // 会跟 Q1 一样卡在 `true`——即使当前渲染路径（Key 列表为空时提前
  // return）不会立刻暴露这个问题，也不该让内部状态留着一个不一致的值。
  // 这里验证"清空又恢复"整个来回之后状态依然干净、能正常继续加载。
  describe("resets loading state when the key list becomes empty mid-request (R3)", () => {
    it("recovers cleanly after the key list empties while a foreground request is pending, then a key reappears", async () => {
      const listKeys = vi.spyOn(we2aiApi, "listKeys");
      listKeys.mockResolvedValueOnce({
        keys: [claudeKey],
        selectedKeyId: 1,
      });
      let resolveFirstModels: ((value: We2aiKeyModels) => void) | undefined;
      const keyModels = vi
        .spyOn(we2aiApi, "keyModels")
        .mockImplementationOnce(
          () =>
            new Promise((resolve) => {
              resolveFirstModels = resolve;
            }),
        )
        .mockResolvedValue(claudeModels);

      renderPage();
      await waitFor(() => expect(resolveFirstModels).toBeDefined());
      expect(screen.getByText(t.loadingModels)).toBeInTheDocument();

      // Key 列表变空（唯一能让 selectedKeyId 变回 null 的途径）：点"刷新"，
      // 这一次 listKeys 返回空列表。
      listKeys.mockResolvedValueOnce({ keys: [], selectedKeyId: null });
      await userEvent.click(screen.getByRole("button", { name: t.refresh }));
      await screen.findByText(t.noKeys);

      // 之前那个前台请求这时才姗姗来迟地 resolve：序号已经作废，不应该
      // 让界面又跳回模型列表或抛出任何异常。
      resolveFirstModels!(claudeModels);
      await new Promise((r) => setTimeout(r, 0));
      expect(screen.getByText(t.noKeys)).toBeInTheDocument();

      // Key 又出现了：点"刷新"重新拿到那个 Key，模型必须能正常加载完成，
      // 不能因为状态没被正确复位而卡在"正在加载"。
      listKeys.mockResolvedValueOnce({ keys: [claudeKey], selectedKeyId: 1 });
      await userEvent.click(screen.getByRole("button", { name: t.refresh }));
      await screen.findByText("claude-sonnet-4-5");
      expect(screen.queryByText(t.loadingModels)).not.toBeInTheDocument();
      expect(keyModels).toHaveBeenCalledTimes(2);
    });
  });
});
