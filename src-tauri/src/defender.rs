/// Windows Defender 排除项：给 gost sidecar 进程添加白名单，避免被杀软误杀。
/// 仅 Windows 生效；macOS/Linux 编译为无操作。

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
pub fn add_gost_exclusion() -> Result<(), String> {
    let exe_dir = std::env::current_exe()
        .map_err(|e| format!("无法定位程序目录: {}", e))?
        .parent()
        .ok_or_else(|| "无法定位程序目录".to_string())?
        .to_string_lossy()
        .replace("'", "''");
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
try {{
    Add-MpPreference -ExclusionPath '{exe_dir}'
    'OK'
}} catch {{
    Write-Output ('ERR:' + $_.Exception.Message)
    exit 1
}}
"#
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text == "OK" || out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Defender 排除项添加失败：{}（可能原因：篡改保护(Tamper Protection)阻止 / Defender 服务未运行 / 组策略锁定）",
            text
        ))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn add_gost_exclusion() -> Result<(), String> {
    Ok(())
}
