import { useCallback, useEffect, useRef, useState } from "react";
import type { We2aiBalance } from "./api";

/** 等待到账时的轮询间隔（10 秒 × 5 分钟 = 30 次，远低于服务端限流）。 */
export const BALANCE_WATCH_INTERVAL_MS = 10_000;
/** 等待到账的最长时间，超时后停止自动轮询（不常驻）。 */
export const BALANCE_WATCH_TIMEOUT_MS = 5 * 60_000;

/**
 * - `idle`：未在检测；
 * - `waiting`：已打开充值页，每 10 秒检查一次；
 * - `success`：检测到余额高于基线，保持到下一次 `start` / `stop`；
 * - `timeout`：5 分钟内未到账，停止自动轮询，但窗口重新聚焦与手动按钮仍会检查一次。
 */
export type BalanceWatchStatus = "idle" | "waiting" | "success" | "timeout";

export interface BalanceWatchSuccess {
  /** 本次到账金额（新余额 − 基线），美元。 */
  gain: number;
  balance: number;
}

interface UseBalanceWatchOptions {
  /** 拉一次最新余额；失败返回 `null`（失败不计入成功判断，也不中断检测）。 */
  fetchBalance: () => Promise<We2aiBalance | null>;
  /** 检测到到账时调用一次（toast 等副作用交给调用方）。 */
  onSuccess?: (result: BalanceWatchSuccess) => void;
}

/**
 * 充值到账检测（设计方案 4.3）。
 *
 * - `start(baseline)`：记录基线余额并进入 `waiting`。基线必须是调用方已确认的有效余额
 *   （调用方在没有余额时应先拉一次，拉不到就不要开始检测）：不能用开始之后的第一次
 *   响应当基线，那样付款先于首次响应到账时会漏报。
 * - 判定规则：`balance > baseline` 即成功，不区分来源（他人代充也算）。
 * - 窗口 `focus`：`waiting` / `timeout` 时立即拉一次，其余状态忽略。
 * - 卸载时清定时器并丢弃在途结果；本 hook 应挂在按会话身份 `key` 的组件里，登出/换账号
 *   即整体卸载。
 * - 已知局限：基线是余额而不是累计充值。等待期间用户同时在消耗额度，且充值金额小于
 *   期间消耗时，余额不会高于基线，检测不到。
 */
export function useBalanceWatch({
  fetchBalance,
  onSuccess,
}: UseBalanceWatchOptions) {
  const [status, setStatusState] = useState<BalanceWatchStatus>("idle");
  const [checks, setChecks] = useState(0);
  const [result, setResult] = useState<BalanceWatchSuccess | null>(null);

  const statusRef = useRef<BalanceWatchStatus>("idle");
  const baselineRef = useRef(0);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const alive = useRef(true);
  // 每次 start / stop / success 递增：在途检查返回时发现已过期就丢弃。
  const attemptRef = useRef(0);
  const inFlightRef = useRef(false);
  const fetchRef = useRef(fetchBalance);
  fetchRef.current = fetchBalance;
  const onSuccessRef = useRef(onSuccess);
  onSuccessRef.current = onSuccess;

  const setStatus = useCallback((next: BalanceWatchStatus) => {
    statusRef.current = next;
    setStatusState(next);
  }, []);

  const clearTimers = useCallback(() => {
    if (intervalRef.current !== null) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
    if (timeoutRef.current !== null) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
  }, []);

  /** `force` 为 false（定时器触发）时，上一次检查还没返回就跳过，避免慢网络下请求堆积。 */
  const runCheck = useCallback(
    async (force: boolean) => {
      if (statusRef.current !== "waiting" && statusRef.current !== "timeout") {
        return;
      }
      if (!force && inFlightRef.current) return;
      const attempt = attemptRef.current;
      inFlightRef.current = true;
      let latest: We2aiBalance | null = null;
      try {
        latest = await fetchRef.current();
      } catch {
        latest = null;
      } finally {
        inFlightRef.current = false;
      }
      if (!alive.current || attempt !== attemptRef.current) return;
      setChecks((n) => n + 1);
      if (!latest) return;

      const baseline = baselineRef.current;
      if (latest.balance > baseline) {
        clearTimers();
        attemptRef.current += 1;
        const success = {
          gain: latest.balance - baseline,
          balance: latest.balance,
        };
        setResult(success);
        setStatus("success");
        onSuccessRef.current?.(success);
      }
    },
    [clearTimers, setStatus],
  );

  const beginWaiting = useCallback(
    (baseline: number) => {
      clearTimers();
      attemptRef.current += 1;
      baselineRef.current = baseline;
      setChecks(0);
      setResult(null);
      setStatus("waiting");
      intervalRef.current = setInterval(() => {
        void runCheck(false);
      }, BALANCE_WATCH_INTERVAL_MS);
      timeoutRef.current = setTimeout(() => {
        if (statusRef.current !== "waiting") return;
        clearTimers();
        setStatus("timeout");
      }, BALANCE_WATCH_TIMEOUT_MS);
    },
    [clearTimers, runCheck, setStatus],
  );

  const start = useCallback(
    (baseline: number) => {
      beginWaiting(baseline);
    },
    [beginWaiting],
  );

  /** 立即检查一次（「我已完成支付」）；只在 `waiting` / `timeout` 时有效。 */
  const checkNow = useCallback(() => {
    void runCheck(true);
  }, [runCheck]);

  /** 超时后重新检查：沿用原基线再等一个 5 分钟周期，并立即拉一次。 */
  const retry = useCallback(() => {
    beginWaiting(baselineRef.current);
    void runCheck(true);
  }, [beginWaiting, runCheck]);

  const stop = useCallback(() => {
    clearTimers();
    attemptRef.current += 1;
    setChecks(0);
    setResult(null);
    setStatus("idle");
  }, [clearTimers, setStatus]);

  useEffect(() => {
    const onFocus = () => {
      void runCheck(true);
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [runCheck]);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      clearTimers();
    };
  }, [clearTimers]);

  return { status, checks, result, start, checkNow, retry, stop };
}
