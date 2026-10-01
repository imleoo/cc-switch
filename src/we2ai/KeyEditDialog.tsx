import { useEffect, useMemo, useRef, useState } from "react";
import "./we2ai-theme.css";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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
import { Switch } from "@/components/ui/switch";
import {
  isWe2aiApiError,
  newIdempotencyKey,
  we2aiApi,
  type We2aiCreatedKey,
  type We2aiCreateKeyInput,
  type We2aiKeyGroupOption,
  type We2aiManagedKey,
  type We2aiUpdateKeyInput,
} from "./api";
import {
  DAY_MS,
  daysUntil,
  endOfLocalDayMs,
  formatRate,
  formatUsedUsd,
  isSessionNeutralError,
  isValidKeyName,
  toDateInputValue,
  todayDateValue,
} from "./keyManageUtils";
import {
  formatWe2aiString,
  getWe2aiErrorMessage,
  getWe2aiKeyErrorMessage,
  type We2aiStrings,
} from "./strings";

const GROUP_NONE = "none";

type ExpiryChoice = "forever" | "7" | "30" | "90" | "custom";

interface KeyEditDialogProps {
  t: We2aiStrings;
  /** `null` = 新建；否则编辑该 Key。弹窗挂载即打开，由父组件控制是否渲染。 */
  keyItem: We2aiManagedKey | null;
  onClose: () => void;
  /** 创建成功：交出一次性明文，父组件负责展示「Key 已创建」卡片。 */
  onCreated: (created: We2aiCreatedKey) => void;
  /** 编辑成功（含「没有任何改动」直接保存的情形不会调用）。 */
  onUpdated: () => void;
  /**
   * 含「重置已用额度」的更新遇到网络/瞬时错误：请求可能已生效（Rust 侧不自动重放，
   * 重放会二次清零），结果未知。父组件应关闭弹窗、提示并重拉列表，让用户先确认
   * 再操作；弹窗不能留着让用户直接重试。
   */
  onResultUnknown: () => void;
  onSessionMaybeEnded: () => void;
}

interface FieldErrors {
  name?: string;
  quota?: string;
  expiry?: string;
}

/** 空串 = 不限（0）；非有限或负数返回 `null`。 */
function parseQuota(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return 0;
  const value = Number(trimmed);
  return Number.isFinite(value) && value >= 0 ? value : null;
}

const inputClass =
  "rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none focus:ring-0 focus:border-[var(--we2ai-orange)]";
const secondaryButtonClass =
  "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]";
const primaryButtonClass =
  "rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]";

/**
 * 新建 / 编辑 Key 共用弹窗（一期字段：名称、分组、额度上限、有效期；编辑另有
 * 状态开关与「重置已用额度」）。
 *
 * 新建请求带 `Idempotency-Key`：每次打开弹窗（组件挂载）生成一个 UUID，同一
 * 提交内容的重试复用它；表单内容变了就换新的，否则服务端会按「同键不同载荷」
 * 拒绝。编辑只提交相对初始值有变化的字段。
 */
export function KeyEditDialog({
  t,
  keyItem,
  onClose,
  onCreated,
  onUpdated,
  onResultUnknown,
  onSessionMaybeEnded,
}: KeyEditDialogProps) {
  const editing = keyItem !== null;
  const initialExpiresAt = keyItem?.expiresAt ?? null;
  const initialDate = toDateInputValue(initialExpiresAt);
  const initialStatusActive = keyItem?.status === "active";

  const [name, setName] = useState(keyItem?.name ?? "");
  const [groupId, setGroupId] = useState<number | null>(
    keyItem?.group?.id ?? null,
  );
  const [quotaText, setQuotaText] = useState(
    keyItem && keyItem.quota > 0 ? String(keyItem.quota) : "",
  );
  const [expiry, setExpiry] = useState<ExpiryChoice>(
    initialExpiresAt && initialDate ? "custom" : "forever",
  );
  const [customDate, setCustomDate] = useState(initialDate);
  // 用户是否主动点过有效期选项。原过期时间解析失败时初始会落在「永久」，
  // 只有用户主动选「永久」才提交清除，避免改名等操作静默清掉过期时间。
  const expiryTouched = useRef(false);
  const [statusActive, setStatusActive] = useState(initialStatusActive);
  const [resetQuota, setResetQuota] = useState(false);

  const [groups, setGroups] = useState<We2aiKeyGroupOption[] | null>(null);
  const [groupsError, setGroupsError] = useState(false);
  const [groupsReload, setGroupsReload] = useState(0);
  const groupTouched = useRef(false);

  const [errors, setErrors] = useState<FieldErrors>({});
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const alive = useRef(true);

  // 幂等键：本次弹窗的初始键 + 最近一次提交的载荷（用来判断重试还是新请求）。
  const idempotencyKey = useRef(newIdempotencyKey());
  const lastAttemptPayload = useRef<string | null>(null);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    setGroupsError(false);
    void we2aiApi
      .listKeyGroups()
      .then((list) => {
        if (cancelled) return;
        setGroups(list);
        // 新建时默认选第一个分组（用户没动过分组下拉的前提下）。
        if (!editing && !groupTouched.current && list.length > 0) {
          setGroupId((current) => current ?? list[0].id);
        }
      })
      .catch((error) => {
        if (cancelled) return;
        console.debug("[we2ai] list key groups failed", error);
        setGroupsError(true);
        if (isWe2aiApiError(error) && !isSessionNeutralError(error.code)) {
          onSessionMaybeEnded();
        }
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [groupsReload, editing]);

  /** 下拉选项：可绑定分组 + 编辑时当前分组（可能已不在可绑定列表里）。 */
  const groupOptions = useMemo(() => {
    const list = (groups ?? []).map((g) => ({
      id: g.id,
      name: g.name,
      rate: g.rate,
    }));
    const current = keyItem?.group;
    if (current && !list.some((g) => g.id === current.id)) {
      list.unshift({ id: current.id, name: current.name, rate: current.rate });
    }
    return list;
  }, [groups, keyItem]);

  const allowNoGroup = !editing || keyItem?.group == null;

  const groupLabel = (g: { name: string; rate: number }) =>
    g.rate !== 1 ? `${g.name} · ×${formatRate(g.rate)}` : g.name;

  const loadingGroups = groups === null && !groupsError;

  const validate = (): {
    errors: FieldErrors;
    name: string;
    quota: number;
    /** 选了有效期时的到期时间戳（编辑用）；永久为 `null`。 */
    expiresAtMs: number | null;
    /** 新建用的有效期天数；永久为 `null`。 */
    expiresInDays: number | null;
  } => {
    const next: FieldErrors = {};
    const trimmed = name.trim();
    // 与 Rust `validate_name` 一致：非空，且 html 转义后的 UTF-8 字节数 ≤ 100。
    if (!isValidKeyName(trimmed)) next.name = t.keyMgrNameInvalid;

    const quota = parseQuota(quotaText);
    if (quota === null) next.quota = t.keyMgrQuotaInvalid;

    let expiresAtMs: number | null = null;
    let expiresInDays: number | null = null;
    const now = Date.now();
    if (expiry === "custom") {
      const end = customDate ? endOfLocalDayMs(customDate) : null;
      if (editing && initialExpiresAt && customDate === initialDate) {
        // 编辑时日期没动：保持原有过期时间，不做「已过去」校验（已过期的 Key
        // 也要能只改名称等其他字段保存）；提交时也不会带 `expires_at`。
        expiresAtMs = null;
      } else if (end === null) next.expiry = t.keyMgrExpiryDateRequired;
      else if (end <= now) next.expiry = t.keyMgrExpiryPast;
      else {
        expiresAtMs = end;
        expiresInDays = daysUntil(end, now);
      }
    } else if (expiry !== "forever") {
      // 预设天数直接取预设值：不能再由时间戳反推，否则毫秒级误差会向上取整多出一天。
      expiresInDays = Number(expiry);
      expiresAtMs = now + expiresInDays * DAY_MS;
    }
    return {
      errors: next,
      name: trimmed,
      quota: quota ?? 0,
      expiresAtMs,
      expiresInDays,
    };
  };

  const reportError = (error: unknown) => {
    if (!alive.current) return;
    const indeterminate =
      !isWe2aiApiError(error) ||
      error.code === "TRANSIENT" ||
      error.code === "NETWORK_ERROR";
    if (editing && resetQuota && indeterminate) {
      onResultUnknown();
      return;
    }
    if (isWe2aiApiError(error)) {
      setSubmitError(getWe2aiKeyErrorMessage(t, error.code));
      if (!isSessionNeutralError(error.code)) onSessionMaybeEnded();
    } else {
      setSubmitError(getWe2aiErrorMessage(t, "NETWORK_ERROR"));
    }
  };

  const submitCreate = async (v: ReturnType<typeof validate>) => {
    const input: Omit<We2aiCreateKeyInput, "idempotencyKey"> = {
      name: v.name,
      ...(groupId !== null ? { groupId } : {}),
      // 0 = 不限：不传，保持与「不设额度」一致。
      ...(v.quota > 0 ? { quota: v.quota } : {}),
      ...(v.expiresInDays !== null ? { expiresInDays: v.expiresInDays } : {}),
    };
    const payload = JSON.stringify(input);
    if (
      lastAttemptPayload.current !== null &&
      lastAttemptPayload.current !== payload
    ) {
      idempotencyKey.current = newIdempotencyKey();
    }
    lastAttemptPayload.current = payload;
    const created = await we2aiApi.createKey({
      idempotencyKey: idempotencyKey.current,
      ...input,
    });
    onCreated(created);
  };

  const submitUpdate = async (v: ReturnType<typeof validate>) => {
    if (!keyItem) return;
    const input: We2aiUpdateKeyInput = {};
    if (v.name !== keyItem.name) input.name = v.name;
    if (groupId !== null && groupId !== (keyItem.group?.id ?? null)) {
      input.groupId = groupId;
    }
    if (v.quota !== keyItem.quota) input.quota = v.quota;

    if (expiry === "forever") {
      if (initialExpiresAt && expiryTouched.current) input.expiresAt = "";
    } else if (expiry === "custom") {
      // 自定义日期没改就不提交（保留原有精确到秒的过期时间）。
      if (customDate !== initialDate && v.expiresAtMs !== null) {
        input.expiresAt = new Date(v.expiresAtMs).toISOString();
      }
    } else if (v.expiresAtMs !== null) {
      input.expiresAt = new Date(v.expiresAtMs).toISOString();
    }

    if (statusActive !== initialStatusActive) {
      input.status = statusActive ? "active" : "inactive";
    }
    if (resetQuota) input.resetQuota = true;

    if (Object.keys(input).length === 0) {
      onClose();
      return;
    }
    await we2aiApi.updateKey(keyItem.id, input);
    onUpdated();
  };

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    const v = validate();
    setErrors(v.errors);
    setSubmitError(null);
    if (Object.keys(v.errors).length > 0) return;
    setSubmitting(true);
    try {
      if (editing) await submitUpdate(v);
      else await submitCreate(v);
    } catch (error) {
      reportError(error);
    } finally {
      if (alive.current) setSubmitting(false);
    }
  };

  const expiryOptions: { value: ExpiryChoice; label: string }[] = [
    { value: "forever", label: t.keyMgrExpiryForever },
    { value: "7", label: t.keyMgrExpiry7 },
    { value: "30", label: t.keyMgrExpiry30 },
    { value: "90", label: t.keyMgrExpiry90 },
    { value: "custom", label: t.keyMgrExpiryCustom },
  ];

  const limitedStatus =
    editing &&
    (keyItem.status === "quota_exhausted" || keyItem.status === "expired");

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !submitting) onClose();
      }}
    >
      <DialogContent className="we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]">
        <form
          // 校验全部自己做：日期框带 min，原生校验会拦住提交并弹系统气泡。
          noValidate
          onSubmit={(e) => void handleSubmit(e)}
          className="flex min-h-0 flex-col"
          data-testid="key-edit-form"
        >
          <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <DialogTitle className="we2ai-heading">
              {editing ? t.keyMgrEditTitle : t.keyMgrCreateTitle}
            </DialogTitle>
            <DialogDescription className="sr-only">
              {editing ? t.keyMgrEditTitle : t.keyMgrCreateTitle}
            </DialogDescription>
          </DialogHeader>

          <div className="we2ai-scroll min-h-0 flex-1 space-y-4 overflow-y-auto px-6 py-5">
            <div className="space-y-1.5">
              <Label htmlFor="we2ai-key-name" className="we2ai-label">
                {t.keyMgrFieldName}
              </Label>
              <Input
                id="we2ai-key-name"
                autoFocus
                value={name}
                placeholder={t.keyMgrNamePlaceholder}
                onChange={(e) => setName(e.target.value)}
                className={inputClass}
              />
              {errors.name && (
                <p role="alert" className="text-xs text-[var(--we2ai-orange)]">
                  {errors.name}
                </p>
              )}
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="we2ai-key-group" className="we2ai-label">
                {t.keyMgrFieldGroup}
              </Label>
              <Select
                value={groupId === null ? GROUP_NONE : String(groupId)}
                disabled={loadingGroups}
                onValueChange={(value) => {
                  // Radix 的隐藏原生 <select> 会在选项集合变化（分组刚加载完）时
                  // 派发空值的 change：忽略空值和非数字，否则会把分组写成 0。
                  const next = value === GROUP_NONE ? null : Number(value);
                  if (
                    value === "" ||
                    (next !== null && !Number.isFinite(next))
                  ) {
                    return;
                  }
                  groupTouched.current = true;
                  setGroupId(next);
                }}
              >
                <SelectTrigger
                  id="we2ai-key-group"
                  aria-label={t.keyMgrFieldGroup}
                  className="rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none"
                >
                  <SelectValue
                    placeholder={
                      loadingGroups ? t.keyMgrGroupsLoading : t.keyMgrGroupNone
                    }
                  />
                </SelectTrigger>
                <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
                  {allowNoGroup && (
                    <SelectItem value={GROUP_NONE}>
                      {t.keyMgrGroupNone}
                    </SelectItem>
                  )}
                  {groupOptions.map((g) => (
                    <SelectItem key={g.id} value={String(g.id)}>
                      {groupLabel(g)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {loadingGroups && (
                <p className="text-xs opacity-70">{t.keyMgrGroupsLoading}</p>
              )}
              {groupsError && (
                <p
                  role="alert"
                  className="flex items-center gap-2 text-xs text-[var(--we2ai-orange)]"
                >
                  <span>{t.keyMgrGroupsFailed}</span>
                  <button
                    type="button"
                    className="we2ai-model-action"
                    onClick={() => setGroupsReload((n) => n + 1)}
                  >
                    {t.offlineRetry}
                  </button>
                </p>
              )}
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="we2ai-key-quota" className="we2ai-label">
                {t.keyMgrFieldQuota}
              </Label>
              <Input
                id="we2ai-key-quota"
                inputMode="decimal"
                value={quotaText}
                placeholder={t.keyMgrQuotaPlaceholder}
                onChange={(e) => setQuotaText(e.target.value)}
                className={inputClass}
              />
              {errors.quota && (
                <p role="alert" className="text-xs text-[var(--we2ai-orange)]">
                  {errors.quota}
                </p>
              )}
              {editing && (
                <label className="flex items-center gap-2 pt-1 text-sm">
                  <Checkbox
                    checked={resetQuota}
                    onCheckedChange={(checked) =>
                      setResetQuota(checked === true)
                    }
                    className="rounded-sm border-2 border-[var(--we2ai-ink)]"
                  />
                  {formatWe2aiString(t.keyMgrResetQuota, {
                    used: formatUsedUsd(keyItem.quotaUsed),
                  })}
                </label>
              )}
            </div>

            <div className="space-y-1.5">
              <span className="we2ai-label">{t.keyMgrFieldExpiry}</span>
              <div
                role="group"
                aria-label={t.keyMgrFieldExpiry}
                className="flex flex-wrap gap-2"
              >
                {expiryOptions.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    aria-pressed={expiry === option.value}
                    onClick={() => {
                      expiryTouched.current = true;
                      setExpiry(option.value);
                    }}
                    className={`we2ai-model-action ${
                      expiry === option.value
                        ? "we2ai-model-action--selected"
                        : ""
                    }`}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
              {expiry === "custom" && (
                <Input
                  type="date"
                  aria-label={t.keyMgrExpiryCustom}
                  min={todayDateValue(Date.now())}
                  value={customDate}
                  onChange={(e) => setCustomDate(e.target.value)}
                  className={`${inputClass} w-48`}
                />
              )}
              {!editing && expiry !== "forever" && (
                <p className="text-xs opacity-70">{t.keyMgrExpiryDayNote}</p>
              )}
              {errors.expiry && (
                <p role="alert" className="text-xs text-[var(--we2ai-orange)]">
                  {errors.expiry}
                </p>
              )}
            </div>

            {editing && (
              <div className="space-y-1.5">
                <div className="flex items-center justify-between gap-4">
                  <span className="we2ai-label">{t.keyMgrFieldStatus}</span>
                  <Switch
                    checked={statusActive}
                    aria-label={t.keyMgrFieldStatus}
                    onCheckedChange={setStatusActive}
                    className="border-2 border-[var(--we2ai-ink)] data-[state=checked]:bg-[var(--we2ai-orange)] data-[state=unchecked]:bg-[var(--we2ai-paper-2)]"
                  />
                </div>
                <p className="text-xs opacity-70">
                  {limitedStatus
                    ? t.keyMgrStatusHintLimited
                    : t.keyMgrStatusHintInactive}
                </p>
              </div>
            )}

            {submitError && (
              <p
                role="alert"
                data-testid="key-edit-error"
                className="text-sm text-[var(--we2ai-orange)]"
              >
                {submitError}
              </p>
            )}
          </div>

          <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <Button
              type="button"
              variant="outline"
              disabled={submitting}
              onClick={onClose}
              className={secondaryButtonClass}
            >
              {t.keyMgrCancel}
            </Button>
            <Button
              type="submit"
              disabled={submitting || loadingGroups}
              className={primaryButtonClass}
            >
              {submitting
                ? t.keyMgrSubmitting
                : editing
                  ? t.keyMgrSave
                  : t.keyMgrCreateSubmit}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export default KeyEditDialog;
