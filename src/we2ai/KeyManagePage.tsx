import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
  type We2aiCreatedKey,
  type We2aiManagedKey,
  type We2aiManagedKeyStatus,
} from "./api";
import { KeyEditDialog } from "./KeyEditDialog";
import {
  describeExpiry,
  describeLastUsed,
  effectiveKeyStatus,
  formatQuotaText,
  formatRate,
  isSessionNeutralError,
  keyStatusLabel,
  quotaPercent,
} from "./keyManageUtils";
import {
  formatWe2aiString,
  getWe2aiErrorMessage,
  getWe2aiKeyErrorMessage,
  type We2aiStrings,
} from "./strings";

/** 列表拉取被更新的一次拉取取代（写操作后缓存失效）时最多自动重拉的次数。 */
const MAX_SUPERSEDED_RETRIES = 2;

type StatusFilter = "all" | We2aiManagedKeyStatus;
const STATUS_FILTERS: StatusFilter[] = [
  "all",
  "active",
  "inactive",
  "quota_exhausted",
  "expired",
];

type EditTarget = { mode: "create" } | { mode: "edit"; key: We2aiManagedKey };

function errorCode(error: unknown): string | null {
  return isWe2aiApiError(error) ? error.code : null;
}

const secondaryButtonClass =
  "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]";
const primaryButtonClass =
  "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]";
const dialogClass =
  "we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]";

interface KeyManagePageProps {
  t: We2aiStrings;
  /** 请求失败且不是网络类错误时回调，让外壳复查会话状态（同模型广场）。 */
  onSessionMaybeEnded: () => void;
  /**
   * 一次性请求：模型广场空状态的「去创建 Key」让外壳切到本 Tab 并置为 `true`，
   * 页面挂载（或该值变为 `true`）时直接打开新建弹窗，随后调用
   * `onCreateRequestHandled` 让外壳复位。本 Tab 切走即卸载，所以不能用计数信号
   * （挂载时的初始值无法与「已消费」区分），而用「请求中」布尔值 + 复位回调。
   */
  openCreateRequested?: boolean;
  onCreateRequestHandled?: () => void;
}

/**
 * Key 管理 Tab（功能 21）：列表 + 新建/编辑/启停/删除/复制。
 *
 * 明文边界：列表数据只有掩码；「复制」调 `copyKey`，由 Rust 从缓存取明文直接
 * 写系统剪贴板，明文不经 IPC；只有创建成功后的「Key 已创建」卡片把一次性明文放在
 * `created` state 里用于展示，关闭即清除（卡片里的复制同样走 `copyKey`）。
 */
export function KeyManagePage({
  t,
  onSessionMaybeEnded,
  openCreateRequested = false,
  onCreateRequestHandled,
}: KeyManagePageProps) {
  const [keys, setKeys] = useState<We2aiManagedKey[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [busyIds, setBusyIds] = useState<ReadonlySet<number>>(new Set());
  const [editTarget, setEditTarget] = useState<EditTarget | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<We2aiManagedKey | null>(
    null,
  );
  const [created, setCreated] = useState<We2aiCreatedKey | null>(null);

  // 相对时间（过期倒计时、最近使用、已过期判定）用的「现在」：每分钟刷新一次，
  // 列表重新拉取成功时也刷新，避免页面停留很久后显示过期的相对时间。
  const [now, setNow] = useState(() => Date.now());
  const alive = useRef(true);
  const inflight = useRef(false);
  const pending = useRef(false);
  const supersededRetries = useRef(0);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);

  const handleSessionError = useCallback(
    (error: unknown) => {
      const code = errorCode(error);
      if (code && !isSessionNeutralError(code)) onSessionMaybeEnded();
    },
    [onSessionMaybeEnded],
  );

  // 同一时刻只保留一个拉取：写操作成功后「事件」与「写操作自己的刷新」几乎同时
  // 触发，后到的请求合并为在途请求结束后的一次重拉，避免两个并发拉取在 Rust
  // 侧互相判为过期。
  const load = useCallback(async () => {
    if (inflight.current) {
      pending.current = true;
      return;
    }
    inflight.current = true;
    setLoading(true);
    setLoadError(null);
    try {
      const list = await we2aiApi.manageListKeys();
      if (!alive.current) return;
      supersededRetries.current = 0;
      setNow(Date.now());
      setKeys(list);
    } catch (error) {
      if (!alive.current) return;
      if (
        errorCode(error) === "KEY_LIST_SUPERSEDED" &&
        supersededRetries.current < MAX_SUPERSEDED_RETRIES
      ) {
        supersededRetries.current += 1;
        pending.current = true;
      } else {
        const code = errorCode(error);
        setLoadError(code ? getWe2aiErrorMessage(t, code) : t.errorNetwork);
        handleSessionError(error);
      }
    } finally {
      inflight.current = false;
      if (alive.current) {
        setLoading(false);
        if (pending.current) {
          pending.current = false;
          void loadRef.current();
        }
      }
    }
  }, [handleSessionError, t]);
  const loadRef = useRef(load);
  loadRef.current = load;

  useEffect(() => {
    void load();
    // 只在挂载时拉取一次；后续刷新由事件与写操作触发。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!openCreateRequested) return;
    setEditTarget({ mode: "create" });
    onCreateRequestHandled?.();
  }, [openCreateRequested, onCreateRequestHandled]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void we2aiApi
      .onKeysChanged(() => void loadRef.current())
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((error) => {
        console.debug("[we2ai] keys listener failed", error);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const setBusy = (id: number, busy: boolean) => {
    setBusyIds((prev) => {
      const next = new Set(prev);
      if (busy) next.add(id);
      else next.delete(id);
      return next;
    });
  };

  const handleCopy = async (key: We2aiManagedKey) => {
    try {
      await we2aiApi.copyKey(key.id);
      toast.success(t.keyMgrCopied);
    } catch (error) {
      if (errorCode(error) === "KEY_NOT_FOUND") {
        // Rust 缓存已失效（写操作后、会话变化后）：刷新一次让缓存重建。
        toast.error(t.keyMgrCopyStale);
        void load();
      } else if (isWe2aiApiError(error)) {
        toast.error(getWe2aiKeyErrorMessage(t, error.code));
        handleSessionError(error);
      } else {
        toast.error(t.keyMgrCopyFailed);
      }
    }
  };

  const handleToggle = async (key: We2aiManagedKey) => {
    const enable = key.status !== "active";
    setBusy(key.id, true);
    try {
      await we2aiApi.updateKey(key.id, {
        status: enable ? "active" : "inactive",
      });
      toast.success(
        formatWe2aiString(
          enable ? t.keyMgrEnabledToast : t.keyMgrDisabledToast,
          { name: key.name },
        ),
      );
      void load();
    } catch (error) {
      toast.error(
        isWe2aiApiError(error)
          ? getWe2aiKeyErrorMessage(t, error.code)
          : t.errorNetwork,
      );
      handleSessionError(error);
    } finally {
      if (alive.current) setBusy(key.id, false);
    }
  };

  const filtered = useMemo(() => {
    if (!keys) return [];
    const needle = query.trim().toLowerCase();
    return keys.filter((k) => {
      if (needle && !k.name.toLowerCase().includes(needle)) return false;
      return (
        statusFilter === "all" || effectiveKeyStatus(k, now) === statusFilter
      );
    });
  }, [keys, query, statusFilter, now]);

  const statusFilterLabel = (value: StatusFilter): string =>
    value === "all" ? t.keyMgrFilterAll : keyStatusLabel(t, value);

  const renderBody = () => {
    if (keys === null) {
      return (
        <div
          className="space-y-3 py-6 text-sm"
          data-testid="key-manage-initial"
        >
          {loadError ? (
            <div className="flex items-center gap-3">
              <span role="alert" className="text-[var(--we2ai-orange)]">
                {loadError}
              </span>
              <Button
                size="sm"
                variant="outline"
                disabled={loading}
                onClick={() => void load()}
                className={secondaryButtonClass}
              >
                {t.offlineRetry}
              </Button>
            </div>
          ) : (
            <span>{t.keyMgrLoading}</span>
          )}
        </div>
      );
    }
    if (keys.length === 0) {
      return (
        <div
          className="we2ai-panel space-y-2 p-6 text-sm"
          data-testid="key-manage-empty"
        >
          <p className="font-black">{t.keyMgrEmpty}</p>
          <p className="opacity-70">{t.keyMgrEmptyHint}</p>
        </div>
      );
    }
    if (filtered.length === 0) {
      return (
        <p
          className="py-6 text-sm opacity-70"
          data-testid="key-manage-no-match"
        >
          {t.keyMgrNoMatch}
        </p>
      );
    }
    return (
      <div className="we2ai-keytable-wrap">
        <table className="we2ai-keytable">
          <thead>
            <tr>
              <th>{t.keyMgrColName}</th>
              <th>{t.keyMgrColKey}</th>
              <th>{t.keyMgrColStatus}</th>
              <th>{t.keyMgrColQuota}</th>
              <th>{t.keyMgrColExpires}</th>
              <th>{t.keyMgrColLastUsed}</th>
              <th>{t.keyMgrColActions}</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((key) => {
              const status = effectiveKeyStatus(key, now);
              const expiry = describeExpiry(t, key.expiresAt, now);
              const percent = quotaPercent(key.quotaUsed, key.quota);
              const busy = busyIds.has(key.id);
              const canToggle = status === "active" || status === "inactive";
              return (
                <tr key={key.id} data-testid={`key-row-${key.id}`}>
                  <td>
                    <div className="we2ai-keytable-name">{key.name}</div>
                    <span
                      className="we2ai-chip"
                      data-testid={`key-group-${key.id}`}
                    >
                      {key.group
                        ? key.group.rate !== 1
                          ? `${key.group.name} ×${formatRate(key.group.rate)}`
                          : key.group.name
                        : t.keyMgrNoGroup}
                    </span>
                  </td>
                  <td>
                    <div className="flex items-center gap-2">
                      <span className="font-mono text-xs">{key.maskedKey}</span>
                      <button
                        type="button"
                        className="we2ai-model-action"
                        aria-label={`${t.keyMgrCopy} ${key.name}`}
                        onClick={() => void handleCopy(key)}
                      >
                        {t.keyMgrCopy}
                      </button>
                    </div>
                  </td>
                  <td>
                    <span
                      className="we2ai-status-chip"
                      data-status={status}
                      data-testid={`key-status-${key.id}`}
                    >
                      {keyStatusLabel(t, status)}
                    </span>
                  </td>
                  <td>
                    <div
                      className="we2ai-quota-text"
                      data-testid={`key-quota-${key.id}`}
                    >
                      {formatQuotaText(t, key.quotaUsed, key.quota)}
                    </div>
                    <div
                      className="we2ai-quota-bar"
                      role="progressbar"
                      aria-label={`${t.keyMgrColQuota} ${key.name}`}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={Math.round(percent)}
                      data-level={
                        percent >= 100 ? "full" : percent >= 80 ? "high" : "ok"
                      }
                    >
                      <span style={{ width: `${percent}%` }} />
                    </div>
                  </td>
                  <td>
                    <span
                      className="we2ai-keytable-expiry"
                      data-soon={expiry.soon ? "true" : "false"}
                      data-past={expiry.past ? "true" : "false"}
                      data-testid={`key-expiry-${key.id}`}
                    >
                      {expiry.text}
                    </span>
                  </td>
                  <td className="text-xs">
                    {describeLastUsed(t, key.lastUsedAt, now)}
                  </td>
                  <td>
                    <div className="flex flex-wrap items-center gap-2">
                      <button
                        type="button"
                        className="we2ai-model-action"
                        disabled={busy}
                        aria-label={`${t.keyMgrEdit} ${key.name}`}
                        onClick={() => setEditTarget({ mode: "edit", key })}
                      >
                        {t.keyMgrEdit}
                      </button>
                      {canToggle && (
                        <button
                          type="button"
                          className="we2ai-model-action"
                          disabled={busy}
                          aria-label={`${
                            status === "active"
                              ? t.keyMgrDisable
                              : t.keyMgrEnable
                          } ${key.name}`}
                          onClick={() => void handleToggle(key)}
                        >
                          {status === "active"
                            ? t.keyMgrDisable
                            : t.keyMgrEnable}
                        </button>
                      )}
                      <button
                        type="button"
                        className="we2ai-model-action"
                        disabled={busy}
                        aria-label={`${t.keyMgrDelete} ${key.name}`}
                        onClick={() => setDeleteTarget(key)}
                      >
                        {t.keyMgrDelete}
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    );
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <Input
          aria-label={t.keyMgrSearchPlaceholder}
          placeholder={t.keyMgrSearchPlaceholder}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="w-56 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none focus:ring-0 focus:border-[var(--we2ai-orange)]"
        />
        <Select
          value={statusFilter}
          onValueChange={(value) => setStatusFilter(value as StatusFilter)}
        >
          <SelectTrigger
            aria-label={t.keyMgrFilterLabel}
            className="w-40 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
            {STATUS_FILTERS.map((value) => (
              <SelectItem key={value} value={value}>
                {statusFilterLabel(value)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          size="sm"
          variant="ghost"
          disabled={loading}
          onClick={() => void load()}
          className="rounded-lg border-transparent shadow-none hover:bg-[var(--we2ai-paper-2)]"
        >
          {t.refresh}
        </Button>
        <button
          type="button"
          className="we2ai-billing-primary we2ai-billing-primary--small ml-auto"
          onClick={() => setEditTarget({ mode: "create" })}
        >
          {t.keyMgrCreate}
        </button>
      </div>

      {keys !== null && loadError && (
        <div className="flex items-center gap-3 text-sm">
          <span role="alert" className="text-[var(--we2ai-orange)]">
            {loadError}
          </span>
          <Button
            size="sm"
            variant="outline"
            disabled={loading}
            onClick={() => void load()}
            className={secondaryButtonClass}
          >
            {t.offlineRetry}
          </Button>
        </div>
      )}

      {renderBody()}

      {editTarget && (
        <KeyEditDialog
          t={t}
          keyItem={editTarget.mode === "edit" ? editTarget.key : null}
          onClose={() => setEditTarget(null)}
          onCreated={(result) => {
            setEditTarget(null);
            setCreated(result);
            toast.success(
              formatWe2aiString(t.keyMgrCreatedToast, {
                name: result.key.name,
              }),
            );
            void load();
          }}
          onUpdated={() => {
            setEditTarget(null);
            toast.success(t.keyMgrSavedToast);
            void load();
          }}
          onResultUnknown={() => {
            // 重置额度的结果未知：关闭弹窗（不留重试入口），提示并重拉，由用户在
            // 列表里确认「已用额度」后再决定是否重来。
            setEditTarget(null);
            toast.error(t.keyMgrErrResultUnknown);
            void load();
          }}
          onSessionMaybeEnded={onSessionMaybeEnded}
        />
      )}

      {created && (
        <KeyCreatedDialog
          t={t}
          created={created}
          // 与列表里的复制同一条路径（含 KEY_NOT_FOUND 时重拉并提示）。
          onCopy={() => handleCopy(created.key)}
          // 关闭即清除前端持有的明文。
          onClose={() => setCreated(null)}
        />
      )}

      {deleteTarget && (
        <DeleteKeyDialog
          t={t}
          keyItem={deleteTarget}
          onClose={() => setDeleteTarget(null)}
          onDeleted={() => {
            const name = deleteTarget.name;
            setDeleteTarget(null);
            toast.success(formatWe2aiString(t.keyMgrDeletedToast, { name }));
            void load();
          }}
          onSessionMaybeEnded={onSessionMaybeEnded}
        />
      )}
    </div>
  );
}

/** 「Key 已创建」卡片：完整明文 + 复制；关闭后父组件清掉明文 state。 */
function KeyCreatedDialog({
  t,
  created,
  onCopy,
  onClose,
}: {
  t: We2aiStrings;
  created: We2aiCreatedKey;
  /** 复制走 Rust（创建时新 Key 的明文已并入管理页缓存），前端不回传明文。 */
  onCopy: () => Promise<void>;
  onClose: () => void;
}) {
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className={dialogClass} data-testid="key-created-dialog">
        <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
          <DialogTitle className="we2ai-heading">
            {t.keyMgrCreatedTitle}
          </DialogTitle>
          <DialogDescription>{t.keyMgrCreatedWarning}</DialogDescription>
        </DialogHeader>
        <div className="space-y-3 px-6 py-5">
          <div className="space-y-1">
            <span className="we2ai-label">{t.keyMgrCreatedNameLabel}</span>
            <div className="font-black">{created.key.name}</div>
          </div>
          <code
            data-testid="key-created-plaintext"
            className="we2ai-keycode select-all"
          >
            {created.plaintext}
          </code>
        </div>
        <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
          <Button
            type="button"
            variant="outline"
            onClick={() => void onCopy()}
            className={secondaryButtonClass}
          >
            {t.keyMgrCopy}
          </Button>
          <Button
            type="button"
            onClick={onClose}
            className={primaryButtonClass}
          >
            {t.keyMgrCreatedClose}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * 删除确认：应用内弹窗（不用 `window.confirm`），要求输入 Key 名称；该 Key 是
 * 当前选中的工具 Key 时额外显示橙色警告。是否在用取现有 `listKeys()` 的
 * `rememberedKeyId`（`key_selection.json` 里用户显式选择过的 Key，无记忆为
 * `null`）；不用 `selectedKeyId`，它在无记忆时回退到第一个 Key，会误报。
 */
function DeleteKeyDialog({
  t,
  keyItem,
  onClose,
  onDeleted,
  onSessionMaybeEnded,
}: {
  t: We2aiStrings;
  keyItem: We2aiManagedKey;
  onClose: () => void;
  onDeleted: () => void;
  onSessionMaybeEnded: () => void;
}) {
  const [typed, setTyped] = useState("");
  // `null` = 还在判断是否在用；判断失败按「不在用」处理（不阻塞删除）。
  const [inUse, setInUse] = useState<boolean | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let attempts = 0;
    const run = async () => {
      try {
        const result = await we2aiApi.listKeys();
        if (!cancelled) setInUse(result.rememberedKeyId === keyItem.id);
      } catch (e) {
        if (
          errorCode(e) === "KEY_LIST_SUPERSEDED" &&
          attempts < MAX_SUPERSEDED_RETRIES
        ) {
          attempts += 1;
          void run();
          return;
        }
        if (!cancelled) setInUse(false);
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [keyItem.id]);

  const matches = typed.trim() === keyItem.name.trim();

  const handleDelete = async () => {
    if (!matches || deleting || inUse === null) return;
    setDeleting(true);
    setError(null);
    try {
      await we2aiApi.deleteKey(keyItem.id);
      onDeleted();
    } catch (e) {
      if (!alive.current) return;
      if (isWe2aiApiError(e)) {
        setError(getWe2aiKeyErrorMessage(t, e.code));
        if (!isSessionNeutralError(e.code)) onSessionMaybeEnded();
      } else {
        setError(t.errorNetwork);
      }
    } finally {
      if (alive.current) setDeleting(false);
    }
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !deleting) onClose();
      }}
    >
      <DialogContent className={dialogClass} data-testid="key-delete-dialog">
        <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
          <DialogTitle className="we2ai-heading">
            {t.keyMgrDeleteTitle}
          </DialogTitle>
          <DialogDescription>
            {formatWe2aiString(t.keyMgrDeleteDescription, {
              name: keyItem.name,
            })}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3 px-6 py-5">
          {inUse && (
            <div
              role="alert"
              data-testid="key-delete-in-use"
              className="border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] px-3 py-2 text-xs font-medium text-[var(--we2ai-paper)]"
            >
              {t.keyMgrDeleteInUse}
            </div>
          )}
          <div className="space-y-1.5">
            <Label htmlFor="we2ai-key-delete-name" className="we2ai-label">
              {t.keyMgrDeleteInputLabel}
            </Label>
            <Input
              id="we2ai-key-delete-name"
              autoFocus
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              className="rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none focus:ring-0 focus:border-[var(--we2ai-orange)]"
            />
          </div>
          {error && (
            <p
              role="alert"
              data-testid="key-delete-error"
              className="text-sm text-[var(--we2ai-orange)]"
            >
              {error}
            </p>
          )}
        </div>
        <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
          <Button
            type="button"
            variant="outline"
            disabled={deleting}
            onClick={onClose}
            className={secondaryButtonClass}
          >
            {t.keyMgrCancel}
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={!matches || deleting || inUse === null}
            onClick={() => void handleDelete()}
            className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-ink)]"
          >
            {t.keyMgrDeleteConfirm}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export default KeyManagePage;
