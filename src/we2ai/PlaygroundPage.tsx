/**
 * AI 营销工坊（功能 27，P1 文本对话）。
 *
 * 只发命令、只听事件：明文 Key 由 Rust 侧取用（不经 IPC），增量经
 * `we2ai-playground-chunk` 事件推送。请求生命周期（方案第 5.1 节）：
 * 先建立事件监听再 invoke；invoke resolve/reject 即终态；终态后刷新余额。
 * 会话历史只存前端内存（本页随外壳以会话身份作 key 重建；切走 Tab 不丢——
 * TabsContent forceMount）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  CircleStop,
  MessageSquarePlus,
  RefreshCw,
  Send,
  SlidersHorizontal,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
  type We2aiPlaygroundMessage,
  type We2aiPlaygroundModels,
  type We2aiPlaygroundUsage,
} from "./api";
import { renderAnnouncementHtml } from "./announcementMarkdown";
import {
  DEFAULT_OUTPUT_LANGUAGE_ID,
  PLAYGROUND_OUTPUT_LANGUAGES,
  buildPlaygroundSystem,
  getWe2aiPlaygroundPresets,
} from "./playgroundPrompts";
import { StudioSelect } from "./StudioMediaParts";
import { StudioImagePanel } from "./StudioImagePanel";
import { StudioVideoPanel } from "./StudioVideoPanel";
import { SESSION_ERROR_CODES, studioErrorMessage } from "./studioErrors";
import { getStudioMediaStrings } from "./studioMediaStrings";
import {
  formatWe2aiString,
  type We2aiLanguage,
  type We2aiStrings,
} from "./strings";

/** 会话里的单条展示消息（发送给网关的 history 由它投影出来）。 */
interface ChatItem {
  role: "user" | "assistant";
  content: string;
  /** 网关流式 usage（终态才有；缺失不显示）。 */
  usage?: We2aiPlaygroundUsage;
  /** 本轮的错误（气泡内展示，不中断历史）。 */
  error?: string;
  /** 用户主动停止：保留部分内容，标记展示。 */
  stopped?: boolean;
}

interface NumberParam {
  enabled: boolean;
  value: string;
}

interface PlaygroundPageProps {
  t: We2aiStrings;
  language: We2aiLanguage;
  /** 非网络类错误（会话可能已终止）时让外壳复查会话状态。 */
  onSessionMaybeEnded: () => void;
  /** 每轮终态（成功/失败/取消）后刷新余额（方案第 5.1 节）。 */
  onTerminal: () => void;
}

export function PlaygroundPage({
  t,
  language,
  onSessionMaybeEnded,
  onTerminal,
}: PlaygroundPageProps) {
  const presets = useMemo(
    () => getWe2aiPlaygroundPresets(language),
    [language],
  );
  const ms = useMemo(() => getStudioMediaStrings(language), [language]);
  // 模式：文本对话 / 图片 / 视频。三个面板常驻挂载、仅切换可见性——切到别的模式
  // 不能取消正在生成（视频任务已提交并计费）。
  const [mode, setMode] = useState<"text" | "image" | "video">("text");
  const [outputLanguageId, setOutputLanguageId] = useState(
    DEFAULT_OUTPUT_LANGUAGE_ID,
  );

  const [keys, setKeys] = useState<We2aiKeyView[] | null>(null);
  const [keysError, setKeysError] = useState<string | null>(null);
  const [loadingKeys, setLoadingKeys] = useState(true);
  const [selectedKeyId, setSelectedKeyId] = useState<number | null>(null);

  const [models, setModels] = useState<We2aiPlaygroundModels | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [loadingModels, setLoadingModels] = useState(false);
  const [model, setModel] = useState<string | null>(null);

  const [presetId, setPresetId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatItem[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [paramsOpen, setParamsOpen] = useState(false);
  const [temperature, setTemperature] = useState<NumberParam>({
    enabled: false,
    value: "0.7",
  });
  const [topP, setTopP] = useState<NumberParam>({
    enabled: false,
    value: "0.9",
  });
  const [maxTokens, setMaxTokens] = useState<NumberParam>({
    enabled: false,
    value: "1024",
  });
  // 增量监听是否就绪（P2）：发送必须等它建立，否则早期 delta 无人接收而永久丢失。
  const [chunkListenerReady, setChunkListenerReady] = useState(false);
  const [chunkListenerError, setChunkListenerError] = useState<string | null>(
    null,
  );
  const [listenerAttempt, setListenerAttempt] = useState(0);

  /**
   * 会话代次（P2）：切换 Key / 新会话 / 换预设时递增。失效三样东西——在途请求
   * 的 requestId、迟到的 delta、迟到的终态——避免旧 Key 的增量或整轮内容写进
   * 新会话（否则会用新 Key 重发旧 Key 的历史并计费）。
   */
  const sessionEpochRef = useRef(0);
  /** 在途请求：requestId + 发起时的代次，终态与 delta 都按它校验归属。 */
  const activeRequestRef = useRef<{ requestId: string; epoch: number } | null>(
    null,
  );
  /** chunk 监听就绪 promise（resolve 为是否可用）。 */
  const listenerReadyRef = useRef<Promise<boolean> | null>(null);
  /**
   * 当前选中的 Key（供 loadKeys 这类无依赖回调比较，避免闭包过期）。除渲染
   * 同步外，所有改选择/清选择的入口都同步写它（Codex 复审修复项）：ref 必须
   * 在「选择已变更、React 还没重渲染」的窗口里也是权威值——写盘前的列表刷新
   * 若在这个窗口读到旧值，会把用户刚选的 Key 弹回去。
   */
  const selectedKeyIdRef = useRef<number | null>(null);
  selectedKeyIdRef.current = selectedKeyId;
  /** 模型请求序号（P2）：丢弃乱序返回的旧 Key 结果。 */
  const modelsRequestSeqRef = useRef(0);
  /** Key 列表请求序号与「被取代」重试计数（P2）。 */
  const keysRequestSeqRef = useRef(0);
  const keysSupersededRetriesRef = useRef(0);
  // 对话流自动滚动到底部。
  const conversationRef = useRef<HTMLDivElement | null>(null);

  /**
   * 统一的会话切换入口（P2）：取消在途请求、递增代次、清空会话与模型。所有
   * 「选中 Key 变化」的路径（手动切换、列表刷新回退）都必须走这里，不能只
   * `setMessages([])` 了事。
   */
  const resetSession = useCallback((nextKeyId: number | null) => {
    const active = activeRequestRef.current;
    activeRequestRef.current = null;
    sessionEpochRef.current += 1;
    if (active) {
      void we2aiApi.playgroundCancel(active.requestId).catch(() => {
        // 取消失败不影响本地失效：代次已变，迟到数据不会再被采纳。
      });
    }
    setStreaming(false);
    setMessages([]);
    setModels(null);
    setModelsError(null);
    setModel(null);
    // 立即成为权威值（不等重渲染）：这个窗口里到达的列表刷新必须看到新选择。
    selectedKeyIdRef.current = nextKeyId;
    setSelectedKeyId(nextKeyId);
  }, []);

  /** 保留当前 Key，只清空会话（新会话 / 换预设）。 */
  const resetConversation = useCallback(() => {
    const active = activeRequestRef.current;
    activeRequestRef.current = null;
    sessionEpochRef.current += 1;
    if (active) {
      void we2aiApi.playgroundCancel(active.requestId).catch(() => {});
    }
    setStreaming(false);
    setMessages([]);
  }, []);

  const loadKeys = useCallback(async () => {
    const seq = ++keysRequestSeqRef.current;
    setLoadingKeys(true);
    setKeysError(null);
    try {
      const list = await we2aiApi.listKeys();
      if (seq !== keysRequestSeqRef.current) return;
      keysSupersededRetriesRef.current = 0;
      setKeys(list.keys);
      // 本地选择仍然有效（在列表内且 active）时优先保留（Codex 验收修复项 1）：
      // `handleSelectKey` 是先改本地、再异步把选择写入磁盘，写盘完成前的一次
      // 列表刷新读到的还是旧记忆——若直接采用返回值，用户刚选的 Key 会被弹回
      // 旧选择并触发 resetSession 清空会话。只有本地 Key 已被删除/失效才回退到
      // 服务端记忆值（Rust 侧已对记忆做过 active 校验与广场回退）。
      const local = selectedKeyIdRef.current;
      const localStillValid =
        local !== null &&
        list.keys.some((k) => k.id === local && k.status === "active");
      const nextKeyId = localStillValid
        ? local
        : (list.studioSelectedKeyId ?? null);
      // Key 因列表变化（被删/失效后回退）或首次加载而变：必须走会话切换，
      // 不能静默改选中项（否则旧 Key 的历史会被带到新 Key 重发）。
      if (local !== null && local !== nextKeyId) {
        resetSession(nextKeyId);
      } else {
        // 首次加载采纳服务端记忆：同样立即成为权威值（见 selectedKeyIdRef 注释）。
        selectedKeyIdRef.current = nextKeyId;
        setSelectedKeyId(nextKeyId);
      }
    } catch (error) {
      if (seq !== keysRequestSeqRef.current) return;
      if (isWe2aiApiError(error) && error.code === "KEY_LIST_SUPERSEDED") {
        // 被更新的一次拉取取代（P2）：有限次重拉，不能停在「无 Key」空状态。
        if (keysSupersededRetriesRef.current < 3) {
          keysSupersededRetriesRef.current += 1;
          void loadKeysRef.current();
          return;
        }
      }
      setKeysError(
        isWe2aiApiError(error) ? error.message : t.studioErrorNetwork,
      );
      if (isWe2aiApiError(error) && SESSION_ERROR_CODES.has(error.code)) {
        onSessionMaybeEnded();
      }
    } finally {
      if (seq === keysRequestSeqRef.current) setLoadingKeys(false);
    }
  }, [onSessionMaybeEnded, resetSession, t.studioErrorNetwork]);
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
    async (keyId: number) => {
      const seq = ++modelsRequestSeqRef.current;
      setLoadingModels(true);
      setModelsError(null);
      // 换 Key 时先清掉旧模型（P2）：否则加载期间仍能用上一个 Key 的 model 发送。
      setModels(null);
      setModel(null);
      try {
        const result = await we2aiApi.playgroundModels(keyId);
        // 结果、异常都校验序号与当前 Key（P2）：A→B→A 快速切换时，B 的迟到
        // 结果不能覆盖 A 的列表；切走后也不再写状态。
        if (
          seq !== modelsRequestSeqRef.current ||
          selectedKeyIdRef.current !== keyId
        ) {
          return;
        }
        setModels(result);
        setModel(result.models[0]?.id ?? null);
      } catch (error) {
        if (
          seq !== modelsRequestSeqRef.current ||
          selectedKeyIdRef.current !== keyId
        ) {
          return;
        }
        setModels(null);
        setModel(null);
        setModelsError(
          isWe2aiApiError(error) ? error.message : t.studioErrorNetwork,
        );
        if (isWe2aiApiError(error) && SESSION_ERROR_CODES.has(error.code)) {
          onSessionMaybeEnded();
        }
      } finally {
        if (seq === modelsRequestSeqRef.current) setLoadingModels(false);
      }
    },
    [onSessionMaybeEnded, t.studioErrorNetwork],
  );

  useEffect(() => {
    if (selectedKeyId !== null) {
      void loadModels(selectedKeyId);
    } else {
      // 没有选中 Key（无 Key / 未预选）：不能留着上一个 Key 的模型。
      modelsRequestSeqRef.current += 1;
      setModels(null);
      setModel(null);
      setLoadingModels(false);
    }
  }, [selectedKeyId, loadModels]);

  const handleSelectKey = useCallback(
    async (keyIdRaw: string) => {
      const keyId = Number(keyIdRaw);
      const key = keys?.find((k) => k.id === keyId);
      if (!key || key.status !== "active" || keyId === selectedKeyId) return;
      // 切换 Key 统一走会话切换（P2）：取消在途请求、递增代次、清会话与模型。
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

  // 增量事件：建立监听并把「就绪」状态交给发送逻辑等待（P2）。此前是 fire-and-forget
  // 订阅，用户在此之前点生成会导致早期 delta 永久丢失；注册失败也照样能发送。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    setChunkListenerReady(false);
    setChunkListenerError(null);
    const ready = we2aiApi
      .onPlaygroundChunk((event) => {
        const active = activeRequestRef.current;
        // 按 requestId + 会话代次双重过滤（P2）：Key 切换后旧流的迟到增量不能
        // 写进新会话。
        if (!active || event.requestId !== active.requestId) return;
        if (active.epoch !== sessionEpochRef.current) return;
        setMessages((prev) => {
          if (prev.length === 0) return prev;
          const next = [...prev];
          const last = next[next.length - 1];
          if (last.role !== "assistant") return prev;
          next[next.length - 1] = {
            ...last,
            content: last.content + event.delta,
          };
          return next;
        });
      })
      .then((fn) => {
        if (cancelled) {
          fn();
          return false;
        }
        unlisten = fn;
        setChunkListenerReady(true);
        return true;
      })
      .catch((error) => {
        console.debug("[we2ai] studio chunk listener failed", error);
        if (!cancelled) setChunkListenerError(t.studioErrorNetwork);
        return false;
      });
    listenerReadyRef.current = ready;
    return () => {
      cancelled = true;
      listenerReadyRef.current = null;
      setChunkListenerReady(false);
      unlisten?.();
    };
  }, [listenerAttempt, t.studioErrorNetwork]);

  // 对话流自动滚动（scrollTo 能力检测：jsdom 等环境未实现）。
  useEffect(() => {
    const el = conversationRef.current;
    if (el && typeof el.scrollTo === "function") {
      el.scrollTo({ top: el.scrollHeight });
    }
  }, [messages]);

  const handleTerminal = useCallback(
    (requestId: string, epoch: number) => {
      const active = activeRequestRef.current;
      const isCurrent =
        !!active && active.requestId === requestId && active.epoch === epoch;
      if (isCurrent) {
        // 只有仍是当前请求才动会话 UI（P2）：会话已切换/已新建时，旧轮次不能
        // 把新会话的 streaming 关掉，也不能清掉新请求的 active 记录。
        activeRequestRef.current = null;
        setStreaming(false);
      }
      // 但余额刷新与「是否仍是当前会话」无关（P2）：被取消、被重置的轮次同样
      // 可能已经消耗了 token，不刷新会让顶栏余额停在旧值。
      onTerminal();
    },
    [onTerminal],
  );

  const handleSend = useCallback(async () => {
    if (streaming) return;
    const text = input.trim();
    if (!text || selectedKeyId === null || !model) return;
    // 在 await 之前把本次发送绑定的会话状态全部取好（P2）：await 之后闭包里的
    // `messages`/`presetId`/`temperature` 等可能已被重置或改动，那时再读会拿到
    // 新会话的值，把旧请求发成新会话的内容。
    const epochAtStart = sessionEpochRef.current;
    const keyIdAtStart = selectedKeyId;
    const modelAtStart = model;
    const presetAtStart = presets.find((p) => p.id === presetId) ?? null;
    const outputLanguageAtStart = outputLanguageId;
    const temperatureAtStart = temperature;
    const topPAtStart = topP;
    const maxTokensAtStart = maxTokens;
    const historyAtStart = messages;
    // 等增量监听就绪（P2）：注册失败或尚未建立时不发送，否则早期 delta 永久丢失。
    const ready = listenerReadyRef.current;
    if (!ready || !(await ready)) {
      setChunkListenerError(t.studioErrorNetwork);
      return;
    }
    // 等待期间用户可能切换了 Key / 新建会话 / 换预设：代次或选中 Key 已变，
    // 直接放弃本次发送（不能把旧闭包的内容按新代次发出去）。同时确认没有别的
    // 请求在这段等待里抢占（并发上限 1，前端也不能自己绕过去）。
    if (
      sessionEpochRef.current !== epochAtStart ||
      selectedKeyIdRef.current !== keyIdAtStart ||
      activeRequestRef.current !== null
    ) {
      return;
    }
    const epoch = epochAtStart;
    const preset = presetAtStart;
    // 历史投影：只含「成功完成的轮次」（user + 有内容的 assistant 且无错误、
    // 未被停止）。错误轮与停止轮整轮丢弃，避免模型接收到半截内容，也不把失败
    // 的请求重复计费。
    const history: We2aiPlaygroundMessage[] = [];
    for (let i = 0; i < historyAtStart.length; i += 1) {
      const item = historyAtStart[i];
      if (item.role !== "user") continue;
      const reply = historyAtStart[i + 1];
      if (
        reply &&
        reply.role === "assistant" &&
        !reply.error &&
        !reply.stopped &&
        reply.content.length > 0
      ) {
        history.push({ role: "user", content: item.content });
        history.push({ role: "assistant", content: reply.content });
      }
    }
    const requestId =
      typeof crypto !== "undefined" && "randomUUID" in crypto
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random().toString(36).slice(2)}`;

    activeRequestRef.current = { requestId, epoch };
    setMessages((prev) => [
      ...prev,
      { role: "user", content: text },
      { role: "assistant", content: "" },
    ]);
    setInput("");
    setStreaming(true);

    try {
      // 请求体全部用 await 之前捕获的值（P2）：不能在这里读闭包里的新状态。
      const outcome = await we2aiApi.playgroundChat({
        requestId,
        keyId: keyIdAtStart,
        model: modelAtStart,
        messages: [...history, { role: "user", content: text }],
        system: buildPlaygroundSystem(preset, outputLanguageAtStart, language),
        temperature: temperatureAtStart.enabled
          ? Number.parseFloat(temperatureAtStart.value)
          : undefined,
        topP: topPAtStart.enabled
          ? Number.parseFloat(topPAtStart.value)
          : undefined,
        maxTokens: maxTokensAtStart.enabled
          ? Number.parseInt(maxTokensAtStart.value, 10)
          : undefined,
      });
      // 终态校验代次（P2）：切 Key 后旧请求的成功结果不能写进新会话。
      const active = activeRequestRef.current;
      if (
        !active ||
        active.requestId !== requestId ||
        active.epoch !== sessionEpochRef.current
      ) {
        return;
      }
      setMessages((prev) => {
        const next = [...prev];
        const last = next[next.length - 1];
        if (last?.role === "assistant") {
          next[next.length - 1] = {
            ...last,
            usage: outcome.usage ?? undefined,
          };
        }
        return next;
      });
    } catch (error) {
      const active = activeRequestRef.current;
      if (
        !active ||
        active.requestId !== requestId ||
        active.epoch !== sessionEpochRef.current
      ) {
        return;
      }
      const code = isWe2aiApiError(error) ? error.code : null;
      const fallback = error instanceof Error ? error.message : "";
      const message = studioErrorMessage(t, ms, code, fallback);
      setMessages((prev) => {
        const next = [...prev];
        const last = next[next.length - 1];
        if (last?.role === "assistant") {
          next[next.length - 1] = {
            ...last,
            error: message,
            stopped: code === "PLAYGROUND_CANCELLED" || undefined,
          };
        }
        return next;
      });
      if (code && SESSION_ERROR_CODES.has(code)) {
        onSessionMaybeEnded();
      }
    } finally {
      handleTerminal(requestId, epoch);
    }
  }, [
    streaming,
    input,
    selectedKeyId,
    model,
    presets,
    presetId,
    outputLanguageId,
    language,
    ms,
    messages,
    temperature,
    topP,
    maxTokens,
    t,
    onSessionMaybeEnded,
    handleTerminal,
  ]);

  const handleStop = useCallback(() => {
    const active = activeRequestRef.current;
    if (active) void we2aiApi.playgroundCancel(active.requestId);
  }, []);

  const handleNewSession = useCallback(() => {
    // 统一走会话切换（P2）：取消在途请求 + 递增代次，旧轮的迟到数据不再被采纳。
    resetConversation();
  }, [resetConversation]);

  const handlePresetClick = useCallback(
    (id: string) => {
      resetConversation();
      setPresetId((prev) => (prev === id ? null : id));
    },
    [resetConversation],
  );

  const activeKey = keys?.find((k) => k.id === selectedKeyId) ?? null;
  const canSend =
    !streaming &&
    !chunkListenerError &&
    chunkListenerReady &&
    input.trim().length > 0 &&
    selectedKeyId !== null &&
    model !== null;

  return (
    <div className="grid gap-4 lg:grid-cols-[220px_minmax(0,1fr)]">
      {/* 左列：Key 选择器 + 场景预设。 */}
      <div className="space-y-4">
        <div className="space-y-2">
          <span className="we2ai-label">{t.keyLabel}</span>
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

        {mode === "text" && (
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <span className="we2ai-label">{t.studioPresetsLabel}</span>
            </div>
            <p className="text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
              {t.studioPresetsHint}
            </p>
            <div className="flex flex-col gap-1.5">
              {presets.map((preset) => (
                <button
                  key={preset.id}
                  type="button"
                  onClick={() => handlePresetClick(preset.id)}
                  data-testid={`we2ai-studio-preset-${preset.id}`}
                  className={`rounded-lg border-2 border-[var(--we2ai-ink)] px-3 py-2 text-left text-sm shadow-none transition-colors ${
                    presetId === preset.id
                      ? "bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)]"
                      : "bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] hover:bg-[var(--we2ai-paper-2)]"
                  }`}
                  title={preset.hint}
                >
                  {preset.title}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* 主列：模式切换 + 文本对话 / 图片 / 视频（常驻挂载，仅切换可见性）。 */}
      <div className="flex min-w-0 flex-col gap-3">
        <div
          className="flex flex-wrap gap-1.5"
          role="group"
          aria-label={t.studioTitle}
        >
          {(
            [
              ["text", ms.modeText],
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
              className={`rounded-lg border-2 border-[var(--we2ai-ink)] px-4 py-1.5 text-sm shadow-none transition-colors ${
                mode === id
                  ? "bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)]"
                  : "bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] hover:bg-[var(--we2ai-paper-2)]"
              }`}
            >
              {label}
            </button>
          ))}
        </div>

        <div
          className={mode === "text" ? "flex min-w-0 flex-col gap-3" : "hidden"}
          data-testid="we2ai-studio-text-mode"
        >
          <div className="flex flex-wrap items-center gap-3">
            <span className="we2ai-label">{t.studioModelLabel}</span>
            {loadingModels ? (
              <span
                className="text-sm"
                data-testid="we2ai-studio-loading-models"
              >
                {t.loadingModels}
              </span>
            ) : modelsError ? (
              <span className="text-sm" data-testid="we2ai-studio-models-error">
                {modelsError}
              </span>
            ) : (models?.models.length ?? 0) === 0 ? (
              <span className="text-sm" data-testid="we2ai-studio-models-empty">
                {models?.blockedReason
                  ? `${t.studioBlockedPrefix}: ${models.blockedReason}`
                  : t.studioModelsEmpty}
              </span>
            ) : (
              <Select value={model ?? undefined} onValueChange={setModel}>
                <SelectTrigger
                  className="w-72 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none"
                  aria-label={t.studioModelLabel}
                  data-testid="we2ai-studio-model-select"
                >
                  <SelectValue placeholder={t.studioModelPlaceholder} />
                </SelectTrigger>
                <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
                  {(models?.models ?? []).map((m) => (
                    <SelectItem key={m.id} value={m.id}>
                      {m.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
            <StudioSelect
              label={ms.outputLanguageLabel}
              value={outputLanguageId}
              options={PLAYGROUND_OUTPUT_LANGUAGES.map((l) => ({
                value: l.id,
                label: l.label[language],
              }))}
              onChange={setOutputLanguageId}
              disabled={streaming}
              testId="we2ai-studio-output-language"
            />
            <Button
              size="sm"
              variant="outline"
              onClick={handleNewSession}
              disabled={messages.length === 0 && !streaming}
              className="h-8 gap-1.5 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-xs text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
              data-testid="we2ai-studio-new-session"
            >
              <MessageSquarePlus className="h-3.5 w-3.5" />
              {t.studioNewSession}
            </Button>
          </div>

          <div
            ref={conversationRef}
            className="we2ai-scroll min-h-64 flex-1 space-y-3 overflow-y-auto rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] p-4"
            data-testid="we2ai-studio-conversation"
          >
            {chunkListenerError && (
              <div
                className="flex items-center gap-2 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-3 py-2 text-sm"
                data-testid="we2ai-studio-listener-error"
              >
                <span>{chunkListenerError}</span>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setListenerAttempt((n) => n + 1)}
                  className="h-7 gap-1.5 text-xs"
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                  {t.refresh}
                </Button>
              </div>
            )}
            {messages.length === 0 ? (
              <p className="text-sm text-[color:color-mix(in_srgb,var(--we2ai-ink)_50%,transparent)]">
                {t.studioDescription}
              </p>
            ) : (
              messages.map((message, index) =>
                message.role === "user" ? (
                  <div
                    key={index}
                    className="ml-auto max-w-[85%] whitespace-pre-wrap rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] px-3 py-2 text-sm text-[var(--we2ai-paper)]"
                    data-testid="we2ai-studio-user-message"
                  >
                    {message.content}
                  </div>
                ) : (
                  <div
                    key={index}
                    className="max-w-[90%] rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-3 py-2 text-sm"
                    data-testid="we2ai-studio-assistant-message"
                  >
                    {message.content ? (
                      <div
                        className="we2ai-markdown"
                        // 公告同款的严格净化（白名单标签、仅 http(s) 链接）。
                        dangerouslySetInnerHTML={{
                          __html: renderAnnouncementHtml(message.content),
                        }}
                      />
                    ) : (
                      <span className="inline-block h-4 w-2 animate-pulse bg-[var(--we2ai-ink)] align-middle" />
                    )}
                    {message.usage && (
                      <div className="mt-1 text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_50%,transparent)]">
                        {formatWe2aiString(t.studioUsageTokens, {
                          total: message.usage.totalTokens,
                        })}
                      </div>
                    )}
                    {message.stopped && (
                      <div className="mt-1 text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_50%,transparent)]">
                        {t.studioStoppedPartial}
                      </div>
                    )}
                    {message.error && (
                      <div
                        className="mt-1 text-xs text-[var(--we2ai-danger,#b91c1c)]"
                        data-testid="we2ai-studio-message-error"
                      >
                        {message.error}
                      </div>
                    )}
                  </div>
                ),
              )
            )}
          </div>

          <div className="space-y-2">
            <textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (
                  e.key === "Enter" &&
                  !e.shiftKey &&
                  !e.nativeEvent.isComposing
                ) {
                  e.preventDefault();
                  if (canSend) void handleSend();
                }
              }}
              placeholder={t.studioInputPlaceholder}
              rows={3}
              disabled={streaming}
              className="w-full resize-none rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] p-3 text-sm text-[var(--we2ai-ink)] placeholder:text-[color:color-mix(in_srgb,var(--we2ai-ink)_40%,transparent)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--we2ai-ink)] disabled:opacity-60"
              data-testid="we2ai-studio-input"
            />
            <div className="flex flex-wrap items-center gap-2">
              {streaming ? (
                <Button
                  size="sm"
                  onClick={handleStop}
                  className="h-8 gap-1.5 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-xs text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]"
                  data-testid="we2ai-studio-stop"
                >
                  <CircleStop className="h-3.5 w-3.5" />
                  {t.studioStop}
                </Button>
              ) : (
                <Button
                  size="sm"
                  onClick={() => void handleSend()}
                  disabled={!canSend}
                  className="h-8 gap-1.5 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-xs text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]"
                  data-testid="we2ai-studio-send"
                >
                  <Send className="h-3.5 w-3.5" />
                  {t.studioSend}
                </Button>
              )}
              <span className="text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_50%,transparent)]">
                {t.studioHistoryHint}
              </span>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => setParamsOpen((v) => !v)}
                className="ml-auto h-8 gap-1.5 text-xs text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
                data-testid="we2ai-studio-params-toggle"
              >
                <SlidersHorizontal className="h-3.5 w-3.5" />
                {t.studioParamsLabel}
              </Button>
            </div>

            {paramsOpen && (
              <div
                className="grid gap-3 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] p-3 sm:grid-cols-3"
                data-testid="we2ai-studio-params"
              >
                {(
                  [
                    {
                      label: t.studioTemperatureLabel,
                      param: temperature,
                      set: setTemperature,
                      step: "0.1",
                      min: "0",
                      max: "2",
                    },
                    {
                      label: t.studioTopPLabel,
                      param: topP,
                      set: setTopP,
                      step: "0.05",
                      min: "0",
                      max: "1",
                    },
                    {
                      label: t.studioMaxTokensLabel,
                      param: maxTokens,
                      set: setMaxTokens,
                      step: "64",
                      min: "1",
                      max: "8192",
                    },
                  ] as const
                ).map(({ label, param, set, step, min, max }) => (
                  <div key={label} className="flex items-center gap-2">
                    <Checkbox
                      id={`we2ai-studio-param-${label}`}
                      checked={param.enabled}
                      onCheckedChange={(checked) =>
                        set((prev) => ({ ...prev, enabled: checked === true }))
                      }
                    />
                    <Label
                      htmlFor={`we2ai-studio-param-${label}`}
                      className="text-xs font-normal"
                    >
                      {label}
                    </Label>
                    <Input
                      type="number"
                      value={param.value}
                      step={step}
                      min={min}
                      max={max}
                      disabled={!param.enabled}
                      onChange={(e) =>
                        set((prev) => ({ ...prev, value: e.target.value }))
                      }
                      className="h-8 w-24 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-xs shadow-none"
                    />
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>

        <div className={mode === "image" ? "" : "hidden"}>
          {selectedKeyId !== null && (
            <StudioImagePanel
              key={selectedKeyId}
              t={t}
              ms={ms}
              language={language}
              keyId={selectedKeyId}
              textModel={model}
              onSessionMaybeEnded={onSessionMaybeEnded}
              onTerminal={onTerminal}
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
              onSessionMaybeEnded={onSessionMaybeEnded}
              onTerminal={onTerminal}
            />
          )}
        </div>
      </div>
    </div>
  );
}
