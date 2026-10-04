//! SubPanel HTTP 客户端：envelope 解包、四种错误响应形态解码、区域基址选择、
//! 请求头常量（方案第 3.1、3.3 节）。
//!
//! 字段名与真实 SubPanel handler/DTO 核对（`backend/internal/handler/auth_handler.go`、
//! `backend/internal/handler/dto/settings.go`、`backend/internal/pkg/response/response.go`，
//! 基线提交 `60e00724b`），不是按方案文字重新臆测的占位符。

use serde::{Deserialize, Deserializer, Serialize};
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

/// `GET /api/v1/user/profile` 中余额相关字段（美元）。所有字段都可能缺失
/// （旧版服务端或字段被裁剪），缺失按 0 处理由 `billing.rs` 负责。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteBalance {
    #[serde(default)]
    pub balance: Option<f64>,
    #[serde(default)]
    pub frozen_balance: Option<f64>,
    #[serde(default)]
    pub total_recharged: Option<f64>,
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

#[derive(Clone)]
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

    /// 余额（`GET /api/v1/user/profile` 的余额字段，充值与余额设计方案 P1）。
    pub async fn get_balance(&self, access_token: &str) -> Result<RemoteBalance, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/user/profile")
                .bearer_auth(access_token),
        )
        .await
    }

    /// Key 列表一页（方案 3.3 节）。`page` 从 1 开始。
    pub async fn list_keys(
        &self,
        access_token: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Paginated<RemoteApiKey>, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/keys")
                .query(&[("page", page), ("page_size", page_size)])
                .bearer_auth(access_token),
        )
        .await
    }

    /// Key 管理页列表一页（全部状态，不带 `status` 过滤；字段含额度/过期/
    /// 最近使用/分组倍率）。分页参数与 [`ApiClient::list_keys`] 相同。
    pub async fn list_managed_keys(
        &self,
        access_token: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Paginated<RemoteManagedKey>, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/keys")
                .query(&[("page", page), ("page_size", page_size)])
                .bearer_auth(access_token),
        )
        .await
    }

    /// 创建 Key（`POST /api/v1/keys`）。服务端要求 `Idempotency-Key` 头
    /// （`RequireKey: true`），同一个值 + 同一请求体重放返回首次结果。
    pub async fn create_key(
        &self,
        access_token: &str,
        idempotency_key: &str,
        body: &Value,
    ) -> Result<RemoteManagedKey, ApiCallError> {
        self.send(
            self.request(reqwest::Method::POST, "/api/v1/keys")
                .header("Idempotency-Key", idempotency_key)
                .bearer_auth(access_token)
                .json(body),
        )
        .await
    }

    /// 更新 Key（`PUT /api/v1/keys/:id`）。
    pub async fn update_key(
        &self,
        access_token: &str,
        key_id: i64,
        body: &Value,
    ) -> Result<RemoteManagedKey, ApiCallError> {
        self.send(
            self.request(reqwest::Method::PUT, &format!("/api/v1/keys/{key_id}"))
                .bearer_auth(access_token)
                .json(body),
        )
        .await
    }

    /// 删除 Key（`DELETE /api/v1/keys/:id`）。
    pub async fn delete_key(&self, access_token: &str, key_id: i64) -> Result<(), ApiCallError> {
        self.send::<Value>(
            self.request(reqwest::Method::DELETE, &format!("/api/v1/keys/{key_id}"))
                .bearer_auth(access_token),
        )
        .await
        .map(|_| ())
    }

    /// 当前用户可绑定的分组（`GET /api/v1/groups/available`）。
    pub async fn list_available_groups(
        &self,
        access_token: &str,
    ) -> Result<Vec<RemoteGroupOption>, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/groups/available")
                .bearer_auth(access_token),
        )
        .await
    }

    /// 当前用户的专属分组倍率（`GET /api/v1/groups/rates`）：`group_id → 倍率`，
    /// JSON 对象的键是字符串；没有专属倍率时服务端可能返回 `null`。
    pub async fn list_group_rates(
        &self,
        access_token: &str,
    ) -> Result<Option<std::collections::HashMap<String, f64>>, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/groups/rates")
                .bearer_auth(access_token),
        )
        .await
    }

    /// B1：某个 Key 可用的模型、每个模型支持的工具与 Key 级准入结果。
    pub async fn get_key_models(
        &self,
        access_token: &str,
        key_id: i64,
    ) -> Result<RemoteKeyModels, ApiCallError> {
        self.send(
            self.request(
                reqwest::Method::GET,
                &format!("/api/v1/desktop/keys/{key_id}/models"),
            )
            .bearer_auth(access_token),
        )
        .await
    }

    /// 当前用户可见的公告（`GET /api/v1/announcements`）。服务端已按生效时间窗
    /// 与定向条件过滤；`unread_only` 为 true 时只返回未读。
    pub async fn list_announcements(
        &self,
        access_token: &str,
        unread_only: bool,
    ) -> Result<Vec<RemoteAnnouncement>, ApiCallError> {
        self.send(
            self.request(reqwest::Method::GET, "/api/v1/announcements")
                .query(&[("unread_only", unread_only)])
                .bearer_auth(access_token),
        )
        .await
    }

    /// 把一条公告标记为已读（`POST /api/v1/announcements/:id/read`，服务端为
    /// upsert，重复调用无副作用）。
    pub async fn mark_announcement_read(
        &self,
        access_token: &str,
        id: i64,
    ) -> Result<(), ApiCallError> {
        self.send::<Value>(
            self.request(
                reqwest::Method::POST,
                &format!("/api/v1/announcements/{id}/read"),
            )
            .bearer_auth(access_token),
        )
        .await
        .map(|_| ())
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

/// `GET /api/v1/announcements` 的单项（SubPanel `dto.UserAnnouncement`）。
/// 时间字段保持 RFC3339 字符串，由使用方按需解析。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteAnnouncement {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    /// Markdown 正文。
    #[serde(default)]
    pub content: String,
    /// `"popup"` | `"silent"`；未识别的值按 `silent` 处理。
    #[serde(default)]
    pub notify_mode: String,
    #[serde(default)]
    pub starts_at: Option<String>,
    #[serde(default)]
    pub ends_at: Option<String>,
    #[serde(default)]
    pub read_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// SubPanel 分页响应 `data`（`response.Paginated`）。
#[derive(Debug, Clone, Deserialize)]
pub struct Paginated<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub page_size: i64,
    #[serde(default)]
    pub pages: i64,
}

/// `/api/v1/keys` 的单个 Key，只解析客户端用到的字段。`key` 是明文，
/// `Debug` 输出里隐去。
#[derive(Clone, Deserialize)]
pub struct RemoteApiKey {
    pub id: i64,
    pub key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub group: Option<RemoteGroup>,
}

impl std::fmt::Debug for RemoteApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteApiKey")
            .field("id", &self.id)
            .field("key", &"<redacted>")
            .field("name", &self.name)
            .field("status", &self.status)
            .field("group", &self.group)
            .finish()
    }
}

/// Key 管理页列表项（`dto.APIKey` 的子集，字段名对照
/// `SubPanel/backend/internal/handler/dto/types.go` 的 `APIKey`）。`key` 是明文，
/// `Debug` 输出里隐去。
#[derive(Clone, Deserialize)]
pub struct RemoteManagedKey {
    pub id: i64,
    pub key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub group: Option<RemoteManagedGroup>,
    /// 额度上限（美元，0 = 不限）。
    #[serde(default)]
    pub quota: Option<f64>,
    #[serde(default)]
    pub quota_used: Option<f64>,
    /// RFC3339；`null` = 永不过期。
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
}

impl std::fmt::Debug for RemoteManagedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteManagedKey")
            .field("id", &self.id)
            .field("key", &"<redacted>")
            .field("name", &self.name)
            .field("status", &self.status)
            .field("group", &self.group)
            .field("quota", &self.quota)
            .field("quota_used", &self.quota_used)
            .field("expires_at", &self.expires_at)
            .field("last_used_at", &self.last_used_at)
            .finish()
    }
}

/// Key 内嵌的分组（`dto.Group` 的子集）。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteManagedGroup {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rate_multiplier: Option<f64>,
}

/// `GET /groups/available` 的单项（`dto.Group` 的子集）。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteGroupOption {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub rate_multiplier: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteGroup {
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

/// B1 响应 `data`（SubPanel `DesktopKeyModelsResponse`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteKeyModels {
    #[serde(default)]
    pub models: Vec<RemoteKeyModel>,
    #[serde(default)]
    pub callable: bool,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    /// B1 定价扩展（`docs/we2ai/B1定价契约.md`）：Key 分组缺失或
    /// 倍率无法解析时服务端整体省略；旧服务端（未实现定价扩展）同样省略，
    /// 反序列化不报错。
    #[serde(default)]
    pub pricing: Option<RemotePricing>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteKeyModel {
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    /// 模型类型：`text`/`image`/`video`/`audio`。旧服务端不返回，为 `None`；
    /// 模型广场只隐藏明确为非 text 的模型，缺失时不过滤。客户端不自行按名称判定类型。
    #[serde(default)]
    pub kind: Option<String>,
    /// 方案 3.2 节 B1 可选能力字段，无数据时服务端省略。
    #[serde(default)]
    pub supports_tool_call: Option<bool>,
    #[serde(default)]
    pub supports_images: Option<bool>,
    #[serde(default)]
    pub reasoning_efforts: Option<Vec<String>>,
    /// 无法解析价格的模型服务端省略此字段，客户端显示"暂无定价"。
    #[serde(default)]
    pub price: Option<RemoteModelPrice>,
}

/// B1 定价扩展顶层 `pricing`：用户 × 分组倍率与当前高峰状态，供客户端把
/// `price` 里已乘倍率的美元单价换算成人民币展示。字段全部 `Option`——个别
/// 字段无法解析时该字段单独省略，而不是整个响应反序列化失败。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemotePricing {
    #[serde(default)]
    pub cny_rate: Option<f64>,
    #[serde(default)]
    pub rate_multiplier: Option<f64>,
    #[serde(default)]
    pub peak_multiplier: Option<f64>,
    #[serde(default)]
    pub peak_active: Option<bool>,
    #[serde(default)]
    pub effective_multiplier: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
}

/// B1 定价扩展每模型 `price`：与实际扣费同源的标准首档价，折后价已乘该模型
/// 实际 `multiplier`（旧服务端缺省时回退顶层 `effective_multiplier`），`base_*`
/// 是未乘倍率的原价，供客户端在倍率 ≠ 1 时显示划线价或倍率标注。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemoteModelPrice {
    #[serde(default)]
    pub billing_mode: Option<String>,
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
    #[serde(default)]
    pub per_request: Option<f64>,
    #[serde(default)]
    pub base_input: Option<f64>,
    #[serde(default)]
    pub base_output: Option<f64>,
    #[serde(default)]
    pub base_cache_read: Option<f64>,
    #[serde(default)]
    pub base_cache_write: Option<f64>,
    #[serde(default)]
    pub base_cache_write_1h: Option<f64>,
    #[serde(default)]
    pub base_per_request: Option<f64>,
    /// v2 契约新增：该模型实际扣费倍率（token 类 = `rate_multiplier ×
    /// peak_multiplier`；image/video 类是独立的图片/视频倍率，不叠加高峰）。
    /// 折后字段 = `base_* × multiplier`。v3 起服务端总是输出这个字段；
    /// 客户端仍按 `Option` 处理以兼容尚未升级到 v3 的旧服务端，缺失时
    /// 回退到顶层 `pricing.effective_multiplier`（`docs/we2ai/B1定价契约.md`）。
    #[serde(default)]
    pub multiplier: Option<f64>,
    /// v3 契约新增：按次计费的单位，`"request"`（字段缺省即此）或
    /// `"second"`（视频按秒计费）。类型是 `Option<Option<String>>` 而不是
    /// 单层 `Option<String>`（Codex 复验 C4）：需要区分"字段完全不存在"
    /// （外层 `None`，缺省按 `"request"` 处理）与"字段存在但值是
    /// `null`"（`Some(None)`，视为不可信，和未识别字符串一样不展示按次
    /// 行）——两者语义不同，但普通 `Option<String>` 反序列化时会把它们
    /// 都折叠成同一个 `None`，见 `deserialize_present_option`。未识别的
    /// 值（含显式 `null`/空字符串）最终由 `to_price_view` 归一化为
    /// `None`，前端据此不展示按次这一行，避免展示错误单位。
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub per_request_unit: Option<Option<String>>,
}

/// 只在字段**确实存在**于 JSON 里时才会被调用（哪怕值是 `null`）——
/// `#[serde(default)]` 负责"字段完全不存在"的情形（给出外层 `None`），
/// 这个函数只负责区分"存在但为 `null`"（返回 `Some(None)`）与"存在且有
/// 具体值"（返回 `Some(Some(t))`）。这是让 `Option<Option<T>>` 真正
/// 区分"缺失"与"显式 null"的标准写法（serde 默认对嵌套 `Option` 会把
/// 两者都折叠成外层 `None`，不会往内层传递）。
fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    // 先按 `Option<T>` 反序列化这个字段确实存在的值（`null` → `None`，
    // 真实值 → `Some(t)`），再整体包一层 `Some`，标记"这个字段确实出现
    // 在了 JSON 里"——两层 `Some`/`None` 分别对应"是否出现"和"是否为
    // null"两个独立维度，不能合并成一层。
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 写 WorkBuddy 条目用的可选能力（方案 4.3 节字段映射）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub supports_tool_call: Option<bool>,
    pub supports_images: Option<bool>,
    pub reasoning_efforts: Option<Vec<String>>,
}

impl RemoteKeyModel {
    pub fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tool_call: self.supports_tool_call,
            supports_images: self.supports_images,
            reasoning_efforts: self.reasoning_efforts.clone(),
        }
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

    // B1 定价扩展（`docs/we2ai/B1定价契约.md`）：字段齐全时正确解析，
    // 且 `base_*` 与折后价分开保留。
    #[test]
    fn remote_key_models_parses_full_pricing_extension() {
        let body = json!({
            "models": [{
                "id": "claude-sonnet-4-5",
                "provider": "anthropic",
                "tools": ["claude_code"],
                "price": {
                    "billing_mode": "token",
                    "input": 3.0,
                    "output": 15.0,
                    "cache_read": 0.3,
                    "base_input": 6.0,
                    "base_output": 30.0,
                    "base_cache_read": 0.6
                }
            }],
            "callable": true,
            "pricing": {
                "cny_rate": 7.2,
                "rate_multiplier": 0.5,
                "peak_multiplier": 1.0,
                "peak_active": false,
                "effective_multiplier": 0.5,
                "unit": "usd_per_1m_tokens"
            }
        });
        let parsed: RemoteKeyModels = serde_json::from_value(body).unwrap();
        let pricing = parsed.pricing.expect("pricing present");
        assert_eq!(pricing.cny_rate, Some(7.2));
        assert_eq!(pricing.effective_multiplier, Some(0.5));
        assert_eq!(pricing.peak_active, Some(false));
        let price = parsed.models[0].price.as_ref().expect("price present");
        assert_eq!(price.billing_mode.as_deref(), Some("token"));
        assert_eq!(price.input, Some(3.0));
        assert_eq!(price.base_input, Some(6.0));
        assert_eq!(price.cache_write, None);
        assert_eq!(price.multiplier, None, "field omitted by this fixture");
    }

    // v2 契约：模型级 `price.multiplier` 与顶层 `pricing.effective_multiplier`
    // 各自独立解析——image 类模型自己的倍率（0.5）与顶层倍率（2.0，来自分组
    // 高峰）不同，两者都必须原样保留，不能互相覆盖。
    #[test]
    fn remote_key_models_parses_per_model_multiplier_independent_of_top_level() {
        let body = json!({
            "models": [{
                "id": "image-gen-1",
                "tools": ["codex"],
                "price": {
                    "billing_mode": "image",
                    "per_request": 0.02,
                    "base_per_request": 0.04,
                    "multiplier": 0.5
                }
            }],
            "callable": true,
            "pricing": {
                "cny_rate": 7.2,
                "effective_multiplier": 2.0
            }
        });
        let parsed: RemoteKeyModels = serde_json::from_value(body).unwrap();
        assert_eq!(
            parsed.pricing.as_ref().and_then(|p| p.effective_multiplier),
            Some(2.0)
        );
        let price = parsed.models[0].price.as_ref().expect("price present");
        assert_eq!(price.billing_mode.as_deref(), Some("image"));
        assert_eq!(price.multiplier, Some(0.5), "model's own multiplier, not the top-level one");
    }

    // v3 契约：`per_request_unit` 正确解析（"request"/"second"两种取值），
    // 缺省时反序列化不报错（旧服务端兼容）。字段类型是
    // `Option<Option<String>>`（Codex 复验 C4）：外层区分"字段是否出现"，
    // 内层才是实际字符串值，详见
    // `remote_model_price_distinguishes_missing_null_and_empty_per_request_unit`。
    #[test]
    fn remote_model_price_parses_per_request_unit() {
        let with_second = json!({"per_request": 0.5, "per_request_unit": "second"});
        let parsed: RemoteModelPrice = serde_json::from_value(with_second).unwrap();
        assert_eq!(parsed.per_request_unit, Some(Some("second".to_string())));

        let missing = json!({"per_request": 0.5});
        let parsed: RemoteModelPrice = serde_json::from_value(missing).unwrap();
        assert_eq!(parsed.per_request_unit, None);
    }

    // 旧服务端（未实现定价扩展）响应里没有 `pricing`/`price`，反序列化不能
    // 报错，两者都应为 `None`（B1 契约"客户端展示规则"：无 pricing 整体不
    // 显示价格区）。
    #[test]
    fn remote_key_models_tolerates_missing_pricing_fields_for_old_servers() {
        let body = json!({
            "models": [{"id": "gpt-5", "tools": ["codex"]}],
            "callable": true
        });
        let parsed: RemoteKeyModels = serde_json::from_value(body).unwrap();
        assert!(parsed.pricing.is_none());
        assert!(parsed.models[0].price.is_none());
        assert!(parsed.models[0].kind.is_none());
    }

    // B1 模型类型：服务端返回 `kind`（以及客户端不使用的 `mode`）时解析为 `Some`，
    // 缺失时为 `None`（旧服务端），未知值原样保留由前端按"非 text 才隐藏"处理。
    #[test]
    fn remote_key_models_parses_kind_when_present() {
        let body = json!({
            "models": [
                {"id": "claude-sonnet-4-5", "tools": ["claude_code"], "mode": "chat", "kind": "text"},
                {"id": "jimeng_t2v_v30", "tools": ["codex"], "mode": "video_generation", "kind": "video"},
                {"id": "legacy", "tools": ["codex"]}
            ],
            "callable": true
        });
        let parsed: RemoteKeyModels = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.models[0].kind.as_deref(), Some("text"));
        assert_eq!(parsed.models[1].kind.as_deref(), Some("video"));
        assert_eq!(parsed.models[2].kind, None);
    }

    // 顶层 `pricing` 对象存在但个别倍率字段缺失：单独字段变 `None`，不影响
    // 其余字段解析、不报错（服务端"倍率无法解析"时的部分降级场景）。
    #[test]
    fn remote_pricing_tolerates_partial_fields() {
        let body = json!({
            "models": [],
            "callable": true,
            "pricing": {"cny_rate": 7.2}
        });
        let parsed: RemoteKeyModels = serde_json::from_value(body).unwrap();
        let pricing = parsed.pricing.expect("pricing present");
        assert_eq!(pricing.cny_rate, Some(7.2));
        assert_eq!(pricing.rate_multiplier, None);
        assert_eq!(pricing.peak_active, None);
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
