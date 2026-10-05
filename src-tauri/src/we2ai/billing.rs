//! 充值入口与余额（功能 20，见自定义开发功能列表.md）。
//!
//! 客户端不实现支付：充值、订单、兑换码统一复用 SubPanel Web 端（`{base}/purchase`、
//! `{base}/orders`），本模块只提供两件事：
//!
//! - `we2ai_get_balance`：经 `call_protected_api` 读 `GET /api/v1/user/profile` 的余额
//!   字段，会话续期与 401 终止处理与 `keys.rs` 一致。字段缺失、为 `null` 或非有限数
//!   一律按 0 处理，不让一个脏字段拖垮顶栏余额。
//! - `we2ai_gateway_info`：返回当前活动会话所在区域的 API 基础地址（`baseUrl`）和
//!   网站地址（`webUrl`），前端据此拼 `/purchase`、`/orders`、`/models`（用 `webUrl`）
//!   和代码示例（用 `baseUrl`），不硬编码域名。

use serde::Serialize;
use tauri::State;

use super::api::RemoteBalance;
use super::commands_auth::We2aiApiError;
use super::session::{SessionError, SessionManager, We2aiSessionState};

/// 余额视图（美元，camelCase）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BalanceView {
    pub balance: f64,
    pub frozen_balance: f64,
    pub total_recharged: f64,
}

/// 当前会话区域的 API 网关地址与网站地址（均不含尾部 `/`）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInfoView {
    pub base_url: String,
    pub web_url: String,
}

/// 缺失或非有限（`NaN`/`Infinity`）按 0；负数保留（欠费账户余额可能为负）。
fn finite_or_zero(v: Option<f64>) -> f64 {
    v.filter(|x| x.is_finite()).unwrap_or(0.0)
}

fn to_balance_view(remote: RemoteBalance) -> BalanceView {
    BalanceView {
        balance: finite_or_zero(remote.balance),
        frozen_balance: finite_or_zero(remote.frozen_balance),
        total_recharged: finite_or_zero(remote.total_recharged),
    }
}

pub async fn get_balance(manager: &SessionManager) -> Result<BalanceView, SessionError> {
    let (remote, _identity) = manager
        .call_protected_api(true, move |api, token| async move {
            api.get_balance(&token).await
        })
        .await?;
    Ok(to_balance_view(remote))
}

pub fn gateway_info(manager: &SessionManager) -> Result<GatewayInfoView, We2aiApiError> {
    let identity = manager
        .current_identity()
        .ok_or(SessionError::NoActiveSession)?;
    Ok(GatewayInfoView {
        base_url: identity.region.base_url().to_string(),
        web_url: identity.region.web_url().to_string(),
    })
}

#[tauri::command]
pub async fn we2ai_get_balance(
    session: State<'_, We2aiSessionState>,
) -> Result<BalanceView, We2aiApiError> {
    let manager = session.0.clone();
    Ok(get_balance(&manager).await?)
}

#[tauri::command]
pub async fn we2ai_gateway_info(
    session: State<'_, We2aiSessionState>,
) -> Result<GatewayInfoView, We2aiApiError> {
    gateway_info(&session.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::we2ai::region::Region;
    use crate::we2ai::secret_store::test_support::InMemorySecretStore;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn manager(dir: &TempDir) -> SessionManager {
        SessionManager::new(
            Arc::new(InMemorySecretStore::new()),
            dir.path().to_path_buf(),
            "test".to_string(),
        )
    }

    fn parse(value: serde_json::Value) -> BalanceView {
        to_balance_view(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn parses_all_balance_fields() {
        let view = parse(json!({
            "id": 42, "email": "u@we2ai.com",
            "balance": 12.48, "frozen_balance": 1.5, "total_recharged": 200.0
        }));
        assert_eq!(
            view,
            BalanceView {
                balance: 12.48,
                frozen_balance: 1.5,
                total_recharged: 200.0
            }
        );
    }

    #[test]
    fn missing_or_null_fields_default_to_zero() {
        let missing = parse(json!({"id": 42, "email": "u@we2ai.com"}));
        assert_eq!(
            missing,
            BalanceView {
                balance: 0.0,
                frozen_balance: 0.0,
                total_recharged: 0.0
            }
        );

        let partial = parse(json!({
            "balance": 3.0, "frozen_balance": null
        }));
        assert_eq!(partial.balance, 3.0);
        assert_eq!(partial.frozen_balance, 0.0);
        assert_eq!(partial.total_recharged, 0.0);
    }

    #[test]
    fn non_finite_values_become_zero_and_negative_balance_is_kept() {
        let view = to_balance_view(RemoteBalance {
            balance: Some(f64::NAN),
            frozen_balance: Some(f64::INFINITY),
            total_recharged: Some(5.0),
        });
        assert_eq!(view.balance, 0.0);
        assert_eq!(view.frozen_balance, 0.0);
        assert_eq!(view.total_recharged, 5.0);

        let negative = parse(json!({"balance": -0.37}));
        assert_eq!(negative.balance, -0.37);
    }

    #[test]
    fn serializes_as_camel_case() {
        let value = serde_json::to_value(BalanceView {
            balance: 1.0,
            frozen_balance: 2.0,
            total_recharged: 3.0,
        })
        .unwrap();
        assert_eq!(
            value,
            json!({"balance": 1.0, "frozenBalance": 2.0, "totalRecharged": 3.0})
        );
        let info = serde_json::to_value(GatewayInfoView {
            base_url: "https://api.we2ai.com".to_string(),
            web_url: "https://we2ai.com".to_string(),
        })
        .unwrap();
        assert_eq!(
            info,
            json!({"baseUrl": "https://api.we2ai.com", "webUrl": "https://we2ai.com"})
        );
    }

    #[tokio::test]
    async fn get_balance_reads_profile_with_bearer_token() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {
                    "id": 42, "email": "u@we2ai.com", "balance": 12.48,
                    "frozen_balance": 0, "total_recharged": 200.0
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());

        let view = get_balance(&manager).await.unwrap();

        assert_eq!(view.balance, 12.48);
        assert_eq!(view.frozen_balance, 0.0);
        assert_eq!(view.total_recharged, 200.0);
    }

    #[tokio::test]
    async fn get_balance_without_session_reports_no_active_session() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);

        let err = get_balance(&manager).await.unwrap_err();

        assert_eq!(err, SessionError::NoActiveSession);
        let api_err: We2aiApiError = err.into();
        assert_eq!(api_err.code, "NO_ACTIVE_SESSION");
    }

    #[tokio::test]
    async fn revoked_token_terminates_the_session_and_surfaces_the_code() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"code": "TOKEN_REVOKED", "message": "revoked"})),
            )
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());

        let err = get_balance(&manager).await.unwrap_err();

        assert_eq!(err, SessionError::Terminated("TOKEN_REVOKED".to_string()));
        assert!(manager.current_identity().is_none(), "会话应已终止");
    }

    #[test]
    fn gateway_info_follows_the_active_session_region() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        manager.test_seed_active(
            Region::International,
            42,
            "access-1",
            "http://unused".to_string(),
        );
        let intl = gateway_info(&manager).unwrap();
        assert_eq!(intl.base_url, "https://api.we2ai.com");
        assert_eq!(intl.web_url, "https://we2ai.com");

        manager.test_seed_active(
            Region::DomesticProd,
            42,
            "access-1",
            "http://unused".to_string(),
        );
        assert_eq!(
            gateway_info(&manager).unwrap().base_url,
            "https://api.wtgo.com.cn"
        );

        manager.test_seed_active(
            Region::DomesticDev,
            42,
            "access-1",
            "http://unused".to_string(),
        );
        assert_eq!(
            gateway_info(&manager).unwrap().base_url,
            "https://jiwu.wtgo.com.cn"
        );
    }

    #[test]
    fn gateway_info_without_session_reports_no_active_session() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);

        let err = gateway_info(&manager).unwrap_err();

        assert_eq!(err.code, "NO_ACTIVE_SESSION");
    }
}
