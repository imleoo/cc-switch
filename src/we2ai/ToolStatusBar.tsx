import { settingsApi } from "@/lib/api/settings";
import type { We2aiToolStatusReport } from "./api";
import { WE2AI_TOOL_LABELS } from "./toolLabels";
import { formatWe2aiString, type We2aiStrings } from "./strings";

interface ToolStatusBarProps {
  t: We2aiStrings;
  report: We2aiToolStatusReport | null;
}

/**
 * 顶栏工具状态（方案第 1 节"顶栏状态刷新"、第 4.4 节）：每个工具显示已安装
 * 版本或官方下载链接，以及当前生效的 WE2AI 模型；CC Switch 也在运行时提示
 * 可能互相覆盖（方案 4.1 节）。
 */
export function ToolStatusBar({ t, report }: ToolStatusBarProps) {
  if (!report) return null;
  return (
    <div className="border-b" data-testid="we2ai-tool-status">
      {report.ccSwitchRunning && (
        <div
          role="status"
          className="bg-amber-50 px-4 py-2 text-xs text-amber-800 dark:bg-amber-950 dark:text-amber-200"
        >
          {t.ccSwitchRunningBanner}
        </div>
      )}
      <ul className="flex flex-wrap gap-x-6 gap-y-1 px-4 py-2 text-xs">
        {report.tools.map((tool) => (
          <li
            key={tool.tool}
            className="flex items-center gap-2"
            data-testid={`we2ai-tool-${tool.tool}`}
          >
            <span className="font-medium">{WE2AI_TOOL_LABELS[tool.tool]}</span>
            {tool.broken ? (
              <span className="text-destructive">{t.toolBroken}</span>
            ) : tool.installed ? (
              tool.version && (
                <span className="text-muted-foreground">{tool.version}</span>
              )
            ) : (
              <>
                <span className="text-muted-foreground">
                  {t.toolNotInstalled}
                </span>
                <button
                  type="button"
                  className="text-primary underline-offset-2 hover:underline"
                  onClick={() =>
                    void settingsApi.openExternal(tool.downloadUrl)
                  }
                >
                  {t.toolDownload}
                </button>
              </>
            )}
            <span className="text-muted-foreground">
              ·{" "}
              {tool.managedModel
                ? formatWe2aiString(t.toolCurrentModel, {
                    model: tool.managedModel,
                  })
                : t.toolNotConfigured}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export default ToolStatusBar;
