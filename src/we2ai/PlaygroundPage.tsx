/** AI 营销工坊：图片与视频面板常驻挂载，切换功能不取消生成。 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
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
  type We2aiKeyView,
  type We2aiWorkKind,
} from "./api";
import { StudioImagePanel } from "./StudioImagePanel";
import { StudioVideoPanel } from "./StudioVideoPanel";
import { SESSION_ERROR_CODES } from "./studioErrors";
import { getStudioMediaStrings } from "./studioMediaStrings";
import type { We2aiLanguage, We2aiStrings } from "./strings";

interface PlaygroundPageProps {
  t: We2aiStrings;
  language: We2aiLanguage;
  onSessionMaybeEnded: () => void;
  onTerminal: () => void;
  onWorkCompleted?: (kind: We2aiWorkKind) => void;
  active?: boolean;
}
const FOCUS_REFRESH_MIN_INTERVAL_MS = 30_000;
interface LoadOptions {
  silent?: boolean;
}

export function PlaygroundPage({
  t,
  language,
  onSessionMaybeEnded,
  onTerminal,
  onWorkCompleted,
  active = true,
}: PlaygroundPageProps) {
  const ms = useMemo(() => getStudioMediaStrings(language), [language]);
  const [mode, setMode] = useState<"image" | "video">("video");
  const [keys, setKeys] = useState<We2aiKeyView[] | null>(null);
  const [keysError, setKeysError] = useState<string | null>(null);
  const [loadingKeys, setLoadingKeys] = useState(true);
  const [selectedKeyId, setSelectedKeyId] = useState<number | null>(null);
  // 文本模型仅供图片 / 视频的「AI 优化提示词」使用，不提供文案对话入口。
  const [model, setModel] = useState<string | null>(null);
  const selectedKeyIdRef = useRef<number | null>(null);
  selectedKeyIdRef.current = selectedKeyId;
  const modelsRequestSeqRef = useRef(0);
  const keysRequestSeqRef = useRef(0);
  const keysSupersededRetriesRef = useRef(0);
  const resetSession = useCallback((nextKeyId: number | null) => {
    modelsRequestSeqRef.current += 1;
    setModel(null);
    selectedKeyIdRef.current = nextKeyId;
    setSelectedKeyId(nextKeyId);
  }, []);
  const loadKeys = useCallback(
    async (opts: LoadOptions = {}) => {
      const silent = opts.silent ?? false;
      const seq = ++keysRequestSeqRef.current;
      if (!silent) {
        setLoadingKeys(true);
        setKeysError(null);
      }
      try {
        const list = await we2aiApi.listKeys();
        if (seq !== keysRequestSeqRef.current) return;
        keysSupersededRetriesRef.current = 0;
        setKeys(list.keys);
        // 本地选择仍然有效（在列表内且 active）时优先保留（Codex 验收修复项 1）：
        // `handleSelectKey` 是先改本地、再异步把选择写入磁盘，写盘完成前的一次
        // 列表刷新读到的还是旧记忆——若直接采用返回值，用户刚选的 Key 会被弹回
        // 旧选择并触发 resetSession 重建面板。只有本地 Key 已被删除/失效才回退到
        // 服务端记忆值（Rust 侧已对记忆做过 active 校验与广场回退）。
        const local = selectedKeyIdRef.current;
        const localStillValid =
          local !== null &&
          list.keys.some(
            (k) => k.id === local && (silent || k.status === "active"),
          );
        const nextKeyId = localStillValid
          ? local
          : (list.studioSelectedKeyId ?? null);
        // Key 因列表变化（被删/失效后回退）或首次加载而变：必须走会话切换，
        // 必须重建媒体面板，避免旧请求的结果写进新 Key。
        if (local !== null && local !== nextKeyId) {
          resetSession(nextKeyId);
        } else {
          // 首次加载采纳服务端记忆：同样立即成为权威值（见 selectedKeyIdRef 注释）。
          selectedKeyIdRef.current = nextKeyId;
          setSelectedKeyId(nextKeyId);
        }
        setKeysError(null);
      } catch (error) {
        if (seq !== keysRequestSeqRef.current) return;
        if (isWe2aiApiError(error) && error.code === "KEY_LIST_SUPERSEDED") {
          // 被更新的一次拉取取代（P2）：有限次重拉，不能停在「无 Key」空状态。
          if (keysSupersededRetriesRef.current < 3) {
            keysSupersededRetriesRef.current += 1;
            void loadKeysRef.current(opts);
            return;
          }
        }
        // 静默刷新失败：保留旧列表，不弹错误（用户没有发起这次请求）。
        if (!silent) {
          setKeysError(
            isWe2aiApiError(error) ? error.message : t.studioErrorNetwork,
          );
        }
        if (isWe2aiApiError(error) && SESSION_ERROR_CODES.has(error.code)) {
          onSessionMaybeEnded();
        }
      } finally {
        // 不按 silent 区分：静默请求可能顶替了一个还在加载的前台请求，前台请求的
        // finally 因序号过期不会执行，这里必须负责收尾。
        if (seq === keysRequestSeqRef.current) setLoadingKeys(false);
      }
    },
    [onSessionMaybeEnded, resetSession, t.studioErrorNetwork],
  );
  const loadKeysRef = useRef(loadKeys);
  loadKeysRef.current = loadKeys;

  useEffect(() => {
    void loadKeys();
  }, [loadKeys]);

  // Key 写操作（创建/编辑/删除/启停）后重拉：选择器与选中项随之更新。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void we2aiApi
      .onKeysChanged(() => void loadKeys())
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((error) => {
        console.debug("[we2ai] studio keys listener failed", error);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadKeys]);

  const loadModels = useCallback(
    async (keyId: number, opts: LoadOptions = {}) => {
      const seq = ++modelsRequestSeqRef.current;
      if (!opts.silent) setModel(null);
      try {
        const result = await we2aiApi.playgroundModels(keyId);
        if (
          seq !== modelsRequestSeqRef.current ||
          selectedKeyIdRef.current !== keyId
        )
          return;
        const list = result.models;
        const fallback = list[0]?.id ?? null;
        setModel((prev) =>
          opts.silent && prev && list.some((m) => m.id === prev)
            ? prev
            : fallback,
        );
      } catch (error) {
        if (
          seq !== modelsRequestSeqRef.current ||
          selectedKeyIdRef.current !== keyId
        )
          return;
        if (!opts.silent) setModel(null);
        if (isWe2aiApiError(error) && SESSION_ERROR_CODES.has(error.code))
          onSessionMaybeEnded();
      }
    },
    [onSessionMaybeEnded],
  );
  useEffect(() => {
    if (selectedKeyId !== null) void loadModels(selectedKeyId);
    else {
      modelsRequestSeqRef.current += 1;
      setModel(null);
    }
  }, [selectedKeyId, loadModels]);
  // ---- 刷新：切回 Tab / 窗口聚焦 / 手动 ----
  const loadModelsRef = useRef(loadModels);
  loadModelsRef.current = loadModels;
  const [refreshing, setRefreshing] = useState(false);
  /** 递增即通知图片 / 视频面板静默重拉各自的模型列表。 */
  const [mediaRefreshTick, setMediaRefreshTick] = useState(0);
  /** 上次刷新时间；首次挂载的拉取也算一次，所以初值取挂载时刻。 */
  const lastRefreshAtRef = useRef(Date.now());

  const refreshAll = useCallback(async () => {
    lastRefreshAtRef.current = Date.now();
    setRefreshing(true);
    setMediaRefreshTick((n) => n + 1);
    try {
      const before = selectedKeyIdRef.current;
      await loadKeysRef.current({ silent: true });
      const after = selectedKeyIdRef.current;
      // 选中的 Key 没变才单独重拉文本模型；变了的话 selectedKeyId 的 effect 会拉。
      if (after !== null && after === before) {
        await loadModelsRef.current(after, { silent: true });
      }
    } finally {
      setRefreshing(false);
    }
  }, []);
  const refreshAllRef = useRef(refreshAll);
  refreshAllRef.current = refreshAll;

  // 从后台 Tab 切回工坊：重拉（forceMount 不会重新挂载，所以必须显式触发）。
  const wasActiveRef = useRef(active);
  useEffect(() => {
    if (active && !wasActiveRef.current) void refreshAllRef.current();
    wasActiveRef.current = active;
  }, [active]);

  // 窗口重新聚焦 / 重新可见：激活状态下、距上次刷新超过间隔才重拉。
  useEffect(() => {
    if (!active) return;
    const onWake = () => {
      if (document.visibilityState === "hidden") return;
      if (
        Date.now() - lastRefreshAtRef.current <
        FOCUS_REFRESH_MIN_INTERVAL_MS
      ) {
        return;
      }
      void refreshAllRef.current();
    };
    window.addEventListener("focus", onWake);
    document.addEventListener("visibilitychange", onWake);
    return () => {
      window.removeEventListener("focus", onWake);
      document.removeEventListener("visibilitychange", onWake);
    };
  }, [active]);

  const handleSelectKey = useCallback(
    async (keyIdRaw: string) => {
      const keyId = Number(keyIdRaw);
      const key = keys?.find((k) => k.id === keyId);
      if (!key || key.status !== "active" || keyId === selectedKeyId) return;
      // 更新 Key 会通过面板的 key 属性重建，取消旧 Key 的在途请求。
      resetSession(keyId);
      try {
        await we2aiApi.playgroundChooseKey(keyId);
      } catch (error) {
        // 记忆保存失败不阻塞使用：下一次选择再试。
        console.debug("[we2ai] studio choose key failed", error);
      }
    },
    [keys, selectedKeyId, resetSession],
  );

  useEffect(
    () => () => {
      keysRequestSeqRef.current += 1;
      modelsRequestSeqRef.current += 1;
    },
    [],
  );
  const activeKey = keys?.find((k) => k.id === selectedKeyId) ?? null;
  return (
    <div className="grid flex-1 gap-4 lg:grid-cols-[220px_minmax(0,1fr)]">
      <div className="space-y-4">
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="we2ai-label">{t.keyLabel}</span>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => void refreshAll()}
              disabled={refreshing || loadingKeys}
              aria-label={t.refresh}
              title={t.refresh}
              className="h-7 w-7 p-0 text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
              data-testid="we2ai-studio-refresh"
            >
              <RefreshCw
                className={`h-3.5 w-3.5 ${refreshing ? "animate-spin" : ""}`}
              />
            </Button>
          </div>
          {loadingKeys ? (
            <span className="text-sm" data-testid="we2ai-studio-loading-keys">
              {t.loadingKeys}
            </span>
          ) : keysError ? (
            <div className="space-y-2">
              <span className="text-sm" data-testid="we2ai-studio-keys-error">
                {keysError}
              </span>
              <Button
                size="sm"
                variant="outline"
                onClick={() => void loadKeys()}
                className="h-8 gap-1.5 text-xs"
              >
                <RefreshCw className="h-3.5 w-3.5" />
                {t.refresh}
              </Button>
            </div>
          ) : (keys?.length ?? 0) === 0 ? (
            <span className="text-sm" data-testid="we2ai-studio-no-keys">
              {t.noKeys}
            </span>
          ) : (
            <Select
              value={selectedKeyId !== null ? String(selectedKeyId) : undefined}
              onValueChange={(v) => void handleSelectKey(v)}
            >
              <SelectTrigger
                className="w-full rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none"
                aria-label={t.keyLabel}
                data-testid="we2ai-studio-key-select"
              >
                <SelectValue placeholder={t.keyPlaceholder} />
              </SelectTrigger>
              <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
                {(keys ?? []).map((key) => (
                  <SelectItem
                    key={key.id}
                    value={String(key.id)}
                    disabled={key.status !== "active"}
                  >
                    {key.name}
                    {key.status !== "active" ? ` · ${key.maskedKey}` : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          {activeKey && (
            <span className="block font-mono text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
              {activeKey.maskedKey}
            </span>
          )}
        </div>
      </div>
      <div className="flex min-w-0 flex-col gap-3">
        <div
          className="flex flex-wrap gap-1.5"
          role="group"
          aria-label={t.studioTitle}
        >
          {(
            [
              ["image", ms.modeImage],
              ["video", ms.modeVideo],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              onClick={() => setMode(id)}
              aria-pressed={mode === id}
              data-testid={`we2ai-studio-mode-${id}`}
              className={`rounded-lg border-2 border-[var(--we2ai-ink)] px-4 py-1.5 text-sm shadow-none transition-colors ${mode === id ? "bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)]" : "bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] hover:bg-[var(--we2ai-paper-2)]"}`}
            >
              {label}
            </button>
          ))}
        </div>
        <p
          className="text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]"
          data-testid="we2ai-studio-copywriting-hint"
        >
          {t.studioCopywritingHint}
        </p>
        <div className={mode === "image" ? "" : "hidden"}>
          {selectedKeyId !== null && (
            <StudioImagePanel
              key={selectedKeyId}
              t={t}
              ms={ms}
              language={language}
              keyId={selectedKeyId}
              textModel={model}
              refreshTick={mediaRefreshTick}
              onSessionMaybeEnded={onSessionMaybeEnded}
              onTerminal={onTerminal}
              onWorkCompleted={onWorkCompleted}
            />
          )}
        </div>
        <div className={mode === "video" ? "" : "hidden"}>
          {selectedKeyId !== null && (
            <StudioVideoPanel
              key={selectedKeyId}
              t={t}
              ms={ms}
              language={language}
              keyId={selectedKeyId}
              textModel={model}
              refreshTick={mediaRefreshTick}
              onSessionMaybeEnded={onSessionMaybeEnded}
              onTerminal={onTerminal}
              onWorkCompleted={onWorkCompleted}
            />
          )}
        </div>
      </div>
    </div>
  );
}
