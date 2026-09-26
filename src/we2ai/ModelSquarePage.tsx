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
  type We2aiToolStatusReport,
} from "./api";
import { ApplyDialog, type ApplyTarget } from "./ApplyDialog";
import { WE2AI_TOOL_LABELS } from "./toolLabels";
import {
  formatWe2aiString,
  getWe2aiErrorMessage,
  type We2aiStrings,
} from "./strings";

/**
 * 只有网络类错误确定与会话无关；其余错误（含 Rust 侧按未知 401 终止会话时
 * 透传的各种错误码）都让外壳重新读取一次会话状态——本地 IPC，代价很小，
 * 避免会话已终止却停留在已登录界面（Fable P3 终验低危项）。
 */
const NETWORK_CODES = new Set(["TRANSIENT", "NETWORK_ERROR"]);

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
}

export function ModelSquarePage({
  t,
  onSessionMaybeEnded,
  toolStatus = null,
  quickCcSwitchStatus = null,
  onApplied,
  onBeforeApplyDialogOpen,
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
  // 只采纳最后一次模型请求的结果：快速切换 Key 时，先发出的慢请求不能覆盖
  // 后选中 Key 的模型列表。
  const modelsRequestSeq = useRef(0);
  // Key 列表同理：连续点刷新时只采纳最后一次（Codex P3 验收第 1 轮中危项）。
  const keysRequestSeq = useRef(0);
  // 最新一次请求在 Rust 侧反被判为过期（两次 invoke 乱序执行）时自动重拉一次。
  const supersededRetried = useRef(false);
  // "刷新"成功后即使选中的 Key 没变也重拉模型与准入状态。
  const [modelsReloadTick, setModelsReloadTick] = useState(0);

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

  const loadModels = useCallback(
    async (keyId: number) => {
      const seq = ++modelsRequestSeq.current;
      setLoadingModels(true);
      setModelsError(null);
      setModels(null);
      try {
        const result = await we2aiApi.keyModels(keyId);
        if (seq === modelsRequestSeq.current) {
          setModels(result);
        }
      } catch (error) {
        if (seq === modelsRequestSeq.current) {
          handleError(error, setModelsError);
        }
      } finally {
        if (seq === modelsRequestSeq.current) {
          setLoadingModels(false);
        }
      }
    },
    [handleError],
  );

  useEffect(() => {
    if (selectedKeyId !== null) {
      void loadModels(selectedKeyId);
    } else {
      modelsRequestSeq.current += 1;
      setModels(null);
    }
  }, [selectedKeyId, loadModels, modelsReloadTick]);

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
          className="border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] px-3 py-2 text-xs font-medium text-[var(--we2ai-paper)]"
        >
          {describeBlockedReason(t, models.blockedReason)}
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
        <ul className="we2ai-model-grid grid sm:grid-cols-2">
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
                  <span className="break-all font-mono text-sm font-bold">
                    {model.id}
                  </span>
                  {model.provider && (
                    <span
                      className={`we2ai-chip shrink-0 ${
                        isInUseSomewhere
                          ? "border-[var(--we2ai-paper)] text-[var(--we2ai-paper)]"
                          : ""
                      }`}
                    >
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
                          className={
                            inUse
                              ? "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-ink)]"
                              : isInUseSomewhere
                                ? "rounded-lg border-[var(--we2ai-paper)] bg-transparent text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-paper)] hover:text-[var(--we2ai-ink)]"
                                : secondaryButtonClass
                          }
                        >
                          {WE2AI_TOOL_LABELS[tool]}
                          {inUse ? " ✓" : ""}
                        </Button>
                      );
                    })}
                  </div>
                )}
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
