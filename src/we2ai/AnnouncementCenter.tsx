import { AnnouncementBell } from "./AnnouncementBell";
import { AnnouncementDialog } from "./AnnouncementDialog";
import type { We2aiStrings } from "./strings";
import { useAnnouncements } from "./useAnnouncements";

/**
 * 公告入口：顶栏铃铛 + 弹窗队列。必须以会话身份（区域 + 账号）作 `key` 挂载，
 * 换账号或区域时整体重建，不沿用上一会话的公告。
 */
export function AnnouncementCenter({
  t,
  onSessionMaybeEnded,
}: {
  t: We2aiStrings;
  /** 拉取/标记已读遇到非网络类错误码时调用，让外壳复查会话状态。 */
  onSessionMaybeEnded?: () => void;
}) {
  const {
    listItems,
    loaded,
    unreadCount,
    current,
    remaining,
    openItem,
    closeCurrent,
  } = useAnnouncements(onSessionMaybeEnded);

  return (
    <>
      <AnnouncementBell
        t={t}
        items={listItems}
        loaded={loaded}
        unreadCount={unreadCount}
        onOpenItem={openItem}
      />
      <AnnouncementDialog
        t={t}
        announcement={current}
        remaining={remaining}
        onClose={closeCurrent}
      />
    </>
  );
}
