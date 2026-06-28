//! 程序入口与核心逻辑。
//!
//! 负责应用生命周期管理（启动、运行、退出）、sing-box / xray 子进程的
//! 启停控制、系统托盘菜单分发、TUN 接口名随机化、DNS 缓存清理，
//! 以及孤立 WinTUN 设备节点的清理。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod error;
mod settings;
mod toast;
mod tray;
mod update;

use error::AppError;

use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::ptr::null;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use tracing::{Event, debug, error, info, warn};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::prelude::*;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, SetConsoleMode,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CREATE_NO_WINDOW, OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CM_Locate_DevNodeW, CR_SUCCESS,
    DN_HAS_PROBLEM, DN_STARTED,
};

#[derive(Clone, Copy)]
enum ConfigKind {
    SingBox,
    Xray,
}

#[derive(Clone)]
struct ConfigAction {
    kind: ConfigKind,
    path: PathBuf,
}

/// 全局应用状态。
struct AppState {
    /// 可执行文件所在目录，所有相对路径以此为基准。
    exe_dir: PathBuf,
    /// 菜单项 ID → 配置文件路径的映射，用于配置切换。
    sing_box_version: Option<String>,
    xray_version: Option<String>,
    /// GDI 位图句柄：绿色（运行中）、黄色（未运行）、红色（未安装）。
    icon_green: isize,
    icon_yellow: isize,
    icon_red: isize,
    settings: settings::Settings,
    /// 子进程句柄，用于直接 kill。
    child_sing_box: Option<Child>,
    child_xray: Option<Child>,
}

/// 全局应用状态，通过 OnceLock + Mutex 实现线程安全的单例。
static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

/// COM 初始化守卫，Drop 时自动调用 CoUninitialize。
struct ComGuard;

impl ComGuard {
    fn new() -> Result<Self, AppError> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|e| AppError::Msg(format!("初始化 COM 失败: {e}")))?;
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn main() {
    if let Err(err) = run() {
        tray::show_error(0, "启动失败", &err.to_string());
    }
}

/// 自定义日志格式：`{时间戳}{毫秒}Z [{级别}] {消息}`，级别带 ANSI 颜色。
struct BracketedLevel;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_BLUE: &str = "\x1b[34m";

fn level_color(level: &tracing::Level) -> &'static str {
    match *level {
        tracing::Level::INFO => ANSI_GREEN,
        tracing::Level::WARN => ANSI_YELLOW,
        tracing::Level::ERROR => ANSI_RED,
        tracing::Level::DEBUG => ANSI_BLUE,
        _ => "",
    }
}

impl<S, N> FormatEvent<S, N> for BracketedLevel
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> std::fmt::Result {
        let now = OffsetDateTime::now_utc();
        let level = event.metadata().level();
        let color = level_color(level);
        let reset = if writer.has_ansi_escapes() { ANSI_RESET } else { "" };
        let lc = if writer.has_ansi_escapes() { color } else { "" };
        write!(
            &mut writer,
            "{}{:03}Z [{lc}{level}{reset}] ",
            now.format(time::macros::format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second]"
            ))
            .map_err(|_| std::fmt::Error)?,
            now.millisecond(),
        )?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// 初始化日志系统。
///
/// 1. 为 Windows 控制台启用 ANSI 转义码支持（彩色输出）
/// 2. 创建 console_layer（stderr）和 file_layer（app.log）
/// 3. 日志级别优先使用 RUST_LOG 环境变量，否则使用 settings.toml 中的配置
fn init_logging(exe_dir: &Path, log_level: &str) -> Result<(), AppError> {
    // 启用 Windows Terminal 的 ANSI 转义码处理，否则颜色代码会显示为乱码
    unsafe {
        if let Ok(handle) = GetStdHandle(STD_ERROR_HANDLE) {
            let mut mode = CONSOLE_MODE::default();
            if GetConsoleMode(handle, &mut mode).is_ok() {
                let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact()
        .event_format(BracketedLevel)
        .with_writer(std::io::stderr);

    let log_path = exe_dir.join("app.log");
    let file = std::fs::File::create(log_path).map_err(|e| AppError::Msg(format!("创建日志文件失败: {e}")))?;
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .compact()
        .event_format(BracketedLevel)
        .with_writer(file);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(console_layer)
        .with(filter)
        .try_init()
        .map_err(|e| AppError::Msg(format!("初始化日志系统失败: {e}")))?;

    Ok(())
}

/// 应用主入口：初始化 COM → 创建目录 → 加载配置 → 初始化日志和 Toast →
/// 检测版本 → 创建托盘图标 → 运行消息循环 → 退出时清理 GDI 资源。
fn run() -> Result<(), AppError> {
    // 初始化 COM（单线程单元模式），Toast 通知和 ShellLink 都依赖 COM
    let _com = ComGuard::new()?;

    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path
        .parent()
        .ok_or(AppError::Msg("无法获取exe目录".into()))?
        .to_path_buf();

    fs::create_dir_all(exe_dir.join("core"))?;
    fs::create_dir_all(exe_dir.join("configs").join("sing-box"))?;
    fs::create_dir_all(exe_dir.join("configs").join("xray"))?;

    let app_settings = settings::Settings::load(&exe_dir);

    init_logging(&exe_dir, &app_settings.log.level)?;
    for w in settings::Settings::take_warnings() {
        warn!("{w}");
    }
    info!("程序启动, exe目录: {}", exe_dir.display());
    debug!(
        "生效配置: proxy={}({}), log_level={}, max_retries={}, retry_delay={}s",
        if app_settings.gh_proxy.enabled {
            "enabled"
        } else {
            "disabled"
        },
        app_settings.gh_proxy.url,
        app_settings.log.level,
        app_settings.download.max_retries,
        app_settings.download.retry_delay_secs,
    );
    toast::setup(&exe_path).map_err(|e| AppError::Msg(format!("初始化 Toast 通知失败: {e}")))?;

    APP.set(Mutex::new(AppState {
        exe_dir: exe_dir.clone(),
        sing_box_version: None,
        xray_version: None,
        icon_green: 0,
        icon_yellow: 0,
        icon_red: 0,
        settings: app_settings,
        child_sing_box: None,
        child_xray: None,
    }))
    .map_err(|_| AppError::Msg("初始化状态失败".into()))?;

    {
        let mut app = app_state_mut().ok_or(AppError::Msg("应用状态不可用".into()))?;
        unsafe {
            app.icon_green = tray::load_icon_bitmap(&exe_dir, "green_circle.ico");
            app.icon_yellow = tray::load_icon_bitmap(&exe_dir, "yellow_circle.ico");
            app.icon_red = tray::load_icon_bitmap(&exe_dir, "red_circle.ico");
        }
    }

    let tooltip = {
        let mut app = app_state_mut().ok_or(AppError::Msg("应用状态不可用".into()))?;
        let (sing_ver, xray_ver) = detect_versions(&app.exe_dir);
        app.sing_box_version = sing_ver;
        app.xray_version = xray_ver;
        format_tooltip(app.sing_box_version.as_deref(), app.xray_version.as_deref())
    };

    unsafe {
        let h_instance = GetModuleHandleW(None)
            .map_err(|e| AppError::Msg(format!("获取模块句柄失败: {e}")))?
            .0 as isize;
        let hwnd = tray::create_window(h_instance)?;
        tray::add_icon(hwnd, h_instance, &exe_dir)?;
        tray::set_tooltip(&tooltip);
        tray::run_message_loop();
    }

    if let Some(app) = app_state() {
        unsafe {
            use windows::Win32::Graphics::Gdi::DeleteObject;
            use windows::Win32::Graphics::Gdi::HGDIOBJ;
            if app.icon_green != 0 {
                let _ = DeleteObject(HGDIOBJ(app.icon_green as *mut std::ffi::c_void));
            }
            if app.icon_yellow != 0 {
                let _ = DeleteObject(HGDIOBJ(app.icon_yellow as *mut std::ffi::c_void));
            }
            if app.icon_red != 0 {
                let _ = DeleteObject(HGDIOBJ(app.icon_red as *mut std::ffi::c_void));
            }
        }
    }

    Ok(())
}

/// 分发托盘菜单命令。
///
/// 重启/终止/更新操作在独立线程中执行（避免阻塞 UI 线程），每个线程
/// 独立初始化 COM。切换配置操作在当前线程同步执行。退出操作终止所有
/// 进程并销毁窗口。
fn execute_menu_command(hwnd: isize, id: u16, config_actions: &HashMap<u16, ConfigAction>) {
    let result = match id {
        tray::ID_RESTART_SING
        | tray::ID_RESTART_XRAY
        | tray::ID_RESTART_ALL
        | tray::ID_STOP_SING
        | tray::ID_STOP_XRAY
        | tray::ID_STOP_ALL => {
            let exe_dir = match exe_dir() {
                Ok(d) => d,
                Err(e) => {
                    error!("获取 exe 目录失败: {e}");
                    tray::show_error(hwnd, "操作失败", &e.to_string());
                    return;
                }
            };
            // 子线程需要独立初始化 COM，否则 Toast 通知会失败
            std::thread::spawn(move || {
                let _com = ComGuard::new();
                let label = match id {
                    tray::ID_RESTART_SING => "重启 sing-box",
                    tray::ID_RESTART_XRAY => "重启 xray",
                    tray::ID_RESTART_ALL => "重启所有服务",
                    tray::ID_STOP_SING => "终止 sing-box",
                    tray::ID_STOP_XRAY => "终止 xray",
                    tray::ID_STOP_ALL => "终止所有服务",
                    _ => "",
                };
                info!("{label}");
                let result = match id {
                    tray::ID_RESTART_SING => restart_sing_box_at(&exe_dir),
                    tray::ID_RESTART_XRAY => restart_xray_at(&exe_dir),
                    tray::ID_RESTART_ALL => restart_all_at(&exe_dir),
                    tray::ID_STOP_SING => stop_processes(&["sing-box.exe"]),
                    tray::ID_STOP_XRAY => stop_processes(&["xray.exe"]),
                    tray::ID_STOP_ALL => stop_all(),
                    _ => unreachable!(),
                };
                if let Err(err) = result {
                    error!("操作失败: {err}");
                    toast::show_toast("操作失败", &err.to_string());
                }
            });
            return;
        }
        tray::ID_UPDATE_ALL | tray::ID_UPDATE_SING | tray::ID_UPDATE_XRAY => {
            let exe_dir = match exe_dir() {
                Ok(d) => d,
                Err(e) => {
                    error!("获取 exe 目录失败: {e}");
                    tray::show_error(hwnd, "操作失败", &e.to_string());
                    return;
                }
            };
            let (sing_ver, xray_ver) = {
                let app = match app_state() {
                    Some(a) => a,
                    None => {
                        error!("应用状态不可用");
                        tray::show_error(hwnd, "操作失败", "应用状态不可用");
                        return;
                    }
                };
                (app.sing_box_version.clone(), app.xray_version.clone())
            };
            let (gh_enabled, gh_url, max_retries, retry_delay) = {
                let app = match app_state() {
                    Some(a) => a,
                    None => {
                        error!("应用状态不可用");
                        tray::show_error(hwnd, "操作失败", "应用状态不可用");
                        return;
                    }
                };
                let s = &app.settings;
                (
                    s.gh_proxy.enabled,
                    s.gh_proxy.url.clone(),
                    s.download.max_retries,
                    s.download.retry_delay_secs,
                )
            };
            if let Err(e) = stop_all() {
                warn!("更新前终止进程失败: {e}");
            }
            std::thread::spawn(move || {
                let _com = ComGuard::new();
                let label = match id {
                    tray::ID_UPDATE_ALL => "更新所有核心",
                    tray::ID_UPDATE_SING => "更新 sing-box",
                    tray::ID_UPDATE_XRAY => "更新 xray",
                    _ => "",
                };
                info!("{label}");
                let result = match id {
                    tray::ID_UPDATE_ALL => update::update_cores(
                        &exe_dir,
                        sing_ver.as_deref(),
                        xray_ver.as_deref(),
                        gh_enabled,
                        &gh_url,
                        max_retries,
                        retry_delay,
                    ),
                    tray::ID_UPDATE_SING => update::update_sing_box(
                        &exe_dir,
                        sing_ver.as_deref(),
                        gh_enabled,
                        &gh_url,
                        max_retries,
                        retry_delay,
                    ),
                    tray::ID_UPDATE_XRAY => update::update_xray(
                        &exe_dir,
                        xray_ver.as_deref(),
                        gh_enabled,
                        &gh_url,
                        max_retries,
                        retry_delay,
                    ),
                    _ => unreachable!(),
                };
                refresh_versions_and_tooltip(&exe_dir);
                if let Err(e) = result {
                    error!("更新失败: {e}");
                    toast::show_toast("更新失败", &e.to_string());
                }
            });
            return;
        }
        tray::ID_OPEN_DIR => {
            let exe_dir = match exe_dir() {
                Ok(d) => d,
                Err(e) => {
                    error!("获取 exe 目录失败: {e}");
                    return;
                }
            };
            debug!("打开程序目录: {}", exe_dir.display());
            let _ = Command::new("explorer").arg(&exe_dir).spawn();
            return;
        }
        tray::ID_EXIT => {
            info!("退出程序");
            if let Err(e) = stop_all() {
                warn!("退出时终止进程失败: {e}");
            }
            unsafe {
                let _ = DestroyWindow(HWND(hwnd as *mut std::ffi::c_void));
            }
            Ok(())
        }
        _ => run_config_action(id, config_actions),
    };

    if let Err(err) = result {
        error!("菜单命令执行失败: {err}");
        toast::show_toast("操作失败", &err.to_string());
    }
}

fn restart_all_at(exe_dir: &Path) -> Result<(), AppError> {
    stop_all()?;
    start_sing_box_at(exe_dir)?;
    start_xray_at(exe_dir)
}

fn restart_sing_box_at(exe_dir: &Path) -> Result<(), AppError> {
    stop_processes(&["sing-box.exe"])?;
    start_sing_box_at(exe_dir)
}

fn restart_xray_at(exe_dir: &Path) -> Result<(), AppError> {
    stop_processes(&["xray.exe"])?;
    start_xray_at(exe_dir)
}

/// 终止所有已知子进程并刷新 DNS。
fn stop_all() -> Result<(), AppError> {
    stop_processes(&["sing-box.exe", "xray.exe"])
}

fn stop_processes(processes: &[&str]) -> Result<(), AppError> {
    // 先通过保存的 Child 句柄直接 kill
    if let Some(mut app) = app_state_mut() {
        for process in processes {
            match *process {
                "sing-box.exe" => {
                    if let Some(ref mut child) = app.child_sing_box {
                        let _ = child.kill();
                    }
                    app.child_sing_box = None;
                }
                "xray.exe" => {
                    if let Some(ref mut child) = app.child_xray {
                        let _ = child.kill();
                    }
                    app.child_xray = None;
                }
                _ => {}
            }
        }
    }
    // 再通过进程名枚举兜底（覆盖其他来源启动的同名进程）
    for process in processes {
        info!("终止进程: {process}");
        kill_processes_by_name(process);
    }
    flush_dns();
    Ok(())
}

/// 通过 Win32 ToolHelp API 枚举所有与 `exe_name` 匹配的进程，返回 PID 列表。
fn find_pids_by_name(exe_name: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            warn!("CreateToolhelp32Snapshot 失败，无法枚举进程");
            return pids;
        };

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name_bytes = &entry.szExeFile[..end];
                let name = String::from_utf16_lossy(name_bytes);
                if name.eq_ignore_ascii_case(exe_name) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }
    pids
}

/// 终止与 `exe_name` 匹配的所有进程。
fn kill_processes_by_name(exe_name: &str) {
    for pid in find_pids_by_name(exe_name) {
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                debug!("终止进程: {} (PID {})", exe_name, pid);
                let _ = TerminateProcess(handle, 1);
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// 检查指定名称的进程是否正在运行。
fn is_process_running(exe_name: &str) -> bool {
    !find_pids_by_name(exe_name).is_empty()
}

/// 启动 sing-box 子进程。启动前清理孤立 WinTUN 设备并随机化 TUN 接口名。
fn start_sing_box_at(exe_dir: &Path) -> Result<(), AppError> {
    cleanup_orphaned_wintun();

    let exe = exe_dir.join("core").join("sing-box.exe");
    let config = exe_dir.join("configs").join("sing-box.json");

    ensure_exists(&exe)?;
    ensure_exists(&config)?;
    randomize_tun_name(&config)?;

    info!("启动 sing-box");
    let mut child = hidden_command(exe)
        .args(["run", "-D"])
        .arg(exe_dir)
        .arg("-c")
        .arg(config)
        .current_dir(exe_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Msg(format!("启动 sing-box 失败: {e}")))?;
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                warn!("[sing-box] {line}");
            }
        });
    }
    if let Some(mut app) = app_state_mut() {
        app.child_sing_box = Some(child);
    }

    Ok(())
}

/// 启动 xray 子进程，stderr 输出重定向到日志。
fn start_xray_at(exe_dir: &Path) -> Result<(), AppError> {
    let exe = exe_dir.join("core").join("xray.exe");
    let config = exe_dir.join("configs").join("xray.json");

    ensure_exists(&exe)?;
    ensure_exists(&config)?;

    info!("启动 xray");
    let mut child = hidden_command(exe)
        .args(["run", "-c"])
        .arg(config)
        .current_dir(exe_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Msg(format!("启动 xray 失败: {e}")))?;
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                warn!("[xray] {line}");
            }
        });
    }
    if let Some(mut app) = app_state_mut() {
        app.child_xray = Some(child);
    }

    Ok(())
}

/// 执行配置切换操作：将选中的配置文件复制到活跃配置路径并重启对应服务。
fn run_config_action(id: u16, config_actions: &HashMap<u16, ConfigAction>) -> Result<(), AppError> {
    let Some(action) = config_actions.get(&id).cloned() else {
        return Ok(());
    };

    let exe_dir = exe_dir()?;
    info!("切换配置: {}", action.path.display());
    match action.kind {
        ConfigKind::SingBox => {
            let dest = exe_dir.join("configs").join("sing-box.json");
            debug!("复制配置: {} -> {}", action.path.display(), dest.display());
            fs::copy(&action.path, dest).map_err(|e| AppError::Msg(format!("切换 sing-box 配置失败: {e}")))?;
            restart_sing_box_at(&exe_dir)
        }
        ConfigKind::Xray => {
            let dest = exe_dir.join("configs").join("xray.json");
            debug!("复制配置: {} -> {}", action.path.display(), dest.display());
            fs::copy(&action.path, dest).map_err(|e| AppError::Msg(format!("切换 xray 配置失败: {e}")))?;
            restart_xray_at(&exe_dir)
        }
    }
}

/// 随机化 sing-box 配置中的 TUN 接口名。
///
/// sing-box TUN 适配器在 Windows 上以固定名称注册，重启时如果旧适配器
/// 未完全释放会导致冲突。通过每次启动时生成随机 6 位十六进制名称来避免。
///
/// 使用字符串替换而非 JSON 序列化来保留原始配置的格式和注释。
fn randomize_tun_name(config_path: &Path) -> Result<(), AppError> {
    let text = fs::read_to_string(config_path).map_err(|e| AppError::Msg(format!("读取 sing-box 配置失败: {e}")))?;
    let json: Value = serde_json::from_str(&text).map_err(|e| AppError::Msg(format!("解析 sing-box 配置失败: {e}")))?;
    let new_name = random_hex_name();

    let old_name = json
        .get("inbounds")
        .and_then(Value::as_array)
        .and_then(|inbounds| {
            inbounds.iter().find_map(|inbound| {
                if inbound.get("type").and_then(Value::as_str) == Some("tun") {
                    inbound.get("interface_name").and_then(Value::as_str)
                } else {
                    None
                }
            })
        })
        .ok_or(AppError::Msg("未在 sing-box.json 中找到 type=tun 的 inbound".into()))?;

    if old_name == new_name {
        return Ok(());
    }

    debug!("随机化 TUN 接口名: {old_name} -> {new_name}");

    let old_pattern = format!("\"interface_name\": \"{}\"", old_name);
    let new_pattern = format!("\"interface_name\": \"{}\"", new_name);
    let new_text = text.replacen(&old_pattern, &new_pattern, 1);
    fs::write(config_path, new_text).map_err(|e| AppError::Msg(format!("写入 sing-box 配置失败: {e}")))
}

/// 生成 6 位随机十六进制字符串，用作 TUN 接口名。
/// 使用 RandomState 对时间戳做哈希，每次运行种子不同。
fn random_hex_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    let seed = std::hash::RandomState::new();
    let mut hasher = seed.build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    hasher.write_u128(nanos);
    format!("{:06x}", hasher.finish() & 0xFF_FFFF)
}

/// 创建不显示控制台窗口的子进程 Command。
fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW.0);
    command
}

// dnsapi.dll 导入，用于刷新系统 DNS 缓存。
#[link(name = "dnsapi")]
unsafe extern "system" {
    fn DnsFlushResolverCache() -> i32;
}

/// 刷新 DNS 缓存。代理进程终止后，残留的 DNS 缓存可能导致解析失败。
fn flush_dns() {
    let result = unsafe { DnsFlushResolverCache() };
    if result != 0 {
        debug!("DNS 缓存刷新成功");
    } else {
        warn!("DNS 缓存刷新失败");
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ProcessState {
    NotInstalled,
    NotRunning,
    Running,
}

fn sing_box_state(app: &AppState) -> ProcessState {
    if !app.exe_dir.join("core").join("sing-box.exe").exists() {
        return ProcessState::NotInstalled;
    }
    if is_process_running("sing-box.exe") {
        ProcessState::Running
    } else {
        ProcessState::NotRunning
    }
}

fn xray_state(app: &AppState) -> ProcessState {
    if !app.exe_dir.join("core").join("xray.exe").exists() {
        return ProcessState::NotInstalled;
    }
    if is_process_running("xray.exe") {
        ProcessState::Running
    } else {
        ProcessState::NotRunning
    }
}

/// 清理孤立或异常的 WinTUN 设备节点。
///
/// sing-box TUN 模式依赖 WinTUN 驱动，异常退出后可能残留无效设备节点，
/// 导致下次启动时 TUN 接口创建失败。此函数：
///
/// 1. 通过 `CM_Get_Device_ID_ListW` 枚举所有设备实例 ID
/// 2. 解析双 null 结尾的多字符串缓冲区
/// 3. 筛选包含 "WINTUN" 的设备
/// 4. 检查设备状态：设备节点不存在（CR_NO_SUCH_DEVNODE）或未启动 / 有异常
/// 5. 通过 `pnputil /remove-device` 移除问题设备
/// 6. 最后执行 `pnputil /scan-devices` 重新扫描硬件
fn cleanup_orphaned_wintun() {
    const CR_NO_SUCH_DEVNODE: u32 = 0x0D; // 设备节点不存在

    debug!("检查孤立 WinTUN 设备...");
    let mut instance_ids: Vec<String> = Vec::new();

    unsafe {
        let mut size = 0u32;
        if CM_Get_Device_ID_List_SizeW(&mut size, null(), 0) != CR_SUCCESS {
            return;
        }
        if size == 0 {
            return;
        }

        let mut buffer: Vec<u16> = vec![0u16; size as usize];
        if CM_Get_Device_ID_ListW(null(), buffer.as_mut_ptr(), size, 0) != CR_SUCCESS {
            return;
        }

        let mut start = 0usize;
        while start < buffer.len() {
            let end = buffer[start..]
                .iter()
                .position(|&c| c == 0)
                .map(|p| start + p)
                .unwrap_or(buffer.len());
            if end == start {
                break;
            }
            let id = String::from_utf16_lossy(&buffer[start..end]);

            if id.to_uppercase().contains("WINTUN") {
                let mut dev_inst = 0u32;
                let wide_id = wide(&id);
                let locate_ret = CM_Locate_DevNodeW(&mut dev_inst, wide_id.as_ptr(), 0);

                if locate_ret == CR_NO_SUCH_DEVNODE {
                    instance_ids.push(id);
                } else if locate_ret == CR_SUCCESS {
                    // 设备存在，检查是否未启动或有异常问题
                    let mut status = 0u32;
                    let mut problem = 0u32;
                    if CM_Get_DevNode_Status(&mut status, &mut problem, dev_inst, 0) == CR_SUCCESS
                        && ((status & DN_STARTED) == 0 || (status & DN_HAS_PROBLEM) != 0)
                    {
                        instance_ids.push(id);
                    }
                }
            }

            start = end + 1;
        }
    }

    if instance_ids.is_empty() {
        debug!("未发现孤立 WinTUN 设备");
        return;
    }
    debug!("发现 {} 个孤立 WinTUN 设备", instance_ids.len());

    for id in &instance_ids {
        debug!("移除孤立 WinTUN 设备: {id}");
        let result = hidden_command("pnputil").args(["/remove-device", id.as_str()]).status();
        match result {
            Ok(status) => debug!("pnputil /remove-device {id}: {status}"),
            Err(e) => warn!("pnputil /remove-device {id} 执行失败: {e}"),
        }
    }

    if !instance_ids.is_empty() {
        debug!("扫描硬件变更 ({} 个设备已移除)", instance_ids.len());
        let _ = hidden_command("pnputil").arg("/scan-devices").status();
    }
}

fn ensure_exists(path: &Path) -> Result<(), AppError> {
    if path.exists() {
        Ok(())
    } else {
        Err(AppError::Msg(format!("文件不存在: {}", path.display())))
    }
}

/// 从多个目录中收集所有 .json 文件，按文件名排序去重。
fn find_json_configs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                paths.push(path);
            }
        }
    }

    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    paths.dedup();
    paths
}

fn exe_dir() -> Result<PathBuf, AppError> {
    app_state()
        .map(|app| app.exe_dir.clone())
        .ok_or_else(|| AppError::Msg("应用状态不可用".into()))
}

fn app_state() -> Option<std::sync::MutexGuard<'static, AppState>> {
    APP.get()?.lock().ok()
}

/// 获取可变应用状态。与 `app_state()` 相同（Mutex::lock 返回可变守卫），
/// 语义上区分只读和可变访问意图。
fn app_state_mut() -> Option<std::sync::MutexGuard<'static, AppState>> {
    app_state()
}

pub(crate) fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_hex_name() {
        let name1 = random_hex_name();
        let name2 = random_hex_name();
        assert_eq!(name1.len(), 6);
        assert!(name1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(name1, name2);
    }
}

/// 检测本地 sing-box 和 xray 的版本。
/// 版本号为 "0.0.0" 表示可执行文件存在但无法获取版本，视为未安装。
fn detect_versions(exe_dir: &Path) -> (Option<String>, Option<String>) {
    let sing_exe = exe_dir.join("core").join("sing-box.exe");
    let xray_exe = exe_dir.join("core").join("xray.exe");
    let sing_ver = if sing_exe.exists() {
        let v = update::get_local_version(&sing_exe, "version");
        if v != "0.0.0" { Some(v) } else { None }
    } else {
        None
    };
    let xray_ver = if xray_exe.exists() {
        let v = update::get_local_version(&xray_exe, "version");
        if v != "0.0.0" { Some(v) } else { None }
    } else {
        None
    };
    (sing_ver, xray_ver)
}

/// 格式化托盘提示文本，显示两个核心的版本状态。
fn format_tooltip(sing_ver: Option<&str>, xray_ver: Option<&str>) -> String {
    let sing = match sing_ver {
        Some(v) => format!("sing-box v{}", v),
        None => "sing-box 未安装".to_string(),
    };
    let xray = match xray_ver {
        Some(v) => format!("xray v{}", v),
        None => "xray 未安装".to_string(),
    };
    format!("{}\n{}", sing, xray)
}

/// 重新检测版本并更新托盘提示文本。
fn refresh_versions_and_tooltip(exe_dir: &Path) {
    if let Some(mut app) = app_state_mut() {
        let (sing_ver, xray_ver) = detect_versions(exe_dir);
        app.sing_box_version = sing_ver;
        app.xray_version = xray_ver;
        let tooltip = format_tooltip(app.sing_box_version.as_deref(), app.xray_version.as_deref());
        drop(app);
        tray::set_tooltip(&tooltip);
    }
}
