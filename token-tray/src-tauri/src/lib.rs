mod autostart;
mod balance;
#[cfg(target_os = "macos")]
mod details_panel;
mod diagnostics;
mod relay;
mod usage;

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::WindowEvent;
use tauri::{Emitter, Manager};

const DETAILS_ANIMATION_DURATION_MS: u64 = 180;
const DETAILS_FOCUS_LOSS_GRACE_PERIOD_MS: u64 = 240;

#[derive(Clone, Default)]
struct DetailsWindowState {
    generation: Arc<AtomicU64>,
}

impl DetailsWindowState {
    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .manage(usage::UsageStore::default())
        .manage(balance::BalanceStore::default())
        .manage(relay::RelayUsageStore::default())
        .manage(DetailsWindowState::default())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = show_details_window(app.clone());
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build());

    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .setup(|app| {
            diagnostics::record(app.handle(), "lifecycle", "started");
            let autostart_enabled = if cfg!(debug_assertions) {
                false
            } else {
                autostart::initialize().unwrap_or_else(|error| {
                    eprintln!("无法启用开机自启: {error}");
                    false
                })
            };

            #[cfg(target_os = "macos")]
            app.handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory)?;

            #[cfg(target_os = "macos")]
            if let Some(details) = app.get_webview_window("details") {
                details.set_visible_on_all_workspaces(true)?;
                details_panel::initialize(&details).map_err(std::io::Error::other)?;
            }

            #[cfg(target_os = "macos")]
            install_details_outside_click_monitor(app.handle().clone());

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

            #[cfg(not(target_os = "macos"))]
            {
                tray_builder = tray_builder.menu(&menu);
            }

            tray_builder = tray_builder.on_tray_icon_event(move |tray, event| {
                if let TrayIconEvent::Click {
                    button,
                    button_state,
                    ..
                } = event
                {
                    match (button, button_state) {
                        (MouseButton::Left, MouseButtonState::Up) => {
                            let _ = toggle_details_window(tray.app_handle().clone());
                        }
                        #[cfg(target_os = "macos")]
                        (MouseButton::Right, MouseButtonState::Down) => {
                            if let Some(details) = tray.app_handle().get_webview_window("details") {
                                if let Err(error) = details.popup_menu(&menu) {
                                    eprintln!("无法打开托盘菜单: {error}");
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });

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
                schedule_hide_after_focus_loss(window.app_handle().clone());
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
    let animation_state = app.state::<DetailsWindowState>().inner().clone();
    animation_state.next_generation();

    #[cfg(target_os = "macos")]
    details
        .set_visible_on_all_workspaces(true)
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    details_panel::initialize_if_needed(&details)?;

    #[cfg(target_os = "macos")]
    let positioned_from_tray = position_details_window_from_tray(&app, &details)?;
    #[cfg(not(target_os = "macos"))]
    let positioned_from_tray = false;

    if !positioned_from_tray {
        if let Some(main) = app.get_webview_window("main") {
            if main.is_visible().unwrap_or(false) {
                position_details_window(&main, &details)?;
            } else {
                details.center().map_err(|error| error.to_string())?;
            }
        } else {
            details.center().map_err(|error| error.to_string())?;
        }
    }

    #[cfg(target_os = "macos")]
    details_panel::show(&app, &details)?;
    #[cfg(not(target_os = "macos"))]
    details.show().map_err(|error| error.to_string())?;
    #[cfg(not(target_os = "macos"))]
    details.set_focus().map_err(|error| error.to_string())?;
    let app_for_opening_event = app.clone();
    details
        .run_on_main_thread(move || {
            let _ = app_for_opening_event.emit_to("details", "details-window-opening", ());
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_details_outside_click_monitor(app: tauri::AppHandle) {
    use block2::RcBlock;
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;

    let mask =
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    let handler = RcBlock::new(move |_event: NonNull<NSEvent>| {
        let Some(details) = app.get_webview_window("details") else {
            return;
        };
        if should_hide_details_after_external_click(details.is_visible().unwrap_or(false), false) {
            let _ = request_hide_details_window(app.clone());
        }
    });

    if let Some(monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler) {
        // The monitor lives for the lifetime of the accessory process. AppKit owns the
        // callback registration; intentionally keep the token alive until process exit.
        std::mem::forget(monitor);
    }
}

#[cfg(target_os = "macos")]
fn fullscreen_collection_behavior(
    current: objc2_app_kit::NSWindowCollectionBehavior,
) -> objc2_app_kit::NSWindowCollectionBehavior {
    current
        | objc2_app_kit::NSWindowCollectionBehavior::CanJoinAllSpaces
        | objc2_app_kit::NSWindowCollectionBehavior::CanJoinAllApplications
        | objc2_app_kit::NSWindowCollectionBehavior::Stationary
        | objc2_app_kit::NSWindowCollectionBehavior::FullScreenAuxiliary
}

#[cfg(target_os = "macos")]
fn position_details_window_from_tray(
    app: &tauri::AppHandle,
    details: &tauri::WebviewWindow,
) -> Result<bool, String> {
    let Some(tray) = app.tray_by_id("token-tray") else {
        return Ok(false);
    };
    let Some(tray_rect) = tray.rect().ok().flatten() else {
        return Ok(false);
    };

    let tray_position = tray_rect.position.to_physical::<i32>(1.0);
    let tray_size = tray_rect.size.to_physical::<u32>(1.0);
    let details_size = details.outer_size().map_err(|error| error.to_string())?;
    let monitor = app
        .monitor_from_point(
            f64::from(tray_position.x) + f64::from(tray_size.width) / 2.0,
            f64::from(tray_position.y) + f64::from(tray_size.height) / 2.0,
        )
        .ok()
        .flatten()
        .or_else(|| details.current_monitor().ok().flatten());
    let position = position_below_anchor(
        tray_position,
        tray_size,
        details_size,
        monitor.as_ref().map(|monitor| monitor.work_area()),
    );

    details
        .set_position(position)
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[tauri::command]
fn toggle_details_window(app: tauri::AppHandle) -> Result<(), String> {
    let details = app
        .get_webview_window("details")
        .ok_or_else(|| "找不到详情窗口".to_string())?;

    if details.is_visible().unwrap_or(false) {
        request_hide_details_window(app)
    } else {
        show_details_window(app)
    }
}

#[tauri::command]
fn hide_details_window(app: tauri::AppHandle) -> Result<(), String> {
    request_hide_details_window(app)
}

fn schedule_hide_after_focus_loss(app: tauri::AppHandle) {
    let animation_state = app.state::<DetailsWindowState>().inner().clone();
    let generation = animation_state.current_generation();
    let app_for_main_thread = app.clone();
    let app_for_hide = app.clone();

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(DETAILS_FOCUS_LOSS_GRACE_PERIOD_MS));
        let _ = app_for_main_thread.run_on_main_thread(move || {
            if !animation_state.is_current(generation) {
                return;
            }
            let Some(details) = app_for_hide.get_webview_window("details") else {
                return;
            };
            let details_visible = details.is_visible().unwrap_or(false);
            let details_focused = details.is_focused().unwrap_or(false);
            let cursor_is_over_taskbar = app_for_hide
                .get_webview_window("main")
                .map(|main| cursor_over_window(&main).unwrap_or(false))
                .unwrap_or(false);

            if should_hide_details_after_focus_loss(
                details_visible,
                details_focused,
                cursor_is_over_taskbar,
            ) {
                let _ = request_hide_details_window(app_for_hide.clone());
            }
        });
    });
}

fn request_hide_details_window(app: tauri::AppHandle) -> Result<(), String> {
    let Some(details) = app.get_webview_window("details") else {
        return Ok(());
    };
    if !details.is_visible().unwrap_or(false) {
        return Ok(());
    }

    let animation_state = app.state::<DetailsWindowState>().inner().clone();
    let generation = animation_state.next_generation();
    let _ = app.emit_to("details", "details-window-closing", ());

    #[cfg(target_os = "macos")]
    details_panel::begin_close(&app, &details)?;

    let app_for_main_thread = app.clone();
    let app_for_lookup = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(DETAILS_ANIMATION_DURATION_MS));
        let _ = app_for_main_thread.run_on_main_thread(move || {
            if !animation_state.is_current(generation) {
                return;
            }
            if let Some(details) = app_for_lookup.get_webview_window("details") {
                #[cfg(target_os = "macos")]
                {
                    let _ = details_panel::hide(&app_for_lookup, &details);
                }
                #[cfg(not(target_os = "macos"))]
                let _ = details.hide();
            }
        });
    });

    Ok(())
}

fn should_hide_details_after_focus_loss(
    details_visible: bool,
    details_focused: bool,
    cursor_is_over_taskbar: bool,
) -> bool {
    details_visible && !details_focused && !cursor_is_over_taskbar
}

fn should_hide_details_after_external_click(
    details_visible: bool,
    clicked_inside_details: bool,
) -> bool {
    details_visible && !clicked_inside_details
}

fn position_below_anchor(
    anchor_position: tauri::PhysicalPosition<i32>,
    anchor_size: tauri::PhysicalSize<u32>,
    details_size: tauri::PhysicalSize<u32>,
    work_area: Option<&tauri::PhysicalRect<i32, u32>>,
) -> tauri::PhysicalPosition<i32> {
    const GAP: i32 = 10;

    let width = details_size.width.max(1) as i32;
    let height = details_size.height.max(1) as i32;
    let anchor_width = anchor_size.width as i32;
    let anchor_height = anchor_size.height as i32;
    let mut x = anchor_position.x + (anchor_width - width) / 2;
    let mut y = anchor_position.y + anchor_height + GAP;

    if let Some(work_area) = work_area {
        let left = work_area.position.x;
        let top = work_area.position.y;
        let right = left.saturating_add(work_area.size.width as i32);
        let bottom = top.saturating_add(work_area.size.height as i32);
        let max_x = (right - width).max(left);
        let max_y = (bottom - height).max(top);
        x = x.max(left).min(max_x);
        y = y.max(top).min(max_y);
    }

    tauri::PhysicalPosition::new(x, y)
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

#[cfg(test)]
mod details_position_tests {
    use super::{
        position_below_anchor, should_hide_details_after_external_click,
        should_hide_details_after_focus_loss,
    };

    #[test]
    fn positions_details_below_tray_and_inside_work_area() {
        let position = position_below_anchor(
            tauri::PhysicalPosition::new(100, 0),
            tauri::PhysicalSize::new(40, 24),
            tauri::PhysicalSize::new(380, 480),
            Some(&tauri::PhysicalRect {
                position: tauri::PhysicalPosition::new(0, 0),
                size: tauri::PhysicalSize::new(1440, 900),
            }),
        );

        assert_eq!(position, tauri::PhysicalPosition::new(0, 34));
    }

    #[test]
    fn does_not_hide_details_during_focus_recovery_or_tray_interaction() {
        assert!(!should_hide_details_after_focus_loss(true, true, false));
        assert!(!should_hide_details_after_focus_loss(true, false, true));
        assert!(should_hide_details_after_focus_loss(true, false, false));
    }

    #[test]
    fn hides_details_when_a_visible_panel_receives_an_external_click() {
        assert!(should_hide_details_after_external_click(true, false));
        assert!(!should_hide_details_after_external_click(true, true));
        assert!(!should_hide_details_after_external_click(false, false));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_window_behavior_tests {
    use super::fullscreen_collection_behavior;
    use objc2_app_kit::NSWindowCollectionBehavior;

    #[test]
    fn includes_fullscreen_auxiliary_behavior_without_dropping_existing_flags() {
        let existing = NSWindowCollectionBehavior::Stationary;
        let behavior = fullscreen_collection_behavior(existing);

        assert!(behavior.contains(NSWindowCollectionBehavior::CanJoinAllSpaces));
        assert!(behavior.contains(NSWindowCollectionBehavior::CanJoinAllApplications));
        assert!(behavior.contains(NSWindowCollectionBehavior::Stationary));
        assert!(behavior.contains(NSWindowCollectionBehavior::FullScreenAuxiliary));
        assert!(behavior.contains(NSWindowCollectionBehavior::Stationary));
    }
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn position_taskbar_window(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}
