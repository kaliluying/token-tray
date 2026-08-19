#[cfg(target_os = "windows")]
mod windows {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
        HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const CONFIG_KEY: &str = "Software\\Token Tray";
    const RUN_VALUE: &str = "Token Tray";
    const ENABLED_VALUE: &str = "AutostartEnabled";
    const ERROR_FILE_NOT_FOUND: u32 = 2;

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open_or_create(path: &str) -> Result<*mut core::ffi::c_void, String> {
        let path = wide(path);
        let mut key = std::ptr::null_mut();
        let result = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
        if result != 0 {
            return Err(format!("打开注册表项失败: {result}"));
        }
        Ok(key)
    }

    fn set_enabled_preference(enabled: bool) -> Result<(), String> {
        let key = open_or_create(CONFIG_KEY)?;
        let value_name = wide(ENABLED_VALUE);
        let value = if enabled { 1u32 } else { 0u32 };
        let result = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_DWORD,
                (&value as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        };
        unsafe {
            RegCloseKey(key);
        }
        if result != 0 {
            return Err(format!("保存开机自启设置失败: {result}"));
        }
        Ok(())
    }

    fn read_enabled_preference() -> Result<Option<bool>, String> {
        let key = open_or_create(CONFIG_KEY)?;
        let value_name = wide(ENABLED_VALUE);
        let mut value_type = 0;
        let mut value = 0u32;
        let mut value_size = std::mem::size_of::<u32>() as u32;
        let result = unsafe {
            RegQueryValueExW(
                key,
                value_name.as_ptr(),
                std::ptr::null(),
                &mut value_type,
                (&mut value as *mut u32).cast(),
                &mut value_size,
            )
        };
        unsafe {
            RegCloseKey(key);
        }
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if result != 0 {
            return Err(format!("读取开机自启设置失败: {result}"));
        }
        Ok(Some(value != 0))
    }

    fn write_run_value() -> Result<(), String> {
        let key = open_or_create(RUN_KEY)?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let command = format!("\"{}\"", executable.display());
        let value_name = wide(RUN_VALUE);
        let value_data = wide(&command);
        let result = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                value_data.as_ptr().cast(),
                (value_data.len() * std::mem::size_of::<u16>()) as u32,
            )
        };
        unsafe {
            RegCloseKey(key);
        }
        if result != 0 {
            return Err(format!("写入开机自启命令失败: {result}"));
        }
        Ok(())
    }

    fn remove_run_value() -> Result<(), String> {
        let key = open_or_create(RUN_KEY)?;
        let value_name = wide(RUN_VALUE);
        let result = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
        unsafe {
            RegCloseKey(key);
        }
        if result != 0 && result != ERROR_FILE_NOT_FOUND {
            return Err(format!("移除开机自启命令失败: {result}"));
        }
        Ok(())
    }

    pub fn initialize() -> Result<bool, String> {
        let enabled = match read_enabled_preference()? {
            Some(enabled) => enabled,
            None => {
                write_run_value()?;
                set_enabled_preference(true)?;
                true
            }
        };
        if enabled {
            write_run_value()?;
        } else {
            remove_run_value()?;
        }
        Ok(enabled)
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        if enabled {
            write_run_value()?;
        } else {
            remove_run_value()?;
        }
        set_enabled_preference(enabled)
    }
}

#[cfg(target_os = "windows")]
pub use windows::{initialize, set_enabled};

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;
    use std::process::Command;

    const LABEL: &str = "com.token-tray.app";

    fn plist_path() -> Result<PathBuf, String> {
        let home = std::env::var_os("HOME").ok_or_else(|| "找不到当前用户目录".to_string())?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    fn uid() -> Result<String, String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err("无法读取当前用户 ID".to_string());
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_string())
            .map_err(|error| error.to_string())
    }

    fn launchctl(args: &[String]) {
        let _ = Command::new("launchctl").args(args).status();
    }

    fn write_plist() -> Result<PathBuf, String> {
        let path = plist_path()?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let parent = path
            .parent()
            .ok_or_else(|| "无法确定自启配置目录".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let escaped_executable = executable
            .display()
            .to_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array><string>{escaped_executable}</string></array>\n<key>RunAtLoad</key><true/>\n</dict>\n</plist>\n"
        );
        std::fs::write(&path, plist).map_err(|error| error.to_string())?;
        Ok(path)
    }

    pub fn initialize() -> Result<bool, String> {
        let path = write_plist()?;
        let user = uid()?;
        launchctl(&[
            "bootstrap".to_string(),
            format!("gui/{user}"),
            path.display().to_string(),
        ]);
        Ok(true)
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let path = plist_path()?;
        let user = uid()?;
        let domain = format!("gui/{user}/{LABEL}");
        launchctl(&["bootout".to_string(), domain]);
        if enabled {
            let path = write_plist()?;
            launchctl(&[
                "bootstrap".to_string(),
                format!("gui/{user}"),
                path.display().to_string(),
            ]);
        } else if path.is_file() {
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub use macos::{initialize, set_enabled};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn initialize() -> Result<bool, String> {
    Ok(false)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn set_enabled(_enabled: bool) -> Result<(), String> {
    Ok(())
}
