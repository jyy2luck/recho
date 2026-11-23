use super::{AUTO, ENABLED, HTTP, HTTPS, SOCKS};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use core_foundation::{
    base::{CFType, CFTypeRef, TCFType},
    boolean::CFBoolean,
    data::CFData,
    dictionary::{
        CFDictionary, CFDictionaryCreateMutableCopy, CFDictionaryRef, CFMutableDictionary,
    },
    number::CFNumber,
    propertylist::{
        create_data, create_with_data, kCFPropertyListBinaryFormat_v1_0, kCFPropertyListImmutable,
    },
    string::{CFString, CFStringRef},
};
use serde_json::{Map, Value, json};
use std::{
    ffi::{CStr, c_char, c_void},
    ptr,
    sync::{Arc, Mutex},
};

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCPreferencesCreate(allocator: CFTypeRef, name: CFStringRef, id: CFStringRef) -> CFTypeRef;
    fn SCPreferencesCreateWithAuthorization(
        allocator: CFTypeRef,
        name: CFStringRef,
        id: CFStringRef,
        authorization: CFTypeRef,
    ) -> CFTypeRef;
    fn SCPreferencesLock(prefs: CFTypeRef, wait: u8) -> u8;
    fn SCPreferencesUnlock(prefs: CFTypeRef) -> u8;
    fn SCPreferencesSynchronize(prefs: CFTypeRef);
    fn SCPreferencesCommitChanges(prefs: CFTypeRef) -> u8;
    fn SCPreferencesApplyChanges(prefs: CFTypeRef) -> u8;
    fn SCDynamicStoreCreate(
        allocator: CFTypeRef,
        name: CFStringRef,
        callback: CFTypeRef,
        context: CFTypeRef,
    ) -> CFTypeRef;
    fn SCDynamicStoreCopyValue(store: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn SCNetworkServiceCopy(prefs: CFTypeRef, service_id: CFStringRef) -> CFTypeRef;
    fn SCNetworkServiceCopyProtocol(service: CFTypeRef, protocol: CFStringRef) -> CFTypeRef;
    static kSCNetworkProtocolTypeProxies: CFStringRef;
    fn SCNetworkProtocolGetEnabled(protocol: CFTypeRef) -> u8;
    fn SCNetworkProtocolSetEnabled(protocol: CFTypeRef, enabled: u8) -> u8;
    fn SCNetworkProtocolGetConfiguration(protocol: CFTypeRef) -> CFDictionaryRef;
    fn SCNetworkProtocolSetConfiguration(protocol: CFTypeRef, config: CFDictionaryRef) -> u8;
    fn SCError() -> i32;
    fn SCErrorString(status: i32) -> *const c_char;
}

#[repr(C)]
struct AuthorizationItem {
    name: *const c_char,
    value_len: usize,
    value: *mut c_void,
    flags: u32,
}

#[repr(C)]
struct AuthorizationRights {
    count: u32,
    items: *mut AuthorizationItem,
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn AuthorizationCreate(
        rights: *const AuthorizationRights,
        env: CFTypeRef,
        flags: u32,
        authorization: *mut CFTypeRef,
    ) -> i32;
    fn AuthorizationCopyRights(
        authorization: CFTypeRef,
        rights: *const AuthorizationRights,
        env: CFTypeRef,
        flags: u32,
        authorized_rights: CFTypeRef,
    ) -> i32;
    fn AuthorizationFree(authorization: CFTypeRef, flags: u32) -> i32;
}

struct AuthorizationRef(CFTypeRef);

// The authorization handle has no thread affinity and is used only under the mutex below.
unsafe impl Send for AuthorizationRef {}

impl Drop for AuthorizationRef {
    fn drop(&mut self) {
        unsafe { AuthorizationFree(self.0, 0) };
    }
}

#[derive(Clone, Default)]
pub(super) struct Authorization(Arc<Mutex<Option<AuthorizationRef>>>);

fn checked(status: i32) -> Result<(), String> {
    match status {
        0 => Ok(()),
        -60006 => Err("已取消系统代理授权".into()),
        -60007 => Err("需要授权才能恢复系统代理，请点击恢复后重试".into()),
        _ => Err(format!("无法获得系统代理修改权限（{status}）")),
    }
}

fn failure(message: &str) -> String {
    // SystemConfiguration documents a program-lifetime C string for every error code.
    let error = unsafe { CStr::from_ptr(SCErrorString(SCError())) }.to_string_lossy();
    format!("{message}（{error}）")
}

fn created(reference: CFTypeRef) -> Option<CFType> {
    (!reference.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(reference) })
}

type Config = CFDictionary<CFString, CFType>;

fn primary_services() -> Result<Vec<String>, String> {
    let name = CFString::new("recho");
    let store = created(unsafe {
        SCDynamicStoreCreate(
            ptr::null(),
            name.as_concrete_TypeRef(),
            ptr::null(),
            ptr::null(),
        )
    })
    .ok_or_else(|| failure("无法读取当前网络服务"))?;
    let mut ids = Vec::new();
    for key in ["State:/Network/Global/IPv4", "State:/Network/Global/IPv6"] {
        let key = CFString::new(key);
        let value =
            unsafe { SCDynamicStoreCopyValue(store.as_CFTypeRef(), key.as_concrete_TypeRef()) };
        if let Some(dict) = created(value).and_then(|value| value.downcast_into::<CFDictionary>()) {
            // The dynamic store returns a dictionary with string keys and CF objects.
            let dict = unsafe { Config::wrap_under_get_rule(dict.as_concrete_TypeRef()) };
            if let Some(id) = dict
                .find(&CFString::new("PrimaryService"))
                .and_then(|v| v.downcast::<CFString>())
            {
                let id = id.to_string();
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    Ok(ids)
}

fn configuration(prefs: &CFType, id: &str) -> Option<CFType> {
    let id = CFString::new(id);
    let service =
        created(unsafe { SCNetworkServiceCopy(prefs.as_CFTypeRef(), id.as_concrete_TypeRef()) })?;
    created(unsafe {
        SCNetworkServiceCopyProtocol(service.as_CFTypeRef(), kSCNetworkProtocolTypeProxies)
    })
}

fn config(protocol: &CFType) -> Config {
    let dict = unsafe { SCNetworkProtocolGetConfiguration(protocol.as_CFTypeRef()) };
    if dict.is_null() {
        Config::from_CFType_pairs(&[])
    } else {
        unsafe { Config::wrap_under_get_rule(dict) }
    }
}

fn keys() -> impl Iterator<Item = &'static str> {
    [HTTP, HTTPS, SOCKS, AUTO].into_iter().flatten().copied()
}

fn encode(value: &CFType) -> Result<Value, String> {
    if let Some(value) = value.downcast::<CFString>() {
        return Ok(json!(value.to_string()));
    }
    if let Some(value) = value.downcast::<CFBoolean>() {
        return Ok(json!(bool::from(value)));
    }
    if let Some(number) = value.downcast::<CFNumber>()
        && let Some(value) = number.to_i64()
    {
        return Ok(json!(value));
    }
    let plist = create_data(value.as_CFTypeRef(), kCFPropertyListBinaryFormat_v1_0)
        .map_err(|error| error.to_string())?;
    Ok(json!({"plist": STANDARD.encode(plist.bytes())}))
}

fn decode(value: &Value) -> Result<CFType, String> {
    if let Some(value) = value.as_str() {
        return Ok(CFString::new(value).as_CFType());
    }
    if let Some(value) = value.as_bool() {
        return Ok(CFBoolean::from(value).as_CFType());
    }
    if let Some(value) = value.as_i64() {
        return Ok(CFNumber::from(value).as_CFType());
    }
    let plist = value["plist"]
        .as_str()
        .ok_or("系统代理恢复记录中的设置值无效")?;
    let bytes = STANDARD.decode(plist).map_err(|error| error.to_string())?;
    let (reference, _) = create_with_data(CFData::from_buffer(&bytes), kCFPropertyListImmutable)
        .map_err(|error| error.to_string())?;
    created(reference).ok_or_else(|| "系统代理恢复记录中的属性列表无效".into())
}

fn values(protocol: &CFType, config: &Config) -> Result<Value, String> {
    let mut values = Map::new();
    values.insert(
        ENABLED.into(),
        json!(unsafe { SCNetworkProtocolGetEnabled(protocol.as_CFTypeRef()) }),
    );
    for key in keys() {
        if let Some(value) = config.find(&CFString::new(key)) {
            values.insert(key.into(), encode(&value)?);
        }
    }
    Ok(Value::Object(values))
}

pub(super) fn read(mut ids: Vec<String>) -> Result<Vec<Value>, String> {
    let missing_is_error = ids.is_empty();
    if missing_is_error {
        ids = primary_services()?;
    }
    let name = CFString::new("recho");
    let prefs = created(unsafe {
        SCPreferencesCreate(ptr::null(), name.as_concrete_TypeRef(), ptr::null())
    })
    .ok_or_else(|| failure("无法读取系统代理"))?;
    let mut services = Vec::new();
    for id in ids {
        match configuration(&prefs, &id) {
            Some(protocol) => {
                services.push(json!({"id":id, "values":values(&protocol, &config(&protocol))?}))
            }
            None if missing_is_error => return Err("当前网络服务不支持系统代理".into()),
            None => (),
        }
    }
    Ok(services)
}

struct Lock(CFType);

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { SCPreferencesUnlock(self.0.as_CFTypeRef()) };
    }
}

pub(super) fn apply(
    changes: &[Value],
    authorization: &Authorization,
    interactive: bool,
) -> Result<(), String> {
    let mut auth = authorization.0.lock().map_err(|error| error.to_string())?;
    if auth.is_none() {
        let mut reference = ptr::null();
        checked(unsafe { AuthorizationCreate(ptr::null(), ptr::null(), 0, &mut reference) })?;
        if reference.is_null() {
            return Err("系统未返回系统代理授权".into());
        }
        *auth = Some(AuthorizationRef(reference));
    }
    let reference = auth.as_ref().expect("created authorization").0;
    let mut item = AuthorizationItem {
        name: c"system.preferences.network".as_ptr(),
        value_len: 0,
        value: ptr::null_mut(),
        flags: 0,
    };
    let rights = AuthorizationRights {
        count: 1,
        items: &mut item,
    };
    let flags = if interactive { 3 } else { 2 }; // extend rights, with optional interaction
    checked(unsafe {
        AuthorizationCopyRights(reference, &rights, ptr::null(), flags, ptr::null())
    })?;
    let name = CFString::new("recho");
    let prefs = created(unsafe {
        SCPreferencesCreateWithAuthorization(
            ptr::null(),
            name.as_concrete_TypeRef(),
            ptr::null(),
            reference,
        )
    })
    .ok_or_else(|| failure("无法打开系统网络设置"))?;
    let raw = prefs.as_CFTypeRef();
    if unsafe { SCPreferencesLock(raw, 0) } == 0 {
        return Err(failure("系统网络设置正由其他程序修改，请重试"));
    }
    let _lock = Lock(prefs.clone());
    unsafe { SCPreferencesSynchronize(raw) };
    for change in changes {
        let id = change["id"].as_str().ok_or("缺少网络服务 ID")?;
        let protocol = configuration(&prefs, id).ok_or("网络服务已移除或不可用")?;
        let config = config(&protocol);
        let current = values(&protocol, &config)?;
        if current != change["expected"] {
            return Err("系统代理已由其他程序修改，请刷新后重试".into());
        }
        let desired = &change["desired"];
        let mut next = unsafe {
            CFMutableDictionary::<CFString, CFType>::wrap_under_create_rule(
                CFDictionaryCreateMutableCopy(ptr::null(), 0, config.as_concrete_TypeRef()),
            )
        };
        for key in keys() {
            if current.get(key) != desired.get(key) {
                let name = CFString::new(key);
                if let Some(value) = desired.get(key) {
                    next.set(name, decode(value)?);
                } else {
                    next.remove(name);
                }
            }
        }
        let protocol = protocol.as_CFTypeRef();
        if unsafe {
            SCNetworkProtocolSetConfiguration(protocol, next.as_concrete_TypeRef().cast_const())
        } == 0
        {
            return Err(failure("无法保存系统代理配置"));
        }
        if current[ENABLED] != desired[ENABLED] {
            let enabled = desired[ENABLED]
                .as_u64()
                .filter(|value| *value <= 1)
                .ok_or("无效的协议状态")?;
            if unsafe { SCNetworkProtocolSetEnabled(protocol, enabled as u8) } == 0 {
                return Err(failure("无法修改代理协议状态"));
            }
        }
    }
    if unsafe { SCPreferencesCommitChanges(raw) } == 0 {
        return Err(failure("无法提交系统代理配置"));
    }
    if unsafe { SCPreferencesApplyChanges(raw) } == 0 {
        return Err(failure("系统代理配置已提交，但应用失败"));
    }
    Ok(())
}
