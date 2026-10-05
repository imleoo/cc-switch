import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type MockInstance,
} from "vitest";
import { toast } from "sonner";
import { settingsApi } from "@/lib/api/settings";
import { we2aiApi, type We2aiBalance, type We2aiRegion } from "@/we2ai/api";
import { BillingPage, formatWe2aiUsd } from "@/we2ai/BillingPage";
import { getWe2aiStrings } from "@/we2ai/strings";
import { BALANCE_WATCH_TIMEOUT_MS } from "@/we2ai/useBalanceWatch";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}));

const t = getWe2aiStrings("zh");

function bal(balance: number, extra: Partial<We2aiBalance> = {}): We2aiBalance {
  return { balance, frozenBalance: 0, totalRecharged: 0, ...extra };
}

const BASE_URLS: Record<We2aiRegion, string> = {
  international: "https://api.we2ai.com",
  domestic_prod: "https://api.wtgo.com.cn",
  domestic_dev: "https://jiwu.wtgo.com.cn",
};

// 国际版网站与 API 网关分域名；国内版同域。
const WEB_URLS: Record<We2aiRegion, string> = {
  international: "https://we2ai.com",
  domestic_prod: "https://api.wtgo.com.cn",
  domestic_dev: "https://jiwu.wtgo.com.cn",
};

async function renderPage(
  options: {
    region?: We2aiRegion | null;
    balance?: We2aiBalance | null;
    refreshBalance?: () => Promise<We2aiBalance | null>;
    onBalanceArrived?: () => void;
    launchSignal?: number;
  } = {},
) {
  const region =
    options.region === undefined ? "international" : options.region;
  const refreshBalance = options.refreshBalance ?? vi.fn(async () => null);
  const element = (launchSignal?: number) => (
    <BillingPage
      t={t}
      region={region}
      balance={options.balance === undefined ? bal(12.48) : options.balance}
      refreshBalance={refreshBalance}
      onBalanceArrived={options.onBalanceArrived}
      launchSignal={launchSignal}
    />
  );
  // 等挂载时的网关地址请求落地，避免 act 警告。
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(element(options.launchSignal));
  });
  const rerender = async (launchSignal: number) => {
    await act(async () => {
      view.rerender(element(launchSignal));
    });
  };
  return { refreshBalance, rerender, unmount: () => view.unmount() };
}

describe("formatWe2aiUsd", () => {
  it("formats two decimals, keeps the sign and guards non-finite values", () => {
    expect(formatWe2aiUsd(12.48)).toBe("$12.48");
    expect(formatWe2aiUsd(0)).toBe("$0.00");
    expect(formatWe2aiUsd(100)).toBe("$100.00");
    expect(formatWe2aiUsd(-0.37)).toBe("-$0.37");
    expect(formatWe2aiUsd(-0.001)).toBe("$0.00");
    expect(formatWe2aiUsd(Number.NaN)).toBe("—");
  });
});

describe("BillingPage", () => {
  let openExternal: MockInstance<typeof settingsApi.openExternal>;
  let region: We2aiRegion;

  beforeEach(() => {
    region = "international";
    openExternal = vi.spyOn(settingsApi, "openExternal").mockResolvedValue();
    vi.spyOn(we2aiApi, "gatewayInfo").mockImplementation(async () => ({
      baseUrl: BASE_URLS[region],
      webUrl: WEB_URLS[region],
    }));
    vi.spyOn(console, "debug").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("renders available, frozen and total amounts with two decimals", async () => {
    await renderPage({
      balance: bal(12.48, { frozenBalance: 1.5, totalRecharged: 200 }),
    });

    expect(screen.getByTestId("billing-available")).toHaveTextContent("$12.48");
    expect(screen.getByTestId("billing-frozen")).toHaveTextContent("$1.50");
    expect(screen.getByTestId("billing-total")).toHaveTextContent("$200.00");
  });

  it("shows a dash and a retry hint when the balance is unavailable", async () => {
    await renderPage({ balance: null });

    expect(screen.getByTestId("billing-available")).toHaveTextContent("—");
    expect(screen.getByText(t.billingBalanceUnavailable)).toBeInTheDocument();
  });

  it("refresh button calls refreshBalance", async () => {
    const { refreshBalance } = await renderPage();

    await userEvent.click(screen.getByRole("button", { name: t.refresh }));

    expect(refreshBalance).toHaveBeenCalledTimes(1);
  });

  it("international: opens {base}/purchase and shows the Stripe note", async () => {
    await renderPage({ region: "international" });

    expect(screen.getByText(t.billingPayNoteInternational)).toBeInTheDocument();
    expect(
      screen.queryByText(t.billingPayNoteDomestic),
    ).not.toBeInTheDocument();
    expect(screen.getByText(t.billingFirstLoginHint)).toBeInTheDocument();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );

    await waitFor(() => {
      expect(openExternal).toHaveBeenCalledTimes(1);
    });
    expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/purchase");
    expect(
      await screen.findByTestId("billing-watch-waiting"),
    ).toBeInTheDocument();
  });

  it("domestic: shows the Alipay / WeChat note and opens the domestic gateway", async () => {
    region = "domestic_prod";
    await renderPage({ region: "domestic_prod" });

    expect(screen.getByText(t.billingPayNoteDomestic)).toBeInTheDocument();
    expect(
      screen.queryByText(t.billingPayNoteInternational),
    ).not.toBeInTheDocument();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );

    await waitFor(() => {
      expect(openExternal).toHaveBeenCalledWith(
        "https://api.wtgo.com.cn/purchase",
      );
    });
  });

  it("order history link opens {base}/orders without starting a watch", async () => {
    await renderPage();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingOrders }),
    );

    await waitFor(() => {
      expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/orders");
    });
    expect(
      screen.queryByTestId("billing-watch-waiting"),
    ).not.toBeInTheDocument();
  });

  it("does not start waiting and reports an error when the browser can't be opened", async () => {
    openExternal.mockRejectedValue(new Error("boom"));
    await renderPage();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );

    await waitFor(() => {
      expect(toast.error).toHaveBeenCalledWith(t.billingOpenFailed);
    });
    expect(
      screen.queryByTestId("billing-watch-waiting"),
    ).not.toBeInTheDocument();
  });

  it("does not open anything and reports an error when the gateway address is unavailable", async () => {
    vi.spyOn(we2aiApi, "gatewayInfo").mockRejectedValue(
      new Error("no session"),
    );
    await renderPage();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );

    await waitFor(() => {
      expect(toast.error).toHaveBeenCalledWith(t.billingGatewayFailed);
    });
    expect(openExternal).not.toHaveBeenCalled();
  });

  it('detects the payment after "I\'ve paid": toast with the received amount', async () => {
    const refreshBalance = vi.fn(async () => bal(112.48));
    const onBalanceArrived = vi.fn();
    await renderPage({ balance: bal(12.48), refreshBalance, onBalanceArrived });

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    await userEvent.click(
      await screen.findByRole("button", { name: t.billingPaid }),
    );

    await waitFor(() => {
      expect(toast.success).toHaveBeenCalledWith("充值成功，到账 $100.00");
    });
    expect(
      await screen.findByTestId("billing-watch-success"),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId("billing-watch-waiting"),
    ).not.toBeInTheDocument();
    expect(onBalanceArrived).toHaveBeenCalledTimes(1);

    // 成功提示可以手动关闭。
    await userEvent.click(
      screen.getByRole("button", { name: t.billingDismiss }),
    );
    expect(
      screen.queryByTestId("billing-watch-success"),
    ).not.toBeInTheDocument();
  });

  it("without a known balance, fetches it first and does not open the browser when that fails", async () => {
    const refreshBalance = vi.fn(
      async (): Promise<We2aiBalance | null> => null,
    );
    await renderPage({ balance: null, refreshBalance });

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );

    expect(
      await screen.findByTestId("billing-baseline-failed"),
    ).toHaveTextContent("无法获取当前余额，请重试");
    expect(refreshBalance).toHaveBeenCalledTimes(1);
    expect(openExternal).not.toHaveBeenCalled();
    expect(
      screen.queryByTestId("billing-watch-waiting"),
    ).not.toBeInTheDocument();
  });

  it("retry after a failed baseline fetch opens /purchase and uses the fetched balance as the baseline", async () => {
    const results: Array<We2aiBalance | null> = [
      null,
      bal(20),
      bal(20),
      bal(25),
    ];
    const refreshBalance = vi.fn(async () => results.shift() ?? null);
    const onBalanceArrived = vi.fn();
    await renderPage({ balance: null, refreshBalance, onBalanceArrived });

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    await screen.findByTestId("billing-baseline-failed");
    expect(openExternal).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: t.offlineRetry }));

    await waitFor(() => {
      expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/purchase");
    });
    expect(openExternal).toHaveBeenCalledTimes(1);
    // 余额请求先于打开浏览器。
    expect(refreshBalance.mock.invocationCallOrder[1]).toBeLessThan(
      openExternal.mock.invocationCallOrder[0],
    );
    expect(
      await screen.findByTestId("billing-watch-waiting"),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId("billing-baseline-failed"),
    ).not.toBeInTheDocument();

    // 基线是 20：同样的 20 不算到账，25 才算（增量 5）。
    await userEvent.click(screen.getByRole("button", { name: t.billingPaid }));
    expect(screen.getByTestId("billing-watch-waiting")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: t.billingPaid }));
    await waitFor(() => {
      expect(toast.success).toHaveBeenCalledWith("充值成功，到账 $5.00");
    });
    expect(onBalanceArrived).toHaveBeenCalledTimes(1);
  });

  it("launchSignal runs the main recharge flow exactly once per change and ignores the initial value", async () => {
    const { rerender } = await renderPage({ launchSignal: 3 });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(openExternal).not.toHaveBeenCalled();

    await rerender(4);
    await waitFor(() => {
      expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/purchase");
    });
    expect(
      await screen.findByTestId("billing-watch-waiting"),
    ).toBeInTheDocument();

    // 同一个值再次渲染不会重复触发。
    await rerender(4);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(openExternal).toHaveBeenCalledTimes(1);
  });

  it("does not open the browser when the page is unmounted while the baseline balance request is still pending", async () => {
    let resolveBalance: (value: We2aiBalance | null) => void = () => {};
    const refreshBalance = vi.fn(
      () =>
        new Promise<We2aiBalance | null>((resolve) => {
          resolveBalance = resolve;
        }),
    );
    const { unmount } = await renderPage({ balance: null, refreshBalance });

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    expect(refreshBalance).toHaveBeenCalledTimes(1);

    // 会话已换（页面按会话身份 key 重建 = 卸载），旧请求这时才成功返回。
    unmount();
    await act(async () => {
      resolveBalance(bal(20));
    });

    expect(openExternal).not.toHaveBeenCalled();
  });

  it("does not open the browser when the page is unmounted while the gateway address request is still pending", async () => {
    let calls = 0;
    let resolveGateway: (value: {
      baseUrl: string;
      webUrl: string;
    }) => void = () => {};
    vi.spyOn(we2aiApi, "gatewayInfo").mockImplementation(() => {
      calls += 1;
      // 挂载时的预取失败，点击时的那次一直挂起。
      if (calls === 1) return Promise.reject(new Error("not yet"));
      return new Promise((resolve) => {
        resolveGateway = resolve;
      });
    });
    const { unmount } = await renderPage();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    expect(calls).toBe(2);

    unmount();
    await act(async () => {
      resolveGateway({
        baseUrl: "https://api.we2ai.com",
        webUrl: "https://we2ai.com",
      });
    });

    expect(openExternal).not.toHaveBeenCalled();
  });

  it("double-clicking the main button quickly opens the browser only once", async () => {
    let finishOpen: () => void = () => {};
    openExternal.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finishOpen = resolve;
        }),
    );
    await renderPage();
    const button = screen.getByRole("button", {
      name: t.billingRechargeButton,
    });

    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => expect(openExternal).toHaveBeenCalledTimes(1));
    await act(async () => {
      finishOpen();
    });

    await waitFor(() => {
      expect(screen.getByTestId("billing-watch-waiting")).toBeInTheDocument();
    });
    expect(openExternal).toHaveBeenCalledTimes(1);
  });

  it("a launchSignal arriving while a launch is in flight does not open a second time", async () => {
    let finishOpen: () => void = () => {};
    openExternal.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finishOpen = resolve;
        }),
    );
    const { rerender } = await renderPage();

    fireEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    await rerender(1);
    await waitFor(() => expect(openExternal).toHaveBeenCalledTimes(1));
    await act(async () => {
      finishOpen();
    });

    await waitFor(() => {
      expect(screen.getByTestId("billing-watch-waiting")).toBeInTheDocument();
    });
    expect(openExternal).toHaveBeenCalledTimes(1);
  });

  it("stop button returns to the idle state", async () => {
    await renderPage();

    await userEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    await userEvent.click(
      await screen.findByRole("button", { name: t.billingStop }),
    );

    expect(
      screen.queryByTestId("billing-watch-waiting"),
    ).not.toBeInTheDocument();
  });

  it("after the 5-minute timeout shows recheck and order links", async () => {
    vi.useFakeTimers();
    await renderPage();

    fireEvent.click(
      screen.getByRole("button", { name: t.billingRechargeButton }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByTestId("billing-watch-waiting")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(BALANCE_WATCH_TIMEOUT_MS);
    });

    expect(screen.getByTestId("billing-watch-timeout")).toBeInTheDocument();
    expect(screen.getByText(t.billingTimeoutTitle)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: t.billingRecheck }),
    ).toBeInTheDocument();

    openExternal.mockClear();
    fireEvent.click(screen.getByRole("button", { name: t.billingViewOrders }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/orders");
  });
});
