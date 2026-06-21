<p align="center">
    <img src="https://sing-box.sagernet.org/assets/icon.svg" width="100px" align="center" />&nbsp;&nbsp;
    <img src="https://xtls.github.io/logo-light.svg" width="100px" align="center" />
    <h2 align="center">sing-box-with-xray</h2>
    <p align="center">
        一个极简 Windows 托盘程序, 使用 sing-box TUN 搭配 Xray
    </p>
</p>

## 简介

本项目用于在 Windows 上运行 sing-box 和 Xray。

sing-box 负责 TUN、路由和大部分流量处理。直连流量由 sing-box 直接出站, 代理流量由 sing-box 转发到本地 socks 出站, 再交给 Xray 与 VPS 通信。

项目现在以 Rust 编写的系统托盘程序作为主入口, 不提供软件主界面。运行后常驻系统托盘, 单击托盘图标即可打开菜单。

## 功能

- 重启
  - 重启 sing-box
  - 重启 xray
  - 重启 sing-box 和 xray
- 终止
  - 终止 sing-box
  - 终止 xray
  - 终止 sing-box 和 xray
- 更新
  - 更新 sing-box、xray 和 jq
- 切换配置文件
  - 切换 sing-box 配置
  - 切换 xray 配置

重启 sing-box 时, 程序会自动随机化 `sing-box.json` 中 TUN 入站的 `interface_name`, 用于避免残留 TUN 网卡导致网络不通。

## 目录结构

推荐将运行文件放在同一个目录中, 例如:

```text
sing-box-with-xray/
  sing-box-with-xray-tray.exe
  sing-box.exe
  xray.exe
  jq.exe
  sing-box.json
  xray.json
  Update.ps1
  icon/
  configs/
    sing-box/
      *.json
    xray/
      *.json
```

其中:

- `sing-box-with-xray-tray.exe`: 托盘程序主入口。
- `sing-box.exe`: sing-box 核心。
- `xray.exe`: Xray 核心。
- `jq.exe`: 当前更新脚本仍会更新 jq。
- `sing-box.json`: 当前正在使用的 sing-box 配置。
- `xray.json`: 当前正在使用的 xray 配置。
- `configs\sing-box`: 可切换的 sing-box 配置目录。
- `configs\xray`: 可切换的 xray 配置目录。
- `Update.ps1`: 当前用于更新 sing-box、xray 和 jq 的兼容脚本。

## 使用方法

1. 编译或下载 `sing-box-with-xray-tray.exe`。
2. 将 `sing-box.exe`、`xray.exe`、`sing-box.json`、`xray.json` 等文件放在同一目录。
3. 双击运行 `sing-box-with-xray-tray.exe`。
4. Windows 弹出 UAC 提示时选择允许。
5. 在系统托盘中单击或右键程序图标, 使用弹出菜单管理 sing-box 和 xray。

程序需要管理员权限运行, 因为 sing-box TUN 和 DNS 缓存清理等操作需要提升权限。

## 配置切换

将备用 sing-box 配置放入:

```text
configs\sing-box\
```

将备用 xray 配置放入:

```text
configs\xray\
```

托盘菜单会自动扫描这些目录下的 `.json` 文件。

选择某个 sing-box 配置后, 程序会将它复制为 `sing-box.json`, 然后重启 sing-box。

选择某个 xray 配置后, 程序会将它复制为 `xray.json`, 然后重启 xray。

## 更新

托盘菜单中的更新功能目前会调用 `Update.ps1`, 用于更新:

- sing-box
- xray
- jq

软件本体不会自动更新。

## 开发

本项目使用 Rust 开发 Windows 托盘程序。

需要安装:

- Rust stable toolchain
- Build Tools for Visual Studio 2022
- MSVC v143
- Windows 10/11 SDK

编译:

```powershell
cargo build --release
```

生成文件:

```text
target\release\sing-box-with-xray-tray.exe
```

项目已经在 `Cargo.toml` 中配置 release 体积优化:

```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

## 旧脚本

仓库中仍保留 `Restart.ps1`、`Stop.ps1` 和 `Update.ps1`。

当前主入口是 Rust 托盘程序。`Restart.ps1` 和 `Stop.ps1` 主要作为旧方案保留; `Update.ps1` 暂时仍被托盘程序用于更新 sing-box、xray 和 jq。
