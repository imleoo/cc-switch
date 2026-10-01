import type { We2aiManagedKey, We2aiManagedKeyStatus } from "./api";
import { formatWe2aiString, type We2aiStrings } from "./strings";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/** 过期倒计时在该时长内显示为橙色。 */
export const EXPIRY_WARNING_MS = 7 * DAY_MS;

function parseTime(iso: string | null): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return Number.isNaN(ms) ? null : ms;
}

/**
 * 展示用状态：服务端对「已过期」是惰性标记（Key 被调用时才改状态），所以
 * `active` 但过期时间已过的 Key 在界面上按已过期显示。
 */
export function effectiveKeyStatus(
  key: Pick<We2aiManagedKey, "status" | "expiresAt">,
  now: number,
): We2aiManagedKeyStatus {
  const status: We2aiManagedKeyStatus =
    key.status === "active" ||
    key.status === "quota_exhausted" ||
    key.status === "expired"
      ? key.status
      : "inactive";
  if (status === "active") {
    const expires = parseTime(key.expiresAt);
    if (expires !== null && expires <= now) return "expired";
  }
  return status;
}

export function keyStatusLabel(
  t: We2aiStrings,
  status: We2aiManagedKeyStatus,
): string {
  switch (status) {
    case "active":
      return t.keyMgrStatusActive;
    case "quota_exhausted":
      return t.keyMgrStatusQuotaExhausted;
    case "expired":
      return t.keyMgrStatusExpired;
    default:
      return t.keyMgrStatusInactive;
  }
}

/** 倍率去尾零：`0.8`、`1.5`、`2`。 */
export function formatRate(rate: number): string {
  return String(Number(rate.toFixed(4)));
}

/** `$3.20`（已用额度，两位小数）。 */
export function formatUsedUsd(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return `$${Math.max(0, value).toFixed(2)}`;
}

/** 额度上限：整数不带小数（`$10`），否则两位（`$10.50`）。 */
export function formatLimitUsd(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return Number.isInteger(value) ? `$${value}` : `$${value.toFixed(2)}`;
}

/** `$3.20 / $10` 或 `$3.20 / 不限`。 */
export function formatQuotaText(
  t: We2aiStrings,
  used: number,
  quota: number,
): string {
  const limit = quota > 0 ? formatLimitUsd(quota) : t.keyMgrQuotaUnlimited;
  return `${formatUsedUsd(used)} / ${limit}`;
}

/** 额度使用百分比（0–100）；不限额度为 0。 */
export function quotaPercent(used: number, quota: number): number {
  if (!(quota > 0) || !Number.isFinite(used)) return 0;
  return Math.min(100, Math.max(0, (used / quota) * 100));
}

export interface ExpiryDescription {
  text: string;
  /** 7 天内将过期（橙色）。 */
  soon: boolean;
  /** 已过期。 */
  past: boolean;
}

export function describeExpiry(
  t: We2aiStrings,
  expiresAt: string | null,
  now: number,
): ExpiryDescription {
  const expires = parseTime(expiresAt);
  if (expires === null) {
    return { text: t.keyMgrNeverExpires, soon: false, past: false };
  }
  const remaining = expires - now;
  if (remaining <= 0) {
    return { text: t.keyMgrExpired, soon: false, past: true };
  }
  const soon = remaining < EXPIRY_WARNING_MS;
  if (remaining < HOUR_MS) {
    return { text: t.keyMgrExpiresWithinHour, soon, past: false };
  }
  if (remaining < DAY_MS) {
    return {
      text: formatWe2aiString(t.keyMgrExpiresInHours, {
        n: Math.floor(remaining / HOUR_MS),
      }),
      soon,
      past: false,
    };
  }
  return {
    text: formatWe2aiString(t.keyMgrExpiresInDays, {
      n: Math.floor(remaining / DAY_MS),
    }),
    soon,
    past: false,
  };
}

export function describeLastUsed(
  t: We2aiStrings,
  lastUsedAt: string | null,
  now: number,
): string {
  const used = parseTime(lastUsedAt);
  if (used === null) return t.keyMgrNeverUsed;
  const elapsed = Math.max(0, now - used);
  if (elapsed < MINUTE_MS) return t.keyMgrJustNow;
  if (elapsed < HOUR_MS) {
    return formatWe2aiString(t.keyMgrMinutesAgo, {
      n: Math.floor(elapsed / MINUTE_MS),
    });
  }
  if (elapsed < DAY_MS) {
    return formatWe2aiString(t.keyMgrHoursAgo, {
      n: Math.floor(elapsed / HOUR_MS),
    });
  }
  return formatWe2aiString(t.keyMgrDaysAgo, {
    n: Math.floor(elapsed / DAY_MS),
  });
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** `<input type="date">` 的值：本地日期 `YYYY-MM-DD`。无法解析返回空串。 */
export function toDateInputValue(iso: string | null): string {
  const ms = parseTime(iso);
  if (ms === null) return "";
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** 本地某天的 23:59:59 对应的时间戳；日期串无效返回 `null`。 */
export function endOfLocalDayMs(dateValue: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(dateValue);
  if (!match) return null;
  const [, y, m, d] = match;
  const date = new Date(Number(y), Number(m) - 1, Number(d), 23, 59, 59, 0);
  const ms = date.getTime();
  return Number.isNaN(ms) ? null : ms;
}

/** 距离某个时间戳还有几天（向上取整，至少 1）；用于创建时把日期换算成 `expires_in_days`。 */
export function daysUntil(targetMs: number, now: number): number {
  return Math.max(1, Math.ceil((targetMs - now) / DAY_MS));
}

/** 本地今天的 `YYYY-MM-DD`，用作日期选择器的 `min`。 */
export function todayDateValue(now: number): string {
  const d = new Date(now);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** SubPanel `api_keys.name` 为 `MaxLen(100)`，按 UTF-8 字节、在 `html.EscapeString` 之后校验。 */
export const KEY_NAME_MAX_ESCAPED_BYTES = 100;

/**
 * Go `html.EscapeString` 之后的 UTF-8 字节数（与 Rust `escaped_name_bytes` 一致）：
 * `&`→`&amp;`(5)、`<`→`&lt;`(4)、`>`→`&gt;`(4)、`"`→`&#34;`(5)、`'`→`&#39;`(5)，
 * 其余字符按自身 UTF-8 长度（按码点计，孤立代理项按 3 字节，同 U+FFFD）。
 */
export function escapedUtf8Length(name: string): number {
  let total = 0;
  for (const ch of name) {
    switch (ch) {
      case "&":
      case '"':
      case "'":
        total += 5;
        break;
      case "<":
      case ">":
        total += 4;
        break;
      default: {
        const cp = ch.codePointAt(0) ?? 0;
        total += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
      }
    }
  }
  return total;
}

/** 名称已 trim：非空且转义后不超过 100 字节。 */
export function isValidKeyName(trimmedName: string): boolean {
  return (
    trimmedName.length > 0 &&
    escapedUtf8Length(trimmedName) <= KEY_NAME_MAX_ESCAPED_BYTES
  );
}

/**
 * 与会话状态无关的错误码：网络类、幂等键「处理中 / 退避中」（服务端在处理上
 * 一次同键请求，稍后重试即可），以及本地剪贴板 / 示例文本校验失败。遇到这些不需要
 * 让外壳复查会话。
 */
const SESSION_NEUTRAL_CODES = new Set([
  "TRANSIENT",
  "NETWORK_ERROR",
  "IDEMPOTENCY_IN_PROGRESS",
  "IDEMPOTENCY_RETRY_BACKOFF",
  "CLIPBOARD_FAILED",
  "SAMPLE_TEXT_INVALID",
]);

export function isSessionNeutralError(code: string): boolean {
  return SESSION_NEUTRAL_CODES.has(code);
}

export { DAY_MS };
