//! 区域与地址（方案第 3.3 节）：一个安装包，登录页切换，记住上次。
//!
//! | 区域 | 地址 | 备注 |
//! |---|---|---|
//! | 国际版 | `https://api.we2ai.com` | 始终可选 |
//! | 国内版正式 | `https://api.wtgo.com.cn` | 始终可选 |
//! | 国内版测试 | `https://jiwu.wtgo.com.cn` | 仅开发构建可选（`cfg!(debug_assertions)`） |

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    International,
    DomesticProd,
    /// 国内版测试环境，仅开发构建可选（`is_available()` 在 release 构建下为 false）。
    DomesticDev,
}

impl Region {
    /// 区域基础地址（不含路径），不含尾部 `/`。
    pub fn base_url(self) -> &'static str {
        match self {
            Region::International => "https://api.we2ai.com",
            Region::DomesticProd => "https://api.wtgo.com.cn",
            Region::DomesticDev => "https://jiwu.wtgo.com.cn",
        }
    }

    /// 该区域在当前构建下是否可选。`DomesticDev` 仅开发构建可选。
    pub fn is_available(self) -> bool {
        match self {
            Region::DomesticDev => cfg!(debug_assertions),
            Region::International | Region::DomesticProd => true,
        }
    }

    /// 全部区域（含当前构建不可选的），用于识别配置里指向任一 WE2AI 网关的地址。
    pub fn all() -> [Region; 3] {
        [
            Region::International,
            Region::DomesticProd,
            Region::DomesticDev,
        ]
    }

    /// 当前构建下登录页可选择的区域列表，顺序即展示顺序。
    pub fn available_regions() -> Vec<Region> {
        [
            Region::International,
            Region::DomesticProd,
            Region::DomesticDev,
        ]
        .into_iter()
        .filter(|r| r.is_available())
        .collect()
    }

    /// 会话索引 / 钥匙串 account 字段使用的稳定字符串标识，不随展示文案变化。
    pub fn storage_key(self) -> &'static str {
        match self {
            Region::International => "international",
            Region::DomesticProd => "domestic_prod",
            Region::DomesticDev => "domestic_dev",
        }
    }

    pub fn from_storage_key(key: &str) -> Option<Region> {
        match key {
            "international" => Some(Region::International),
            "domestic_prod" => Some(Region::DomesticProd),
            "domestic_dev" => Some(Region::DomesticDev),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domestic_dev_only_available_in_debug_builds() {
        assert_eq!(Region::DomesticDev.is_available(), cfg!(debug_assertions));
        assert!(Region::International.is_available());
        assert!(Region::DomesticProd.is_available());
    }

    #[test]
    fn storage_key_round_trips() {
        for region in [
            Region::International,
            Region::DomesticProd,
            Region::DomesticDev,
        ] {
            assert_eq!(Region::from_storage_key(region.storage_key()), Some(region));
        }
        assert_eq!(Region::from_storage_key("bogus"), None);
    }

    #[test]
    fn available_regions_excludes_unavailable_ones() {
        let regions = Region::available_regions();
        assert!(regions.contains(&Region::International));
        assert!(regions.contains(&Region::DomesticProd));
        assert_eq!(
            regions.contains(&Region::DomesticDev),
            cfg!(debug_assertions)
        );
    }
}
