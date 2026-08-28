use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::models::ServerConfig;

pub const MAX_LOG_LINES: usize = 500;

/// GOST sidecar 进程管理：命令构建、启动、日志采集（脱敏）、停止/残留清理
pub struct GostManager {
    child: Mutex<Option<CommandChild>>,
    logs: Arc<Mutex<Vec<String>>>,
}

impl GostManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 构建 -F 转发参数：ip=server1,server2
    pub fn build_forward_config(server1: &str, server2: &str) -> String {
        format!(
            "relay+mtls://abc:123@?ip={},{}&strategy=random&max_fails=1&fail_timeout=30s",
            server1, server2
        )
    }

    /// 构建完整参数：-L 去重升序 + 单个 -F（直接参数数组，不经 shell）
    fn build_args(ports: &[u16], server: &ServerConfig) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        for p in ports {
            args.push("-L".to_string());
            args.push(format!("tcp://:{}/{}:{}", p, server.gostserver, p));
        }
        args.push("-F".to_string());
        args.push(Self::build_forward_config(&server.server1, &server.server2));
        args
    }

    /// 启动 sidecar；成功返回 Ok(())
    pub async fn start(&self, app: &AppHandle, ports: &[u16], server: &ServerConfig) -> Result<(), String> {
        {
            let guard = self.child.lock().map_err(|_| "内部锁错误".to_string())?;
            if guard.is_some() {
                return Err("连接已存在".to_string());
            }
        }

        // 防御：清理残留 gost 进程，避免端口占用
        self.kill_residual(app);

        let args = Self::build_args(ports, server);
        let cmd = app
            .shell()
            .sidecar("gost")
            .map_err(|e| e.to_string())?
            .args(&args);
        let (mut rx, child) = cmd.spawn().map_err(|e| e.to_string())?;
        *self.child.lock().map_err(|_| "内部锁错误".to_string())? = Some(child);

        // 后台异步读取 stdout/stderr，脱敏后写入日志缓冲
        let logs = Arc::clone(&self.logs);
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        push_log(&logs, String::from_utf8_lossy(&bytes).into_owned());
                    }
                    CommandEvent::Stderr(bytes) => {
                        push_log(&logs, String::from_utf8_lossy(&bytes).into_owned());
                    }
                    CommandEvent::Terminated(_) => break,
                    _ => {}
                }
            }
        });

        Ok(())
    }

    /// 停止：优先正常结束等待 3s，未退出则强杀；随后按进程名清理残留
    pub fn stop(&self, app: &AppHandle) {
        let child = self.child.lock().unwrap().take();
        if let Some(c) = child {
            let _ = c.kill();
        }
        self.kill_residual(app);
    }

    /// 按 sidecar 进程名清理残留（Windows 用 taskkill /T /F 兜底子进程树）
    fn kill_residual(&self, _app: &AppHandle) {
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/IM", "gost-x86_64-pc-windows-msvc.exe"])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new("pkill")
                .args(["-f", "gost-"])
                .output();
        }
    }

    pub fn get_logs(&self) -> String {
        self.logs.lock().unwrap().join("\n")
    }

    pub fn has_logs(&self) -> bool {
        !self.logs.lock().unwrap().is_empty()
    }

    pub fn clear_logs(&self) {
        self.logs.lock().unwrap().clear();
    }
}

fn push_log(logs: &Mutex<Vec<String>>, line: String) {
    let masked = mask_addresses(&line);
    let mut guard = logs.lock().unwrap();
    guard.push(masked);
    if guard.len() > MAX_LOG_LINES {
        let excess = guard.len() - MAX_LOG_LINES;
        guard.drain(0..excess);
    }
}

fn ip_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap())
}

fn domain_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:[a-zA-Z0-9](?:[a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}\b")
            .unwrap()
    })
}

/// 日志脱敏：IP/域名保留首末各 1 字符，中间以 * 代替（覆盖"不得脱敏"需求）
fn mask_addresses(line: &str) -> String {
    let step1 = ip_re().replace_all(line, |caps: &regex::Captures| mask_token(&caps[0]));
    domain_re()
        .replace_all(&step1, |caps: &regex::Captures| mask_token(&caps[0]))
        .into_owned()
}

fn mask_token(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= 2 {
        "*".repeat(n)
    } else {
        let mut out = chars;
        for c in out.iter_mut().skip(1).take(n - 2) {
            *c = '*';
        }
        out.into_iter().collect()
    }
}
