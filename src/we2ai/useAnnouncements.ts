import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isWe2aiApiError, we2aiApi, type We2aiAnnouncement } from "./api";

/** 窗口重新可见/聚焦时，距上次成功拉取超过该时长才重拉（与模型广场一致）。 */
const STALE_THRESHOLD_MS = 60_000;

/** 网络类错误码：不代表会话状态变化（与模型广场一致）。 */
const NETWORK_CODES = new Set(["TRANSIENT", "NETWORK_ERROR"]);

function createdAtMs(a: We2aiAnnouncement): number {
  const ms = Date.parse(a.createdAt);
  return Number.isNaN(ms) ? 0 : ms;
}

function byCreatedAtAsc(a: We2aiAnnouncement, b: We2aiAnnouncement): number {
  return createdAtMs(a) - createdAtMs(b) || a.id - b.id;
}

/**
 * 公告状态：拉取、弹窗队列、已读。
 *
 * - 触发拉取：挂载时、窗口重新可见/聚焦（>60 秒）、Rust 后台轮询发现变化的事件。
 *   不设前端定时器——WebView 隐藏后定时器会被节流，5 分钟轮询由 Rust 侧负责。
 * - 拉取失败一律静默（只 `console.debug`），等下一次触发；但遇到非网络类错误码
 *   （会话被终止、需要重新登录等）时调用 `onSessionMaybeEnded`，让外壳复查会话状态。
 * - 已读以服务端 `readAt` 为准；关闭时先本地乐观标记，随后调用服务端。标记失败时
 *   本会话内不再弹出该条，并在之后每次拉取仍显示未读时重试标记。
 * - 本 hook 应挂在按会话身份 `key` 的组件里：换账号/区域即整体重建，不沿用旧数据。
 */
export function useAnnouncements(onSessionMaybeEnded: () => void = () => {}) {
  const [items, setItems] = useState<We2aiAnnouncement[] | null>(null);
  const [viewingId, setViewingId] = useState<number | null>(null);
  const [dismissed, setDismissed] = useState<ReadonlySet<number>>(new Set());

  const alive = useRef(true);
  const requestSeq = useRef(0);
  const fetchedAt = useRef<number | null>(null);
  const dismissedRef = useRef(dismissed);
  dismissedRef.current = dismissed;
  // 本地已关闭并乐观标记为已读的时间：在途的旧拉取响应带着服务端 readAt=null 返回
  // 时不能把它们覆盖回未读。
  const localReadAt = useRef(new Map<number, string>());
  const onSessionMaybeEndedRef = useRef(onSessionMaybeEnded);
  onSessionMaybeEndedRef.current = onSessionMaybeEnded;

  const reportError = useCallback((error: unknown) => {
    if (isWe2aiApiError(error) && !NETWORK_CODES.has(error.code)) {
      onSessionMaybeEndedRef.current();
    }
  }, []);

  // 已在请求中的标记：连续刷新时同一条不并发重复提交。
  const markingIds = useRef(new Set<number>());

  const markRead = useCallback(
    (id: number) => {
      if (markingIds.current.has(id)) return;
      markingIds.current.add(id);
      void we2aiApi
        .markAnnouncementRead(id)
        .catch((error) => {
          console.debug("[we2ai] mark announcement read failed", error);
          reportError(error);
        })
        .finally(() => markingIds.current.delete(id));
    },
    [reportError],
  );

  const refresh = useCallback(async () => {
    const seq = ++requestSeq.current;
    try {
      const list = await we2aiApi.listAnnouncements();
      if (!alive.current || seq !== requestSeq.current) return;
      fetchedAt.current = Date.now();
      setItems(
        list.map((a) => {
          const local = localReadAt.current.get(a.id);
          return !a.readAt && local ? { ...a, readAt: local } : a;
        }),
      );
      // 正在查看的公告已下架（到期/定向变化）时清掉，避免它重新出现时自己弹出来。
      setViewingId((prev) =>
        prev !== null && !list.some((a) => a.id === prev) ? null : prev,
      );
      for (const a of list) {
        if (!a.readAt && dismissedRef.current.has(a.id)) {
          markRead(a.id);
        }
      }
    } catch (error) {
      console.debug("[we2ai] announcements fetch failed", error);
      if (alive.current && seq === requestSeq.current) reportError(error);
    }
  }, [markRead, reportError]);

  useEffect(() => {
    alive.current = true;
    void refresh();
    return () => {
      alive.current = false;
    };
  }, [refresh]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void we2aiApi
      .onAnnouncementsChanged(() => void refresh())
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((error) => {
        console.debug("[we2ai] announcements listener failed", error);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh]);

  useEffect(() => {
    const onVisibleOrFocus = () => {
      if (document.hidden) return;
      const at = fetchedAt.current;
      if (at !== null && Date.now() - at < STALE_THRESHOLD_MS) return;
      void refresh();
    };
    document.addEventListener("visibilitychange", onVisibleOrFocus);
    window.addEventListener("focus", onVisibleOrFocus);
    return () => {
      document.removeEventListener("visibilitychange", onVisibleOrFocus);
      window.removeEventListener("focus", onVisibleOrFocus);
    };
  }, [refresh]);

  const popupQueue = useMemo(
    () =>
      (items ?? [])
        .filter(
          (a) => a.notifyMode === "popup" && !a.readAt && !dismissed.has(a.id),
        )
        .sort(byCreatedAtAsc),
    [items, dismissed],
  );

  const viewing = useMemo(
    () =>
      viewingId === null
        ? null
        : ((items ?? []).find((a) => a.id === viewingId) ?? null),
    [items, viewingId],
  );

  const current = viewing ?? popupQueue[0] ?? null;
  const remaining = viewing ? 0 : Math.max(0, popupQueue.length - 1);

  const closeCurrent = useCallback(() => {
    if (!current) return;
    const target = current;
    setViewingId(null);
    setDismissed((prev) => new Set(prev).add(target.id));
    if (!target.readAt) {
      const now = new Date().toISOString();
      localReadAt.current.set(target.id, now);
      setItems((prev) =>
        prev
          ? prev.map((a) => (a.id === target.id ? { ...a, readAt: now } : a))
          : prev,
      );
      markRead(target.id);
    }
  }, [current, markRead]);

  const openItem = useCallback((id: number) => setViewingId(id), []);

  const listItems = useMemo(
    () => [...(items ?? [])].sort((a, b) => byCreatedAtAsc(b, a)),
    [items],
  );
  const unreadCount = useMemo(
    () => (items ?? []).filter((a) => !a.readAt).length,
    [items],
  );

  return {
    listItems,
    loaded: items !== null,
    unreadCount,
    current,
    remaining,
    openItem,
    closeCurrent,
  };
}
