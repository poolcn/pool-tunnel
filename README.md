# pool-tunnel

pool.cn.com 矿池加密连接 — Rust + Tauri 2 跨平台桌面客户端（Windows / macOS / Ubuntu）。

原 .NET Framework 4.8 WPF 版（`tunnel.exe`）的跨平台重构：拉取矿池列表、按币种选择端口、启动内嵌 GOST（sidecar）建立加密转发隧道，支持在线矿机统计与上报。

## 功能

- HTTPS 拉取 `https://pool.cn.com/tunnel/pool.txt`（10s 超时、重试 2 次、间隔 2s，不阻塞 UI）
- 按币种首次出现顺序分组展示；勾选持久化（重启恢复）
- **连接前强制刷新最新服务器配置**（`server1`/`server2`/`gostserver`），拉取失败禁止连接
- 内嵌 GOST v2.12.0 sidecar：`-L` 去重升序 + 单个 `-F`（参数数组直传，不经 shell）
- 日志区：500 行内存、IP/域名脱敏（首末各 1 字符中间 `*`）、复制剪贴板
- 在线矿机统计：本地端口 ∈ 已开端口 且 ESTABLISHED 的 TCP 连接数（5s 刷新，标题栏显示）
- 公网 IP 获取（IPv4 优先）+ 每 10s 上报 `https://pool.cn.com/tunnel/online.php`
- 系统托盘、单实例、窗口最小化隐藏；标题栏版本号 V1.0.0（取自 `tauri.conf.json` version）
- Windows 专属：Defender 排除项（启动自动添加）、NSIS 安装器 perMachine（需管理员）

## 目录结构

```
pool-tunnel/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs / lib.rs      # 入口 + Builder/插件/命令注册
│   │   ├── commands.rs           # Tauri commands + 后台定时任务（5s 统计/10s 上报）
│   │   ├── config.rs             # pool.txt 拉取/解析/缓存/勾选
│   │   ├── gost.rs               # GOST 命令构建 + sidecar 进程/日志/清理
│   │   ├── net.rs                # TCP 连接统计（netstat2，三平台统一）
│   │   ├── reporter.rs           # 公网 IP + online.php 上报
│   │   ├── defender.rs           # Windows Defender 排除项（cfg windows）
│   │   ├── state.rs / models.rs
│   ├── binaries/                 # gost sidecar（三平台，按 target triple 命名）
│   ├── capabilities/             # 权限
│   ├── icons/                    # 应用图标
│   ├── tauri.conf.json
│   └── Cargo.toml
├── ui/                           # 前端（Vanilla HTML/CSS/JS 单文件）
└── README.md
```

## 前置依赖（按平台）

**通用**
- Rust 工具链（rustup 安装 stable）

**Windows**
- Visual Studio Build Tools（含 C++ 桌面负载 / MSVC 链接器）— `cargo build` 需要 `link.exe`
- WebView2 运行时（Windows 10/11 自带）

**macOS**
- Xcode Command Line Tools（`xcode-select --install`）
- WebKit（系统自带）

**Ubuntu**
```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev build-essential curl wget file libssl-dev libayatana-appindicator3-dev
```

## 开发运行

```bash
# 首次：安装 Tauri CLI（任选其一）
cargo install tauri-cli --version "^2" --locked   # 或 npm i -g @tauri-apps/cli

cd pool-tunnel
cargo tauri dev        # 开发模式（热重载）
cargo tauri build      # 打包发布
```

不带 tauri-cli 时可用：

```bash
cd src-tauri && cargo run        # 仅编译运行（不打包）
cd src-tauri && cargo check      # 快速检查编译错误
```

## 打包发布

`cargo tauri build` 产出：

| 平台 | 产物 |
|---|---|
| Windows | `src-tauri/target/release/bundle/nsis/*.exe`（安装包）+ `msi/*.msi` |
| macOS（Intel） | `bundle/dmg/*_x64.dmg`，构建参数 `--target x86_64-apple-darwin` |
| macOS（M 系列） | `bundle/dmg/*_aarch64.dmg`，构建参数 `--target aarch64-apple-darwin` |
| Ubuntu | `src-tauri/target/release/bundle/deb/*.deb` + `appimage/*.AppImage`（需 Linux 环境） |

> - macOS 双架构分别出包，各自携带对应架构的 gost sidecar（`gost-x86_64-apple-darwin` / `gost-aarch64-apple-darwin`）。
> - 三个平台产物必须在对应平台构建（或用 CI 矩阵：windows-latest / macos-latest / ubuntu-22.04，见 `.github/workflows/release.yml`，CI 会自动出 Windows + macOS 双架构 + Ubuntu 全部安装包）。

## 数据目录

- Windows：`%APPDATA%\com.poolcn.tunnel\`
- macOS：`~/Library/Application Support/com.poolcn.tunnel/`
- Linux：`~/.local/share/com.poolcn.tunnel/`
- 含 `pool_cache.txt`（矿池缓存）、`config.json`（勾选持久化）

## 已知限制

- 未签名：Windows UAC 提示「未知发布者」、macOS 门禁提示为正常现象
- GOST sidecar 架构：Windows / Linux 为 x86_64；macOS 提供 Intel（x86_64）与 M 系列（arm64）双架构
- 在线矿机统计口径：本地端口 ∈ 已开端口 + ESTABLISHED（三平台一致）
