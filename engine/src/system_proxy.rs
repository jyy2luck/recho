//! Explicit current-user proxy operations; the client journals ownership before applying.
#[cfg(windows)]
use crate::wire::{Error, Result};
#[cfg(windows)]
use serde_json::{Value, json};

#[cfg(windows)]
pub fn control(method: &str, input: Value) -> Result<Value> {
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{Foundation::GlobalFree, Networking::WinInet::*};
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = LOCK
        .lock()
        .map_err(|_| Error::bad("System proxy lock failed"))?;
    let mut options = [INTERNET_PER_CONN_OPTIONW::default(); 3];
    for (option, kind) in options.iter_mut().zip([
        INTERNET_PER_CONN_FLAGS,
        INTERNET_PER_CONN_PROXY_SERVER,
        INTERNET_PER_CONN_AUTOCONFIG_URL,
    ]) {
        option.dwOption = kind;
    }
    let mut list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        dwOptionCount: 3,
        pOptions: options.as_mut_ptr(),
        ..Default::default()
    };
    let mut length = list.dwSize;
    if unsafe {
        InternetQueryOptionW(
            ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            (&mut list as *mut INTERNET_PER_CONN_OPTION_LISTW).cast(),
            &mut length,
        )
    } == 0
    {
        return Err(Error::bad(std::io::Error::last_os_error()));
    }
    unsafe fn take_string(value: *mut u16) -> String {
        if value.is_null() {
            return String::new();
        }
        let mut length = 0;
        unsafe {
            while length < 32768 && *value.add(length) != 0 {
                length += 1;
            }
        }
        let result = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, length) });
        unsafe {
            GlobalFree(value.cast());
        }
        result
    }
    let current = unsafe {
        json!({"flags":options[0].Value.dwValue,"server":take_string(options[1].Value.pszValue),"pac":take_string(options[2].Value.pszValue)})
    };
    if method == "POST" {
        let changes = input["changes"]
            .as_array()
            .ok_or_else(|| Error::bad("Proxy changes required"))?;
        if changes.len() != 1 || changes[0]["id"] != "default" || changes[0]["expected"] != current
        {
            return Err(Error::new(
                409,
                "System proxy changed; refresh before applying",
            ));
        }
        let desired = &changes[0]["desired"];
        if desired.as_object().is_none_or(|v| v.len() != 3)
            || desired["flags"].as_u64().is_none_or(|v| v > 15)
            || !desired["server"].is_string()
            || !desired["pac"].is_string()
        {
            return Err(Error::bad("Invalid proxy snapshot"));
        }
        let wide = |key: &str| -> Result<Vec<u16>> {
            let text = desired[key].as_str().unwrap();
            if text.len() > 32768 || text.contains('\0') {
                return Err(Error::bad("Invalid proxy string"));
            }
            Ok(text.encode_utf16().chain(Some(0)).collect())
        };
        let mut server = wide("server")?;
        let mut pac = wide("pac")?;
        options[0].Value.dwValue = desired["flags"].as_u64().unwrap() as u32;
        options[1].Value.pszValue = server.as_mut_ptr();
        options[2].Value.pszValue = pac.as_mut_ptr();
        list.pOptions = options.as_mut_ptr();
        if unsafe {
            InternetSetOptionW(
                ptr::null(),
                INTERNET_OPTION_PER_CONNECTION_OPTION,
                (&list as *const INTERNET_PER_CONN_OPTION_LISTW).cast(),
                list.dwSize,
            )
        } == 0
        {
            return Err(Error::bad(std::io::Error::last_os_error()));
        }
        unsafe {
            InternetSetOptionW(
                ptr::null(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                ptr::null(),
                0,
            );
            InternetSetOptionW(ptr::null(), INTERNET_OPTION_REFRESH, ptr::null(), 0);
        }
        return Ok(json!({"ok":true}));
    }
    Ok(json!({"services":[{"id":"default","name":"Windows 当前用户","values":current}]}))
}
