//! 登录验证码：窗口内导航拦截，不经过系统协议（方案第 5.1 节）。
//!
//! 分两层：
//! - [`CaptchaRegistry`] + [`evaluate_navigation`]：与 Tauri 运行时无关的纯逻辑
//!   （nonce 登记、一次性消费、120 秒过期、URL 校验、按 provider 提取票据字段），
//!   可在没有真实窗口的情况下完整单测。
//! - [`open_captcha_window`]：真正创建 `WebviewWindow` 并注册 `on_navigation`
//!   的编排代码，只能在真实 Tauri 运行时里跑，不做单测（见任务报告"需要真实
//!   环境才能验证的项"）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;

use super::api::{CaptchaProvider, CaptchaTicket};
use super::region::Region;

/// pending 一次性票据的存活时间（方案第 5.1 节：120 秒）。
const PENDING_TTL: Duration = Duration::from_secs(120);

/// SubPanel 导航拦截目标路径（`backend/internal/router` / 前端
/// `DesktopCaptchaView.vue` 的 `navigateToDone()`），固定不含查询串。
const DONE_PATH: &str = "/desktop/captcha/done";

/// 一次性验证码请求登记。
pub struct PendingCaptcha {
    pub provider: CaptchaProvider,
    pub region: Region,
    expires_at: Instant,
}

impl PendingCaptcha {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// nonce → pending 登记表。每个 `SessionManager`/命令层共享一个实例（Tauri
/// managed state），跨多次登录 / 发短信请求复用。
#[derive(Default)]
pub struct CaptchaRegistry {
    pending: Mutex<HashMap<String, PendingCaptcha>>,
    /// 序列化 `open_captcha_window` 调用：验证码窗口标签固定为
    /// `"we2ai-captcha"`，同时创建两个同标签窗口会失败——邮箱登录、发短信、
    /// 手机登录三个入口都可能触发验证码窗口，用户可能在第一个还没关闭时就
    /// 点了另一个（Codex 代码评审中危项 4）。持锁贯穿整个
    /// `open_captcha_window` 调用期间，第二个请求会排队等第一个窗口关闭后
    /// 再打开，而不是创建失败。
    window_lock: tokio::sync::Mutex<()>,
}

impl CaptchaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 生成 32 字节随机 nonce 并登记 pending（120 秒过期）。返回值即用于
    /// `?n=<nonce>` 查询参数（URL-safe base64，无需再做 URL 编码）。
    pub fn register(&self, provider: CaptchaProvider, region: Region) -> String {
        self.register_with_ttl(provider, region, PENDING_TTL)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_with_ttl(
        &self,
        provider: CaptchaProvider,
        region: Region,
        ttl: Duration,
    ) -> String {
        let nonce = generate_nonce();
        let entry = PendingCaptcha {
            provider,
            region,
            expires_at: Instant::now() + ttl,
        };
        self.pending.lock().unwrap().insert(nonce.clone(), entry);
        nonce
    }

    /// 一次性消费：命中且未过期才返回 `Some` 并移除该条目；不存在、已被消费过、
    /// 或已过期均返回 `None`（且顺带清掉过期条目，不需要调用方区分这三种情况）。
    fn consume(&self, nonce: &str) -> Option<PendingCaptcha> {
        let mut guard = self.pending.lock().unwrap();
        let entry = guard.remove(nonce)?;
        if entry.is_expired() {
            None
        } else {
            Some(entry)
        }
    }

    /// 窗口被用户关闭 / 整体流程放弃时显式作废，避免残留内存并防止稍后凭同一
    /// nonce 蹭到已经放弃的验证码流程。
    pub fn invalidate(&self, nonce: &str) {
        self.pending.lock().unwrap().remove(nonce);
    }
}

fn generate_nonce() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 验证码页面地址：`<区域基址>/desktop/captcha?n=<nonce>&p=<provider>`。
pub fn captcha_page_url(region: Region, provider: CaptchaProvider, nonce: &str) -> String {
    format!(
        "{}/desktop/captcha?n={}&p={}",
        region.base_url(),
        nonce,
        provider.as_query_value()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationOutcome {
    /// 不是 `/desktop/captcha/done` 路径的导航（加载验证码组件本身、第三方
    /// 验证码 SDK 资源等），放行、不拦截。
    Ignore,
    /// 命中 done 路径且全部校验通过：取消导航，交付票据。
    Ticket(CaptchaTicket),
    /// 命中 done 路径但校验失败：取消导航，不交付票据。
    Rejected(RejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// nonce 未登记 / 已被消费过一次 / 已超过 120 秒过期——三种情况在
    /// [`CaptchaRegistry::consume`] 内合并处理，调用方不需要区分。
    NonceUnknownConsumedOrExpired,
    InsecureScheme,
    HostMismatch,
    ProviderMismatch,
    UnknownProvider,
    MissingFields,
    /// 非 done 路径的导航，但既不是 https、也不属于当前区域或已知验证码
    /// SDK 域名——一律拒绝（Codex 代码评审高危项：验证码窗口此前对这类导航
    /// 一律放行，等同于允许窗口被导航到任意第三方页面）。
    DisallowedNavigation,
}

/// 已知验证码服务商的脚本/挑战域名精确白名单，仅用于放行验证码窗口内*非*
/// done-path 的导航。来源为 SubPanel 前端实际加载的脚本地址（不是凭空列出，
/// 均可在源码中核实）：
/// - Cloudflare Turnstile：`frontend/src/components/TurnstileWidget.vue`
///   （`https://challenges.cloudflare.com/turnstile/v0/api.js`）
/// - 腾讯云验证码（天御）：`frontend/src/utils/tencentCaptcha.ts`
///   （国内 `https://turing.captcha.qcloud.com/TJCaptcha.js`，
///   国际 `https://ca.turing.captcha.qcloud.com/TJNCaptcha-global.js`）
/// - 阿里云验证码：`frontend/src/components/AliyunCaptchaWidget.vue`
///   （`https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js`）
///
/// **修正记录（Opus 5 代码评审中危项 1，v1 的假设有误）**：v1 文档注释断言
/// "`on_navigation` 只对顶层 frame 的导航触发"，这是错的——macOS 上 wry 的
/// `WKWebView` 导航策略回调（`wry::wkwebview::navigation::navigation_policy`，
/// wry 0.54.3 源码）直接把 `WKNavigationAction` 的 URL 转给回调函数，完全没
/// 有检查 `action.targetFrame()?.isMainFrame()`；Linux 的 WebKitGTK
/// `decide-policy` 信号同样覆盖子 frame。也就是说验证码挑战 iframe 自身的
/// 导航（包括初始 `about:blank`/`about:srcdoc`、以及挑战脚本在 iframe 内部
/// 跳转到供应商其他子域）也会经过这里，此前只认精确 host 会把这些合法的
/// iframe 导航一起拒绝，导致验证码组件在真机上加载不出来。
const CAPTCHA_SDK_HOSTS: &[&str] = &[
    "challenges.cloudflare.com",
    "turing.captcha.qcloud.com",
    "ca.turing.captcha.qcloud.com",
    "o.alicdn.com",
];

/// 按域名后缀匹配的验证码 SDK 域（腾讯、阿里的挑战 iframe 会在自己的域名
/// 体系下跳转到其他子域，精确 host 覆盖不了）。后缀必须整段匹配（要求前面
/// 恰好是一个 `.` 分隔符或整个 host 相等），防止 `evilqcloud.com` 之类把
/// `qcloud.com` 当子串藏在自己域名里的仿冒手法命中。列表来源：Opus 5 代码
/// 评审给出（腾讯天御域 `captcha.qcloud.com` 及其静态资源域 `gtimg.com`；
/// 阿里云验证码静态资源域 `alicdn.com` 及 API 域 `aliyuncs.com`）——本机未
/// 找到 SubPanel 源码里的进一步逐条证据，已在功能列表登记为 P5"分别在
/// macOS/Linux 上实测三家验证码"的实机验收项，届时如发现子域不在这份列表
/// 里再补充。
const CAPTCHA_SDK_HOST_SUFFIXES: &[&str] = &[
    ".captcha.qcloud.com",
    ".gtimg.com",
    ".alicdn.com",
    ".aliyuncs.com",
];

fn host_matches_suffix(host: &str, suffix_with_dot: &str) -> bool {
    let bare = &suffix_with_dot[1..]; // 去掉前导 "."
    host == bare || host.ends_with(suffix_with_dot)
}

/// 非 done 路径导航是否允许放行：
/// - `about:` scheme（`about:blank`/`about:srcdoc`）无条件放行——这是 iframe
///   的初始空文档/`srcdoc` 内容，本身不承载可导航到的远程地址，没有
///   host，放行不构成安全风险。
/// - `data:`/`blob:` 不放行：Turnstile/腾讯天御/阿里云验证码三家的官方
///   接入方式都是 `<script src="https://...">` 加载脚本、脚本自己创建指向
///   真实 https 地址的 iframe，本机没有找到证据表明官方集成流程会用到
///   `data:`/`blob:` 导航；默认拒绝更安全，真机验收（见下方 P5 登记）如果
///   发现某家验证码确实依赖它们，再补证据放开。
/// - 其余必须是 https，且 host 等于当前区域域名、命中 [`CAPTCHA_SDK_HOSTS`]
///   精确匹配、或命中 [`CAPTCHA_SDK_HOST_SUFFIXES`] 后缀匹配之一。
fn is_allowed_non_done_navigation(url: &url::Url, expected_region: Region) -> bool {
    if url.scheme() == "about" {
        return true;
    }
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let expected_host = url::Url::parse(expected_region.base_url())
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    if expected_host.as_deref() == Some(host) {
        return true;
    }
    if CAPTCHA_SDK_HOSTS.contains(&host) {
        return true;
    }
    CAPTCHA_SDK_HOST_SUFFIXES
        .iter()
        .any(|suffix| host_matches_suffix(host, suffix))
}

/// 判定一次导航应如何处理，并在命中 done 路径时一次性消费对应的 pending
/// 登记。`expected_region` 是打开验证码窗口时使用的区域（窗口创建时已知，
/// 不依赖 nonce 查表），用于非 done 路径导航的 host 校验（见
/// [`is_allowed_non_done_navigation`]）。
///
/// done 路径的校验顺序对应方案第 5.1 节："校验 https、host=当前区域、路径、
/// p 与 pending 一致、nonce"——路径判定最先做（决定是否需要拦截这次导航），
/// 其余四项在路径命中后按 https → nonce（消费即校验存在性）→ host →
/// provider 的顺序展开（消费提前到 host/scheme 校验之前是为了保证 nonce
/// 永远被"用掉"一次，即便这次导航本身因协议不对而被拒绝，也不给下一次重放
/// 留下可乘之机）。
pub fn evaluate_navigation(
    registry: &CaptchaRegistry,
    url: &url::Url,
    expected_region: Region,
) -> NavigationOutcome {
    if url.path() != DONE_PATH {
        return if is_allowed_non_done_navigation(url, expected_region) {
            NavigationOutcome::Ignore
        } else {
            NavigationOutcome::Rejected(RejectReason::DisallowedNavigation)
        };
    }

    let fragment = url.fragment().unwrap_or("");
    let params: HashMap<String, String> = url::form_urlencoded::parse(fragment.as_bytes())
        .into_owned()
        .collect();

    let Some(nonce) = params.get("n") else {
        return NavigationOutcome::Rejected(RejectReason::MissingFields);
    };

    let Some(pending) = registry.consume(nonce) else {
        return NavigationOutcome::Rejected(RejectReason::NonceUnknownConsumedOrExpired);
    };

    if url.scheme() != "https" {
        return NavigationOutcome::Rejected(RejectReason::InsecureScheme);
    }

    let expected_host = url::Url::parse(pending.region.base_url())
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    if expected_host.as_deref() != url.host_str() {
        return NavigationOutcome::Rejected(RejectReason::HostMismatch);
    }

    let Some(p) = params.get("p") else {
        return NavigationOutcome::Rejected(RejectReason::MissingFields);
    };
    let Some(provider) = CaptchaProvider::from_query_value(p) else {
        return NavigationOutcome::Rejected(RejectReason::UnknownProvider);
    };
    if provider != pending.provider {
        return NavigationOutcome::Rejected(RejectReason::ProviderMismatch);
    }

    match provider {
        CaptchaProvider::Turnstile => match params.get("t") {
            Some(token) => NavigationOutcome::Ticket(CaptchaTicket::Turnstile(token.clone())),
            None => NavigationOutcome::Rejected(RejectReason::MissingFields),
        },
        CaptchaProvider::Tencent => match (params.get("ticket"), params.get("randstr")) {
            (Some(ticket), Some(randstr)) => NavigationOutcome::Ticket(CaptchaTicket::Tencent {
                ticket: ticket.clone(),
                randstr: randstr.clone(),
            }),
            _ => NavigationOutcome::Rejected(RejectReason::MissingFields),
        },
        CaptchaProvider::Aliyun => match params.get("param") {
            Some(param) => NavigationOutcome::Ticket(CaptchaTicket::Aliyun(param.clone())),
            None => NavigationOutcome::Rejected(RejectReason::MissingFields),
        },
    }
}

/// 验证码流程失败原因（窗口层面，不含 [`RejectReason`]——那是"收到了一次被拒绝
/// 的导航"，这里是"整个流程没能拿到票据"）。
#[derive(Debug, Clone)]
pub enum CaptchaFlowError {
    /// 用户在完成验证码前关闭了窗口。
    WindowClosed,
    /// 120 秒内未完成（与 pending TTL 对齐）。
    Timeout,
    /// 创建窗口失败（Tauri 运行时错误）。
    WindowCreateFailed(String),
}

impl std::fmt::Display for CaptchaFlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptchaFlowError::WindowClosed => write!(f, "验证码窗口已关闭"),
            CaptchaFlowError::Timeout => write!(f, "验证码超时"),
            CaptchaFlowError::WindowCreateFailed(e) => write!(f, "无法打开验证码窗口: {e}"),
        }
    }
}

/// Tauri managed state：跨命令共享同一个 pending 登记表。
pub struct We2aiCaptchaState(pub Arc<CaptchaRegistry>);

/// 打开验证码窗口并等待用户完成挑战，返回票据。
///
/// 依赖真实 Tauri 运行时（创建 `WebviewWindow`、拦截 `on_navigation`、监听窗口
/// 关闭事件），不适合也没有在单测中覆盖——`evaluate_navigation` 已经覆盖了这里
/// 面全部有意义的判定逻辑；这个函数只是把判定结果接到真实窗口的生命周期上。
/// 验证码窗口只加载远程页面（`WebviewUrl::External`），不落在 `main` 窗口的
/// capabilities 配置范围内，因此不获得任何 IPC 能力。
pub async fn open_captcha_window(
    app: tauri::AppHandle,
    registry: Arc<CaptchaRegistry>,
    region: Region,
    provider: CaptchaProvider,
) -> Result<CaptchaTicket, CaptchaFlowError> {
    // 持锁贯穿本函数剩余部分（含窗口打开、等待用户完成、关闭）：验证码窗口
    // 标签固定为 "we2ai-captcha"，并发调用会创建失败；这里改为排队而不是
    // 报错（Codex 代码评审中危项 4）。`_window_guard` 在函数返回时（任意
    // return 路径）自动释放。
    let _window_guard = registry.window_lock.lock().await;

    let nonce = registry.register(provider, region);
    let url_str = captcha_page_url(region, provider, &nonce);
    let url = url::Url::parse(&url_str)
        .map_err(|e| CaptchaFlowError::WindowCreateFailed(e.to_string()))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<CaptchaTicket, CaptchaFlowError>>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let tx_nav = tx.clone();
    let tx_close = tx.clone();
    let registry_nav = registry.clone();
    let nonce_close = nonce.clone();
    let registry_close = registry.clone();
    // 窗口"真正销毁完成"信号，独立于上面交付票据/关闭结果的 `tx`——不管
    // 整个流程从哪条路径结束（拿到票据 / 被拒绝 / 超时 / 用户手动关闭），
    // 都要等这个窗口的 `Destroyed` 事件真正触发之后才能释放
    // `_window_guard`，见下方调用处的说明（Codex 代码评审低危项 3）。
    let (destroyed_tx, destroyed_rx) = tokio::sync::oneshot::channel::<()>();
    let destroyed_tx = Arc::new(Mutex::new(Some(destroyed_tx)));

    let window =
        tauri::WebviewWindowBuilder::new(&app, "we2ai-captcha", tauri::WebviewUrl::External(url))
            .title("验证码")
            .inner_size(420.0, 640.0)
            .on_navigation(move |navigated| {
                match evaluate_navigation(&registry_nav, navigated, region) {
                    NavigationOutcome::Ignore => true,
                    NavigationOutcome::Ticket(ticket) => {
                        if let Some(sender) = tx_nav.lock().unwrap().take() {
                            let _ = sender.send(Ok(ticket));
                        }
                        false
                    }
                    NavigationOutcome::Rejected(reason) => {
                        log::warn!("[we2ai] 验证码导航被拒绝: {reason:?}");
                        false
                    }
                }
            })
            .build()
            .map_err(|e| {
                // 窗口建不出来这张 pending 就再也没有机会被消费或被 on_window_event
                // 清理（那两条路径都要求窗口先建成功），必须在这里就地作废，否则会
                // 一直占位到自然过期（Codex 代码评审中危项 4）。
                registry.invalidate(&nonce);
                CaptchaFlowError::WindowCreateFailed(e.to_string())
            })?;

    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            registry_close.invalidate(&nonce_close);
            if let Some(sender) = tx_close.lock().unwrap().take() {
                let _ = sender.send(Err(CaptchaFlowError::WindowClosed));
            }
            if let Some(sender) = destroyed_tx.lock().unwrap().take() {
                let _ = sender.send(());
            }
        }
    });

    let result = tokio::time::timeout(PENDING_TTL, rx).await;
    let _ = window.close();
    registry.invalidate(&nonce);

    // `window.close()` 在部分平台（如 Windows 上的 WebView2）上不是同步销毁
    // 完成的——真正的原生资源释放、标签 "we2ai-captcha" 变为可复用，要等
    // `WindowEvent::Destroyed` 真正触发。`_window_guard` 要在这之后才释放，
    // 否则排在锁后面的下一次 `open_captcha_window` 可能在旧窗口标签还没被
    // 系统回收时就尝试创建同名新窗口，导致 `.build()` 失败（Codex 代码评审
    // 低危项 3）。5 秒兜底超时防止 `Destroyed` 因为某种异常没有触发时把锁
    // 永久卡住——即便真的发生，最坏影响也只是下一次验证码请求偶发失败，
    // 用户重试即可恢复，不是数据损坏或安全问题。若窗口在这之前已经被用户
    // 手动关闭，`destroyed_rx` 这时已经有值，`await` 会立即返回。
    let _ = tokio::time::timeout(Duration::from_secs(5), destroyed_rx).await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(_)) => Err(CaptchaFlowError::WindowClosed),
        Err(_) => Err(CaptchaFlowError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done_url(query: &str) -> url::Url {
        url::Url::parse(&format!(
            "https://api.we2ai.com/desktop/captcha/done#{query}"
        ))
        .unwrap()
    }

    #[test]
    fn nonce_is_32_bytes_of_randomness_and_unique() {
        let registry = CaptchaRegistry::new();
        let a = registry.register(CaptchaProvider::Turnstile, Region::International);
        let b = registry.register(CaptchaProvider::Turnstile, Region::International);
        assert_ne!(a, b);
        let decoded = URL_SAFE_NO_PAD.decode(&a).expect("valid base64url");
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn non_done_path_is_ignored_without_consuming_pending() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Turnstile, Region::International);
        let url = url::Url::parse(&format!(
            "https://api.we2ai.com/desktop/captcha?n={nonce}&p=turnstile"
        ))
        .unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Ignore
        );
        // 仍未被消费，之后的真实 done 导航应该还能成功。
        let done = done_url(&format!("n={nonce}&p=turnstile&t=tok"));
        assert_eq!(
            evaluate_navigation(&registry, &done, Region::International),
            NavigationOutcome::Ticket(CaptchaTicket::Turnstile("tok".to_string()))
        );
    }

    #[test]
    fn valid_turnstile_done_navigation_yields_ticket_and_cancels() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Turnstile, Region::International);
        let url = done_url(&format!("n={nonce}&p=turnstile&t=abc123"));
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Ticket(CaptchaTicket::Turnstile("abc123".to_string()))
        );
    }

    #[test]
    fn valid_tencent_done_navigation_yields_ticket_with_two_fields() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Tencent, Region::International);
        let url = done_url(&format!("n={nonce}&p=tencent&ticket=t1&randstr=r1"));
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Ticket(CaptchaTicket::Tencent {
                ticket: "t1".to_string(),
                randstr: "r1".to_string(),
            })
        );
    }

    #[test]
    fn valid_aliyun_done_navigation_yields_ticket() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Aliyun, Region::DomesticProd);
        let url = url::Url::parse(&format!(
            "https://api.wtgo.com.cn/desktop/captcha/done#n={nonce}&p=aliyun&param=xyz"
        ))
        .unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::DomesticProd),
            NavigationOutcome::Ticket(CaptchaTicket::Aliyun("xyz".to_string()))
        );
    }

    #[test]
    fn wrong_host_is_rejected_and_consumes_nonce() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Turnstile, Region::International);
        let url = url::Url::parse(&format!(
            "https://evil.example.com/desktop/captcha/done#n={nonce}&p=turnstile&t=tok"
        ))
        .unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::HostMismatch)
        );
        // 一次性消费：即便被拒绝，nonce 也已作废，不能重放。
        let retry = done_url(&format!("n={nonce}&p=turnstile&t=tok"));
        assert_eq!(
            evaluate_navigation(&registry, &retry, Region::International),
            NavigationOutcome::Rejected(RejectReason::NonceUnknownConsumedOrExpired)
        );
    }

    #[test]
    fn insecure_scheme_is_rejected() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Turnstile, Region::International);
        let url = url::Url::parse(&format!(
            "http://api.we2ai.com/desktop/captcha/done#n={nonce}&p=turnstile&t=tok"
        ))
        .unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::InsecureScheme)
        );
    }

    #[test]
    fn unregistered_nonce_is_rejected() {
        let registry = CaptchaRegistry::new();
        let url = done_url("n=never-registered&p=turnstile&t=tok");
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::NonceUnknownConsumedOrExpired)
        );
    }

    #[test]
    fn duplicate_consumption_is_rejected() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Turnstile, Region::International);
        let url = done_url(&format!("n={nonce}&p=turnstile&t=tok"));
        assert!(matches!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Ticket(_)
        ));
        // 同一个 nonce 第二次提交（重放）必须被拒绝。
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::NonceUnknownConsumedOrExpired)
        );
    }

    #[test]
    fn expired_pending_is_rejected() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register_with_ttl(
            CaptchaProvider::Turnstile,
            Region::International,
            Duration::from_millis(1),
        );
        std::thread::sleep(Duration::from_millis(20));
        let url = done_url(&format!("n={nonce}&p=turnstile&t=tok"));
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::NonceUnknownConsumedOrExpired)
        );
    }

    #[test]
    fn provider_mismatch_is_rejected() {
        let registry = CaptchaRegistry::new();
        // 用 tencent 注册，却在 done 导航里带 p=turnstile。
        let nonce = registry.register(CaptchaProvider::Tencent, Region::International);
        let url = done_url(&format!("n={nonce}&p=turnstile&t=tok"));
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::ProviderMismatch)
        );
    }

    #[test]
    fn missing_provider_specific_fields_is_rejected() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Tencent, Region::International);
        // 缺 randstr。
        let url = done_url(&format!("n={nonce}&p=tencent&ticket=t1"));
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::MissingFields)
        );
    }

    #[test]
    fn invalidate_removes_pending_before_any_navigation() {
        let registry = CaptchaRegistry::new();
        let nonce = registry.register(CaptchaProvider::Turnstile, Region::International);
        registry.invalidate(&nonce);
        let url = done_url(&format!("n={nonce}&p=turnstile&t=tok"));
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::NonceUnknownConsumedOrExpired)
        );
    }

    #[test]
    fn captcha_page_url_includes_nonce_and_provider() {
        let url = captcha_page_url(Region::International, CaptchaProvider::Tencent, "abc");
        assert_eq!(url, "https://api.we2ai.com/desktop/captcha?n=abc&p=tencent");
    }

    // Codex 代码评审高危项 1（验证码窗口纵深防御）：非 done 路径的导航必须
    // 限制在"当前区域域名 + 已知验证码 SDK 域名"之内，其余一律拒绝。

    #[test]
    fn non_done_navigation_to_arbitrary_https_host_is_rejected() {
        let registry = CaptchaRegistry::new();
        let url = url::Url::parse("https://evil.example.com/phishing").unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::DisallowedNavigation)
        );
    }

    #[test]
    fn non_done_navigation_over_http_is_rejected_even_on_the_expected_host() {
        let registry = CaptchaRegistry::new();
        let url = url::Url::parse("http://api.we2ai.com/desktop/captcha").unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::DisallowedNavigation)
        );
    }

    #[test]
    fn non_done_navigation_to_known_captcha_sdk_hosts_is_allowed() {
        let registry = CaptchaRegistry::new();
        for host in CAPTCHA_SDK_HOSTS {
            let url = url::Url::parse(&format!("https://{host}/some/script.js")).unwrap();
            assert_eq!(
                evaluate_navigation(&registry, &url, Region::International),
                NavigationOutcome::Ignore,
                "expected {host} to be an allowed captcha SDK host"
            );
        }
    }

    #[test]
    fn non_done_navigation_to_lookalike_host_is_rejected() {
        let registry = CaptchaRegistry::new();
        // 子串命中但不是精确域名匹配（如 "challenges.cloudflare.com.evil.com"
        // 或反过来），必须被拒绝——`Url::host_str()` 精确匹配整个 host，
        // `CAPTCHA_SDK_HOSTS.contains` 也是精确字符串比较，这里断言这个组合
        // 行为，防止将来改成子串/后缀匹配引入绕过。
        let url = url::Url::parse("https://challenges.cloudflare.com.evil.com/x").unwrap();
        assert_eq!(
            evaluate_navigation(&registry, &url, Region::International),
            NavigationOutcome::Rejected(RejectReason::DisallowedNavigation)
        );
    }

    // Codex 代码评审中危项 1：macOS 的 wry WKWebView 导航策略回调、Linux 的
    // WebKitGTK decide-policy 都覆盖子 frame（iframe）导航，不只是主 frame。
    // `about:blank`/`about:srcdoc` 是 iframe 的初始空文档/`srcdoc` 内容，
    // 必须放行，否则验证码 iframe 加载不出来。

    #[test]
    fn about_blank_and_srcdoc_navigations_are_allowed() {
        let registry = CaptchaRegistry::new();
        for url_str in ["about:blank", "about:srcdoc"] {
            let url = url::Url::parse(url_str).unwrap();
            assert_eq!(
                evaluate_navigation(&registry, &url, Region::International),
                NavigationOutcome::Ignore,
                "expected {url_str} to be allowed (iframe initial document)"
            );
        }
    }

    #[test]
    fn data_and_blob_navigations_are_rejected_by_default() {
        // 没有找到证据表明 Turnstile/腾讯天御/阿里云验证码的官方集成方式会
        // 用到 data:/blob: 导航，默认拒绝（见 is_allowed_non_done_navigation
        // 的文档注释）；如果真机验收发现某家依赖它们，需要补证据后再调整
        // 这条测试。
        let registry = CaptchaRegistry::new();
        for url_str in [
            "data:text/html,<script>alert(1)</script>",
            "blob:https://challenges.cloudflare.com/1234-5678",
        ] {
            let url = url::Url::parse(url_str).unwrap();
            assert_eq!(
                evaluate_navigation(&registry, &url, Region::International),
                NavigationOutcome::Rejected(RejectReason::DisallowedNavigation),
                "expected {url_str} to be rejected by default"
            );
        }
    }

    #[test]
    fn non_done_navigation_to_captcha_sdk_host_suffixes_is_allowed() {
        let registry = CaptchaRegistry::new();
        let cases = [
            // 后缀本身（不带子域）。
            "https://gtimg.com/img.png",
            "https://alicdn.com/widget.js",
            "https://aliyuncs.com/api",
            "https://captcha.qcloud.com/x",
            // 真实场景下的子域。
            "https://imgcache.gtimg.com/img.png",
            "https://a1.alicdn.com/widget.js",
            "https://afs.aliyuncs.com/api",
            "https://turing.captcha.qcloud.com/TJCaptcha.js",
        ];
        for url_str in cases {
            let url = url::Url::parse(url_str).unwrap();
            assert_eq!(
                evaluate_navigation(&registry, &url, Region::International),
                NavigationOutcome::Ignore,
                "expected {url_str} to be allowed via a captcha SDK host suffix"
            );
        }
    }

    #[test]
    fn non_done_navigation_to_suffix_lookalike_host_is_rejected() {
        let registry = CaptchaRegistry::new();
        // "evilqcloud.com" 之类把 "qcloud.com"/"gtimg.com" 藏成自己域名的一
        // 部分（而不是真正的子域，前面没有分隔用的 "."），必须被拒绝，否则
        // 后缀匹配就形同虚设。
        let cases = [
            "https://evilgtimg.com/x",
            "https://notgtimg.com.evil.com/x",
            "https://evilqcloud.com/x",
            "https://alicdn.com.evil.com/x",
        ];
        for url_str in cases {
            let url = url::Url::parse(url_str).unwrap();
            assert_eq!(
                evaluate_navigation(&registry, &url, Region::International),
                NavigationOutcome::Rejected(RejectReason::DisallowedNavigation),
                "expected {url_str} to be rejected (suffix lookalike)"
            );
        }
    }

    // Codex 代码评审中危项 4：验证码窗口标签固定为 "we2ai-captcha"，并发调用
    // `open_captcha_window` 必须排队而不是让后一个创建失败。这里不驱动完整
    // 的真实窗口创建（那部分依赖真实 Tauri 运行时，见模块顶部文档），只验证
    // 序列化本身用的这把锁确实会阻塞第二个持锁者，直到第一个释放。
    #[tokio::test]
    async fn window_lock_serializes_concurrent_captcha_flows() {
        let registry = Arc::new(CaptchaRegistry::new());
        let guard1 = registry.window_lock.lock().await;

        let registry2 = registry.clone();
        let second_acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_acquired_writer = second_acquired.clone();
        let handle = tokio::spawn(async move {
            let _guard2 = registry2.window_lock.lock().await;
            second_acquired_writer.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // 第一个持锁者还没释放时，第二个必须还在排队。
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !second_acquired.load(std::sync::atomic::Ordering::SeqCst),
            "a second captcha flow must not proceed while the first still holds the window lock"
        );

        drop(guard1);
        handle.await.unwrap();
        assert!(
            second_acquired.load(std::sync::atomic::Ordering::SeqCst),
            "the second captcha flow must proceed once the first releases the window lock"
        );
    }
}
