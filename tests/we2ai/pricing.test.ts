import { describe, expect, it } from "vitest";
import type { We2aiModelPrice, We2aiPricing } from "@/we2ai/api";
import {
  buildWe2aiPerRequestPriceRow,
  buildWe2aiPriceRows,
  buildWe2aiTokenPriceRow,
  convertWe2aiUsdToCny,
  formatWe2aiMultiplier,
  formatWe2aiPriceAmounts,
  formatWe2aiPriceLine,
  formatWe2aiPriceNumber,
  isWe2aiModelSubjectToGroupPeak,
  isWe2aiPriceDiscounted,
  isWe2aiPriceSurcharged,
  resolveWe2aiEffectiveMultiplier,
} from "@/we2ai/pricing";

function pricing(overrides: Partial<We2aiPricing> = {}): We2aiPricing {
  return {
    cnyRate: 7.2,
    rateMultiplier: 0.5,
    peakMultiplier: 1,
    peakActive: false,
    effectiveMultiplier: 0.5,
    unit: "usd_per_1m_tokens",
    ...overrides,
  };
}

function price(overrides: Partial<We2aiModelPrice> = {}): We2aiModelPrice {
  return {
    billingMode: "token",
    input: null,
    output: null,
    cacheRead: null,
    cacheWrite: null,
    cacheWrite1h: null,
    perRequest: null,
    baseInput: null,
    baseOutput: null,
    baseCacheRead: null,
    baseCacheWrite: null,
    baseCacheWrite1h: null,
    basePerRequest: null,
    multiplier: null,
    perRequestUnit: "request",
    ...overrides,
  };
}

describe("formatWe2aiPriceNumber", () => {
  it("keeps exactly 2 decimals for values >= 1", () => {
    expect(formatWe2aiPriceNumber(1)).toBe("1.00");
    expect(formatWe2aiPriceNumber(12.345)).toBe("12.35");
    expect(formatWe2aiPriceNumber(1.5)).toBe("1.50");
  });

  it("keeps 4 significant figures and trims trailing zeros for values < 1", () => {
    expect(formatWe2aiPriceNumber(0.005)).toBe("0.005");
    expect(formatWe2aiPriceNumber(0.12345)).toBe("0.1235");
    expect(formatWe2aiPriceNumber(0.1)).toBe("0.1");
    expect(formatWe2aiPriceNumber(0.0001234)).toBe("0.0001234");
  });

  it("rounds a value just under 1 up into the >= 1 branch instead of showing 1.0000", () => {
    expect(formatWe2aiPriceNumber(0.99996)).toBe("1.00");
  });

  // Codex 验收 C1：极小的正数如果继续走"4 位有效数字对应的小数位数"这条
  // 路径，会算出超过 `toFixed` 上限（100）的小数位数而抛 `RangeError`，
  // 中断整个卡片渲染。低于 0.0001 一律显示 "<0.0001"，不再尝试精确渲染，
  // 也不能显示成 "0"（会被误读成免费）。
  it("shows a bounded \"<0.0001\" for extremely small values instead of crashing or showing 0", () => {
    expect(() => formatWe2aiPriceNumber(1e-98)).not.toThrow();
    expect(formatWe2aiPriceNumber(1e-98)).toBe("<0.0001");
    expect(formatWe2aiPriceNumber(1e-12)).toBe("<0.0001");
    expect(formatWe2aiPriceNumber(Number.MIN_VALUE)).toBe("<0.0001");
    expect(formatWe2aiPriceNumber(-1e-98)).toBe("-<0.0001");
    for (const value of [1e-98, 1e-12, Number.MIN_VALUE, -1e-98]) {
      expect(formatWe2aiPriceNumber(value)).not.toBe("0");
    }
  });

  it("still uses exact 4-significant-figure formatting right at and just below the 0.0001 threshold", () => {
    expect(formatWe2aiPriceNumber(0.0001)).toBe("0.0001");
    expect(formatWe2aiPriceNumber(0.00009999)).toBe("<0.0001");
  });

  it("shows zero as \"0\" but non-finite values as \"—\", not \"0\" (Opus 复核 P6)", () => {
    expect(formatWe2aiPriceNumber(0)).toBe("0");
    expect(formatWe2aiPriceNumber(Number.NaN)).toBe("—");
    expect(formatWe2aiPriceNumber(Number.POSITIVE_INFINITY)).toBe("—");
    expect(formatWe2aiPriceNumber(Number.NEGATIVE_INFINITY)).toBe("—");
  });

  it("keeps a negative sign", () => {
    expect(formatWe2aiPriceNumber(-0.005)).toBe("-0.005");
    expect(formatWe2aiPriceNumber(-2)).toBe("-2.00");
  });
});

describe("convertWe2aiUsdToCny / formatWe2aiPriceLine", () => {
  it("multiplies by cnyRate and formats both currencies", () => {
    expect(convertWe2aiUsdToCny(1, 7.2)).toBeCloseTo(7.2);
    expect(formatWe2aiPriceLine(1, 7.2)).toBe("¥7.20 / $1.00");
    expect(formatWe2aiPriceLine(0.1, 6.8)).toBe("¥0.68 / $0.1");
  });

  it("formats each currency independently through formatWe2aiPriceAmounts", () => {
    expect(formatWe2aiPriceAmounts(3, 7.2)).toEqual({
      cny: "21.60",
      usd: "3.00",
    });
  });
});

describe("buildWe2aiTokenPriceRow", () => {
  it("returns null when the field has no data", () => {
    expect(buildWe2aiTokenPriceRow(price(), pricing(), "input")).toBeNull();
  });

  it("shows the discounted price with a strikethrough base price when the multiplier is not 1", () => {
    const row = buildWe2aiTokenPriceRow(
      price({ input: 3, baseInput: 6 }),
      pricing({ effectiveMultiplier: 0.5, cnyRate: 7.2 }),
      "input",
    );
    expect(row).toEqual({
      field: "input",
      unit: "perMillionTokens",
      line: "¥21.60 / $3.00",
      strikethroughLine: "¥43.20 / $6.00",
    });
  });

  it("does not show a strikethrough price when the effective multiplier is 1", () => {
    const row = buildWe2aiTokenPriceRow(
      price({ input: 3, baseInput: 3 }),
      pricing({ effectiveMultiplier: 1 }),
      "input",
    );
    expect(row?.strikethroughLine).toBeNull();
  });

  it("does not show a strikethrough price when there is no base price to compare against", () => {
    const row = buildWe2aiTokenPriceRow(
      price({ input: 3, baseInput: null }),
      pricing({ effectiveMultiplier: 0.5 }),
      "input",
    );
    expect(row?.strikethroughLine).toBeNull();
  });
});

describe("buildWe2aiPerRequestPriceRow", () => {
  it("formats a per-request price the same way as token prices", () => {
    const row = buildWe2aiPerRequestPriceRow(
      price({ billingMode: "per_request", perRequest: 0.5, basePerRequest: 1 }),
      pricing({ effectiveMultiplier: 0.5, cnyRate: 7.2 }),
    );
    expect(row).toEqual({
      field: "perRequest",
      unit: "perRequest",
      line: "¥3.60 / $0.5",
      strikethroughLine: "¥7.20 / $1.00",
    });
  });

  // v3 契约：未识别的 per_request_unit（已在 Rust 侧归一化，这里模拟一个
  // 理论上不该出现、但仍要防御的取值）不展示这一行，避免展示错误单位。
  it("returns null when perRequestUnit is not recognized", () => {
    const row = buildWe2aiPerRequestPriceRow(
      price({
        billingMode: "per_request",
        perRequest: 0.5,
        perRequestUnit: null,
      }),
      pricing(),
    );
    expect(row).toBeNull();
  });

  it("uses the perSecond unit for a video model billed by the second", () => {
    const row = buildWe2aiPerRequestPriceRow(
      price({
        billingMode: "video",
        perRequest: 0.5,
        perRequestUnit: "second",
      }),
      pricing({ effectiveMultiplier: 1 }),
    );
    expect(row?.unit).toBe("perSecond");
  });
});

describe("buildWe2aiPriceRows", () => {
  it("returns an empty array when pricing is missing (no price area at all)", () => {
    expect(buildWe2aiPriceRows(price({ input: 3 }), null)).toEqual([]);
  });

  it("returns an empty array when the model has no price data (shows 'no pricing available')", () => {
    expect(buildWe2aiPriceRows(null, pricing())).toEqual([]);
  });

  it("builds input/output/cacheRead rows for token billing, skipping fields with no data", () => {
    const rows = buildWe2aiPriceRows(
      price({ input: 3, output: 15, cacheRead: null }),
      pricing({ effectiveMultiplier: 1 }),
    );
    expect(rows.map((r) => r.field)).toEqual(["input", "output"]);
  });

  it("builds a single row for per-request billing", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "per_request", perRequest: 0.02 }),
      pricing(),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].field).toBe("perRequest");
  });

  // Opus 复核 P2：image/video 等非 token 计费方式同样用 perRequest 兜底。
  it("shows a per-request row for image billing", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "image", perRequest: 0.02 }),
      pricing(),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].field).toBe("perRequest");
  });

  it("shows a per-request row for video billing", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "video", perRequest: 0.5 }),
      pricing(),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].field).toBe("perRequest");
  });

  it("falls back to a per-request row when billingMode is \"token\" but all token fields are empty", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "token", perRequest: 0.03 }),
      pricing(),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].field).toBe("perRequest");
  });

  it("shows \"no pricing\" (empty rows) when billingMode is not token and there is no perRequest either", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "image", perRequest: null }),
      pricing(),
    );
    expect(rows).toEqual([]);
  });

  // Opus 复核 Q4：billing_mode 缺省（服务端归一化为空字符串时的防御性
  // 兜底）按 "token" 处理，而不是被误判成"非 token"从而只显示按次价。
  it("treats an empty billingMode as \"token\" (Q4)", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "", input: 3, output: 15 }),
      pricing({ effectiveMultiplier: 1 }),
    );
    expect(rows.map((r) => r.field)).toEqual(["input", "output"]);
  });

  // Opus 复核 Q5：非 token 计费方式没有 perRequest，但确实有 token 字段
  // 时，兜底展示 token 行，而不是直接判"暂无定价"。
  it("falls back to token rows when a non-token billingMode has no perRequest but does have token fields (Q5)", () => {
    const rows = buildWe2aiPriceRows(
      price({ billingMode: "image", perRequest: null, input: 3 }),
      pricing({ effectiveMultiplier: 1 }),
    );
    expect(rows.map((r) => r.field)).toEqual(["input"]);
  });

  // v3 契约：per_request_unit 不被识别时不能展示这一行（避免错误单位）；
  // 有 token 字段时按 Q5 同样的逻辑兜底展示，没有则"暂无定价"。
  it("falls back to token rows when perRequestUnit is unrecognized but token fields exist", () => {
    const rows = buildWe2aiPriceRows(
      price({
        billingMode: "image",
        perRequest: 0.5,
        perRequestUnit: null,
        input: 3,
      }),
      pricing({ effectiveMultiplier: 1 }),
    );
    expect(rows.map((r) => r.field)).toEqual(["input"]);
  });

  it("shows no pricing when perRequestUnit is unrecognized and there are no token fields either", () => {
    const rows = buildWe2aiPriceRows(
      price({
        billingMode: "image",
        perRequest: 0.5,
        perRequestUnit: null,
      }),
      pricing(),
    );
    expect(rows).toEqual([]);
  });
});

describe("isWe2aiModelSubjectToGroupPeak (追加需求，取代 Q2 的 billingMode 判定)", () => {
  it("is true for a null price (nothing to override the top-level multiplier with)", () => {
    expect(isWe2aiModelSubjectToGroupPeak(null, pricing())).toBe(true);
  });

  it("is true when the model has no multiplier of its own, regardless of billingMode", () => {
    expect(
      isWe2aiModelSubjectToGroupPeak(
        price({ billingMode: "image", multiplier: null }),
        pricing({ effectiveMultiplier: 1.5 }),
      ),
    ).toBe(true);
  });

  it("is true for a non-token (per_request) model whose own multiplier numerically matches the top-level one", () => {
    // 这正是取代 billingMode 判定的原因：普通 per_request 模型也可能叠加
    // 分组高峰，不能仅凭 billingMode !== "token" 就排除。
    expect(
      isWe2aiModelSubjectToGroupPeak(
        price({ billingMode: "per_request", multiplier: 1.5 }),
        pricing({ effectiveMultiplier: 1.5 }),
      ),
    ).toBe(true);
  });

  it("is true when the multiplier matches within the tolerance (floating-point noise)", () => {
    expect(
      isWe2aiModelSubjectToGroupPeak(
        price({ multiplier: 1.5 + 1e-10 }),
        pricing({ effectiveMultiplier: 1.5 }),
      ),
    ).toBe(true);
  });

  it("is false for an image/video model whose own multiplier does not match the top-level one", () => {
    expect(
      isWe2aiModelSubjectToGroupPeak(
        price({ billingMode: "image", multiplier: 2 }),
        pricing({ effectiveMultiplier: 4 }),
      ),
    ).toBe(false);
  });

  it("is true for a token-billing model with a matching multiplier (positive control)", () => {
    expect(
      isWe2aiModelSubjectToGroupPeak(
        price({ billingMode: "token", multiplier: 1.5 }),
        pricing({ effectiveMultiplier: 1.5 }),
      ),
    ).toBe(true);
  });
});

describe("multiplier tolerance and formatting (Opus 复核 P4/P5)", () => {
  it("treats a multiplier within the epsilon of 1 as neither discounted nor surcharged", () => {
    expect(isWe2aiPriceDiscounted(0.9999999999)).toBe(false);
    expect(isWe2aiPriceSurcharged(1.0000000001)).toBe(false);
    expect(isWe2aiPriceDiscounted(1)).toBe(false);
    expect(isWe2aiPriceSurcharged(1)).toBe(false);
  });

  it("detects a real discount or surcharge outside the tolerance", () => {
    expect(isWe2aiPriceDiscounted(0.5)).toBe(true);
    expect(isWe2aiPriceSurcharged(1.5)).toBe(true);
    expect(isWe2aiPriceDiscounted(1.5)).toBe(false);
    expect(isWe2aiPriceSurcharged(0.5)).toBe(false);
  });

  it("formats a multiplier with up to 4 significant figures, trimming trailing zeros", () => {
    expect(formatWe2aiMultiplier(1.5)).toBe("1.5");
    expect(formatWe2aiMultiplier(2)).toBe("2");
    expect(formatWe2aiMultiplier(1.25)).toBe("1.25");
    expect(formatWe2aiMultiplier(Number.NaN)).toBe("—");
  });

  // Codex 验收 C3：固定 2 位小数会把"贴近 1 但不等于 1"的倍率四舍五入
  // 成 1.00 再去尾零变成 "1"，和真正的"没有加价"（倍率恰好为 1）无法
  // 区分。改用 4 位有效数字后必须能保留这类差异。
  it("keeps a multiplier close to 1 distinguishable from exactly 1", () => {
    expect(formatWe2aiMultiplier(1.004)).toBe("1.004");
    expect(formatWe2aiMultiplier(0.996)).toBe("0.996");
    expect(formatWe2aiMultiplier(1)).toBe("1");
  });

  // Codex 复验 C5：即使经过 C3 修复（4 位有效数字），像 `1.0004` 这种值
  // 用 4 位有效数字四舍五入后恰好也是 "1.000" → 去尾零变成 "1"，和真正
  // 的"没有加价"无法区分。需要逐步升级到更多有效数字，直到能与 "1" 区分。
  it("escalates precision beyond 4 significant figures for values extremely close to 1", () => {
    expect(formatWe2aiMultiplier(1.0004)).toBe("1.0004");
    expect(formatWe2aiMultiplier(1.00001)).toBe("1.00001");
    expect(formatWe2aiMultiplier(0.99999)).toBe("0.99999");
  });

  it("falls back to \"≈1\" when even 10 significant figures cannot distinguish the value from 1", () => {
    // 差值贴着 1e-9 容差边界，即使升到 10 位有效数字仍然会被
    // 四舍五入回 "1.000000000"。
    expect(formatWe2aiMultiplier(1 + 4e-11)).toBe("≈1");
    expect(formatWe2aiMultiplier(1 - 4e-11)).toBe("≈1");
  });

  it("does not need the \"≈1\" fallback once the difference is large enough for 10 significant figures to show it", () => {
    // 有效数字算法对 <1 的一侧多留一位小数（整数部分的"1"本身不占用一位
    // 有效数字），所以同样 10 位有效数字上限下，<1 一侧能分辨的最小差值
    // 比 >1 一侧更小——这是算法固有的不对称，不是 bug。
    expect(formatWe2aiMultiplier(1 - 1e-10)).toBe("0.9999999999");
  });

  it("returns exactly \"1\" (not \"≈1\") when the multiplier truly equals 1", () => {
    expect(formatWe2aiMultiplier(1)).toBe("1");
  });

  it("does not show a strikethrough when the discounted/base lines are textually identical despite the multiplier not being exactly 1", () => {
    // 0.999999999 与 1 的差在容差之外时按普通折扣路径处理，但四舍五入到
    // 展示精度后两行文字可能凑巧相同，这种情况不应该展示重复的划线价。
    const row = buildWe2aiTokenPriceRow(
      price({ input: 3.0000000001, baseInput: 3 }),
      pricing({ effectiveMultiplier: 0.99999999, cnyRate: 1 }),
      "input",
    );
    expect(row?.strikethroughLine).toBeNull();
  });

  it("never shows a strikethrough for a surcharge (multiplier > 1), even with base data available", () => {
    const row = buildWe2aiTokenPriceRow(
      price({ input: 6, baseInput: 3 }),
      pricing({ effectiveMultiplier: 2 }),
      "input",
    );
    expect(row?.line).toBe("¥43.20 / $6.00");
    expect(row?.strikethroughLine).toBeNull();
  });
});

describe("resolveWe2aiEffectiveMultiplier (v2 契约：模型级 multiplier)", () => {
  it("prefers the model's own multiplier over the top-level pricing multiplier", () => {
    const p = price({ multiplier: 0.5 });
    const pr = pricing({ effectiveMultiplier: 2 });
    expect(resolveWe2aiEffectiveMultiplier(p, pr)).toBe(0.5);
  });

  it("falls back to the top-level pricing multiplier when the model has none", () => {
    const p = price({ multiplier: null });
    const pr = pricing({ effectiveMultiplier: 2 });
    expect(resolveWe2aiEffectiveMultiplier(p, pr)).toBe(2);
  });

  it("uses the model's own (lower) multiplier for an image model even though the group is at peak pricing", () => {
    // 图片模型有独立倍率（0.5），不应该被顶层因分组高峰而升高的倍率（2）
    // 误判为加价。
    const row = buildWe2aiPerRequestPriceRow(
      price({
        billingMode: "image",
        perRequest: 0.02,
        basePerRequest: 0.04,
        multiplier: 0.5,
      }),
      pricing({ effectiveMultiplier: 2, cnyRate: 1 }),
    );
    expect(row?.line).toBe("¥0.02 / $0.02");
    expect(row?.strikethroughLine).toBe("¥0.04 / $0.04");
  });
});
