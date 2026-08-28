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
    pub ip: Mutex<String>,
    pub miners: Mutex<u32>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            config: ConfigService::new(data_dir),
            gost: GostManager::new(),
            state: Mutex::new(TunnelState::Idle),
            server: Mutex::new(None),
            ports: Mutex::new(Vec::new()),
            ip: Mutex::new(String::new()),
            miners: Mutex::new(0),
        }
    }
}
