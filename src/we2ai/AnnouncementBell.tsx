import { useState } from "react";
import { Bell } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { We2aiAnnouncement } from "./api";
import { formatWe2aiString, type We2aiStrings } from "./strings";

interface AnnouncementBellProps {
  t: We2aiStrings;
  /** 最新的在前。 */
  items: We2aiAnnouncement[];
  /** 是否至少成功拉取过一次；未加载时不能显示「暂无公告」。 */
  loaded: boolean;
  unreadCount: number;
  onOpenItem: (id: number) => void;
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString();
}

/** 顶栏铃铛：带未读数角标，点击打开公告列表（未读加粗，可看已读）。 */
export function AnnouncementBell({
  t,
  items,
  loaded,
  unreadCount,
  onOpenItem,
}: AnnouncementBellProps) {
  const [open, setOpen] = useState(false);
  const label =
    unreadCount > 0
      ? formatWe2aiString(t.announcementBellUnread, { count: unreadCount })
      : t.announcementBell;

  return (
    <>
      <button
        type="button"
        aria-label={label}
        aria-haspopup="dialog"
        onClick={() => setOpen(true)}
        className="we2ai-bell"
      >
        <Bell className="h-4 w-4" aria-hidden="true" />
        {unreadCount > 0 && (
          <span className="we2ai-bell-badge" aria-hidden="true">
            {unreadCount > 99 ? "99+" : unreadCount}
          </span>
        )}
      </button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]">
          <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <DialogTitle className="we2ai-heading">
              {t.announcementListTitle}
            </DialogTitle>
            <DialogDescription>{t.announcementListDescription}</DialogDescription>
          </DialogHeader>
          <div className="we2ai-scroll min-h-0 flex-1 overflow-y-auto px-6 py-4">
            {items.length === 0 ? (
              loaded && (
                <p className="we2ai-label py-6 text-center">
                  {t.announcementListEmpty}
                </p>
              )
            ) : (
              <ul className="space-y-3">
                {items.map((a) => {
                  const unread = !a.readAt;
                  return (
                    <li key={a.id}>
                      <button
                        type="button"
                        data-unread={unread ? "true" : "false"}
                        onClick={() => {
                          setOpen(false);
                          onOpenItem(a.id);
                        }}
                        className="we2ai-announcement-item"
                      >
                        <span className="we2ai-announcement-item-title">
                          {a.title}
                        </span>
                        <span className="we2ai-announcement-item-meta">
                          <span className="we2ai-chip">
                            {unread ? t.announcementUnreadTag : t.announcementReadTag}
                          </span>
                          <span>{formatDate(a.createdAt)}</span>
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
          <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="we2ai-model-action we2ai-model-action--selected"
            >
              {t.announcementClose}
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
