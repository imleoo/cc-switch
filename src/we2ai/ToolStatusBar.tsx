import { useRef, useState } from "react";
import { toast } from "sonner";
import { settingsApi } from "@/lib/api/settings";
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
  isWe2aiApiError,
  we2aiApi,
  type We2aiApplyPlan,
  type We2aiTool,
  type We2aiToolStatusReport,
} from "./api";
import { WE2AI_TOOL_LABELS } from "./toolLabels";
import { formatWe2aiString, type We2aiStrings } from "./strings";
import { extractErrorMessage } from "@/utils/errorUtils";

interface ToolStatusBarProps {
  t: We2aiStrings;
  report: We2aiToolStatusReport | null;
  /** 恢复官方配置成功后调用，供上层刷新顶栏工具状态（方案 P6）。 */
  onRestored?: () => void;
}

/**
 * 顶栏工具状态（方案第 1 节"顶栏状态刷新"、第 4.4 节）：每个工具显示已安装
 * 版本或官方下载链接，以及当前生效的 WE2AI 模型；CC Switch 也在运行时提示
 * 可能互相覆盖（方案 4.1 节）。
 */
export function ToolStatusBar({ t, report, onRestored }: ToolStatusBarProps) {
  const [confirmTool, setConfirmTool] = useState<We2aiTool | null>(null);
  const [plan, setPlan] = useState<We2aiApplyPlan | null>(null);
  const [planFailed, setPlanFailed] = useState(false);
  const [restoring, setRestoring] = useState(false);
  // 用请求序号丢弃过期的 restorePlan 响应：快速切换"打开 A → 关闭 → 打开
  // B"时，A 的响应可能比 B 的更晚到达，不能让 A 的结果盖掉 B 正在展示的
  // 弹窗（Opus 复核中危项 4）。
  const planRequestIdRef = useRef(0);

  const openConfirm = (tool: We2aiTool) => {
    const requestId = ++planRequestIdRef.current;
    setConfirmTool(tool);
    setPlan(null);
    setPlanFailed(false);
    // 恢复用独立的 restorePlan：applyPlan 展示的是"将写入什么"，
    // env.ANTHROPIC_API_KEY（删除）与 Codex 模型目录文件在恢复场景下是反的
    // （恢复是写回 Key、从不碰模型目录文件）。
    we2aiApi
      .restorePlan(tool)
      .then((p) => {
        if (planRequestIdRef.current === requestId) setPlan(p);
      })
      .catch(() => {
        if (planRequestIdRef.current === requestId) setPlanFailed(true);
      });
  };

  const closeConfirm = () => {
    if (restoring) return;
    setConfirmTool(null);
  };

  const handleRestore = async () => {
    if (!confirmTool) return;
    setRestoring(true);
    try {
      const outcome = await we2aiApi.restoreOfficial([confirmTool]);
      if (outcome.restored.length > 0) {
        toast.success(
          formatWe2aiString(t.toolsRestored, {
            count: outcome.restored.length,
          }),
        );
      }
      if (outcome.skipped.length > 0) {
        toast.warning(t.toolsRestoreSkipped, {
          description: outcome.skipped.join("\n"),
        });
      }
      if (outcome.restored.length === 0 && outcome.skipped.length === 0) {
        toast.info(t.toolsRestoreNoop);
      }
      onRestored?.();
    } catch (error) {
      toast.error(t.toolsRestoreFailed, {
        description: isWe2aiApiError(error)
          ? error.message
          : extractErrorMessage(error) || undefined,
      });
    } finally {
      setRestoring(false);
      setConfirmTool(null);
    }
  };

  if (!report) return null;
  return (
    <div
      className="we2ai-ticker border-b-[2.5px] border-[var(--we2ai-ink)]"
      data-testid="we2ai-tool-status"
    >
      {report.ccSwitchRunning && (
        <div
          role="status"
          className="we2ai-ticker-item border-b border-[color:color-mix(in_srgb,var(--we2ai-paper)_25%,transparent)] bg-[var(--we2ai-orange)] px-4 py-1.5 text-[var(--we2ai-paper)]"
        >
          {t.ccSwitchRunningBanner}
        </div>
      )}
      <ul className="flex flex-wrap gap-x-6 gap-y-1 px-4 py-2">
        {report.tools.map((tool) => (
          <li
            key={tool.tool}
            className="we2ai-ticker-item flex items-center gap-2"
            data-testid={`we2ai-tool-${tool.tool}`}
          >
            <span>{WE2AI_TOOL_LABELS[tool.tool]}</span>
            {tool.broken ? (
              <span className="text-[var(--we2ai-orange)]">{t.toolBroken}</span>
            ) : tool.installed ? (
              tool.version && (
                <span className="normal-case tracking-normal text-[color:color-mix(in_srgb,var(--we2ai-paper)_60%,transparent)]">
                  {tool.version}
                </span>
              )
            ) : (
              <>
                <span className="text-[color:color-mix(in_srgb,var(--we2ai-paper)_60%,transparent)]">
                  {t.toolNotInstalled}
                </span>
                <button
                  type="button"
                  className="underline-offset-2 hover:text-[var(--we2ai-orange)] hover:underline"
                  onClick={() =>
                    void settingsApi.openExternal(tool.downloadUrl)
                  }
                >
                  {t.toolDownload}
                </button>
              </>
            )}
            <span className="normal-case tracking-normal text-[color:color-mix(in_srgb,var(--we2ai-paper)_60%,transparent)]">
              ·{" "}
              {tool.managedModel
                ? formatWe2aiString(t.toolCurrentModel, {
                    model: tool.managedModel,
                  })
                : t.toolNotConfigured}
            </span>
            {tool.managedModel && (
              <button
                type="button"
                className="underline-offset-2 hover:text-[var(--we2ai-orange)] hover:underline"
                onClick={() => openConfirm(tool.tool)}
              >
                {t.restoreOfficialAction}
              </button>
            )}
          </li>
        ))}
      </ul>

      <Dialog
        open={confirmTool != null}
        onOpenChange={(open) => !open && closeConfirm()}
      >
        <DialogContent className="we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]">
          <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <DialogTitle className="we2ai-heading">
              {formatWe2aiString(t.restoreOfficialConfirmTitle, {
                tool: confirmTool ? WE2AI_TOOL_LABELS[confirmTool] : "",
              })}
            </DialogTitle>
            <DialogDescription>
              {t.restoreOfficialConfirmDescription}
            </DialogDescription>
          </DialogHeader>
          {plan && (
            <div
              className="mx-6 space-y-2 border-2 border-[var(--we2ai-ink)] p-3 text-xs"
              data-testid="we2ai-restore-plan"
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
            </div>
          )}
          {planFailed && (
            <p role="alert" className="mx-6 text-xs text-[var(--we2ai-orange)]">
              {t.applyPlanFailed}
            </p>
          )}
          <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <Button
              variant="outline"
              disabled={restoring}
              onClick={closeConfirm}
              className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
            >
              {t.applyCancel}
            </Button>
            <Button
              disabled={restoring || !plan}
              onClick={() => void handleRestore()}
              className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]"
            >
              {restoring ? t.restoring : t.restoreOfficialConfirmConfirm}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default ToolStatusBar;
