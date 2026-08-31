use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::config::ConfigService;
use crate::gost::GostManager;
use crate::models::{ServerConfig, TunnelState};

/// 跨 command 共享的应用状态
pub struct AppState {
    pub config: ConfigService,
    pub gost: GostManager,
    pub state: Mutex<TunnelState>,
    pub server: Mutex<Option<ServerConfig>>,
    pub ports: Mutex<Vec<u16>>,
    /// 币种 -> 端口列表（连接时按勾选建立，用于按币种统计矿机）
    pub coin_ports: Mutex<HashMap<String, Vec<u16>>>,
    /// 币种 -> 在线矿机数
    pub coin_miners: Mutex<HashMap<String, u32>>,
    /// 本机默认出口网卡内网 IPv4，仅用于 UI 显示
    pub lan_ip: Mutex<String>,
    /// 公网出口 IPv4，仅用于 online.php 上报
    pub public_ip: Mutex<String>,
    pub miners: Mutex<u32>,
    /// 加密隧道一（server1）延迟 ms
    pub delay1: Mutex<Option<u32>>,
    /// 加密隧道二（server2）延迟 ms
    pub delay2: Mutex<Option<u32>>,
    /// 客户端运行事件日志（内网IP/公网IP/上报状态等），与 gost 日志一并在 UI 显示，上限 100 条
    pub sys_logs: Mutex<Vec<String>>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            config: ConfigService::new(data_dir),
            gost: GostManager::new(),
            state: Mutex::new(TunnelState::Idle),
            server: Mutex::new(None),
            ports: Mutex::new(Vec::new()),
            coin_ports: Mutex::new(HashMap::new()),
            coin_miners: Mutex::new(HashMap::new()),
            lan_ip: Mutex::new(String::new()),
            public_ip: Mutex::new(String::new()),
            miners: Mutex::new(0),
            delay1: Mutex::new(None),
            delay2: Mutex::new(None),
            sys_logs: Mutex::new(Vec::new()),
        }
    }
}
