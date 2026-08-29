use std::collections::HashMap;
use std::time::Duration;

/// 公网 IP 获取（IPv4 优先）与 online.php 上报
pub struct Reporter;

impl Reporter {
    /// 获取公网 IPv4：先 api-ipv4.ip.sb，失败回退 api.ip.sb
    pub async fn get_public_ip(&self) -> Option<String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;

        for url in ["https://api-ipv4.ip.sb/ip", "https://api.ip.sb/ip"] {
            let resp = match client.get(url).send().await {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !resp.status().is_success() {
                continue;
            }
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if !ip.is_empty() {
                    return Some(ip);
                }
            }
        }
        None
    }

    /// 上报 ip + miners(总数) + coins(各币种明细) 到 online.php
    pub async fn report(
        &self,
        ip: &str,
        miners: u32,
        coins: &HashMap<String, u32>,
    ) -> Result<(), String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;

        let coins_json = serde_json::to_string(coins).unwrap_or_else(|_| "{}".to_string());

        let resp = client
            .post("https://pool.cn.com/tunnel/online.php")
            .form(&[
                ("ip", ip),
                ("miners", &miners.to_string()),
                ("coins", &coins_json),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", resp.status()))
        }
    }
}
