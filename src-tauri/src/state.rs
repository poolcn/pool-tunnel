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
    pub ip: Mutex<String>,
    pub miners: Mutex<u32>,
    /// 加密隧道一（server1）延迟 ms
    pub delay1: Mutex<Option<u32>>,
    /// 加密隧道二（server2）延迟 ms
    pub delay2: Mutex<Option<u32>>,
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
            ip: Mutex::new(String::new()),
            miners: Mutex::new(0),
            delay1: Mutex::new(None),
            delay2: Mutex::new(None),
        }
    }
}
