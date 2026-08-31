use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use tauri::{AppHandle, Manager};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::models::ServerConfig;

pub const MAX_LOG_LINES: usize = 500;

// ---- 编译期内嵌对应平台的 gost 二进制（单 EXE 自包含） ----
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const GOST_BIN: &[u8] = include_bytes!("../binaries/gost-x86_64-pc-windows-msvc.exe");
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const GOST_BIN: &[u8] = include_bytes!("../binaries/gost-x86_64-apple-darwin");
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GOST_BIN: &[u8] = include_bytes!("../binaries/gost-aarch64-apple-darwin");
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GOST_BIN: &[u8] = include_bytes!("../binaries/gost-x86_64-unknown-linux-gnu");

/// gost 可执行文件名（Windows 带 .exe）
#[cfg(target_os = "windows")]
fn gost_file_name() -> &'static str {
    "gost.exe"
}
#[cfg(not(target_os = "windows"))]
fn gost_file_name() -> &'static str {
    "gost"
}

/// GOST 进程管理：单 EXE 内嵌二进制、运行时释放、命令构建、日志采集（脱敏）、停止/残留清理
pub struct GostManager {
    child: Mutex<Option<Child>>,
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

    /// 确保 gost 可执行文件就位：
    /// - Windows：exe 同目录（免安装可写），存在则复用，否则从内嵌字节释放
    /// - macOS/Linux：.app 包内目录只读，改释放到应用数据目录（~/.local/share 或 ~/Library/Application Support）
    fn ensure_gost(app: &AppHandle) -> Result<PathBuf, String> {
        #[cfg(target_os = "windows")]
        {
            let _ = app; // Windows 用 exe 同目录，不需要数据目录
            let exe_path = std::env::current_exe().map_err(|e| format!("无法定位程序目录: {}", e))?;
            let exe_dir = exe_path
                .parent()
                .ok_or_else(|| "无法定位程序目录".to_string())?
                .to_path_buf();
            let target = exe_dir.join(gost_file_name());
            if target.exists() {
                return Ok(target);
            }
            fs::write(&target, GOST_BIN).map_err(|e| format!("释放 gost 失败: {}", e))?;
            return Ok(target);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("无法获取应用数据目录: {}", e))?;
            fs::create_dir_all(&data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;
            let target = data_dir.join(gost_file_name());
            if target.exists() {
                return Ok(target);
            }
            fs::write(&target, GOST_BIN).map_err(|e| format!("释放 gost 失败: {}", e))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("设置 gost 权限失败: {}", e))?;
            }
            return Ok(target);
        }
    }

    /// 启动 gost；成功返回 Ok(())
    pub async fn start(
        &self,
        app: &AppHandle,
        ports: &[u16],
        server: &ServerConfig,
    ) -> Result<(), String> {
        {
            let guard = self.child.lock().map_err(|_| "内部锁错误".to_string())?;
            if guard.is_some() {
                return Err("连接已存在".to_string());
            }
        }

        // 防御：清理残留 gost 进程，避免端口占用
        self.kill_residual();

        // 确保 gost 就位（Windows exe 同目录 / macOS·Linux 应用数据目录，存在则用否则释放）
        let gost_path = Self::ensure_gost(app)?;
        let args = Self::build_args(ports, server);

        let mut cmd = Command::new(&gost_path);
        cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let mut child = cmd.spawn().map_err(|e| format!("启动 gost 失败: {}", e))?;

        // 后台线程读取 stdout/stderr，脱敏后写入日志缓冲
        let logs_out = Arc::clone(&self.logs);
        if let Some(stdout) = child.stdout.take() {
            tauri::async_runtime::spawn_blocking(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        push_log(&logs_out, l);
                    }
                }
            });
        }
        let logs_err = Arc::clone(&self.logs);
        if let Some(stderr) = child.stderr.take() {
            tauri::async_runtime::spawn_blocking(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        push_log(&logs_err, l);
                    }
                }
            });
        }

        *self.child.lock().map_err(|_| "内部锁错误".to_string())? = Some(child);
        Ok(())
    }

    /// 停止：Windows 杀整个进程树；unix kill；wait 回收子进程避免僵尸；随后按进程名清理残留
    pub fn stop(&self, _app: &AppHandle) {
        let child = self.child.lock().unwrap().take();
        if let Some(mut c) = child {
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &c.id().to_string()])
                    .creation_flags(0x08000000)
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = c.kill();
            }
            let _ = c.wait();
        }
        self.kill_residual();
    }

    /// 检测 gost 子进程是否已退出；退出则回收句柄并返回 true。
    /// 锁被毒化或未启动时返回 false（不误判为崩溃）。
    pub fn has_exited(&self) -> bool {
        let mut guard = match self.child.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        match guard.as_mut() {
            Some(c) => match c.try_wait() {
                Ok(Some(_)) => {
                    guard.take();
                    true
                }
                _ => false,
            },
            None => false,
        }
    }

    /// 按进程名清理残留（Windows 用 taskkill /T /F；unix 用 pkill -x）
    fn kill_residual(&self) {
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/IM", "gost.exe"])
                .creation_flags(0x08000000)
                .output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = Command::new("pkill").args(["-x", "gost"]).output();
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
