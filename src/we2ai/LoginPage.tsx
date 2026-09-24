import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import we2aiLogo from "@/assets/icons/web-logo.svg";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  formatWe2aiString,
  getWe2aiErrorMessage,
  type We2aiStrings,
} from "./strings";
import { isWe2aiApiError, we2aiApi, type We2aiRegion } from "./api";

/** 手机登录只在国内区域展示（方案第 0 节决策 2：国内版手机验证码登录）。 */
function isDomesticRegion(region: We2aiRegion): boolean {
  return region === "domestic_prod" || region === "domestic_dev";
}

function regionLabel(t: We2aiStrings, region: We2aiRegion): string {
  switch (region) {
    case "international":
      return t.regionInternational;
    case "domestic_prod":
      return t.regionDomesticProd;
    case "domestic_dev":
      return t.regionDomesticDev;
  }
}

/** 从一次 `invoke()` rejection 中提取展示用错误文案。 */
function describeError(
  t: We2aiStrings,
  error: unknown,
): { code: string | null; message: string } {
  if (isWe2aiApiError(error)) {
    return { code: error.code, message: getWe2aiErrorMessage(t, error.code) };
  }
  return { code: null, message: t.errorNetwork };
}

const SMS_RESEND_SECONDS = 60;

interface LoginPageProps {
  t: We2aiStrings;
  onLoginSuccess: () => void;
}

type LoginStage =
  | { kind: "credentials" }
  | { kind: "twoFa"; tempToken: string; emailMasked: string };

export function LoginPage({ t, onLoginSuccess }: LoginPageProps) {
  const [regions, setRegions] = useState<We2aiRegion[]>(["international"]);
  const [region, setRegion] = useState<We2aiRegion>("international");
  const [regionsLoaded, setRegionsLoaded] = useState(false);
  const [regionSwitchBusy, setRegionSwitchBusy] = useState(false);

  const [tab, setTab] = useState<"email" | "phone">("email");
  const [stage, setStage] = useState<LoginStage>({ kind: "credentials" });

  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [totpCode, setTotpCode] = useState("");
  const [phone, setPhone] = useState("");
  const [smsCode, setSmsCode] = useState("");
  const [resendCountdown, setResendCountdown] = useState(0);

  const [emailBusy, setEmailBusy] = useState(false);
  const [twoFaBusy, setTwoFaBusy] = useState(false);
  const [sendCodeBusy, setSendCodeBusy] = useState(false);
  const [phoneLoginBusy, setPhoneLoginBusy] = useState(false);

  const [errorCode, setErrorCode] = useState<string | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  const countdownTimer = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [available, last] = await Promise.all([
          we2aiApi.getAvailableRegions(),
          we2aiApi.getLastRegion(),
        ]);
        if (cancelled) return;
        const list =
          available.length > 0
            ? available
            : (["international"] as We2aiRegion[]);
        setRegions(list);
        const initial = last && list.includes(last) ? last : list[0];
        setRegion(initial);
      } catch (error) {
        console.error("[we2ai] failed to load region list", error);
      } finally {
        if (!cancelled) setRegionsLoaded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!isDomesticRegion(region) && tab === "phone") {
      setTab("email");
    }
  }, [region, tab]);

  useEffect(() => {
    return () => {
      if (countdownTimer.current) clearInterval(countdownTimer.current);
    };
  }, []);

  const handleRegionChange = (value: string) => {
    const next = value as We2aiRegion;
    setRegion(next);
    setErrorCode(null);
    setErrorMessage(null);
    setStage({ kind: "credentials" });
    void we2aiApi.setLastRegion(next).catch((error) => {
      // 写入失败不阻断本次切换，但要让用户知道下次启动可能回到旧区域
      // （Codex 代码评审第 6 轮中危项 3：此前后端吞掉错误，这里永远不触发）。
      console.warn("[we2ai] failed to persist last region", error);
      toast.error(t.lastRegionSaveFailed);
    });
    // 切区域时该区域可能已经有一份钥匙串里保存的会话（此前登录过、切走又
    // 切回来，或者本次是应用启动后第一次在登录页里切换区域）：尝试静默
    // 恢复，成功（含离线保留）则直接进入已登录界面，不需要用户重新输入
    // 邮箱密码。
    setRegionSwitchBusy(true);
    void we2aiApi
      .resumeSession(next)
      .then((outcome) => {
        if (outcome === "restored" || outcome === "offlineRetained") {
          onLoginSuccess();
        }
      })
      .catch((error) => {
        console.warn(
          "[we2ai] failed to resume session for the new region",
          error,
        );
      })
      .finally(() => {
        setRegionSwitchBusy(false);
      });
  };

  const showError = (error: unknown) => {
    const { code, message } = describeError(t, error);
    setErrorCode(code);
    setErrorMessage(message);
  };

  const handleEmailLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setErrorCode(null);
    setErrorMessage(null);
    setEmailBusy(true);
    try {
      const outcome = await we2aiApi.loginEmail(region, email.trim(), password);
      if (outcome.kind === "loggedIn") {
        onLoginSuccess();
      } else {
        setStage({
          kind: "twoFa",
          tempToken: outcome.tempToken,
          emailMasked: outcome.emailMasked,
        });
      }
    } catch (error) {
      showError(error);
    } finally {
      setEmailBusy(false);
    }
  };

  const handleTwoFaSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (stage.kind !== "twoFa") return;
    setErrorCode(null);
    setErrorMessage(null);
    setTwoFaBusy(true);
    try {
      await we2aiApi.login2fa(region, stage.tempToken, totpCode.trim());
      onLoginSuccess();
    } catch (error) {
      showError(error);
    } finally {
      setTwoFaBusy(false);
    }
  };

  const startResendCountdown = () => {
    setResendCountdown(SMS_RESEND_SECONDS);
    if (countdownTimer.current) clearInterval(countdownTimer.current);
    countdownTimer.current = setInterval(() => {
      setResendCountdown((prev) => {
        if (prev <= 1) {
          if (countdownTimer.current) clearInterval(countdownTimer.current);
          return 0;
        }
        return prev - 1;
      });
    }, 1000);
  };

  const handleSendCode = async () => {
    setErrorCode(null);
    setErrorMessage(null);
    setSendCodeBusy(true);
    try {
      await we2aiApi.sendSmsCode(region, phone.trim());
      startResendCountdown();
    } catch (error) {
      showError(error);
    } finally {
      setSendCodeBusy(false);
    }
  };

  const handlePhoneLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setErrorCode(null);
    setErrorMessage(null);
    setPhoneLoginBusy(true);
    try {
      await we2aiApi.loginPhone(region, phone.trim(), smsCode.trim());
      onLoginSuccess();
    } catch (error) {
      showError(error);
    } finally {
      setPhoneLoginBusy(false);
    }
  };

  const displayedError =
    errorMessage &&
    (errorCode === "BACKEND_MODE_ACTIVE" && tab === "phone"
      ? t.errorBackendModeActivePhoneHint
      : errorMessage);

  // 邮箱登录、发短信、手机登录三个操作都可能触发 Rust 侧打开验证码窗口
  // （固定标签 `"we2ai-captcha"`），同时发起两个会互相冲突（后一个的
  // `WebviewWindowBuilder::build()` 会因为标签重复而失败）。Rust 侧
  // `CaptchaRegistry::window_lock` 已经把 `open_captcha_window` 调用整体
  // 序列化（见 `captcha.rs`），这里的按钮置灰是配合的 UX 措施：避免用户点了
  // 第二个按钮之后长时间"卡住没反应"（其实是在排队等第一个验证码窗口
  // 关闭），而不是唯一的正确性保障（Codex 代码评审中危项 4）。
  const captchaFlowBusy = emailBusy || sendCodeBusy || phoneLoginBusy;

  // Codex 代码评审第 3 轮高危项 1：登录请求在途时仍可切区域，切区域又异步
  // 恢复目标区域的会话，稍后完成的旧区域登录会把已经生效的新区域会话覆盖
  // 掉。Rust 侧（`session.rs` 的登录/恢复操作代次）已经堵死了"错误会话生效"
  // 这个正确性问题，这里的按钮置灰是同一个修复的前端配合措施：登录/2FA
  // 请求在途时不能切区域，切区域请求在途时不能提交登录，减少这类竞态在
  // 真实使用中出现的机会（不是唯一的正确性保障，Rust 侧代次校验才是）。
  const loginFlowBusy = captchaFlowBusy || twoFaBusy;

  return (
    <div className="flex h-screen w-screen items-center justify-center bg-background p-6">
      <Card className="w-full max-w-md">
        <CardHeader className="items-center text-center">
          <img
            src={we2aiLogo}
            alt=""
            aria-hidden="true"
            className="mb-2 h-10 w-10"
          />
          <CardTitle>{t.loginTitle}</CardTitle>
          <CardDescription>{t.loginSubtitle}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="we2ai-region">{t.regionLabel}</Label>
            <Select
              value={region}
              onValueChange={handleRegionChange}
              disabled={
                !regionsLoaded ||
                regionSwitchBusy ||
                stage.kind === "twoFa" ||
                loginFlowBusy
              }
            >
              <SelectTrigger id="we2ai-region">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {regions.map((r) => (
                  <SelectItem key={r} value={r}>
                    {regionLabel(t, r)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {stage.kind === "twoFa" ? (
            <form className="space-y-4" onSubmit={handleTwoFaSubmit}>
              <div className="space-y-1">
                <p className="text-sm font-medium">{t.twoFaTitle}</p>
                <p className="text-xs text-muted-foreground">
                  {formatWe2aiString(t.twoFaDescription, {
                    email: stage.emailMasked,
                  })}
                </p>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="we2ai-totp">{t.twoFaCodeLabel}</Label>
                <Input
                  id="we2ai-totp"
                  inputMode="numeric"
                  maxLength={6}
                  autoFocus
                  value={totpCode}
                  onChange={(e) =>
                    setTotpCode(e.target.value.replace(/\D/g, ""))
                  }
                />
              </div>
              {displayedError && (
                <p className="text-sm text-destructive">{displayedError}</p>
              )}
              <div className="flex gap-2">
                <Button
                  type="button"
                  variant="outline"
                  className="flex-1"
                  onClick={() => {
                    setStage({ kind: "credentials" });
                    setErrorCode(null);
                    setErrorMessage(null);
                  }}
                >
                  {t.twoFaBack}
                </Button>
                <Button
                  type="submit"
                  className="flex-1"
                  disabled={
                    twoFaBusy || totpCode.length !== 6 || regionSwitchBusy
                  }
                >
                  {twoFaBusy ? t.twoFaSubmitBusy : t.twoFaSubmit}
                </Button>
              </div>
            </form>
          ) : (
            <Tabs
              value={tab}
              onValueChange={(v) => setTab(v as "email" | "phone")}
            >
              <TabsList className="w-full">
                <TabsTrigger className="flex-1" value="email">
                  {t.tabEmailLogin}
                </TabsTrigger>
                {isDomesticRegion(region) && (
                  <TabsTrigger className="flex-1" value="phone">
                    {t.tabPhoneLogin}
                  </TabsTrigger>
                )}
              </TabsList>

              <TabsContent value="email">
                <form className="space-y-4" onSubmit={handleEmailLogin}>
                  <div className="space-y-1.5">
                    <Label htmlFor="we2ai-email">{t.emailLabel}</Label>
                    <Input
                      id="we2ai-email"
                      type="email"
                      autoComplete="username"
                      required
                      value={email}
                      onChange={(e) => setEmail(e.target.value)}
                    />
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="we2ai-password">{t.passwordLabel}</Label>
                    <Input
                      id="we2ai-password"
                      type="password"
                      autoComplete="current-password"
                      required
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                    />
                  </div>
                  {displayedError && (
                    <p className="text-sm text-destructive">{displayedError}</p>
                  )}
                  <Button
                    type="submit"
                    className="w-full"
                    disabled={loginFlowBusy || regionSwitchBusy}
                  >
                    {emailBusy ? t.loginButtonBusy : t.loginButton}
                  </Button>
                </form>
              </TabsContent>

              {isDomesticRegion(region) && (
                <TabsContent value="phone">
                  <form className="space-y-4" onSubmit={handlePhoneLogin}>
                    <div className="space-y-1.5">
                      <Label htmlFor="we2ai-phone">{t.phoneLabel}</Label>
                      <Input
                        id="we2ai-phone"
                        type="tel"
                        autoComplete="tel"
                        required
                        value={phone}
                        onChange={(e) => setPhone(e.target.value)}
                      />
                    </div>
                    <div className="space-y-1.5">
                      <Label htmlFor="we2ai-sms-code">{t.phoneCodeLabel}</Label>
                      <div className="flex gap-2">
                        <Input
                          id="we2ai-sms-code"
                          inputMode="numeric"
                          required
                          value={smsCode}
                          onChange={(e) => setSmsCode(e.target.value)}
                          className="flex-1"
                        />
                        <Button
                          type="button"
                          variant="outline"
                          disabled={
                            loginFlowBusy ||
                            regionSwitchBusy ||
                            resendCountdown > 0 ||
                            !phone.trim()
                          }
                          onClick={() => void handleSendCode()}
                        >
                          {sendCodeBusy
                            ? t.sendCodeButtonBusy
                            : resendCountdown > 0
                              ? formatWe2aiString(t.resendCodeIn, {
                                  seconds: resendCountdown,
                                })
                              : t.sendCodeButton}
                        </Button>
                      </div>
                    </div>
                    {displayedError && (
                      <p className="text-sm text-destructive">
                        {displayedError}
                      </p>
                    )}
                    <Button
                      type="submit"
                      className="w-full"
                      disabled={loginFlowBusy || regionSwitchBusy}
                    >
                      {phoneLoginBusy
                        ? t.phoneLoginButtonBusy
                        : t.phoneLoginButton}
                    </Button>
                  </form>
                </TabsContent>
              )}
            </Tabs>
          )}

          <p className="text-center text-xs text-muted-foreground">
            {t.freeLoginNote}
          </p>
        </CardContent>
      </Card>
    </div>
  );
}

/** 供 We2aiShell 在登出后展示一次性提示，避免每个调用点重复拼文案。 */
export function notifyLogoutOutcome(
  t: We2aiStrings,
  outcome: "revoked" | "localOnly" | "notLoggedIn",
) {
  if (outcome === "revoked") {
    toast.success(t.logoutSuccessRevoked);
  } else if (outcome === "localOnly") {
    toast.message(t.logoutSuccessLocalOnly);
  }
}

export default LoginPage;
