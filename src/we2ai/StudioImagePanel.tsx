/**
 * AI 营销工坊 · 图片模式（功能 27，P2）。
 *
 * 模板 → 字段 → 可编辑的提示词 → 生成。明文 Key 由 Rust 侧取用，面板只持有
 * `keyId`；图片由 Rust 落盘并回传 data URL 供展示。父组件以 `key={keyId}` 挂载
 * 本面板，换 Key 即整体重建；卸载时取消在途请求。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CircleStop, FolderOpen, Download, ImageIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  isWe2aiApiError,
  we2aiApi,
  type We2aiMediaFile,
  type We2aiWorkKind,
  type We2aiReferenceImage,
} from "./api";
import {
  FieldsForm,
  PromptEditor,
  ReferencePicker,
  StudioSelect,
  TemplateChips,
  studioBoxClass,
  studioButtonClass,
  studioMutedClass,
} from "./StudioMediaParts";
import { SESSION_ERROR_CODES, studioErrorMessage } from "./studioErrors";
import {
  newRequestId,
  readReferenceFile,
  useMediaModels,
  useStudioCompile,
} from "./studioMediaCommon";
import {
  buildCompileSystem,
  buildImagePrompt,
  getStudioImageTemplates,
  missingRequiredFields,
  pickFraming,
  type StudioValues,
} from "./studioMediaTemplates";
import type { StudioMediaStrings } from "./studioMediaStrings";
import {
  formatWe2aiString,
  type We2aiLanguage,
  type We2aiStrings,
} from "./strings";

interface StudioImagePanelProps {
  t: We2aiStrings;
  ms: StudioMediaStrings;
  language: We2aiLanguage;
  keyId: number;
  /** 该 Key 下选中的文本模型，用于「AI 优化提示词」；没有则该按钮置灰。 */
  textModel: string | null;
  /** 递增即静默重拉本面板的模型列表（切回 Tab、窗口聚焦、手动刷新）。 */
  refreshTick?: number;
  onSessionMaybeEnded: () => void;
  /** 每次终态（成功 / 失败 / 取消）后刷新余额。 */
  onTerminal: () => void;
  onWorkCompleted?: (kind: We2aiWorkKind) => void;
}

export function StudioImagePanel({
  t,
  ms,
  language,
  keyId,
  textModel,
  refreshTick = 0,
  onSessionMaybeEnded,
  onTerminal,
  onWorkCompleted,
}: StudioImagePanelProps) {
  const templates = useMemo(
    () => getStudioImageTemplates(language),
    [language],
  );
  const [templateId, setTemplateId] = useState(templates[0].id);
  const template =
    templates.find((tpl) => tpl.id === templateId) ?? templates[0];
  const [values, setValues] = useState<StudioValues>({});

  const {
    models,
    model,
    selectModel,
    loading,
    error: modelsError,
  } = useMediaModels(
    keyId,
    "image",
    t.studioErrorNetwork,
    onSessionMaybeEnded,
    refreshTick,
  );
  const caps = model?.capabilities ?? null;

  const [reference, setReference] = useState<{
    reference: We2aiReferenceImage;
    previewUrl: string;
  } | null>(null);
  const [referenceError, setReferenceError] = useState<string | null>(null);
  const effectiveReference = caps?.supportsReference ? reference : null;

  const autoPrompt = useMemo(
    () =>
      buildImagePrompt(template.id, values, {
        hasReference: effectiveReference !== null,
      }),
    [template.id, values, effectiveReference],
  );
  const [prompt, setPrompt] = useState(autoPrompt);
  const [dirty, setDirty] = useState(false);
  useEffect(() => {
    if (!dirty) setPrompt(autoPrompt);
  }, [autoPrompt, dirty]);

  // 参数：默认值由模板构图 + 模型能力表推出，用户改过的值以覆盖项保存；换模板 / 换模型时清空覆盖。
  const defaults = useMemo(() => {
    if (!caps) return { size: "", aspectRatio: "", imageSize: "" };
    const framing = pickFraming(template.framing, caps);
    return {
      size: framing.size ?? "",
      aspectRatio: framing.aspectRatio ?? "",
      imageSize: caps.imageSizes[0] ?? "",
    };
  }, [caps, template.framing]);
  const [overrides, setOverrides] = useState<{
    size?: string;
    aspectRatio?: string;
    imageSize?: string;
    n?: number;
  }>({});
  const size = overrides.size ?? defaults.size;
  const aspectRatio = overrides.aspectRatio ?? defaults.aspectRatio;
  const imageSize = overrides.imageSize ?? defaults.imageSize;
  const n = Math.min(overrides.n ?? 1, caps?.maxN ?? 1);

  const [generating, setGenerating] = useState(false);
  const [images, setImages] = useState<We2aiMediaFile[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const activeRef = useRef<string | null>(null);

  const { compiling, compile } = useStudioCompile(keyId, textModel);
  const busy = generating || compiling;
  const missing = missingRequiredFields(template.fields, values);
  const canGenerate =
    !busy && model !== null && prompt.trim() !== "" && missing.length === 0;

  // 卸载（换 Key、登出重建）时取消在途请求。
  useEffect(
    () => () => {
      const id = activeRef.current;
      activeRef.current = null;
      if (id) void we2aiApi.playgroundCancel(id).catch(() => {});
    },
    [],
  );

  const showError = useCallback(
    (e: unknown) => {
      const code = isWe2aiApiError(e) ? e.code : null;
      const fallback = e instanceof Error ? e.message : "";
      setError(studioErrorMessage(t, ms, code, fallback));
      if (code && SESSION_ERROR_CODES.has(code)) onSessionMaybeEnded();
    },
    [t, ms, onSessionMaybeEnded],
  );

  const handlePickTemplate = (id: string) => {
    setTemplateId(id);
    setOverrides({});
  };

  const handleSelectModel = (id: string) => {
    selectModel(id);
    setOverrides({});
    setReferenceError(null);
  };

  const handlePickReference = async (file: File) => {
    setReferenceError(null);
    const result = await readReferenceFile(file);
    if (!result.ok) {
      setReferenceError(
        result.reason === "type"
          ? ms.referenceBadType
          : result.reason === "size"
            ? ms.referenceTooLarge
            : ms.errorMediaInvalid,
      );
      return;
    }
    setReference({
      reference: result.reference,
      previewUrl: result.previewUrl,
    });
  };

  const handleGenerate = async () => {
    if (!canGenerate || !model) return;
    const requestId = newRequestId();
    activeRef.current = requestId;
    setGenerating(true);
    setError(null);
    setNotice(null);
    try {
      const outcome = await we2aiApi.playgroundImage({
        requestId,
        keyId,
        model: model.id,
        prompt: prompt.trim(),
        n: model.capabilities.maxN > 1 ? n : undefined,
        size: size || undefined,
        aspectRatio: aspectRatio || undefined,
        imageSize: imageSize || undefined,
        reference: effectiveReference?.reference,
      });
      if (activeRef.current !== requestId) return;
      setImages(outcome.images);
      onWorkCompleted?.("image");
    } catch (e) {
      if (activeRef.current !== requestId) return;
      showError(e);
    } finally {
      if (activeRef.current === requestId) {
        activeRef.current = null;
        setGenerating(false);
      }
      onTerminal();
    }
  };

  const handleStop = () => {
    const id = activeRef.current;
    if (id) void we2aiApi.playgroundCancel(id);
  };

  const handleOptimize = async () => {
    if (!textModel) return;
    const summary = template.fields
      .map((f) => ({ f, v: (values[f.id] ?? "").trim() }))
      .filter(({ v }) => v !== "")
      .map(({ f, v }) => `${f.label}: ${v}`)
      .join("\n");
    const previous = prompt;
    setError(null);
    setDirty(true);
    setPrompt("");
    const result = await compile(
      buildCompileSystem("image", "en"),
      `${summary}\n\n---\n${previous}`,
      (delta) => setPrompt((p) => p + delta),
    );
    if (!result.ok) {
      setPrompt(previous);
      if (result.code !== undefined || result.message !== undefined) {
        setError(
          studioErrorMessage(t, ms, result.code ?? null, result.message ?? ""),
        );
        if (result.code && SESSION_ERROR_CODES.has(result.code))
          onSessionMaybeEnded();
      }
    }
    onTerminal();
  };

  const handleSaveAs = async (file: We2aiMediaFile) => {
    setNotice(null);
    try {
      const dest = await we2aiApi.playgroundMediaSaveAs(file.path);
      if (dest) setNotice(formatWe2aiString(ms.savedTo, { path: dest }));
    } catch (e) {
      showError(e);
    }
  };

  const handleReveal = async (file: We2aiMediaFile) => {
    try {
      await we2aiApi.playgroundMediaOpen(file.path, true);
    } catch {
      setError(ms.fileActionFailed);
    }
  };

  const sizeLabel = (value: string) => value.replace("x", " × ");

  return (
    <div
      className="flex min-w-0 flex-col gap-4"
      data-testid="we2ai-studio-image-panel"
    >
      <div className="flex flex-wrap items-center gap-3">
        <span className="we2ai-label">{ms.modelLabel}</span>
        {loading ? (
          <span className="text-sm" data-testid="we2ai-studio-image-loading">
            {ms.loadingModels}
          </span>
        ) : modelsError ? (
          <span
            className="text-sm"
            data-testid="we2ai-studio-image-models-error"
          >
            {modelsError}
          </span>
        ) : (models?.models.length ?? 0) === 0 ? (
          <span
            className="text-sm"
            data-testid="we2ai-studio-image-models-empty"
          >
            {models?.blockedReason
              ? `${t.studioBlockedPrefix}: ${models.blockedReason}`
              : ms.imageModelsEmpty}
          </span>
        ) : (
          <select
            value={model?.id ?? ""}
            onChange={(e) => handleSelectModel(e.target.value)}
            disabled={busy}
            aria-label={ms.modelLabel}
            data-testid="we2ai-studio-image-model"
            className={`${studioBoxClass} h-8 w-72 px-2 text-sm disabled:opacity-60`}
          >
            {(models?.models ?? []).map((m) => (
              <option key={m.id} value={m.id}>
                {m.id}
              </option>
            ))}
          </select>
        )}
      </div>

      <section className="space-y-2">
        <span className="we2ai-label">{ms.templatesLabel}</span>
        <TemplateChips
          templates={templates}
          activeId={template.id}
          onPick={handlePickTemplate}
          disabled={busy}
          testPrefix="we2ai-studio-image"
        />
      </section>

      <section className="space-y-2">
        <span className="we2ai-label">{ms.fieldsLabel}</span>
        <FieldsForm
          ms={ms}
          fields={template.fields}
          values={values}
          onChange={(id, v) => setValues((prev) => ({ ...prev, [id]: v }))}
          disabled={busy}
          testPrefix="we2ai-studio-image"
        />
      </section>

      <ReferencePicker
        ms={ms}
        supported={caps?.supportsReference ?? false}
        previewUrl={reference?.previewUrl ?? null}
        hint={ms.referenceHintImage}
        error={referenceError}
        onPick={(file) => void handlePickReference(file)}
        onClear={() => setReference(null)}
        disabled={busy}
        testPrefix="we2ai-studio-image"
      />

      <PromptEditor
        ms={ms}
        value={prompt}
        onChange={(v) => {
          setPrompt(v);
          setDirty(true);
        }}
        dirty={dirty}
        onReset={() => {
          setDirty(false);
          setPrompt(autoPrompt);
        }}
        onOptimize={() => void handleOptimize()}
        optimizing={compiling}
        canOptimize={textModel !== null}
        disabled={generating}
        testPrefix="we2ai-studio-image"
      />

      {caps && (
        <div className="flex flex-wrap items-center gap-4">
          {caps.sizes.length > 0 && (
            <StudioSelect
              label={ms.imageSizeLabel}
              value={size}
              options={caps.sizes.map((s) => ({
                value: s,
                label: sizeLabel(s),
              }))}
              onChange={(v) => setOverrides((o) => ({ ...o, size: v }))}
              disabled={busy}
              testId="we2ai-studio-image-size"
            />
          )}
          {caps.aspectRatios.length > 0 && (
            <StudioSelect
              label={ms.imageAspectLabel}
              value={aspectRatio}
              options={caps.aspectRatios.map((r) => ({ value: r, label: r }))}
              onChange={(v) => setOverrides((o) => ({ ...o, aspectRatio: v }))}
              disabled={busy}
              testId="we2ai-studio-image-aspect"
            />
          )}
          {caps.imageSizes.length > 0 && (
            <StudioSelect
              label={ms.imageQualityLabel}
              value={imageSize}
              options={caps.imageSizes.map((s) => ({ value: s, label: s }))}
              onChange={(v) => setOverrides((o) => ({ ...o, imageSize: v }))}
              disabled={busy}
              testId="we2ai-studio-image-quality"
            />
          )}
          {caps.maxN > 1 && (
            <StudioSelect
              label={ms.imageCountLabel}
              value={String(n)}
              options={Array.from({ length: caps.maxN }, (_, i) => ({
                value: String(i + 1),
                label: String(i + 1),
              }))}
              onChange={(v) => setOverrides((o) => ({ ...o, n: Number(v) }))}
              disabled={busy}
              testId="we2ai-studio-image-count"
            />
          )}
        </div>
      )}

      <div className="flex flex-wrap items-center gap-3">
        {generating ? (
          <Button
            size="sm"
            onClick={handleStop}
            className={studioButtonClass.ink}
            data-testid="we2ai-studio-image-stop"
          >
            <CircleStop className="h-3.5 w-3.5" />
            {ms.stop}
          </Button>
        ) : (
          <Button
            size="sm"
            onClick={() => void handleGenerate()}
            disabled={!canGenerate}
            className={studioButtonClass.ink}
            data-testid="we2ai-studio-image-generate"
          >
            <ImageIcon className="h-3.5 w-3.5" />
            {ms.imageGenerate}
          </Button>
        )}
        {generating && (
          <span className="text-xs" data-testid="we2ai-studio-image-progress">
            {ms.imageGenerating}
          </span>
        )}
        {!generating && missing.length > 0 && (
          <span
            className={`text-xs ${studioMutedClass}`}
            data-testid="we2ai-studio-image-missing"
          >
            {formatWe2aiString(ms.fillRequired, {
              fields: missing.map((f) => f.label).join("、"),
            })}
          </span>
        )}
        <span className={`text-xs ${studioMutedClass}`}>
          {ms.imageCostNote}
        </span>
      </div>

      {error && (
        <div
          className="rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-3 py-2 text-sm text-[var(--we2ai-danger,#b91c1c)]"
          data-testid="we2ai-studio-image-error"
        >
          {error}
        </div>
      )}
      {notice && (
        <div className="text-xs" data-testid="we2ai-studio-image-notice">
          {notice}
        </div>
      )}

      <div
        className={`${studioBoxClass} min-h-40 p-3`}
        data-testid="we2ai-studio-image-results"
      >
        {images.length === 0 ? (
          <p className={`text-sm ${studioMutedClass}`}>{ms.imageResultEmpty}</p>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2">
            {images.map((file, index) => (
              <figure key={file.path} className="space-y-2">
                {file.dataUrl && (
                  <img
                    src={file.dataUrl}
                    alt=""
                    className="w-full rounded-lg border-2 border-[var(--we2ai-ink)] object-contain"
                    data-testid={`we2ai-studio-image-result-${index}`}
                  />
                )}
                <figcaption className="flex flex-wrap gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void handleSaveAs(file)}
                    className={studioButtonClass.plain}
                    data-testid={`we2ai-studio-image-save-${index}`}
                  >
                    <Download className="h-3.5 w-3.5" />
                    {ms.saveAs}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void handleReveal(file)}
                    className={studioButtonClass.plain}
                    data-testid={`we2ai-studio-image-reveal-${index}`}
                  >
                    <FolderOpen className="h-3.5 w-3.5" />
                    {ms.revealFile}
                  </Button>
                </figcaption>
              </figure>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
