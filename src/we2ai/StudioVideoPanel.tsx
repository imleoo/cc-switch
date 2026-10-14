/**
 * AI 营销工坊 · 视频模式（功能 27，P3）。
 *
 * 模板（五镜头 / 三镜头）→ 字段 → 可编辑的提示词 → 提交并轮询。invoke 阻塞至终态，
 * 进度经 `we2ai-playground-progress` 事件推送。取消只放弃等待——网关没有文档化的
 * 取消接口，已提交的任务可能仍会计费，界面明示。成片由 Rust 下载落盘，应用内用资源
 * 协议预览，失败时回退到「用系统播放器打开」。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { CircleStop, Download, FolderOpen, Film, Play } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  isWe2aiApiError,
  we2aiApi,
  type We2aiMediaFile,
  type We2aiReferenceImage,
  type We2aiVideoProgressEvent,
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
  utf8Bytes,
} from "./studioMediaCommon";
import {
  GROK_VIDEO_PROMPT_MAX_BYTES,
  MEDIA_PROMPT_MAX_CHARS,
  buildCompileSystem,
  buildVideoPrompt,
  getStudioVideoTemplates,
  missingRequiredFields,
  videoPromptLanguage,
  type StudioValues,
} from "./studioMediaTemplates";
import type { StudioMediaStrings } from "./studioMediaStrings";
import {
  formatWe2aiString,
  type We2aiLanguage,
  type We2aiStrings,
} from "./strings";

interface StudioVideoPanelProps {
  t: We2aiStrings;
  ms: StudioMediaStrings;
  language: We2aiLanguage;
  keyId: number;
  textModel: string | null;
  onSessionMaybeEnded: () => void;
  onTerminal: () => void;
}

/** 默认时长取 10 秒（成本与五镜头结构的折中），没有 10 秒就取最长。 */
function defaultDuration(durations: number[]): number {
  if (durations.includes(10)) return 10;
  return durations[durations.length - 1] ?? 10;
}

function pickDefault(options: string[], preferred: string): string {
  return options.includes(preferred) ? preferred : (options[0] ?? "");
}

export function StudioVideoPanel({
  t,
  ms,
  language,
  keyId,
  textModel,
  onSessionMaybeEnded,
  onTerminal,
}: StudioVideoPanelProps) {
  const templates = useMemo(
    () => getStudioVideoTemplates(language),
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
  } = useMediaModels(keyId, "video", t.studioErrorNetwork, onSessionMaybeEnded);
  const caps = model?.capabilities ?? null;
  const family = model?.family ?? "grok";

  const [reference, setReference] = useState<{
    reference: We2aiReferenceImage;
    previewUrl: string;
  } | null>(null);
  const [referenceError, setReferenceError] = useState<string | null>(null);
  const effectiveReference = caps?.supportsReference ? reference : null;

  const [overrides, setOverrides] = useState<{
    duration?: number;
    aspectRatio?: string;
    resolution?: string;
    audio?: boolean;
  }>({});
  const duration =
    overrides.duration ?? (caps ? defaultDuration(caps.durations) : 10);
  const aspectRatio =
    overrides.aspectRatio ??
    (caps ? pickDefault(caps.aspectRatios, "16:9") : "");
  const resolution =
    overrides.resolution ?? (caps ? pickDefault(caps.resolutions, "720p") : "");
  const audio = (overrides.audio ?? false) && (caps?.supportsAudio ?? false);

  const promptLang = videoPromptLanguage(family);
  const autoPrompt = useMemo(
    () =>
      buildVideoPrompt(template.id, values, {
        lang: promptLang,
        duration,
        hasReference: effectiveReference !== null,
      }),
    [template.id, values, promptLang, duration, effectiveReference],
  );
  const [prompt, setPrompt] = useState(autoPrompt);
  const [dirty, setDirty] = useState(false);
  useEffect(() => {
    if (!dirty) setPrompt(autoPrompt);
  }, [autoPrompt, dirty]);

  const [generating, setGenerating] = useState(false);
  const [progress, setProgress] = useState<We2aiVideoProgressEvent | null>(
    null,
  );
  const [video, setVideo] = useState<We2aiMediaFile | null>(null);
  const [previewFailed, setPreviewFailed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const activeRef = useRef<string | null>(null);

  const { compiling, compile } = useStudioCompile(keyId, textModel);
  const busy = generating || compiling;
  const missing = missingRequiredFields(template.fields, values);
  const promptBytes = utf8Bytes(prompt);
  const isGrok = family === "grok";
  const promptOverLimit =
    prompt.length > MEDIA_PROMPT_MAX_CHARS ||
    (isGrok && promptBytes > GROK_VIDEO_PROMPT_MAX_BYTES);
  const canGenerate =
    !busy &&
    model !== null &&
    prompt.trim() !== "" &&
    missing.length === 0 &&
    !promptOverLimit;

  useEffect(
    () => () => {
      const id = activeRef.current;
      if (id) void we2aiApi.playgroundCancel(id).catch(() => {});
    },
    [],
  );

  // 进度事件：只认当前请求。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void we2aiApi
      .onPlaygroundProgress((event) => {
        if (event.requestId === activeRef.current) setProgress(event);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) =>
        console.debug("[we2ai] studio progress listener failed", e),
      );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const showError = useCallback(
    (e: unknown) => {
      const code = isWe2aiApiError(e) ? e.code : null;
      const fallback = e instanceof Error ? e.message : "";
      setError(studioErrorMessage(t, ms, code, fallback));
      if (code && SESSION_ERROR_CODES.has(code)) onSessionMaybeEnded();
    },
    [t, ms, onSessionMaybeEnded],
  );

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
    setProgress(null);
    setError(null);
    setNotice(null);
    setPreviewFailed(false);
    try {
      const outcome = await we2aiApi.playgroundVideo({
        requestId,
        keyId,
        model: model.id,
        prompt: prompt.trim(),
        duration,
        aspectRatio: aspectRatio || undefined,
        resolution: resolution || undefined,
        generateAudio: model.capabilities.supportsAudio ? audio : undefined,
        reference: effectiveReference?.reference,
      });
      if (activeRef.current !== requestId) return;
      setVideo(outcome.video);
    } catch (e) {
      if (activeRef.current !== requestId) return;
      showError(e);
    } finally {
      if (activeRef.current === requestId) {
        activeRef.current = null;
        setGenerating(false);
        setProgress(null);
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
      buildCompileSystem("video", promptLang),
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

  const handleOpen = async (file: We2aiMediaFile, reveal: boolean) => {
    try {
      await we2aiApi.playgroundMediaOpen(file.path, reveal);
    } catch {
      setError(ms.fileActionFailed);
    }
  };

  const stageLabel =
    progress?.stage === "downloading"
      ? ms.videoStageDownloading
      : progress?.stage === "running"
        ? ms.videoStageRunning
        : ms.videoStageSubmitted;

  return (
    <div
      className="flex min-w-0 flex-col gap-4"
      data-testid="we2ai-studio-video-panel"
    >
      <div className="flex flex-wrap items-center gap-3">
        <span className="we2ai-label">{ms.modelLabel}</span>
        {loading ? (
          <span className="text-sm" data-testid="we2ai-studio-video-loading">
            {ms.loadingModels}
          </span>
        ) : modelsError ? (
          <span
            className="text-sm"
            data-testid="we2ai-studio-video-models-error"
          >
            {modelsError}
          </span>
        ) : (models?.models.length ?? 0) === 0 ? (
          <span
            className="text-sm"
            data-testid="we2ai-studio-video-models-empty"
          >
            {models?.blockedReason
              ? `${t.studioBlockedPrefix}: ${models.blockedReason}`
              : ms.videoModelsEmpty}
          </span>
        ) : (
          <select
            value={model?.id ?? ""}
            onChange={(e) => handleSelectModel(e.target.value)}
            disabled={busy}
            aria-label={ms.modelLabel}
            data-testid="we2ai-studio-video-model"
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
          onPick={(id) => setTemplateId(id)}
          disabled={busy}
          testPrefix="we2ai-studio-video"
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
          testPrefix="we2ai-studio-video"
        />
      </section>

      <ReferencePicker
        ms={ms}
        supported={caps?.supportsReference ?? false}
        previewUrl={reference?.previewUrl ?? null}
        hint={
          caps && caps.minReferenceSide > 0
            ? `${ms.referenceHintVideo}；${formatWe2aiString(ms.referenceMinSide, { n: caps.minReferenceSide })}`
            : ms.referenceHintVideo
        }
        error={referenceError}
        onPick={(file) => void handlePickReference(file)}
        onClear={() => setReference(null)}
        disabled={busy}
        testPrefix="we2ai-studio-video"
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
        byteLimit={isGrok ? GROK_VIDEO_PROMPT_MAX_BYTES : undefined}
        byteCount={isGrok ? promptBytes : undefined}
        testPrefix="we2ai-studio-video"
      />

      {caps && (
        <div className="flex flex-wrap items-center gap-4">
          {caps.durations.length > 0 && (
            <StudioSelect
              label={ms.videoDurationLabel}
              value={String(duration)}
              options={caps.durations.map((d) => ({
                value: String(d),
                label: formatWe2aiString(ms.videoDurationUnit, { n: d }),
              }))}
              onChange={(v) =>
                setOverrides((o) => ({ ...o, duration: Number(v) }))
              }
              disabled={busy}
              testId="we2ai-studio-video-duration"
            />
          )}
          {caps.aspectRatios.length > 0 && (
            <StudioSelect
              label={ms.videoAspectLabel}
              value={aspectRatio}
              options={caps.aspectRatios.map((r) => ({ value: r, label: r }))}
              onChange={(v) => setOverrides((o) => ({ ...o, aspectRatio: v }))}
              disabled={busy}
              testId="we2ai-studio-video-aspect"
            />
          )}
          {caps.resolutions.length > 0 && (
            <StudioSelect
              label={ms.videoResolutionLabel}
              value={resolution}
              options={caps.resolutions.map((r) => ({ value: r, label: r }))}
              onChange={(v) => setOverrides((o) => ({ ...o, resolution: v }))}
              disabled={busy}
              testId="we2ai-studio-video-resolution"
            />
          )}
          {caps.supportsAudio && (
            <label className="flex items-center gap-2 text-xs">
              <input
                type="checkbox"
                checked={audio}
                disabled={busy}
                onChange={(e) =>
                  setOverrides((o) => ({ ...o, audio: e.target.checked }))
                }
                data-testid="we2ai-studio-video-audio"
              />
              <span className="we2ai-label">{ms.videoAudioLabel}</span>
            </label>
          )}
        </div>
      )}
      {duration < 10 && (
        <p className={`text-xs ${studioMutedClass}`}>
          {ms.videoShortDurationNote}
        </p>
      )}

      <div className="flex flex-wrap items-center gap-3">
        {generating ? (
          <Button
            size="sm"
            onClick={handleStop}
            className={studioButtonClass.ink}
            data-testid="we2ai-studio-video-stop"
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
            data-testid="we2ai-studio-video-generate"
          >
            <Film className="h-3.5 w-3.5" />
            {ms.videoGenerate}
          </Button>
        )}
        {generating && (
          <span className="text-xs" data-testid="we2ai-studio-video-progress">
            {stageLabel}
            {progress?.progress != null ? ` ${progress.progress}%` : ""}
            {progress
              ? ` · ${formatWe2aiString(ms.videoElapsed, { secs: progress.elapsedSecs })}`
              : ""}
          </span>
        )}
        {!generating && missing.length > 0 && (
          <span
            className={`text-xs ${studioMutedClass}`}
            data-testid="we2ai-studio-video-missing"
          >
            {formatWe2aiString(ms.fillRequired, {
              fields: missing.map((f) => f.label).join("、"),
            })}
          </span>
        )}
      </div>
      <p className={`text-xs ${studioMutedClass}`}>
        {ms.videoCostNote}
        {generating ? ` ${ms.videoCancelNote}` : ""}
      </p>

      {error && (
        <div
          className="rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-3 py-2 text-sm text-[var(--we2ai-danger,#b91c1c)]"
          data-testid="we2ai-studio-video-error"
        >
          {error}
        </div>
      )}
      {notice && (
        <div className="text-xs" data-testid="we2ai-studio-video-notice">
          {notice}
        </div>
      )}

      <div
        className={`${studioBoxClass} min-h-40 p-3`}
        data-testid="we2ai-studio-video-result"
      >
        {!video ? (
          <p className={`text-sm ${studioMutedClass}`}>{ms.videoResultEmpty}</p>
        ) : (
          <div className="space-y-2">
            {!previewFailed ? (
              <video
                src={convertFileSrc(video.path)}
                controls
                onError={() => setPreviewFailed(true)}
                className="max-h-96 w-full rounded-lg border-2 border-[var(--we2ai-ink)] bg-black"
                data-testid="we2ai-studio-video-player"
              />
            ) : (
              <p
                className="text-sm"
                data-testid="we2ai-studio-video-preview-failed"
              >
                {ms.videoPreviewFailed}
              </p>
            )}
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                variant="outline"
                onClick={() => void handleOpen(video, false)}
                className={studioButtonClass.plain}
                data-testid="we2ai-studio-video-open"
              >
                <Play className="h-3.5 w-3.5" />
                {ms.openFile}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => void handleOpen(video, true)}
                className={studioButtonClass.plain}
                data-testid="we2ai-studio-video-reveal"
              >
                <FolderOpen className="h-3.5 w-3.5" />
                {ms.revealFile}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => void handleSaveAs(video)}
                className={studioButtonClass.plain}
                data-testid="we2ai-studio-video-save"
              >
                <Download className="h-3.5 w-3.5" />
                {ms.saveAs}
              </Button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
