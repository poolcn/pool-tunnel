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

/// 分离获取本机内网 IPv4 与公网出口 IPv4，并向 online.php 上报
pub struct Reporter;

fn is_private_ipv4(value: &str) -> bool {
    let octets: Vec<u8> = value.split('.').filter_map(|v| v.parse().ok()).collect();
    if octets.len() != 4 || octets[0] == 127 || octets[0] == 169 && octets[1] == 254 {
        return false;
    }
    matches!(octets[0], 10 | 192) || (octets[0] == 172 && (16..=31).contains(&octets[1]))
}

fn extract_ipv4(text: &str) -> Option<String> {
    text.split_whitespace()
        .filter_map(|token| token.split('/').next())
        .map(|token| token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.'))
        .find(|token| is_private_ipv4(token))
        .map(ToString::to_string)
}

/// 依据系统路由表找到第一条默认 IPv4 出口，再读取该接口的第一个内网 IPv4。
/// 不访问公网 IP 查询服务；失败时返回 None，由调用方保留原有空地址兜底。
fn get_default_lan_ip() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "(Get-NetIPConfiguration | Where-Object {$_.IPv4DefaultGateway -ne $null} | Sort-Object InterfaceIndex | Select-Object -First 1).IPv4Address.IPAddress",
            ])
            .creation_flags(0x08000000)
            .output()
            .ok()?;
        return extract_ipv4(&String::from_utf8_lossy(&output.stdout));
    }

    #[cfg(target_os = "macos")]
    {
        let route = Command::new("route").args(["-n", "get", "default"]).output().ok()?;
        let route_text = String::from_utf8_lossy(&route.stdout);
        let interface = route_text
            .lines()
            .find_map(|line| line.trim().strip_prefix("interface: "))?
            .trim();
        let output = Command::new("ipconfig").args(["getifaddr", interface]).output().ok()?;
        return extract_ipv4(&String::from_utf8_lossy(&output.stdout));
    }

    #[cfg(target_os = "linux")]
    {
        let route = Command::new("ip").args(["route", "show", "default"]).output().ok()?;
        let route_text = String::from_utf8_lossy(&route.stdout);
        let interface = route_text
            .lines()
            .find_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                parts.windows(2).find(|pair| pair[0] == "dev").map(|pair| pair[1])
            })?;
        let output = Command::new("ip")
            .args(["-4", "-o", "addr", "show", "dev", interface, "scope", "global"])
            .output()
            .ok()?;
        return extract_ipv4(&String::from_utf8_lossy(&output.stdout));
    }

    #[allow(unreachable_code)]
    None
}

impl Reporter {
    /// 获取默认网络出口网卡的第一个内网 IPv4，仅用于矿机连接地址显示。
    pub fn get_local_ip(&self) -> Option<String> {
        get_default_lan_ip()
    }

    /// 获取公网出口 IPv4，仅用于向 online.php 上报客户端公网来源地址。
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
