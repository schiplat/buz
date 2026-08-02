//! 统一日志脱敏入口 — "哪些值 / 哪些 key 属于敏感" 的唯一决策点。
//!
//! Manager 与 Runner 共用同一套类别与收集逻辑，新增敏感类别时不遗漏、不改错：
//!
//! - **Manager（数据生产方）** 在构造 job 下发 JSON 时，把需要按值打码的字段值
//!   （registry 密码、OSS 签名 URL 等）统一登记进 `log_mask_values`，并在生产该值
//!   处用 [`SecretClass`] 标注类别。
//! - **Runner（消费方）** 从 `secrets` keys、`masking_input_keys`、step `secrets:`
//!   与 `log_mask_values` 统一构造 [`SecretCatalog`]，再交由 [`mask_secrets`] 打码，
//!   不再感知具体敏感字段。
//!
//! 新增敏感类别三步：1) 在此枚举注册；2) 在 Manager 生产该值处登记；3) Runner 零改动。

use std::collections::{HashMap, HashSet};

/// 敏感信息类别注册表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretClass {
    /// Infrastructure Secret：step secrets / team-shared secrets。
    Secret,
    /// 触发输入中标记 `masking: true` 的 key。
    MaskingInput,
    /// Registry 密码（`registry_secret`）。
    RegistryPassword,
    /// OSS config 签名 URL（`config_source` → `BUZ_CONFIG_URL`）。
    ConfigUrl,
    /// SSH 主机凭据。
    SshKey,
}

/// 一次执行单元的敏感值注册表。
#[derive(Debug, Clone, Default)]
pub struct SecretCatalog {
    /// 登记为敏感的 env key：RUN echo 按 key 打码，且其 env 值加入打码值集合。
    keys: HashSet<String>,
    /// 直接登记的打码值：不依赖 env key，按值打码（registry 密码、签名 URL 等）。
    values: Vec<String>,
    /// 敏感但 echo 中只提示"已注入"的 key（显示 `KEY=<injected>`，不打印值、不打 `***`）。
    /// 值仍参与按值打码 —— 这只影响 RUN echo 的展示形态。
    injected_keys: HashSet<String>,
}

impl SecretCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个敏感 env key。
    pub fn add_key(&mut self, key: impl Into<String>) {
        self.keys.insert(key.into());
    }

    /// 批量登记敏感 env key。
    pub fn add_keys(&mut self, keys: impl IntoIterator<Item = String>) {
        self.keys.extend(keys);
    }

    /// 登记一个敏感但 echo 中只提示"已注入"的 key（如 `BUZ_CONFIG_URL`）。
    pub fn add_injected_key(&mut self, key: impl Into<String>) {
        let key = key.into();
        self.keys.insert(key.clone());
        self.injected_keys.insert(key);
    }

    /// 登记一个直接打码值（不依赖 env key）。
    pub fn add_value(&mut self, value: impl Into<String>) {
        self.values.push(value.into());
    }

    /// RUN echo 是否应对该 key 打码。
    pub fn is_secret_key(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// RUN echo 是否应对该 key 显示"已注入"提示而非 `***`。
    pub fn is_injected_key(&self, key: &str) -> bool {
        self.injected_keys.contains(key)
    }

    /// 汇总本次执行的所有打码值：敏感 key 在 env 中的值 + 直接登记值（跳过空值）。
    pub fn secret_values(&self, env: &HashMap<String, String>) -> Vec<String> {
        let mut out: Vec<String> = self
            .keys
            .iter()
            .filter_map(|k| env.get(k))
            .filter(|v| !v.is_empty())
            .cloned()
            .collect();
        out.extend(self.values.iter().filter(|v| !v.is_empty()).cloned());
        out
    }
}

/// 对 `text` 中出现的所有敏感值替换为 `***`。
///
/// 值短于 3 字符跳过（太常见，如 `\n` 或短 token，易误伤）。
pub fn mask_secrets(text: &str, secrets: &[String]) -> String {
    if secrets.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let mut result = text.to_string();
    for secret in secrets {
        if secret.len() < 3 {
            continue;
        }
        result = result.replace(secret.as_str(), "***");
    }
    result
}

#[cfg(test)]
#[path = "log_masking_tests.rs"]
mod tests;
