use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use tauri::AppHandle;
#[cfg(not(target_os = "windows"))]
use tauri::Manager; // 仅非 Windows 分支用 app.path() 取应用数据目录

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
    /// 本实例释放的 gost 完整路径（ensure_gost 后写入），残留清理按此路径过滤，避免误杀同名进程
    gost_path: Mutex<Option<PathBuf>>,
}

impl GostManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            logs: Arc::new(Mutex::new(Vec::new())),
            gost_path: Mutex::new(None),
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

    /// 已存在的 gost 文件是否可用：大小与内嵌字节一致才复用。
    /// 能发现半截文件（上次写入被强杀）与版本更替（大小几乎必然变化），避免损坏/旧版被永久复用。
    fn gost_file_valid(target: &PathBuf) -> bool {
        target
            .metadata()
            .map(|m| m.len() as usize == GOST_BIN.len())
            .unwrap_or(false)
    }

    /// 原子写入 gost：先写临时文件再 rename，杜绝强杀留下半截可执行文件。
    fn write_gost_atomic(target: &PathBuf) -> Result<(), String> {
        let tmp = target.with_extension("tmp");
        fs::write(&tmp, GOST_BIN).map_err(|e| format!("释放 gost 失败: {}", e))?;
        // Windows 上 rename 不能覆盖已存在目标，先删（窗口极小，且目标已判定为损坏/旧版）
        if target.exists() {
            let _ = fs::remove_file(target);
        }
        fs::rename(&tmp, target).map_err(|e| format!("替换 gost 失败: {}", e))?;
        Ok(())
    }

    /// 确保 gost 可执行文件就位：
    /// - Windows：exe 同目录（免安装可写），存在且大小一致则复用，否则从内嵌字节原子释放
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
            if Self::gost_file_valid(&target) {
                return Ok(target);
            }
            Self::write_gost_atomic(&target)?;
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
            if !Self::gost_file_valid(&target) {
                Self::write_gost_atomic(&target)?;
            }
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

        // 确保 gost 就位（Windows exe 同目录 / macOS·Linux 应用数据目录，大小一致才复用否则原子释放）
        let gost_path = Self::ensure_gost(app)?;
        *self.gost_path.lock().unwrap_or_else(|e| e.into_inner()) = Some(gost_path.clone());

        // 防御：清理残留 gost 进程（仅限本实例释放路径），避免端口占用
        self.kill_residual();

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
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
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

    /// 按完整路径清理残留：只杀本实例释放的 gost，绝不误杀用户机器上其他同名代理工具。
    /// 路径未知（从未成功 ensure_gost）时直接跳过——没有我们的释放路径就没有我们的残留。
    fn kill_residual(&self) {
        let path = self
            .gost_path
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(path) = path else { return };
        #[cfg(target_os = "windows")]
        {
            let escaped = path.to_string_lossy().replace("'", "''");
            let script = format!(
                "Get-CimInstance Win32_Process -Filter \"Name='gost.exe'\" | Where-Object {{ $_.ExecutablePath -eq '{}' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
                escaped
            );
            let _ = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .creation_flags(0x08000000)
                .output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            // pkill -f 匹配完整命令行（含路径），只命中本实例释放的 gost
            let pattern = path.to_string_lossy().into_owned();
            let _ = Command::new("pkill").args(["-f", &pattern]).output();
        }
    }

    pub fn get_logs(&self) -> String {
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).join("\n")
    }

    pub fn has_logs(&self) -> bool {
        !self.logs.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    pub fn clear_logs(&self) {
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

fn push_log(logs: &Mutex<Vec<String>>, line: String) {
    let masked = mask_addresses(&line);
    let mut guard = logs.lock().unwrap_or_else(|e| e.into_inner());
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
/// 白名单：.exe 结尾的可执行文件名（如 gost.exe）不脱敏，避免排障时关键信息被误伤
fn mask_addresses(line: &str) -> String {
    let step1 = ip_re().replace_all(line, |caps: &regex::Captures| mask_token(&caps[0]));
    domain_re()
        .replace_all(&step1, |caps: &regex::Captures| {
            let token = &caps[0];
            if token.to_ascii_lowercase().ends_with(".exe") {
                token.to_string()
            } else {
                mask_token(token)
            }
        })
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
