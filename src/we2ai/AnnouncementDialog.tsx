import { useMemo, type MouseEvent } from "react";
import { settingsApi } from "@/lib/api/settings";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { We2aiAnnouncement } from "./api";
import {
  isOpenableAnnouncementUrl,
  renderAnnouncementHtml,
} from "./announcementMarkdown";
import { formatWe2aiString, type We2aiStrings } from "./strings";

/**
 * 公告正文：Markdown 经净化后渲染。所有链接点击都被拦截，http / https 交给
 * `open_external` 在系统浏览器打开，绝不在应用 WebView 内导航。
 */
export function AnnouncementContent({ markdown }: { markdown: string }) {
  const html = useMemo(() => renderAnnouncementHtml(markdown), [markdown]);

  const handleClick = (event: MouseEvent<HTMLDivElement>) => {
    const target = event.target as Element | null;
    const anchor = target?.closest("a");
    if (!anchor || !event.currentTarget.contains(anchor)) return;
    event.preventDefault();
    const href = anchor.getAttribute("href");
    if (isOpenableAnnouncementUrl(href)) {
      void settingsApi.openExternal(href).catch((error) => {
        console.debug("[we2ai] open announcement link failed", error);
      });
    }
  };

  return (
    <div
      className="we2ai-announcement-content"
      onClick={handleClick}
      // 中键与链接上的右键菜单（含「打开链接」）同样不能触发 WebView 内导航。
      onAuxClick={(event) => event.preventDefault()}
      onContextMenu={(event) => {
        if ((event.target as Element | null)?.closest("a")) {
          event.preventDefault();
        }
      }}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

interface AnnouncementDialogProps {
  t: We2aiStrings;
  /** 为 `null` 时弹窗关闭。 */
  announcement: We2aiAnnouncement | null;
  /** 排在当前之后的未读弹窗公告条数。 */
  remaining: number;
  /** 关闭（Escape / 右上角关闭 / 「知道了」）；由调用方负责标记已读。 */
  onClose: () => void;
}

/** 单条公告弹窗：焦点被困在弹窗内，Escape 关闭。 */
export function AnnouncementDialog({
  t,
  announcement,
  remaining,
  onClose,
}: AnnouncementDialogProps) {
  return (
    <Dialog
      open={announcement !== null}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      {announcement && (
        <DialogContent
          key={announcement.id}
          // 点遮罩不关闭：只有 Escape、右上角关闭与「知道了」才算已读确认。
          onInteractOutside={(event) => event.preventDefault()}
          onPointerDownOutside={(event) => event.preventDefault()}
          className="we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]"
        >
          <DialogHeader className="flex-row items-start justify-between gap-3 border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <div className="min-w-0 space-y-1.5">
              <DialogTitle className="we2ai-heading break-words">
                {announcement.title}
              </DialogTitle>
              <DialogDescription className="we2ai-label">
                {t.announcementDialogDescription}
              </DialogDescription>
            </div>
            <button
              type="button"
              aria-label={t.announcementClose}
              onClick={onClose}
              className="we2ai-model-action shrink-0"
            >
              ×
            </button>
          </DialogHeader>
          <div className="we2ai-scroll min-h-0 flex-1 overflow-y-auto px-6 py-4">
            <AnnouncementContent markdown={announcement.content} />
          </div>
          <DialogFooter className="items-center border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            {remaining > 0 && (
              <span className="we2ai-label mr-auto">
                {formatWe2aiString(t.announcementQueueRemaining, {
                  count: remaining,
                })}
              </span>
            )}
            <button
              type="button"
              onClick={onClose}
              className="we2ai-model-action we2ai-model-action--selected"
            >
              {t.announcementGotIt}
            </button>
          </DialogFooter>
        </DialogContent>
      )}
    </Dialog>
  );
}
