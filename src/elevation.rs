//! Windows UAC elevation and administrator permission checks.

use std::path::{Path, PathBuf};

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    pub const TOKEN_QUERY: u32 = 0x0008;
    pub const TOKEN_ELEVATION: u32 = 20;

    #[repr(C)]
    pub struct TokenElevation {
        pub token_is_elevated: u32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn OpenProcessToken(
            process_handle: *mut c_void,
            desired_access: u32,
            token_handle: *mut *mut c_void,
        ) -> i32;

        pub fn GetTokenInformation(
            token_handle: *mut c_void,
            token_information_class: u32,
            token_information: *mut c_void,
            token_information_length: u32,
            return_length: *mut u32,
        ) -> i32;

        pub fn CloseHandle(handle: *mut c_void) -> i32;
    }

    pub const SEE_MASK_NOCLOSEPROCESS: u32 = 0x00000040;
    pub const INFINITE: u32 = 0xFFFFFFFF;

    #[repr(C)]
    pub struct ShellExecuteInfoW {
        pub cb_size: u32,
        pub f_mask: u32,
        pub hwnd: *mut c_void,
        pub lp_verb: *const u16,
        pub lp_file: *const u16,
        pub lp_parameters: *const u16,
        pub lp_directory: *const u16,
        pub n_show: i32,
        pub h_inst_app: *mut c_void,
        pub lp_id_list: *mut c_void,
        pub lp_class: *const u16,
        pub hkey_class: *mut c_void,
        pub dw_hot_key: u32,
        pub h_icon: *mut c_void,
        pub h_process: *mut c_void,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn WaitForSingleObject(h_handle: *mut c_void, dw_milliseconds: u32) -> u32;
        pub fn GetExitCodeProcess(h_process: *mut c_void, lp_exit_code: *mut u32) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        pub fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;

        pub fn ShellExecuteExW(p_exec_info: *mut ShellExecuteInfoW) -> i32;
    }
}

/// Returns true if the current process has elevated Administrator privileges.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    unsafe {
        use std::ffi::c_void;
        use std::ptr::null_mut;

        let mut token: *mut c_void = null_mut();
        if win32::OpenProcessToken(win32::GetCurrentProcess(), win32::TOKEN_QUERY, &mut token) == 0 {
            return false;
        }

        let mut elevation = win32::TokenElevation { token_is_elevated: 0 };
        let mut ret_len = 0u32;
        let ok = win32::GetTokenInformation(
            token,
            win32::TOKEN_ELEVATION,
            &mut elevation as *mut _ as *mut c_void,
            std::mem::size_of::<win32::TokenElevation>() as u32,
            &mut ret_len,
        );

        win32::CloseHandle(token);

        ok != 0 && elevation.token_is_elevated != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Relaunches plum.exe with elevated administrator privileges (UAC prompt).
pub fn relaunch_elevated(args: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

        let mut param_str = String::new();
        for arg in args {
            if !param_str.is_empty() {
                param_str.push(' ');
            }
            if arg.contains(' ') || arg.contains('\t') || arg.is_empty() {
                param_str.push('"');
                param_str.push_str(arg);
                param_str.push('"');
            } else {
                param_str.push_str(arg);
            }
        }
        let param_w: Vec<u16> = param_str.encode_utf16().chain(std::iter::once(0)).collect();
        let op_w: Vec<u16> = "runas\0".encode_utf16().collect();

        const SW_SHOWNORMAL: i32 = 1;
        let res = win32::ShellExecuteW(
            std::ptr::null_mut(),
            op_w.as_ptr(),
            exe_w.as_ptr(),
            param_w.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );

        if res > 32 {
            Ok(())
        } else {
            Err(format!("UAC elevation canceled or failed (code {res})"))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err("Elevation is only supported on Windows".into())
    }
}

/// Runs plum.exe with elevated administrator privileges (UAC prompt) and waits for completion.
pub fn run_elevated_wait(args: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null_mut;

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

        let mut param_str = String::new();
        for arg in args {
            if !param_str.is_empty() {
                param_str.push(' ');
            }
            if arg.contains(' ') || arg.contains('\t') || arg.is_empty() {
                param_str.push('"');
                param_str.push_str(arg);
                param_str.push('"');
            } else {
                param_str.push_str(arg);
            }
        }
        let param_w: Vec<u16> = param_str.encode_utf16().chain(std::iter::once(0)).collect();
        let op_w: Vec<u16> = "runas\0".encode_utf16().collect();

        const SW_HIDE: i32 = 0;
        let mut sei = win32::ShellExecuteInfoW {
            cb_size: std::mem::size_of::<win32::ShellExecuteInfoW>() as u32,
            f_mask: win32::SEE_MASK_NOCLOSEPROCESS,
            hwnd: null_mut(),
            lp_verb: op_w.as_ptr(),
            lp_file: exe_w.as_ptr(),
            lp_parameters: param_w.as_ptr(),
            lp_directory: std::ptr::null(),
            n_show: SW_HIDE,
            h_inst_app: null_mut(),
            lp_id_list: null_mut(),
            lp_class: std::ptr::null(),
            hkey_class: null_mut(),
            dw_hot_key: 0,
            h_icon: null_mut(),
            h_process: null_mut(),
        };

        let ok = win32::ShellExecuteExW(&mut sei);
        if ok == 0 || sei.h_process.is_null() {
            return Err("Отказано в разрешении администратора (UAC отклонён)".into());
        }

        win32::WaitForSingleObject(sei.h_process, win32::INFINITE);

        let mut exit_code = 0u32;
        win32::GetExitCodeProcess(sei.h_process, &mut exit_code);
        win32::CloseHandle(sei.h_process);

        if exit_code == 0 {
            Ok(())
        } else {
            Err(format!("Ошибка извлечения (код завершения: {exit_code})"))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err("Elevation is only supported on Windows".into())
    }
}

/// Returns true if the path is a UNC or network path (e.g. \\wsl$\..., \\server\share).
/// Windows UAC elevation cannot elevate network shares or WSL Plan9 mounts.
pub fn is_unc_or_network(path: &Path) -> bool {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\") || s.starts_with("//") {
        return true;
    }
    #[cfg(windows)]
    if is_remote_drive(path) {
        return true;
    }
    false
}

#[cfg(windows)]
fn is_remote_drive(path: &Path) -> bool {
    if let Some(s) = path.to_str() {
        if s.len() >= 2 && s.as_bytes()[1] == b':' {
            let root = format!("{}:\\\0", &s[..1]);
            let root_w: Vec<u16> = root.encode_utf16().collect();
            #[link(name = "kernel32")]
            extern "system" {
                fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
            }
            const DRIVE_REMOTE: u32 = 4;
            return unsafe { GetDriveTypeW(root_w.as_ptr()) == DRIVE_REMOTE };
        }
    }
    false
}

/// Tests whether reading the inputs or writing to out_path requires Administrator privileges.
pub fn check_permission_needed(inputs: &[PathBuf], out_path: Option<&Path>) -> bool {
    if is_elevated() {
        return false;
    }

    // 1. Check if output path is writable
    if let Some(out) = out_path {
        if !can_write(out) {
            return true;
        }
    }

    // 2. Check if inputs are readable (sample first 20 to avoid freezing on massive selections)
    for input in inputs.iter().take(20) {
        if !can_read(input) {
            return true;
        }
    }

    false
}

pub fn can_write(path: &Path) -> bool {
    if is_elevated() {
        return true;
    }
    if is_unc_or_network(path) {
        // Windows UAC cannot grant rights to network or WSL filesystems
        return true;
    }

    let mut cur = Some(path);
    while let Some(dir) = cur {
        if dir.exists() {
            if dir.is_dir() {
                let test_name = format!(".plum_perm_probe_{}.tmp", std::process::id());
                let probe = dir.join(test_name);
                return match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
                    Ok(f) => {
                        drop(f);
                        let _ = std::fs::remove_file(&probe);
                        true
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
                    Err(_) => true,
                };
            } else {
                return match std::fs::OpenOptions::new().write(true).open(dir) {
                    Ok(_) => true,
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
                    Err(_) => true,
                };
            }
        }
        cur = dir.parent();
    }
    true
}

fn can_read(input: &Path) -> bool {
    if is_unc_or_network(input) {
        return true;
    }
    if input.is_file() {
        match std::fs::File::open(input) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
            Err(_) => true,
        }
    } else if input.is_dir() {
        // Non-recursive: if a directory is protected by Windows ACLs, read_dir itself fails
        match std::fs::read_dir(input) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
            Err(_) => true,
        }
    } else {
        true
    }
}
