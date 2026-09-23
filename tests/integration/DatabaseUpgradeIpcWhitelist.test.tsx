import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, it, afterEach } from "vitest";
import { http, HttpResponse } from "msw";
import { server } from "../msw/server";
import { DatabaseUpgrade } from "@/components/DatabaseUpgrade";
import { WE2AI_MODE } from "@/config/we2ai";
import { isWe2aiIpcAllowed } from "@/we2ai/ipcWhitelist";

const TAURI_ENDPOINT = "http://tauri.local";

describe("DatabaseUpgrade IPC whitelist (WE2AI mode)", () => {
  afterEach(() => {
    server.resetHandlers();
  });

  it("only invokes IPC commands that are on the WE2AI whitelist and hides the config-dir entry", async () => {
    // 本仓库始终以 WE2AI_MODE=true 构建，见 src/config/we2ai.ts。
    expect(WE2AI_MODE).toBe(true);

    const invokedCommands: string[] = [];
    server.use(
      http.post(`${TAURI_ENDPOINT}/*`, ({ request }) => {
        const command = request.url.slice(`${TAURI_ENDPOINT}/`.length);
        invokedCommands.push(command);
        return HttpResponse.json(true);
      }),
    );

    render(<DatabaseUpgrade payload={{}} />);

    // WE2AI 模式下启动检查（check_app_update_available）被跳过，直接进入
    // upgradable，不应在挂载阶段产生任何 invoke。
    const upgradeButton = await screen.findByRole("button", {
      name: "升级应用",
    });
    expect(invokedCommands).toEqual([]);

    // we2ai: open_app_config_folder 不在白名单内，WE2AI 模式下该入口必须
    // 完全不渲染，而不只是禁用。
    expect(
      screen.queryByRole("button", { name: "打开配置目录" }),
    ).not.toBeInTheDocument();

    fireEvent.click(upgradeButton);

    await waitFor(() => {
      expect(invokedCommands).toContain("install_update_and_restart");
    });

    for (const command of invokedCommands) {
      expect(
        isWe2aiIpcAllowed(command),
        `command "${command}" invoked by DatabaseUpgrade is not in the WE2AI IPC whitelist`,
      ).toBe(true);
    }
  });
});
