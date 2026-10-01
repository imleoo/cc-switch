import { describe, expect, it } from "vitest";
import {
  escapedUtf8Length,
  isSessionNeutralError,
  isValidKeyName,
} from "@/we2ai/keyManageUtils";

// 与 Rust `key_manage.rs::escaped_name_bytes` / `validate_name` 的用例一一对应。
describe("escapedUtf8Length", () => {
  it("matches Go html.EscapeString followed by UTF-8 encoding", () => {
    expect(escapedUtf8Length("a&b")).toBe("a&amp;b".length);
    expect(escapedUtf8Length("<>\"'")).toBe("&lt;&gt;&#34;&#39;".length);
    expect(escapedUtf8Length("中")).toBe(3);
    expect(escapedUtf8Length("é")).toBe(2);
    expect(escapedUtf8Length("😀")).toBe(4);
    expect(escapedUtf8Length("")).toBe(0);
  });
});

describe("isValidKeyName", () => {
  it("limits the escaped UTF-8 byte length to 100 and requires non-empty", () => {
    expect(isValidKeyName("中".repeat(33))).toBe(true);
    expect(isValidKeyName("中".repeat(34))).toBe(false);
    expect(isValidKeyName("x".repeat(100))).toBe(true);
    expect(isValidKeyName("x".repeat(101))).toBe(false);
    expect(isValidKeyName(`${"x".repeat(95)}&`)).toBe(true);
    expect(isValidKeyName(`${"x".repeat(96)}&`)).toBe(false);
    expect(isValidKeyName(`${"x".repeat(96)}<`)).toBe(true);
    expect(isValidKeyName(`${"x".repeat(97)}<`)).toBe(false);
    expect(isValidKeyName(`${"x".repeat(95)}"`)).toBe(true);
    expect(isValidKeyName(`${"x".repeat(96)}'`)).toBe(false);
    expect(isValidKeyName(`中中&&${"x".repeat(84)}`)).toBe(true);
    expect(isValidKeyName(`中中&&${"x".repeat(85)}`)).toBe(false);
    expect(isValidKeyName("😀".repeat(25))).toBe(true);
    expect(isValidKeyName("😀".repeat(26))).toBe(false);
    expect(isValidKeyName("")).toBe(false);
  });
});

describe("isSessionNeutralError", () => {
  it("treats network and idempotency-in-progress errors as unrelated to the session", () => {
    for (const code of [
      "TRANSIENT",
      "NETWORK_ERROR",
      "IDEMPOTENCY_IN_PROGRESS",
      "IDEMPOTENCY_RETRY_BACKOFF",
    ]) {
      expect(isSessionNeutralError(code)).toBe(true);
    }
    for (const code of [
      "TOKEN_REVOKED",
      "API_KEY_COUNT_EXCEEDED",
      "KEY_NOT_FOUND",
    ]) {
      expect(isSessionNeutralError(code)).toBe(false);
    }
  });
});
