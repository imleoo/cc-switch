import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { settingsApi } from "@/lib/api/settings";
import { we2aiApi, type We2aiBalance, type We2aiRegion } from "./api";
import { useBalanceWatch } from "./useBalanceWatch";
import { formatWe2aiString, type We2aiStrings } from "./strings";

/** 美元金额两位小数：`$12.48`、`-$0.37`；非有限值显示 `—`。 */
export function formatWe2aiUsd(value: number): string {
  if (!Number.isFinite(value)) return "—";
  const fixed = Math.abs(value).toFixed(2);
  const negative = value < 0 && Number(fixed) !== 0;
  return `${negative ? "-" : ""}$${fixed}`;
}

function regionName(
  t: We2aiStrings,
  region: We2aiRegion | null,
): string | null {
  switch (region) {
    case "international":
      return t.regionInternational;
    case "domestic_prod":
      return t.regionDomesticProd;
    case "domestic_dev":
      return t.regionDomesticDev;
    default:
      return null;
  }
}

function hostOf(baseUrl: string): string {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

interface BillingPageProps {
  t: We2aiStrings;
  /** 当前会话区域，决定支付方式说明文案。 */
  region: We2aiRegion | null;
  /** 外壳持有的最新余额；`null` 表示尚未加载或加载失败。 */
  balance: We2aiBalance | null;
  /** 拉取最新余额（同时刷新顶栏）；失败返回 `null`，不抛错。 */
  refreshBalance: () => Promise<We2aiBalance | null>;
  /** 检测到到账时调用（外壳据此让模型广场重拉 Key 准入状态）。 */
  onBalanceArrived?: () => void;
  /**
   * 一次性启动信号：数值变化（模型广场「余额不足」提示条的「去充值」）时执行与主按钮
   * 相同的流程（取基线 → 打开 `/purchase` → 开始到账检测）。首次渲染的初始值不触发。
   */
  launchSignal?: number;
}

/**
 * 充值 Tab（功能 20）：客户端不实现支付，统一在系统浏览器打开 Web 充值页
 * `{base}/purchase`，之后轮询余额检测到账。基础地址由 Rust `we2ai_gateway_info`
 * 提供，前端不硬编码域名。
 */
export function BillingPage({
  t,
  region,
  balance,
  refreshBalance,
  onBalanceArrived,
  launchSignal = 0,
}: BillingPageProps) {
  const [baseUrl, setBaseUrl] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [opening, setOpening] = useState(false);
  // 取不到当前余额：没有基线就无法判断是否到账，不打开浏览器，提示后可重试。
  const [baselineFailed, setBaselineFailed] = useState(false);
  // 同步防重入：`opening` state 在同一轮事件里读到的可能还是旧值。
  const launchingRef = useRef(false);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    void we2aiApi
      .gatewayInfo()
      .then((info) => {
        if (!cancelled) setBaseUrl(info.webUrl);
      })
      .catch((error) => {
        // 静默：点击按钮时会再取一次并给出明确提示。
        console.debug("[we2ai] gateway info failed", error);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const watch = useBalanceWatch({
    fetchBalance: refreshBalance,
    onSuccess: ({ gain }) => {
      toast.success(
        formatWe2aiString(t.billingSuccessToast, {
          amount: formatWe2aiUsd(gain),
        }),
      );
      onBalanceArrived?.();
    },
  });

  const resolveBaseUrl = useCallback(async (): Promise<string | null> => {
    if (baseUrl) return baseUrl;
    try {
      const info = await we2aiApi.gatewayInfo();
      // 等待期间页面已卸载（登出/换账号/换区域时按会话身份 key 重建）：放弃，
      // 不能拿旧会话区域的地址继续打开支付页。
      if (!alive.current) return null;
      setBaseUrl(info.webUrl);
      return info.webUrl;
    } catch (error) {
      console.debug("[we2ai] gateway info failed", error);
      if (alive.current) toast.error(t.billingGatewayFailed);
      return null;
    }
  }, [baseUrl, t.billingGatewayFailed]);

  /** 在系统浏览器打开 `{base}{path}`；成功返回 `true`。 */
  const openWebPage = useCallback(
    async (path: "/purchase" | "/orders"): Promise<boolean> => {
      const base = await resolveBaseUrl();
      if (!base || !alive.current) return false;
      try {
        await settingsApi.openExternal(`${base}${path}`);
        return true;
      } catch (error) {
        console.debug("[we2ai] open external failed", error);
        if (alive.current) toast.error(t.billingOpenFailed);
        return false;
      }
    },
    [resolveBaseUrl, t.billingOpenFailed],
  );

  const handleRecharge = async () => {
    if (launchingRef.current) return;
    launchingRef.current = true;
    setOpening(true);
    setBaselineFailed(false);
    try {
      // 到账判定依赖基线：没有有效余额时先拉一次，拿不到就不打开支付页。
      let baseline = balance?.balance;
      if (baseline === undefined) {
        const latest = await refreshBalance();
        // 页面随会话身份 key 重建：卸载即会话已换，旧请求晚到也不再继续。
        if (!alive.current) return;
        if (!latest) {
          setBaselineFailed(true);
          return;
        }
        baseline = latest.balance;
      }
      if (await openWebPage("/purchase")) {
        if (alive.current) watch.start(baseline);
      }
    } finally {
      launchingRef.current = false;
      if (alive.current) setOpening(false);
    }
  };
  const handleRechargeRef = useRef(handleRecharge);
  handleRechargeRef.current = handleRecharge;

  const lastLaunchSignal = useRef(launchSignal);
  useEffect(() => {
    if (launchSignal === lastLaunchSignal.current) return;
    lastLaunchSignal.current = launchSignal;
    void handleRechargeRef.current();
  }, [launchSignal]);

  const handleRefresh = async () => {
    setRefreshing(true);
    try {
      await refreshBalance();
    } finally {
      if (alive.current) setRefreshing(false);
    }
  };

  const regionText = regionName(t, region);
  const payNote =
    region === "international"
      ? t.billingPayNoteInternational
      : region
        ? t.billingPayNoteDomestic
        : null;
  const amount = (value: number | undefined) =>
    value === undefined ? "—" : formatWe2aiUsd(value);

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-4">
        <p className="text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
          {t.billingDescription}
        </p>
        {regionText && (
          <span className="we2ai-label shrink-0">
            {t.regionLabel} · {regionText}
          </span>
        )}
      </div>

      <div className="we2ai-billing-balance we2ai-panel">
        <div className="we2ai-billing-cell">
          <span className="we2ai-label">{t.billingAvailable}</span>
          <div
            className="we2ai-billing-amount"
            data-testid="billing-available"
            data-low={balance && balance.balance < 1 ? "true" : "false"}
          >
            {amount(balance?.balance)}
          </div>
        </div>
        <div className="we2ai-billing-cell">
          <span className="we2ai-label">{t.billingFrozen}</span>
          <div className="we2ai-billing-amount" data-testid="billing-frozen">
            {amount(balance?.frozenBalance)}
          </div>
        </div>
        <div className="we2ai-billing-cell">
          <span className="we2ai-label">{t.billingTotalRecharged}</span>
          <div className="we2ai-billing-amount" data-testid="billing-total">
            {amount(balance?.totalRecharged)}
          </div>
          <button
            type="button"
            disabled={refreshing}
            onClick={() => void handleRefresh()}
            className="we2ai-model-action mt-2 self-start"
          >
            {t.refresh}
          </button>
        </div>
      </div>
      {!balance && !refreshing && (
        <p role="status" className="text-xs text-[var(--we2ai-orange)]">
          {t.billingBalanceUnavailable}
        </p>
      )}

      <div className="grid gap-5 md:grid-cols-[1.3fr_1fr]">
        <div className="we2ai-panel space-y-3 p-5">
          <div className="we2ai-label">{t.billingSectionRecharge}</div>
          <button
            type="button"
            disabled={opening}
            onClick={() => void handleRecharge()}
            className="we2ai-billing-primary"
          >
            {t.billingRechargeButton}
          </button>
          {payNote && (
            <p className="text-sm">
              {baseUrl && (
                <span className="font-mono">{hostOf(baseUrl)}/purchase · </span>
              )}
              {payNote}
            </p>
          )}
          <p className="text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
            {t.billingFirstLoginHint}
          </p>
        </div>

        <div className="we2ai-panel space-y-3 bg-[var(--we2ai-paper-2)] p-5">
          <div className="we2ai-label">{t.billingSectionMore}</div>
          <button
            type="button"
            onClick={() => void openWebPage("/orders")}
            className="we2ai-link"
          >
            {t.billingOrders}
          </button>
          <p className="text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
            {t.billingMoreHint}
          </p>
        </div>
      </div>

      {baselineFailed && (
        <div
          role="alert"
          data-testid="billing-baseline-failed"
          className="we2ai-panel flex items-center justify-between gap-3 p-4"
        >
          <span className="text-sm text-[var(--we2ai-orange)]">
            {t.billingBaselineFailed}
          </span>
          <button
            type="button"
            disabled={opening}
            onClick={() => void handleRecharge()}
            className="we2ai-model-action"
          >
            {t.offlineRetry}
          </button>
        </div>
      )}

      {watch.status === "waiting" && (
        <div
          role="status"
          data-testid="billing-watch-waiting"
          className="we2ai-panel space-y-3 p-5"
        >
          <div className="we2ai-billing-waiting">{t.billingWaiting}</div>
          <div className="we2ai-billing-pulse" aria-hidden="true" />
          <p className="text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
            {formatWe2aiString(t.billingWaitingMeta, { count: watch.checks })}
          </p>
          <div className="flex gap-3">
            <button
              type="button"
              onClick={watch.checkNow}
              className="we2ai-billing-primary we2ai-billing-primary--small"
            >
              {t.billingPaid}
            </button>
            <button
              type="button"
              onClick={watch.stop}
              className="we2ai-model-action"
            >
              {t.billingStop}
            </button>
          </div>
        </div>
      )}

      {watch.status === "timeout" && (
        <div
          role="status"
          data-testid="billing-watch-timeout"
          className="we2ai-panel space-y-3 p-5"
        >
          <p className="font-black">{t.billingTimeoutTitle}</p>
          <p className="text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
            {t.billingTimeoutHint}
          </p>
          <div className="flex flex-wrap gap-3">
            <button
              type="button"
              onClick={watch.retry}
              className="we2ai-billing-primary we2ai-billing-primary--small"
            >
              {t.billingRecheck}
            </button>
            <button
              type="button"
              onClick={() => void openWebPage("/orders")}
              className="we2ai-model-action"
            >
              {t.billingViewOrders}
            </button>
          </div>
        </div>
      )}

      {watch.status === "success" && watch.result && (
        <div
          role="status"
          data-testid="billing-watch-success"
          className="we2ai-panel space-y-2 p-5"
        >
          <div className="we2ai-billing-toast">
            ✓{" "}
            {formatWe2aiString(t.billingSuccessToast, {
              amount: formatWe2aiUsd(watch.result.gain),
            })}
          </div>
          <button
            type="button"
            onClick={watch.stop}
            className="we2ai-model-action"
          >
            {t.billingDismiss}
          </button>
        </div>
      )}
    </div>
  );
}
