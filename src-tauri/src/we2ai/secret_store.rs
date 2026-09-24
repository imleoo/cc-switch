//! refresh_token 的系统钥匙串存储抽象（方案第 5.2 节）。
//!
//! 抽成 trait 而不是让 `session.rs` 直接依赖 `keyring` crate，是为了能在单测里
//! 注入"钥匙串锁定 / 不可用 / 拒绝访问"等失败场景——真实系统钥匙串在 CI /
//! 沙箱环境里往往不可用或会弹出授权提示，不适合被单测依赖。

use std::fmt;

/// 钥匙串操作失败。不区分"锁定 / 不可用 / 拒绝访问"的具体子类型——方案第 5.2
/// 节对这三种情况的处理完全一致（退化为本次会话内存保存并提示），调用方不需要
/// 区分。
#[derive(Debug, Clone)]
pub struct SecretStoreError(pub String);

impl fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "系统钥匙串不可用: {}", self.0)
    }
}

impl std::error::Error for SecretStoreError {}

/// 系统钥匙串抽象。`service`/`account` 对应方案第 5.2 节：
/// `service = "com.we2ai.desktop"`，`account = "{region}:{user_id}"`。
pub trait SecretStore: Send + Sync {
    /// 不存在对应条目时返回 `Ok(None)`，与"钥匙串不可用"（`Err`）区分。
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretStoreError>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError>;
    /// 不存在对应条目时视为成功（幂等）。
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretStoreError>;
}

/// 生产环境实现：基于 `keyring` crate 的系统钥匙串（macOS Keychain / Windows
/// Credential Manager / Linux Secret Service，见 `Cargo.toml` 对应 target 的
/// feature 选择）。
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretStoreError> {
        let entry =
            keyring::Entry::new(service, account).map_err(|e| SecretStoreError(e.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretStoreError(e.to_string())),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        let entry =
            keyring::Entry::new(service, account).map_err(|e| SecretStoreError(e.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|e| SecretStoreError(e.to_string()))
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), SecretStoreError> {
        let entry =
            keyring::Entry::new(service, account).map_err(|e| SecretStoreError(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretStoreError(e.to_string())),
        }
    }
}

/// 钥匙串 service 常量（方案第 5.2 节）。
pub const SERVICE_NAME: &str = "com.we2ai.desktop";

/// 钥匙串 account 字段：`{region}:{user_id}`。
pub fn account_key(region_storage_key: &str, user_id: i64) -> String {
    format!("{region_storage_key}:{user_id}")
}

#[cfg(test)]
pub mod test_support {
    //! 仅测试使用的内存钥匙串实现，可注入"写入失败 / 读取失败 / 删除失败"，
    //! 用于覆盖方案第 5.2 节"钥匙串写入失败进入需重新登录"等场景，而不依赖
    //! 真实系统钥匙串。

    use super::{SecretStore, SecretStoreError};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::Mutex;

    /// 一次性阻塞门：`set()`/`get()`/`delete()` 进入阻塞时先通过 `reached`
    /// 通知测试"已经到达阻塞点"（测试用 `BlockHandle::wait_reached` 异步
    /// 等待，不用 sleep 猜时机——Codex 代码评审第 5 轮低危项 6），再在
    /// `release` 上等待测试放行。
    struct BlockGate {
        reached: Sender<()>,
        release: Receiver<()>,
    }

    /// 测试持有的句柄：先 `wait_reached().await` 确定阻塞的调用已经真正卡在
    /// 阻塞点，拿到 [`ReleaseHandle`] 后再决定何时 `release()`。
    pub struct BlockHandle {
        reached_rx: Receiver<()>,
        release_tx: Sender<()>,
    }

    impl BlockHandle {
        /// 异步等待阻塞点被真正到达（把 std 阻塞 `recv()` 丢给
        /// `spawn_blocking`，不占用/阻塞 tokio 工作线程或当前测试任务）。
        pub async fn wait_reached(self) -> ReleaseHandle {
            let reached_rx = self.reached_rx;
            tokio::task::spawn_blocking(move || {
                let _ = reached_rx.recv();
            })
            .await
            .expect("blocking wait-for-reached task panicked");
            ReleaseHandle {
                release_tx: self.release_tx,
            }
        }
    }

    /// 放行阻塞调用。
    pub struct ReleaseHandle {
        release_tx: Sender<()>,
    }

    impl ReleaseHandle {
        pub fn release(self) {
            let _ = self.release_tx.send(());
        }
    }

    #[derive(Default)]
    pub struct InMemorySecretStore {
        data: Mutex<HashMap<(String, String), String>>,
        fail_get: AtomicBool,
        fail_set: AtomicBool,
        fail_delete: AtomicBool,
        /// 结构性重构（Codex 第 4/5 轮）交错测试用：见 [`BlockGate`]。这些
        /// 方法本身是同步的，调用方（`session.rs`）通过
        /// `tokio::task::spawn_blocking` 派发，因此阻塞的是阻塞线程池的
        /// 线程，不会卡住 tokio 工作线程或任何 `summary()`/`call_protected()`
        /// 之类的读路径——这正是要验证的架构性质。
        block_next_get: Mutex<Option<BlockGate>>,
        block_next_set: Mutex<Option<BlockGate>>,
        block_next_delete: Mutex<Option<BlockGate>>,
    }

    impl InMemorySecretStore {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn set_fail_get(&self, fail: bool) {
            self.fail_get.store(fail, Ordering::SeqCst);
        }

        pub fn set_fail_set(&self, fail: bool) {
            self.fail_set.store(fail, Ordering::SeqCst);
        }

        pub fn set_fail_delete(&self, fail: bool) {
            self.fail_delete.store(fail, Ordering::SeqCst);
        }

        pub fn contains(&self, service: &str, account: &str) -> bool {
            self.data
                .lock()
                .unwrap()
                .contains_key(&(service.to_string(), account.to_string()))
        }

        fn install_gate(slot: &Mutex<Option<BlockGate>>) -> BlockHandle {
            let (reached_tx, reached_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            *slot.lock().unwrap() = Some(BlockGate {
                reached: reached_tx,
                release: release_rx,
            });
            BlockHandle {
                reached_rx,
                release_tx,
            }
        }

        /// 让下一次 `get()` 调用阻塞，返回的 [`BlockHandle`] 先
        /// `.wait_reached().await` 确认已经卡在阻塞点，再决定何时 `release()`。
        pub fn block_next_get(&self) -> BlockHandle {
            Self::install_gate(&self.block_next_get)
        }

        /// 同上，针对 `set()`。
        pub fn block_next_set(&self) -> BlockHandle {
            Self::install_gate(&self.block_next_set)
        }

        /// 同上，针对 `delete()`。
        pub fn block_next_delete(&self) -> BlockHandle {
            Self::install_gate(&self.block_next_delete)
        }
    }

    fn wait_at_gate(slot: &Mutex<Option<BlockGate>>) {
        if let Some(gate) = slot.lock().unwrap().take() {
            let _ = gate.reached.send(());
            let _ = gate.release.recv();
        }
    }

    impl SecretStore for InMemorySecretStore {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretStoreError> {
            wait_at_gate(&self.block_next_get);
            if self.fail_get.load(Ordering::SeqCst) {
                return Err(SecretStoreError("simulated get failure".to_string()));
            }
            Ok(self
                .data
                .lock()
                .unwrap()
                .get(&(service.to_string(), account.to_string()))
                .cloned())
        }

        fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError> {
            wait_at_gate(&self.block_next_set);
            if self.fail_set.load(Ordering::SeqCst) {
                return Err(SecretStoreError("simulated set failure".to_string()));
            }
            self.data.lock().unwrap().insert(
                (service.to_string(), account.to_string()),
                secret.to_string(),
            );
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), SecretStoreError> {
            wait_at_gate(&self.block_next_delete);
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(SecretStoreError("simulated delete failure".to_string()));
            }
            self.data
                .lock()
                .unwrap()
                .remove(&(service.to_string(), account.to_string()));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::InMemorySecretStore;
    use super::*;

    #[test]
    fn account_key_formats_region_and_user_id() {
        assert_eq!(account_key("international", 42), "international:42");
    }

    #[test]
    fn in_memory_store_round_trips() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get("svc", "acct").unwrap(), None);
        store.set("svc", "acct", "secret").unwrap();
        assert_eq!(
            store.get("svc", "acct").unwrap(),
            Some("secret".to_string())
        );
        store.delete("svc", "acct").unwrap();
        assert_eq!(store.get("svc", "acct").unwrap(), None);
    }

    #[test]
    fn in_memory_store_can_simulate_failures() {
        let store = InMemorySecretStore::new();
        store.set_fail_set(true);
        assert!(store.set("svc", "acct", "secret").is_err());
        store.set_fail_set(false);
        store.set("svc", "acct", "secret").unwrap();

        store.set_fail_get(true);
        assert!(store.get("svc", "acct").is_err());

        store.set_fail_get(false);
        store.set_fail_delete(true);
        assert!(store.delete("svc", "acct").is_err());
    }

    #[test]
    fn delete_of_missing_entry_is_idempotent_success() {
        let store = InMemorySecretStore::new();
        assert!(store.delete("svc", "missing").is_ok());
    }
}
