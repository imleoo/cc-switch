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
import { formatWe2aiString, getWe2aiStrings } from "@/we2ai/strings";

const t = getWe2aiStrings("zh");

/** 测试用最简 `We2aiPlanFile`：多数场景 display 与 path 相同即可。 */
const planFile = (p: string) => ({ display: p, path: p });

/** 测试用最简 `We2aiExtraChange`：多数场景 id 与 display 相同即可。 */
const extraChange = (display: string, id: string = display) => ({
  id,
  display,
});

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

function setup(
  toolStatus: We2aiToolStatusReport | null = status,
  onBeforeApplyDialogOpen?: () => Promise<boolean>,
) {
  vi.spyOn(we2aiApi, "listKeys").mockResolvedValue({
    keys: [key],
    selectedKeyId: 7,
  });
  vi.spyOn(we2aiApi, "keyModels").mockResolvedValue(models);
  vi.spyOn(we2aiApi, "applyPlan").mockImplementation(async (tool) => ({
    files: [planFile(`/home/u/${tool}.conf`)],
    fields: ["model"],
    extraChanges: [],
  }));
  const onApplied = vi.fn();
  render(
    <ThemeProvider defaultTheme="system" storageKey="we2ai-apply-test-theme">
      <ModelSquarePage
        t={t}
        onSessionMaybeEnded={vi.fn()}
        toolStatus={toolStatus}
        onApplied={onApplied}
        onBeforeApplyDialogOpen={onBeforeApplyDialogOpen}
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
      expectedExtraChanges: [],
    });
    // Codex 未安装：成功提示里带上"安装后即可使用"。
    expect(
      await screen.findByText(/Codex 尚未安装，配置已写好/),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  // Opus 复核中危项 S1：Codex 模型目录文件的真实文件名（上游历史遗留常量，
  // 字面含 "cc-switch"）绝不能出现在确认弹窗的 DOM 里——只渲染 `display`
  // 中性标签，`path` 只用作 React key，不出现在任何文本节点。
  it("never renders the real cc-switch filename in the confirm dialog, only the neutral display label", async () => {
    setup();
    vi.spyOn(we2aiApi, "applyPlan").mockResolvedValue({
      files: [
        planFile("/home/u/.codex/config.toml"),
        {
          display: "~/.codex/ 下的 Codex 模型目录文件",
          path: "/home/u/.codex/cc-switch-model-catalog.json",
        },
      ],
      fields: ["model_provider", "model"],
      extraChanges: [],
    });

    const dialog = await openApply("gpt-5", "Codex");
    expect(
      await within(dialog).findByText("~/.codex/ 下的 Codex 模型目录文件"),
    ).toBeInTheDocument();
    expect(dialog.textContent?.toLowerCase()).not.toContain("cc-switch");
  });

  // 偏差修复项 F（方案第 4.1 节"应用启动与每次 apply 前检测 CC Switch
  // 进程"）：此前只在登录后检查一次工具状态，登录后才启动 CC Switch 时，下
  // 一次点击工具按钮打开确认弹窗不会得到新的并存提示。现在打开确认弹窗前
  // 必须先回调一次，让外壳重新拉取工具状态（含 `ccSwitchRunning`）。
  it("refreshes tool status before opening the apply confirmation dialog", async () => {
    const onBeforeApplyDialogOpen = vi.fn(async () => true);
    setup(status, onBeforeApplyDialogOpen);
    expect(onBeforeApplyDialogOpen).not.toHaveBeenCalled();

    await openApply("gpt-5", "Codex");

    expect(onBeforeApplyDialogOpen).toHaveBeenCalledTimes(1);
  });

  // Codex 验收 X5：此前 `onBeforeApplyDialogOpen` 只是发起就不再等待，确认
  // 按钮不受这次检测约束——用户可能在新的并存警告返回之前就已经点了确认。
  // 现在必须在这次检测完成前禁用确认按钮，完成后才恢复可点。
  it("disables the confirm button until the before-open tool status check settles", async () => {
    let resolveDetection: (completed: boolean) => void = () => {};
    const detectionPromise = new Promise<boolean>((resolve) => {
      resolveDetection = resolve;
    });
    const onBeforeApplyDialogOpen = vi.fn(() => detectionPromise);
    setup(status, onBeforeApplyDialogOpen);

    const dialog = await openApply("gpt-5", "Codex");
    const confirmButton = await within(dialog).findByRole("button", {
      name: t.applyConfirm,
    });
    // 计划已经加载完成（否则 `!plan` 本身就会让按钮保持禁用，测不出这次
    // 检测专属的门控），但检测尚未 resolve，按钮必须仍然是禁用的。
    await within(dialog).findByTestId("we2ai-apply-plan");
    expect(confirmButton).toBeDisabled();

    resolveDetection(true);
    await waitFor(() => expect(confirmButton).not.toBeDisabled());
  });

  // Codex 验收 Y1：检测超时/失败（`onBeforeApplyDialogOpen` 解出 `false`）
  // 不应该继续挡着确认按钮——展示"未能完成检测"提示，但仍然放行确认。
  it("allows confirming even when the before-open detection times out", async () => {
    const onBeforeApplyDialogOpen = vi.fn(async () => false);
    setup(status, onBeforeApplyDialogOpen);

    const dialog = await openApply("gpt-5", "Codex");
    const confirmButton = await within(dialog).findByRole("button", {
      name: t.applyConfirm,
    });
    await within(dialog).findByTestId("we2ai-apply-plan");

    await waitFor(() =>
      expect(
        within(dialog).getByTestId("we2ai-apply-detection-incomplete"),
      ).toBeInTheDocument(),
    );
    expect(confirmButton).not.toBeDisabled();
  });

  // 偏差修复项 B（方案第 4.2 节"只覆盖列出的托管字段，其他内容原样保留"）：
  // 计划里的 `extraChanges`（上游写入管道会一并改动、但非 WE2AI 托管的内容）
  // 必须在确认弹窗里如实展示，而不是悄悄发生。
  it("shows extra changes the upstream writer will make, alongside the managed fields", async () => {
    setup();
    vi.spyOn(we2aiApi, "applyPlan").mockResolvedValue({
      files: [planFile("/home/u/.codex/config.toml")],
      fields: ["model_provider", "model"],
      extraChanges: [
        extraChange(
          "[model_providers.openai]（写入时会被上游管道一并重命名并规范化，这是 Codex 保留名表，非 WE2AI 托管）",
        ),
      ],
    });

    const dialog = await openApply("gpt-5", "Codex");

    expect(
      await within(dialog).findByTestId("we2ai-apply-extra-changes"),
    ).toHaveTextContent("model_providers.openai");
    expect(within(dialog).getByText(t.applyExtraChangesLabel)).toBeInTheDocument();
  });

  // 偏差修复项 B 的 L6 追加：确认时必须把用户看到的 `extraChanges` 原样带
  // 回给 Rust 侧，供写入前重新计算比对（不是只在弹窗里展示一下就忘记）。
  it("sends the confirmed plan's extraChanges back when applying", async () => {
    setup();
    vi.spyOn(we2aiApi, "applyPlan").mockResolvedValue({
      files: [planFile("/home/u/.codex/config.toml")],
      fields: ["model_provider", "model"],
      extraChanges: [extraChange("[model_providers.openai].name（写入时会被补全）")],
    });
    const apply = vi.spyOn(we2aiApi, "applyModel").mockResolvedValue({
      model: "gpt-5",
      files: ["/home/u/.codex/config.toml"],
      warnings: [],
    });

    const dialog = await openApply("gpt-5", "Codex");
    await within(dialog).findByTestId("we2ai-apply-extra-changes");
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );

    await waitFor(() => expect(apply).toHaveBeenCalledTimes(1));
    expect(apply.mock.calls[0][0].expectedExtraChanges).toEqual([
      "[model_providers.openai].name（写入时会被补全）",
    ]);
  });

  // 配置在"计划展示→点击确认"期间被外部改动：Rust 侧拒绝并返回
  // EXTRA_CHANGES_STALE，前端必须展示明确提示要求重新确认，而不是当成
  // 普通失败一笔带过。
  it("shows a clear message and does not report success when the plan went stale before confirming", async () => {
    setup();
    vi.spyOn(we2aiApi, "applyModel").mockRejectedValue({
      code: "EXTRA_CHANGES_STALE",
      message: "stale",
    });

    const dialog = await openApply("gpt-5", "Codex");
    await userEvent.click(
      within(dialog).getByRole("button", { name: t.applyConfirm }),
    );

    expect(
      await screen.findByText(t.errorApplyExtraChangesStale),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  it("shows no extra-changes section when the plan has none", async () => {
    setup();
    const dialog = await openApply("gpt-5", "Codex");
    await within(dialog).findByText("/home/u/codex.conf");

    expect(
      within(dialog).queryByTestId("we2ai-apply-extra-changes"),
    ).not.toBeInTheDocument();
  });

  // Opus 复核低危项 L5：CC Switch 也在运行的提示此前只在顶栏
  // `ToolStatusBar` 展示，确认弹窗本身看不到；用户此刻正准备点击"确认"
  // 真正写入，比顶栏更需要在这个时间点看到提示。
  it("shows the CC Switch running warning inside the confirm dialog when it is running", async () => {
    setup({ ...status, ccSwitchRunning: true });
    const dialog = await openApply("gpt-5", "Codex");
    expect(
      await within(dialog).findByTestId("we2ai-apply-other-tool-running"),
    ).toHaveTextContent(t.ccSwitchRunningBanner);
  });

  it("does not show the CC Switch running warning inside the confirm dialog when it is not running", async () => {
    setup({ ...status, ccSwitchRunning: false });
    const dialog = await openApply("gpt-5", "Codex");
    await within(dialog).findByText("/home/u/codex.conf");
    expect(
      within(dialog).queryByTestId("we2ai-apply-other-tool-running"),
    ).not.toBeInTheDocument();
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

  // P6：顶栏"恢复官方"按钮只出现在已指定 WE2AI 模型的工具行上，点击后弹窗
  // 确认，确认后调用 restoreOfficial 并只传当前这一个工具。
  it("only shows the restore action for tools with a WE2AI model, and restores just that tool", async () => {
    vi.spyOn(we2aiApi, "restorePlan").mockResolvedValue({
      files: [planFile("/home/u/.claude/settings.json")],
      fields: [
        "env.ANTHROPIC_BASE_URL（移除）",
        "env.ANTHROPIC_API_KEY（如之前保存过用户自己的 Key，则写回）",
      ],
      extraChanges: [],
    });
    const restoreOfficial = vi
      .spyOn(we2aiApi, "restoreOfficial")
      .mockResolvedValue({
        restored: ["/home/u/.claude/settings.json"],
        unchanged: [],
        skipped: [],
      });
    const onRestored = vi.fn();
    render(
      <ThemeProvider
        defaultTheme="system"
        storageKey="we2ai-tool-status-test-theme"
      >
        <ToolStatusBar t={t} report={status} onRestored={onRestored} />
        <Toaster />
      </ThemeProvider>,
    );

    const claude = screen.getByTestId("we2ai-tool-claude_code");
    expect(
      within(claude).getByRole("button", { name: t.restoreOfficialAction }),
    ).toBeInTheDocument();
    const codex = screen.getByTestId("we2ai-tool-codex");
    expect(
      within(codex).queryByRole("button", { name: t.restoreOfficialAction }),
    ).not.toBeInTheDocument();

    await userEvent.click(
      within(claude).getByRole("button", { name: t.restoreOfficialAction }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(
      await within(dialog).findByText("/home/u/.claude/settings.json"),
    ).toBeInTheDocument();
    // 恢复计划的语义是"移除/写回"，不能出现 apply 计划里"删除"用户自己
    // Key 的措辞（Opus 复核中危项 2）。
    expect(within(dialog).queryByText(/删除/)).not.toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", {
        name: t.restoreOfficialConfirmConfirm,
      }),
    );

    await waitFor(() => expect(restoreOfficial).toHaveBeenCalledTimes(1));
    expect(restoreOfficial).toHaveBeenCalledWith(["claude_code"]);
    await waitFor(() => expect(onRestored).toHaveBeenCalledTimes(1));
    expect(
      await screen.findByText(formatWe2aiString(t.toolsRestored, { count: 1 })),
    ).toBeInTheDocument();
  });

  // Opus 复核中危项 4：快速切换"打开 A → 关闭 → 打开 B"时，A 的
  // restorePlan 响应可能比 B 的更晚到达，不能覆盖掉 B 正在展示的弹窗。
  it("ignores a stale restorePlan response from a previously opened tool", async () => {
    const reportWithBoth: We2aiToolStatusReport = {
      ...status,
      tools: status.tools.map((tool) =>
        tool.tool === "codex" ? { ...tool, managedModel: "gpt-5" } : tool,
      ),
    };
    let resolveClaudePlan: (plan: {
      files: { display: string; path: string }[];
      fields: string[];
      extraChanges: { id: string; display: string }[];
    }) => void;
    const claudePlanPromise = new Promise<{
      files: { display: string; path: string }[];
      fields: string[];
      extraChanges: { id: string; display: string }[];
    }>((resolve) => {
      resolveClaudePlan = resolve;
    });
    vi.spyOn(we2aiApi, "restorePlan").mockImplementation(async (tool) => {
      if (tool === "claude_code") {
        return claudePlanPromise;
      }
      return {
        files: [planFile("/home/u/.codex/config.toml")],
        fields: ["model_provider（移除）"],
        extraChanges: [],
      };
    });

    render(
      <ThemeProvider
        defaultTheme="system"
        storageKey="we2ai-tool-status-stale-plan-theme"
      >
        <ToolStatusBar t={t} report={reportWithBoth} />
        <Toaster />
      </ThemeProvider>,
    );

    const claude = screen.getByTestId("we2ai-tool-claude_code");
    const codex = screen.getByTestId("we2ai-tool-codex");

    // 打开 Claude Code（其 restorePlan 一直挂起不 resolve），关闭，再打开
    // Codex（立即 resolve）。
    await userEvent.click(
      within(claude).getByRole("button", { name: t.restoreOfficialAction }),
    );
    await screen.findByRole("dialog");
    await userEvent.click(screen.getByRole("button", { name: t.applyCancel }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );

    await userEvent.click(
      within(codex).getByRole("button", { name: t.restoreOfficialAction }),
    );
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText("/home/u/.codex/config.toml");

    // Claude 的响应现在才姗姗来迟——不能覆盖掉正在展示的 Codex 计划。
    resolveClaudePlan!({
      files: [planFile("/home/u/.claude/settings.json")],
      fields: ["env.ANTHROPIC_BASE_URL（移除）"],
      extraChanges: [],
    });
    await waitFor(() =>
      expect(
        within(dialog).getByText("/home/u/.codex/config.toml"),
      ).toBeInTheDocument(),
    );
    expect(
      within(dialog).queryByText("/home/u/.claude/settings.json"),
    ).not.toBeInTheDocument();
  });

  it("keeps the confirm button disabled when the restore plan fails to load", async () => {
    vi.spyOn(we2aiApi, "restorePlan").mockRejectedValue(new Error("boom"));
    render(
      <ThemeProvider
        defaultTheme="system"
        storageKey="we2ai-tool-status-plan-failed-theme"
      >
        <ToolStatusBar t={t} report={status} />
        <Toaster />
      </ThemeProvider>,
    );
    const claude = screen.getByTestId("we2ai-tool-claude_code");
    await userEvent.click(
      within(claude).getByRole("button", { name: t.restoreOfficialAction }),
    );
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText(t.applyPlanFailed);
    expect(
      within(dialog).getByRole("button", {
        name: t.restoreOfficialConfirmConfirm,
      }),
    ).toBeDisabled();
  });
});
