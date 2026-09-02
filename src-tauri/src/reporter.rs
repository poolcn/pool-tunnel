use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// 客户端版本号（编译期取自 Cargo.toml 的 version，单一来源）
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 获取操作系统版本；命令不可用或读取失败时返回稳定的系统名称兜底值。
/// Windows 上使用 systeminfo 的 "OS 名称" 行（如「Microsoft Windows 11 专业版」），并用 decode_console
/// 解码中文控制台的 GBK 输出，避免 from_utf8_lossy 把"[版]"或"专业版"打成 U+FFFD。
/// 注意：首次调用会同步执行 systeminfo（部分机器 10-60s），禁止在 async 上下文直接调用，
/// 必须经 spawn_blocking（应用启动时已预热一次，之后命中 OnceLock 缓存）。
pub(crate) fn detect_os_version() -> String {
    static OS_VERSION: OnceLock<String> = OnceLock::new();
    OS_VERSION
        .get_or_init(|| {
            #[cfg(target_os = "windows")]
            {
                // 优先 systeminfo 的 "OS 名称:" 行（含「专业版」「家庭版」等中文 SKU，需 GBK 解码）
                // creation_flags(0x08000000) = CREATE_NO_WINDOW，避免弹出 CMD 黑窗
                if let Ok(output) = Command::new("systeminfo")
                    .creation_flags(0x08000000)
                    .output()
                {
                    let text = decode_console(&output.stdout);
                    for line in text.lines() {
                        let line = line.trim();
                        // 注意是中文冒号「：」——systeminfo 中文 Windows 输出是 GBK
                        if line.starts_with("OS 名称")
                            || line.starts_with("OS Name")
                            || line.starts_with("OS名称")
                        {
                            if let Some((_, value)) = line.split_once(':').or_else(|| line.split_once('：')) {
                                let v = value.trim();
                                if !v.is_empty() {
                                    return v.to_string();
                                }
                            }
                        }
                    }
                }
                // 回退 ver（GBK 解码，避开 from_utf8_lossy 的 U+FFFD）
                if let Ok(output) = Command::new("cmd")
                    .args(["/C", "ver"])
                    .creation_flags(0x08000000)
                    .output()
                {
                    let text = decode_console(&output.stdout).trim().to_string();
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

/// 严格解析 "a.b.c.d" 为 4 个 0-255 的八位组；畸形输入（段数不等于 4、非数字、超 255）一律拒绝。
fn parse_ipv4_octets(value: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        octets[i] = p.parse::<u8>().ok()?;
    }
    Some(octets)
}

/// RFC1918 三段私网地址：10/8、172.16/12、192.168/16。
fn is_rfc1918_ipv4(value: &str) -> bool {
    match parse_ipv4_octets(value) {
        Some([10, _, _, _]) => true,
        Some([172, b, _, _]) => (16..=31).contains(&b),
        Some([192, 168, _, _]) => true,
        _ => false,
    }
}

/// 宽松局域网判定：RFC1918 之外额外接受运营商级 NAT 段 100.64.0.0/10
/// （校园网/酒店/部分运营商家宽下网卡地址即此段，矿机同段内仍可互通）。
fn is_lan_ipv4(value: &str) -> bool {
    if is_rfc1918_ipv4(value) {
        return true;
    }
    matches!(parse_ipv4_octets(value), Some([100, b, _, _]) if (64..=127).contains(&b))
}

/// 可作为"存在默认网关"证据的 IPv4：排除回环、APIPA（169.254，DHCP 失败自动地址）、
/// 未指定地址与组播/保留段。网关本身允许是 CGNAT 或公网地址。
fn is_usable_ipv4(value: &str) -> bool {
    match parse_ipv4_octets(value) {
        Some([0, _, _, _]) | Some([127, _, _, _]) => false,
        Some([169, 254, _, _]) => false,
        Some([a, _, _, _]) if a >= 224 => false,
        other => other.is_some(),
    }
}

/// 控制台命令输出解码：优先按 UTF-8（纯 ASCII 或系统开了 UTF-8 代码页时直接正确），
/// 失败时按 GBK 解码（中文 Windows 的 OEM 936 控制台输出）。
/// 旧实现 from_utf8_lossy 会把"默认网关"等中文标签打成乱码，导致 ipconfig 阶段在
/// 中文系统上永远匹配不到网关行——本次修复的关键点之一。
pub(crate) fn decode_console(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::GBK.decode(bytes).0.to_string(),
    }
}

/// 共享 reqwest Client：复用连接池/TLS 会话，避免每 10s 上报都重建。
/// 超时统一取各调用点最大值 10s（原 report 5s / get_public_ip 6s / fetch 10s）。
pub(crate) fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 从文本 token 中提取第一个满足谓词的 IPv4（自动剥离括号后缀如 "(首选)"）。
fn extract_ipv4_where<F: Fn(&str) -> bool>(text: &str, pred: F) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.split('/').next().unwrap_or(token))
        .map(|token| token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.'))
        .find(|token| pred(token))
        .map(ToString::to_string)
}

fn extract_lan_ipv4(text: &str) -> Option<String> {
    extract_ipv4_where(text, is_lan_ipv4)
}

/// 从 Windows `route print -4` 的活动路由中选默认路由的接口 IPv4。
/// 多条默认路由（物理网卡 + VPN/虚拟网卡）时：优先 RFC1918 地址的行，再按跃点数（实际出口优先级）取最小。
/// 典型行：`0.0.0.0  0.0.0.0  192.168.168.1  192.168.168.155  35`。
fn extract_windows_default_route_ip(text: &str) -> Option<String> {
    let mut best: Option<(bool, u32, String)> = None; // (是否RFC1918, 跃点数, 接口IP)
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 && parts[0] == "0.0.0.0" && parts[1] == "0.0.0.0" {
            let interface_ip = parts[3].trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
            // PPP 拨号默认路由的接口地址是本端 PPP 地址（公网/CGNAT），is_lan_ipv4 拒绝后交给后续阶段
            if !is_lan_ipv4(interface_ip) {
                continue;
            }
            let strict = is_rfc1918_ipv4(interface_ip);
            let metric: u32 = parts.get(4).and_then(|m| m.parse().ok()).unwrap_or(u32::MAX);
            let better = match &best {
                None => true,
                Some((b_strict, b_metric, _)) => {
                    (strict && !*b_strict) || (strict == *b_strict && metric < *b_metric)
                }
            };
            if better {
                best = Some((strict, metric, interface_ip.to_string()));
            }
        }
    }
    best.map(|(_, _, ip)| ip)
}

/// 解析 `ipconfig` 输出。两轮择优（严格/宽松），兼容多类网络环境：
/// 1) 有默认网关的适配器 + 严格 RFC1918（标准家庭/办公网络，正常路径）
/// 2) 同上但宽松接受 CGNAT（100.64.x 网络：网关与本机地址均非 RFC1918）
/// 3) 任意适配器 + RFC1918（PPPoE 直拨 + 无网关局域网卡的多网卡矿机：矿机应连的卡没有网关）
/// 4) 任意适配器 + 宽松局域网（最后兜底）
fn extract_ipconfig_ip(text: &str, relaxed: bool) -> Option<String> {
    // 轮 1/2：带网关的适配器。IPv4 行先记候选，同适配器块内出现网关行即命中。
    let mut candidate: Option<String> = None;
    let mut has_gateway = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            has_gateway = false;
            candidate = None;
            continue;
        }
        if trimmed.contains("IPv4") && trimmed.contains(':') {
            if relaxed {
                candidate = extract_lan_ipv4(trimmed);
            } else {
                candidate = extract_ipv4_where(trimmed, is_rfc1918_ipv4);
            }
        } else if trimmed.contains("Default Gateway") || trimmed.contains("网关") {
            // 网关地址本身可以是 CGNAT/公网，只要是可用 IPv4 即证明该适配器可对外
            has_gateway = extract_ipv4_where(trimmed, is_usable_ipv4).is_some();
        }
        if has_gateway {
            if let Some(ip) = candidate.take() {
                return Some(ip);
            }
        }
    }

    // 轮 3/4：不要求网关，全适配器扫描（APIPA/回环由地址段判定排除）
    if relaxed {
        extract_lan_ipv4(text)
    } else {
        extract_ipv4_where(text, is_rfc1918_ipv4)
    }
}

/// 依据系统路由表找到第一条默认 IPv4 出口，再读取该接口的第一个内网 IPv4。
/// 不访问公网 IP 查询服务；失败时返回 None，由调用方保留原有空地址兜底。
fn get_default_lan_ip() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        // 第一优先级：读取 Windows 活动 IPv4 路由表，按跃点数选默认路由的接口地址。
        if let Ok(output) = Command::new("route")
            .args(["print", "-4"])
            .creation_flags(0x08000000)
            .output()
        {
            if output.status.success() {
                if let Some(ip) = extract_windows_default_route_ip(&decode_console(&output.stdout)) {
                    return Some(ip);
                }
            }
        }

        // 第二优先级：PowerShell 按路由度量取真实 IPv4 出口网卡地址（与系统选路一致）。
        if let Ok(output) = Command::new("powershell")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "$r=Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Sort-Object RouteMetric,InterfaceMetric | Select-Object -First 1; if($r){(Get-NetIPAddress -InterfaceIndex $r.InterfaceIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object {$_.IPAddress -notlike '169.254*' -and $_.IPAddress -notlike '127*'} | Select-Object -First 1).IPAddress}",
            ])
            .creation_flags(0x08000000)
            .output()
        {
            if output.status.success() {
                if let Some(ip) = extract_lan_ipv4(&decode_console(&output.stdout)) {
                    return Some(ip);
                }
            }
        }

        // 第三优先级：解析 ipconfig，严格 RFC1918 优先、宽松（CGNAT/无网关局域网卡）兜底。
        if let Ok(output) = Command::new("ipconfig")
            .creation_flags(0x08000000)
            .output()
        {
            if output.status.success() {
                let text = decode_console(&output.stdout);
                if let Some(ip) = extract_ipconfig_ip(&text, false) {
                    return Some(ip);
                }
                if let Some(ip) = extract_ipconfig_ip(&text, true) {
                    return Some(ip);
                }
            }
        }

        return None;
    }

    #[cfg(target_os = "macos")]
    {
        let route = Command::new("route").args(["-n", "get", "default"]).output().ok()?;
        let route_text = decode_console(&route.stdout);
        let interface = route_text
            .lines()
            .find_map(|line| line.trim().strip_prefix("interface: "))?
            .trim();
        let output = Command::new("ipconfig").args(["getifaddr", interface]).output().ok()?;
        return extract_lan_ipv4(&decode_console(&output.stdout));
    }

    #[cfg(target_os = "linux")]
    {
        let route = Command::new("ip").args(["route", "show", "default"]).output().ok()?;
        let route_text = decode_console(&route.stdout);
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
        return extract_lan_ipv4(&decode_console(&output.stdout));
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
    /// 多端点冗余：ip.sb 在部分大陆网络不可达（DNS 污染/连接超时），此前只依赖它会
    /// 导致 public_ip 一直为空、上报循环整体不执行——线上"看不到任何上报记录"的主因。
    pub async fn get_public_ip(&self) -> Option<String> {
        let client = http_client();

        const ENDPOINTS: [&str; 4] = [
            "https://api-ipv4.ip.sb/ip",   // 原端点，海外
            "https://4.ipw.cn",            // 国内直连，返回纯 IPv4
            "https://ipv4.icanhazip.com",  // Cloudflare 支持
            "http://ip.3322.net",          // 纯 HTTP 最后兜底（仅回显 IP，无敏感数据）
        ];
        for url in ENDPOINTS {
            let resp = match client.get(url).send().await {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !resp.status().is_success() {
                continue;
            }
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                // 校验返回确实是合法 IPv4，避免异常响应体（如错误页）被当作公网 IP 缓存并持续上报
                if ip.parse::<std::net::Ipv4Addr>().is_ok() {
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
        let client = http_client();

        let coins_json = serde_json::to_string(coins).unwrap_or_else(|_| "{}".to_string());
        // systeminfo 为阻塞系统命令，必须经 spawn_blocking；正常已预热命中缓存，此调用近乎零开销
        let os_version = tauri::async_runtime::spawn_blocking(detect_os_version)
            .await
            .unwrap_or_else(|_| "未知".to_string());

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
