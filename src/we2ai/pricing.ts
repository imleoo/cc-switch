/**
 * 模型广场分组折扣价的纯格式化函数（B1 定价扩展，
 * `docs/we2ai/B1定价契约.md`"客户端展示规则"）。
 *
 * 独立于渲染逻辑，方便单测覆盖数字格式边界；`We2aiPricing`/`We2aiModelPrice`
 * 的字段含义见 `src/we2ai/api.ts`。
 */

import type { We2aiModelPrice, We2aiPricing } from "./api";

/**
 * <1 分支的最小可展示绝对值（Codex 验收 C1）：低于这个值一律显示
 * `"<0.0001"`，不再尝试精确渲染。极小的正数（如 `1e-98`）如果继续走
 * "4 位有效数字对应的小数位数"这条路径，会算出超过 `toFixed` 上限
 * （100）的小数位数而抛 `RangeError`，中断整个卡片渲染；即使算出来的
 * 位数没有超过上限（如 `1e-12`），展示几十个前导零对用户也没有意义。
 * `"<0.0001"` 而不是 `"0"`——"0" 会被误读成免费。
 */
const MIN_DISPLAYABLE_ABS = 0.0001;

/**
 * 契约规定的数字格式：≥1 保留 2 位小数；<1 保留 4 位有效数字，去掉多余
 * 尾零，但低于 `MIN_DISPLAYABLE_ABS` 时显示 `"<0.0001"`（见上，Codex
 * 验收 C1）。用手动计算有效数字位数而不是 `toPrecision`，避免科学计数法
 * 输出，也避免四舍五入进位到 1 之后仍停留在 <1 分支（如 0.99996 应显示
 * 为 "1.00" 而不是 "1.0000"）。
 *
 * 非有限值（`NaN`/`Infinity`）返回 "—"，不返回 "0"（Opus 复核 P6）——"0"
 * 是一个具体的、可能被误读为"免费"的价格，调用方在正常路径下应当已经
 * 把非有限值过滤掉（Rust 侧 `finite_nonnegative`/`finite_positive`），这里
 * 只是这个纯函数自身的防御性兜底，不应该悄悄编造一个看似正常的价格。
 */
export function formatWe2aiPriceNumber(value: number): string {
  if (!Number.isFinite(value)) return "—";
  if (value === 0) return "0";
  const sign = value < 0 ? "-" : "";
  const abs = Math.abs(value);

  if (abs >= 1) {
    return sign + abs.toFixed(2);
  }

  if (abs < MIN_DISPLAYABLE_ABS) {
    return `${sign}<${MIN_DISPLAYABLE_ABS}`;
  }

  const exponent = Math.floor(Math.log10(abs));
  const decimals = Math.max(4 - 1 - exponent, 0);
  let text = abs.toFixed(decimals);

  // 四舍五入可能把 <1 的值进位到 1 及以上（如 0.99996 → "1.0000"）。
  if (Number(text) >= 1) {
    return sign + Number(text).toFixed(2);
  }

  if (text.includes(".")) {
    text = text.replace(/0+$/, "").replace(/\.$/, "");
  }
  return sign + text;
}

/** 美元单价换算为人民币：人民币 = 美元 × `pricing.cnyRate`。 */
export function convertWe2aiUsdToCny(usd: number, cnyRate: number): number {
  return usd * cnyRate;
}

/** 单条价格展示行的格式化数值（未拼接货币符号，供组件自行排版）。 */
export interface We2aiPriceAmounts {
  cny: string;
  usd: string;
}

export function formatWe2aiPriceAmounts(
  usd: number,
  cnyRate: number,
): We2aiPriceAmounts {
  return {
    cny: formatWe2aiPriceNumber(convertWe2aiUsdToCny(usd, cnyRate)),
    usd: formatWe2aiPriceNumber(usd),
  };
}

/** `¥x.xx / $y.yy` 形式的展示文案。 */
export function formatWe2aiPriceLine(usd: number, cnyRate: number): string {
  const { cny, usd: usdText } = formatWe2aiPriceAmounts(usd, cnyRate);
  return `¥${cny} / $${usdText}`;
}

export type We2aiTokenPriceField =
  | "input"
  | "output"
  | "cacheRead"
  | "cacheWrite"
  | "cacheWrite1h";

type We2aiNumericPriceField = Exclude<
  keyof We2aiModelPrice,
  "billingMode" | "perRequestUnit"
>;

const BASE_FIELD: Record<We2aiTokenPriceField, We2aiNumericPriceField> = {
  input: "baseInput",
  output: "baseOutput",
  cacheRead: "baseCacheRead",
  cacheWrite: "baseCacheWrite",
  cacheWrite1h: "baseCacheWrite1h",
};

/**
 * 倍率比较容差（Opus 复核 P4）：浮点运算（`rate_multiplier × peak_multiplier`
 * 在服务端算出 `effective_multiplier`）可能让"逻辑上等于 1"的倍率变成
 * `0.9999999999999999` 这类值，直接 `!== 1` 比较会误判成"有折扣/加价"。
 */
const MULTIPLIER_EPSILON = 1e-9;

/** 倍率 < 1（有折扣）。 */
export function isWe2aiPriceDiscounted(effectiveMultiplier: number): boolean {
  return 1 - effectiveMultiplier > MULTIPLIER_EPSILON;
}

/** 倍率 > 1（高峰/加价）——Opus 复核 P5 产品决定：这种情况不显示划线原价，
 * 改在价格区显示"×{m} 倍率"标注。 */
export function isWe2aiPriceSurcharged(effectiveMultiplier: number): boolean {
  return effectiveMultiplier - 1 > MULTIPLIER_EPSILON;
}

/**
 * 该模型实际生效的倍率（v2 契约）：模型级 `price.multiplier` 优先——
 * image/video 类有独立的图片/视频倍率，不叠加分组高峰；token 类通常没有
 * 这个字段（由顶层 `rate_multiplier × peak_multiplier` 覆盖）。模型级字段
 * 缺失（或服务端给出的值无效，已在 Rust 侧过滤为 `null`）时回退到顶层
 * `pricing.effectiveMultiplier`。划线原价与"×{m} 倍率"标注都必须用这个
 * 解析后的值判定，不能直接用顶层倍率——否则 image/video 模型会被顶层的
 * 分组高峰倍率错误影响。
 */
export function resolveWe2aiEffectiveMultiplier(
  price: We2aiModelPrice,
  pricing: We2aiPricing,
): number {
  return price.multiplier ?? pricing.effectiveMultiplier;
}

/** 按 `sig` 位有效数字格式化一个正数，去掉多余尾零；`decimals` 做了
 * 上限保护，避免极端值触发 `toFixed` 的位数上限。 */
function formatSignificantDigits(abs: number, sig: number): string {
  const exponent = Math.floor(Math.log10(abs));
  const decimals = Math.min(Math.max(sig - 1 - exponent, 0), 20);
  let text = abs.toFixed(decimals);
  if (text.includes(".")) {
    text = text.replace(/0+$/, "").replace(/\.$/, "");
  }
  return text;
}

/** `formatWe2aiMultiplier` 逐步升级精度时尝试的最高有效数字位数
 * （Codex 复验 C5）。 */
const MAX_MULTIPLIER_SIGNIFICANT_DIGITS = 10;

/**
 * 倍率的展示格式：最多 4 位有效数字，去掉多余尾零（"1.5000" → "1.5"，
 * "2.000" → "2"，"1.0040" → "1.004"）。非有限值返回 "—"。与
 * `formatWe2aiPriceNumber` 分开是因为倍率不是货币，不需要"≥1 固定 2 位
 * 小数"这条货币展示规则——固定 2 位小数曾经把 `1.004` 这类"贴近 1 但不
 * 等于 1"的倍率四舍五入成 `1.00`、再去尾零变成 `"1"`，与"没有加价"的
 * `1` 无法区分（Codex 验收 C3）。
 *
 * 但只把有效数字从 2 位提到 4 位并不能根治问题（Codex 复验 C5）：
 * `1.0004` 恰好用 4 位有效数字四舍五入也会变成 `1.000` → 去尾零后仍是
 * `"1"`，调用方（`isWe2aiPriceSurcharged`/`isWe2aiPriceDiscounted`）已经
 * 判定这个值确实偏离 1（容差 `1e-9` 之外），不能让它被格式化成看起来
 * "没有加价/折扣"的 `"1"`。真正等于 1 时直接返回 `"1"`；不等于 1 时逐步
 * 把有效数字从 4 位提到最多 10 位，直到格式化结果不再是 `"1"`；即使到
 * 10 位仍然是 `"1"`（差值极小，贴着 `1e-9` 容差边界，属于浮点精度的
 * 固有极限)，也不能显示成容易被误读为"没有加价"的 `"1"`，改为显示
 * `"≈1"`（前缀"≈"，中英通用，不需要分别翻译）。
 */
export function formatWe2aiMultiplier(value: number): string {
  if (!Number.isFinite(value)) return "—";
  if (value === 0) return "0";
  const sign = value < 0 ? "-" : "";
  const abs = Math.abs(value);

  if (abs === 1) return `${sign}1`;

  for (let sig = 4; sig <= MAX_MULTIPLIER_SIGNIFICANT_DIGITS; sig++) {
    const text = formatSignificantDigits(abs, sig);
    if (text !== "1") {
      return sign + text;
    }
  }
  return `${sign}≈1`;
}

/**
 * 一条价格行的展示单位（v3 契约追加 `perSecond`）：token 类固定"每百万
 * tokens"；按次计费按 `price.perRequestUnit` 决定是"每次"还是"每秒"
 * （视频类）。
 */
export type We2aiPriceRowUnit = "perMillionTokens" | "perRequest" | "perSecond";

/** 单条价格行的展示数据。 */
export interface We2aiPriceRow {
  field: We2aiTokenPriceField | "perRequest";
  unit: We2aiPriceRowUnit;
  /** `¥x.xx / $y.yy`（token 类）或按次计费的同格式文案。 */
  line: string;
  /**
   * 倍率 < 1（有折扣）且有原价数据、且原价文案与折后价文案不同（Opus 复核
   * P4：容差比较之后仍可能因四舍五入巧合显示成同一串文字，这种情况不展示
   * 重复的划线价）时才给出；倍率 ≥ 1（含加价，Opus 复核 P5）或没有原价
   * 数据时为 `null`。
   */
  strikethroughLine: string | null;
}

/** 单条价格行折后价与（可能有的）原价划线共用的构造逻辑。`multiplier` 是
 * 已经按 `resolveWe2aiEffectiveMultiplier` 解析过的、这个模型实际生效的
 * 倍率——调用方负责解析一次并传入，避免每个字段各自重新解析。 */
function buildPriceRowFromValues(
  field: We2aiPriceRow["field"],
  unit: We2aiPriceRowUnit,
  value: number,
  baseValue: number | null,
  cnyRate: number,
  multiplier: number,
): We2aiPriceRow {
  const line = formatWe2aiPriceLine(value, cnyRate);
  let strikethroughLine: string | null = null;
  if (isWe2aiPriceDiscounted(multiplier) && baseValue != null) {
    const baseLine = formatWe2aiPriceLine(baseValue, cnyRate);
    strikethroughLine = baseLine !== line ? baseLine : null;
  }
  return { field, unit, line, strikethroughLine };
}

/**
 * 某个 token 类字段（输入/输出/缓存读/缓存写）的展示行；该字段无数据时
 * 返回 `null`（契约"客户端展示规则"：有则显示）。
 */
export function buildWe2aiTokenPriceRow(
  price: We2aiModelPrice,
  pricing: We2aiPricing,
  field: We2aiTokenPriceField,
): We2aiPriceRow | null {
  const value = price[field];
  if (value == null) return null;
  return buildPriceRowFromValues(
    field,
    "perMillionTokens",
    value,
    price[BASE_FIELD[field]],
    pricing.cnyRate,
    resolveWe2aiEffectiveMultiplier(price, pricing),
  );
}

/**
 * 按次计费的展示行（`billingMode !== "token"` 且有 `perRequest`；也用作
 * `billingMode === "token"` 但 token 字段全空时的兜底，见
 * `buildWe2aiPriceRows`）。`perRequestUnit` 未识别（已在 Rust 侧归一化，
 * 这里只会看到 `"request"`/`"second"`/`null` 三种取值）时返回 `null`——
 * v3 契约：宁可不展示这一行，也不能展示错误的单位。
 */
export function buildWe2aiPerRequestPriceRow(
  price: We2aiModelPrice,
  pricing: We2aiPricing,
): We2aiPriceRow | null {
  const value = price.perRequest;
  if (value == null) return null;
  const unit: We2aiPriceRowUnit | null =
    price.perRequestUnit === "second"
      ? "perSecond"
      : price.perRequestUnit === "request"
        ? "perRequest"
        : null;
  if (unit == null) return null;
  return buildPriceRowFromValues(
    "perRequest",
    unit,
    value,
    price.basePerRequest,
    pricing.cnyRate,
    resolveWe2aiEffectiveMultiplier(price, pricing),
  );
}

const TOKEN_PRICE_FIELDS: We2aiTokenPriceField[] = [
  "input",
  "output",
  "cacheRead",
];

/**
 * 服务端缺省/空字符串 `billingMode` 已在 Rust 侧归一化为 `"token"`
 * （Opus 复核 Q4，`keys.rs::to_price_view`）；这里再做一次防御性兜底，
 * 避免未来某处绕过那层归一化时前端仍然把空字符串误判成"非 token"。
 */
function normalizedBillingMode(price: We2aiModelPrice): string {
  return price.billingMode === "" ? "token" : price.billingMode;
}

/**
 * 一个模型卡片要展示的全部价格行（Opus 复核 P2、Q4、Q5）：
 * - `billingMode`（缺省/空字符串按 "token" 处理，Q4）为 `"token"` →
 *   展示输入 / 输出 / 缓存读（有则显示）；全部为空时用 `perRequest` 兜底
 *   显示按次价，而不是直接判"暂无定价"。
 * - `billingMode !== "token"`（`per_request`/`image`/`video` 等契约里列出
 *   的非 token 计费方式）且有 `perRequest` → 显示按次这一行；没有
 *   `perRequest` 但有 token 字段时，同样兜底展示 token 行（Q5，数据本身
 *   有价格，只是分类字段与实际字段对不上，不应该直接判"暂无定价"）。
 * `pricing` 缺失时返回空数组，调用方据此判断"暂无定价"还是"不显示价格区"。
 */
export function buildWe2aiPriceRows(
  price: We2aiModelPrice | null,
  pricing: We2aiPricing | null,
): We2aiPriceRow[] {
  if (!price || !pricing) return [];
  const tokenRows = TOKEN_PRICE_FIELDS.map((field) =>
    buildWe2aiTokenPriceRow(price, pricing, field),
  ).filter((row): row is We2aiPriceRow => row !== null);
  const perRequestRow = buildWe2aiPerRequestPriceRow(price, pricing);

  if (normalizedBillingMode(price) === "token") {
    if (tokenRows.length > 0) return tokenRows;
    return perRequestRow ? [perRequestRow] : [];
  }
  if (perRequestRow) return [perRequestRow];
  return tokenRows;
}

/**
 * 该模型是否实际叠加了分组高峰倍率（追加需求，取代 Q2 里"按
 * `billingMode === 'token'` 判定"的做法）：`billing_mode` 不是判定依据——
 * 普通 `per_request` 计费的模型也可能叠加分组高峰，仅凭计费方式字符串
 * 会误判。改为直接比较数值：该模型没有自己的 `multiplier`（缺省），或者
 * 有但数值上（容差 `1e-9` 内）等于顶层 `pricing.effectiveMultiplier`，
 * 就说明它确实是按顶层倍率（含分组高峰）计价的；image/video 类模型的
 * `multiplier` 是独立的图片/视频倍率，数值上不会等于顶层倍率，因此判定
 * 为 `false`，不跟着标"高峰价"。
 */
export function isWe2aiModelSubjectToGroupPeak(
  price: We2aiModelPrice | null,
  pricing: We2aiPricing,
): boolean {
  if (!price || price.multiplier == null) return true;
  return Math.abs(price.multiplier - pricing.effectiveMultiplier) <= MULTIPLIER_EPSILON;
}

