mod autostart;
mod balance;
mod diagnostics;
mod relay;
mod usage;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::Manager;
use tauri::WindowEvent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(usage::UsageStore::default())
        .manage(balance::BalanceStore::default())
        .manage(relay::RelayUsageStore::default())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = show_details_window(app.clone());
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            diagnostics::record(app.handle(), "lifecycle", "started");
            let autostart_enabled = if cfg!(debug_assertions) {
                false
            } else {
                autostart::initialize().unwrap_or_else(|error| {
                    eprintln!("无法启用开机自启: {error}");
                    true
                })
            };

            let show = MenuItem::with_id(app, "show", "打开统计面板", true, None::<&str>)?;
            let autostart = CheckMenuItem::with_id(
                app,
                "autostart",
                "开机启动",
                true,
                autostart_enabled,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &autostart, &quit])?;

            #[allow(unused_mut)]
            let mut tray_builder = TrayIconBuilder::with_id("token-tray")
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .ok_or_else(|| "找不到应用图标资源".to_string())?,
                )
                .tooltip("Token Tray")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "show" => {
                        let _ = show_details_window(app.clone());
                    }
                    "autostart" => {
                        let enabled = autostart.is_checked().unwrap_or(autostart_enabled);
                        if let Err(error) = autostart::set_enabled(enabled) {
                            eprintln!("无法更新开机自启: {error}");
                            let _ = autostart.set_checked(!enabled);
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });

            {
                tray_builder = tray_builder.on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = toggle_details_window(tray.app_handle().clone());
                    }
                });
            }

            #[cfg(target_os = "macos")]
            {
                tray_builder = tray_builder.title("0");
            }
            let tray = tray_builder.build(app)?;
            let store = app.state::<usage::UsageStore>().inner().clone();
            usage::start_sync_worker(app.handle().clone(), tray.clone(), store);

            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                window.set_decorations(false)?;
                window.set_always_on_top(true)?;
                window.set_skip_taskbar(true)?;
                window.set_resizable(false)?;
                position_taskbar_window(&window).map_err(std::io::Error::other)?;
                window.show()?;

                let recovery_window = window.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if position_taskbar_window(&recovery_window).is_ok() {
                        let _ = recovery_window.show();
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(false) if window.label() == "details" => {
                let cursor_is_over_taskbar = window
                    .app_handle()
                    .get_webview_window("main")
                    .map(|main| cursor_over_window(&main).unwrap_or(false))
                    .unwrap_or(false);
                if !cursor_is_over_taskbar {
                    let _ = window.hide();
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            balance::get_balance,
            balance::open_balance_config,
            relay::get_relay_usage,
            relay::open_relay_config,
            usage::get_usage_snapshot,
            usage::sync_usage_now,
            show_details_window,
            toggle_details_window,
            hide_details_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
fn show_details_window(app: tauri::AppHandle) -> Result<(), String> {
    let details = app
        .get_webview_window("details")
        .ok_or_else(|| "找不到详情窗口".to_string())?;

    if let Some(main) = app.get_webview_window("main") {
        if main.is_visible().unwrap_or(false) {
            position_details_window(&main, &details)?;
        } else {
            details.center().map_err(|error| error.to_string())?;
        }
    } else {
        details.center().map_err(|error| error.to_string())?;
    }

    details.show().map_err(|error| error.to_string())?;
    details.set_focus().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn toggle_details_window(app: tauri::AppHandle) -> Result<(), String> {
    let details = app
        .get_webview_window("details")
        .ok_or_else(|| "找不到详情窗口".to_string())?;

    if details.is_visible().unwrap_or(false) {
        details.hide().map_err(|error| error.to_string())
    } else {
        show_details_window(app)
    }
}

#[tauri::command]
fn hide_details_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(details) = app.get_webview_window("details") {
        details.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn position_details_window(
    anchor: &tauri::WebviewWindow,
    details: &tauri::WebviewWindow,
) -> Result<(), String> {
    let anchor_position = anchor.outer_position().map_err(|error| error.to_string())?;
    let anchor_size = anchor.outer_size().map_err(|error| error.to_string())?;
    let details_size = details.outer_size().map_err(|error| error.to_string())?;
    let width = details_size.width.max(1) as i32;
    let height = details_size.height.max(1) as i32;
    let anchor_width = anchor_size.width as i32;
    let anchor_height = anchor_size.height as i32;
    let mut x = anchor_position.x + (anchor_width - width) / 2;
    let mut y = anchor_position.y - height - 10;

    if let Some(monitor) = anchor
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| details.current_monitor().ok().flatten())
    {
        let work_area = monitor.work_area();
        let left = work_area.position.x;
        let top = work_area.position.y;
        let right = left.saturating_add(work_area.size.width as i32);
        let bottom = top.saturating_add(work_area.size.height as i32);
        let max_x = (right - width).max(left);
        let max_y = (bottom - height).max(top);

        if y < top {
            y = anchor_position.y + anchor_height + 10;
        }
        x = x.max(left).min(max_x);
        y = y.max(top).min(max_y);
    }

    details
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn cursor_over_window(window: &tauri::WebviewWindow) -> Result<bool, String> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let mut cursor = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return Err("无法读取鼠标位置".to_string());
    }

    let right = position.x.saturating_add(size.width as i32);
    let bottom = position.y.saturating_add(size.height as i32);
    Ok(cursor.x >= position.x && cursor.x < right && cursor.y >= position.y && cursor.y < bottom)
}

#[cfg(not(target_os = "windows"))]
fn cursor_over_window(_window: &tauri::WebviewWindow) -> Result<bool, String> {
    Ok(false)
}

#[cfg(target_os = "windows")]
fn clamp_position(value: i32, min: i32, max: i32) -> i32 {
    if min <= max {
        value.clamp(min, max)
    } else {
        min.saturating_add(max.saturating_sub(min) / 2)
    }
}

#[cfg(target_os = "windows")]
fn position_taskbar_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, DeleteObject, SetWindowRgn};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, FindWindowW, GetParent, GetWindowRect, SetParent, SetWindowLongPtrW,
        SetWindowPos, GWL_EXSTYLE, GWL_STYLE, HWND_TOP, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, WS_CHILD, WS_EX_NOACTIVATE, WS_VISIBLE,
    };

    const BASE_WIDTH: i32 = 140;
    const BASE_HEIGHT: i32 = 38;
    const BASE_DPI: u32 = 96;
    const GAP: i32 = 8;

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let taskbar_class = wide("Shell_TrayWnd");
    let tray_class = wide("TrayNotifyWnd");
    let taskbar = unsafe { FindWindowW(taskbar_class.as_ptr(), std::ptr::null()) };
    if taskbar.is_null() {
        return Err("找不到 Windows 任务栏".to_string());
    }

    let tray = unsafe {
        FindWindowExW(
            taskbar,
            std::ptr::null_mut(),
            tray_class.as_ptr(),
            std::ptr::null(),
        )
    };
    let mut taskbar_rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let mut tray_rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(taskbar, &mut taskbar_rect) } == 0 {
        return Err("无法读取 Windows 任务栏位置".to_string());
    }
    let tray_left = if !tray.is_null() && unsafe { GetWindowRect(tray, &mut tray_rect) } != 0 {
        tray_rect.left
    } else {
        taskbar_rect.right - 8
    };

    let taskbar_width = taskbar_rect.right - taskbar_rect.left;
    let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
    let app_hwnd = window.hwnd().map_err(|error| error.to_string())?.0;
    let dpi = unsafe { GetDpiForWindow(app_hwnd) }.max(BASE_DPI);
    let width = ((BASE_WIDTH as u32 * dpi + BASE_DPI / 2) / BASE_DPI) as i32;
    let height = ((BASE_HEIGHT as u32 * dpi + BASE_DPI / 2) / BASE_DPI) as i32;
    let horizontal_padding = ((taskbar_width - width).max(0) / 2).min(4);
    let vertical_padding = ((taskbar_height - height).max(0) / 2).min(4);
    let child_style = WS_CHILD | WS_VISIBLE;
    let child_ex_style = WS_EX_NOACTIVATE;

    if unsafe { GetParent(app_hwnd) } != taskbar {
        if unsafe { SetParent(app_hwnd, taskbar) }.is_null() {
            return Err("无法将统计卡片挂载到 Windows 任务栏".to_string());
        }
    }

    let horizontal = taskbar_width >= taskbar_height;
    let (x, y) = if horizontal {
        (
            clamp_position(
                tray_left - width - GAP,
                taskbar_rect.left + horizontal_padding,
                taskbar_rect.right - width - horizontal_padding,
            ),
            clamp_position(
                taskbar_rect.top + ((taskbar_height - height) / 2).max(0),
                taskbar_rect.top + vertical_padding,
                taskbar_rect.bottom - height - vertical_padding,
            ),
        )
    } else {
        (
            clamp_position(
                taskbar_rect.left + ((taskbar_width - width) / 2).max(0),
                taskbar_rect.left + horizontal_padding,
                taskbar_rect.right - width - horizontal_padding,
            ),
            clamp_position(
                if !tray.is_null() {
                    tray_rect.top - height - GAP
                } else {
                    taskbar_rect.bottom - height - GAP
                },
                taskbar_rect.top + vertical_padding,
                taskbar_rect.bottom - height - vertical_padding,
            ),
        )
    };

    let corner_radius = ((10 * dpi + BASE_DPI / 2) / BASE_DPI) as i32;
    let region = unsafe {
        CreateRoundRectRgn(
            0,
            0,
            width + 1,
            height + 1,
            corner_radius * 2,
            corner_radius * 2,
        )
    };
    if region.is_null() {
        return Err("无法创建任务栏圆角区域".to_string());
    }
    if unsafe { SetWindowRgn(app_hwnd, region, 1) } == 0 {
        unsafe { DeleteObject(region as _) };
        return Err("无法设置任务栏圆角区域".to_string());
    }

    unsafe {
        SetWindowLongPtrW(app_hwnd, GWL_STYLE, child_style as isize);
        SetWindowLongPtrW(app_hwnd, GWL_EXSTYLE, child_ex_style as isize);
        if SetWindowPos(
            app_hwnd,
            HWND_TOP,
            x - taskbar_rect.left,
            y - taskbar_rect.top,
            width,
            height,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        ) == 0
        {
            return Err("无法设置任务栏卡片位置".to_string());
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::clamp_position;

    #[test]
    fn handles_taskbar_range_smaller_than_widget() {
        assert_eq!(clamp_position(1041, 1044, 1038), 1041);
    }

    #[test]
    fn clamps_normal_taskbar_range() {
        assert_eq!(clamp_position(5, 10, 20), 10);
        assert_eq!(clamp_position(25, 10, 20), 20);
    }
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn position_taskbar_window(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}
