/**
 * 调用示例模板（功能 22）：6 种语言（curl / Python / Node / Java / Go / PowerShell）× 3 协议 = 18 个纯函数渲染结果。
 *
 * **明文边界**：这里只处理「Key 表达式」，从不接触真实 Key。调用方决定
 * `keyExpr`：
 * - `{ kind: "env" }`：读环境变量 `WE2AI_API_KEY`（默认，代码框永不出现 Key）；
 * - `{ kind: "literal", value }`：把 `value` 当字符串字面量写进代码——显示用掩码，
 *   复制用「待替换串」，真实 Key 由 Rust 在写剪贴板时替换（`we2ai_copy_text_with_key`）。
 *
 * base 规则与 `apply.rs` 写入工具一致：OpenAI 兼容 / Responses 用 `{base}/v1`，
 * Anthropic 用 `{base}`；SDK 示例传 SDK 自己会再拼路径的 base，原生 HTTP 示例用完整端点。
 */

export type SampleLang =
  | "curl"
  | "python"
  | "node"
  | "java"
  | "go"
  | "powershell";
export type SampleProtocol = "openai" | "anthropic" | "responses";

export const SAMPLE_LANGS: readonly SampleLang[] = [
  "curl",
  "python",
  "node",
  "java",
  "go",
  "powershell",
];
export const SAMPLE_PROTOCOLS: readonly SampleProtocol[] = [
  "openai",
  "anthropic",
  "responses",
];

/** 环境变量名。示例、提示文案与测试共用同一个字面量。 */
export const API_KEY_ENV = "WE2AI_API_KEY";

/**
 * 「待替换」占位串：不会与示例代码里的其它内容冲突，Rust 侧据此替换为真实 Key。
 * 与 `key_manage.rs::SAMPLE_KEY_PLACEHOLDER` 各写一份字面量，`check-guards.sh` 4.16 校验一致；
 * 命令 `we2ai_copy_text_with_key` 不再接收占位串参数，Rust 只认自己的常量。
 */
export const KEY_PLACEHOLDER = "__WE2AI_API_KEY__";

export type KeyExpr = { kind: "env" } | { kind: "literal"; value: string };

export interface SampleParams {
  /** 网关根地址（`we2ai_gateway_info` 的 `baseUrl`，不含 `/v1`）。 */
  baseUrl: string;
  model: string;
  keyExpr: KeyExpr;
}

const USER_MESSAGE = "Hello";
const ANTHROPIC_VERSION = "2023-06-01";
const ANTHROPIC_MAX_TOKENS = 1024;

function trimBase(baseUrl: string): string {
  return baseUrl.replace(/\/+$/, "");
}

/** 抽屉里展示/复制的 Base URL：Anthropic 用根地址，其余带 `/v1`。 */
export function sampleBaseUrl(
  protocol: SampleProtocol,
  baseUrl: string,
): string {
  const base = trimBase(baseUrl);
  return protocol === "anthropic" ? base : `${base}/v1`;
}

function endpointPath(protocol: SampleProtocol): string {
  switch (protocol) {
    case "openai":
      return "/v1/chat/completions";
    case "anthropic":
      return "/v1/messages";
    case "responses":
      return "/v1/responses";
  }
}

function endpointUrl(protocol: SampleProtocol, baseUrl: string): string {
  return `${trimBase(baseUrl)}${endpointPath(protocol)}`;
}

/**
 * 双引号字符串字面量。`JSON.stringify` 产出的转义（`\"` `\\` `\n` `\uXXXX`）在
 * Python / JS / Java / Go 里都是合法的；它不会产出 `\u000a` 这类会被 Java 预处理
 * 成真换行的转义（控制字符用 `\n` `\r` 等短转义）。
 */
function q(value: string): string {
  return JSON.stringify(value);
}

/** 代码里的 Key 表达式：环境变量用语言自己的读法，字面量走字符串转义。 */
function keyValue(keyExpr: KeyExpr, envExpr: string): string {
  return keyExpr.kind === "env" ? envExpr : q(keyExpr.value);
}

/** 请求体里的 JSON 片段（保持单行、最小化）。 */
function jsonBody(protocol: SampleProtocol, model: string): string {
  const message = `[{"role":"user","content":${q(USER_MESSAGE)}}]`;
  switch (protocol) {
    case "openai":
      return `{"model":${q(model)},"messages":${message}}`;
    case "anthropic":
      return `{"model":${q(model)},"max_tokens":${ANTHROPIC_MAX_TOKENS},"messages":${message}}`;
    case "responses":
      return `{"model":${q(model)},"input":${q(USER_MESSAGE)}}`;
  }
}

// ---------------------------------------------------------------------------
// Shell 转义
// ---------------------------------------------------------------------------

function shellSingle(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

/** 双引号内需要转义的字符：反斜杠、双引号、`$`、反引号。 */
function shellDoubleInner(value: string): string {
  return value.replace(/[\\"$`]/g, "\\$&");
}

/** URL 只含安全字符时原样输出，否则加单引号。 */
function shellWord(value: string): string {
  return /^[A-Za-z0-9:/._~%@+=,-]+$/.test(value) ? value : shellSingle(value);
}

// ---------------------------------------------------------------------------
// curl
// ---------------------------------------------------------------------------

function curlKey(keyExpr: KeyExpr): string {
  return keyExpr.kind === "env"
    ? `$${API_KEY_ENV}`
    : shellDoubleInner(keyExpr.value);
}

function renderCurl(protocol: SampleProtocol, p: SampleParams): string {
  const key = curlKey(p.keyExpr);
  const lines = [`curl ${shellWord(endpointUrl(protocol, p.baseUrl))}`];
  if (protocol === "anthropic") {
    lines.push(`-H "x-api-key: ${key}"`);
    lines.push(`-H "anthropic-version: ${ANTHROPIC_VERSION}"`);
  } else {
    lines.push(`-H "Authorization: Bearer ${key}"`);
  }
  lines.push(`-H "Content-Type: application/json"`);
  lines.push(`-d ${shellSingle(jsonBody(protocol, p.model))}`);
  return lines.map((l, i) => (i === 0 ? l : `  ${l}`)).join(" \\\n");
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

function renderPython(protocol: SampleProtocol, p: SampleParams): string {
  const env = p.keyExpr.kind === "env";
  const key = keyValue(p.keyExpr, `os.environ[${q(API_KEY_ENV)}]`);
  const sdkBase = q(sampleBaseUrl(protocol, p.baseUrl));
  const imports: string[] = [];
  if (env) imports.push("import os");

  if (protocol === "anthropic") {
    return [
      ...imports,
      "from anthropic import Anthropic",
      "",
      `client = Anthropic(base_url=${sdkBase}, api_key=${key})`,
      "msg = client.messages.create(",
      `    model=${q(p.model)},`,
      `    max_tokens=${ANTHROPIC_MAX_TOKENS},`,
      `    messages=[{"role": "user", "content": ${q(USER_MESSAGE)}}],`,
      ")",
      "print(msg.content[0].text)",
    ].join("\n");
  }

  const head = [
    ...imports,
    "from openai import OpenAI",
    "",
    `client = OpenAI(base_url=${sdkBase}, api_key=${key})`,
  ];
  if (protocol === "responses") {
    return [
      ...head,
      "resp = client.responses.create(",
      `    model=${q(p.model)},`,
      `    input=${q(USER_MESSAGE)},`,
      ")",
      "print(resp.output_text)",
    ].join("\n");
  }
  return [
    ...head,
    "resp = client.chat.completions.create(",
    `    model=${q(p.model)},`,
    `    messages=[{"role": "user", "content": ${q(USER_MESSAGE)}}],`,
    ")",
    "print(resp.choices[0].message.content)",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Node.js（ESM，顶层 await）
// ---------------------------------------------------------------------------

function renderNode(protocol: SampleProtocol, p: SampleParams): string {
  const key = keyValue(p.keyExpr, `process.env.${API_KEY_ENV}`);
  const sdkBase = q(sampleBaseUrl(protocol, p.baseUrl));
  const message = `[{ role: "user", content: ${q(USER_MESSAGE)} }]`;

  if (protocol === "anthropic") {
    return [
      'import Anthropic from "@anthropic-ai/sdk";',
      "",
      "const client = new Anthropic({",
      `  baseURL: ${sdkBase},`,
      `  apiKey: ${key},`,
      "});",
      "",
      "const msg = await client.messages.create({",
      `  model: ${q(p.model)},`,
      `  max_tokens: ${ANTHROPIC_MAX_TOKENS},`,
      `  messages: ${message},`,
      "});",
      "console.log(msg.content[0].text);",
    ].join("\n");
  }

  const head = [
    'import OpenAI from "openai";',
    "",
    "const client = new OpenAI({",
    `  baseURL: ${sdkBase},`,
    `  apiKey: ${key},`,
    "});",
    "",
  ];
  if (protocol === "responses") {
    return [
      ...head,
      "const resp = await client.responses.create({",
      `  model: ${q(p.model)},`,
      `  input: ${q(USER_MESSAGE)},`,
      "});",
      "console.log(resp.output_text);",
    ].join("\n");
  }
  return [
    ...head,
    "const resp = await client.chat.completions.create({",
    `  model: ${q(p.model)},`,
    `  messages: ${message},`,
    "});",
    "console.log(resp.choices[0].message.content);",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Java（java.net.http，JDK 11+；不用文本块，普通字符串拼接以兼容 JDK 11）
// ---------------------------------------------------------------------------

/** 把 JSON 请求体拆成若干 Java 字符串字面量拼接，保证 JDK 11 可编译。 */
function javaBody(protocol: SampleProtocol, model: string): string {
  const message = `"messages":[{"role":"user","content":${q(USER_MESSAGE)}}]`;
  const parts: string[] = [];
  parts.push("{");
  parts.push(`"model":${q(model)},`);
  switch (protocol) {
    case "openai":
      parts.push(message);
      break;
    case "anthropic":
      parts.push(`"max_tokens":${ANTHROPIC_MAX_TOKENS},`);
      parts.push(message);
      break;
    case "responses":
      parts.push(`"input":${q(USER_MESSAGE)}`);
      break;
  }
  parts.push("}");
  return parts
    .map((part, i) => `${i === 0 ? "" : "    + "}${q(part)}`)
    .join("\n");
}

function renderJava(protocol: SampleProtocol, p: SampleParams): string {
  const key = keyValue(p.keyExpr, `System.getenv(${q(API_KEY_ENV)})`);
  const authHeaders =
    protocol === "anthropic"
      ? [
          `            .header("x-api-key", apiKey)`,
          `            .header("anthropic-version", ${q(ANTHROPIC_VERSION)})`,
        ]
      : [`            .header("Authorization", "Bearer " + apiKey)`];
  const body = javaBody(protocol, p.model)
    .split("\n")
    .map((line, i) => (i === 0 ? line : `        ${line}`))
    .join("\n");
  return [
    "import java.net.URI;",
    "import java.net.http.HttpClient;",
    "import java.net.http.HttpRequest;",
    "import java.net.http.HttpResponse;",
    "",
    "public class We2aiDemo {",
    "    public static void main(String[] args) throws Exception {",
    `        String apiKey = ${key};`,
    `        String body = ${body};`,
    "",
    `        HttpRequest req = HttpRequest.newBuilder(URI.create(${q(endpointUrl(protocol, p.baseUrl))}))`,
    ...authHeaders,
    `            .header("Content-Type", "application/json")`,
    "            .POST(HttpRequest.BodyPublishers.ofString(body))",
    "            .build();",
    "        HttpResponse<String> resp = HttpClient.newHttpClient()",
    "            .send(req, HttpResponse.BodyHandlers.ofString());",
    "        System.out.println(resp.body());",
    "    }",
    "}",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Go（net/http 标准库）
// ---------------------------------------------------------------------------

/** 请求体不含反引号时用原始字符串（更易读），否则退回双引号转义。 */
function goBody(json: string): string {
  return json.includes("`") ? q(json) : `\`${json}\``;
}

function renderGo(protocol: SampleProtocol, p: SampleParams): string {
  const env = p.keyExpr.kind === "env";
  const key = keyValue(p.keyExpr, `os.Getenv(${q(API_KEY_ENV)})`);
  const authHeaders =
    protocol === "anthropic"
      ? [
          `\treq.Header.Set("x-api-key", apiKey)`,
          `\treq.Header.Set("anthropic-version", ${q(ANTHROPIC_VERSION)})`,
        ]
      : [`\treq.Header.Set("Authorization", "Bearer "+apiKey)`];
  return [
    "package main",
    "",
    "import (",
    '\t"fmt"',
    '\t"io"',
    '\t"net/http"',
    ...(env ? ['\t"os"'] : []),
    '\t"strings"',
    ")",
    "",
    "func main() {",
    `\tapiKey := ${key}`,
    `\tbody := ${goBody(jsonBody(protocol, p.model))}`,
    "",
    `\treq, err := http.NewRequest("POST", ${q(endpointUrl(protocol, p.baseUrl))}, strings.NewReader(body))`,
    "\tif err != nil {",
    "\t\tpanic(err)",
    "\t}",
    ...authHeaders,
    `\treq.Header.Set("Content-Type", "application/json")`,
    "",
    "\tresp, err := http.DefaultClient.Do(req)",
    "\tif err != nil {",
    "\t\tpanic(err)",
    "\t}",
    "\tdefer resp.Body.Close()",
    "",
    "\tout, err := io.ReadAll(resp.Body)",
    "\tif err != nil {",
    "\t\tpanic(err)",
    "\t}",
    "\tfmt.Println(string(out))",
    "}",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// PowerShell（Invoke-RestMethod；Windows PowerShell 5.1 与 PowerShell 7+ 通用）
// ---------------------------------------------------------------------------

/** PowerShell 单引号字符串：`'` 与它的弯引号变体（PowerShell 也当作引号）都要翻倍。 */
function psSingle(value: string): string {
  return `'${value.replace(/['\u2018\u2019\u201A\u201B]/g, (ch) => ch + ch)}'`;
}

function renderPowerShell(protocol: SampleProtocol, p: SampleParams): string {
  // 环境变量：双引号内的 `$env:NAME` 会被展开；字面量走单引号，不会展开 `$`。
  const bearer =
    p.keyExpr.kind === "env"
      ? `"Bearer $env:${API_KEY_ENV}"`
      : psSingle(`Bearer ${p.keyExpr.value}`);
  const apiKey =
    p.keyExpr.kind === "env"
      ? `$env:${API_KEY_ENV}`
      : psSingle(p.keyExpr.value);
  const headers =
    protocol === "anthropic"
      ? [
          `    "x-api-key" = ${apiKey}`,
          `    "anthropic-version" = ${psSingle(ANTHROPIC_VERSION)}`,
        ]
      : [`    "Authorization" = ${bearer}`];
  return [
    "$headers = @{",
    ...headers,
    "}",
    `$body = ${psSingle(jsonBody(protocol, p.model))}`,
    "",
    "$resp = Invoke-RestMethod `",
    `  -Uri ${psSingle(endpointUrl(protocol, p.baseUrl))} \``,
    "  -Method Post `",
    "  -Headers $headers `",
    '  -ContentType "application/json" `',
    "  -Body ([System.Text.Encoding]::UTF8.GetBytes($body))",
    "$resp | ConvertTo-Json -Depth 10",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

export function renderSample(
  lang: SampleLang,
  protocol: SampleProtocol,
  params: SampleParams,
): string {
  switch (lang) {
    case "curl":
      return renderCurl(protocol, params);
    case "python":
      return renderPython(protocol, params);
    case "node":
      return renderNode(protocol, params);
    case "java":
      return renderJava(protocol, params);
    case "go":
      return renderGo(protocol, params);
    case "powershell":
      return renderPowerShell(protocol, params);
  }
}
