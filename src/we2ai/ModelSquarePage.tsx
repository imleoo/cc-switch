import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  isWe2aiApiError,
  resolveCcSwitchRunning,
  we2aiApi,
  type We2aiCcSwitchRunningStatus,
  type We2aiKeyModels,
  type We2aiKeyView,
  type We2aiModelPrice,
  type We2aiPricing,
  type We2aiToolStatusReport,
} from "./api";
import { ApplyDialog, type ApplyTarget } from "./ApplyDialog";
import { WE2AI_TOOL_LABELS } from "./toolLabels";
import {
  buildWe2aiPriceRows,
  formatWe2aiMultiplier,
  isWe2aiModelSubjectToGroupPeak,
  isWe2aiPriceSurcharged,
  resolveWe2aiEffectiveMultiplier,
  type We2aiPriceRow,
} from "./pricing";
import {
  formatWe2aiString,
  getWe2aiErrorMessage,
  type We2aiStrings,
} from "./strings";

/** 价格行字段 → 展示文案；`cacheWrite`/`cacheWrite1h` 不在契约展示范围内，
 * `buildWe2aiPriceRows` 也不会产出它们，这里给个安全兜底而不是抛错。 */
function priceRowLabel(t: We2aiStrings, row: We2aiPriceRow): string {
  switch (row.field) {
    case "input":
      return t.priceInput;
    case "output":
      return t.priceOutput;
    case "cacheRead":
      return t.priceCacheRead;
    case "perRequest":
      // Opus 复核 R1：按秒计费用"按秒"标签，不能沿用"按次"——那样会变成
      // "按次 … 每秒"这种自相矛盾的组合。
      return row.unit === "perSecond" ? t.pricePerSecond : t.pricePerRequest;
    default:
      return "";
  }
}

function priceRowUnit(t: We2aiStrings, unit: We2aiPriceRow["unit"]): string {
  switch (unit) {
    case "perRequest":
      return t.priceUnitPerRequest;
    case "perSecond":
      return t.priceUnitPerSecond;
    case "perMillionTokens":
    default:
      return t.priceUnitPerMillionTokens;
  }
}

/** 模型卡片的价格区域：`pricing` 缺失时整体不渲染；模型没有 `price` 时
 * 显示"暂无定价"（B1 契约"客户端展示规则"）。 */
function ModelPriceSection({
  t,
  price,
  pricing,
  dimmed,
}: {
  t: We2aiStrings;
  price: We2aiModelPrice | null;
  pricing: We2aiPricing | null;
  dimmed: boolean;
}) {
  if (!pricing) return null;
  const rows = buildWe2aiPriceRows(price, pricing);
  const mutedClass = dimmed
    ? "text-[color:color-mix(in_srgb,var(--we2ai-paper)_70%,transparent)]"
    : "text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]";
  // Opus 复核 P5 产品决定：加价（倍率 > 1，含高峰）不显示划线原价，改用
  // 倍率标注；折扣（< 1）继续用逐行划线原价（见 pricing.ts）。v2 契约：
  // 优先用这个模型自己的 `price.multiplier`（image/video 类不叠加分组
  // 高峰），没有时才回退到顶层 `pricing.effectiveMultiplier`。
  const resolvedMultiplier = price
    ? resolveWe2aiEffectiveMultiplier(price, pricing)
    : pricing.effectiveMultiplier;
  const surcharged = isWe2aiPriceSurcharged(resolvedMultiplier);
  // 追加需求（取代 Q2 里"按 billingMode === 'token' 判定"的做法）：
  // image/video 类模型有自己独立的 multiplier、不叠加分组高峰，顶层
  // `peakActive` 对它们不成立，不能跟着一起标"高峰价"；但 billing_mode
  // 本身不是可靠的判定依据——普通 per_request 计费的模型也可能叠加分组
  // 高峰。改为直接比较这个模型的 multiplier 是否（在容差内）等于顶层
  // effectiveMultiplier，数值相等才说明它确实叠加了分组高峰。
  const showPeakBadge =
    pricing.peakActive && isWe2aiModelSubjectToGroupPeak(price, pricing);

  return (
    <div
      className={`mt-3 border-t pt-2 text-xs ${
        dimmed
          ? "border-[var(--we2ai-paper)]/30"
          : "border-[var(--we2ai-ink)]/20"
      }`}
      data-testid="we2ai-model-price"
    >
      {rows.length === 0 ? (
        <p className={mutedClass}>{t.priceUnavailable}</p>
      ) : (
        <>
          {(showPeakBadge || surcharged) && (
            <div className="mb-1 flex flex-wrap gap-1">
              {showPeakBadge && (
                <span
                  className="we2ai-chip inline-block"
                  data-testid="we2ai-price-peak-active"
                >
                  {t.pricePeakActive}
                </span>
              )}
              {surcharged && (
                <span
                  className="we2ai-chip inline-block"
                  data-testid="we2ai-price-multiplier-badge"
                >
                  {formatWe2aiString(t.priceMultiplierBadge, {
                    multiplier: formatWe2aiMultiplier(resolvedMultiplier),
                  })}
                </span>
              )}
            </div>
          )}
          <ul className="space-y-1">
            {rows.map((row) => (
              <li
                key={row.field}
                className="flex items-baseline justify-between gap-2"
                data-testid={`we2ai-price-row-${row.field}`}
              >
                <span>{priceRowLabel(t, row)}</span>
                <span className="text-right">
                  {row.strikethroughLine && (
                    <del
                      className={`mr-1 ${mutedClass}`}
                      data-testid={`we2ai-price-strikethrough-${row.field}`}
                    >
                      <span className="sr-only">
                        {t.priceOriginalSrLabel}
                      </span>
                      {row.strikethroughLine}
                    </del>
                  )}
                  <span data-testid={`we2ai-price-value-${row.field}`}>
                    <span className="sr-only">
                      {t.priceDiscountedSrLabel}
                    </span>
                    {row.line}
                  </span>{" "}
                  <span className={mutedClass}>
                    {priceRowUnit(t, row.unit)}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

/**
 * 只有网络类错误确定与会话无关；其余错误（含 Rust 侧按未知 401 终止会话时
 * 透传的各种错误码）都让外壳重新读取一次会话状态——本地 IPC，代价很小，
 * 避免会话已终止却停留在已登录界面（Fable P3 终验低危项）。
 */
const NETWORK_CODES = new Set(["TRANSIENT", "NETWORK_ERROR"]);

/** 价格新鲜度维护参数（Opus 复核 P3，`docs/we2ai/B1定价契约.md`）。 */
const STALE_MODELS_THRESHOLD_MS = 60_000;
const PERIODIC_MODELS_REFRESH_MS = 5 * 60_000;

export { WE2AI_TOOL_LABELS };

function errorCode(error: unknown): string | null {
  return isWe2aiApiError(error) ? error.code : null;
}

export function describeBlockedReason(
  t: We2aiStrings,
  reason: string | null,
): string {
  switch (reason) {
    case "API_KEY_QUOTA_EXHAUSTED":
      return t.keyBlockedQuotaExhausted;
    case "API_KEY_EXPIRED":
      return t.keyBlockedExpired;
    case "API_KEY_DISABLED":
      return t.keyBlockedDisabled;
    case "INSUFFICIENT_BALANCE":
      return t.keyBlockedInsufficientBalance;
    case "SUBSCRIPTION_NOT_FOUND":
    case "SUBSCRIPTION_INVALID":
    case "SUBSCRIPTION_MAINTENANCE_FAILED":
      return t.keyBlockedSubscription;
    case "USAGE_LIMIT_EXCEEDED":
      return t.keyBlockedUsageLimit;
    case "GROUP_DISABLED":
    case "GROUP_DELETED":
    case "GROUP_NOT_ALLOWED":
      return t.keyBlockedGroup;
    case "ACCESS_DENIED":
      return t.keyBlockedIp;
    case null:
      return t.keyBlockedGeneric;
    default:
      return formatWe2aiString(t.keyBlockedWithCode, { code: reason });
  }
}

interface ModelSquarePageProps {
  t: We2aiStrings;
  /** 会话可能已失效时调用，外壳重新读取会话状态决定是否回登录页。 */
  onSessionMaybeEnded: () => void;
  /** 工具安装与当前生效模型，用于标记"当前使用中"与安装提示。 */
  toolStatus?: We2aiToolStatusReport | null;
  /**
   * apply 前快速检测（`onBeforeApplyDialogOpen`）查到的三态结果，独立于
   * `toolStatus`（Codex 验收 Z1）：完整的 `toolStatus` 可能仍在等待（如
   * 首次登录后联网查版本尚未返回，此时是 `null`），不能让确认弹窗的并存
   * 警告因此错过快速检测已经查到的结果。
   *
   * 取得确定结果（`"running"`/`"not_running"`）时优先于旧的完整报告
   * （Codex 验收 W2）：此前用 `quickRunning || toolStatus?.ccSwitchRunning`
   * 这种 OR 合并，快速检测查到 `"not_running"` 时 `quickRunning` 是
   * `false`，如果旧的完整报告恰好还是 `true`（如另一个工具刚退出、新一轮
   * 完整检测还没返回或失败），OR 合并会让警告继续显示——这次最新的、更
   * 准确的快速结果被旧数据盖过去了。只有快速结果是 `"unknown"`（检测本身
   * 没能得出结论）或还没有过一次快速结果（`null`）时才回退到
   * `toolStatus?.ccSwitchRunning`。
   */
  quickCcSwitchStatus?: We2aiCcSwitchRunningStatus | null;
  /** 写入成功后回调，外壳据此刷新顶栏工具状态。 */
  onApplied?: () => void;
  /**
   * 打开确认弹窗前回调，外壳据此刷新一次工具状态（含 CC Switch 是否在
   * 运行）。修复：此前只在登录后检查一次（方案第 4.1 节"应用启动与每次
   * apply 前检测 CC Switch 进程"），登录后才启动 CC Switch 时，下一次点击
   * 工具按钮打开确认弹窗不会得到新的并存提示；确认写入的硬性拒绝（接管
   * 冲突）本就在 Rust 侧每次 apply 时实时判定，不受这里的前端缓存影响。
   *
   * 返回 `Promise`（Codex 验收 X5）：此前这里只是发起就不再等待，确认弹窗
   * 打开后立即可点确认，看到的仍是这次检测开始前的旧 `toolStatus`——改为
   * 把这个 Promise 原样转交给 `ApplyDialog`，由它在本次检测完成前禁用
   * 确认按钮。返回值 `true`/`false` 表示检测是否在超时前完成（Codex
   * 验收 Y1）：`false` 时 `ApplyDialog` 会提示"未能完成检测"，但仍然放行
   * 确认——避免网络异常时把确认按钮永久挡住。
   */
  onBeforeApplyDialogOpen?: () => Promise<boolean>;
  /** Key 因余额不足被拦截时，提示条上的「去充值」按钮：外壳切到充值 Tab。 */
  onOpenBilling?: () => void;
  /**
   * 外部刷新信号：数值变化（如充值到账）时重拉当前 Key 的模型与准入状态，让
   * 「余额不足」提示条立即更新。首次渲染的初始值不触发。
   */
  reloadSignal?: number;
}

export function ModelSquarePage({
  t,
  onSessionMaybeEnded,
  toolStatus = null,
  quickCcSwitchStatus = null,
  onApplied,
  onBeforeApplyDialogOpen,
  onOpenBilling,
  reloadSignal = 0,
}: ModelSquarePageProps) {
  // Codex 验收 W2/V2：快速检测取得确定结果时优先于旧的完整报告，而不是
  // 与之 OR 合并（见 `quickCcSwitchStatus` 的文档）；复用与顶栏
  // （`We2aiShell.tsx`）同一份判定函数，不要各写一套。
  const ccSwitchRunning = resolveCcSwitchRunning(
    quickCcSwitchStatus,
    toolStatus?.ccSwitchRunning,
  );
  const [applyTarget, setApplyTarget] = useState<ApplyTarget | null>(null);
  const [keys, setKeys] = useState<We2aiKeyView[] | null>(null);
  const [selectedKeyId, setSelectedKeyId] = useState<number | null>(null);
  const [keysError, setKeysError] = useState<string | null>(null);
  const [loadingKeys, setLoadingKeys] = useState(false);
  const [models, setModels] = useState<We2aiKeyModels | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [loadingModels, setLoadingModels] = useState(false);
  // 这批模型/价格数据的拉取时间（毫秒），用来判断是否已经"过期"需要
  // 静默刷新（Opus 复核 P3）。用 ref 供不订阅重渲染的事件监听器读取最新值，
  // 避免每次拉取都要重新订阅监听器。
  const modelsFetchedAtRef = useRef<number | null>(null);
  // 只采纳最后一次模型请求的结果：快速切换 Key 时，先发出的慢请求不能覆盖
  // 后选中 Key 的模型列表。
  const modelsRequestSeq = useRef(0);
  // Key 列表同理：连续点刷新时只采纳最后一次（Codex P3 验收第 1 轮中危项）。
  const keysRequestSeq = useRef(0);
  // 最新一次请求在 Rust 侧反被判为过期（两次 invoke 乱序执行）时自动重拉一次。
  const supersededRetried = useRef(false);
  // "刷新"成功后即使选中的 Key 没变也重拉模型与准入状态。
  const [modelsReloadTick, setModelsReloadTick] = useState(0);
  const lastReloadSignal = useRef(reloadSignal);
  useEffect(() => {
    if (reloadSignal === lastReloadSignal.current) return;
    lastReloadSignal.current = reloadSignal;
    setModelsReloadTick((n) => n + 1);
  }, [reloadSignal]);

  const handleError = useCallback(
    (error: unknown, setMessage: (message: string) => void) => {
      const code = errorCode(error);
      if (code && !NETWORK_CODES.has(code)) {
        onSessionMaybeEnded();
      }
      setMessage(code ? getWe2aiErrorMessage(t, code) : t.errorNetwork);
    },
    [onSessionMaybeEnded, t],
  );

  const loadKeys = useCallback(async () => {
    const seq = ++keysRequestSeq.current;
    setLoadingKeys(true);
    setKeysError(null);
    try {
      const result = await we2aiApi.listKeys();
      if (seq === keysRequestSeq.current) {
        supersededRetried.current = false;
        setKeys(result.keys);
        setSelectedKeyId(result.selectedKeyId);
        setModelsReloadTick((n) => n + 1);
      }
    } catch (error) {
      if (seq !== keysRequestSeq.current) return;
      if (errorCode(error) === "KEY_LIST_SUPERSEDED") {
        // 被更新的一次拉取取代：那一次负责渲染。若那一次恰好是较早发出的
        // 请求（已被前端序号丢弃），这里重拉一次，避免界面停在加载中。
        if (!supersededRetried.current) {
          supersededRetried.current = true;
          void loadKeysRef.current();
        }
        return;
      }
      handleError(error, setKeysError);
    } finally {
      if (seq === keysRequestSeq.current) {
        setLoadingKeys(false);
      }
    }
  }, [handleError]);
  const loadKeysRef = useRef(loadKeys);
  loadKeysRef.current = loadKeys;

  useEffect(() => {
    void loadKeys();
  }, [loadKeys]);

  // 前台请求是否正在进行中（Opus 复核 Q1）：用 ref 而不是 `loadingModels`
  // state，因为要在静默刷新发起前同步读取最新值——如果静默刷新在前台
  // 请求完成前抢先把 `modelsRequestSeq` 往前推一格，前台请求 resolve 时
  // 序号已经不是自己发起时的那个，`finally` 里就再也不会执行，
  // `loadingModels` 会永远卡在 `true`（连带"重试"按钮一直禁用）。
  const loadingModelsRef = useRef(false);

  const loadModels = useCallback(
    async (keyId: number, opts: { silent?: boolean } = {}) => {
      const silent = opts.silent ?? false;
      // 前台请求进行中时，静默刷新直接放弃这一次——不抢占序号，避免上面
      // 说的"前台 finally 永远跑不到"问题；下一次 focus/定时器 tick 再
      // 试，不需要排队重试。
      if (silent && loadingModelsRef.current) {
        return;
      }
      const seq = ++modelsRequestSeq.current;
      // 静默刷新（价格新鲜度维护，Opus 复核 P3）不清空当前展示内容、也不
      // 显示加载态——用户没有发起这次请求，不应该被打断；失败只在
      // console 留痕（会话终止类错误例外，见下方 catch 分支，Q6），等
      // 下一次显式操作（切换 Key/手动刷新）再走前台错误处理。
      if (!silent) {
        loadingModelsRef.current = true;
        setLoadingModels(true);
        setModelsError(null);
        setModels(null);
        // 新的前台请求（切换 Key、手动刷新）意味着旧数据即将被替换，不能
        // 让过期判断在这次请求完成前沿用上一个 Key/上一批数据的拉取时间
        // （Opus 复核 Q1）。
        modelsFetchedAtRef.current = null;
      }
      try {
        const result = await we2aiApi.keyModels(keyId);
        if (seq === modelsRequestSeq.current) {
          setModels(result);
          // 静默刷新成功也要清掉之前可能展示的前台错误——数据已经是新的
          // 了，不应该继续挂着一条"获取失败，请重试"（Opus 复核 Q1）。
          setModelsError(null);
          modelsFetchedAtRef.current = Date.now();
        }
      } catch (error) {
        if (seq === modelsRequestSeq.current) {
          if (silent) {
            console.error("[we2ai] silent price refresh failed", error);
            // Q6：静默刷新遇到会话终止类错误（不是单纯的网络抖动）时也要
            // 让外壳复查一次会话状态，不能因为这次刷新是"背着用户"发起的
            // 就把会话已失效这件事也一并悄悄吞掉。
            const code = errorCode(error);
            if (code && !NETWORK_CODES.has(code)) {
              onSessionMaybeEnded();
            }
          } else {
            handleError(error, setModelsError);
          }
        }
      } finally {
        // 不区分 silent：只要这次请求仍是"最新"的那个就负责把
        // `loadingModels` 复位。静默请求正常不会把它设为 true，这里复位
        // 是幂等的；真正要防的是"前台请求的序号被后来的静默请求抢走，
        // 前台自己的 finally 因为序号不匹配而永远跑不到"（Opus 复核 Q1，
        // 现在已经被上面的 `loadingModelsRef` 检查提前拦截，这里的
        // "不分 silent" 是第二道防线）。
        if (seq === modelsRequestSeq.current) {
          loadingModelsRef.current = false;
          setLoadingModels(false);
        }
      }
    },
    [handleError, onSessionMaybeEnded],
  );
  const loadModelsRef = useRef(loadModels);
  loadModelsRef.current = loadModels;

  useEffect(() => {
    if (selectedKeyId !== null) {
      void loadModels(selectedKeyId);
    } else {
      // Opus 复核 R3：这里让请求序号作废，与 loadModels() 里"序号不匹配时
      // 不再执行 finally"是同一套机制——如果这时恰好有一个前台请求仍在
      // 进行中，它的 finally 再也不会跑到，loadingModels 会跟 Q1 一样卡在
      // true。这个分支自己负责把状态复位，不指望那个失效请求的 finally。
      modelsRequestSeq.current += 1;
      loadingModelsRef.current = false;
      setLoadingModels(false);
      setModels(null);
      modelsFetchedAtRef.current = null;
    }
  }, [selectedKeyId, loadModels, modelsReloadTick]);

  // 价格随高峰/峰谷边界变化会过期（Opus 复核 P3）：窗口重新可见或获得
  // 焦点时，若距上次拉取已超过 60 秒就静默重拉一次；此外页面可见时每 5
  // 分钟定时刷新一次。用 ref 读取最新的 selectedKeyId/loadModels，
  // 避免每次渲染都要重新订阅这两个监听器/定时器。
  const selectedKeyIdRef = useRef(selectedKeyId);
  selectedKeyIdRef.current = selectedKeyId;

  const refreshModelsIfStale = useCallback(() => {
    const keyId = selectedKeyIdRef.current;
    if (keyId === null) return;
    const fetchedAt = modelsFetchedAtRef.current;
    if (fetchedAt !== null && Date.now() - fetchedAt < STALE_MODELS_THRESHOLD_MS) {
      return;
    }
    void loadModelsRef.current(keyId, { silent: true });
  }, []);

  useEffect(() => {
    const onVisibilityOrFocus = () => {
      if (document.hidden) return;
      refreshModelsIfStale();
    };
    document.addEventListener("visibilitychange", onVisibilityOrFocus);
    window.addEventListener("focus", onVisibilityOrFocus);
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityOrFocus);
      window.removeEventListener("focus", onVisibilityOrFocus);
    };
  }, [refreshModelsIfStale]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      const keyId = selectedKeyIdRef.current;
      if (keyId === null) return;
      void loadModelsRef.current(keyId, { silent: true });
    }, PERIODIC_MODELS_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, []);

  const handleSelectKey = (value: string) => {
    const keyId = Number(value);
    setSelectedKeyId(keyId);
    // 记忆失败不影响本次使用，只是下次启动不会默认选中它。
    void we2aiApi.selectKey(keyId).catch(() => undefined);
  };

  const secondaryButtonClass =
    "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]";

  if (keys === null) {
    return (
      <div className="space-y-3 py-6 text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
        {keysError ? (
          <div className="flex items-center gap-3">
            <span role="alert" className="text-[var(--we2ai-orange)]">
              {keysError}
            </span>
            <Button
              size="sm"
              variant="outline"
              disabled={loadingKeys}
              onClick={() => void loadKeys()}
              className={secondaryButtonClass}
            >
              {t.offlineRetry}
            </Button>
          </div>
        ) : (
          <span>{t.loadingKeys}</span>
        )}
      </div>
    );
  }

  if (keys.length === 0) {
    return (
      <div className="space-y-3 py-6 text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
        <p>{t.noKeys}</p>
        <Button
          size="sm"
          variant="outline"
          disabled={loadingKeys}
          onClick={() => void loadKeys()}
          className={secondaryButtonClass}
        >
          {t.refresh}
        </Button>
      </div>
    );
  }

  const selectedKey = keys.find((k) => k.id === selectedKeyId) ?? null;

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <span className="we2ai-label">{t.keyLabel}</span>
        {keys.length === 1 && selectedKey ? (
          <span className="text-sm" data-testid="we2ai-single-key">
            {selectedKey.name}
            {selectedKey.groupName ? ` · ${selectedKey.groupName}` : ""}
          </span>
        ) : (
          <Select
            value={selectedKeyId !== null ? String(selectedKeyId) : undefined}
            onValueChange={handleSelectKey}
          >
            <SelectTrigger
              className="w-72 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none"
              aria-label={t.keyLabel}
            >
              <SelectValue placeholder={t.keyPlaceholder} />
            </SelectTrigger>
            <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
              {keys.map((key) => (
                <SelectItem key={key.id} value={String(key.id)}>
                  {key.name}
                  {key.groupName ? ` · ${key.groupName}` : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        {selectedKey && (
          <span className="font-mono text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
            {selectedKey.maskedKey}
          </span>
        )}
        <Button
          size="sm"
          variant="ghost"
          disabled={loadingKeys}
          onClick={() => void loadKeys()}
          className="rounded-lg border-transparent shadow-none hover:bg-[var(--we2ai-paper-2)]"
        >
          {t.refresh}
        </Button>
      </div>

      {models && !models.callable && (
        <div
          role="alert"
          className="flex items-center justify-between gap-3 border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] px-3 py-2 text-xs font-medium text-[var(--we2ai-paper)]"
        >
          <span>{describeBlockedReason(t, models.blockedReason)}</span>
          {models.blockedReason === "INSUFFICIENT_BALANCE" && onOpenBilling && (
            <button
              type="button"
              onClick={onOpenBilling}
              className="we2ai-model-action shrink-0"
            >
              {t.keyBlockedTopUp}
            </button>
          )}
        </div>
      )}

      {modelsError && (
        <div className="flex items-center gap-3 text-sm">
          <span role="alert" className="text-[var(--we2ai-orange)]">
            {modelsError}
          </span>
          {selectedKeyId !== null && (
            <Button
              size="sm"
              variant="outline"
              disabled={loadingModels}
              onClick={() => void loadModels(selectedKeyId)}
              className={secondaryButtonClass}
            >
              {t.offlineRetry}
            </Button>
          )}
        </div>
      )}

      {loadingModels && (
        <p className="text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
          {t.loadingModels}
        </p>
      )}

      {models && models.models.length === 0 && (
        <p className="text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
          {t.noModels}
        </p>
      )}

      {models && models.models.length > 0 && (
        <ul className="we2ai-model-grid">
          {models.models.map((model) => {
            const isInUseSomewhere = model.tools.some(
              (tool) =>
                toolStatus?.tools.find((s) => s.tool === tool)?.managedModel ===
                model.id,
            );
            return (
              <li
                key={model.id}
                className={`we2ai-model-cell p-4 ${
                  isInUseSomewhere ? "we2ai-model-cell--orange" : ""
                }`}
                data-testid="we2ai-model-card"
              >
                <div className="mb-3 flex items-baseline justify-between gap-2">
                  <span className="break-words font-mono text-sm font-bold">
                    {model.id}
                  </span>
                  {model.provider && (
                    <span className="we2ai-model-provider shrink-0">
                      {model.provider}
                    </span>
                  )}
                </div>
                {model.tools.length === 0 ? (
                  <p
                    className={`text-xs ${
                      isInUseSomewhere
                        ? "text-[color:color-mix(in_srgb,var(--we2ai-paper)_70%,transparent)]"
                        : "text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]"
                    }`}
                  >
                    {t.modelNoTools}
                  </p>
                ) : (
                  <div className="flex flex-wrap gap-2">
                    {model.tools.map((tool) => {
                      const inUse =
                        toolStatus?.tools.find((s) => s.tool === tool)
                          ?.managedModel === model.id;
                      return (
                        <Button
                          key={tool}
                          size="sm"
                          variant={inUse ? "default" : "outline"}
                          disabled={!models.callable || selectedKeyId === null}
                          title={
                            models.callable
                              ? inUse
                                ? t.applyCurrent
                                : undefined
                              : describeBlockedReason(t, models.blockedReason)
                          }
                          onClick={() => {
                            setApplyTarget({ tool, model: model.id });
                          }}
                          className={`we2ai-model-action ${
                            inUse ? "we2ai-model-action--selected" : ""
                          }`}
                        >
                          {WE2AI_TOOL_LABELS[tool]}
                          {inUse ? " ✓" : ""}
                        </Button>
                      );
                    })}
                  </div>
                )}
                <ModelPriceSection
                  t={t}
                  price={model.price}
                  pricing={models.pricing}
                  dimmed={isInUseSomewhere}
                />
              </li>
            );
          })}
        </ul>
      )}

      {selectedKeyId !== null && (
        <ApplyDialog
          t={t}
          keyId={selectedKeyId}
          target={applyTarget}
          claudeModels={
            models?.models
              .filter((m) => m.tools.includes("claude_code"))
              .map((m) => m.id) ?? []
          }
          toolInstalled={
            applyTarget
              ? (toolStatus?.tools.find((s) => s.tool === applyTarget.tool)
                  ?.installed ?? true)
              : true
          }
          ccSwitchRunning={ccSwitchRunning}
          onBeforeApplyDialogOpen={onBeforeApplyDialogOpen}
          onClose={() => setApplyTarget(null)}
          onApplied={() => onApplied?.()}
        />
      )}
    </div>
  );
}

export default ModelSquarePage;
