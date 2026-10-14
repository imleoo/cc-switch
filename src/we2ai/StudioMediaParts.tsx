/**
 * 图片 / 视频面板共用的 UI 片段（功能 27，P2/P3）。
 * 视觉沿用工坊的 neo-brutalist 样式变量；下拉用原生 select，保证可访问与可测。
 */

import { useRef } from "react";
import { ImagePlus, Sparkles, Undo2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { StudioField, StudioValues } from "./studioMediaTemplates";
import { formatWe2aiString } from "./strings";
import type { StudioMediaStrings } from "./studioMediaStrings";

const BOX =
  "rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]";
const MUTED =
  "text-[color:color-mix(in_srgb,var(--we2ai-ink)_55%,transparent)]";
const INK_BUTTON =
  "h-8 gap-1.5 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-xs text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]";
const PLAIN_BUTTON =
  "h-8 gap-1.5 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-xs text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]";

export const studioButtonClass = { ink: INK_BUTTON, plain: PLAIN_BUTTON };
export const studioMutedClass = MUTED;
export const studioBoxClass = BOX;

export function StudioSelect({
  label,
  value,
  options,
  onChange,
  disabled,
  testId,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
  disabled?: boolean;
  testId: string;
}) {
  return (
    <label className="flex items-center gap-2 text-xs">
      <span className="we2ai-label">{label}</span>
      <select
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        data-testid={testId}
        className={`${BOX} h-8 px-2 text-xs disabled:opacity-60`}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}

export function TemplateChips({
  templates,
  activeId,
  onPick,
  disabled,
  testPrefix,
}: {
  templates: { id: string; title: string; hint: string }[];
  activeId: string | null;
  onPick: (id: string) => void;
  disabled?: boolean;
  testPrefix: string;
}) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {templates.map((tpl) => (
        <button
          key={tpl.id}
          type="button"
          disabled={disabled}
          title={tpl.hint}
          onClick={() => onPick(tpl.id)}
          data-testid={`${testPrefix}-template-${tpl.id}`}
          aria-pressed={activeId === tpl.id}
          className={`rounded-lg border-2 border-[var(--we2ai-ink)] px-3 py-1.5 text-sm shadow-none transition-colors disabled:opacity-60 ${
            activeId === tpl.id
              ? "bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)]"
              : "bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] hover:bg-[var(--we2ai-paper-2)]"
          }`}
        >
          {tpl.title}
        </button>
      ))}
    </div>
  );
}

export function FieldsForm({
  ms,
  fields,
  values,
  onChange,
  disabled,
  testPrefix,
}: {
  ms: StudioMediaStrings;
  fields: StudioField[];
  values: StudioValues;
  onChange: (id: string, value: string) => void;
  disabled?: boolean;
  testPrefix: string;
}) {
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      {fields.map((field) => (
        <label key={field.id} className="flex flex-col gap-1 text-xs">
          <span className="we2ai-label">
            {field.label}
            {field.required ? (
              <span className="ml-1 text-[var(--we2ai-danger,#b91c1c)]">
                · {ms.requiredMark}
              </span>
            ) : null}
          </span>
          <input
            type="text"
            value={values[field.id] ?? ""}
            placeholder={field.placeholder}
            disabled={disabled}
            onChange={(e) => onChange(field.id, e.target.value)}
            data-testid={`${testPrefix}-field-${field.id}`}
            className={`${BOX} h-9 px-2 text-sm placeholder:text-[color:color-mix(in_srgb,var(--we2ai-ink)_40%,transparent)] disabled:opacity-60`}
          />
        </label>
      ))}
    </div>
  );
}

export function PromptEditor({
  ms,
  value,
  onChange,
  dirty,
  onReset,
  onOptimize,
  optimizing,
  canOptimize,
  disabled,
  byteLimit,
  byteCount,
  testPrefix,
}: {
  ms: StudioMediaStrings;
  value: string;
  onChange: (value: string) => void;
  dirty: boolean;
  onReset: () => void;
  onOptimize: () => void;
  optimizing: boolean;
  /** 该 Key 下有可用文本模型；否则「AI 优化」置灰并说明。 */
  canOptimize: boolean;
  disabled?: boolean;
  byteLimit?: number;
  byteCount?: number;
  testPrefix: string;
}) {
  const overLimit =
    byteLimit !== undefined && byteCount !== undefined && byteCount > byteLimit;
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="we2ai-label">{ms.promptLabel}</span>
        {dirty && (
          <span
            className={`text-xs ${MUTED}`}
            data-testid={`${testPrefix}-prompt-dirty`}
          >
            {ms.promptEdited}
          </span>
        )}
      </div>
      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={8}
        disabled={disabled}
        data-testid={`${testPrefix}-prompt`}
        className={`${BOX} w-full resize-y p-3 text-sm leading-relaxed focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--we2ai-ink)] disabled:opacity-60`}
      />
      <div className="flex flex-wrap items-center gap-2">
        <Button
          size="sm"
          variant="outline"
          onClick={onOptimize}
          disabled={
            disabled || !canOptimize || optimizing || value.trim() === ""
          }
          title={canOptimize ? undefined : ms.aiOptimizeNeedText}
          className={PLAIN_BUTTON}
          data-testid={`${testPrefix}-optimize`}
        >
          <Sparkles className="h-3.5 w-3.5" />
          {optimizing ? ms.aiOptimizing : ms.aiOptimize}
        </Button>
        <Button
          size="sm"
          variant="outline"
          onClick={onReset}
          disabled={disabled || !dirty}
          className={PLAIN_BUTTON}
          data-testid={`${testPrefix}-prompt-reset`}
        >
          <Undo2 className="h-3.5 w-3.5" />
          {ms.promptReset}
        </Button>
        {byteLimit !== undefined && byteCount !== undefined && (
          <span
            className={`ml-auto text-xs ${overLimit ? "text-[var(--we2ai-danger,#b91c1c)]" : MUTED}`}
            data-testid={`${testPrefix}-prompt-bytes`}
          >
            {formatWe2aiString(ms.promptBytes, {
              used: byteCount,
              max: byteLimit,
            })}
            {overLimit ? ` · ${ms.promptTooLong}` : ""}
          </span>
        )}
      </div>
    </div>
  );
}

export function ReferencePicker({
  ms,
  supported,
  previewUrl,
  hint,
  error,
  onPick,
  onClear,
  disabled,
  testPrefix,
}: {
  ms: StudioMediaStrings;
  supported: boolean;
  previewUrl: string | null;
  hint: string;
  error: string | null;
  onPick: (file: File) => void;
  onClear: () => void;
  disabled?: boolean;
  testPrefix: string;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  return (
    <div className="space-y-2">
      <span className="we2ai-label">{ms.referenceLabel}</span>
      {!supported ? (
        <p
          className={`text-xs ${MUTED}`}
          data-testid={`${testPrefix}-reference-unsupported`}
        >
          {ms.referenceUnsupported}
        </p>
      ) : (
        <div className="flex flex-wrap items-center gap-3">
          <input
            ref={inputRef}
            type="file"
            accept="image/png,image/jpeg,image/webp"
            className="hidden"
            data-testid={`${testPrefix}-reference-input`}
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) onPick(file);
              e.target.value = "";
            }}
          />
          {previewUrl ? (
            <>
              <img
                src={previewUrl}
                alt=""
                className="h-16 w-16 rounded-lg border-2 border-[var(--we2ai-ink)] object-cover"
                data-testid={`${testPrefix}-reference-preview`}
              />
              <Button
                size="sm"
                variant="outline"
                onClick={onClear}
                disabled={disabled}
                className={PLAIN_BUTTON}
                data-testid={`${testPrefix}-reference-clear`}
              >
                <X className="h-3.5 w-3.5" />
                {ms.referenceRemove}
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              variant="outline"
              onClick={() => inputRef.current?.click()}
              disabled={disabled}
              className={PLAIN_BUTTON}
              data-testid={`${testPrefix}-reference-add`}
            >
              <ImagePlus className="h-3.5 w-3.5" />
              {ms.referenceAdd}
            </Button>
          )}
          <span className={`text-xs ${MUTED}`}>{hint}</span>
        </div>
      )}
      {error && (
        <p
          className="text-xs text-[var(--we2ai-danger,#b91c1c)]"
          data-testid={`${testPrefix}-reference-error`}
        >
          {error}
        </p>
      )}
    </div>
  );
}
