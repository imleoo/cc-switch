import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";
import { copyText } from "@/lib/clipboard";
import {
  we2aiApi,
  type We2aiApiError,
  type We2aiKeyModels,
  type We2aiManagedKey,
} from "@/we2ai/api";
import { CodeSampleDrawer } from "@/we2ai/CodeSampleDrawer";
import { KEY_PLACEHOLDER } from "@/we2ai/codeSamples";
import { getWe2aiStrings } from "@/we2ai/strings";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}));

vi.mock("@/lib/clipboard", () => ({
  copyText: vi.fn(async () => undefined),
}));

const t = getWe2aiStrings("zh");
const BASE = "https://api.we2ai.com";
const SECRET = "sk-we2ai-plaintext-should-never-render-1234";
const MASKED = "sk-we2…7777";

const keyItem: We2aiManagedKey = {
  id: 7,
  name: "日常开发",
  status: "active",
  maskedKey: MASKED,
  group: { id: 3, name: "优选分组", rate: 0.8 },
  quota: 0,
  quotaUsed: 0,
  expiresAt: null,
  lastUsedAt: null,
};

function apiError(code: string): We2aiApiError {
  return { code, message: code } as We2aiApiError;
}

function models(ids: string[], callable = true): We2aiKeyModels {
  return {
    models: ids.map((id) => ({ id, provider: null, tools: [], price: null })),
    callable,
    blockedReason: callable ? null : "insufficient_balance",
    pricing: null,
  };
}

function mockBackend(
  options: {
    platform?: string;
    modelIds?: string[];
    /** 设置后 B1 失败（如 Key 已禁用/不在缓存）。 */
    modelsError?: unknown;
    callable?: boolean;
  } = {},
) {
  const { platform = "openai", modelIds = ["claude-sonnet-4-5", "gpt-4.1"] } =
    options;
  vi.spyOn(we2aiApi, "gatewayInfo").mockResolvedValue({
    baseUrl: BASE,
    webUrl: "https://we2ai.com",
  });
  vi.spyOn(we2aiApi, "listKeyGroups").mockResolvedValue([
    { id: 3, name: "优选分组", platform, rate: 0.8 },
  ]);
  const keyModels = vi.spyOn(we2aiApi, "keyModels");
  if (options.modelsError !== undefined) {
    keyModels.mockRejectedValue(options.modelsError);
  } else {
    keyModels.mockResolvedValue(models(modelIds, options.callable ?? true));
  }
  return {
    copyTextWithKey: vi.spyOn(we2aiApi, "copyTextWithKey").mockResolvedValue(),
    copyPlainText: vi.spyOn(we2aiApi, "copyPlainText").mockResolvedValue(),
    keyModels,
  };
}

async function renderDrawer(
  props: Partial<React.ComponentProps<typeof CodeSampleDrawer>> = {},
) {
  const onClose = vi.fn();
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(
      <CodeSampleDrawer t={t} keyItem={keyItem} onClose={onClose} {...props} />,
    );
  });
  return { ...view, onClose };
}

const code = () => screen.getByTestId("sample-code").textContent ?? "";
const protocolButton = (name: string) => screen.getByRole("button", { name });
const langTab = (name: string) => screen.getByRole("tab", { name });

describe("CodeSampleDrawer", () => {
  beforeEach(() => {
    vi.spyOn(we2aiApi, "gatewayInfo").mockResolvedValue({
      baseUrl: BASE,
      webUrl: "https://we2ai.com",
    });
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  describe("header and defaults", () => {
    it("shows the key name and group in the title and starts on curl", async () => {
      mockBackend();
      await renderDrawer();

      expect(
        await screen.findByRole("heading", {
          name: "调用示例 · 日常开发（优选分组）",
        }),
      ).toBeInTheDocument();
      expect(langTab("curl")).toHaveAttribute("aria-selected", "true");
      expect(code()).toContain(
        "curl https://api.we2ai.com/v1/chat/completions",
      );
    });

    it("defaults to OpenAI compatible for non-anthropic groups", async () => {
      mockBackend({ platform: "openai" });
      await renderDrawer();

      expect(protocolButton("OpenAI 兼容")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(protocolButton("Anthropic")).toHaveAttribute(
        "aria-pressed",
        "false",
      );
    });

    it("defaults to Anthropic when the key's group platform is anthropic", async () => {
      mockBackend({ platform: "anthropic" });
      await renderDrawer();

      await waitFor(() =>
        expect(protocolButton("Anthropic")).toHaveAttribute(
          "aria-pressed",
          "true",
        ),
      );
      expect(code()).toContain("https://api.we2ai.com/v1/messages");
      expect(code()).toContain("x-api-key");
    });

    it("keeps the user's own protocol choice when the group platform arrives later", async () => {
      mockBackend({ platform: "anthropic" });
      let resolveGroups!: (v: never[] | unknown) => void;
      vi.spyOn(we2aiApi, "listKeyGroups").mockReturnValue(
        new Promise((resolve) => {
          resolveGroups = resolve as typeof resolveGroups;
        }),
      );
      await renderDrawer();

      await userEvent.click(protocolButton("Responses"));
      await act(async () => {
        resolveGroups([
          { id: 3, name: "优选分组", platform: "anthropic", rate: 1 },
        ]);
      });

      expect(protocolButton("Responses")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(protocolButton("Anthropic")).toHaveAttribute(
        "aria-pressed",
        "false",
      );
    });

    it("falls back to OpenAI compatible when the group list cannot be loaded", async () => {
      mockBackend();
      vi.spyOn(we2aiApi, "listKeyGroups").mockRejectedValue(
        apiError("NETWORK_ERROR"),
      );
      await renderDrawer();

      expect(protocolButton("OpenAI 兼容")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(code()).toContain("/v1/chat/completions");
    });

    it("a key without a group shows 无分组 and never asks for the group list", async () => {
      const backend = mockBackend();
      const listGroups = vi.spyOn(we2aiApi, "listKeyGroups");
      await renderDrawer({ keyItem: { ...keyItem, group: null } });

      expect(
        await screen.findByRole("heading", {
          name: "调用示例 · 日常开发（无分组）",
        }),
      ).toBeInTheDocument();
      expect(listGroups).not.toHaveBeenCalled();
      expect(backend.keyModels).toHaveBeenCalledWith(7);
    });
  });

  describe("protocol and language switching", () => {
    it("switches protocol: code, auth header and Base URL follow the base rule (/v1 vs bare)", async () => {
      mockBackend();
      await renderDrawer();

      expect(screen.getByTestId("sample-base-url")).toHaveTextContent(
        `${BASE}/v1`,
      );
      expect(code()).toContain("Authorization: Bearer");

      await userEvent.click(protocolButton("Anthropic"));
      expect(screen.getByTestId("sample-base-url").textContent).toBe(BASE);
      expect(code()).toContain("https://api.we2ai.com/v1/messages");
      expect(code()).toContain("x-api-key: $WE2AI_API_KEY");
      expect(code()).toContain("anthropic-version: 2023-06-01");

      await userEvent.click(protocolButton("Responses"));
      expect(screen.getByTestId("sample-base-url").textContent).toBe(
        `${BASE}/v1`,
      );
      expect(code()).toContain("https://api.we2ai.com/v1/responses");
      expect(code()).toContain('"input":"Hello"');
    });

    it("switches language and keeps the protocol and model", async () => {
      mockBackend();
      await renderDrawer();
      await userEvent.click(protocolButton("Anthropic"));

      await userEvent.click(langTab("Python"));
      expect(langTab("Python")).toHaveAttribute("aria-selected", "true");
      expect(code()).toContain("from anthropic import Anthropic");
      expect(code()).toContain('base_url="https://api.we2ai.com"');
      expect(code()).toContain('model="claude-sonnet-4-5"');

      await userEvent.click(langTab("Node.js"));
      expect(code()).toContain('from "@anthropic-ai/sdk"');

      await userEvent.click(langTab("Java"));
      expect(code()).toContain("java.net.http.HttpClient");

      await userEvent.click(langTab("Go"));
      expect(code()).toContain("package main");

      await userEvent.click(langTab("PowerShell"));
      expect(code()).toContain("Invoke-RestMethod");
      expect(code()).toContain("https://api.we2ai.com/v1/messages");
    });

    it("shows a prerequisite line above the code for every language (and follows the protocol)", async () => {
      mockBackend();
      await renderDrawer();
      const prerequisite = () =>
        screen.getByTestId("sample-prerequisite").textContent ?? "";

      expect(prerequisite()).toContain("bash / zsh");
      expect(prerequisite()).toContain("Git Bash");
      expect(prerequisite()).toContain("PowerShell");

      await userEvent.click(langTab("Python"));
      expect(prerequisite()).toContain("pip install openai");
      await userEvent.click(langTab("Node.js"));
      expect(prerequisite()).toContain("npm i openai");
      expect(prerequisite()).toContain("Node.js 22+");
      expect(prerequisite()).toContain(".mjs");
      await userEvent.click(protocolButton("Anthropic"));
      expect(prerequisite()).toContain("npm i @anthropic-ai/sdk");
      await userEvent.click(langTab("Python"));
      expect(prerequisite()).toContain("pip install anthropic");
      await userEvent.click(langTab("Java"));
      expect(prerequisite()).toContain("JDK 11+");
      expect(prerequisite()).toContain("We2aiDemo.java");
      await userEvent.click(langTab("Go"));
      expect(prerequisite()).toContain("Go 1.20+");
      expect(prerequisite()).toContain("go run main.go");
      await userEvent.click(langTab("PowerShell"));
      expect(prerequisite()).toContain("PowerShell 5.1");
    });

    it("language tabs are a labelled tablist with roving tabindex and arrow-key navigation", async () => {
      mockBackend();
      await renderDrawer();
      const tablist = screen.getByRole("tablist", { name: "语言" });
      expect(within(tablist).getAllByRole("tab")).toHaveLength(6);
      expect(langTab("curl")).toHaveAttribute("tabindex", "0");
      expect(langTab("Python")).toHaveAttribute("tabindex", "-1");

      langTab("curl").focus();
      await userEvent.keyboard("{ArrowRight}");
      expect(langTab("Python")).toHaveAttribute("aria-selected", "true");
      expect(langTab("Python")).toHaveFocus();
      expect(langTab("Python")).toHaveAttribute("tabindex", "0");
      expect(langTab("curl")).toHaveAttribute("tabindex", "-1");

      await userEvent.keyboard("{ArrowLeft}{ArrowLeft}");
      // 从第一个再往左回绕到最后一个。
      expect(langTab("PowerShell")).toHaveAttribute("aria-selected", "true");
      expect(langTab("PowerShell")).toHaveFocus();

      await userEvent.keyboard("{ArrowRight}");
      expect(langTab("curl")).toHaveAttribute("aria-selected", "true");
      await userEvent.keyboard("{End}");
      expect(langTab("PowerShell")).toHaveFocus();
      await userEvent.keyboard("{Home}");
      expect(langTab("curl")).toHaveFocus();
    });

    it("shows both the bash and the PowerShell environment variable commands", async () => {
      mockBackend();
      await renderDrawer();

      const hint = screen.getByTestId("sample-key-hint");
      expect(hint).toHaveTextContent("export WE2AI_API_KEY=<你的 Key>");
      expect(hint).toHaveTextContent('$env:WE2AI_API_KEY="<你的 Key>"');
    });

    it("shows the environment variable hint above the code by default", async () => {
      mockBackend();
      await renderDrawer();

      expect(screen.getByTestId("sample-key-hint")).toHaveTextContent(
        "export WE2AI_API_KEY=<你的 Key>",
      );
    });
  });

  describe("model selection", () => {
    it("lists only the callable models from B1 and defaults to the first", async () => {
      mockBackend({ modelIds: ["gpt-4.1", "claude-sonnet-4-5", "gpt-4.1"] });
      await renderDrawer();

      const select = await screen.findByRole("combobox", { name: "模型" });
      const options = within(select)
        .getAllByRole("option")
        .map((o) => o.textContent);
      expect(options).toEqual(["gpt-4.1", "claude-sonnet-4-5"]);
      expect(select).toHaveValue("gpt-4.1");
      expect(code()).toContain('"model":"gpt-4.1"');
      expect(screen.queryByTestId("sample-model-fallback")).toBeNull();

      await userEvent.selectOptions(select, "claude-sonnet-4-5");
      expect(code()).toContain('"model":"claude-sonnet-4-5"');
    });

    it("hides non-text models (image/video/audio/other) but keeps text and kind-less ones", async () => {
      const backend = mockBackend();
      backend.keyModels.mockResolvedValue({
        ...models([]),
        models: [
          { id: "text-model", kind: "text" },
          { id: "legacy-model" },
          { id: "image-model", kind: "image" },
          { id: "video-model", kind: "video" },
          { id: "audio-model", kind: "audio" },
          { id: "embed-model", kind: "other" },
          { id: "future-model", kind: "hologram" },
        ].map((m) => ({ provider: null, tools: [], price: null, ...m })),
      });
      await renderDrawer();

      const select = await screen.findByRole("combobox", { name: "模型" });
      const options = within(select)
        .getAllByRole("option")
        .map((o) => o.textContent);
      expect(options).toEqual(["text-model", "legacy-model"]);
      expect(code()).toContain('"model":"text-model"');
    });

    it("degrades to the text box when every model is filtered out as non-text", async () => {
      const backend = mockBackend();
      backend.keyModels.mockResolvedValue({
        ...models([]),
        models: [
          { id: "image-model", kind: "image" },
          { id: "video-model", kind: "video" },
        ].map((m) => ({ provider: null, tools: [], price: null, ...m })),
      });
      await renderDrawer();

      expect(await screen.findByRole("textbox", { name: "模型" })).toHaveValue(
        "gpt-4.1",
      );
      expect(screen.getByTestId("sample-model-fallback")).toBeInTheDocument();
      expect(screen.queryByRole("combobox", { name: "模型" })).toBeNull();
    });

    it("degrades to an editable text box with a protocol-specific default when B1 fails", async () => {
      mockBackend({ modelsError: apiError("KEY_NOT_FOUND") });
      await renderDrawer();

      const input = await screen.findByRole("textbox", { name: "模型" });
      expect(input).toHaveValue("gpt-4.1");
      expect(screen.getByTestId("sample-model-fallback")).toHaveTextContent(
        "请手动填写模型名",
      );
      expect(code()).toContain('"model":"gpt-4.1"');

      // 默认值跟着协议走。
      await userEvent.click(protocolButton("Anthropic"));
      expect(input).toHaveValue("claude-sonnet-4-5");

      // 手改之后不再被协议覆盖，代码随输入更新。
      await userEvent.clear(input);
      await userEvent.type(input, "my-model");
      expect(code()).toContain('"model":"my-model"');
      await userEvent.click(protocolButton("Responses"));
      expect(input).toHaveValue("my-model");
      expect(code()).toContain('"model":"my-model"');
    });

    it("degrades to the text box when B1 says not callable or returns no models", async () => {
      mockBackend({ modelIds: ["gpt-4.1"], callable: false });
      const first = await renderDrawer();
      expect(await screen.findByRole("textbox", { name: "模型" })).toHaveValue(
        "gpt-4.1",
      );
      first.unmount();

      vi.restoreAllMocks();
      mockBackend({ modelIds: [] });
      await renderDrawer();
      expect(
        await screen.findByRole("textbox", { name: "模型" }),
      ).toBeInTheDocument();
      expect(screen.getByTestId("sample-model-fallback")).toBeInTheDocument();
    });

    it("shows a replace-me hint next to the default fallback model and drops it once edited", async () => {
      mockBackend({ modelsError: apiError("KEY_NOT_FOUND") });
      await renderDrawer();
      const input = await screen.findByRole("textbox", { name: "模型" });

      expect(screen.getByTestId("sample-model-default-hint")).toHaveTextContent(
        "请替换为你分组可用的模型",
      );
      await userEvent.type(input, "x");
      expect(screen.queryByTestId("sample-model-default-hint")).toBeNull();
    });

    it("disables copy and warns when the model name contains the key placeholder", async () => {
      mockBackend({ modelsError: apiError("KEY_NOT_FOUND") });
      await renderDrawer();
      const input = await screen.findByRole("textbox", { name: "模型" });
      const copy = screen.getByRole("button", { name: "复制代码" });
      expect(copy).toBeEnabled();

      await userEvent.clear(input);
      await userEvent.type(input, `m-${KEY_PLACEHOLDER}`);

      expect(copy).toBeDisabled();
      expect(
        screen.getByTestId("sample-model-placeholder-conflict"),
      ).toBeInTheDocument();

      await userEvent.clear(input);
      await userEvent.type(input, "fine");
      expect(copy).toBeEnabled();
      expect(
        screen.queryByTestId("sample-model-placeholder-conflict"),
      ).toBeNull();
    });

    it("keeps copy disabled while the model list is still loading", async () => {
      mockBackend();
      let resolveModels!: (value: We2aiKeyModels) => void;
      vi.spyOn(we2aiApi, "keyModels").mockReturnValue(
        new Promise((resolve) => {
          resolveModels = resolve;
        }),
      );
      await renderDrawer();

      expect(screen.getByRole("button", { name: "复制代码" })).toBeDisabled();

      await act(async () => {
        resolveModels(models(["gpt-4.1"]));
      });
      expect(screen.getByRole("button", { name: "复制代码" })).toBeEnabled();
    });

    it("disables copy while the model name is blank", async () => {
      mockBackend({ modelsError: apiError("KEY_NOT_FOUND") });
      await renderDrawer();
      const input = await screen.findByRole("textbox", { name: "模型" });

      await userEvent.clear(input);

      expect(screen.getByRole("button", { name: "复制代码" })).toBeDisabled();
    });
  });

  describe("copy code without the real key", () => {
    it("copies the displayed text (environment variable) through we2ai_copy_text, never the web clipboard or the key command", async () => {
      const { copyTextWithKey, copyPlainText } = mockBackend();
      await renderDrawer();
      await screen.findByRole("combobox", { name: "模型" });

      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      await waitFor(() => expect(copyPlainText).toHaveBeenCalledTimes(1));
      expect(copyText).not.toHaveBeenCalled();
      const copied = copyPlainText.mock.calls[0][0];
      expect(copied).toBe(code());
      expect(copied).toContain("$WE2AI_API_KEY");
      expect(copied).not.toContain(KEY_PLACEHOLDER);
      expect(copied).not.toContain(MASKED);
      expect(copyTextWithKey).not.toHaveBeenCalled();
      expect(toast.success).toHaveBeenCalledWith(t.keyMgrCopied);
    });

    it("shows a failure toast when the clipboard write fails", async () => {
      const { copyPlainText } = mockBackend();
      copyPlainText.mockRejectedValueOnce(apiError("CLIPBOARD_FAILED"));
      await renderDrawer();

      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrCopyFailed),
      );
    });

    it("copies the Base URL for the current protocol through we2ai_copy_text", async () => {
      const { copyTextWithKey, copyPlainText } = mockBackend();
      await renderDrawer();

      await userEvent.click(
        screen.getByRole("button", { name: "复制 Base URL" }),
      );
      await waitFor(() =>
        expect(copyPlainText).toHaveBeenLastCalledWith(`${BASE}/v1`),
      );

      await userEvent.click(protocolButton("Anthropic"));
      await userEvent.click(
        screen.getByRole("button", { name: "复制 Base URL" }),
      );
      await waitFor(() => expect(copyPlainText).toHaveBeenLastCalledWith(BASE));
      expect(copyText).not.toHaveBeenCalled();
      expect(copyTextWithKey).not.toHaveBeenCalled();
    });
  });

  describe("fill in the real key", () => {
    async function checkFill() {
      await userEvent.click(
        screen.getByRole("checkbox", { name: "填入真实 Key" }),
      );
    }

    it("shows the masked key literal and the copy hint, never the placeholder or the env var", async () => {
      mockBackend();
      await renderDrawer();
      await checkFill();

      expect(code()).toContain(`Authorization: Bearer ${MASKED}`);
      expect(code()).not.toContain(KEY_PLACEHOLDER);
      expect(code()).not.toContain("$WE2AI_API_KEY");
      expect(screen.getByTestId("sample-key-hint")).toHaveTextContent(
        "复制时将填入完整 Key",
      );
      expect(screen.queryByText(/export WE2AI_API_KEY/)).toBeNull();

      // 其它语言同样只显示掩码。
      await userEvent.click(langTab("Python"));
      expect(code()).toContain(`api_key="${MASKED}"`);
      expect(code()).not.toContain("os.environ");
      await userEvent.click(langTab("Go"));
      expect(code()).toContain(`apiKey := "${MASKED}"`);
    });

    it("copy goes through we2ai_copy_text_with_key with the placeholder version, not the plain-text command", async () => {
      const { copyTextWithKey, copyPlainText } = mockBackend();
      await renderDrawer();
      await checkFill();

      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      await waitFor(() => expect(copyTextWithKey).toHaveBeenCalledTimes(1));
      const [id, text] = copyTextWithKey.mock.calls[0];
      expect(copyTextWithKey.mock.calls[0]).toHaveLength(2);
      expect(id).toBe(7);
      expect(text).toContain(`Authorization: Bearer ${KEY_PLACEHOLDER}`);
      expect(text).not.toContain(MASKED);
      expect(text).not.toContain("$WE2AI_API_KEY");
      // 其余部分与显示版本一致：把占位串换回掩码就等于代码框内容。
      expect(text.split(KEY_PLACEHOLDER).join(MASKED)).toBe(code());
      expect(copyPlainText).not.toHaveBeenCalled();
      expect(copyText).not.toHaveBeenCalled();
      expect(toast.success).toHaveBeenCalledWith(t.keyMgrCopied);
    });

    it("unchecking restores the environment variable version and the plain-text copy", async () => {
      const { copyTextWithKey, copyPlainText } = mockBackend();
      await renderDrawer();
      await checkFill();
      await checkFill();

      expect(code()).toContain("$WE2AI_API_KEY");
      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));
      await waitFor(() => expect(copyPlainText).toHaveBeenCalledTimes(1));
      expect(copyTextWithKey).not.toHaveBeenCalled();
    });

    it("KEY_NOT_FOUND from Rust refreshes the list and asks to retry", async () => {
      const { copyTextWithKey } = mockBackend();
      copyTextWithKey.mockRejectedValueOnce(apiError("KEY_NOT_FOUND"));
      const onKeyStale = vi.fn();
      await renderDrawer({ onKeyStale });
      await checkFill();

      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrCopyStale),
      );
      expect(onKeyStale).toHaveBeenCalledTimes(1);
      // 抽屉仍在，可以再点一次。
      expect(screen.getByTestId("code-sample-drawer")).toBeInTheDocument();
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "复制代码" })).toBeEnabled(),
      );
    });

    it("clipboard failure shows the copy-failed message without a session check", async () => {
      const { copyTextWithKey } = mockBackend();
      copyTextWithKey.mockRejectedValueOnce(apiError("CLIPBOARD_FAILED"));
      const onSessionMaybeEnded = vi.fn();
      await renderDrawer({ onSessionMaybeEnded });
      await checkFill();

      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      await waitFor(() =>
        expect(toast.error).toHaveBeenCalledWith(t.keyMgrCopyFailed),
      );
      expect(onSessionMaybeEnded).not.toHaveBeenCalled();
    });

    it("other API errors ask the shell to re-check the session", async () => {
      const { copyTextWithKey } = mockBackend();
      copyTextWithKey.mockRejectedValueOnce(apiError("TOKEN_REVOKED"));
      const onSessionMaybeEnded = vi.fn();
      await renderDrawer({ onSessionMaybeEnded });
      await checkFill();

      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      await waitFor(() => expect(onSessionMaybeEnded).toHaveBeenCalledTimes(1));
      expect(toast.error).toHaveBeenCalled();
    });
  });

  describe("plaintext boundary", () => {
    it("no plaintext ever reaches the DOM, in any protocol, language or fill mode", async () => {
      const { copyTextWithKey } = mockBackend();
      // 即使 Rust 侧（被 mock 的命令）手里有明文，它也只会进剪贴板，不会回到前端。
      copyTextWithKey.mockImplementation(async () => {
        void SECRET;
      });
      await renderDrawer();

      for (const fill of [false, true]) {
        if (fill) {
          await userEvent.click(
            screen.getByRole("checkbox", { name: "填入真实 Key" }),
          );
        }
        for (const protocol of ["OpenAI 兼容", "Anthropic", "Responses"]) {
          await userEvent.click(protocolButton(protocol));
          for (const lang of [
            "curl",
            "Python",
            "Node.js",
            "Java",
            "Go",
            "PowerShell",
          ]) {
            await userEvent.click(langTab(lang));
            expect(document.body.textContent).not.toContain(SECRET);
            expect(document.body.textContent).not.toContain(KEY_PLACEHOLDER);
          }
        }
      }
      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));
      await waitFor(() => expect(copyTextWithKey).toHaveBeenCalled());
      expect(document.body.textContent).not.toContain(SECRET);
      expect(document.body.textContent).not.toContain(KEY_PLACEHOLDER);
    });

    it("never calls a command that returns the plaintext", async () => {
      mockBackend();
      const copyKey = vi.spyOn(we2aiApi, "copyKey");
      await renderDrawer();
      await userEvent.click(
        screen.getByRole("checkbox", { name: "填入真实 Key" }),
      );
      await userEvent.click(screen.getByRole("button", { name: "复制代码" }));

      expect(copyKey).not.toHaveBeenCalled();
      expect(
        Object.keys(we2aiApi).filter((name) => /reveal/i.test(name)),
      ).toEqual([]);
    });
  });

  describe("gateway address", () => {
    it("shows a retry when the gateway info cannot be read and recovers on retry", async () => {
      mockBackend();
      const gateway = vi
        .spyOn(we2aiApi, "gatewayInfo")
        .mockRejectedValueOnce(apiError("NETWORK_ERROR"))
        .mockResolvedValue({ baseUrl: BASE, webUrl: "https://we2ai.com" });
      await renderDrawer();

      expect(await screen.findByRole("alert")).toHaveTextContent(
        t.sampleBaseFailed,
      );
      expect(screen.queryByTestId("sample-code")).toBeNull();
      expect(screen.getByRole("button", { name: "复制代码" })).toBeDisabled();

      await userEvent.click(
        screen.getByRole("button", { name: t.offlineRetry }),
      );

      await waitFor(() => expect(gateway).toHaveBeenCalledTimes(2));
      expect(await screen.findByTestId("sample-code")).toBeInTheDocument();
      expect(code()).toContain("https://api.we2ai.com/v1/chat/completions");
    });
  });

  describe("closing", () => {
    it("the close button and Escape both call onClose", async () => {
      mockBackend();
      const { onClose } = await renderDrawer();

      await userEvent.click(
        screen.getByRole("button", { name: "关闭调用示例" }),
      );
      expect(onClose).toHaveBeenCalledTimes(1);

      await userEvent.keyboard("{Escape}");
      expect(onClose).toHaveBeenCalledTimes(2);
    });

    it("leaves no state behind: re-mounting starts from the defaults", async () => {
      mockBackend();
      const first = await renderDrawer();
      await userEvent.click(protocolButton("Anthropic"));
      await userEvent.click(langTab("Go"));
      await userEvent.click(
        screen.getByRole("checkbox", { name: "填入真实 Key" }),
      );
      first.unmount();
      expect(screen.queryByTestId("code-sample-drawer")).toBeNull();

      await renderDrawer();

      expect(protocolButton("OpenAI 兼容")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(langTab("curl")).toHaveAttribute("aria-selected", "true");
      expect(
        screen.getByRole("checkbox", { name: "填入真实 Key" }),
      ).not.toBeChecked();
    });

    it("applies the we2ai theme class to the portal content", async () => {
      mockBackend();
      await renderDrawer();

      expect(screen.getByTestId("code-sample-drawer")).toHaveClass(
        "we2ai-theme",
      );
    });
  });
});
