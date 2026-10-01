import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { We2aiBalance } from "@/we2ai/api";
import {
  BALANCE_WATCH_INTERVAL_MS,
  BALANCE_WATCH_TIMEOUT_MS,
  useBalanceWatch,
} from "@/we2ai/useBalanceWatch";

function bal(balance: number): We2aiBalance {
  return { balance, frozenBalance: 0, totalRecharged: 0 };
}

/** 用可变变量模拟服务端余额；失败时返回 `null`。 */
function setup() {
  let current: We2aiBalance | null = bal(10);
  const fetchBalance = vi.fn(async () => current);
  const onSuccess = vi.fn();
  const hook = renderHook(() => useBalanceWatch({ fetchBalance, onSuccess }));
  return {
    fetchBalance,
    onSuccess,
    hook,
    setBalance(next: number | null) {
      current = next === null ? null : bal(next);
    },
  };
}

async function tick(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe("useBalanceWatch", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("stays idle and never fetches before start", async () => {
    const { hook, fetchBalance } = setup();
    await tick(BALANCE_WATCH_INTERVAL_MS * 3);
    expect(hook.result.current.status).toBe("idle");
    expect(fetchBalance).not.toHaveBeenCalled();
  });

  it("polls every 10s while waiting and succeeds once balance exceeds the baseline", async () => {
    const { hook, fetchBalance, onSuccess, setBalance } = setup();

    act(() => hook.result.current.start(10));
    expect(hook.result.current.status).toBe("waiting");
    expect(fetchBalance).not.toHaveBeenCalled();

    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(1);
    expect(hook.result.current.status).toBe("waiting");
    expect(hook.result.current.checks).toBe(1);

    // 余额没变：不算到账。
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(2);
    expect(hook.result.current.status).toBe("waiting");

    setBalance(110);
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(hook.result.current.status).toBe("success");
    expect(hook.result.current.result).toEqual({ gain: 100, balance: 110 });
    expect(onSuccess).toHaveBeenCalledTimes(1);
    expect(onSuccess).toHaveBeenCalledWith({ gain: 100, balance: 110 });

    // 成功后定时器已清，不再继续拉取，也不会重复回调。
    const calls = fetchBalance.mock.calls.length;
    await tick(BALANCE_WATCH_INTERVAL_MS * 5);
    expect(fetchBalance).toHaveBeenCalledTimes(calls);
    expect(onSuccess).toHaveBeenCalledTimes(1);
  });

  it("does not treat a lower or equal balance as success", async () => {
    const { hook, onSuccess, setBalance } = setup();
    act(() => hook.result.current.start(10));

    setBalance(9.5);
    await tick(BALANCE_WATCH_INTERVAL_MS);
    setBalance(10);
    await tick(BALANCE_WATCH_INTERVAL_MS);

    expect(hook.result.current.status).toBe("waiting");
    expect(onSuccess).not.toHaveBeenCalled();
  });

  it("times out after 5 minutes and stops polling", async () => {
    const { hook, fetchBalance, onSuccess } = setup();
    act(() => hook.result.current.start(10));

    await tick(BALANCE_WATCH_TIMEOUT_MS);

    expect(hook.result.current.status).toBe("timeout");
    expect(onSuccess).not.toHaveBeenCalled();
    // 10s × 30 次，之后不再轮询。
    expect(fetchBalance).toHaveBeenCalledTimes(30);
    await tick(BALANCE_WATCH_INTERVAL_MS * 5);
    expect(fetchBalance).toHaveBeenCalledTimes(30);
  });

  it("retry after a timeout restarts waiting with the original baseline and checks immediately", async () => {
    const { hook, fetchBalance, onSuccess, setBalance } = setup();
    act(() => hook.result.current.start(10));
    await tick(BALANCE_WATCH_TIMEOUT_MS);
    expect(hook.result.current.status).toBe("timeout");
    const before = fetchBalance.mock.calls.length;

    setBalance(15);
    await act(async () => {
      hook.result.current.retry();
    });

    expect(fetchBalance).toHaveBeenCalledTimes(before + 1);
    expect(hook.result.current.status).toBe("success");
    expect(onSuccess).toHaveBeenCalledWith({ gain: 5, balance: 15 });
  });

  it("retry keeps waiting (and polling) when nothing has arrived yet", async () => {
    const { hook, fetchBalance } = setup();
    act(() => hook.result.current.start(10));
    await tick(BALANCE_WATCH_TIMEOUT_MS);
    const before = fetchBalance.mock.calls.length;

    await act(async () => {
      hook.result.current.retry();
    });
    expect(hook.result.current.status).toBe("waiting");
    expect(hook.result.current.checks).toBe(1);

    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(before + 2);
  });

  it("stop returns to idle, clears timers and discards an in-flight result", async () => {
    let release: (value: We2aiBalance | null) => void = () => {};
    const fetchBalance = vi.fn(
      () => new Promise<We2aiBalance | null>((resolve) => (release = resolve)),
    );
    const onSuccess = vi.fn();
    const hook = renderHook(() => useBalanceWatch({ fetchBalance, onSuccess }));

    act(() => hook.result.current.start(10));
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(1);

    act(() => hook.result.current.stop());
    expect(hook.result.current.status).toBe("idle");

    // 在途请求在停止之后才返回高余额：必须被丢弃。
    await act(async () => {
      release(bal(999));
    });
    expect(hook.result.current.status).toBe("idle");
    expect(onSuccess).not.toHaveBeenCalled();

    await tick(BALANCE_WATCH_TIMEOUT_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(1);
    expect(hook.result.current.status).toBe("idle");
  });

  it("fetches immediately on window focus while waiting, and ignores focus when idle", async () => {
    const { hook, fetchBalance, onSuccess, setBalance } = setup();

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    expect(fetchBalance).not.toHaveBeenCalled();

    act(() => hook.result.current.start(10));
    setBalance(25);
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    expect(fetchBalance).toHaveBeenCalledTimes(1);
    expect(hook.result.current.status).toBe("success");
    expect(onSuccess).toHaveBeenCalledWith({ gain: 15, balance: 25 });
  });

  it("window focus also checks after a timeout", async () => {
    const { hook, onSuccess, setBalance } = setup();
    act(() => hook.result.current.start(10));
    await tick(BALANCE_WATCH_TIMEOUT_MS);
    expect(hook.result.current.status).toBe("timeout");

    setBalance(12);
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    expect(hook.result.current.status).toBe("success");
    expect(onSuccess).toHaveBeenCalledTimes(1);
  });

  it('checkNow ("I\'ve paid") fetches immediately', async () => {
    const { hook, fetchBalance, setBalance } = setup();
    act(() => hook.result.current.start(10));
    setBalance(11);

    await act(async () => {
      hook.result.current.checkNow();
    });

    expect(fetchBalance).toHaveBeenCalledTimes(1);
    expect(hook.result.current.status).toBe("success");
  });

  it("ignores failed fetches and keeps waiting", async () => {
    const { hook, onSuccess, setBalance } = setup();
    act(() => hook.result.current.start(10));

    setBalance(null);
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(hook.result.current.status).toBe("waiting");
    expect(onSuccess).not.toHaveBeenCalled();

    setBalance(20);
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(hook.result.current.status).toBe("success");
  });

  it("clears its timers and ignores focus after unmount", async () => {
    const { hook, fetchBalance } = setup();
    act(() => hook.result.current.start(10));

    hook.unmount();
    await tick(BALANCE_WATCH_TIMEOUT_MS);
    window.dispatchEvent(new Event("focus"));

    expect(fetchBalance).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('"I\'ve paid" fetches immediately even while a poll request is still in flight', async () => {
    const resolvers: Array<(value: We2aiBalance | null) => void> = [];
    const fetchBalance = vi.fn(
      () =>
        new Promise<We2aiBalance | null>((resolve) => resolvers.push(resolve)),
    );
    const onSuccess = vi.fn();
    const hook = renderHook(() => useBalanceWatch({ fetchBalance, onSuccess }));

    act(() => hook.result.current.start(10));
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(1);

    // 定时器触发的请求还没返回时，定时器不会再叠加请求；手动点击则强制再拉一次。
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(1);
    await act(async () => {
      hook.result.current.checkNow();
    });
    expect(fetchBalance).toHaveBeenCalledTimes(2);

    await act(async () => {
      resolvers[1](bal(110));
    });
    expect(hook.result.current.status).toBe("success");
    expect(onSuccess).toHaveBeenCalledWith({ gain: 100, balance: 110 });
  });

  it("a second start discards the in-flight result of the first one", async () => {
    const resolvers: Array<(value: We2aiBalance | null) => void> = [];
    const fetchBalance = vi.fn(
      () =>
        new Promise<We2aiBalance | null>((resolve) => resolvers.push(resolve)),
    );
    const onSuccess = vi.fn();
    const hook = renderHook(() => useBalanceWatch({ fetchBalance, onSuccess }));

    act(() => hook.result.current.start(10));
    await tick(BALANCE_WATCH_INTERVAL_MS);
    expect(fetchBalance).toHaveBeenCalledTimes(1);

    // 第二次 start（新基线 100）：第一轮在途的请求之后才返回 60，按旧基线 10
    // 会判成功，但必须被丢弃。
    act(() => hook.result.current.start(100));
    expect(hook.result.current.checks).toBe(0);
    await act(async () => {
      resolvers[0](bal(60));
    });

    expect(hook.result.current.status).toBe("waiting");
    expect(hook.result.current.checks).toBe(0);
    expect(onSuccess).not.toHaveBeenCalled();

    // 新一轮按新基线判断。
    await tick(BALANCE_WATCH_INTERVAL_MS);
    await act(async () => {
      resolvers[1](bal(150));
    });
    expect(hook.result.current.status).toBe("success");
    expect(onSuccess).toHaveBeenCalledWith({ gain: 50, balance: 150 });
  });
});
