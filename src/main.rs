#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, LoadImageW, MessageBoxW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON,
    LR_DEFAULTSIZE, LR_LOADFROMFILE, MB_ICONERROR, MB_OK, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MSG, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_LBUTTONUP,
    WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

const WM_TRAY_ICON: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;

const ID_RESTART_SING: u16 = 101;
const ID_RESTART_XRAY: u16 = 102;
const ID_RESTART_ALL: u16 = 103;
const ID_STOP_SING: u16 = 201;
const ID_STOP_XRAY: u16 = 202;
const ID_STOP_ALL: u16 = 203;
const ID_UPDATE_CORE: u16 = 301;
const ID_EXIT: u16 = 999;
const ID_SING_CONFIG_BASE: u16 = 1000;
const ID_XRAY_CONFIG_BASE: u16 = 2000;

static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

#[derive(Clone)]
enum ConfigKind {
    SingBox,
    Xray,
}

#[derive(Clone)]
struct ConfigAction {
    kind: ConfigKind,
    path: PathBuf,
}

struct AppState {
    work_dir: PathBuf,
    config_actions: HashMap<u16, ConfigAction>,
}

fn main() {
    if let Err(err) = run() {
        show_error(null_mut(), "启动失败", &err);
    }
}

fn run() -> Result<(), String> {
    let work_dir = detect_work_dir();
    fs::create_dir_all(work_dir.join("configs").join("sing-box")).map_err(|e| e.to_string())?;
    fs::create_dir_all(work_dir.join("configs").join("xray")).map_err(|e| e.to_string())?;

    unsafe {
        let h_instance = GetModuleHandleW(null());
        let class_name = wide("SingBoxWithXrayTrayWindow");

        let wnd_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: h_instance,
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };

        if RegisterClassW(&wnd_class) == 0 {
            return Err("注册托盘窗口类失败".to_string());
        }

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("sing-box-with-xray").as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            null_mut(),
            null_mut(),
            h_instance,
            null(),
        );

        if hwnd.is_null() {
            return Err("创建托盘窗口失败".to_string());
        }

        APP.set(Mutex::new(AppState {
            work_dir,
            config_actions: HashMap::new(),
        }))
        .map_err(|_| "初始化状态失败".to_string())?;

        add_tray_icon(hwnd, h_instance)?;

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY_ICON => {
            let event = lparam as u32;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                show_tray_menu(hwnd);
            }
            0
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn add_tray_icon(hwnd: HWND, h_instance: HINSTANCE) -> Result<(), String> {
    let icon = load_app_icon(h_instance);
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_ICON;
    nid.hIcon = icon;
    set_wstr_array(&mut nid.szTip, "sing-box-with-xray");

    if Shell_NotifyIconW(NIM_ADD, &mut nid) == 0 {
        return Err("添加系统托盘图标失败".to_string());
    }

    Ok(())
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    Shell_NotifyIconW(NIM_DELETE, &mut nid);
}

unsafe fn load_app_icon(h_instance: HINSTANCE) -> HICON {
    if let Some(app) = app_state() {
        let icon_path = app.work_dir.join("icon").join("Restart.ico");
        if icon_path.exists() {
            let icon_path = wide_path(&icon_path);
            let icon = LoadImageW(
                h_instance,
                icon_path.as_ptr(),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE | LR_DEFAULTSIZE,
            );
            if !icon.is_null() {
                return icon as HICON;
            }
        }
    }

    LoadIconW(null_mut(), IDI_APPLICATION)
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let selected = if let Some(mut app) = app_state_mut() {
        let menu = CreatePopupMenu();
        let restart_menu = CreatePopupMenu();
        let stop_menu = CreatePopupMenu();
        let update_menu = CreatePopupMenu();
        let switch_menu = CreatePopupMenu();
        let sing_menu = CreatePopupMenu();
        let xray_menu = CreatePopupMenu();

        append_item(restart_menu, ID_RESTART_SING, "重启 sing-box");
        append_item(restart_menu, ID_RESTART_XRAY, "重启 xray");
        append_item(restart_menu, ID_RESTART_ALL, "重启 sing-box 和 xray");

        append_item(stop_menu, ID_STOP_SING, "终止 sing-box");
        append_item(stop_menu, ID_STOP_XRAY, "终止 xray");
        append_item(stop_menu, ID_STOP_ALL, "终止 sing-box 和 xray");

        append_item(update_menu, ID_UPDATE_CORE, "更新 sing-box / xray / jq");

        app.config_actions.clear();
        let work_dir = app.work_dir.clone();
        append_config_items(
            &mut app,
            sing_menu,
            ConfigKind::SingBox,
            ID_SING_CONFIG_BASE,
            &[work_dir.join("configs").join("sing-box")],
        );
        append_config_items(
            &mut app,
            xray_menu,
            ConfigKind::Xray,
            ID_XRAY_CONFIG_BASE,
            &[
                work_dir.join("configs").join("xray"),
                work_dir.join("templates"),
            ],
        );

        append_submenu(switch_menu, sing_menu, "切换 sing-box 配置");
        append_submenu(switch_menu, xray_menu, "切换 xray 配置");

        append_submenu(menu, restart_menu, "重启");
        append_submenu(menu, stop_menu, "终止");
        append_submenu(menu, update_menu, "更新");
        append_submenu(menu, switch_menu, "切换配置文件");
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        append_item(menu, ID_EXIT, "退出托盘程序");

        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        );

        DestroyMenu(menu);

        selected
    } else {
        0
    };

    if selected != 0 {
        execute_menu_command(hwnd, selected as u16);
    }
}

unsafe fn append_item(menu: HMENU, id: u16, label: &str) {
    let label = wide(label);
    AppendMenuW(menu, MF_STRING, id as usize, label.as_ptr());
}

unsafe fn append_disabled_item(menu: HMENU, label: &str) {
    let label = wide(label);
    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, label.as_ptr());
}

unsafe fn append_submenu(menu: HMENU, submenu: HMENU, label: &str) {
    let label = wide(label);
    AppendMenuW(menu, MF_STRING | MF_POPUP, submenu as usize, label.as_ptr());
}

unsafe fn append_config_items(
    app: &mut AppState,
    menu: HMENU,
    kind: ConfigKind,
    base_id: u16,
    dirs: &[PathBuf],
) {
    let mut id = base_id;
    let mut added = 0;

    for path in find_json_configs(dirs) {
        if id >= base_id + 900 {
            break;
        }

        let label = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("未命名配置")
            .to_string();
        append_item(menu, id, &label);
        app.config_actions.insert(
            id,
            ConfigAction {
                kind: kind.clone(),
                path,
            },
        );
        id += 1;
        added += 1;
    }

    if added == 0 {
        append_disabled_item(menu, "未找到 .json 配置");
    }
}

fn execute_menu_command(hwnd: HWND, id: u16) {
    let result = match id {
        ID_RESTART_SING => restart_sing_box(),
        ID_RESTART_XRAY => restart_xray(),
        ID_RESTART_ALL => restart_all(),
        ID_STOP_SING => stop_processes(&["sing-box.exe"]),
        ID_STOP_XRAY => stop_processes(&["xray.exe"]),
        ID_STOP_ALL => stop_all(),
        ID_UPDATE_CORE => run_update_script(),
        ID_EXIT => {
            unsafe { DestroyWindow(hwnd) };
            Ok(())
        }
        _ => run_config_action(id),
    };

    if let Err(err) = result {
        show_error(hwnd, "操作失败", &err);
    }
}

fn restart_all() -> Result<(), String> {
    stop_all()?;
    start_sing_box()?;
    start_xray()
}

fn restart_sing_box() -> Result<(), String> {
    stop_processes(&["sing-box.exe"])?;
    start_sing_box()
}

fn restart_xray() -> Result<(), String> {
    stop_processes(&["xray.exe"])?;
    start_xray()
}

fn stop_all() -> Result<(), String> {
    stop_processes(&["sing-box.exe", "xray.exe"])?;
    flush_dns();
    Ok(())
}

fn stop_processes(processes: &[&str]) -> Result<(), String> {
    for process in processes {
        let _ = hidden_command("taskkill")
            .args(["/F", "/IM", process])
            .status();
    }
    Ok(())
}

fn start_sing_box() -> Result<(), String> {
    let work_dir = work_dir()?;
    let exe = work_dir.join("sing-box.exe");
    let config = work_dir.join("sing-box.json");

    ensure_exists(&exe)?;
    ensure_exists(&config)?;
    randomize_tun_name(&config)?;

    hidden_command(exe)
        .args(["run", "-D"])
        .arg(&work_dir)
        .arg("-c")
        .arg(config)
        .current_dir(work_dir)
        .spawn()
        .map_err(|e| format!("启动 sing-box 失败: {e}"))?;

    Ok(())
}

fn start_xray() -> Result<(), String> {
    let work_dir = work_dir()?;
    let exe = work_dir.join("xray.exe");
    let config = work_dir.join("xray.json");

    ensure_exists(&exe)?;
    ensure_exists(&config)?;

    hidden_command(exe)
        .args(["run", "-c"])
        .arg(config)
        .current_dir(work_dir)
        .spawn()
        .map_err(|e| format!("启动 xray 失败: {e}"))?;

    Ok(())
}

fn run_update_script() -> Result<(), String> {
    let work_dir = work_dir()?;
    let script = work_dir.join("Update.ps1");
    ensure_exists(&script)?;

    Command::new("powershell.exe")
        .args(["-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .current_dir(work_dir)
        .spawn()
        .map_err(|e| format!("启动更新脚本失败: {e}"))?;

    Ok(())
}

fn run_config_action(id: u16) -> Result<(), String> {
    let action = {
        let app = app_state().ok_or("应用状态不可用")?;
        app.config_actions.get(&id).cloned()
    };

    let Some(action) = action else {
        return Ok(());
    };

    let work_dir = work_dir()?;
    match action.kind {
        ConfigKind::SingBox => {
            fs::copy(&action.path, work_dir.join("sing-box.json"))
                .map_err(|e| format!("切换 sing-box 配置失败: {e}"))?;
            restart_sing_box()
        }
        ConfigKind::Xray => {
            fs::copy(&action.path, work_dir.join("xray.json"))
                .map_err(|e| format!("切换 xray 配置失败: {e}"))?;
            restart_xray()
        }
    }
}

fn randomize_tun_name(config_path: &Path) -> Result<(), String> {
    let text =
        fs::read_to_string(config_path).map_err(|e| format!("读取 sing-box 配置失败: {e}"))?;
    let mut json: Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 sing-box 配置失败: {e}"))?;
    let new_name = random_hex_name();
    let mut changed = false;

    if let Some(inbounds) = json.get_mut("inbounds").and_then(Value::as_array_mut) {
        for inbound in inbounds {
            if inbound.get("type").and_then(Value::as_str) == Some("tun") {
                if let Some(obj) = inbound.as_object_mut() {
                    obj.insert(
                        "interface_name".to_string(),
                        Value::String(new_name.clone()),
                    );
                    changed = true;
                }
            }
        }
    }

    if !changed {
        return Err("未在 sing-box.json 中找到 type=tun 的 inbound".to_string());
    }

    let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    fs::write(config_path, text).map_err(|e| format!("写入 sing-box 配置失败: {e}"))
}

fn random_hex_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{:06x}", nanos & 0xFF_FFFF)
}

fn run_hidden_program(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    run_hidden_program(program)
}

fn flush_dns() {
    let _ = hidden_command("ipconfig").arg("/flushdns").status();
}

fn ensure_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err(format!("文件不存在: {}", path.display()))
    }
}

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

    paths.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
    paths.dedup();
    paths
}

fn detect_work_dir() -> PathBuf {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Some(dir) = find_work_dir_from(&current_dir) {
        return dir;
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(dir) = find_work_dir_from(dir) {
                return dir;
            }
        }
    }

    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        let app_dir = PathBuf::from(user_profile)
            .join("Apps")
            .join("sing-box-with-xray");
        if app_dir.exists() {
            return app_dir;
        }
    }

    current_dir
}

fn find_work_dir_from(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|dir| {
        if dir.join("sing-box.json").exists() || dir.join("Restart.ps1").exists() {
            Some(dir.to_path_buf())
        } else {
            None
        }
    })
}

fn work_dir() -> Result<PathBuf, String> {
    app_state()
        .map(|app| app.work_dir.clone())
        .ok_or_else(|| "应用状态不可用".to_string())
}

fn app_state() -> Option<std::sync::MutexGuard<'static, AppState>> {
    APP.get()?.lock().ok()
}

fn app_state_mut() -> Option<std::sync::MutexGuard<'static, AppState>> {
    app_state()
}

fn show_error(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title = wide(title);
        let message = wide(message);
        MessageBoxW(hwnd, message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

fn set_wstr_array<const N: usize>(target: &mut [u16; N], value: &str) {
    let wide = wide(value);
    let len = wide.len().saturating_sub(1).min(N - 1);
    target[..len].copy_from_slice(&wide[..len]);
    target[len] = 0;
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
