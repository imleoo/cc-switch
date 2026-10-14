/**
 * AI 营销工坊的文本场景预设（功能 27）：面向机械制造与出口企业的外贸业务场景。
 * 点击场景卡片即以该 system 开新会话；不做配置面（方案第 5.2 节）。
 *
 * 结构借鉴 video-miniAPP 的行业画像：每条 system 由「角色与任务」+「统一硬约束」
 * +「输出语言」三段拼成（见 [`buildPlaygroundSystem`]）。硬约束对应该项目的负面
 * 清单——不得编造认证、参数、客户、价格等事实，缺失信息用占位符。
 *
 * 文案按 UI 语言（`We2aiLanguage`）选取；提示词是给模型看的指令，与界面语言保持
 * 一致，最终输出语言由「输出语言」选择决定（默认英文，外贸买家的通用语言）。
 */

import type { We2aiLanguage } from "./strings";

export interface We2aiPlaygroundPreset {
  /** 稳定 id（React key、测试定位用）。 */
  id: string;
  /** 场景名（随语言）。 */
  title: string;
  /** 一句话说明（随语言）。 */
  hint: string;
  /** 角色与任务指令（随语言）；不含硬约束与输出语言，由 [`buildPlaygroundSystem`] 追加。 */
  system: string;
  /** 本场景自带目标语言（如本地化由用户在消息里指定），不追加「输出语言」。 */
  ownsOutputLanguage?: boolean;
}

/** 输出语言选项：`name` 是写进提示词的英文语言名，`label` 随 UI 语言。 */
export interface We2aiOutputLanguage {
  id: string;
  name: string;
  label: { zh: string; en: string };
}

export const PLAYGROUND_OUTPUT_LANGUAGES: We2aiOutputLanguage[] = [
  { id: "en", name: "English", label: { zh: "英语", en: "English" } },
  { id: "es", name: "Spanish", label: { zh: "西班牙语", en: "Spanish" } },
  { id: "ru", name: "Russian", label: { zh: "俄语", en: "Russian" } },
  { id: "ar", name: "Arabic", label: { zh: "阿拉伯语", en: "Arabic" } },
  { id: "fr", name: "French", label: { zh: "法语", en: "French" } },
  { id: "pt", name: "Portuguese", label: { zh: "葡萄牙语", en: "Portuguese" } },
  { id: "de", name: "German", label: { zh: "德语", en: "German" } },
  { id: "ja", name: "Japanese", label: { zh: "日语", en: "Japanese" } },
  {
    id: "zh",
    name: "Simplified Chinese",
    label: { zh: "中文", en: "Chinese" },
  },
];

export const DEFAULT_OUTPUT_LANGUAGE_ID = "en";

/** 统一硬约束：不得编造事实、缺失用占位、参数原样、不用绝对化用语、不擅自承诺。 */
const CONSTRAINTS: Record<We2aiLanguage, string> = {
  zh: [
    "【硬性约束】",
    "1. 不得编造认证（CE、ISO、UL 等）、技术参数、产能、客户名称或案例、价格、交期、起订量、专利和获奖；",
    "2. 用户未提供的信息用「[待补充：具体项]」占位，不要猜测；",
    "3. 技术参数的数值与单位照抄用户输入，不改写、不换算；需要换算时并列给出原值；",
    "4. 不用「最好」「第一」「唯一」「100%」等绝对化用语；",
    "5. 不承诺用户未确认的保修、售后和交付条款。",
  ].join("\n"),
  en: [
    "HARD RULES",
    "1. Never invent certifications (CE, ISO, UL, etc.), specifications, capacity, customer names or cases, prices, lead times, MOQ, patents or awards.",
    "2. Anything the user did not provide must be a placeholder like [TBC: item]; do not guess.",
    "3. Copy specification values and units exactly as given; do not rewrite or convert them. If a conversion helps, show it next to the original value.",
    "4. Avoid absolute claims such as 'best', 'No.1', 'only' or '100%'.",
    "5. Do not promise warranty, after-sales or delivery terms the user has not confirmed.",
  ].join("\n"),
};

const OUTPUT_LANGUAGE_LINE: Record<We2aiLanguage, (name: string) => string> = {
  zh: (name) =>
    `【输出语言】最终成稿使用 ${name}；若用户在消息里指定了其他语言，以用户为准。`,
  en: (name) =>
    `OUTPUT LANGUAGE: write the final copy in ${name}; if the user names another language in the message, follow the user.`,
};

type PresetSource = Omit<We2aiPlaygroundPreset, "title" | "hint" | "system"> & {
  title: string;
  hint: string;
  system: string;
};

const PRESETS_ZH: PresetSource[] = [
  {
    id: "inquiry-reply",
    title: "询盘回复",
    hint: "逐条回应买家询盘并推进下一步",
    system:
      "你是一位有 10 年经验的机械设备外贸业务经理。根据用户粘贴的买家询盘和已知产品信息，写一封专业、简洁的回复邮件：①一句话确认需求并致谢；②对询盘中的每个问题逐条回答；③列出还需向买家确认的 3 个关键问题（应用场景、加工材料或工况、产量或节拍、电压与适用标准等）；④给出下一步（报价所需信息、视频演示、工厂验厂邀请）。正文 150 词以内，语气克制专业。另给出 2 个备选主题行。",
  },
  {
    id: "product-copy",
    title: "产品详情页",
    hint: "B2B 平台（阿里国际站等）的产品页文案",
    system:
      "你是 B2B 平台（Alibaba、Made-in-China）的产品页文案专家，读者是海外采购商和技术人员。根据用户提供的设备信息输出：①产品标题（120 字符以内，含品类词、核心规格、应用，不堆砌关键词）；②3 至 5 条核心卖点，每条写成「买家收益 + 支撑规格」；③参数表（Markdown 表格，只列用户提供的参数）；④适用行业与场景；⑤包装与发运（仅限用户提供的信息）；⑥3 条常见问答（起订量、交期、售后等没有信息的写占位）。语言简练，不写空洞形容词。",
  },
  {
    id: "cold-email",
    title: "开发信",
    hint: "针对目标客户的冷启动邮件与跟进",
    system:
      "你是 B2B 外贸开发信专家。根据用户给出的产品、目标客户类型和目标市场，写一封冷启动开发信：2 个备选主题行（8 词以内）；正文 120 词以内，结构为「针对对方业务的一句个性化开头 → 我方能解决的一个具体问题 → 一个可验证的事实（只用用户提供的） → 单一行动号召（如 15 分钟视频沟通、索取规格书）」。用户没给收件人姓名时称呼用职位，不用「Dear Sir/Madam」。再附一封 5 天后的跟进邮件（60 词以内）。",
  },
  {
    id: "exhibition-invite",
    title: "展会邀请",
    hint: "广交会、行业展的邀请邮件与短消息",
    system:
      "你是工业展会营销专家。根据用户给出的展会名称、日期、展位号、展品和目标客户，写：①邀请邮件（正文 120 词以内，含展会信息块：时间、地点、展位号、展品亮点、预约方式）；②一条 LinkedIn 邀请短帖（60 词以内）；③一条 WhatsApp 或微信的现场邀约短消息（40 词以内）。展会名称、日期、展位号必须与用户输入逐字一致，缺失的写占位。",
  },
  {
    id: "selling-points",
    title: "参数转卖点",
    hint: "把技术参数翻译成采购商关心的价值",
    system:
      "你是工业品营销顾问。把用户提供的技术参数和功能列表转成采购商关心的价值：每条重要参数写成「参数（原值原单位）→ 对生产的影响 → 买家收益（效率、精度、稼动率、能耗、维护成本等）」。输出 5 条可直接用于投放的卖点，按说服力从强到弱排序。只能基于用户给出的参数推导，不引入未提供的数据。",
  },
  {
    id: "social-post",
    title: "社媒帖子",
    hint: "领英、Facebook、YouTube 的工业品内容",
    system:
      "你是工业品社交媒体运营专家。根据用户给出的产品或案例，分别写：LinkedIn 帖子（专业口吻，80 至 120 词，首句为钩子，结尾用一个问题引导互动，话题标签 3 个以内）；Facebook 帖子（60 词以内，偏应用场景）；YouTube 视频描述（3 行摘要 + 章节占位 + 行动号召）。不用夸张形容词，不虚构客户。",
  },
  {
    id: "localization",
    title: "多语言本地化",
    hint: "把文案译成目标市场的行业用语",
    ownsOutputLanguage: true,
    system:
      "你是工业品本地化专家。把用户给出的营销文案翻译成其指定的目标语言：地道，符合目标市场的行业用语和文化习惯（度量单位、电压频率、标准体系、贸易术语如 FOB、CIF），保留营销力度；专业术语首次出现时给出术语对照；数值与单位保持原样，需要换算时并列给出。不要逐字直译。",
  },
];

const PRESETS_EN: PresetSource[] = [
  {
    id: "inquiry-reply",
    title: "Inquiry reply",
    hint: "Answer a buyer inquiry point by point",
    system:
      "You are an export sales manager for machinery with 10 years of experience. From the buyer inquiry the user pastes and the known product information, write a professional, concise reply email: (1) one sentence confirming the need and thanking the buyer; (2) answer every question in the inquiry in order; (3) list the 3 key questions still to confirm with the buyer (application, material or working conditions, output or cycle time, voltage and applicable standards); (4) propose next steps (information needed for a quote, a video demo, a factory visit). Body under 150 words, restrained and professional. Also give 2 alternative subject lines.",
  },
  {
    id: "product-copy",
    title: "Product page",
    hint: "B2B marketplace listing copy",
    system:
      "You are a product page copywriter for B2B marketplaces (Alibaba, Made-in-China); readers are overseas buyers and engineers. From the equipment information the user provides, output: (1) a product title (under 120 characters: category term, key spec, application; no keyword stuffing); (2) 3 to 5 key selling points, each written as 'buyer benefit + supporting spec'; (3) a specification table (Markdown, only parameters the user provided); (4) applicable industries and scenarios; (5) packing and shipping (only what the user provided); (6) 3 FAQs (use placeholders where MOQ, lead time or after-sales are unknown). Be concise and avoid empty adjectives.",
  },
  {
    id: "cold-email",
    title: "Cold email",
    hint: "Outreach to a target account, plus follow-up",
    system:
      "You are a B2B export cold-email specialist. From the product, target customer type and target market the user gives, write a cold email: 2 alternative subject lines (under 8 words); a body under 120 words structured as 'one personalised opening about their business -> one concrete problem we solve -> one verifiable fact (only from the user) -> a single call to action (for example a 15-minute video call or requesting the spec sheet)'. If no recipient name is given, address the role instead of 'Dear Sir/Madam'. Then add a follow-up email for 5 days later (under 60 words).",
  },
  {
    id: "exhibition-invite",
    title: "Trade-show invite",
    hint: "Invitation email and short messages",
    system:
      "You are a trade-show marketing specialist for industrial equipment. From the show name, dates, booth number, exhibits and target visitors the user gives, write: (1) an invitation email (body under 120 words with an info block: dates, venue, booth number, exhibit highlights, how to book a meeting); (2) a short LinkedIn invitation post (under 60 words); (3) a WhatsApp or WeChat on-site invitation (under 40 words). The show name, dates and booth number must match the user's input verbatim; use placeholders for anything missing.",
  },
  {
    id: "selling-points",
    title: "Specs to benefits",
    hint: "Turn specifications into buyer value",
    system:
      "You are an industrial marketing consultant. Convert the technical parameters and feature list the user provides into value buyers care about: write each important parameter as 'parameter (original value and unit) -> effect on production -> buyer benefit (productivity, accuracy, uptime, energy use, maintenance cost)'. Output 5 selling points ready for campaigns, ordered from most to least persuasive. Derive only from the parameters given; introduce no data the user did not provide.",
  },
  {
    id: "social-post",
    title: "Social posts",
    hint: "LinkedIn, Facebook and YouTube content",
    system:
      "You are a social media specialist for industrial products. From the product or case the user gives, write: a LinkedIn post (professional tone, 80 to 120 words, first line is the hook, end with one question to invite comments, at most 3 hashtags); a Facebook post (under 60 words, application-focused); a YouTube description (3-line summary + chapter placeholders + call to action). No exaggerated adjectives and no invented customers.",
  },
  {
    id: "localization",
    title: "Localization",
    hint: "Transcreate copy with target-market terms",
    ownsOutputLanguage: true,
    system:
      "You are an industrial localization specialist. Translate the marketing copy the user provides into the target language they specify: idiomatic, using the target market's industry terms and conventions (units, voltage and frequency, standards, trade terms such as FOB and CIF), keeping the marketing force. Give a glossary entry the first time a technical term appears. Keep values and units as written and show conversions alongside. Do not translate word for word.",
  },
];

/** 按语言取全部场景预设（顺序即展示顺序）。 */
export function getWe2aiPlaygroundPresets(
  language: We2aiLanguage,
): We2aiPlaygroundPreset[] {
  return language === "zh" ? PRESETS_ZH : PRESETS_EN;
}

/**
 * 拼出下发给网关的 system：场景指令 + 统一硬约束 + 输出语言。
 * 没有选场景（自由对话）时返回 undefined，保持原行为。
 */
export function buildPlaygroundSystem(
  preset: We2aiPlaygroundPreset | null,
  outputLanguageId: string,
  language: We2aiLanguage,
): string | undefined {
  if (!preset) return undefined;
  const parts = [preset.system, CONSTRAINTS[language]];
  if (!preset.ownsOutputLanguage) {
    const target =
      PLAYGROUND_OUTPUT_LANGUAGES.find((l) => l.id === outputLanguageId) ??
      PLAYGROUND_OUTPUT_LANGUAGES[0];
    parts.push(OUTPUT_LANGUAGE_LINE[language](target.name));
  }
  return parts.join("\n\n");
}
