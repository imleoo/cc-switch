/**
 * AI 营销工坊的图片 / 视频模板（功能 27，P2/P3）：面向机械制造与出口企业。
 *
 * 结构借鉴 video-miniAPP 的「行业画像 + 五镜分镜」：
 * - 全局固定参数（写实纪实质感、画面不出现任何文字 / Logo / 认证标志）；
 * - 五段式分镜，每镜带固定运镜；时长不足 10 秒时压缩为三镜；
 * - 负面清单：不得编造图片中没有的参数、认证、客户；人物只出现背影 / 手部，
 *   既规避人脸，也规避 Seedance 对真人首帧必须走 asset 的限制；
 * - 业务关键词用 `[]` 锁定（产品名、型号），让 AI 编译与人工修改都能辨认。
 *
 * 提示词语言：图片一律英文（生图模型对英文工业术语更稳）；视频 Seedance 用中文、
 * Grok 用英文。用户填写的字段值原样嵌入，不翻译。
 */

import type { We2aiLanguage } from "./strings";

export type StudioPromptLanguage = "zh" | "en";

type Bi = Record<We2aiLanguage, string>;

export interface StudioFieldDef {
  id: string;
  label: Bi;
  placeholder: Bi;
  required?: boolean;
  multiline?: boolean;
}

export interface StudioField {
  id: string;
  label: string;
  placeholder: string;
  required: boolean;
  multiline: boolean;
}

export type StudioValues = Record<string, string>;

const val = (values: StudioValues, id: string): string =>
  (values[id] ?? "").trim();

// ---------------------------------------------------------------------------
// 公共字段（各模板按需组合）
// ---------------------------------------------------------------------------

const F_PRODUCT: StudioFieldDef = {
  id: "productName",
  required: true,
  label: { zh: "产品名称", en: "Product name" },
  placeholder: {
    zh: "数控加工中心 VMC-850",
    en: "CNC machining center VMC-850",
  },
};

const F_APPEARANCE: StudioFieldDef = {
  id: "appearance",
  label: { zh: "外观与材质", en: "Appearance & materials" },
  placeholder: {
    zh: "灰白机身、蓝色防护门、不锈钢工作台",
    en: "grey-white body, blue guard doors, stainless steel table",
  },
};

const F_FEATURES: StudioFieldDef = {
  id: "features",
  label: { zh: "核心特征", en: "Key features" },
  placeholder: {
    zh: "三轴联动、高刚性铸铁床身（只写真实具备的）",
    en: "3-axis, rigid cast-iron bed (only real features)",
  },
};

const F_INDUSTRY: StudioFieldDef = {
  id: "industryUse",
  label: { zh: "应用行业", en: "Application industry" },
  placeholder: { zh: "汽车零部件加工", en: "automotive parts machining" },
};

const F_PROCESS: StudioFieldDef = {
  id: "process",
  label: { zh: "加工或工艺过程", en: "Process shown" },
  placeholder: {
    zh: "高速铣削、冷却液冲刷",
    en: "high-speed milling with coolant",
  },
};

const F_SETTING: StudioFieldDef = {
  id: "setting",
  label: { zh: "场景补充", en: "Scene notes" },
  placeholder: {
    zh: "两排设备、天车、地面划线",
    en: "two rows of machines, overhead crane, floor markings",
  },
};

const F_PACKING: StudioFieldDef = {
  id: "packing",
  label: { zh: "包装方式", en: "Packing method" },
  placeholder: {
    zh: "木箱 + 防锈膜 + 钢带",
    en: "wooden crate + rust-proof film + steel strapping",
  },
};

function localizeFields(
  defs: StudioFieldDef[],
  language: We2aiLanguage,
): StudioField[] {
  return defs.map((d) => ({
    id: d.id,
    label: d.label[language],
    placeholder: d.placeholder[language],
    required: d.required === true,
    multiline: d.multiline === true,
  }));
}

/** 必填字段是否都已填写。 */
export function missingRequiredFields(
  fields: StudioField[],
  values: StudioValues,
): StudioField[] {
  return fields.filter((f) => f.required && val(values, f.id) === "");
}

// ---------------------------------------------------------------------------
// 图片模板
// ---------------------------------------------------------------------------

/** 画面里的统一约束（对应参考项目的全局固定参数 + 负面清单）。 */
const IMAGE_GLOBAL_EN =
  "Photorealistic industrial photography. No text, letters, numbers, logos, watermarks, certification marks or brand marks anywhere in the image. No identifiable human faces; if workers appear they are seen from behind or at a distance, wearing safety helmets and protective gear. Mechanically plausible structure: do not add parts, buttons, screens or features that are not described.";

const IMAGE_REFERENCE_EN =
  "Use the provided reference image as the exact product: preserve its shape, proportions, color, materials and structure, and change only the background, lighting and camera angle as described.";

export interface StudioImageTemplate {
  id: string;
  title: Bi;
  hint: Bi;
  fields: StudioFieldDef[];
  /** 偏好的构图；会按所选模型的能力表取最接近的可用值。 */
  framing: { size: string; aspectRatio: string };
  build: (values: StudioValues) => string;
}

function subject(values: StudioValues): string {
  const name = val(values, "productName") || "the machine";
  const appearance = val(values, "appearance");
  return appearance ? `${name} (${appearance})` : name;
}

function emphasise(values: StudioValues): string {
  const features = val(values, "features");
  return features ? ` Emphasise: ${features}.` : "";
}

const IMAGE_TEMPLATES: StudioImageTemplate[] = [
  {
    id: "hero-product",
    title: { zh: "设备主图", en: "Hero product shot" },
    hint: { zh: "浅灰棚拍，询盘站首图", en: "Studio catalog main image" },
    fields: [F_PRODUCT, F_APPEARANCE, F_FEATURES],
    framing: { size: "1024x1024", aspectRatio: "1:1" },
    build: (v) =>
      `Professional catalog photograph of ${subject(v)}. Three-quarter front view, the whole machine in sharp focus on a clean light-grey seamless studio background, soft even studio lighting with a subtle floor reflection, realistic metal surfaces and paint finish.${emphasise(v)}`,
  },
  {
    id: "factory-floor",
    title: { zh: "工厂车间实景", en: "Factory floor" },
    hint: {
      zh: "整洁车间，展示生产实力",
      en: "Clean workshop showing capability",
    },
    fields: [F_PRODUCT, F_APPEARANCE, F_SETTING],
    framing: { size: "1536x1024", aspectRatio: "16:9" },
    build: (v) =>
      `Wide interior photograph of a modern, clean machinery workshop with ${subject(v)} installed on the production floor. Bright even overhead lighting, polished concrete floor with safety markings, organised tooling, strong depth and leading lines.${val(v, "setting") ? ` Scene notes: ${val(v, "setting")}.` : ""}`,
  },
  {
    id: "process-closeup",
    title: { zh: "工艺细节特写", en: "Process close-up" },
    hint: {
      zh: "微距展示加工与工艺品质",
      en: "Macro of machining or welding quality",
    },
    fields: [F_PRODUCT, F_PROCESS],
    framing: { size: "1536x1024", aspectRatio: "4:3" },
    build: (v) =>
      `Macro close-up photograph of the ${val(v, "process") || "precision machining"} process on ${subject(v)}: shallow depth of field, crisp metal texture, realistic sparks or coolant spray where appropriate, dramatic side lighting, industrial quality-control feel.`,
  },
  {
    id: "application-scene",
    title: { zh: "应用场景", en: "On-site application" },
    hint: {
      zh: "设备在客户现场工作的样子",
      en: "Machine working in a customer setting",
    },
    fields: [F_PRODUCT, F_APPEARANCE, F_INDUSTRY],
    framing: { size: "1536x1024", aspectRatio: "16:9" },
    build: (v) =>
      `Documentary-style medium-wide photograph of ${subject(v)} operating on site in a ${val(v, "industryUse") || "factory"} environment, visibly doing real work on its workpiece. Natural working light, believable surroundings, the machine clearly the main subject.`,
  },
  {
    id: "packing-shipping",
    title: { zh: "装箱发运", en: "Packing & shipping" },
    hint: {
      zh: "出口包装与装柜场景",
      en: "Export packing and container loading",
    },
    fields: [F_PRODUCT, F_PACKING],
    framing: { size: "1536x1024", aspectRatio: "4:3" },
    build: (v) =>
      `Export packing and loading scene: ${subject(v)} secured and protected for sea freight (${val(v, "packing") || "wooden crate, rust-proof film, steel strapping"}), a forklift loading it into a 40-foot shipping container at a port-side yard in daylight. Orderly, professional logistics atmosphere.`,
  },
  {
    id: "exhibition-poster",
    title: { zh: "展会海报底图", en: "Trade-show poster base" },
    hint: {
      zh: "留白版面，标题后期自行添加",
      en: "Empty headline area; add text later",
    },
    fields: [F_PRODUCT, F_APPEARANCE],
    framing: { size: "1024x1536", aspectRatio: "3:4" },
    build: (v) =>
      `Poster background image for a trade-show invitation: ${subject(v)} as the hero on a bold deep-blue to charcoal gradient stage with rim lighting and soft light beams, subtle reflective floor, and a large clean empty area in the upper third reserved for a headline added later. Do not render any text.`,
  },
];

export interface StudioImageTemplateView {
  id: string;
  title: string;
  hint: string;
  fields: StudioField[];
  framing: { size: string; aspectRatio: string };
}

export function getStudioImageTemplates(
  language: We2aiLanguage,
): StudioImageTemplateView[] {
  return IMAGE_TEMPLATES.map((t) => ({
    id: t.id,
    title: t.title[language],
    hint: t.hint[language],
    fields: localizeFields(t.fields, language),
    framing: t.framing,
  }));
}

/** 拼装图片提示词（英文）：模板正文 + 全局约束 + 参考图条款。 */
export function buildImagePrompt(
  templateId: string,
  values: StudioValues,
  opts: { hasReference: boolean },
): string {
  const template = IMAGE_TEMPLATES.find((t) => t.id === templateId);
  if (!template) return "";
  const parts = [template.build(values), IMAGE_GLOBAL_EN];
  if (opts.hasReference) parts.push(IMAGE_REFERENCE_EN);
  return parts.join(" ");
}

/** 把模板偏好的构图落到模型能力表上：不在表里就取表里第一个，表为空则不下发。 */
export function pickFraming(
  framing: { size: string; aspectRatio: string },
  caps: { sizes: string[]; aspectRatios: string[] },
): { size?: string; aspectRatio?: string } {
  const out: { size?: string; aspectRatio?: string } = {};
  if (caps.sizes.length > 0) {
    out.size = caps.sizes.includes(framing.size) ? framing.size : caps.sizes[0];
  }
  if (caps.aspectRatios.length > 0) {
    out.aspectRatio = caps.aspectRatios.includes(framing.aspectRatio)
      ? framing.aspectRatio
      : caps.aspectRatios[0];
  }
  return out;
}

// ---------------------------------------------------------------------------
// 视频模板
// ---------------------------------------------------------------------------

type BiPrompt = Record<StudioPromptLanguage, string>;

/** 全局固定参数（对应 shopUniversalGlobalParams，去掉门店特有的 POI / 团购条目）。 */
const VIDEO_GLOBAL: BiPrompt = {
  zh: "写实工业纪实质感，画面稳定、镜头平滑；画面内全程禁止出现任何字幕、水印、Logo、品牌标识、认证标志、文字贴纸、价格牌和屏幕可读文字；不出现可辨认的人脸，人物只用背影、侧影或手部特写，并佩戴安全帽、护目镜等防护装备；设备结构合理，不凭空增加描述中没有的部件。",
  en: "Realistic industrial documentary look, steady and smooth camera. No subtitles, watermarks, logos, brand marks, certification marks, text stickers, price tags or readable on-screen text at any time. No identifiable faces: people appear only from behind, in profile or as hand close-ups, wearing helmets and safety glasses. Mechanically plausible equipment; do not add parts that are not described.",
};

/** 负面清单：只用输入里给出的事实（对应 negativePrompt 的「禁止编造」）。 */
const VIDEO_NEGATIVE: BiPrompt = {
  zh: "只使用输入中给出的产品名称、型号和特征，不得推测或补写参数、产能、认证、客户名称；未提供的数字和标志一律不出现在画面中。",
  en: "Use only the product name, model and features given; never invent specifications, capacity, certifications or customer names; no numbers or marks that were not provided may appear in the frame.",
};

const VIDEO_REFERENCE: BiPrompt = {
  zh: "以参考图作为第一帧和产品依据，保持设备外形、颜色和结构一致，只让镜头与环境发生变化。",
  en: "Use the reference image as the first frame and as the product reference; keep the machine's shape, color and structure unchanged, and animate only the camera and surroundings.",
};

const VIDEO_STYLE: BiPrompt = {
  zh: "冷灰与金属质感为主，自然光与车间灯光结合",
  en: "cool grey and metallic tones, natural light mixed with workshop lighting",
};

const VIDEO_FALLBACKS: Record<string, BiPrompt> = {
  process: { zh: "核心加工", en: "core machining" },
  industryUse: { zh: "客户现场", en: "customer site" },
  setting: { zh: "整洁的生产车间", en: "a clean production workshop" },
  packing: { zh: "木箱包装", en: "wooden crate packing" },
};

export interface StudioVideoTemplate {
  id: string;
  title: Bi;
  hint: Bi;
  fields: StudioFieldDef[];
  /** 五段式分镜（每段含固定运镜）；`{product}`、`{process}` 等占位在拼装时替换。 */
  shots: BiPrompt[];
}

const VIDEO_TEMPLATES: StudioVideoTemplate[] = [
  {
    id: "factory-strength",
    title: { zh: "工厂实力展示", en: "Factory capability" },
    hint: {
      zh: "厂区 → 车间 → 核心设备 → 质检 → 装柜",
      en: "Plant to workshop to QC to loading",
    },
    fields: [F_PRODUCT, F_APPEARANCE, F_PROCESS, F_SETTING],
    shots: [
      {
        zh: "航拍缓慢推进的现代化机械制造厂区外景，厂房整洁，晴朗天空，货车缓缓进出，运镜为航拍缓推。",
        en: "A slow aerial push-in over a modern machinery plant, tidy buildings, clear sky, trucks moving in and out.",
      },
      {
        zh: "{setting}，{product}与同类设备整齐排列，天车缓慢移动，工人以背影作业，运镜为中景横向平稳平移。",
        en: "{setting}, {product} and similar machines in orderly rows, an overhead crane moving slowly, workers seen from behind; steady medium lateral pan.",
      },
      {
        zh: "{product}的核心加工特写，{process}，火花与金属屑真实可信，运镜为微距缓慢推进。",
        en: "Close-up of {product} in operation, {process}, believable sparks and chips; slow macro push-in.",
      },
      {
        zh: "质检区内，戴手套的手使用卡尺或三坐标测量仪检测零件，仪器屏幕不出现可读文字，运镜为固定机位静止拍摄。",
        en: "In the QC area, gloved hands measure a part with calipers or a CMM, screens show no readable text; locked-off static shot.",
      },
      {
        zh: "成品整齐装箱后叉车装入集装箱，收尾为全景缓慢拉远并定格。",
        en: "Finished machines crated and loaded into a container by forklift; closing wide shot pulling back slowly and holding.",
      },
    ],
  },
  {
    id: "product-demo",
    title: { zh: "设备功能演示", en: "Product demo" },
    hint: {
      zh: "外观 → 关键部件 → 运行 → 成品 → 整机定格",
      en: "Hero to components to running to output",
    },
    fields: [F_PRODUCT, F_APPEARANCE, F_FEATURES, F_PROCESS],
    shots: [
      {
        zh: "{product}静置于洁净的浅灰背景前，灯光扫过机身，运镜为缓慢环绕半圈。",
        en: "{product} standing against a clean light-grey backdrop while light sweeps across the body; slow half-orbit.",
      },
      {
        zh: "{product}的关键部件近景（主轴、导轨、控制面板等），金属质感清晰，面板不出现可读文字，运镜为微距缓慢推进。",
        en: "Close-up of {product}'s key components (spindle, guideways, control panel), crisp metal texture, panel without readable text; slow macro push-in.",
      },
      {
        zh: "{product}正在运行，{process}，动作连贯、节奏稳定，运镜为中景固定机位。",
        en: "{product} running, {process}, smooth and steady motion; static medium shot.",
      },
      {
        zh: "加工完成的工件被手取出并展示，表面光洁，运镜为手部特写后轻微拉出。",
        en: "A finished workpiece lifted out by a hand and shown, clean surface finish; hand close-up easing out slightly.",
      },
      {
        zh: "{product}整机回到洁净背景前，收尾为全景缓慢拉远并定格。",
        en: "The whole {product} back against the clean backdrop; closing wide shot pulling back slowly and holding.",
      },
    ],
  },
  {
    id: "process-craft",
    title: { zh: "工艺品质", en: "Craft & quality" },
    hint: {
      zh: "原料 → 加工 → 装配 → 检测 → 成品",
      en: "Material to machining to assembly to test",
    },
    fields: [F_PRODUCT, F_APPEARANCE, F_PROCESS],
    shots: [
      {
        zh: "整齐堆放的金属原材料与板材，叉车缓慢运送，运镜为低机位缓慢推进。",
        en: "Neatly stacked metal stock and plates, a forklift moving them slowly; low-angle slow push-in.",
      },
      {
        zh: "{process}，刀具与工件接触处的细节，冷却液与金属屑真实可信，运镜为微距推拉。",
        en: "{process}, detail where tool meets workpiece, believable coolant and chips; macro push-pull.",
      },
      {
        zh: "装配工位上，手部将{product}的部件装配到位，工具与夹具整洁，运镜为中景横向平移。",
        en: "At an assembly station, hands fit {product}'s components into place, tools and fixtures tidy; medium lateral pan.",
      },
      {
        zh: "整机试运行与检测，仪表与指示灯正常，屏幕不出现可读文字，运镜为固定机位静止拍摄。",
        en: "Test run and inspection of the complete machine, gauges and indicator lights normal, no readable screen text; static locked-off shot.",
      },
      {
        zh: "喷涂完成、表面一致的成品{product}在检验区整齐排列，收尾为全景缓慢拉远并定格。",
        en: "Finished, evenly painted {product} units lined up in the inspection area; closing wide shot pulling back slowly and holding.",
      },
    ],
  },
  {
    id: "shipping-delivery",
    title: { zh: "装柜发运", en: "Packing & shipping" },
    hint: {
      zh: "入库 → 包装 → 装柜 → 封柜 → 港口",
      en: "Warehouse to crating to container to port",
    },
    fields: [F_PRODUCT, F_PACKING],
    shots: [
      {
        zh: "成品仓库内，{product}整齐待发，顶灯明亮，运镜为中景缓慢推进。",
        en: "Inside the finished-goods warehouse, {product} ready to ship under bright ceiling lights; slow medium push-in.",
      },
      {
        zh: "{packing}，工人以背影作业，包装细节清晰，运镜为近景横向平移。",
        en: "{packing}, workers seen from behind, packing detail clear; close lateral pan.",
      },
      {
        zh: "叉车将包装好的{product}平稳装入集装箱，运镜为中景固定机位。",
        en: "A forklift loading the packed {product} smoothly into a container; static medium shot.",
      },
      {
        zh: "集装箱门关闭并加封，手部锁紧箱门杠，运镜为特写。",
        en: "Container doors closed and sealed, a hand locking the door bar; close-up.",
      },
      {
        zh: "港口堆场的集装箱与龙门吊，收尾为全景缓慢拉远并定格。",
        en: "Containers and gantry cranes at a port yard; closing wide shot pulling back slowly and holding.",
      },
    ],
  },
  {
    id: "exhibition-invite",
    title: { zh: "展会邀请", en: "Trade-show invitation" },
    hint: {
      zh: "展品 → 展厅 → 洽谈 → 演示 → 展台",
      en: "Exhibit to hall to meeting to demo",
    },
    fields: [F_PRODUCT, F_APPEARANCE, F_FEATURES],
    shots: [
      {
        zh: "聚光灯下的{product}样机，灯光扫过机身，不出现任何展牌文字，运镜为缓慢环绕。",
        en: "A {product} exhibit under spotlights, light sweeping the body, no signage text visible; slow orbit.",
      },
      {
        zh: "大型工业展厅的整体氛围，参观者以背影走动，远处灯光与展台，运镜为跟随式缓慢前进。",
        en: "The atmosphere of a large industrial exhibition hall, visitors walking away from camera, booths and lights in the distance; slow follow-forward move.",
      },
      {
        zh: "展台洽谈桌旁，手部翻看产品资料并握手，不出现可读文字，运镜为特写后轻微拉出。",
        en: "At a booth meeting table, hands paging through brochures and shaking hands, no readable text; close-up easing out.",
      },
      {
        zh: "{product}样机现场演示，动作清晰，围观者以背影站立，运镜为中景固定机位。",
        en: "Live demonstration of the {product} exhibit, clear motion, onlookers seen from behind; static medium shot.",
      },
      {
        zh: "整个展台的全景，灯光明亮、整洁大气，收尾为缓慢拉远并定格。",
        en: "Wide view of the whole booth, bright, tidy and imposing; closing slow pull-back that holds.",
      },
    ],
  },
];

export interface StudioVideoTemplateView {
  id: string;
  title: string;
  hint: string;
  fields: StudioField[];
}

export function getStudioVideoTemplates(
  language: We2aiLanguage,
): StudioVideoTemplateView[] {
  return VIDEO_TEMPLATES.map((t) => ({
    id: t.id,
    title: t.title[language],
    hint: t.hint[language],
    fields: localizeFields(t.fields, language),
  }));
}

function fillTokens(
  text: string,
  values: StudioValues,
  lang: StudioPromptLanguage,
): string {
  const productName = val(values, "productName");
  const productRef = productName
    ? `[${productName}]`
    : lang === "zh"
      ? "[产品名称]"
      : "[product name]";
  return text.replace(/\{(\w+)\}/g, (_, key: string) => {
    if (key === "product") return productRef;
    const given = val(values, key);
    if (given) return given;
    return VIDEO_FALLBACKS[key]?.[lang] ?? "";
  });
}

/** 时长不足 10 秒压缩为三镜（开场、核心、收尾），否则五镜。 */
function pickShots(shots: BiPrompt[], duration: number): BiPrompt[] {
  return duration >= 10 ? shots : [shots[0], shots[2], shots[4]];
}

function timeRange(
  index: number,
  count: number,
  duration: number,
  lang: StudioPromptLanguage,
): string {
  const start = Math.round((index * duration) / count);
  const end = Math.round(((index + 1) * duration) / count);
  return lang === "zh" ? `${start}-${end}秒` : `${start}-${end}s`;
}

/** 拼装视频提示词：风格 → 产品锁定 → 全局参数 → 分镜 → 负面清单 → 参考图条款。 */
export function buildVideoPrompt(
  templateId: string,
  values: StudioValues,
  opts: { lang: StudioPromptLanguage; duration: number; hasReference: boolean },
): string {
  const template = VIDEO_TEMPLATES.find((t) => t.id === templateId);
  if (!template) return "";
  const { lang, duration, hasReference } = opts;
  const model = val(values, "model");
  const appearance = val(values, "appearance");
  const features = val(values, "features");
  const subjectLine =
    lang === "zh"
      ? `产品：${fillTokens("{product}", values, lang)}${model ? `（型号 [${model}]）` : ""}${appearance ? `；外观：${appearance}` : ""}${features ? `；核心特征：${features}` : ""}。`
      : `Product: ${fillTokens("{product}", values, lang)}${model ? ` (model [${model}])` : ""}${appearance ? `; appearance: ${appearance}` : ""}${features ? `; key features: ${features}` : ""}.`;
  const shots = pickShots(template.shots, duration);
  const shotLines = shots.map((shot, i) => {
    const range = timeRange(i, shots.length, duration, lang);
    const label =
      lang === "zh" ? `镜头${i + 1}（${range}）` : `Shot ${i + 1} (${range})`;
    return `${label}：${fillTokens(shot[lang], values, lang)}`;
  });
  const head =
    lang === "zh"
      ? `${duration}秒工业宣传短片，${VIDEO_STYLE.zh}。`
      : `A ${duration}-second industrial promo film, ${VIDEO_STYLE.en}.`;
  const lines = [
    head,
    subjectLine,
    ...shotLines,
    VIDEO_GLOBAL[lang],
    VIDEO_NEGATIVE[lang],
  ];
  if (hasReference) lines.push(VIDEO_REFERENCE[lang]);
  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// AI 提示词编译（可选）：把模板草稿 + 字段交给文本模型润色，借鉴参考项目的
// videoPromptAssistSystemPrompt，改为机械制造与出口语境。
// ---------------------------------------------------------------------------

const COMPILE_VIDEO: BiPrompt = {
  zh: [
    "你是一名专业的 AI 视频生成提示词编译专家，服务机械制造与出口企业。请把用户给出的产品信息和模板草稿改写成一条可直接用于视频生成模型的提示词。",
    "规则：",
    "1. 用户明确给出的产品名称、型号、参数、企业信息必须原样保留，并用英文半角中括号 [] 括住，不得改写、遗漏或补充；用户没给的信息不要编造，需要时用描述性占位，如 [产品名称]。",
    "2. 画面内不出现任何文字、字幕、Logo、水印、认证标志；人物只用背影、侧影或手部特写。",
    "3. 设备外形、颜色和结构以用户描述为准，不凭空增加部件。",
    "4. 保留草稿的分镜时间结构，按「画面主体、动作变化、运镜方式、节奏与氛围、结尾收束」连贯表达，运镜具体可执行。",
    "5. 只输出最终提示词，不要解释、标题或 Markdown；长度控制在 450 个汉字以内。",
  ].join("\n"),
  en: [
    "You are an expert prompt compiler for AI video generation, serving machinery manufacturers and exporters. Rewrite the product information and template draft the user gives into one prompt ready for a video generation model.",
    "Rules:",
    "1. Keep the product name, model, specifications and company details the user gave exactly as written and wrap each in square brackets [ ]; never rewrite, drop or add to them. Do not invent anything the user did not give; use a descriptive placeholder such as [product name] when needed.",
    "2. No text, subtitles, logos, watermarks or certification marks in frame; people only from behind, in profile or as hand close-ups.",
    "3. The machine's shape, color and structure follow the user's description; add no parts.",
    "4. Keep the draft's shot-and-time structure and express it coherently across subject, action, camera move, pacing and closing; camera moves must be concrete and executable.",
    "5. Output only the final prompt, with no explanation, heading or Markdown, within 220 words.",
  ].join("\n"),
};

const COMPILE_IMAGE: BiPrompt = {
  zh: "",
  en: [
    "You are an expert prompt writer for AI image generation, serving machinery manufacturers and exporters. Rewrite the product information and template draft the user gives into one English prompt ready for an image generation model.",
    "Rules:",
    "1. Keep the product name, model and features the user gave exactly as written; never invent specifications, brands or certifications.",
    "2. No text, letters, numbers, logos, watermarks or certification marks in the image; no identifiable human faces.",
    "3. Keep the machine's shape, color and structure as described; add no parts. If the draft mentions a reference image, keep that clause.",
    "4. Describe subject, camera angle, lighting, background and material finish concretely.",
    "5. Output only the final prompt, with no explanation, heading or Markdown, within 130 words.",
  ].join("\n"),
};

/** AI 编译的 system 提示词。图片提示词一律英文。 */
export function buildCompileSystem(
  kind: "image" | "video",
  lang: StudioPromptLanguage,
): string {
  return kind === "image" ? COMPILE_IMAGE.en : COMPILE_VIDEO[lang];
}

/** 视频提示词语言：Seedance 用中文，其余英文。 */
export function videoPromptLanguage(family: string): StudioPromptLanguage {
  return family === "seedance" ? "zh" : "en";
}

/** Grok 视频提示词上限（字节），与 Rust `MEDIA_GROK_MAX_PROMPT_BYTES` 一致。 */
export const GROK_VIDEO_PROMPT_MAX_BYTES = 4096;
/** 通用提示词上限（字符），与 Rust `MEDIA_MAX_PROMPT_CHARS` 一致。 */
export const MEDIA_PROMPT_MAX_CHARS = 4000;
