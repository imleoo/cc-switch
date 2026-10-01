import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  API_KEY_ENV,
  KEY_PLACEHOLDER,
  SAMPLE_LANGS,
  SAMPLE_PROTOCOLS,
  renderSample,
  sampleBaseUrl,
  type KeyExpr,
  type SampleLang,
  type SampleProtocol,
} from "@/we2ai/codeSamples";

const BASE = "https://api.we2ai.com";
const MODEL = "claude-sonnet-4-5";
const ENV: KeyExpr = { kind: "env" };

function render(
  lang: SampleLang,
  protocol: SampleProtocol,
  patch: Partial<{ baseUrl: string; model: string; keyExpr: KeyExpr }> = {},
): string {
  return renderSample(lang, protocol, {
    baseUrl: BASE,
    model: MODEL,
    keyExpr: ENV,
    ...patch,
  });
}

describe("renderSample snapshots (env placeholder)", () => {
  for (const lang of SAMPLE_LANGS) {
    for (const protocol of SAMPLE_PROTOCOLS) {
      it(`${lang} / ${protocol}`, () => {
        expect(render(lang, protocol)).toMatchSnapshot();
      });
    }
  }
});

describe("renderSample snapshots (literal mask)", () => {
  for (const lang of SAMPLE_LANGS) {
    for (const protocol of SAMPLE_PROTOCOLS) {
      it(`${lang} / ${protocol}`, () => {
        expect(
          render(lang, protocol, {
            keyExpr: { kind: "literal", value: "sk-a1b2…9f3e" },
          }),
        ).toMatchSnapshot();
      });
    }
  }
});

describe("renderSample protocol rules", () => {
  it("covers exactly 6 languages x 3 protocols", () => {
    expect(SAMPLE_LANGS).toEqual([
      "curl",
      "python",
      "node",
      "java",
      "go",
      "powershell",
    ]);
    expect(SAMPLE_PROTOCOLS).toHaveLength(3);
  });

  it("sampleBaseUrl: /v1 for OpenAI and Responses, bare base for Anthropic, trailing slashes trimmed", () => {
    expect(sampleBaseUrl("openai", BASE)).toBe(`${BASE}/v1`);
    expect(sampleBaseUrl("responses", BASE)).toBe(`${BASE}/v1`);
    expect(sampleBaseUrl("anthropic", BASE)).toBe(BASE);
    expect(sampleBaseUrl("openai", `${BASE}//`)).toBe(`${BASE}/v1`);
    expect(sampleBaseUrl("anthropic", `${BASE}/`)).toBe(BASE);
  });

  it("curl: native endpoint per protocol; Anthropic uses x-api-key + anthropic-version, others Bearer", () => {
    const openai = render("curl", "openai");
    expect(openai).toContain(`curl ${BASE}/v1/chat/completions`);
    expect(openai).toContain(`Authorization: Bearer $${API_KEY_ENV}`);
    expect(openai).not.toContain("x-api-key");

    const responses = render("curl", "responses");
    expect(responses).toContain(`curl ${BASE}/v1/responses`);
    expect(responses).toContain(`Authorization: Bearer $${API_KEY_ENV}`);
    expect(responses).toContain(`"input":"Hello"`);

    const anthropic = render("curl", "anthropic");
    expect(anthropic).toContain(`curl ${BASE}/v1/messages`);
    expect(anthropic).toContain(`x-api-key: $${API_KEY_ENV}`);
    expect(anthropic).toContain("anthropic-version: 2023-06-01");
    expect(anthropic).toContain(`"max_tokens":1024`);
    expect(anthropic).not.toContain("Authorization");
  });

  it("SDK samples: OpenAI and Responses base_url ends with /v1, Anthropic base_url is the bare base", () => {
    for (const lang of ["python", "node"] as const) {
      for (const protocol of ["openai", "responses"] as const) {
        expect(render(lang, protocol)).toContain(`"${BASE}/v1"`);
      }
      const anthropic = render(lang, "anthropic");
      expect(anthropic).toContain(`"${BASE}"`);
      expect(anthropic).not.toContain(`"${BASE}/v1"`);
    }
    expect(render("python", "openai")).toContain("from openai import OpenAI");
    expect(render("python", "responses")).toContain("client.responses.create(");
    expect(render("python", "anthropic")).toContain(
      "from anthropic import Anthropic",
    );
    expect(render("node", "openai")).toContain('from "openai"');
    expect(render("node", "anthropic")).toContain('from "@anthropic-ai/sdk"');
  });

  it("PowerShell: Invoke-RestMethod against the full endpoint; Bearer for OpenAI-style, x-api-key for Anthropic; UTF-8 body", () => {
    for (const [protocol, path] of [
      ["openai", "/v1/chat/completions"],
      ["responses", "/v1/responses"],
      ["anthropic", "/v1/messages"],
    ] as const) {
      const code = render("powershell", protocol);
      expect(code).toContain("Invoke-RestMethod");
      expect(code).toContain(`-Uri '${BASE}${path}'`);
      expect(code).toContain("GetBytes($body)");
      if (protocol === "anthropic") {
        expect(code).toContain(`"x-api-key" = $env:${API_KEY_ENV}`);
        expect(code).toContain(`"anthropic-version" = '2023-06-01'`);
        expect(code).not.toContain("Authorization");
      } else {
        expect(code).toContain(
          `"Authorization" = "Bearer $env:${API_KEY_ENV}"`,
        );
        expect(code).not.toContain("x-api-key");
      }
    }
  });

  it("Java / Go: full native endpoint, Bearer for OpenAI-style and x-api-key for Anthropic, no third-party imports", () => {
    for (const lang of ["java", "go"] as const) {
      expect(render(lang, "openai")).toContain(`${BASE}/v1/chat/completions`);
      expect(render(lang, "responses")).toContain(`${BASE}/v1/responses`);
      expect(render(lang, "anthropic")).toContain(`${BASE}/v1/messages`);
      expect(render(lang, "anthropic")).toContain("x-api-key");
      expect(render(lang, "anthropic")).toContain("2023-06-01");
      expect(render(lang, "openai")).toContain("Bearer");
      expect(render(lang, "openai")).not.toContain("x-api-key");
    }
    const java = render("java", "openai");
    expect(java).toContain("java.net.http.HttpClient");
    // 兼容 JDK 11：不使用文本块。
    expect(java).not.toContain('"""');
    expect(render("go", "openai")).toContain('"net/http"');
  });
});

describe("renderSample key expression modes", () => {
  it("env: every language reads WE2AI_API_KEY and never contains a literal key", () => {
    const expected: Record<SampleLang, string> = {
      curl: `$${API_KEY_ENV}`,
      python: `os.environ["${API_KEY_ENV}"]`,
      node: `process.env.${API_KEY_ENV}`,
      java: `System.getenv("${API_KEY_ENV}")`,
      go: `os.Getenv("${API_KEY_ENV}")`,
      powershell: `$env:${API_KEY_ENV}`,
    };
    for (const lang of SAMPLE_LANGS) {
      for (const protocol of SAMPLE_PROTOCOLS) {
        const code = render(lang, protocol);
        expect(code, `${lang}/${protocol}`).toContain(expected[lang]);
        expect(code).not.toContain(KEY_PLACEHOLDER);
      }
    }
    // Python 只在读环境变量时才 import os。
    expect(render("python", "openai")).toContain("import os\n");
    // Go 只在读环境变量时才 import os。
    expect(render("go", "openai")).toContain('\t"os"\n');
  });

  it("literal (mask): shows the mask as a string literal and drops the env reads", () => {
    const mask = "sk-a1b2…9f3e";
    for (const lang of SAMPLE_LANGS) {
      for (const protocol of SAMPLE_PROTOCOLS) {
        const code = render(lang, protocol, {
          keyExpr: { kind: "literal", value: mask },
        });
        expect(code, `${lang}/${protocol}`).toContain(mask);
        expect(code).not.toContain(API_KEY_ENV);
        expect(code).not.toContain(KEY_PLACEHOLDER);
      }
    }
    expect(
      render("python", "openai", { keyExpr: { kind: "literal", value: mask } }),
    ).not.toContain("import os");
    expect(
      render("go", "openai", { keyExpr: { kind: "literal", value: mask } }),
    ).not.toContain('"os"');
  });

  it("literal (replacement placeholder): appears in every place the key is used, unmodified by escaping", () => {
    const literal: KeyExpr = { kind: "literal", value: KEY_PLACEHOLDER };
    for (const lang of SAMPLE_LANGS) {
      for (const protocol of SAMPLE_PROTOCOLS) {
        const code = render(lang, protocol, { keyExpr: literal });
        expect(code, `${lang}/${protocol}`).toContain(KEY_PLACEHOLDER);
        // 占位串在所有语言里都不会被转义改写，所以 Rust 的全文替换必然命中；
        // 去掉占位串后不应再有任何环境变量读取（占位串自身含环境变量名）。
        expect(code.split(KEY_PLACEHOLDER).join("")).not.toContain(API_KEY_ENV);
      }
    }
    expect(render("curl", "openai", { keyExpr: literal })).toContain(
      `Authorization: Bearer ${KEY_PLACEHOLDER}`,
    );
    expect(render("curl", "anthropic", { keyExpr: literal })).toContain(
      `x-api-key: ${KEY_PLACEHOLDER}`,
    );
    // 占位串里没有会被任何语言转义的字符。
    expect(JSON.stringify(KEY_PLACEHOLDER)).toBe(`"${KEY_PLACEHOLDER}"`);
    expect(KEY_PLACEHOLDER).toMatch(/^[A-Za-z0-9_]+$/);
  });

  it("literal values with shell-special characters are escaped inside curl double quotes", () => {
    const code = render("curl", "openai", {
      keyExpr: { kind: "literal", value: 'a"b$c`d\\e' },
    });
    expect(code).toContain('Bearer a\\"b\\$c\\`d\\\\e"');
  });
});

describe("renderSample escaping", () => {
  const TRICKY = `we"ird\\model'name\`x`;

  /** 从 curl 的 `-d '...'` 取回 JSON 并解析（还原 `'\\''`）。 */
  function curlBody(code: string): { model: string } {
    const match = /-d ('(?:[^']|'\\'')*')\s*$/.exec(code);
    expect(match).not.toBeNull();
    const raw = match![1].slice(1, -1).replace(/'\\''/g, "'");
    return JSON.parse(raw);
  }

  it("curl: model with quotes, backslashes and single quotes round-trips through shell quoting and JSON", () => {
    for (const protocol of SAMPLE_PROTOCOLS) {
      expect(curlBody(render("curl", protocol, { model: TRICKY })).model).toBe(
        TRICKY,
      );
    }
  });

  it("powershell: single quotes (and curly quote variants) are doubled so the body round-trips", () => {
    const model = "it's\u2018x\u2019y";
    const code = render("powershell", "openai", { model });
    const match = /\$body = '((?:[^']|'')*)'/.exec(code);
    expect(match).not.toBeNull();
    const raw = match![1]
      .replace(/''/g, "'")
      .replace(/(['\u2018\u2019])\1/g, "$1");
    expect(JSON.parse(raw).model).toBe(model);
  });

  it("python / node: the model literal is a valid escaped string", () => {
    const quoted = JSON.stringify(TRICKY);
    for (const lang of ["python", "node"] as const) {
      for (const protocol of SAMPLE_PROTOCOLS) {
        expect(
          render(lang, protocol, { model: TRICKY }),
          `${lang}/${protocol}`,
        ).toContain(quoted);
      }
    }
  });

  it("java: concatenated literals rebuild the exact JSON body", () => {
    for (const protocol of SAMPLE_PROTOCOLS) {
      const code = render("java", protocol, { model: TRICKY });
      const bodyBlock = code.slice(
        code.indexOf("String body ="),
        code.indexOf("HttpRequest req"),
      );
      const literals = bodyBlock.match(/"(?:[^"\\]|\\.)*"/g) ?? [];
      const joined = literals.map((l) => JSON.parse(l) as string).join("");
      expect(JSON.parse(joined).model, protocol).toBe(TRICKY);
    }
  });

  it("go: raw string body when the model has no backtick, escaped string when it does", () => {
    const plain = render("go", "openai", { model: "m\"x'y" });
    const raw = /body := `([^`]*)`/.exec(plain);
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw![1]).model).toBe("m\"x'y");

    // TRICKY 含反引号：退回双引号转义字符串，仍能还原。
    const tricky = render("go", "openai", { model: TRICKY });
    const quoted = /body := ("(?:[^"\\]|\\.)*")/.exec(tricky);
    expect(quoted).not.toBeNull();
    expect(JSON.parse(JSON.parse(quoted![1])).model).toBe(TRICKY);
  });

  it("control characters in the model never produce a raw newline inside a string literal", () => {
    const model = "line1\nline2\u0001";
    for (const lang of SAMPLE_LANGS) {
      for (const protocol of SAMPLE_PROTOCOLS) {
        const code = render(lang, protocol, { model });
        expect(code, `${lang}/${protocol}`).not.toContain("line1\nline2");
        // Java 会先处理 \uXXXX：换行必须用短转义，不能出现 \u000a / \u000d。
        expect(code.toLowerCase()).not.toContain("\\u000a");
        expect(code.toLowerCase()).not.toContain("\\u000d");
      }
    }
  });

  it("a base URL with a trailing slash does not double up slashes in endpoints", () => {
    expect(render("curl", "openai", { baseUrl: `${BASE}/` })).toContain(
      `curl ${BASE}/v1/chat/completions`,
    );
    expect(render("go", "anthropic", { baseUrl: `${BASE}/` })).toContain(
      `"${BASE}/v1/messages"`,
    );
  });
});

/**
 * 供 `scripts/we2ai/check-samples.sh` 使用：设置 `WE2AI_SAMPLES_DIR` 时把全部组合
 * （环境变量 / 掩码字面量两种 Key 表达 × 含特殊字符的模型名）写成文件做语法检查；
 * 平时（包括 CI 的 `pnpm test:unit`）整段跳过。
 */
describe.skipIf(!process.env.WE2AI_SAMPLES_DIR)("dump samples", () => {
  it("dump", () => {
    const root = process.env.WE2AI_SAMPLES_DIR as string;
    const files: Record<SampleLang, string> = {
      curl: "sample.sh",
      python: "sample.py",
      node: "sample.mjs",
      java: "We2aiDemo.java",
      go: "main.go",
      powershell: "sample.ps1",
    };
    for (const lang of SAMPLE_LANGS) {
      for (const protocol of SAMPLE_PROTOCOLS) {
        for (const [mode, keyExpr] of [
          ["env", ENV],
          ["mask", { kind: "literal", value: "sk-a1b2…9f3e" }],
        ] as const) {
          const dir = join(root, `${lang}_${protocol}_${mode}`);
          mkdirSync(dir, { recursive: true });
          writeFileSync(
            join(dir, files[lang]),
            `${render(lang, protocol, { keyExpr, model: `we"ird'model` })}\n`,
          );
        }
      }
    }
  });
});
