//! plum_shext — Explorer shell extension for the NEW Windows 11 context menu.
//!
//! Implements IExplorerCommand (loaded in-process by Explorer).
//! Exposes top-level "Compress to" (with hover submenu for instant format compression)
//! and "Browse with Plum".

use std::sync::Mutex;

use windows::core::{implement, BOOL, GUID, IUnknown, Interface, PWSTR};
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL, E_POINTER, HINSTANCE, HMODULE, S_FALSE, S_OK};
use windows::Win32::System::Com::{CoTaskMemAlloc, CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl,
    IShellItemArray, ECF_HASSUBCOMMANDS, ECF_SEPARATORBEFORE, ECS_ENABLED, SIGDN_FILESYSPATH,
};

const COMPRESS_CLSID: GUID = GUID::from_u128(0x7c1e9a62_5b3d_4f8c_9e21_a64d0f1e8b31);
const BROWSE_CLSID: GUID = GUID::from_u128(0x4f2d8b15_7c6a_49e3_8d24_9b5c0a1e2f63);
const WIN11_COMPRESS_CLSID: GUID = GUID::from_u128(0x7ae6900f_6eb0_44a2_9ca1_db2f7ef352af);

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

static DLL_DIR: Mutex<Option<String>> = Mutex::new(None);

fn is_russian_ui() -> bool {
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        (GetUserDefaultUILanguage() & 0x3ff) == 0x19
    }
    #[cfg(not(windows))]
    false
}

fn target_stem_from_array(arr: &IShellItemArray) -> Option<String> {
    let count = unsafe { arr.GetCount().unwrap_or(0) };
    if count == 0 {
        return None;
    }
    if count == 1 {
        if let Ok(item) = unsafe { arr.GetItemAt(0) } {
            if let Ok(pw) = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) } {
                let path_str = unsafe { pw_to_string(pw) };
                unsafe { CoTaskMemFree(Some(pw.as_ptr().cast())) };
                let p = std::path::Path::new(&path_str);
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if !stem.is_empty() {
                    return Some(stem);
                }
            }
        }
    } else if let Ok(item) = unsafe { arr.GetItemAt(0) } {
        if let Ok(pw) = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) } {
            let path_str = unsafe { pw_to_string(pw) };
            unsafe { CoTaskMemFree(Some(pw.as_ptr().cast())) };
            let p = std::path::Path::new(&path_str);
            if let Some(parent) = p.parent() {
                if let Some(name) = parent.file_name() {
                    let s = name.to_string_lossy().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    Some("archive".to_string())
}

#[implement(IExplorerCommand)]
struct Cmd {
    title: &'static str,
    mode: &'static str, // "--compress" or ""
    format: Option<&'static str>, // Some("zip"), Some("7z"), etc.
    is_parent: bool,
    separator_before: bool,
}

impl IExplorerCommand_Impl for Cmd_Impl {
    fn GetTitle(
        &self,
        psiitemarray: windows::core::Ref<'_, IShellItemArray>,
    ) -> windows::core::Result<PWSTR> {
        if let Some(fmt) = self.format {
            let ext = match fmt {
                "zip" => ".zip",
                "7z" => ".7z",
                "tar.zst" => ".tar.zst",
                "tar.gz" => ".tar.gz",
                "tar.xz" => ".tar.xz",
                _ => ".zip",
            };
            if let Ok(arr) = psiitemarray.ok() {
                if let Some(stem) = target_stem_from_array(&arr) {
                    let display_stem = if stem.chars().count() > 30 {
                        let s: String = stem.chars().take(28).collect();
                        format!("{s}…")
                    } else {
                        stem
                    };
                    return alloc_wide(&format!("{display_stem}{ext}"));
                }
            }
            alloc_wide(&format!("*{ext}"))
        } else {
            alloc_wide(self.title)
        }
    }

    fn GetIcon(
        &self,
        _psiitemarray: windows::core::Ref<'_, IShellItemArray>,
    ) -> windows::core::Result<PWSTR> {
        if self.is_parent || (self.format.is_none() && self.mode.is_empty()) {
            let dir = dll_dir();
            let icon = format!("{dir}plum.exe");
            alloc_wide(&icon)
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn GetState(
        &self,
        psiitemarray: windows::core::Ref<'_, IShellItemArray>,
        _fpin: BOOL,
    ) -> windows::core::Result<u32> {
        if let Ok(arr) = psiitemarray.ok() {
            let count = unsafe { arr.GetCount().unwrap_or(0) };
            if count > 0 {
                return Ok(ECS_ENABLED.0 as u32);
            }
        }
        Ok(ECS_ENABLED.0 as u32)
    }

    fn GetToolTip(
        &self,
        _psiitemarray: windows::core::Ref<'_, IShellItemArray>,
    ) -> windows::core::Result<PWSTR> {
        alloc_wide(self.title)
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Err(E_NOTIMPL.into())
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        if !self.is_parent {
            return Err(E_NOTIMPL.into());
        }

        let custom_title = if is_russian_ui() {
            "Дополнительные параметры…"
        } else {
            "Custom options…"
        };

        let sub_cmds: Vec<IExplorerCommand> = vec![
            Cmd {
                title: "ZIP",
                mode: "--compress",
                format: Some("zip"),
                is_parent: false,
                separator_before: false,
            }
            .into(),
            Cmd {
                title: "7z",
                mode: "--compress",
                format: Some("7z"),
                is_parent: false,
                separator_before: false,
            }
            .into(),
            Cmd {
                title: "tar.zst",
                mode: "--compress",
                format: Some("tar.zst"),
                is_parent: false,
                separator_before: false,
            }
            .into(),
            Cmd {
                title: "tar.gz",
                mode: "--compress",
                format: Some("tar.gz"),
                is_parent: false,
                separator_before: false,
            }
            .into(),
            Cmd {
                title: "tar.xz",
                mode: "--compress",
                format: Some("tar.xz"),
                is_parent: false,
                separator_before: false,
            }
            .into(),
            Cmd {
                title: custom_title,
                mode: "--compress",
                format: None,
                is_parent: false,
                separator_before: true,
            }
            .into(),
        ];

        let enumerator = EnumCommands {
            commands: sub_cmds,
            index: Mutex::new(0),
        };
        Ok(enumerator.into())
    }

    fn Invoke(
        &self,
        psiitemarray: windows::core::Ref<'_, IShellItemArray>,
        _pbc: windows::core::Ref<'_, IBindCtx>,
    ) -> windows::core::Result<()> {
        let arr = psiitemarray.ok()?;
        let count = unsafe { arr.GetCount().unwrap_or(0) };
        if count == 0 {
            return Err(E_FAIL.into());
        }

        let dir = dll_dir();
        let exe = format!("{dir}plum.exe");
        let mut cmd = std::process::Command::new(&exe);
        if !self.mode.is_empty() {
            cmd.arg(self.mode);
        }

        for i in 0..count {
            if let Ok(item) = unsafe { arr.GetItemAt(i) } {
                if let Ok(pw) = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) } {
                    let path = unsafe { pw_to_string(pw) };
                    unsafe { CoTaskMemFree(Some(pw.as_ptr().cast())) };
                    cmd.arg(&path);
                }
            }
        }

        if let Some(fmt) = self.format {
            cmd.arg("--format");
            cmd.arg(fmt);
            cmd.arg("--auto-start");
            cmd.arg("--auto-close");
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NEW_CONSOLE);
        }
        cmd.spawn().map_err(|e| {
            windows::core::Error::from_hresult(windows::core::HRESULT(
                e.raw_os_error().unwrap_or(5) as i32,
            ))
        })?;
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        let mut flags = 0u32;
        if self.is_parent {
            flags |= ECF_HASSUBCOMMANDS.0 as u32;
        }
        if self.separator_before {
            flags |= ECF_SEPARATORBEFORE.0 as u32;
        }
        Ok(flags)
    }
}

#[implement(IEnumExplorerCommand)]
struct EnumCommands {
    commands: Vec<IExplorerCommand>,
    index: Mutex<usize>,
}

impl IEnumExplorerCommand_Impl for EnumCommands_Impl {
    fn Next(
        &self,
        celt: u32,
        puicommand: *mut Option<IExplorerCommand>,
        pceltfetched: *mut u32,
    ) -> windows::core::HRESULT {
        if puicommand.is_null() || (celt > 1 && pceltfetched.is_null()) {
            return E_POINTER;
        }
        let mut idx = match self.index.lock() {
            Ok(g) => g,
            Err(_) => return E_FAIL,
        };
        let mut fetched = 0u32;
        while fetched < celt && *idx < self.commands.len() {
            unsafe {
                *puicommand.add(fetched as usize) = Some(self.commands[*idx].clone());
            }
            *idx += 1;
            fetched += 1;
        }
        if !pceltfetched.is_null() {
            unsafe {
                *pceltfetched = fetched;
            }
        }
        if fetched == celt {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> windows::core::Result<()> {
        if let Ok(mut idx) = self.index.lock() {
            *idx = (*idx + celt as usize).min(self.commands.len());
        }
        Ok(())
    }

    fn Reset(&self) -> windows::core::Result<()> {
        if let Ok(mut idx) = self.index.lock() {
            *idx = 0;
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumExplorerCommand> {
        let idx = self.index.lock().map(|i| *i).unwrap_or(0);
        let cloned = EnumCommands {
            commands: self.commands.clone(),
            index: Mutex::new(idx),
        };
        Ok(cloned.into())
    }
}

fn dll_dir() -> String {
    DLL_DIR
        .lock()
        .map(|g| g.clone().unwrap_or_default())
        .unwrap_or_default()
}

unsafe fn pw_to_string(pw: PWSTR) -> String {
    let mut len = 0usize;
    while *pw.as_ptr().add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(pw.as_ptr(), len);
    String::from_utf16_lossy(slice)
}

fn alloc_wide(s: &str) -> windows::core::Result<PWSTR> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * 2;
    unsafe {
        let p = CoTaskMemAlloc(bytes) as *mut u16;
        if p.is_null() {
            return Err(E_FAIL.into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        Ok(PWSTR::from_raw(p))
    }
}

#[implement(IClassFactory)]
struct Factory {
    clsid: GUID,
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        punkouter: windows::core::Ref<'_, IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if punkouter.ok().is_ok() {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8004_0100u32 as i32, // CLASS_E_NOAGGREGATION
            )));
        }
        let cmd: IExplorerCommand = if self.clsid == COMPRESS_CLSID || self.clsid == WIN11_COMPRESS_CLSID {
            let parent_title = if is_russian_ui() {
                "Сжать в"
            } else {
                "Compress to"
            };
            Cmd {
                title: parent_title,
                mode: "--compress",
                format: None,
                is_parent: true,
                separator_before: false,
            }
            .into()
        } else {
            Cmd {
                title: "Browse with Plum",
                mode: "",
                format: None,
                is_parent: false,
                separator_before: false,
            }
            .into()
        };
        unsafe { cmd.query(riid, ppv).ok()? };
        Ok(())
    }

    fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[no_mangle]
extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> windows::core::HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    let clsid = unsafe { *rclsid };
    if clsid != COMPRESS_CLSID && clsid != BROWSE_CLSID && clsid != WIN11_COMPRESS_CLSID {
        return windows::core::HRESULT(0x8004_0101u32 as i32); // CLASS_E_CLASSNOTAVAILABLE
    }
    let factory: IClassFactory = Factory { clsid }.into();
    unsafe { factory.query(riid, ppv) }
}

#[no_mangle]
extern "system" fn DllCanUnloadNow() -> windows::core::HRESULT {
    windows::core::HRESULT(1) // S_FALSE: stay loaded while Explorer runs
}

#[no_mangle]
extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut core::ffi::c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        let mut buf = [0u16; 1024];
        let n = unsafe { GetModuleFileNameW(Some(HMODULE(hinst.0)), &mut buf) };
        let path = String::from_utf16_lossy(&buf[..n as usize]);
        let dir = match path.rfind('\\') {
            Some(i) => path[..=i].to_string(),
            None => path.clone(),
        };
        if let Ok(mut g) = DLL_DIR.lock() {
            *g = Some(dir);
        }
    }
    BOOL(1)
}