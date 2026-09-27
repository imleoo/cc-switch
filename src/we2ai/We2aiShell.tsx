import { useCallback, useEffect, useRef, useState } from "react";
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
import { DRAG_REGION_ATTR, isMac } from "@/lib/platform";
import "./we2ai-theme.css";
import {
  resolveCcSwitchRunning,
  we2aiApi,
  type We2aiCcSwitchRunningStatus,
  type We2aiSessionSummary,
  type We2aiSettings,
  type We2aiToolStatusReport,
} from "./api";
import { LoginPage, notifyLogoutOutcome } from "./LoginPage";
import { ModelSquarePage } from "./ModelSquarePage";
import { ToolStatusBar } from "./ToolStatusBar";
import {
  getWe2aiStrings,
  formatWe2aiString,
  resolveWe2aiLanguage,
  type We2aiLanguage,
} from "./strings";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Checkbox } from "@/components/ui/checkbox";
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { extractErrorMessage } from "@/utils/errorUtils";

/**
 * WE2AI 的极简客户端外壳：登录页（未登录时）+ 品牌顶栏 + 模型广场占位页 +
 * 精简设置页（已登录时）。
 *
 * 不接触任何供应商/MCP/Skills/代理相关命令——页面上出现的每一个 `invoke()`
 * 都必须落在方案第 6.2 节的白名单内：全部 `we2ai_*` 命令（含 P2 新增的登录
 * 会话命令）、`get_auto_launch_status`、`set_auto_launch`、`set_window_theme`
 * （经 ThemeProvider）、`install_update_and_restart`、`open_external`。
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

  // 登录会话（方案第 5.1、5.2 节）。`sessionChecked` 为 false 时既不是"已登录"
  // 也不是"未登录"，是"还不知道"——避免闪一下登录页再跳走。
  const [session, setSession] = useState<We2aiSessionSummary | null>(null);
  const [sessionChecked, setSessionChecked] = useState(false);
  const [logoutDialogOpen, setLogoutDialogOpen] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const [restoreOfficialOnLogout, setRestoreOfficialOnLogout] = useState(false);
  const [retryingOffline, setRetryingOffline] = useState(false);

  const t = getWe2aiStrings(language);

  const LOGGED_OUT_SUMMARY: We2aiSessionSummary = {
    loggedIn: false,
    region: null,
    emailMasked: null,
    keyringDegraded: false,
    indexDegraded: false,
    offlineRetryInSeconds: null,
    localCleanupPending: false,
  };

  // 断网启动恢复时（`ResumeOutcome::OfflineRetained`）：Rust 侧已经把会话
  // 原样保留在内存里并启动了后台指数退避重试循环，`offlineRetryInSeconds`
  // 非 `null` 就是"正在离线重试"，界面仍然按"已登录"渲染、不回登录页
  // （方案第 5.2 节"断网不回登录页"，Codex 代码评审高危项 5）。
  const offline = session?.offlineRetryInSeconds != null;

  // Codex 代码评审第 5 轮中危项 5：`commit_lock` 持锁写钥匙串期间不加超时
  // 强行放锁（真实系统钥匙串弹出授权对话框时，超时放锁反而会让并发的另一
  // 次提交在钥匙串还没写完时就抢着继续，产生新的竞态；这个取舍已经登记在
  // `自定义开发功能列表.md` 功能 9）。前端能做的只是在等待明显变长时告诉
  // 用户"发生了什么"，而不是假装很快就会有结果。
  const withKeyringWaitHint = useCallback(
    <T,>(promise: Promise<T>): Promise<T> => {
      const timer = window.setTimeout(() => {
        toast.message(t.waitingForKeyringAuthorization);
      }, 10000);
      return promise.finally(() => window.clearTimeout(timer));
    },
    [t],
  );

  const refreshSessionStatus = useCallback(async () => {
    try {
      const summary = await we2aiApi.sessionStatus();
      setSession(summary);
    } catch (error) {
      console.error("[we2ai] failed to load session status", error);
      setSession(LOGGED_OUT_SUMMARY);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const lastRegion = (await we2aiApi.getLastRegion()) ?? "international";
        await withKeyringWaitHint(we2aiApi.resumeSession(lastRegion));
        if (!cancelled) await refreshSessionStatus();
      } catch (error) {
        console.error("[we2ai] failed to resume session", error);
        if (!cancelled) setSession(LOGGED_OUT_SUMMARY);
      } finally {
        if (!cancelled) setSessionChecked(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [refreshSessionStatus, withKeyringWaitHint]);

  const handleLoginSuccess = useCallback(() => {
    void refreshSessionStatus();
  }, [refreshSessionStatus]);

  // 轮询专用：与 `refreshSessionStatus` 的区别是失败时保留当前（已登录/
  // 离线重试中）状态，而不是把用户误踢回登录页——轮询只是每秒查一次本地
  // 状态，一次 IPC 抖动/瞬时失败不代表会话真的失效，真正的终止会话判断
  // 由 Rust 侧的 `SessionManager` 负责（Codex 代码评审第 3 轮低危项 2）。
  const pollSessionStatus = useCallback(async () => {
    try {
      const summary = await we2aiApi.sessionStatus();
      setSession(summary);
    } catch (error) {
      console.error("[we2ai] failed to poll session status", error);
    }
  }, []);

  // 离线倒计时展示（Codex 代码评审中危项 3：自动退避重试 + 前端倒计时）：
  // Rust 侧的后台重试循环独立于前端运行，这里每秒轮询一次
  // `we2ai_session_status`（纯本地状态查询，不产生网络请求）刷新倒计时与
  // "是否已经自动恢复"。标签页切到后台时（`document.hidden`）暂停轮询，
  // 切回前台立即补一次并恢复轮询——不需要在用户看不到界面时持续发起 IPC
  // 调用（Codex 代码评审第 3 轮低危项 2）。
  useEffect(() => {
    if (!offline) return;
    let timer: number | null = null;
    const startPolling = () => {
      if (timer != null) return;
      timer = window.setInterval(() => {
        void pollSessionStatus();
      }, 1000);
    };
    const stopPolling = () => {
      if (timer != null) {
        window.clearInterval(timer);
        timer = null;
      }
    };
    const onVisibilityChange = () => {
      if (document.hidden) {
        stopPolling();
      } else {
        void pollSessionStatus();
        startPolling();
      }
    };
    if (!document.hidden) {
      startPolling();
    }
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      stopPolling();
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [offline, pollSessionStatus]);

  const handleRetryNow = useCallback(async () => {
    setRetryingOffline(true);
    try {
      await withKeyringWaitHint(we2aiApi.retryNow());
      await refreshSessionStatus();
    } catch (error) {
      console.error("[we2ai] failed to retry now", error);
    } finally {
      setRetryingOffline(false);
    }
  }, [refreshSessionStatus, withKeyringWaitHint]);

  // 窗口获得焦点、或浏览器判定网络恢复时，立即重试一次，不用等后台退避
  // 循环的下一次定时唤醒。
  useEffect(() => {
    if (!offline) return;
    const onFocusOrOnline = () => {
      void handleRetryNow();
    };
    window.addEventListener("focus", onFocusOrOnline);
    window.addEventListener("online", onFocusOrOnline);
    return () => {
      window.removeEventListener("focus", onFocusOrOnline);
      window.removeEventListener("online", onFocusOrOnline);
    };
  }, [offline, handleRetryNow]);

  // P6：登出弹窗勾选"同时恢复工具的官方配置"时，登出成功后执行——对全部
  // 三个工具调用 restoreOfficial，移除 WE2AI 写入的一切（取代旧版只删 Key
  // 的 removeToolKeys）。
  const restoreOfficialForAllTools = async () => {
    try {
      const result = await we2aiApi.restoreOfficial([
        "claude_code",
        "codex",
        "workbuddy",
      ]);
      if (result.restored.length > 0) {
        toast.success(
          formatWe2aiString(t.toolsRestored, {
            count: result.restored.length,
          }),
        );
      }
      if (result.skipped.length > 0) {
        toast.warning(t.toolsRestoreSkipped, {
          description: result.skipped.join("\n"),
        });
      }
      // 三个工具里可能只指定过一个，其余两个"未指向 WE2AI"是正常情况，不是
      // 失败——只在既没有恢复成功、也没有真失败时才用中性提示（Opus 复核
      // 高危项 1：不能让这种正常情况弹出警告 toast）。
      if (result.restored.length === 0 && result.skipped.length === 0) {
        toast.info(t.toolsRestoreNoop);
      }
    } catch (error) {
      toast.error(t.toolsRestoreFailed, {
        description: extractErrorMessage(error) || undefined,
      });
    }
  };

  const handleConfirmLogout = async () => {
    setLoggingOut(true);
    try {
      const outcome = await withKeyringWaitHint(we2aiApi.logout());
      // 勾选了恢复官方配置：只要会话已登出（含本机清理失败的情形）就执行，
      // 不能因后续走重试流程而丢掉用户的选择（Fable P5 增量终验中危项）。
      if (restoreOfficialOnLogout && outcome !== "notLoggedIn") {
        await restoreOfficialForAllTools();
      }
      if (outcome === "localCleanupFailed") {
        // Codex 代码评审第 5 轮高危项 3：本地清理两项都没能成功，不能保证
        // 重启不会恢复这个会话——不能表现成"已经退出"，界面继续保持已登录
        // 状态，提示清理失败并允许用户重试。
        toast.error(t.logoutCleanupFailed, {
          action: {
            label: t.offlineRetry,
            onClick: () => void handleRetryLocalCleanup(),
          },
        });
      } else {
        notifyLogoutOutcome(t, outcome);
        // 按真实摘要渲染：其他区域可能仍有待清理残留（localCleanupPending），
        // 固定写入"未登录"摘要会把提示与重试入口隐藏（Codex 验收第 7 轮高危项 2）。
        await refreshSessionStatus();
      }
    } catch (error) {
      console.error("[we2ai] logout failed", error);
      toast.error(t.logoutFailed, {
        description: extractErrorMessage(error) || undefined,
      });
    } finally {
      setLoggingOut(false);
      setLogoutDialogOpen(false);
      setRestoreOfficialOnLogout(false);
    }
  };

  // Codex 代码评审第 6 轮高危项 3：重试必须调用专门的
  // `we2ai_retry_local_cleanup`，只重试本地清理（索引 + 钥匙串），不能再走
  // `handleConfirmLogout` → `we2ai_logout`——此时会话已经没有可用的
  // refresh token，重新发起远端登出请求毫无意义。重试仍失败时保持"待清理"
  // 界面并允许再次重试；成功则和普通登出一样清空前端会话状态。
  const handleRetryLocalCleanup = async () => {
    setLoggingOut(true);
    try {
      const outcome = await withKeyringWaitHint(we2aiApi.retryLocalCleanup());
      if (outcome === "localCleanupFailed") {
        toast.error(t.logoutCleanupFailed, {
          action: {
            label: t.offlineRetry,
            onClick: () => void handleRetryLocalCleanup(),
          },
        });
      } else {
        notifyLogoutOutcome(t, outcome);
        // 清理的是之前登出/终止留下的残留，可能与当前活跃会话（其他区域）
        // 无关：重新读取真实状态，而不是强行切到未登录（Codex 验收第 6 轮）。
        await refreshSessionStatus();
      }
    } catch (error) {
      console.error("[we2ai] retry local cleanup failed", error);
      toast.error(t.logoutFailed, {
        description: extractErrorMessage(error) || undefined,
      });
    } finally {
      setLoggingOut(false);
    }
  };

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
      // 原始错误（如 updater 插件的 "Could not fetch a valid release JSON
      // from the remote"）只写 console：它是英文、面向开发者的底层措辞，
      // 直接展示给用户不友好，也不会随界面语言切换。toast 描述改用固定的
      // 本地化提示，用户可操作的信息只有"稍后重试"。
      console.error("[we2ai] check for updates failed", error);
      toast.error(t.checkFailed, {
        description: t.checkFailedHint,
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

  // 顶栏工具状态（方案第 1、4.4 节）：登录后与每次写入成功后刷新。
  const [toolStatus, setToolStatus] = useState<We2aiToolStatusReport | null>(
    null,
  );
  // 只采纳最后一次工具状态请求的结果（Opus 复核低危项 L5）：现在打开确认
  // 弹窗前也会触发一次刷新（`onBeforeApplyDialogOpen`），与登录后、写入
  // 成功后的刷新可能连续触发，较晚发出但更早返回的请求不能被更早发出、
  // 但更晚返回的过期响应覆盖——与 `ModelSquarePage.tsx` 的
  // `modelsRequestSeq`/`keysRequestSeq` 同一模式。
  const toolStatusRequestSeq = useRef(0);
  const refreshToolStatus = useCallback(async () => {
    const seq = ++toolStatusRequestSeq.current;
    try {
      const report = await we2aiApi.toolStatus();
      if (seq === toolStatusRequestSeq.current) {
        setToolStatus(report);
      }
    } catch {
      // 检测失败不影响使用，保留上一次结果。
    }
  }, []);
  // 快速检测结果存独立 state，不依赖 `toolStatus`（Codex 验收 Z1）：此前
  // 直接 `setToolStatus((prev) => prev ? {...} : prev)`——首次登录后完整
  // 检测仍在等 npm（`toolStatus` 还是 `null`）时，快速检测就算查到 CC
  // Switch 正在运行，结果也会被 `prev` 为 `null` 这个分支悄悄丢弃，函数
  // 却仍然返回 `true`（"检测完成"），弹窗因此既无并存警告也无"未能完成
  // 检测"提示。加序号丢弃过期结果：关闭重开弹窗会再触发一次这个回调，旧的
  // （更慢的）请求不能在新请求之后才返回并覆盖它。
  const [quickCcSwitchCheck, setQuickCcSwitchCheck] = useState<{
    seq: number;
    status: We2aiCcSwitchRunningStatus;
  } | null>(null);
  const quickCcSwitchCheckSeq = useRef(0);
  const loggedInIdentity = session?.loggedIn
    ? `${session.region ?? ""}:${session.emailMasked ?? ""}`
    : null;
  useEffect(() => {
    if (loggedInIdentity) {
      void refreshToolStatus();
    } else {
      setToolStatus(null);
    }
    // 登出/切换账号（`loggedInIdentity` 变化）时快速检测的旧结果同样要
    // 清空（Codex 验收 W4）：不然新会话第一次打开确认弹窗，可能还短暂
    // 展示着上一个账号/上一次登录期间残留的检测结果。同时让序号"作废"
    // （递增而不是清零，避免与旧序号 0 巧合相等），任何还在途的旧请求
    // 即使这时才返回也不会再被采纳。
    quickCcSwitchCheckSeq.current += 1;
    setQuickCcSwitchCheck(null);
  }, [loggedInIdentity, refreshToolStatus]);

  // apply 前只等"CC Switch 是否在运行"这一项快速检测（Codex 验收 Y1）：
  // 此前这里直接等待完整的 `refreshToolStatus()`——它连带调用
  // `get_tool_versions`，每次都无缓存联网查询 npm 最新版本，单次超时
  // `LATEST_PROBE_TIMEOUT`=15s（`commands/misc.rs`），国内网络下可能让确认
  // 按钮被禁用近 30 秒。改为只等 `ccSwitchRunningQuick()`（Rust 侧本身已有
  // 子进程级超时），并在这里再加一层更短的客户端超时兜底——防的是极端情况
  // 下 IPC 往返本身异常缓慢，而不是重复子进程那层超时；超时按"未能完成
  // 检测"处理并放行（返回 `false`），不无限期挡住确认按钮。完整的顶栏刷新
  // 仍然异步、独立进行，不等待也不阻塞这次调用。
  const APPLY_QUICK_CHECK_TIMEOUT_MS = 2500;
  const checkCcSwitchRunningBeforeApply = useCallback(async (): Promise<boolean> => {
    void refreshToolStatus();
    const seq = ++quickCcSwitchCheckSeq.current;
    let timeoutHandle: ReturnType<typeof setTimeout> | undefined;
    try {
      const status = await Promise.race([
        we2aiApi.ccSwitchRunningQuick(),
        new Promise<never>((_, reject) => {
          timeoutHandle = setTimeout(
            () => reject(new Error("we2ai: quick tool-status check timed out")),
            APPLY_QUICK_CHECK_TIMEOUT_MS,
          );
        }),
      ]);
      if (seq === quickCcSwitchCheckSeq.current) {
        setQuickCcSwitchCheck({ seq, status });
      }
      // "unknown"（Codex 验收 Z2）：检测子进程启动失败/非零退出等，没能
      // 得出结论，跟超时一样按"未能完成检测"处理，不能当成"确认完成"。
      return status !== "unknown";
    } catch {
      if (seq === quickCcSwitchCheckSeq.current) {
        setQuickCcSwitchCheck({ seq, status: "unknown" });
      }
      return false;
    } finally {
      if (timeoutHandle) clearTimeout(timeoutHandle);
    }
  }, [refreshToolStatus]);

  // macOS 用 `titleBarStyle: "Overlay"`（`src-tauri/tauri.conf.json`），红绿灯
  // 悬浮在内容之上、不占布局空间：顶栏需要预留左侧空间，否则 logo/文字会被
  // 红绿灯遮住。其他平台没有这个问题，不需要额外留白。
  const macDragPadding = isMac() ? "pl-20" : "pl-4";

  // 全部 hooks 已在上方无条件声明完毕，以下按会话状态分支渲染不同视图，不再
  // 有新的 hook 调用——不违反 hooks 调用顺序规则。
  if (!sessionChecked) {
    return (
      <div className="we2ai-theme flex h-screen w-screen items-center justify-center">
        {isMac() && (
          <div
            className="fixed inset-x-0 top-0 h-8"
            {...DRAG_REGION_ATTR}
            aria-hidden="true"
          />
        )}
        <img
          src={we2aiLogo}
          alt=""
          aria-hidden="true"
          className="h-8 w-8 animate-pulse"
        />
      </div>
    );
  }

  if (!session?.loggedIn) {
    if (session?.localCleanupPending) {
      // 登出或会话终止时本机凭据没清掉：重启可能恢复旧会话，登录页上方给出
      // 提示与重试入口，不能静默（Codex 验收第 5 轮高危项 2）。
      return (
        <div className="we2ai-theme flex h-screen w-screen flex-col">
          <div
            role="alert"
            className="flex items-center justify-between gap-3 border-b-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] px-4 py-2 text-sm font-medium text-[var(--we2ai-paper)]"
          >
            <span>{t.localCleanupPendingBanner}</span>
            <Button
              size="sm"
              variant="outline"
              disabled={loggingOut}
              onClick={() => void handleRetryLocalCleanup()}
              className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
            >
              {t.offlineRetry}
            </Button>
          </div>
          <div className="min-h-0 flex-1">
            <LoginPage t={t} onLoginSuccess={handleLoginSuccess} />
          </div>
        </div>
      );
    }
    return <LoginPage t={t} onLoginSuccess={handleLoginSuccess} />;
  }

  return (
    <div className="we2ai-theme flex h-screen w-screen flex-col">
      <header
        className={`flex h-14 shrink-0 items-center justify-between border-b-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] pr-4 ${macDragPadding}`}
        data-tauri-drag-region
      >
        <span className="we2ai-heading flex items-center gap-2 text-base">
          <img src={we2aiLogo} alt="" aria-hidden="true" className="h-5 w-5" />
          {t.brand}
        </span>
        <div
          className="flex items-center gap-3"
          style={{ WebkitAppRegion: "no-drag" } as any}
        >
          {session.emailMasked && (
            <span className="we2ai-label">
              {formatWe2aiString(t.loggedInAs, { email: session.emailMasked })}
            </span>
          )}
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setLogoutDialogOpen(true)}
            className="rounded-lg border-transparent px-3 shadow-none hover:bg-[var(--we2ai-paper-2)]"
          >
            {t.logoutButton}
          </Button>
        </div>
      </header>

      <ToolStatusBar
        t={t}
        // Codex 验收 V2：顶栏与确认弹窗必须展示一致的"CC Switch 是否在
        // 运行"——复用同一份 `resolveCcSwitchRunning`（快速检测取得确定
        // 结果时优先，`unknown`/尚无结果时回退完整报告），不要各写一套。
        // `toolStatus` 为 `null` 时（完整报告还没加载）`ToolStatusBar`
        // 本身就整体不渲染，这里不需要特殊处理。
        report={
          toolStatus
            ? {
                ...toolStatus,
                ccSwitchRunning: resolveCcSwitchRunning(
                  quickCcSwitchCheck?.status,
                  toolStatus.ccSwitchRunning,
                ),
              }
            : null
        }
        onRestored={() => void refreshToolStatus()}
      />

      <Dialog open={logoutDialogOpen} onOpenChange={setLogoutDialogOpen}>
        <DialogContent className="we2ai-theme rounded-none border-[2.5px] border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-[8px_8px_0_0_var(--we2ai-ink)]">
          <DialogHeader className="border-b-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <DialogTitle className="we2ai-heading">
              {t.logoutConfirmTitle}
            </DialogTitle>
            <DialogDescription>{t.logoutConfirmDescription}</DialogDescription>
          </DialogHeader>
          <label className="flex items-center gap-2 px-6 text-sm">
            <Checkbox
              checked={restoreOfficialOnLogout}
              disabled={loggingOut}
              onCheckedChange={(checked) =>
                setRestoreOfficialOnLogout(checked === true)
              }
              className="rounded-sm border-2 border-[var(--we2ai-ink)]"
            />
            {t.logoutRestoreOfficial}
          </label>
          <DialogFooter className="border-t-[2.5px] border-[var(--we2ai-ink)] bg-transparent">
            <Button
              variant="outline"
              onClick={() => setLogoutDialogOpen(false)}
              disabled={loggingOut}
              className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
            >
              {t.logoutConfirmCancel}
            </Button>
            <Button
              variant="destructive"
              onClick={() => void handleConfirmLogout()}
              disabled={loggingOut}
              className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-ink)]"
            >
              {t.logoutConfirmConfirm}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {offline && (
        <div className="flex items-center justify-between gap-3 border-b-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-4 py-2 text-xs text-[var(--we2ai-ink)]">
          <span>
            {t.offlineBanner}
            {typeof session?.offlineRetryInSeconds === "number" &&
              " · " +
                formatWe2aiString(t.offlineRetryCountdown, {
                  seconds: session.offlineRetryInSeconds,
                })}
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={retryingOffline}
            onClick={() => void handleRetryNow()}
            className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
          >
            {retryingOffline ? t.offlineRetrying : t.offlineRetry}
          </Button>
        </div>
      )}

      {session.keyringDegraded && (
        <div className="border-b-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-4 py-2 text-xs text-[var(--we2ai-ink)]">
          {t.keyringDegradedWarning}
        </div>
      )}

      {/* Codex 代码评审第 3 轮高危项 2：会话索引写入失败时的独立提示，与
          钥匙串退化分开展示——原因不同（索引写失败 vs 钥匙串不可用），
          文案不应该互相混淆。 */}
      {!session.keyringDegraded && session.indexDegraded && (
        <div className="border-b-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper-2)] px-4 py-2 text-xs text-[var(--we2ai-ink)]">
          {t.sessionNotPersistableWarning}
        </div>
      )}

      {/* 其他区域此前登出/终止时本机凭据未能清除（待清理残留独立于当前会话），
          当前登录另一区域时也要提示并允许重试（Codex 验收第 6 轮高危项 2）。 */}
      {session.localCleanupPending && (
        <div
          role="alert"
          className="flex items-center justify-between gap-3 border-b-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-orange)] px-4 py-2 text-xs font-medium text-[var(--we2ai-paper)]"
        >
          <span>{t.localCleanupPendingBanner}</span>
          <Button
            size="sm"
            variant="outline"
            disabled={loggingOut}
            onClick={() => void handleRetryLocalCleanup()}
            className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
          >
            {t.offlineRetry}
          </Button>
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-6">
        <Tabs defaultValue="marketplace" className="mx-auto max-w-3xl">
          <TabsList className="rounded-none border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] p-0">
            <TabsTrigger
              value="marketplace"
              className="we2ai-label rounded-none border-0 data-[state=active]:bg-[var(--we2ai-ink)] data-[state=active]:text-[var(--we2ai-paper)] data-[state=active]:shadow-none data-[state=inactive]:bg-transparent data-[state=inactive]:text-[var(--we2ai-ink)] data-[state=inactive]:opacity-100 data-[state=inactive]:hover:bg-[var(--we2ai-paper-2)]"
            >
              {t.navMarketplace}
            </TabsTrigger>
            <TabsTrigger
              value="settings"
              className="we2ai-label rounded-none border-0 border-l-2 border-[var(--we2ai-ink)] data-[state=active]:bg-[var(--we2ai-ink)] data-[state=active]:text-[var(--we2ai-paper)] data-[state=active]:shadow-none data-[state=inactive]:bg-transparent data-[state=inactive]:text-[var(--we2ai-ink)] data-[state=inactive]:opacity-100 data-[state=inactive]:hover:bg-[var(--we2ai-paper-2)]"
            >
              {t.navSettings}
            </TabsTrigger>
          </TabsList>

          <TabsContent value="marketplace">
            <Card className="we2ai-panel">
              <CardHeader className="border-b-2 border-[var(--we2ai-ink)]">
                <CardTitle className="we2ai-heading text-xl">
                  {t.marketplaceTitle}
                </CardTitle>
                <CardDescription className="text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
                  {t.marketplaceDescription}
                </CardDescription>
              </CardHeader>
              <CardContent className="pt-6">
                {/* 以会话身份作 key：换账号或换区域后整页重建，不沿用上个
                    会话的 Key 列表与选择。 */}
                <ModelSquarePage
                  key={`${session.region ?? ""}:${session.emailMasked ?? ""}`}
                  t={t}
                  onSessionMaybeEnded={() => void refreshSessionStatus()}
                  toolStatus={toolStatus}
                  quickCcSwitchStatus={quickCcSwitchCheck?.status ?? null}
                  onApplied={() => void refreshToolStatus()}
                  onBeforeApplyDialogOpen={checkCcSwitchRunningBeforeApply}
                />
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value="settings" className="space-y-4">
            <Card className="we2ai-panel">
              <CardHeader className="border-b-2 border-[var(--we2ai-ink)]">
                <CardTitle className="we2ai-heading text-xl">
                  {t.settingsTitle}
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-5 pt-6">
                <div className="flex items-center justify-between gap-4">
                  <span className="we2ai-label">{t.themeLabel}</span>
                  <Select
                    value={theme}
                    onValueChange={(v) => setTheme(v as typeof theme)}
                  >
                    <SelectTrigger className="w-36 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
                      <SelectItem value="light">{t.themeLight}</SelectItem>
                      <SelectItem value="dark">{t.themeDark}</SelectItem>
                      <SelectItem value="system">{t.themeSystem}</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="flex items-center justify-between gap-4">
                  <span className="we2ai-label">{t.languageLabel}</span>
                  <Select value={language} onValueChange={handleLanguageChange}>
                    <SelectTrigger className="w-36 rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent className="we2ai-theme rounded-lg border-2 border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)]">
                      <SelectItem value="zh">简体中文</SelectItem>
                      <SelectItem value="en">English</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="flex items-center justify-between gap-4">
                  <div>
                    <div className="we2ai-label">{t.launchOnStartupLabel}</div>
                    <div className="mt-1 text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
                      {t.launchOnStartupDescription}
                    </div>
                  </div>
                  <Switch
                    checked={launchOnStartup}
                    disabled={launchOnStartupBusy || !settingsLoaded}
                    onCheckedChange={(checked) => {
                      void handleLaunchOnStartupChange(checked);
                    }}
                    className="border-2 border-[var(--we2ai-ink)] data-[state=checked]:bg-[var(--we2ai-orange)] data-[state=unchecked]:bg-[var(--we2ai-paper-2)]"
                  />
                </div>

                <div className="flex items-center justify-between gap-4">
                  <div>
                    <div className="we2ai-label">{t.silentStartupLabel}</div>
                    <div className="mt-1 text-xs text-[color:color-mix(in_srgb,var(--we2ai-ink)_60%,transparent)]">
                      {t.silentStartupDescription}
                    </div>
                  </div>
                  <Switch
                    checked={silentStartup}
                    disabled={!settingsLoaded}
                    onCheckedChange={handleSilentStartupChange}
                    className="border-2 border-[var(--we2ai-ink)] data-[state=checked]:bg-[var(--we2ai-orange)] data-[state=unchecked]:bg-[var(--we2ai-paper-2)]"
                  />
                </div>
              </CardContent>
            </Card>

            <Card className="we2ai-panel">
              <CardHeader className="border-b-2 border-[var(--we2ai-ink)]">
                <CardTitle className="we2ai-heading flex items-center gap-2 text-xl">
                  <img src={we2aiLogo} alt={t.brand} className="h-5 w-5" />
                  {t.aboutTitle}
                </CardTitle>
                <CardDescription className="text-[color:color-mix(in_srgb,var(--we2ai-ink)_70%,transparent)]">
                  {t.versionLabel} {appVersion ? `v${appVersion}` : "…"}
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-wrap items-center gap-3 pt-6">
                <Button
                  variant="outline"
                  onClick={handleOpenWebsite}
                  className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
                >
                  {t.officialWebsite}
                </Button>

                {update.hasUpdate ? (
                  <Button
                    onClick={handleInstallUpdate}
                    disabled={installing}
                    className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-ink)] text-[var(--we2ai-paper)] shadow-none hover:bg-[var(--we2ai-orange)]"
                  >
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
                    className="rounded-lg border-[var(--we2ai-ink)] bg-[var(--we2ai-paper)] text-[var(--we2ai-ink)] shadow-none hover:bg-[var(--we2ai-paper-2)]"
                  >
                    {update.isChecking ? t.checking : t.checkForUpdates}
                  </Button>
                )}

                {!update.isChecking &&
                  !update.hasUpdate &&
                  (update.error ? (
                    // 失败分支直接看 `update.error`，不依赖 `updateInfo`
                    // （Opus 复核 P9）：`updateInfo` 只在成功查到新版本时
                    // 才写入，失败不会清空它——"先查到更新、再检查失败"
                    // 这种顺序下 `updateInfo` 仍然是上一次的非 null 值，
                    // 若继续要求 `updateInfo === null` 才判定失败，会让
                    // 这种场景既不显示"已是最新版本"（因为 hasUpdate 已被
                    // 重置为 false）也不显示"检查失败"，状态区整个消失。
                    <span
                      className="we2ai-label text-[var(--we2ai-orange)]"
                      role="status"
                      data-testid="we2ai-update-check-failed"
                    >
                      {t.checkFailed}
                    </span>
                  ) : (
                    <span className="we2ai-label opacity-60">
                      {t.upToDate}
                    </span>
                  ))}
              </CardContent>
            </Card>
          </TabsContent>
        </Tabs>
      </div>
    </div>
  );
}

export default We2aiShell;
