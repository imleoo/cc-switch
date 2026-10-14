/**
 * 工坊错误 code → 本地化文案（功能 27）。文本对话与图片 / 视频面板共用：
 * 旧 code 的文案在 `strings.ts`，P2/P3 新增 code 的文案在 `studioMediaStrings.ts`。
 * 未识别的 code 回退到调用方给的文案，再回退到通用提示。
 */

import type { We2aiPlaygroundErrorCode } from "./api";
import type { StudioMediaStrings } from "./studioMediaStrings";
import type { We2aiStrings } from "./strings";

export function studioErrorMessage(
  t: We2aiStrings,
  ms: StudioMediaStrings,
  code: string | null,
  fallback: string,
): string {
  switch (code as We2aiPlaygroundErrorCode | null) {
    case "PLAYGROUND_KEY_NOT_FOUND":
      return t.studioErrorKeyNotFound;
    case "PLAYGROUND_KEY_INACTIVE":
      return t.studioErrorKeyInactive;
    case "PLAYGROUND_KEY_INVALID":
      return t.studioErrorKeyInvalid;
    case "PLAYGROUND_QUOTA_EXHAUSTED":
      return t.studioErrorQuota;
    case "PLAYGROUND_RATE_LIMITED":
      return t.studioErrorRateLimited;
    case "PLAYGROUND_IDENTITY_CHANGED":
    case "SESSION_CHANGED":
      return t.studioErrorIdentity;
    case "PLAYGROUND_CANCELLED":
      return t.studioErrorCancelled;
    case "PLAYGROUND_NETWORK":
      return t.studioErrorNetwork;
    case "PLAYGROUND_PAYLOAD_TOO_LARGE":
      return t.studioErrorPayload;
    case "PLAYGROUND_BUSY":
      return t.studioErrorBusy;
    case "PLAYGROUND_UPSTREAM_ERROR":
      return t.studioErrorUpstream;
    case "PLAYGROUND_REJECTED":
      return ms.errorRejected;
    case "PLAYGROUND_MEDIA_FAILED":
      return ms.errorMediaFailed;
    case "PLAYGROUND_MEDIA_TIMEOUT":
      return ms.errorMediaTimeout;
    case "PLAYGROUND_MEDIA_INVALID":
      return ms.errorMediaInvalid;
    case "PLAYGROUND_PATH_INVALID":
      return ms.errorPathInvalid;
    case "PLAYGROUND_SAVE_FAILED":
      return ms.errorSaveFailed;
    default:
      return fallback || t.studioErrorFallback;
  }
}

/** 会话级错误（应让外壳复查会话状态）。 */
export const SESSION_ERROR_CODES = new Set([
  "NO_ACTIVE_SESSION",
  "SESSION_CHANGED",
  "PLAYGROUND_IDENTITY_CHANGED",
]);
