use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// 客户端版本号（编译期取自 Cargo.toml 的 version，单一来源）
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 获取操作系统版本；命令不可用或读取失败时返回稳定的系统名称兜底值。
fn detect_os_version() -> String {
    static OS_VERSION: OnceLock<String> = OnceLock::new();
    OS_VERSION
        .get_or_init(|| {
            #[cfg(target_os = "windows")]
            {
                let output = Command::new("cmd")
                    .args(["/C", "ver"])
                    .creation_flags(0x08000000)
                    .output();
                if let Ok(output) = output {
                    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !text.is_empty() {
                        return text;
                    }
                }
                return "Windows".to_string();
            }

            #[cfg(target_os = "macos")]
            {
                if let Ok(output) = Command::new("sw_vers").arg("-productVersion").output() {
                    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !text.is_empty() {
                        return format!("macOS {}", text);
                    }
                }
                return "macOS".to_string();
            }

            #[cfg(target_os = "linux")]
            {
                if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
                    for line in content.lines() {
                        if let Some(value) = line.strip_prefix("VERSION_ID=") {
                            let version = value.trim_matches('"').trim_matches('\'');
                            if !version.is_empty() {
                                return format!("Linux {}", version);
                            }
                        }
                    }
                }
                return "Linux".to_string();
            }

            #[allow(unreachable_code)]
            std::env::consts::OS.to_string()
        })
        .clone()
}

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

    /// 上报 ip + miners(总数) + coins(各币种明细) + version(软件版本) + os_version(操作系统版本) 到 online.php
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
        let os_version = detect_os_version();

        let resp = client
            .post("https://pool.cn.com/tunnel/online.php")
            .form(&[
                ("ip", ip),
                ("miners", &miners.to_string()),
                ("coins", &coins_json),
                ("version", APP_VERSION),
                ("os_version", &os_version),
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
