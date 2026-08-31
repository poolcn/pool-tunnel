use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

use crate::models::{CoinGroup, CoinGroupDto, PoolItemDto, TunnelState};
use crate::net;
use crate::reporter::Reporter;
use crate::state::AppState;

/// 记录客户端运行事件（内网IP/公网IP/上报状态等）到 UI 可见日志，最多保留 100 条。
fn log_event(state: &AppState, msg: impl Into<String>) {
    if let Ok(mut logs) = state.sys_logs.lock() {
        if logs.len() >= 100 {
            logs.remove(0);
        }
        logs.push(msg.into());
    }
}

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

/// 测量当前 server1/server2 延迟并写入共享状态。
/// ping 是阻塞系统调用（双服务器串行约 8-10s），放入 spawn_blocking 避免阻塞 async 运行时。
async fn measure_delays(state: &AppState) -> (Option<u32>, Option<u32>) {
    let server = state.server.lock().unwrap().clone();
    let (d1, d2) = match server {
        Some(s) => {
            let s1 = s.server1.clone();
            let s2 = s.server2.clone();
            tauri::async_runtime::spawn_blocking(move || (ping_host(&s1), ping_host(&s2)))
                .await
                .unwrap_or((None, None))
        }
        None => (None, None),
    };
    if let Some(v) = d1 {
        *state.delay1.lock().unwrap() = Some(v);
    }
    if let Some(v) = d2 {
        *state.delay2.lock().unwrap() = Some(v);
    }
    (d1, d2)
}

/// 从缓存加载初始列表 + 勾选，并在启动时立即测量一次延迟。
#[tauri::command]
pub async fn get_initial_state(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
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
    let running = *state.state.lock().unwrap() == TunnelState::Running;
    let (delay1, delay2) = measure_delays(&state).await;
    // 必须在 measure_delays（ping 阻塞约 8-10s）之后才读取 lan_ip，用最新值构建 DTO。
    // 否则前端 init() 先收到 lan-ip-updated 事件渲染出正确 IP，随后本函数用 ping 前读取的
    // 旧空 IP 覆盖回去，导致 Win10 上状态文本被刷成「未检测到可用 IPv4」。
    let ip = state.lan_ip.lock().unwrap().clone();
    Ok(serde_json::json!({
        "groups": build_dtos(&groups, &selected, &ip),
        "server_complete": server.is_complete(),
        "selected": selected,
        "running": running,
        "delay1": delay1,
        "delay2": delay2,
    }))
}

/// 轻量刷新：用缓存的池列表 + 当前 lan_ip 重新构建 DTO，不重拉 pool.txt、不重测延迟。
/// 用于 lan-ip-updated 事件回调：内网IP异步就绪或变更后，让 UI 立即拿到正确的 endpoint 文本。
#[tauri::command]
pub fn rebuild_groups_with_lan_ip(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let groups = if let Some(cache) = state.config.load_cache() {
        let (g, _, _) = state.config.parse(&cache);
        g
    } else {
        Vec::new()
    };
    let selected = state.config.load_selected();
    let ip = state.lan_ip.lock().unwrap().clone();
    Ok(serde_json::json!({
        "groups": build_dtos(&groups, &selected, &ip),
        "lan_ip": ip,
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
    let ip = state.lan_ip.lock().unwrap().clone();
    let (delay1, delay2) = measure_delays(&state).await;
    Ok(serde_json::json!({
        "groups": build_dtos(&groups, &selected, &ip),
        "server": server,
        "errors": errors,
        "selected": selected,
        "delay1": delay1,
        "delay2": delay2,
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
                // 每币种端口去重，避免同一端口连接数被重复累计
                let plist = coin_ports.entry(e.coin.clone()).or_default();
                if !plist.contains(&e.port) {
                    plist.push(e.port);
                }
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
    // 系统事件日志（内网IP/公网IP/上报状态）与 gost 隧道日志合并展示，系统日志在前
    let sys = state.sys_logs.lock().unwrap().join("\n");
    let gost = state.gost.get_logs();
    let mut parts: Vec<String> = Vec::new();
    if !sys.is_empty() {
        parts.push("[系统事件]".to_string());
        parts.push(sys);
    }
    if !gost.is_empty() {
        if !parts.is_empty() {
            parts.push(String::new());
        }
        parts.push(gost);
    }
    Ok(parts.join("\n"))
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let cur = *state.state.lock().unwrap();
    let miners = *state.miners.lock().unwrap();
    let ports = state.ports.lock().unwrap().clone();
    let has_logs = state.gost.has_logs() || !state.sys_logs.lock().unwrap().is_empty();
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

/// 手动重测延迟：对 server1/server2 各 ping 4 次取平均。
#[tauri::command]
pub async fn ping_delay(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let (d1, d2) = measure_delays(&state).await;
    Ok(serde_json::json!({ "delay1": d1, "delay2": d2 }))
}

#[tauri::command]
pub fn get_app_version() -> String {
    format!("V{}", env!("CARGO_PKG_VERSION"))
}

/// 后台任务：5s 按币种统计矿机 + 10s 上报 + 30s 定时测延迟（启动即持续上报）
pub fn start_background_tasks(app: AppHandle) {
    // 启动时读取本机默认出口网卡内网 IP，仅用于 UI 显示；结果写入系统事件日志便于排障。
    let app_lan_ip = app.clone();
    tauri::async_runtime::spawn(async move {
        let st = app_lan_ip.state::<AppState>();
        match Reporter.get_local_ip() {
            Some(ip) => {
                *st.lan_ip.lock().unwrap() = ip.clone();
                log_event(&*st, format!("内网IP检测成功: {}", ip));
                // 通知前端刷新 endpoint，修复 init() 调用先于 lan_ip 就绪的竞态
                let _ = app_lan_ip.emit("lan-ip-updated", &ip);
            }
            None => log_event(&*st, "内网IP检测失败（route/PowerShell/ipconfig 均未命中）".to_string()),
        }
    });

    // 启动即上报：单独获取公网出口 IP，仅用于 online.php 上报；结果写入日志。
    let app_public_ip = app.clone();
    tauri::async_runtime::spawn(async move {
        let st = app_public_ip.state::<AppState>();
        match Reporter.get_public_ip().await {
            Some(ip) => {
                *st.public_ip.lock().unwrap() = ip.clone();
                log_event(&*st, format!("公网IP获取成功: {}", ip));
            }
            None => log_event(&*st, "公网IP获取失败（4 个端点均不可达，将不上报）".to_string()),
        }
    });

    // 5s 矿机统计（按币种维度）+ gost 存活检测
    let app_miners = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let st = app_miners.state::<AppState>();
            // 存活检测：gost 崩溃后状态由 Running 置为 Failed，避免 UI 假运行
            if *st.state.lock().unwrap() == TunnelState::Running && st.gost.has_exited() {
                st.ports.lock().unwrap().clear();
                st.coin_ports.lock().unwrap().clear();
                st.coin_miners.lock().unwrap().clear();
                *st.state.lock().unwrap() = TunnelState::Failed;
            }
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

    // 10s 上报（公网出口 IP 每 10 分钟刷新一次；含按币种明细）
    let app_report = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_public_ip_refresh = Instant::now() - Duration::from_secs(601);
        let mut last_report_err: Option<String> = None; // 上报失败去重，避免每 10s 刷屏
        loop {
            let st = app_report.state::<AppState>();
            if last_public_ip_refresh.elapsed() >= Duration::from_secs(600) {
                match Reporter.get_public_ip().await {
                    Some(ip) => {
                        *st.public_ip.lock().unwrap() = ip;
                        last_public_ip_refresh = Instant::now();
                    }
                    None => {
                        // 失败也推进计时，避免每 10s 都重试 4 个端点拖慢循环
                        last_public_ip_refresh = Instant::now();
                    }
                }
            }
            let ip = st.public_ip.lock().unwrap().clone();
            let miners = *st.miners.lock().unwrap();
            let coin_miners = st.coin_miners.lock().unwrap().clone();
            if !ip.is_empty() {
                match Reporter.report(&ip, miners, &coin_miners).await {
                    Ok(()) => {
                        if last_report_err.is_some() {
                            log_event(&*st, "上报恢复".to_string());
                            last_report_err = None;
                        }
                    }
                    Err(e) => {
                        if last_report_err.as_deref() != Some(e.as_str()) {
                            log_event(&*st, format!("上报失败: {}", e));
                            last_report_err = Some(e);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });

    // 30s 定时测延迟 + 刷新内网 IP（网络切换/DHCP 变化后保持最新）；启动首次测量由 get_initial_state 完成。
    let app_ping = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            let st = app_ping.state::<AppState>();
            let _ = measure_delays(&st).await;
            // 内网 IP 获取为阻塞系统命令，放入 spawn_blocking
            let new_ip = tauri::async_runtime::spawn_blocking(|| Reporter.get_local_ip()).await;
            if let Ok(Some(ip)) = new_ip {
                let changed = {
                    let mut guard = st.lan_ip.lock().unwrap();
                    if *guard != ip {
                        *guard = ip.clone();
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    log_event(&*st, format!("内网IP已更新: {}", ip));
                }
                // 每 30s 无条件通知前端刷新 endpoint（IP 变更时或作兜底恢复），
                // 确保 UI 因竞态被覆盖成「未检测到可用 IPv4」后能自动恢复正常。
                let _ = app_ping.emit("lan-ip-updated", &ip);
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}
