import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { settingsApi } from "@/lib/api/settings";
import {
  we2aiApi,
  type We2aiKeyModels,
  type We2aiToolStatusReport,
} from "@/we2ai/api";
import { ModelSquarePage } from "@/we2ai/ModelSquarePage";
import { ToolStatusBar } from "@/we2ai/ToolStatusBar";
import { getWe2aiStrings } from "@/we2ai/strings";

const t = getWe2aiStrings("zh");

if (typeof window.matchMedia !== "function") {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

const key = {
  id: 7,
  name: "工作",
  groupName: "混合组",
  status: "active",
  maskedKey: "sk-we2…7777",
};

const models: We2aiKeyModels = {
  models: [
    {
      id: "claude-sonnet-4-5",
      provider: "anthropic",
      tools: ["claude_code", "workbuddy"],
    },
    { id: "claude-haiku-4-5", provider: "anthropic", tools: ["claude_code"] },
    { id: "gpt-5", provider: null, tools: ["codex"] },
  ],
  callable: true,
  blockedReason: null,
};

const status: We2aiToolStatusReport = {
  tools: [
    {
      tool: "claude_code",
      installed: true,
      broken: false,
      version: "2.1.0",
      downloadUrl: "https://docs.anthropic.com/en/docs/claude-code/setup",
      managedModel: "claude-sonnet-4-5",
    },
    {
      tool: "codex",
      installed: false,
      broken: false,
      version: null,
      downloadUrl: "https://github.com/openai/codex/releases",
      managedModel: null,
    },
    {
      tool: "workbuddy",
      installed: true,
      broken: false,
      version: "5.5.3",
      downloadUrl: "https://www.workbuddy.ai/downloads",
      managedModel: null,
    },
  ],
  ccSwitchRunning: false,
};

function setup(toolStatus: We2aiToolStatusReport | null = status) {
  vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
    keys: [key],
    selectedKeyId: 7,
  });
  vi.spyOn(we2aiApi, "keyModels").mockResolvedValue(models);
  vi.spyOn(we2aiApi, "applyPlan").mockImplementation(async (tool) => ({
    files: [`/home/u/${tool}.conf`],
    fields: ["model"],
  }));
  const onApplied = vi.fn();
  render(
    <ThemeProvider defaultTheme="system" storageKey="we2ai-apply-test-theme">
      <ModelSquarePage
        t={t}
        onSessionMaybeEnded={vi.fn()}
        toolStatus={toolStatus}
        onApplied={onApplied}
      />
      <Toaster />
    </ThemeProvider>,
  );
  return onApplied;
}

async function openApply(modelId: string, toolLabel: string) {
  const card = (await screen.findAllByTestId("we2ai-model-card")).find((c) =>
    within(c).queryByText(modelId),
  )!;
  await userEvent.click(
    within(card).getByRole("button", { name: new RegExp(`^${toolLabel}`) }),
  );
  return await screen.findByRole("dialog");
}

describe("WE2AI apply flow", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("confirms with the files and fields to be written, then applies", async () => {
    const onApplied = setup();
    const apply = vi.spyOn(we2aiApi, "applyModel").mockResolvedValue({
      model: "gpt-5",
      files: ["/home/u/.codex/config.toml"],
      warnings: [],
    });

    const dialog = await openApply("gpt-5", "Codex");
    expect(
      await within(dialog).findByText("/home/u/codex.conf"),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );

    await waitFor(() => expect(onApplied).toHaveBeenCalledTimes(1));
    expect(apply).toHaveBeenCalledWith({
      tool: "codex",
      keyId: 7,
      model: "gpt-5",
      claudeSlots: undefined,
      overwrite: false,
    });
    // Codex 未安装：成功提示里带上"安装后即可使用"。
    expect(
      await screen.findByText(/Codex 尚未安装，配置已写好/),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  it("lets Claude Code slots be chosen separately under advanced", async () => {
    setup();
    const apply = vi.spyOn(we2aiApi, "applyModel").mockResolvedValue({
      model: "claude-sonnet-4-5",
      files: [],
      warnings: [],
    });
    const dialog = await openApply("claude-sonnet-4-5", "Claude Code");
    await userEvent.click(within(dialog).getByText(t.applyAdvanced));
    await userEvent.click(
      within(dialog).getByRole("combobox", { name: t.slotHaiku }),
    );
    await userEvent.click(
      await screen.findByRole("option", { name: "claude-haiku-4-5" }),
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );

    await waitFor(() => expect(apply).toHaveBeenCalledTimes(1));
    expect(apply.mock.calls[0][0].claudeSlots).toEqual({
      sonnet: null,
      opus: null,
      haiku: "claude-haiku-4-5",
    });
  });

  it("asks before overwriting a WorkBuddy entry and retries with overwrite", async () => {
    const onApplied = setup();
    const apply = vi
      .spyOn(we2aiApi, "applyModel")
      .mockRejectedValueOnce({
        code: "WORKBUDDY_CONFIRM_OVERWRITE",
        message: "exists",
      })
      .mockResolvedValueOnce({
        model: "claude-sonnet-4-5",
        files: [],
        warnings: [],
      });
    const dialog = await openApply("claude-sonnet-4-5", "WorkBuddy");
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );

    expect(
      await screen.findByText(t.workbuddyOverwriteTitle),
    ).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: t.workbuddyOverwriteConfirm }),
    );
    await waitFor(() => expect(onApplied).toHaveBeenCalledTimes(1));
    expect(apply.mock.calls[1][0].overwrite).toBe(true);
  });

  it("cancelling the WorkBuddy overwrite writes nothing more", async () => {
    const onApplied = setup();
    const apply = vi.spyOn(we2aiApi, "applyModel").mockRejectedValue({
      code: "WORKBUDDY_CONFIRM_OVERWRITE",
      message: "exists",
    });
    const dialog = await openApply("claude-sonnet-4-5", "WorkBuddy");
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );
    await screen.findByText(t.workbuddyOverwriteTitle);
    await userEvent.click(screen.getByRole("button", { name: t.applyCancel }));

    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(apply).toHaveBeenCalledTimes(1);
    expect(onApplied).not.toHaveBeenCalled();
  });

  it("shows the takeover conflict and does not report success", async () => {
    const onApplied = setup();
    vi.spyOn(we2aiApi, "applyModel").mockRejectedValue({
      code: "TAKEOVER_CONFLICT",
      message: "taken over",
    });
    const dialog = await openApply("gpt-5", "Codex");
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );
    expect(await screen.findByText(t.errorApplyTakeover)).toBeInTheDocument();
    expect(onApplied).not.toHaveBeenCalled();
  });

  it("does not allow confirming when the write plan cannot be loaded", async () => {
    setup();
    vi.spyOn(we2aiApi, "applyPlan").mockRejectedValue(new Error("boom"));
    const apply = vi.spyOn(we2aiApi, "applyModel");
    const dialog = await openApply("gpt-5", "Codex");
    expect(
      await within(dialog).findByText(t.applyPlanFailed),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    ).toBeDisabled();
    expect(apply).not.toHaveBeenCalled();
  });

  it("marks the tool button whose WE2AI model is in use", async () => {
    setup();
    const card = (await screen.findAllByTestId("we2ai-model-card")).find((c) =>
      within(c).queryByText("claude-sonnet-4-5"),
    )!;
    expect(
      within(card).getByRole("button", { name: "Claude Code ✓" }),
    ).toBeInTheDocument();
    expect(
      within(card).getByRole("button", { name: "WorkBuddy" }),
    ).toBeInTheDocument();
  });
});

describe("ToolStatusBar", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("shows versions, download links for missing tools and the current model", async () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue(undefined as never);
    render(<ToolStatusBar t={t} report={status} />);

    const claude = screen.getByTestId("we2ai-tool-claude_code");
    expect(claude).toHaveTextContent("2.1.0");
    expect(claude).toHaveTextContent("当前：claude-sonnet-4-5");
    const codex = screen.getByTestId("we2ai-tool-codex");
    expect(codex).toHaveTextContent(t.toolNotInstalled);
    await userEvent.click(
      within(codex).getByRole("button", { name: t.toolDownload }),
    );
    expect(openExternal).toHaveBeenCalledWith(
      "https://github.com/openai/codex/releases",
    );
    expect(screen.queryByText(t.ccSwitchRunningBanner)).not.toBeInTheDocument();
  });

  it("tells installed-but-broken apart from not installed", () => {
    const broken = {
      ...status,
      tools: status.tools.map((tool) =>
        tool.tool === "claude_code"
          ? { ...tool, installed: true, broken: true, version: null }
          : tool,
      ),
    };
    render(<ToolStatusBar t={t} report={broken} />);
    const claude = screen.getByTestId("we2ai-tool-claude_code");
    expect(claude).toHaveTextContent(t.toolBroken);
    expect(claude).not.toHaveTextContent(t.toolNotInstalled);
  });

  it("warns when CC Switch is also running", () => {
    render(
      <ToolStatusBar t={t} report={{ ...status, ccSwitchRunning: true }} />,
    );
    expect(screen.getByText(t.ccSwitchRunningBanner)).toBeInTheDocument();
  });
});
