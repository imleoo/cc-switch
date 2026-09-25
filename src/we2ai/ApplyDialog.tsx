import { useEffect, useState } from "react";
import { toast } from "sonner";
import "./we2ai-theme.css";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
  type We2aiApplyOutcome,
  type We2aiApplyPlan,
  type We2aiTool,
} from "./api";
import { WE2AI_TOOL_LABELS } from "./toolLabels";
import {
  formatWe2aiString,
  getWe2aiErrorMessage,
  type We2aiStrings,
} from "./strings";

/** 这些错误码的 Rust 文案含具体路径或原因，直接展示。 */
const DETAIL_CODES = new Set([
  "CONFIG_DIR_NOT_PRIVATE",
  "APPLY_FAILED",
  "APPLY_READBACK_MISMATCH",
  "APPLY_EXTERNAL_MODIFICATION",
  "APPLY_ROLLBACK_INCOMPLETE",
]);

export function describeApplyError(t: We2aiStrings, error: unknown): string {
  if (!isWe2aiApiError(error)) return t.errorApplyGeneric;
  switch (error.code) {
    case "TAKEOVER_CONFLICT":
      return t.errorApplyTakeover;
    case "TAKEOVER_DETECTED":
      return t.errorApplyTakeoverDetected;
    case "PROVIDER_PRECONDITION":
      return t.errorApplyPrecondition;
    case "WORKBUDDY_CONCURRENT_MODIFICATION":
      return t.errorApplyConcurrent;
    case "KEY_NOT_FOUND":
    case "NO_ACTIVE_SESSION":
      return getWe2aiErrorMessage(t, error.code);
    default:
      return DETAIL_CODES.has(error.code)
        ? `${t.errorApplyGeneric}：${error.message}`
        : t.errorApplyGeneric;
  }
}

/** 槽位下拉里"与主模型相同"的取值。 */
const SAME_AS_MAIN = "__same__";

export interface ApplyTarget {
  tool: We2aiTool;
  model: string;
}

interface ApplyDialogProps {
  t: We2aiStrings;
  keyId: number;
  target: ApplyTarget | null;
  /** 该 Key 下支持 Claude Code 的模型，供"高级"槽位选择。 */
  claudeModels: string[];
  toolInstalled: boolean;
  onClose: () => void;
  onApplied: (outcome: We2aiApplyOutcome) => void;
}

/**
 * 确认弹窗（方案第 1 节）：展示将写入的文件与字段；Claude Code 可在"高级"
 * 里分别指定三个槽位；WorkBuddy 同名条目需要二次确认覆盖（方案 4.3 节）。
 */
export function ApplyDialog({
  t,
  keyId,
  target,
  claudeModels,
  toolInstalled,
  onClose,
  onApplied,
}: ApplyDialogProps) {
  const [plan, setPlan] = useState<We2aiApplyPlan | null>(null);
  const [planFailed, setPlanFailed] = useState(false);
  const [applying, setApplying] = useState(false);
  const [needsOverwrite, setNeedsOverwrite] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [slots, setSlots] = useState({
    sonnet: SAME_AS_MAIN,
    opus: SAME_AS_MAIN,
    haiku: SAME_AS_MAIN,
  });

  useEffect(() => {
    setPlan(null);
    setPlanFailed(false);
    setNeedsOverwrite(false);
    setShowAdvanced(false);
    setSlots({ sonnet: SAME_AS_MAIN, opus: SAME_AS_MAIN, haiku: SAME_AS_MAIN });
    if (!target) return;
    let cancelled = false;
    we2aiApi
      .applyPlan(target.tool)
      .then((p) => {
        if (!cancelled) setPlan(p);
      })
      .catch(() => {
        // 不展示将写入的文件与字段就不允许确认（方案第 1 节）。
        if (!cancelled) setPlanFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [target]);

  if (!target) return null;
  const toolLabel = WE2AI_TOOL_LABELS[target.tool];

  const slotValue = (v: string) => (v === SAME_AS_MAIN ? null : v);

  const handleApply = async (overwrite: boolean) => {
    setApplying(true);
    try {
      const outcome = await we2aiApi.applyModel({
        tool: target.tool,
        keyId,
        model: target.model,
        claudeSlots:
          target.tool === "claude_code"
            ? {
                sonnet: slotValue(slots.sonnet),
                opus: slotValue(slots.opus),
                haiku: slotValue(slots.haiku),
              }
            : undefined,
        overwrite,
      });
      const notes = [...outcome.warnings];
      if (!toolInstalled) {
        notes.push(
          formatWe2aiString(t.applyToolNotInstalled, { tool: toolLabel }),
        );
      }
      toast.success(
        formatWe2aiString(t.applySuccess, {
          model: outcome.model,
          tool: toolLabel,
        }),
        notes.length > 0 ? { description: notes.join("\n") } : undefined,
      );
      onApplied(outcome);
      onClose();
    } catch (error) {
      if (
        isWe2aiApiError(error) &&
        error.code === "WORKBUDDY_CONFIRM_OVERWRITE"
      ) {
        setNeedsOverwrite(true);
      } else {
        toast.error(describeApplyError(t, error));
        onClose();
      }
    } finally {
      setApplying(false);
    }
  };

  const slotSelect = (key: "sonnet" | "opus" | "haiku", label: string) => (
    <div className="flex items-center justify-between gap-3" key={key}>
      <span className="text-xs">{label}</span>
      <Select
        value={slots[key]}
        onValueChange={(v) => setSlots((s) => ({ ...s, [key]: v }))}
      >
        <SelectTrigger
          className="h-8 w-56 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-xs text-[var(--we2ai-ink)] shadow-none"
          aria-label={label}
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
          <SelectItem value={SAME_AS_MAIN}>{target.model}</SelectItem>
          {claudeModels
            .filter((m) => m !== target.model)
            .map((m) => (
              <SelectItem key={m} value={m}>
                {m}
              </SelectItem>
            ))}
        </SelectContent>
      </Select>
    </div>
  );

  const primaryButtonClass =
    "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]";
  const secondaryButtonClass =
    "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]";

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !applying) onClose();
      }}
    >
      <DialogContent className="we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]">
        {needsOverwrite ? (
          <>
            <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
              <DialogTitle className="we2ai-heading">
                {t.workbuddyOverwriteTitle}
              </DialogTitle>
              <DialogDescription>
                {formatWe2aiString(t.workbuddyOverwriteDescription, {
                  model: target.model,
                })}
              </DialogDescription>
            </DialogHeader>
            <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
              <Button
                variant="outline"
                disabled={applying}
                onClick={onClose}
                className={secondaryButtonClass}
              >
                {t.applyCancel}
              </Button>
              <Button
                disabled={applying}
                onClick={() => void handleApply(true)}
                className={primaryButtonClass}
              >
                {applying ? t.applying : t.workbuddyOverwriteConfirm}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
              <DialogTitle className="we2ai-heading">
                {formatWe2aiString(t.applyConfirmTitle, {
                  model: target.model,
                  tool: toolLabel,
                })}
              </DialogTitle>
              <DialogDescription>{t.applyConfirmDescription}</DialogDescription>
            </DialogHeader>
            {plan && (
              <div
                className="mx-6 space-y-2 border-2 border-[var(--we2ai-ink)] p-3 text-xs"
                data-testid="we2ai-apply-plan"
              >
                <div>
                  <div className="we2ai-label">{t.applyFilesLabel}</div>
                  {plan.files.map((f) => (
                    <div key={f} className="break-all font-mono">
                      {f}
                    </div>
                  ))}
                </div>
                <div>
                  <div className="we2ai-label">{t.applyFieldsLabel}</div>
                  <div className="font-mono">{plan.fields.join("、")}</div>
                </div>
              </div>
            )}
            {planFailed && (
              <p
                role="alert"
                className="mx-6 text-xs text-[var(--we2ai-orange)]"
              >
                {t.applyPlanFailed}
              </p>
            )}
            {target.tool === "claude_code" && claudeModels.length > 1 && (
              <div className="mx-6 space-y-2">
                <button
                  type="button"
                  className="we2ai-label underline-offset-2 hover:text-[var(--we2ai-orange)] hover:underline"
                  onClick={() => setShowAdvanced((v) => !v)}
                >
                  {t.applyAdvanced}
                </button>
                {showAdvanced && (
                  <div className="space-y-2">
                    {slotSelect("sonnet", t.slotSonnet)}
                    {slotSelect("opus", t.slotOpus)}
                    {slotSelect("haiku", t.slotHaiku)}
                  </div>
                )}
              </div>
            )}
            <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
              <Button
                variant="outline"
                disabled={applying}
                onClick={onClose}
                className={secondaryButtonClass}
              >
                {t.applyCancel}
              </Button>
              <Button
                disabled={applying || !plan}
                onClick={() => void handleApply(false)}
                className={primaryButtonClass}
              >
                {applying ? t.applying : t.applyConfirm}
              </Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

export default ApplyDialog;
