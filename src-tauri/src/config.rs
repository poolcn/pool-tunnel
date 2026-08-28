use std::collections::HashMap;
use std::path::PathBuf;

use crate::models::{CoinGroup, PoolEntry, ServerConfig};

/// 矿池列表拉取、解析与本地缓存/勾选持久化
pub struct ConfigService {
    pool_url: String,
    data_dir: PathBuf,
}

impl ConfigService {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            pool_url: "https://pool.cn.com/tunnel/pool.txt".to_string(),
            data_dir,
        }
    }

    fn cache_path(&self) -> PathBuf {
        self.data_dir.join("pool_cache.txt")
    }

    fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.json")
    }

    /// 拉取最新列表：10s 超时，失败重试 2 次，间隔 2s
    pub async fn fetch(&self) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

        let mut last_err = String::from("未知错误");
        for attempt in 0..=2 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            match client.get(&self.pool_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    return resp.text().await.map_err(|e| e.to_string());
                }
                Ok(resp) => last_err = format!("HTTP {}", resp.status()),
                Err(e) => last_err = e.to_string(),
            }
        }
        Err(format!("网络请求失败，请检查网络后重试（{}）", last_err))
    }

    /// 解析 pool.txt 原文；返回 (分组列表, 服务器配置, 非法行提示)
    pub fn parse(&self, content: &str) -> (Vec<CoinGroup>, ServerConfig, Vec<String>) {
        let mut groups: Vec<CoinGroup> = Vec::new();
        let mut group_map: HashMap<String, usize> = HashMap::new();
        let mut server = ServerConfig::default();
        let mut errors: Vec<String> = Vec::new();

        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // 服务器转发配置字段（前缀匹配，值可含冒号）
            if let Some(v) = trimmed.strip_prefix("server1:") {
                server.server1 = v.trim().to_string();
                continue;
            }
            if let Some(v) = trimmed.strip_prefix("server2:") {
                server.server2 = v.trim().to_string();
                continue;
            }
            if let Some(v) = trimmed.strip_prefix("gostserver:") {
                server.gostserver = v.trim().to_string();
                continue;
            }

            let parts: Vec<&str> = trimmed.split(':').collect();
            if parts.len() != 3 {
                errors.push(format!("第 {} 行格式非法（需为 币种:矿池名称:端口）", i + 1));
                continue;
            }
            let coin = parts[0].trim();
            let pool = parts[1].trim();
            let port: u16 = match parts[2].trim().parse::<u16>() {
                Ok(p) if p >= 1 => p,
                _ => {
                    errors.push(format!("第 {} 行端口非法（需为 1-65535 的整数）", i + 1));
                    continue;
                }
            };
            if coin.is_empty() || pool.is_empty() {
                errors.push(format!("第 {} 行币种或矿池名称为空", i + 1));
                continue;
            }

            let idx = match group_map.get(coin) {
                Some(&x) => x,
                None => {
                    groups.push(CoinGroup {
                        coin: coin.to_string(),
                        entries: Vec::new(),
                    });
                    let x = groups.len() - 1;
                    group_map.insert(coin.to_string(), x);
                    x
                }
            };
            groups[idx].entries.push(PoolEntry {
                coin: coin.to_string(),
                pool_name: pool.to_string(),
                port,
            });
        }

        (groups, server, errors)
    }

    pub fn load_cache(&self) -> Option<String> {
        std::fs::read_to_string(self.cache_path()).ok()
    }

    pub fn save_cache(&self, content: &str) {
        if let Some(dir) = self.data_dir.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(self.cache_path(), content);
    }

    pub fn load_selected(&self) -> Vec<String> {
        let raw = std::fs::read_to_string(self.config_path()).unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn save_selected(&self, keys: &[String]) {
        if let Some(dir) = self.data_dir.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(self.config_path(), serde_json::to_string(keys).unwrap_or_default());
    }
}
