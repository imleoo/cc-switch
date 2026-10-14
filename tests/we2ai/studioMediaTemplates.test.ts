import { describe, expect, it } from "vitest";
import {
  GROK_VIDEO_PROMPT_MAX_BYTES,
  MEDIA_PROMPT_MAX_CHARS,
  buildCompileSystem,
  buildImagePrompt,
  buildVideoPrompt,
  getStudioImageTemplates,
  getStudioVideoTemplates,
  missingRequiredFields,
  pickFraming,
  videoPromptLanguage,
} from "@/we2ai/studioMediaTemplates";
import {
  PLAYGROUND_OUTPUT_LANGUAGES,
  buildPlaygroundSystem,
  getWe2aiPlaygroundPresets,
} from "@/we2ai/playgroundPrompts";

const VALUES = {
  productName: "CNC machining center VMC-850",
  appearance: "grey-white body, blue guard doors",
  features: "rigid cast-iron bed",
  process: "high-speed milling",
  industryUse: "automotive parts",
  packing: "wooden crate",
  setting: "two rows of machines",
};

const utf8 = (s: string) => new TextEncoder().encode(s).length;

describe("图片模板", () => {
  const templates = getStudioImageTemplates("zh");

  it("覆盖机械出口的六个场景，且产品名必填", () => {
    expect(templates.map((t) => t.id)).toEqual([
      "hero-product",
      "factory-floor",
      "process-closeup",
      "application-scene",
      "packing-shipping",
      "exhibition-poster",
    ]);
    for (const tpl of templates) {
      const required = tpl.fields.filter((f) => f.required).map((f) => f.id);
      expect(required).toEqual(["productName"]);
    }
  });

  it.each(templates.map((t) => t.id))(
    "%s：嵌入用户字段、带全局约束、无残留占位",
    (id) => {
      const prompt = buildImagePrompt(id, VALUES, { hasReference: false });
      expect(prompt).toContain(VALUES.productName);
      expect(prompt).toContain("No text, letters, numbers, logos");
      expect(prompt).toContain("No identifiable human faces");
      expect(prompt).not.toMatch(/[{}]|undefined/);
      expect(prompt.length).toBeLessThanOrEqual(MEDIA_PROMPT_MAX_CHARS);
    },
  );

  it("只有带参考图时才追加保持产品外形的条款", () => {
    const without = buildImagePrompt("hero-product", VALUES, {
      hasReference: false,
    });
    const withRef = buildImagePrompt("hero-product", VALUES, {
      hasReference: true,
    });
    expect(without).not.toContain("reference image");
    expect(withRef).toContain("preserve its shape");
  });

  it("没填可选字段也能出完整提示词，不会编造外观或参数", () => {
    const prompt = buildImagePrompt(
      "hero-product",
      { productName: "Pump" },
      { hasReference: false },
    );
    expect(prompt).toContain("Pump");
    expect(prompt).not.toMatch(/Emphasise|\(\)/);
  });

  it("展会海报底图明确留白且不渲染文字", () => {
    const prompt = buildImagePrompt("exhibition-poster", VALUES, {
      hasReference: false,
    });
    expect(prompt).toContain("empty area");
    expect(prompt).toContain("Do not render any text");
  });

  it("模板正文不凭空带认证声明", () => {
    for (const tpl of templates) {
      const prompt = buildImagePrompt(
        tpl.id,
        { productName: "Pump" },
        { hasReference: false },
      );
      expect(prompt).not.toMatch(/\b(CE|ISO|UL|TUV)\b/);
    }
  });
});

describe("构图落到模型能力表", () => {
  it("在表里就用模板偏好，不在就取第一个，表为空则不下发", () => {
    const framing = { size: "1536x1024", aspectRatio: "16:9" };
    expect(
      pickFraming(framing, {
        sizes: ["1024x1024", "1536x1024"],
        aspectRatios: [],
      }),
    ).toEqual({ size: "1536x1024" });
    expect(
      pickFraming(framing, {
        sizes: ["1024x1024"],
        aspectRatios: ["1:1", "9:16"],
      }),
    ).toEqual({
      size: "1024x1024",
      aspectRatio: "1:1",
    });
    expect(pickFraming(framing, { sizes: [], aspectRatios: [] })).toEqual({});
  });
});

describe("视频模板", () => {
  const templates = getStudioVideoTemplates("zh");

  it("覆盖工厂实力、功能演示、工艺品质、装柜发运、展会邀请", () => {
    expect(templates.map((t) => t.id)).toEqual([
      "factory-strength",
      "product-demo",
      "process-craft",
      "shipping-delivery",
      "exhibition-invite",
    ]);
  });

  const cases = templates.flatMap((t) =>
    (["zh", "en"] as const).flatMap((lang) =>
      [5, 10, 15].map((d) => [t.id, lang, d] as const),
    ),
  );

  it.each(cases)(
    "%s / %s / %is：结构完整、无残留占位",
    (id, lang, duration) => {
      const prompt = buildVideoPrompt(id, VALUES, {
        lang,
        duration,
        hasReference: false,
      });
      const shotCount = (
        prompt.match(lang === "zh" ? /镜头\d（/g : /Shot \d \(/g) ?? []
      ).length;
      expect(shotCount).toBe(duration >= 10 ? 5 : 3);
      // 业务关键词用 [] 锁定。
      expect(prompt).toContain(`[${VALUES.productName}]`);
      expect(prompt).not.toMatch(/[{}]|undefined/);
      // 最后一镜的结束时间等于总时长，时间轴连续。
      const ends = [
        ...prompt.matchAll(lang === "zh" ? /-(\d+)秒）/g : /-(\d+)s\)/g),
      ].map((m) => Number(m[1]));
      expect(ends[ends.length - 1]).toBe(duration);
      expect([...ends].sort((a, b) => a - b)).toEqual(ends);
    },
  );

  it("画面禁止文字 / Logo / 认证标志与人脸，且只用输入里的事实", () => {
    const zh = buildVideoPrompt("factory-strength", VALUES, {
      lang: "zh",
      duration: 15,
      hasReference: false,
    });
    expect(zh).toContain("禁止出现任何字幕、水印、Logo");
    expect(zh).toContain("认证标志");
    expect(zh).toContain("不出现可辨认的人脸");
    expect(zh).toContain("不得推测或补写参数");
    const en = buildVideoPrompt("factory-strength", VALUES, {
      lang: "en",
      duration: 15,
      hasReference: false,
    });
    expect(en).toContain("No subtitles, watermarks, logos");
    expect(en).toContain("never invent specifications");
  });

  it("没填产品名时用占位而不是留空", () => {
    const zh = buildVideoPrompt(
      "product-demo",
      {},
      { lang: "zh", duration: 10, hasReference: false },
    );
    expect(zh).toContain("[产品名称]");
    const en = buildVideoPrompt(
      "product-demo",
      {},
      { lang: "en", duration: 10, hasReference: false },
    );
    expect(en).toContain("[product name]");
  });

  it("参考图条款只在带参考图时出现", () => {
    const base = { lang: "zh", duration: 10 } as const;
    expect(
      buildVideoPrompt("product-demo", VALUES, {
        ...base,
        hasReference: false,
      }),
    ).not.toContain("第一帧");
    expect(
      buildVideoPrompt("product-demo", VALUES, { ...base, hasReference: true }),
    ).toContain("第一帧");
  });

  it("Grok 英文提示词在字段写得很长时仍不超过 4096 字节", () => {
    const long = "x".repeat(150);
    const values = {
      productName: long,
      model: long,
      appearance: long,
      features: long,
      process: long,
      industryUse: long,
      packing: long,
      setting: long,
    };
    for (const tpl of templates) {
      const prompt = buildVideoPrompt(tpl.id, values, {
        lang: "en",
        duration: 15,
        hasReference: true,
      });
      expect(utf8(prompt), tpl.id).toBeLessThanOrEqual(
        GROK_VIDEO_PROMPT_MAX_BYTES,
      );
    }
  });

  it("常规输入下 Seedance 中文提示词也在通用字符上限内", () => {
    for (const tpl of templates) {
      const prompt = buildVideoPrompt(tpl.id, VALUES, {
        lang: "zh",
        duration: 15,
        hasReference: true,
      });
      expect(prompt.length, tpl.id).toBeLessThanOrEqual(MEDIA_PROMPT_MAX_CHARS);
    }
  });

  it("按模型族选择提示词语言", () => {
    expect(videoPromptLanguage("seedance")).toBe("zh");
    expect(videoPromptLanguage("grok")).toBe("en");
  });
});

describe("必填校验与 AI 编译提示词", () => {
  it("缺必填项时返回缺失字段", () => {
    const fields = getStudioImageTemplates("zh")[0].fields;
    expect(missingRequiredFields(fields, {}).map((f) => f.id)).toEqual([
      "productName",
    ]);
    expect(
      missingRequiredFields(fields, { productName: "  " }).map((f) => f.id),
    ).toEqual(["productName"]);
    expect(missingRequiredFields(fields, { productName: "Pump" })).toEqual([]);
  });

  it("编译提示词锁定业务关键词并禁止画面文字", () => {
    expect(buildCompileSystem("video", "zh")).toContain("中括号 []");
    expect(buildCompileSystem("video", "zh")).toContain("认证标志");
    expect(buildCompileSystem("video", "en")).toContain("square brackets");
    expect(buildCompileSystem("image", "zh")).toContain(
      "never invent specifications",
    );
  });
});

describe("文本预设与系统提示词", () => {
  const presets = getWe2aiPlaygroundPresets("zh");

  it("是外贸业务场景，并保留 product-copy 这个稳定 id", () => {
    expect(presets.map((p) => p.id)).toEqual([
      "inquiry-reply",
      "product-copy",
      "cold-email",
      "exhibition-invite",
      "selling-points",
      "social-post",
      "localization",
    ]);
    expect(getWe2aiPlaygroundPresets("en").map((p) => p.id)).toEqual(
      presets.map((p) => p.id),
    );
  });

  it("没选场景时不下发 system（自由对话保持原行为）", () => {
    expect(buildPlaygroundSystem(null, "en", "zh")).toBeUndefined();
  });

  it.each(["zh", "en"] as const)(
    "%s：每个场景都带硬约束，且不编造认证",
    (lang) => {
      for (const preset of getWe2aiPlaygroundPresets(lang)) {
        const system = buildPlaygroundSystem(preset, "es", lang)!;
        expect(system, preset.id).toContain(
          lang === "zh" ? "【硬性约束】" : "HARD RULES",
        );
        expect(system, preset.id).toMatch(
          lang === "zh" ? /不得编造认证/ : /Never invent certifications/,
        );
      }
    },
  );

  it("输出语言：普通场景追加，本地化场景不追加", () => {
    const byId = (id: string) => presets.find((p) => p.id === id)!;
    expect(buildPlaygroundSystem(byId("cold-email"), "es", "zh")).toContain(
      "Spanish",
    );
    expect(
      buildPlaygroundSystem(byId("cold-email"), "unknown", "zh"),
    ).toContain("English");
    const localization = buildPlaygroundSystem(
      byId("localization"),
      "es",
      "zh",
    )!;
    expect(localization).not.toContain("输出语言");
    expect(localization).not.toContain("Spanish");
  });

  it("输出语言默认英语，覆盖主要出口市场", () => {
    expect(PLAYGROUND_OUTPUT_LANGUAGES[0].id).toBe("en");
    expect(PLAYGROUND_OUTPUT_LANGUAGES.map((l) => l.id)).toEqual(
      expect.arrayContaining(["es", "ru", "ar", "fr", "pt", "de"]),
    );
  });
});
