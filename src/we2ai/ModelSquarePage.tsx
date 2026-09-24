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
  we2aiApi,
  type We2aiKeyModels,
  type We2aiKeyView,
  type We2aiTool,
} from "./api";
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

export const WE2AI_TOOL_LABELS: Record<We2aiTool, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
  workbuddy: "WorkBuddy",
};

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
}

export function ModelSquarePage({
  t,
  onSessionMaybeEnded,
}: ModelSquarePageProps) {
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

  if (keys === null) {
    return (
      <div className="space-y-3 py-6 text-sm text-muted-foreground">
        {keysError ? (
          <div className="flex items-center gap-3">
            <span role="alert" className="text-destructive">
              {keysError}
            </span>
            <Button
              size="sm"
              variant="outline"
              disabled={loadingKeys}
              onClick={() => void loadKeys()}
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
      <div className="space-y-3 py-6 text-sm text-muted-foreground">
        <p>{t.noKeys}</p>
        <Button
          size="sm"
          variant="outline"
          disabled={loadingKeys}
          onClick={() => void loadKeys()}
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
        <span className="text-sm font-medium">{t.keyLabel}</span>
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
            <SelectTrigger className="w-72" aria-label={t.keyLabel}>
              <SelectValue placeholder={t.keyPlaceholder} />
            </SelectTrigger>
            <SelectContent>
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
          <span className="font-mono text-xs text-muted-foreground">
            {selectedKey.maskedKey}
          </span>
        )}
        <Button
          size="sm"
          variant="ghost"
          disabled={loadingKeys}
          onClick={() => void loadKeys()}
        >
          {t.refresh}
        </Button>
      </div>

      {models && !models.callable && (
        <div
          role="alert"
          className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200"
        >
          {describeBlockedReason(t, models.blockedReason)}
        </div>
      )}

      {modelsError && (
        <div className="flex items-center gap-3 text-sm">
          <span role="alert" className="text-destructive">
            {modelsError}
          </span>
          {selectedKeyId !== null && (
            <Button
              size="sm"
              variant="outline"
              disabled={loadingModels}
              onClick={() => void loadModels(selectedKeyId)}
            >
              {t.offlineRetry}
            </Button>
          )}
        </div>
      )}

      {loadingModels && (
        <p className="text-sm text-muted-foreground">{t.loadingModels}</p>
      )}

      {models && models.models.length === 0 && (
        <p className="text-sm text-muted-foreground">{t.noModels}</p>
      )}

      {models && models.models.length > 0 && (
        <ul className="grid gap-3 sm:grid-cols-2">
          {models.models.map((model) => (
            <li
              key={model.id}
              className="rounded-lg border p-3"
              data-testid="we2ai-model-card"
            >
              <div className="mb-2 flex items-baseline justify-between gap-2">
                <span className="break-all font-mono text-sm font-medium">
                  {model.id}
                </span>
                {model.provider && (
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {model.provider}
                  </span>
                )}
              </div>
              {model.tools.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  {t.modelNoTools}
                </p>
              ) : (
                <div className="flex flex-wrap gap-2">
                  {model.tools.map((tool) => (
                    <Button
                      key={tool}
                      size="sm"
                      variant="outline"
                      disabled
                      title={
                        models.callable
                          ? t.applyComingSoon
                          : describeBlockedReason(t, models.blockedReason)
                      }
                    >
                      {WE2AI_TOOL_LABELS[tool]}
                    </Button>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export default ModelSquarePage;
