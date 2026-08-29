use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State};

use crate::models::{CoinGroup, CoinGroupDto, PoolItemDto, TunnelState};
use crate::net;
use crate::reporter::Reporter;
use crate::state::AppState;

fn build_dtos(groups: &[CoinGroup], selected: &[String], ip: &str) -> Vec<CoinGroupDto> {
    let selected_set: HashSet<String> = selected.iter().cloned().collect();
    groups
        .iter()
        .map(|g| CoinGroupDto {
            coin: g.coin.clone(),
            items: g
                .entries
                .iter()
                .map(|e| {
                    let key = e.key();
                    PoolItemDto {
                        key: key.clone(),
                        pool_name: e.pool_name.clone(),
                        port: e.port,
                        endpoint: if ip.is_empty() {
                            "未检测到可用 IPv4".to_string()
                        } else {
                            format!("局域网矿机加密连接端口 {}:{}", ip, e.port)
                        },
                        is_checked: selected_set.contains(&key),
                    }
                })
                .collect(),
        })
        .collect()
}

fn state_str(s: TunnelState) -> &'static str {
    match s {
        TunnelState::Idle => "idle",
        TunnelState::Starting => "starting",
        TunnelState::Running => "running",
        TunnelState::Failed => "failed",
    }
}

/// 从 "域名:端口" 提取域名（用于 ping）
fn ping_host(addr: &str) -> Option<u32> {
    let host = addr.split(':').next().unwrap_or(addr);
    if host.is_empty() {
        return None;
    }
    crate::ping::ping_avg(host)
}

/// 从缓存加载初始列表 + 勾选（启动时前端调用一次）
#[tauri::command]
pub fn get_initial_state(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let mut server = Default::default();
    let groups = if let Some(cache) = state.config.load_cache() {
        let (g, s, _) = state.config.parse(&cache);
        server = s;
        g
    } else {
        Vec::new()
    };
    let selected = state.config.load_selected();
    *state.server.lock().unwrap() = Some(server.clone());
    let ip = state.ip.lock().unwrap().clone();
    let running = *state.state.lock().unwrap() == TunnelState::Running;
    Ok(serde_json::json!({
        "groups": build_dtos(&groups, &selected, &ip),
        "server_complete": server.is_complete(),
        "selected": selected,
        "running": running,
    }))
}

/// 拉取最新列表（10s 超时/重试2次），保存缓存并返回最新数据
#[tauri::command]
pub async fn refresh_config(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let content = state.config.fetch().await?;
    let (groups, server, errors) = state.config.parse(&content);
    if groups.is_empty() {
        return Err("矿池列表为空或格式异常，请联系管理员".to_string());
    }
    state.config.save_cache(&content);
    *state.server.lock().unwrap() = Some(server.clone());
    let selected = state.config.load_selected();
    let ip = state.ip.lock().unwrap().clone();
    Ok(serde_json::json!({
        "groups": build_dtos(&groups, &selected, &ip),
        "server": server,
        "errors": errors,
    }))
}

/// 开始加密连接：先强制刷新最新服务器配置，成功才启动 GOST
#[tauri::command]
pub async fn start_tunnel(
    app: AppHandle,
    state: State<'_, AppState>,
    selected: Vec<String>,
) -> Result<String, String> {
    {
        let cur = *state.state.lock().unwrap();
        if cur == TunnelState::Running || cur == TunnelState::Starting {
            return Err("连接已存在".to_string());
        }
    }
    if selected.is_empty() {
        return Err("未选择任何矿池".to_string());
    }

    *state.state.lock().unwrap() = TunnelState::Starting;

    // 连接前强制拉取最新配置（server1/server2/gostserver）
    let content = match state.config.fetch().await {
        Ok(c) => c,
        Err(e) => {
            *state.state.lock().unwrap() = TunnelState::Failed;
            return Err(e);
        }
    };
    let (groups, server, _errors) = state.config.parse(&content);
    if groups.is_empty() {
        *state.state.lock().unwrap() = TunnelState::Failed;
        return Err("矿池列表为空或格式异常，请联系管理员".to_string());
    }
    state.config.save_cache(&content);
    *state.server.lock().unwrap() = Some(server.clone());

    // 拉取期间被用户停止则中止
    if *state.state.lock().unwrap() != TunnelState::Starting {
        return Err("已取消".to_string());
    }

    if !server.is_complete() {
        *state.state.lock().unwrap() = TunnelState::Failed;
        return Err("服务器配置缺失（server1/server2/gostserver），请更新矿池列表后重试".to_string());
    }

    // 按勾选 key 提取去重端口 + 建立 币种->端口 映射
    let selected_set: HashSet<String> = selected.iter().cloned().collect();
    let mut ports: Vec<u16> = Vec::new();
    let mut coin_ports: HashMap<String, Vec<u16>> = HashMap::new();
    for g in &groups {
        for e in &g.entries {
            if selected_set.contains(&e.key()) {
                if !ports.contains(&e.port) {
                    ports.push(e.port);
                }
                coin_ports.entry(e.coin.clone()).or_default().push(e.port);
            }
        }
    }
    ports.sort_unstable();
    if ports.is_empty() {
        *state.state.lock().unwrap() = TunnelState::Failed;
        return Err("未选择任何矿池".to_string());
    }

    *state.ports.lock().unwrap() = ports.clone();
    *state.coin_ports.lock().unwrap() = coin_ports;
    state.config.save_selected(&selected);
    state.gost.clear_logs();

    state
        .gost
        .start(&app, &ports, &server)
        .await
        .map_err(|e| {
            *state.state.lock().unwrap() = TunnelState::Failed;
            e
        })?;

    *state.state.lock().unwrap() = TunnelState::Running;
    Ok(format!("已启动 {} 个监听端口", ports.len()))
}

/// 停止连接：强杀 GOST 进程并清理残留
#[tauri::command]
pub fn stop_tunnel(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.gost.stop(&app);
    state.ports.lock().unwrap().clear();
    state.coin_ports.lock().unwrap().clear();
    state.coin_miners.lock().unwrap().clear();
    *state.state.lock().unwrap() = TunnelState::Idle;
    Ok(())
}

#[tauri::command]
pub fn get_logs(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.gost.get_logs())
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let cur = *state.state.lock().unwrap();
    let miners = *state.miners.lock().unwrap();
    let ports = state.ports.lock().unwrap().clone();
    let has_logs = state.gost.has_logs();
    let delay1 = *state.delay1.lock().unwrap();
    let delay2 = *state.delay2.lock().unwrap();
    let coins = state.coin_miners.lock().unwrap().clone();
    Ok(serde_json::json!({
        "state": state_str(cur),
        "miners": miners,
        "ports": ports,
        "has_logs": has_logs,
        "delay1": delay1,
        "delay2": delay2,
        "coins": coins,
    }))
}

#[tauri::command]
pub fn set_selected(state: State<'_, AppState>, selected: Vec<String>) -> Result<(), String> {
    state.config.save_selected(&selected);
    Ok(())
}

/// 手动重测延迟：对 server1/server2 各 ping 4 次取平均
#[tauri::command]
pub async fn ping_delay(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let server = state.server.lock().unwrap().clone();
    let (d1, d2) = match server {
        Some(s) => (ping_host(&s.server1), ping_host(&s.server2)),
        None => (None, None),
    };
    if let Some(v) = d1 {
        *state.delay1.lock().unwrap() = Some(v);
    }
    if let Some(v) = d2 {
        *state.delay2.lock().unwrap() = Some(v);
    }
    Ok(serde_json::json!({ "delay1": d1, "delay2": d2 }))
}

#[tauri::command]
pub fn get_app_version() -> String {
    format!("V{}", env!("CARGO_PKG_VERSION"))
}

/// 后台任务：5s 按币种统计矿机 + 10s 上报 + 30s 定时测延迟（启动即持续上报）
pub fn start_background_tasks(app: AppHandle) {
    // 启动即上报：先取一次公网 IP
    let app_ip = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(ip) = Reporter.get_public_ip().await {
            *app_ip.state::<AppState>().ip.lock().unwrap() = ip;
        }
    });

    // 5s 矿机统计（按币种维度）
    let app_miners = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let st = app_miners.state::<AppState>();
            let ports = st.ports.lock().unwrap().clone();
            let by_port = net::count_by_port(&ports);
            let coin_ports = st.coin_ports.lock().unwrap().clone();
            let mut coin_miners: HashMap<String, u32> = HashMap::new();
            let mut total = 0u32;
            for (coin, plist) in &coin_ports {
                let mut c = 0u32;
                for p in plist {
                    if let Some(n) = by_port.get(p) {
                        c += *n;
                    }
                }
                coin_miners.insert(coin.clone(), c);
                total += c;
            }
            *st.coin_miners.lock().unwrap() = coin_miners;
            *st.miners.lock().unwrap() = total;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    // 10s 上报（公网 IP 每 10 分钟刷新一次；含按币种明细）
    let app_report = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_ip_refresh = Instant::now() - Duration::from_secs(601);
        loop {
            let st = app_report.state::<AppState>();
            if last_ip_refresh.elapsed() >= Duration::from_secs(600) {
                if let Some(ip) = Reporter.get_public_ip().await {
                    *st.ip.lock().unwrap() = ip.clone();
                    last_ip_refresh = Instant::now();
                }
            }
            let ip = st.ip.lock().unwrap().clone();
            let miners = *st.miners.lock().unwrap();
            let coin_miners = st.coin_miners.lock().unwrap().clone();
            if !ip.is_empty() {
                let _ = Reporter.report(&ip, miners, &coin_miners).await;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });

    // 30s 定时测延迟（对 server1/server2 各 ping 4 次）
    let app_ping = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let st = app_ping.state::<AppState>();
            let server = st.server.lock().unwrap().clone();
            if let Some(s) = server {
                let d1 = ping_host(&s.server1);
                let d2 = ping_host(&s.server2);
                if let Some(v) = d1 {
                    *st.delay1.lock().unwrap() = Some(v);
                }
                if let Some(v) = d2 {
                    *st.delay2.lock().unwrap() = Some(v);
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}
