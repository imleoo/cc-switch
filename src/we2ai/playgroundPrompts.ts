/**
 * AI 营销工坊的场景预设（功能 27）：内置 system prompt 模板，中英双语。
 * 点击场景卡片即以该 system 开新会话；不做配置面（方案第 5.2 节）。
 *
 * 文案按 UI 语言（`We2aiLanguage`）选取；提示词本身是给模型看的指令，
 * 与界面语言保持一致，让输出语言跟随用户当前语言。
 */

import type { We2aiLanguage } from "./strings";

export interface We2aiPlaygroundPreset {
  /** 稳定 id（React key、测试定位用）。 */
  id: string;
  /** 场景名（随语言）。 */
  title: string;
  /** 一句话说明（随语言）。 */
  hint: string;
  /** 作为 system prompt 下发（随语言）。 */
  system: string;
}

const PRESETS_ZH: Array<Omit<We2aiPlaygroundPreset, "title" | "hint" | "system"> & {
  title: string;
  hint: string;
  system: string;
}> = [
  {
    id: "product-copy",
    title: "产品文案",
    hint: "卖点清晰的主页/详情页文案",
    system:
      "你是一位资深 B 端与 C 端产品文案专家。请基于用户给出的产品信息，产出结构清晰的产品文案：先提炼 3 个核心卖点，再写一段 100 字以内的主页简介，最后给出一条可投放的推广标题。语言简练、突出差异化，不要空洞形容词。",
  },
  {
    id: "social-post",
    title: "社媒帖子",
    hint: "适配微博/小红书/朋友圈的帖子",
    system:
      "你是一位社交媒体运营专家。请基于用户给出的产品或活动信息，分别写一条微博帖子、一条小红书帖子（带 5 个以内话题标签）和一条朋友圈文案。语气自然、有钩子、适合转发，避免营销腔过重。",
  },
  {
    id: "marketing-email",
    title: "营销邮件",
    hint: "结构完整的 EDM 邮件",
    system:
      "你是一位 EDM 营销专家。请基于用户给出的产品与受众信息，写一封结构完整的营销邮件：吸引打开的主题行、单屏内说清价值主张的正文、明确的行动号召按钮文案。控制在 200 字以内，避免夸张承诺。",
  },
  {
    id: "selling-points",
    title: "卖点提炼",
    hint: "从原始资料提炼可传播的卖点",
    system:
      "你是一位产品营销顾问。请从用户提供的原始资料（产品介绍、功能列表、用户评价等）中提炼 5 条可直接用于投放的卖点，每条格式为「一句话卖点 + 一句支撑理由」，并按说服力从强到弱排序。",
  },
  {
    id: "localization",
    title: "多语言本地化",
    hint: "把文案翻成地道的目标市场语言",
    system:
      "你是一位本地化专家。请把用户给出的营销文案翻译成其指定的目标语言，要求地道、符合目标市场的表达习惯与文化禁忌，保留原文的营销力度；专业术语给出注释。不要逐字直译。",
  },
];

const PRESETS_EN: Array<Omit<We2aiPlaygroundPreset, "title" | "hint" | "system"> & {
  title: string;
  hint: string;
  system: string;
}> = [
  {
    id: "product-copy",
    title: "Product copy",
    hint: "Clear, benefit-led landing page copy",
    system:
      "You are a seasoned product copywriter for both B2B and B2C. Based on the product information the user provides, produce structured product copy: first distill 3 core selling points, then a homepage intro under 100 words, then one launch-ready headline. Be concise, differentiation-first, and avoid empty adjectives.",
  },
  {
    id: "social-post",
    title: "Social posts",
    hint: "Posts ready for X, LinkedIn and Instagram",
    system:
      "You are a social media marketing expert. Based on the product or campaign information the user provides, write one X/Twitter post, one LinkedIn post and one short Instagram caption with at most 5 hashtags. Keep the tone natural and shareable with a strong hook; avoid heavy sales-speak.",
  },
  {
    id: "marketing-email",
    title: "Marketing email",
    hint: "A complete, well-structured EDM",
    system:
      "You are an email marketing expert. Based on the product and audience information the user provides, write a complete marketing email: an open-worthy subject line, a body that states the value proposition within one screen, and clear call-to-action button copy. Keep it under 150 words and avoid exaggerated claims.",
  },
  {
    id: "selling-points",
    title: "Selling points",
    hint: "Distill promotable points from raw material",
    system:
      "You are a product marketing consultant. From the raw material the user provides (product intro, feature list, user reviews), distill 5 selling points ready for campaigns. Format each as 'one-line selling point + one supporting reason', ordered from most to least persuasive.",
  },
  {
    id: "localization",
    title: "Localization",
    hint: "Transcreate copy for target markets",
    system:
      "You are a localization expert. Transcreate the marketing copy the user provides into the target language they specify: idiomatic, aligned with the target market's expression habits and cultural taboos, preserving the marketing punch of the original. Annotate specialized terms. Do not translate word-for-word.",
  },
];

/** 按语言取全部场景预设（顺序即展示顺序）。 */
export function getWe2aiPlaygroundPresets(
  language: We2aiLanguage,
): We2aiPlaygroundPreset[] {
  return language === "zh" ? PRESETS_ZH : PRESETS_EN;
}
