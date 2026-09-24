//! SubPanel HTTP 客户端：envelope 解包、四种错误响应形态解码、区域基址选择、
//! 请求头常量（方案第 3.1、3.3 节）。
//!
//! 字段名与真实 SubPanel handler/DTO 核对（`backend/internal/handler/auth_handler.go`、
//! `backend/internal/handler/dto/settings.go`、`backend/internal/pkg/response/response.go`，
//! 基线提交 `60e00724b`），不是按方案文字重新臆测的占位符。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use super::region::Region;

/// 固定的 User-Agent：跨版本、跨系统不变（session binding 以 UA+IP 为指纹，
/// UA 变化会撤销整个 refresh 家族，方案第 3.1 节）。
pub const USER_AGENT: &str = "WE2AI-Desktop";

/// `X-We2ai-Client: desktop/<version>/<os>`，版本与系统信息放这里而不是 UA。
pub fn client_header_value(app_version: &str) -> String {
    format!("desktop/{app_version}/{}", std::env::consts::OS)
}

/// 四种错误响应形态解码后的统一错误码（方案第 3.1 节）：
/// - 有 `reason` 取 `reason`
/// - 否则 `code` 为字符串时取 `code`
/// - 都没有时按 HTTP 状态码处理
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    /// 业务错误码（来自 `reason` 或字符串 `code`），如 `TOKEN_EXPIRED`。
    Named(String),
    /// 无法识别业务错误码时，退化为 HTTP 状态码本身。
    Status(u16),
}

impl ErrorCode {
    pub fn as_named(&self) -> Option<&str> {
        match self {
            ErrorCode::Named(s) => Some(s.as_str()),
            ErrorCode::Status(_) => None,
        }
    }
}

/// 从响应体与 HTTP 状态码解码出 [`ErrorCode`]。`body` 为 `None`（如非 JSON 响应体、
/// 空响应体）时按 HTTP 状态码处理。
pub fn decode_error_code(status: u16, body: Option<&Value>) -> ErrorCode {
    let Some(body) = body else {
        return ErrorCode::Status(status);
    };
    if let Some(reason) = body.get("reason").and_then(Value::as_str) {
        if !reason.is_empty() {
            return ErrorCode::Named(reason.to_string());
        }
    }
    if let Some(code_str) = body.get("code").and_then(Value::as_str) {
        if !code_str.is_empty() {
            return ErrorCode::Named(code_str.to_string());
        }
    }
    ErrorCode::Status(status)
}

/// 解码后的 API 错误：附带原始 HTTP 状态码（分类"未知 401"等场景需要）与展示用
/// message（后端不保证 `message` 一定存在，取不到时给出通用兜底文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ApiError {}

/// 网络层错误（连接失败、超时、DNS 等）：与 [`ApiError`]（服务端已返回响应）区分，
/// 因为失败分类里网络错误一律保留凭证退避重试，不看错误码。
#[derive(Debug, Clone)]
pub struct NetworkError(pub String);

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for NetworkError {}

#[derive(Debug, Clone)]
pub enum ApiCallError {
    /// 服务端已返回响应，但业务失败（含四种错误形态）。
    Api(ApiError),
    /// 请求根本没有到达 / 没有收到响应（断网、超时、TLS 失败等）。
    Network(NetworkError),
}

impl std::fmt::Display for ApiCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiCallError::Api(e) => write!(f, "{e}"),
            ApiCallError::Network(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ApiCallError {}

/// 验证码票据，按 provider 结构化（方案第 5.1 节 / SubPanel B2、B3）。
///
/// 字段名对应 SubPanel 实际请求 DTO（`LoginRequest` / `SendSmsCodeRequest` /
/// `PhoneLoginRequest`，`backend/internal/handler/auth_handler.go`）：
/// - Turnstile → `turnstile_token`
/// - 阿里云 → 同样放进 `turnstile_token`（服务端 `VerifyCaptcha` 按当前启用的
///   provider 解释该字段，不是三个字段并存）
/// - 腾讯 → `tencent_captcha_ticket` + `tencent_captcha_randstr`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptchaTicket {
    Turnstile(String),
    Tencent { ticket: String, randstr: String },
    Aliyun(String),
}

impl CaptchaTicket {
    /// 把票据写入请求体（`serde_json::Map`），供登录 / 发短信 / 手机登录请求复用。
    fn apply_to(&self, map: &mut serde_json::Map<String, Value>) {
        match self {
            CaptchaTicket::Turnstile(token) | CaptchaTicket::Aliyun(token) => {
                map.insert("turnstile_token".to_string(), Value::String(token.clone()));
            }
            CaptchaTicket::Tencent { ticket, randstr } => {
                map.insert(
                    "tencent_captcha_ticket".to_string(),
                    Value::String(ticket.clone()),
                );
                map.insert(
                    "tencent_captcha_randstr".to_string(),
                    Value::String(randstr.clone()),
                );
            }
        }
    }
}

/// `GET /api/v1/settings/public` 中与验证码相关的子集（其余公开设置字段本客户端
/// 用不到，不在此结构体中列出）。字段名对应 `dto.PublicSettings`
/// （`backend/internal/handler/dto/settings.go`）。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CaptchaPublicSettings {
    #[serde(default)]
    pub turnstile_enabled: bool,
    #[serde(default)]
    pub turnstile_site_key: String,
    #[serde(default)]
    pub tencent_captcha_enabled: bool,
    #[serde(default)]
    pub tencent_captcha_app_id: String,
    #[serde(default)]
    pub tencent_captcha_region: String,
    #[serde(default)]
    pub aliyun_captcha_enabled: bool,
    #[serde(default)]
    pub aliyun_captcha_scene_id: String,
    #[serde(default)]
    pub aliyun_captcha_prefix: String,
    #[serde(default)]
    pub aliyun_captcha_region: String,
    #[serde(default)]
    pub backend_mode_enabled: bool,
}

/// 验证码 provider 标识，对应 `/desktop/captcha?p=<provider>` 的取值
/// （`turnstile` | `tencent` | `aliyun`，见 SubPanel `DesktopCaptchaView.vue`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptchaProvider {
    Turnstile,
    Tencent,
    Aliyun,
}

impl CaptchaProvider {
    pub fn as_query_value(self) -> &'static str {
        match self {
            CaptchaProvider::Turnstile => "turnstile",
            CaptchaProvider::Tencent => "tencent",
            CaptchaProvider::Aliyun => "aliyun",
        }
    }

    pub fn from_query_value(value: &str) -> Option<Self> {
        match value {
            "turnstile" => Some(CaptchaProvider::Turnstile),
            "tencent" => Some(CaptchaProvider::Tencent),
            "aliyun" => Some(CaptchaProvider::Aliyun),
            _ => None,
        }
    }
}

impl CaptchaPublicSettings {
    /// 当前唯一启用的验证码 provider；线上三家同时开启不是受支持的配置，取第一个
    /// 命中的（Turnstile 优先，其次腾讯，再次阿里云），与前端 `actionProvider`
    /// 判定顺序一致（`DesktopCaptchaView.vue`）。任一家开启才需要弹验证码窗口。
    pub fn active_provider(&self) -> Option<CaptchaProvider> {
        if self.turnstile_enabled {
            Some(CaptchaProvider::Turnstile)
        } else if self.tencent_captcha_enabled {
            Some(CaptchaProvider::Tencent)
        } else if self.aliyun_captcha_enabled {
            Some(CaptchaProvider::Aliyun)
        } else {
            None
        }
    }
}

/// 登录 / 2FA / 手机登录成功时的令牌对（对应 `AuthResponse` /
/// `RefreshTokenResponse`）。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: i64,
}

/// 用户资料（`GET /api/v1/user/profile`）中本客户端需要的字段（`id`/`email`
/// 用于钥匙串 account 与会话索引的 `email_masked`）。
#[derive(Debug, Clone, Deserialize)]
pub struct UserProfile {
    pub id: i64,
    pub email: String,
}

/// `POST /api/v1/auth/login` 的两种成功结果。
#[derive(Debug, Clone)]
pub enum LoginResult {
    Tokens(TokenPair),
    Requires2fa {
        temp_token: String,
        email_masked: String,
    },
}

/// `POST /api/v1/auth/logout` 响应。
#[derive(Debug, Clone, Deserialize)]
pub struct LogoutResult {
    #[serde(default)]
    pub revoked: bool,
}

pub struct ApiClient {
    http: reqwest::Client,
    region: Region,
    /// 实际请求使用的基址。生产环境恒等于 `region.base_url()`；测试期通过
    /// [`ApiClient::new_with_base_url`] 指向 `wiremock` mock server，从而能对
    /// 真实的 HTTP 往返（而不只是纯函数）做端到端断言。
    base_url: String,
    client_header: String,
}

/// 进程级共享的 `reqwest::Client`。`reqwest::Client` 内部已经是
/// `Arc` 包装，`.clone()` 廉价且共享连接池/TLS 会话缓存；此前
/// `ApiClient::new*` 每次调用都 `Client::builder()...build()`，相当于每次
/// 登录/刷新/登出请求都重新建一次连接池，白白丢弃了 keep-alive 复用的机会
/// （Codex 代码评审低危项 1）。用 `once_cell::sync::Lazy` 在整个进程生命周期
/// 内只构建一次。
///
/// 代理策略：不调用 `.no_proxy()`，因此走 reqwest 默认行为——遵循系统代理
/// 设置与 `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` 环境变量。WE2AI 客户端本身
/// 不提供独立的全局代理配置项（不同于 cc-switch 上游的出站代理设置，那是
/// 给工具调用配的，不影响这里对 SubPanel 的登录/会话请求）。
///
/// 测试构建下不保留空闲连接：每个 `#[tokio::test]` 有自己的运行时，而
/// hyper 连接池里的连接绑定在建立它的运行时上；该测试结束、运行时关闭后，
/// 另一个测试复用这条池化连接会直接得到连接错误，全量并行跑测时表现为
/// 随机的 `Transient`（曾导致 `same_account_double_login_race_...` 偶发
/// 失败）。生产进程只有一个运行时，保留连接复用。
static SHARED_HTTP_CLIENT: once_cell::sync::Lazy<reqwest::Client> =
    once_cell::sync::Lazy::new(|| {
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(USER_AGENT);
        #[cfg(test)]
        let builder = builder.pool_max_idle_per_host(0);
        builder
            .build()
            .expect("failed to build we2ai reqwest client")
    });

impl ApiClient {
    pub fn new(region: Region, app_version: &str) -> Self {
        Self::new_with_base_url(region, app_version, region.base_url().to_string())
    }

    /// 区域仍决定失败分类等语义，但请求实际发往 `base_url`。生产路径下恒为
    /// `region.base_url()`（见 [`ApiClient::new`]）；测试期指向
    /// `wiremock::MockServer::uri()`，从而能对真实 HTTP 往返做端到端断言，
    /// 而不只是测纯函数。
    pub(crate) fn new_with_base_url(region: Region, app_version: &str, base_url: String) -> Self {
        Self {
            http: SHARED_HTTP_CLIENT.clone(),
            region,
            base_url,
            client_header: client_header_value(app_version),
        }
    }

    pub fn region(&self) -> Region {
        self.region
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header("X-We2ai-Client", &self.client_header)
    }

    /// 统一处理响应：2xx 解析 `data` 字段为 `T`；非 2xx 按四种错误形态解码。
    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<T, ApiCallError> {
        let response = builder
            .send()
            .await
            .map_err(|e| ApiCallError::Network(NetworkError(e.to_string())))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| ApiCallError::Network(NetworkError(e.to_string())))?;
        let body: Option<Value> = serde_json::from_str(&text).ok();

        if (200..300).contains(&status) {
            let data = body
                .as_ref()
                .and_then(|b| b.get("data"))
                .cloned()
                .unwrap_or(Value::Null);
            serde_json::from_value(data).map_err(|e| {
                ApiCallError::Api(ApiError {
                    code: ErrorCode::Status(status),
                    status,
                    message: format!("解析响应失败: {e}"),
                })
            })
        } else {
            let code = decode_error_code(status, body.as_ref());
            let message = body
                .as_ref()
                .and_then(|b| b.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("请求失败")
                .to_string();
            Err(ApiCallError::Api(ApiError {
                code,
                status,
                message,
            }))
        }
    }

    pub async fn get_public_settings(&self) -> Result<CaptchaPublicSettings, ApiCallError> {
        self.send(self.request(reqwest::Method::GET, "/api/v1/settings/public"))
            .await
    }

    pub async fn login_email(
        &self,
        email: &str,
        password: &str,
        captcha: Option<&CaptchaTicket>,
    ) -> Result<LoginResult, ApiCallError> {
        let mut body = serde_json::Map::new();
        body.insert("email".to_string(), Value::String(email.to_string()));
        body.insert("password".to_string(), Value::String(password.to_string()));
        if let Some(ticket) = captcha {
            ticket.apply_to(&mut body);
        }
        let value: Value = self
            .send(
                self.request(reqwest::Method::POST, "/api/v1/auth/login")
                    .json(&body),
            )
            .await?;
        parse_login_response(value)
    }

    pub async fn login_2fa(
        &self,
        temp_token: &str,
        totp_code: &str,
    ) -> Result<TokenPair, ApiCallError> {
        let body = serde_json::json!({
            "temp_token": temp_token,
            "totp_code": totp_code,
        });
        self.send(
            self.request(reqwest::Method::POST, "/api/v1/auth/login/2fa")
                .json(&body),
        )
        .await
    }

    pub async fn send_sms_code(
        &self,
        phone: &str,
        captcha: Option<&CaptchaTicket>,
    ) -> Result<(), ApiCallError> {
        let mut body = serde_json::Map::new();
        body.insert("phone".to_string(), Value::String(phone.to_string()));
        if let Some(ticket) = captcha {
            ticket.apply_to(&mut body);
        }
        let _: Value = self
            .send(
                self.request(reqwest::Method::POST, "/api/v1/auth/send-sms-code")
                    .json(&body),
            )
            .await?;
        Ok(())
    }

    pub async fn login_phone(
        &self,
        phone: &str,
        code: &str,
        captcha: Option<&CaptchaTicket>,
    ) -> Result<TokenPair, ApiCallError> {
        let mut body = serde_json::Map::new();
        body.insert("phone".to_string(), Value::String(phone.to_string()));
        body.insert("code".to_string(), Value::String(code.to_string()));
        if let Some(ticket) = captcha {
            ticket.apply_to(&mut body);
        }
        self.send(
            self.request(reqwest::Method::POST, "/api/v1/auth/phone-login")
                .json(&body),
        )
        .await
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, ApiCallError> {
        let body = serde_json::json!({ "refresh_token": refresh_token });
        self.send(
            self.request(reqwest::Method::POST, "/api/v1/auth/refresh")
                .json(&body),
        )
        .await
    }

    /// 登出：`refresh_token` 为 `None` 时服务端不撤销任何家族（方案第 3.3 节），
    /// 仍会发起请求（用于清本地状态前尽力通知服务端），但调用方通常应始终带上
    /// 当前 refresh token。
    pub async fn logout(&self, refresh_token: Option<&str>) -> Result<LogoutResult, ApiCallError> {
        let body = match refresh_token {
            Some(token) => serde_json::json!({ "refresh_token": token }),
            None => serde_json::json!({}),
        };
        self.send(
            self.request(reqwest::Method::POST, "/api/v1/auth/logout")
                .json(&body),
        )
        .await
    }

    pub async fn get_profile(&self, access_token: &str) -> Result<UserProfile, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/user/profile")
                .bearer_auth(access_token),
        )
        .await
    }

    /// 供已登录后受保护接口使用的通用 GET，外部按需扩展（P2 阶段仅 `get_profile`
    /// 使用受保护接口，此处保留供 session.rs 的失败分类测试复用同一套 send 逻辑）。
    #[cfg(test)]
    pub(crate) async fn get_authed(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<Value, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, path)
                .bearer_auth(access_token),
        )
        .await
    }

    /// 同上，POST 版本——供 `call_protected` 的 `idempotent=false` 分支测试
    /// 复用同一套 send 逻辑（非幂等请求遇瞬时错误不能自动重放）。
    #[cfg(test)]
    pub(crate) async fn post_authed(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<Value, ApiCallError> {
        self.send(
            self.request(reqwest::Method::POST, path)
                .bearer_auth(access_token),
        )
        .await
    }
}

fn parse_login_response(value: Value) -> Result<LoginResult, ApiCallError> {
    let requires_2fa = value
        .get("requires_2fa")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if requires_2fa {
        let temp_token = value
            .get("temp_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let email_masked = value
            .get("user_email_masked")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok(LoginResult::Requires2fa {
            temp_token,
            email_masked,
        });
    }
    let tokens: TokenPair = serde_json::from_value(value).map_err(|e| {
        ApiCallError::Api(ApiError {
            code: ErrorCode::Status(200),
            status: 200,
            message: format!("解析登录响应失败: {e}"),
        })
    })?;
    Ok(LoginResult::Tokens(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_agent_constant_has_no_version() {
        // 固定字符串，不随应用版本或系统变化（版本/系统信息放
        // `X-We2ai-Client` 头，见 `client_header_value`）——不是"不含数字"
        // （"WE2AI" 本身含数字 2），而是不随构建变化，逐字节比对即可断言。
        assert_eq!(USER_AGENT, "WE2AI-Desktop");
        assert!(
            !USER_AGENT.contains('.'),
            "must not look like a version string"
        );
    }

    #[test]
    fn client_header_includes_version_and_os() {
        let header = client_header_value("4.20.4");
        assert!(header.starts_with("desktop/4.20.4/"));
        assert!(header.ends_with(std::env::consts::OS));
    }

    // 四种错误响应形态解码（方案第 3.1 节）。
    #[test]
    fn decodes_shape_one_business_error_with_reason() {
        let body = json!({"code": 401, "message": "expired", "reason": "TOKEN_EXPIRED"});
        assert_eq!(
            decode_error_code(401, Some(&body)),
            ErrorCode::Named("TOKEN_EXPIRED".to_string())
        );
    }

    #[test]
    fn decodes_shape_two_middleware_error_with_string_code() {
        let body = json!({"code": "TOKEN_REVOKED", "message": "revoked"});
        assert_eq!(
            decode_error_code(401, Some(&body)),
            ErrorCode::Named("TOKEN_REVOKED".to_string())
        );
    }

    #[test]
    fn decodes_shape_three_generic_error_falls_back_to_status() {
        let body = json!({"code": 400, "message": "bad request"});
        assert_eq!(decode_error_code(400, Some(&body)), ErrorCode::Status(400));
    }

    #[test]
    fn decodes_shape_four_rate_limit_without_code_falls_back_to_status() {
        let body = json!({"error": "rate limited", "message": "too many requests"});
        assert_eq!(decode_error_code(429, Some(&body)), ErrorCode::Status(429));
    }

    #[test]
    fn decodes_missing_body_by_status() {
        assert_eq!(decode_error_code(500, None), ErrorCode::Status(500));
    }

    #[test]
    fn reason_takes_priority_over_string_code() {
        // 理论上不会同时出现，但解码优先级必须是 reason 优先。
        let body = json!({"code": "SOMETHING_ELSE", "reason": "TOKEN_EXPIRED", "message": "x"});
        assert_eq!(
            decode_error_code(401, Some(&body)),
            ErrorCode::Named("TOKEN_EXPIRED".to_string())
        );
    }

    #[test]
    fn numeric_code_without_reason_is_not_named() {
        let body = json!({"code": 403, "message": "forbidden"});
        assert_eq!(decode_error_code(403, Some(&body)), ErrorCode::Status(403));
    }

    #[test]
    fn active_provider_prefers_turnstile_then_tencent_then_aliyun() {
        let mut settings = CaptchaPublicSettings {
            aliyun_captcha_enabled: true,
            ..Default::default()
        };
        assert_eq!(settings.active_provider(), Some(CaptchaProvider::Aliyun));
        settings.tencent_captcha_enabled = true;
        assert_eq!(settings.active_provider(), Some(CaptchaProvider::Tencent));
        settings.turnstile_enabled = true;
        assert_eq!(settings.active_provider(), Some(CaptchaProvider::Turnstile));
    }

    #[test]
    fn active_provider_is_none_when_all_disabled() {
        let settings = CaptchaPublicSettings::default();
        assert_eq!(settings.active_provider(), None);
    }

    #[test]
    fn captcha_ticket_maps_to_expected_request_fields() {
        let mut map = serde_json::Map::new();
        CaptchaTicket::Turnstile("tok".into()).apply_to(&mut map);
        assert_eq!(
            map.get("turnstile_token").and_then(Value::as_str),
            Some("tok")
        );

        let mut map = serde_json::Map::new();
        CaptchaTicket::Aliyun("param-val".into()).apply_to(&mut map);
        assert_eq!(
            map.get("turnstile_token").and_then(Value::as_str),
            Some("param-val")
        );

        let mut map = serde_json::Map::new();
        CaptchaTicket::Tencent {
            ticket: "t".into(),
            randstr: "r".into(),
        }
        .apply_to(&mut map);
        assert_eq!(
            map.get("tencent_captcha_ticket").and_then(Value::as_str),
            Some("t")
        );
        assert_eq!(
            map.get("tencent_captcha_randstr").and_then(Value::as_str),
            Some("r")
        );
    }

    #[test]
    fn parse_login_response_detects_requires_2fa() {
        let value = json!({
            "requires_2fa": true,
            "temp_token": "tmp-123",
            "user_email_masked": "a***@b.com"
        });
        match parse_login_response(value).unwrap() {
            LoginResult::Requires2fa {
                temp_token,
                email_masked,
            } => {
                assert_eq!(temp_token, "tmp-123");
                assert_eq!(email_masked, "a***@b.com");
            }
            LoginResult::Tokens(_) => panic!("expected requires_2fa branch"),
        }
    }

    #[test]
    fn parse_login_response_detects_token_pair() {
        let value = json!({
            "access_token": "a",
            "refresh_token": "r",
            "expires_in": 3600,
            "token_type": "Bearer"
        });
        match parse_login_response(value).unwrap() {
            LoginResult::Tokens(pair) => {
                assert_eq!(pair.access_token, "a");
                assert_eq!(pair.refresh_token, "r");
                assert_eq!(pair.expires_in, 3600);
            }
            LoginResult::Requires2fa { .. } => panic!("expected tokens branch"),
        }
    }

    async fn mock_client(region: Region) -> (wiremock::MockServer, ApiClient) {
        let server = wiremock::MockServer::start().await;
        let client = ApiClient::new_with_base_url(region, "test", server.uri());
        (server, client)
    }

    #[tokio::test]
    async fn get_public_settings_decodes_real_envelope_via_mock_server() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (server, client) = mock_client(Region::International).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/settings/public"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "message": "success",
                "data": {
                    "turnstile_enabled": true,
                    "turnstile_site_key": "site-key-123",
                    "backend_mode_enabled": false
                }
            })))
            .mount(&server)
            .await;

        let settings = client.get_public_settings().await.unwrap();
        assert!(settings.turnstile_enabled);
        assert_eq!(settings.turnstile_site_key, "site-key-123");
        assert!(!settings.backend_mode_enabled);
    }

    #[tokio::test]
    async fn login_email_decodes_requires_2fa_via_mock_server() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (server, client) = mock_client(Region::International).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "message": "success",
                "data": {
                    "requires_2fa": true,
                    "temp_token": "tmp-abc",
                    "user_email_masked": "u***@we2ai.com"
                }
            })))
            .mount(&server)
            .await;

        match client.login_email("u@we2ai.com", "pw", None).await.unwrap() {
            LoginResult::Requires2fa {
                temp_token,
                email_masked,
            } => {
                assert_eq!(temp_token, "tmp-abc");
                assert_eq!(email_masked, "u***@we2ai.com");
            }
            LoginResult::Tokens(_) => panic!("expected requires_2fa"),
        }
    }

    #[tokio::test]
    async fn login_email_with_captcha_sends_turnstile_token_field() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (server, client) = mock_client(Region::International).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(body_json(json!({
                "email": "u@we2ai.com",
                "password": "pw",
                "turnstile_token": "captcha-tok"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "message": "success",
                "data": {
                    "access_token": "a",
                    "refresh_token": "r",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                }
            })))
            .mount(&server)
            .await;

        let ticket = CaptchaTicket::Turnstile("captcha-tok".to_string());
        let result = client
            .login_email("u@we2ai.com", "pw", Some(&ticket))
            .await
            .unwrap();
        match result {
            LoginResult::Tokens(pair) => assert_eq!(pair.access_token, "a"),
            LoginResult::Requires2fa { .. } => panic!("expected tokens"),
        }
    }

    #[tokio::test]
    async fn refresh_decodes_middleware_error_shape_as_named_code() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (server, client) = mock_client(Region::International).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "REFRESH_TOKEN_REUSED",
                "message": "refresh token reused"
            })))
            .mount(&server)
            .await;

        let err = client.refresh("stale-token").await.unwrap_err();
        match err {
            ApiCallError::Api(api_err) => {
                assert_eq!(api_err.status, 401);
                assert_eq!(
                    api_err.code,
                    ErrorCode::Named("REFRESH_TOKEN_REUSED".to_string())
                );
            }
            ApiCallError::Network(_) => panic!("expected api error"),
        }
    }

    #[tokio::test]
    async fn logout_decodes_revoked_flag() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (server, client) = mock_client(Region::International).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "message": "success",
                "data": {"message": "Logged out successfully", "revoked": true}
            })))
            .mount(&server)
            .await;

        let result = client.logout(Some("refresh-token")).await.unwrap();
        assert!(result.revoked);
    }

    #[tokio::test]
    async fn network_error_is_distinguished_from_api_error() {
        // 指向一个不存在的本地端口，模拟断网 / 连接失败。
        let client = ApiClient::new_with_base_url(
            Region::International,
            "test",
            "http://127.0.0.1:1".to_string(),
        );
        let err = client.get_public_settings().await.unwrap_err();
        assert!(matches!(err, ApiCallError::Network(_)));
    }
}
