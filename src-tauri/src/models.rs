use serde::{Deserialize, Serialize};

/// pool.txt 末尾的服务器转发配置（server1/server2/gostserver）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    pub server1: String,
    pub server2: String,
    pub gostserver: String,
}

impl ServerConfig {
    pub fn is_complete(&self) -> bool {
        !self.server1.is_empty() && !self.server2.is_empty() && !self.gostserver.is_empty()
    }
}

/// 单个矿池条目
#[derive(Debug, Clone)]
pub struct PoolEntry {
    pub coin: String,
    pub pool_name: String,
    pub port: u16,
}

impl PoolEntry {
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.coin, self.pool_name, self.port)
    }
}

/// 按币种分组
#[derive(Debug, Clone)]
pub struct CoinGroup {
    pub coin: String,
    pub entries: Vec<PoolEntry>,
}

/// 前端展示用的矿池条目 DTO
#[derive(Debug, Clone, Serialize)]
pub struct PoolItemDto {
    pub key: String,
    pub pool_name: String,
    pub port: u16,
    pub endpoint: String,
    pub is_checked: bool,
}

/// 前端展示用的币种分组 DTO
#[derive(Debug, Clone, Serialize)]
pub struct CoinGroupDto {
    pub coin: String,
    pub items: Vec<PoolItemDto>,
}

/// 连接状态
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelState {
    Idle,
    Starting,
    Running,
    Failed,
}
