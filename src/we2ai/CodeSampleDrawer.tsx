import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { toast } from "sonner";
import "./we2ai-theme.css";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { isWe2aiApiError, we2aiApi, type We2aiManagedKey } from "./api";
import { isTextModel } from "./modelKind";
import {
  API_KEY_ENV,
  KEY_PLACEHOLDER,
  SAMPLE_LANGS,
  SAMPLE_PROTOCOLS,
  renderSample,
  sampleBaseUrl,
  type KeyExpr,
  type SampleLang,
  type SampleProtocol,
} from "./codeSamples";
import { isSessionNeutralError } from "./keyManageUtils";
import {
  formatWe2aiString,
  getWe2aiKeyErrorMessage,
  type We2aiStrings,
} from "./strings";

const LANG_LABELS: Record<SampleLang, string> = {
  curl: "curl",
  python: "Python",
  node: "Node.js",
  java: "Java",
  go: "Go",
  powershell: "PowerShell",
};

/** 取不到模型列表时手填框的预填默认值（按协议）。 */
function defaultModel(protocol: SampleProtocol): string {
  return protocol === "anthropic" ? "claude-sonnet-4-5" : "gpt-4.1";
}

/** 每种语言代码块上方的前置条件说明。 */
function prerequisite(
  t: We2aiStrings,
  lang: SampleLang,
  protocol: SampleProtocol,
): string {
  const anthropic = protocol === "anthropic";
  switch (lang) {
    case "curl":
      return t.sampleReqCurl;
    case "python":
      return anthropic ? t.sampleReqPythonAnthropic : t.sampleReqPythonOpenai;
    case "node":
      return anthropic ? t.sampleReqNodeAnthropic : t.sampleReqNodeOpenai;
    case "java":
      return t.sampleReqJava;
    case "go":
      return t.sampleReqGo;
    case "powershell":
      return t.sampleReqPowershell;
  }
}

type ModelsState =
  | { status: "loading" }
  | { status: "list"; ids: string[] }
  | { status: "fallback" };

const secondaryButtonClass =
  "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]";

interface CodeSampleDrawerProps {
  t: We2aiStrings;
  keyItem: We2aiManagedKey;
  onClose: () => void;
  /** 「填入真实 Key」复制时 Rust 缓存里没有这个 Key：让父组件重拉列表重建缓存。 */
  onKeyStale?: () => void;
  /** 复制失败且不是剪贴板/输入类错误时回调，让外壳复查会话状态。 */
  onSessionMaybeEnded?: () => void;
}

/**
 * 调用示例右侧抽屉（功能 22）。父组件按需挂载、关闭即卸载，所以协议/语言/模型/
 * 勾选状态都不会残留。
 *
 * 明文边界：代码框永不显示明文。默认示例读环境变量 `WE2AI_API_KEY`；勾选「填入
 * 真实 Key」时代码框显示掩码，**复制**时前端把占位串 `KEY_PLACEHOLDER` 版本交给
 * Rust（`copyTextWithKey`），由 Rust 从管理页缓存取明文替换后写剪贴板，明文不经 IPC。
 * 不含 Key 的文本（环境变量版代码、Base URL）走 `copyPlainText`，同样由 Rust 写剪贴板
 * （WE2AI 模式下 `copyText` 会被 IPC gate 拒绝并回退到丢失手势的 `navigator.clipboard`）。
 * 本文件不得调用任何返回明文的命令（`check-guards.sh` 4.16 校验）。
 */
export function CodeSampleDrawer({
  t,
  keyItem,
  onClose,
  onKeyStale,
  onSessionMaybeEnded,
}: CodeSampleDrawerProps) {
  const [protocol, setProtocol] = useState<SampleProtocol>("openai");
  const [lang, setLang] = useState<SampleLang>("curl");
  const [fillReal, setFillReal] = useState(false);
  const [baseUrl, setBaseUrl] = useState<string | null>(null);
  const [baseFailed, setBaseFailed] = useState(false);
  const [models, setModels] = useState<ModelsState>({ status: "loading" });
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const [customModel, setCustomModel] = useState<string | null>(null);
  const [copying, setCopying] = useState(false);
  // 用户手动选过协议后，异步到达的「分组平台」不再覆盖默认值。
  const protocolTouched = useRef(false);
  const alive = useRef(true);
  const tablistRef = useRef<HTMLDivElement>(null);
  const idPrefix = useId();

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const loadBase = useCallback(async () => {
    setBaseFailed(false);
    try {
      const info = await we2aiApi.gatewayInfo();
      if (alive.current) setBaseUrl(info.baseUrl);
    } catch {
      if (alive.current) setBaseFailed(true);
    }
  }, []);

  useEffect(() => {
    void loadBase();
  }, [loadBase]);

  // 模型：B1 只认模型广场缓存里的 Key（active/quota_exhausted），已禁用/已过期或不在
  // 缓存时会失败；失败、不可调用或空列表（含过滤掉非文本模型后为空）都退化为手填。
  useEffect(() => {
    let cancelled = false;
    void we2aiApi
      .keyModels(keyItem.id)
      .then((result) => {
        if (cancelled) return;
        const ids = result.callable
          ? Array.from(
              new Set(result.models.filter(isTextModel).map((m) => m.id)),
            )
          : [];
        setModels(
          ids.length > 0 ? { status: "list", ids } : { status: "fallback" },
        );
      })
      .catch(() => {
        if (!cancelled) setModels({ status: "fallback" });
      });
    return () => {
      cancelled = true;
    };
  }, [keyItem.id]);

  // 默认协议取决于 Key 分组的 platform；管理列表视图没有这个字段，从分组下拉接口取。
  useEffect(() => {
    const groupId = keyItem.group?.id;
    if (groupId === undefined) return;
    let cancelled = false;
    void we2aiApi
      .listKeyGroups()
      .then((groups) => {
        if (cancelled || protocolTouched.current) return;
        const group = groups.find((g) => g.id === groupId);
        if (group?.platform === "anthropic") setProtocol("anthropic");
      })
      .catch(() => {
        // 取不到就保持默认的 OpenAI 兼容。
      });
    return () => {
      cancelled = true;
    };
  }, [keyItem.group?.id]);

  const modelOptions = models.status === "list" ? models.ids : null;
  const model = modelOptions
    ? selectedModel && modelOptions.includes(selectedModel)
      ? selectedModel
      : modelOptions[0]
    : (customModel ?? defaultModel(protocol));
  const trimmedModel = model.trim();
  // 模型名里出现 Key 占位串会让 Rust 把它也替换成明文，所以直接禁止。
  const modelHasPlaceholder = model.includes(KEY_PLACEHOLDER);
  const modelsLoading = models.status === "loading";

  const groupName = keyItem.group?.name ?? t.keyMgrNoGroup;
  const title = formatWe2aiString(t.sampleTitle, {
    name: keyItem.name,
    group: groupName,
  });
  const displayBase = baseUrl ? sampleBaseUrl(protocol, baseUrl) : null;

  const render = (keyExpr: KeyExpr): string | null =>
    baseUrl === null
      ? null
      : renderSample(lang, protocol, { baseUrl, model: trimmedModel, keyExpr });

  // 显示版本：环境变量，或掩码字面量。复制版本：环境变量，或待替换串。
  const shownCode = render(
    fillReal ? { kind: "literal", value: keyItem.maskedKey } : { kind: "env" },
  );
  const copyCode = render(
    fillReal ? { kind: "literal", value: KEY_PLACEHOLDER } : { kind: "env" },
  );

  const handleCopyError = (error: unknown) => {
    if (isWe2aiApiError(error)) {
      if (error.code === "KEY_NOT_FOUND") {
        // Rust 缓存已失效（写操作后、会话变化后）：让父组件重拉列表重建缓存。
        toast.error(t.keyMgrCopyStale);
        onKeyStale?.();
      } else {
        toast.error(getWe2aiKeyErrorMessage(t, error.code));
        if (!isSessionNeutralError(error.code)) onSessionMaybeEnded?.();
      }
    } else {
      toast.error(t.keyMgrCopyFailed);
    }
  };

  const handleCopyCode = async () => {
    if (copyCode === null || copying) return;
    setCopying(true);
    try {
      if (fillReal) {
        await we2aiApi.copyTextWithKey(keyItem.id, copyCode);
      } else {
        await we2aiApi.copyPlainText(copyCode);
      }
      toast.success(t.keyMgrCopied);
    } catch (error) {
      handleCopyError(error);
    } finally {
      if (alive.current) setCopying(false);
    }
  };

  const handleCopyBase = async () => {
    if (displayBase === null) return;
    try {
      await we2aiApi.copyPlainText(displayBase);
      toast.success(t.keyMgrCopied);
    } catch (error) {
      handleCopyError(error);
    }
  };

  // 语言 tab 的键盘操作：左右方向键、Home/End 切换并把焦点移到新 tab。
  const handleTabKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const index = SAMPLE_LANGS.indexOf(lang);
    let next: number;
    switch (event.key) {
      case "ArrowRight":
        next = (index + 1) % SAMPLE_LANGS.length;
        break;
      case "ArrowLeft":
        next = (index - 1 + SAMPLE_LANGS.length) % SAMPLE_LANGS.length;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = SAMPLE_LANGS.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    setLang(SAMPLE_LANGS[next]);
    tablistRef.current
      ?.querySelectorAll<HTMLButtonElement>('[role="tab"]')
      [next]?.focus();
  };

  const protocolLabels: Record<SampleProtocol, string> = {
    openai: t.sampleProtocolOpenai,
    anthropic: t.sampleProtocolAnthropic,
    responses: t.sampleProtocolResponses,
  };

  const protocolLabelId = `${idPrefix}-protocol`;
  const modelInputId = `${idPrefix}-model`;
  const panelId = `${idPrefix}-panel`;

  return (
    <DialogPrimitive.Root
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="we2ai-drawer-overlay" />
        <DialogPrimitive.Content
          className="we2ai-theme we2ai-drawer"
          data-testid="code-sample-drawer"
          aria-describedby={undefined}
        >
          <div className="we2ai-drawer-head">
            <DialogPrimitive.Title className="we2ai-heading text-lg">
              {title}
            </DialogPrimitive.Title>
            <DialogPrimitive.Close
              className="we2ai-drawer-close"
              aria-label={t.sampleClose}
            >
              ×
            </DialogPrimitive.Close>
          </div>

          <div className="we2ai-drawer-body">
            <span className="we2ai-label" id={protocolLabelId}>
              {t.sampleProtocolLabel}
            </span>
            <div
              className="we2ai-segmented"
              role="group"
              aria-labelledby={protocolLabelId}
            >
              {SAMPLE_PROTOCOLS.map((value) => (
                <button
                  key={value}
                  type="button"
                  aria-pressed={protocol === value}
                  onClick={() => {
                    protocolTouched.current = true;
                    setProtocol(value);
                  }}
                >
                  {protocolLabels[value]}
                </button>
              ))}
            </div>

            <div className="mt-4 flex flex-wrap items-end gap-x-5 gap-y-3">
              <div className="min-w-[220px] flex-1 space-y-1.5">
                <label className="we2ai-label" htmlFor={modelInputId}>
                  {t.sampleModelLabel}
                </label>
                {models.status === "loading" ? (
                  <select
                    id={modelInputId}
                    className="we2ai-native-select"
                    disabled
                    aria-label={t.sampleModelLabel}
                  >
                    <option>{t.sampleModelsLoading}</option>
                  </select>
                ) : modelOptions ? (
                  <select
                    id={modelInputId}
                    className="we2ai-native-select"
                    value={model}
                    onChange={(e) => setSelectedModel(e.target.value)}
                  >
                    {modelOptions.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </select>
                ) : (
                  <Input
                    id={modelInputId}
                    value={model}
                    onChange={(e) => setCustomModel(e.target.value)}
                    spellCheck={false}
                    autoComplete="off"
                    className="rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] font-mono text-[var(--we2ai-ink)] shadow-none focus:ring-0 focus:border-[var(--we2ai-orange)]"
                  />
                )}
              </div>
              <label className="flex cursor-pointer items-center gap-2 pb-2 text-sm font-bold">
                <input
                  type="checkbox"
                  checked={fillReal}
                  onChange={(e) => setFillReal(e.target.checked)}
                  className="h-4 w-4 accent-[var(--we2ai-blue)]"
                />
                {t.sampleFillRealKey}
              </label>
            </div>
            {models.status === "fallback" && (
              <p
                className="mt-2 text-xs opacity-80"
                data-testid="sample-model-fallback"
              >
                {t.sampleModelFallbackNote}
              </p>
            )}
            {models.status === "fallback" && customModel === null && (
              <p
                className="mt-1 text-xs opacity-80"
                data-testid="sample-model-default-hint"
              >
                {t.sampleModelDefaultHint}
              </p>
            )}
            {modelHasPlaceholder && (
              <p
                role="alert"
                className="mt-1 text-xs text-[var(--we2ai-orange)]"
                data-testid="sample-model-placeholder-conflict"
              >
                {t.sampleModelPlaceholderConflict}
              </p>
            )}

            <div
              ref={tablistRef}
              className="we2ai-lang-tabs"
              role="tablist"
              aria-label={t.sampleLangLabel}
              onKeyDown={handleTabKeyDown}
            >
              {SAMPLE_LANGS.map((value) => (
                <button
                  key={value}
                  type="button"
                  role="tab"
                  aria-selected={lang === value}
                  aria-controls={panelId}
                  tabIndex={lang === value ? 0 : -1}
                  onClick={() => setLang(value)}
                >
                  {LANG_LABELS[value]}
                </button>
              ))}
            </div>

            <div className="we2ai-envhint" data-testid="sample-key-hint">
              {fillReal ? (
                t.sampleFillRealHint
              ) : (
                <>
                  {t.sampleEnvHint}
                  <div>
                    <code>{`export ${API_KEY_ENV}=<${t.sampleEnvKeyPlaceholder}>`}</code>
                    {" (bash / zsh)"}
                  </div>
                  <div>
                    <code>{`$env:${API_KEY_ENV}="<${t.sampleEnvKeyPlaceholder}>"`}</code>
                    {" (PowerShell)"}
                  </div>
                </>
              )}
            </div>
            <p
              className="mt-2 text-xs opacity-80"
              data-testid="sample-prerequisite"
            >
              {prerequisite(t, lang, protocol)}
            </p>

            <div id={panelId} role="tabpanel">
              {shownCode !== null ? (
                <pre className="we2ai-codeblock" tabIndex={0}>
                  <code data-testid="sample-code">{shownCode}</code>
                </pre>
              ) : (
                <div className="mt-3 flex items-center gap-3 text-sm">
                  {baseFailed ? (
                    <>
                      <span role="alert" className="text-[var(--we2ai-orange)]">
                        {t.sampleBaseFailed}
                      </span>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => void loadBase()}
                        className={secondaryButtonClass}
                      >
                        {t.offlineRetry}
                      </Button>
                    </>
                  ) : (
                    <span>{t.sampleBaseLoading}</span>
                  )}
                </div>
              )}
            </div>

            {displayBase !== null && (
              <div className="we2ai-baseurl-row">
                <span>
                  {t.sampleBaseUrlLabel}：
                  <code data-testid="sample-base-url">{displayBase}</code>
                </span>
                <button
                  type="button"
                  className="we2ai-model-action"
                  aria-label={t.sampleCopyBaseUrl}
                  onClick={() => void handleCopyBase()}
                >
                  {t.keyMgrCopy}
                </button>
              </div>
            )}

            <div className="mt-5 text-right">
              <button
                type="button"
                className="we2ai-billing-primary we2ai-billing-primary--small"
                disabled={
                  copyCode === null ||
                  trimmedModel === "" ||
                  modelHasPlaceholder ||
                  modelsLoading ||
                  copying
                }
                onClick={() => void handleCopyCode()}
              >
                {t.sampleCopyCode}
              </button>
            </div>
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

export default CodeSampleDrawer;
