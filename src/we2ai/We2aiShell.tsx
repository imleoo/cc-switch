import { useCallback, useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { toast } from "sonner";
// 故意从 "i18next" 直接拿单例，而不是 "@/i18n"：后者的顶层代码会立即执行
// i18n.init(...)（生产环境下无副作用，因为 main.tsx 早就初始化过同一个单例；
// 但测试环境下 We2aiShell 会被 App.tsx 静态 import，一旦先于测试自己的
// i18n.init(...) 跑到，就会用生产资源包重新初始化全局单例，污染其他测试用例
// 对 i18n 行为的假设）。changeLanguage 对同一个单例生效，不需要经过 "@/i18n"。
import i18n from "i18next";
import we2aiLogo from "@/assets/icons/web-logo.svg";
import { useTheme } from "@/components/theme-provider";
import { useUpdate } from "@/contexts/UpdateContext";
import { settingsApi } from "@/lib/api/settings";
import { WE2AI_WEBSITE_URL } from "@/config/we2ai";
import { we2aiApi, type We2aiSettings } from "./api";
import {
  getWe2aiStrings,
  resolveWe2aiLanguage,
  type We2aiLanguage,
} from "./strings";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { extractErrorMessage } from "@/utils/errorUtils";

/**
 * WE2AI 的极简客户端外壳：品牌顶栏 + 模型广场占位页 + 精简设置页。
 *
 * P0 阶段没有登录与模型广场，也不接触任何供应商/MCP/Skills/代理相关命令——
 * 页面上出现的每一个 `invoke()` 都必须落在方案第 6.2 节的白名单内：
 * `we2ai_get_settings`、`we2ai_save_settings`、`get_auto_launch_status`、
 * `set_auto_launch`、`set_window_theme`（经 ThemeProvider）、
 * `install_update_and_restart`、`open_external`。
 * 检查更新走 `useUpdate()` → `@tauri-apps/plugin-updater` 的 `check()`
 * （插件命令 `plugin:updater|check`，走插件分发，不经过 invoke_handler/gate，
 * 由 capabilities/default.json 的 `updater:default` 权限放行），不是我们
 * 自己的 `check_for_updates` 命令——本页面不调用后者。
 */
export function We2aiShell() {
  const { theme, setTheme } = useTheme();
  const update = useUpdate();

  const [language, setLanguage] = useState<We2aiLanguage>("zh");
  const [appVersion, setAppVersion] = useState<string>("");
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [silentStartup, setSilentStartup] = useState(false);
  const [launchOnStartup, setLaunchOnStartup] = useState(false);
  const [launchOnStartupBusy, setLaunchOnStartupBusy] = useState(false);
  const [savedSettings, setSavedSettings] = useState<We2aiSettings | null>(
    null,
  );
  const [installing, setInstalling] = useState(false);

  const t = getWe2aiStrings(language);

  useEffect(() => {
    let cancelled = false;
    void getVersion().then((v) => {
      if (!cancelled) setAppVersion(v);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [settings, autoLaunch] = await Promise.all([
          we2aiApi.getSettings(),
          settingsApi.getAutoLaunchStatus().catch(() => false),
        ]);
        if (cancelled) return;
        setSavedSettings(settings);
        setLanguage(resolveWe2aiLanguage(settings.language));
        setSilentStartup(settings.silentStartup);
        setLaunchOnStartup(autoLaunch);
      } catch (error) {
        console.error("[we2ai] failed to load settings", error);
      } finally {
        if (!cancelled) setSettingsLoaded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const persistSettings = useCallback(
    async (patch: Partial<We2aiSettings>) => {
      // 兜底默认值需与后端 AppSettings::default()（settings.rs）一致，
      // 尤其 minimizeToTrayOnClose 后端默认是 true，不是 false。
      const base: We2aiSettings = savedSettings ?? {
        language: null,
        silentStartup: false,
        useAppWindowControls: false,
        showInTray: true,
        minimizeToTrayOnClose: true,
      };
      const next: We2aiSettings = { ...base, ...patch };
      try {
        await we2aiApi.saveSettings(next);
        setSavedSettings(next);
      } catch (error) {
        toast.error(t.saveFailed, {
          description: extractErrorMessage(error) || undefined,
        });
      }
    },
    [savedSettings, t.saveFailed],
  );

  const handleLanguageChange = (value: string) => {
    const next = resolveWe2aiLanguage(value);
    setLanguage(next);
    void persistSettings({ language: value });
    // 同步 i18next 实例的内存语言状态。i18next.changeLanguage() 本身不会
    // 写 localStorage——`src/i18n/index.ts` 只在启动时读一次
    // localStorage.getItem("language") 决定初始语言，之后的持久化要显式写，
    // 与上游 useSettings.ts 的做法一致，否则下次启动 i18n 初始化读到的还是
    // 旧语言。已知限制：托盘菜单的显示语言在下次读取 settings.language 前
    // 不会立即刷新（刷新需要额外的非白名单命令），不因此扩大 IPC 白名单。
    void i18n.changeLanguage(value);
    try {
      if (typeof window !== "undefined") {
        window.localStorage.setItem("language", value);
      }
    } catch (error) {
      console.warn("[we2ai] Failed to persist language preference", error);
    }
  };

  const handleSilentStartupChange = (checked: boolean) => {
    setSilentStartup(checked);
    void persistSettings({ silentStartup: checked });
  };

  const handleLaunchOnStartupChange = async (checked: boolean) => {
    setLaunchOnStartupBusy(true);
    try {
      await settingsApi.setAutoLaunch(checked);
      setLaunchOnStartup(checked);
    } catch (error) {
      toast.error(t.saveFailed, {
        description: extractErrorMessage(error) || undefined,
      });
    } finally {
      setLaunchOnStartupBusy(false);
    }
  };

  const handleCheckForUpdates = async () => {
    try {
      await update.checkUpdate();
    } catch (error) {
      toast.error(t.checkFailed, {
        description: extractErrorMessage(error) || undefined,
      });
    }
  };

  const handleInstallUpdate = async () => {
    setInstalling(true);
    try {
      const restarting = await settingsApi.installUpdateAndRestart();
      if (!restarting) {
        // 竞态：确认更新和真正安装之间已无可用更新（install_update_and_restart
        // 返回 false），应用不会重启，需要复位按钮状态，否则会一直卡在“检查中”。
        setInstalling(false);
      }
      // restarting === true：应用即将重启，保持 installing 态直到进程退出。
    } catch (error) {
      toast.error(t.checkFailed, {
        description: extractErrorMessage(error) || undefined,
      });
      setInstalling(false);
    }
  };

  const handleOpenWebsite = () => {
    void settingsApi.openExternal(WE2AI_WEBSITE_URL);
  };

  return (
    <div className="flex h-screen w-screen flex-col bg-background text-foreground">
      <header
        className="flex h-12 shrink-0 items-center justify-between border-b px-4"
        data-tauri-drag-region
      >
        <span className="flex items-center gap-2 text-sm font-semibold tracking-wide">
          <img src={we2aiLogo} alt="" aria-hidden="true" className="h-5 w-5" />
          {t.brand}
        </span>
      </header>

      <div className="flex-1 overflow-y-auto p-6">
        <Tabs defaultValue="marketplace" className="mx-auto max-w-2xl">
          <TabsList>
            <TabsTrigger value="marketplace">{t.navMarketplace}</TabsTrigger>
            <TabsTrigger value="settings">{t.navSettings}</TabsTrigger>
          </TabsList>

          <TabsContent value="marketplace">
            <Card>
              <CardHeader>
                <CardTitle>{t.marketplaceTitle}</CardTitle>
                <CardDescription>{t.marketplaceComingSoon}</CardDescription>
              </CardHeader>
              <CardContent>
                <p className="text-sm text-muted-foreground">
                  {t.marketplaceDescription}
                </p>
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value="settings" className="space-y-4">
            <Card>
              <CardHeader>
                <CardTitle>{t.settingsTitle}</CardTitle>
              </CardHeader>
              <CardContent className="space-y-5">
                <div className="flex items-center justify-between gap-4">
                  <span className="text-sm font-medium">{t.themeLabel}</span>
                  <Select
                    value={theme}
                    onValueChange={(v) => setTheme(v as typeof theme)}
                  >
                    <SelectTrigger className="w-36">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="light">{t.themeLight}</SelectItem>
                      <SelectItem value="dark">{t.themeDark}</SelectItem>
                      <SelectItem value="system">{t.themeSystem}</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="flex items-center justify-between gap-4">
                  <span className="text-sm font-medium">{t.languageLabel}</span>
                  <Select value={language} onValueChange={handleLanguageChange}>
                    <SelectTrigger className="w-36">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="zh">简体中文</SelectItem>
                      <SelectItem value="en">English</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="flex items-center justify-between gap-4">
                  <div>
                    <div className="text-sm font-medium">
                      {t.launchOnStartupLabel}
                    </div>
                    <div className="text-xs text-muted-foreground">
                      {t.launchOnStartupDescription}
                    </div>
                  </div>
                  <Switch
                    checked={launchOnStartup}
                    disabled={launchOnStartupBusy || !settingsLoaded}
                    onCheckedChange={(checked) => {
                      void handleLaunchOnStartupChange(checked);
                    }}
                  />
                </div>

                <div className="flex items-center justify-between gap-4">
                  <div>
                    <div className="text-sm font-medium">
                      {t.silentStartupLabel}
                    </div>
                    <div className="text-xs text-muted-foreground">
                      {t.silentStartupDescription}
                    </div>
                  </div>
                  <Switch
                    checked={silentStartup}
                    disabled={!settingsLoaded}
                    onCheckedChange={handleSilentStartupChange}
                  />
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <img src={we2aiLogo} alt={t.brand} className="h-5 w-5" />
                  {t.aboutTitle}
                </CardTitle>
                <CardDescription>
                  {t.versionLabel} {appVersion ? `v${appVersion}` : "…"}
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-wrap items-center gap-3">
                <Button variant="outline" onClick={handleOpenWebsite}>
                  {t.officialWebsite}
                </Button>

                {update.hasUpdate ? (
                  <Button onClick={handleInstallUpdate} disabled={installing}>
                    {installing
                      ? t.checking
                      : `${t.updateAvailable}${
                          update.updateInfo?.availableVersion
                            ? ` v${update.updateInfo.availableVersion}`
                            : ""
                        } · ${t.installAndRestart}`}
                  </Button>
                ) : (
                  <Button
                    variant="outline"
                    onClick={handleCheckForUpdates}
                    disabled={update.isChecking}
                  >
                    {update.isChecking ? t.checking : t.checkForUpdates}
                  </Button>
                )}

                {!update.isChecking &&
                  !update.hasUpdate &&
                  update.updateInfo === null && (
                    <span className="text-xs text-muted-foreground">
                      {t.upToDate}
                    </span>
                  )}
              </CardContent>
            </Card>
          </TabsContent>
        </Tabs>
      </div>
    </div>
  );
}

export default We2aiShell;
