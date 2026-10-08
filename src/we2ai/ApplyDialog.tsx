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
  // 偏差修复项 A：数据根权限收紧失败时的具体路径与原因，同样值得展示，
  // 不能落进只显示通用文案的 default 分支。
  "DATA_ROOT_NOT_HARDENED",
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
    // 偏差修复项 B 的 L6 追加：确认期间配置被外部改动，计划已经过时。
    case "EXTRA_CHANGES_STALE":
      return t.errorApplyExtraChangesStale;
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
  /**
   * CC Switch 也在运行时提示——此前只有顶栏 `ToolStatusBar` 展示这个警告，
   * 确认弹窗本身看不到（Opus 复核低危项 L5）。用户此刻正准备点击"确认"
   * 真正写入，比顶栏更需要在这个时间点看到提示。
   */
  ccSwitchRunning: boolean;
  /**
   * 打开确认弹窗时触发一次工具状态检测（含 CC Switch 是否在运行），并在
   * 这里等待它完成（Codex 验收 X5）：此前调用方只是发起就不再等待，确认
   * 按钮不受这次检测约束——用户可能在检测结果（尤其是新出现的并存警告）
   * 返回之前就已经点了确认。改为本组件自己持有一个"正在检测"状态，检测
   * 完成前禁用确认按钮。返回 `false`（Codex 验收 Y1）表示这次检测在超时
   * 前没能完成（网络异常等）：不再继续阻塞确认按钮，改为展示一条"未能
   * 完成检测"的提示，让用户知情后自行决定是否继续。
   */
  onBeforeApplyDialogOpen?: () => Promise<boolean>;
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
  ccSwitchRunning,
  onBeforeApplyDialogOpen,
  onClose,
  onApplied,
}: ApplyDialogProps) {
  const [plan, setPlan] = useState<We2aiApplyPlan | null>(null);
  const [planFailed, setPlanFailed] = useState(false);
  const [applying, setApplying] = useState(false);
  const [needsOverwrite, setNeedsOverwrite] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(true);
  // Codex 验收 X5：本次打开弹窗触发的工具状态检测是否仍在进行。确认按钮
  // 在它完成前必须禁用，否则用户可能在新的并存警告返回之前就已经点了
  // 确认。没有传入回调（如旧版本调用方）时视为"从不检测"，不阻塞确认。
  const [checkingToolStatus, setCheckingToolStatus] = useState(false);
  // Codex 验收 Y1：本次检测是否在超时前完成。超时后不再继续禁用确认
  // 按钮（见 `checkingToolStatus` 的清理逻辑），只展示一条提示。
  const [detectionIncomplete, setDetectionIncomplete] = useState(false);
  const [slots, setSlots] = useState({
    sonnet: SAME_AS_MAIN,
    opus: SAME_AS_MAIN,
    haiku: SAME_AS_MAIN,
  });

  useEffect(() => {
    setPlan(null);
    setPlanFailed(false);
    setNeedsOverwrite(false);
    setShowAdvanced(true);
    setSlots({ sonnet: SAME_AS_MAIN, opus: SAME_AS_MAIN, haiku: SAME_AS_MAIN });
    setCheckingToolStatus(false);
    setDetectionIncomplete(false);
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
    if (onBeforeApplyDialogOpen) {
      setCheckingToolStatus(true);
      void onBeforeApplyDialogOpen()
        .then((completed) => {
          if (!cancelled) setDetectionIncomplete(!completed);
        })
        .finally(() => {
          if (!cancelled) setCheckingToolStatus(false);
        });
    }
    return () => {
      cancelled = true;
    };
  }, [target, onBeforeApplyDialogOpen]);

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
        expectedExtraChanges: plan?.extraChanges.map((c) => c.id) ?? [],
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
                disabled={applying || checkingToolStatus}
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
            {ccSwitchRunning && (
              <p
                role="alert"
                data-testid="we2ai-apply-other-tool-running"
                className="mx-6 text-xs text-[var(--we2ai-orange)]"
              >
                {t.ccSwitchRunningBanner}
              </p>
            )}
            {detectionIncomplete && (
              <p
                role="alert"
                data-testid="we2ai-apply-detection-incomplete"
                className="mx-6 text-xs text-[var(--we2ai-orange)]"
              >
                {t.applyDetectionIncomplete}
              </p>
            )}
            {plan && (
              <div
                className="mx-6 space-y-2 border-2 border-[var(--we2ai-ink)] p-3 text-xs"
                data-testid="we2ai-apply-plan"
              >
                <div>
                  <div className="we2ai-label">{t.applyFilesLabel}</div>
                  {plan.files.map((f) => (
                    <div key={f.path} className="break-all font-mono">
                      {f.display}
                    </div>
                  ))}
                </div>
                <div>
                  <div className="we2ai-label">{t.applyFieldsLabel}</div>
                  <div className="font-mono">{plan.fields.join("、")}</div>
                </div>
                {plan.extraChanges.length > 0 && (
                  <div data-testid="we2ai-apply-extra-changes">
                    <div className="we2ai-label text-[var(--we2ai-orange)]">
                      {t.applyExtraChangesLabel}
                    </div>
                    {plan.extraChanges.map((c) => (
                      <div key={c.id} className="break-all font-mono">
                        {c.display}
                      </div>
                    ))}
                  </div>
                )}
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
                disabled={applying || !plan || checkingToolStatus}
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
