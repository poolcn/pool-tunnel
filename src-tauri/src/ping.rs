use std::sync::OnceLock;

use regex::Regex;

/// 对 host 执行 4 次 ping，返回平均 RTT（毫秒）；失败返回 None。
/// Windows 用 `ping -n 4`，macOS/Linux 用 `ping -c 4`，解析输出中的 time/时间 值取平均。
pub fn ping_avg(host: &str) -> Option<u32> {
    if host.is_empty() {
        return None;
    }

    let output = if cfg!(target_os = "windows") {
        std::process::Command::new("ping")
            .args(["-n", "4", host])
            .output()
            .ok()?
    } else {
        std::process::Command::new("ping")
            .args(["-c", "4", host])
            .output()
            .ok()?
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let times = extract_times(&text);
    if times.is_empty() {
        return None;
    }
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    Some(avg.round() as u32)
}

/// 从 ping 输出提取所有 RTT 值（兼容中英文：time= / 时间= / time<）
fn extract_times(text: &str) -> Vec<f64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:time|时间)[=<]\s*([0-9.]+)\s*ms").expect("invalid ping regex")
    });
    let mut v = Vec::new();
    for cap in re.captures_iter(text) {
        if let Ok(x) = cap[1].parse::<f64>() {
            v.push(x);
        }
    }
    v
}
