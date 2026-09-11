//! Windows user-level environment variable adapter.
//!
//! Only the Core-derived [`AiKeyEnvName`] supplied by the caller is touched.
//! The adapter never enumerates the user's environment and never includes a
//! key value in an error or warning.  Registry and current-process updates are
//! treated as one operation: if process refresh fails after registry success,
//! the previous registry value (including absence) is restored before the
//! error is returned.

use devresidue_core::ai::{AiKeyEnvName, EnvKeyStore};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Environment::SetEnvironmentVariableW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

const ENVIRONMENT_KEY: &str = "Environment";
const BROADCAST_TIMEOUT_MS: u32 = 1_000;

/// HKCU-backed implementation of Core's [`EnvKeyStore`] port.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsUserEnvKeyStore;

impl WindowsUserEnvKeyStore {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl EnvKeyStore for WindowsUserEnvKeyStore {
    fn set(&self, name: &AiKeyEnvName, value: &str) -> Result<(), String> {
        let name_wide = checked_wide(name.as_str())?;
        let value_wide = checked_wide(value)?;
        let backend = WindowsRegistryProcessBackend;
        set_with_backend(&backend, &name_wide, &value_wide)?;
        broadcast_environment_change();
        Ok(())
    }

    fn get(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        let name_wide = checked_wide(name.as_str())?;
        let backend = WindowsRegistryProcessBackend;
        backend
            .read_registry(&name_wide)?
            .map(|bytes| decode_registry_string(&bytes))
            .transpose()
    }

    fn remove(&self, name: &AiKeyEnvName) -> Result<(), String> {
        let name_wide = checked_wide(name.as_str())?;
        let backend = WindowsRegistryProcessBackend;
        remove_with_backend(&backend, &name_wide)?;
        broadcast_environment_change();
        Ok(())
    }

    fn get_process(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        let name_wide = checked_wide(name.as_str())?;
        let value = match std::env::var(name.as_str()) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err("current process environment value is not valid Unicode".to_string())
            }
        };
        let _ = name_wide;
        Ok(value)
    }
}

/// Registry/process seam used by the production adapter and failure-injection
/// tests.  It is intentionally private: platform callers receive only the
/// safe `WindowsUserEnvKeyStore` API.
trait RegistryProcessBackend {
    fn read_registry(&self, name: &[u16]) -> Result<Option<Vec<u8>>, String>;
    fn write_registry(&self, name: &[u16], bytes: &[u8]) -> Result<(), String>;
    fn remove_registry(&self, name: &[u16]) -> Result<(), String>;
    fn set_process(&self, name: &[u16], value: Option<&[u16]>) -> Result<(), String>;
}

fn set_with_backend(
    backend: &dyn RegistryProcessBackend,
    name: &[u16],
    value: &[u16],
) -> Result<(), String> {
    let previous = backend.read_registry(name)?;
    let expected = utf16_bytes(value);
    backend.write_registry(name, &expected)?;
    let observed = match backend.read_registry(name) {
        Ok(value) => value,
        Err(error) => {
            return Err(restore_after_registry_error(backend, name, previous, error));
        }
    };
    let Some(observed) = observed.as_deref() else {
        return Err(restore_after_registry_error(
            backend,
            name,
            previous,
            "user environment registry value disappeared after write".to_string(),
        ));
    };
    if let Err(error) = validate_registry_bytes(REG_SZ, observed) {
        return Err(restore_after_registry_error(backend, name, previous, error));
    }
    if observed != expected.as_slice() {
        return Err(restore_after_registry_error(
            backend,
            name,
            previous,
            "user environment registry value did not match the requested value".to_string(),
        ));
    }
    if backend.set_process(name, Some(value)).is_err() {
        restore_registry(backend, name, previous)?;
        return Err("unable to refresh the current process environment".to_string());
    }
    Ok(())
}

fn remove_with_backend(backend: &dyn RegistryProcessBackend, name: &[u16]) -> Result<(), String> {
    let previous = backend.read_registry(name)?;
    backend.remove_registry(name)?;
    let observed = match backend.read_registry(name) {
        Ok(value) => value,
        Err(error) => {
            return Err(restore_after_registry_error(backend, name, previous, error));
        }
    };
    if observed.is_some() {
        return Err(restore_after_registry_error(
            backend,
            name,
            previous,
            "user environment registry value remained after removal".to_string(),
        ));
    }
    if backend.set_process(name, None).is_err() {
        restore_registry(backend, name, previous)?;
        return Err("unable to refresh the current process environment".to_string());
    }
    Ok(())
}

fn restore_registry(
    backend: &dyn RegistryProcessBackend,
    name: &[u16],
    previous: Option<Vec<u8>>,
) -> Result<(), String> {
    match previous {
        Some(bytes) => {
            backend.write_registry(name, &bytes).map_err(|_| {
                "unable to restore the previous user environment registry value".to_string()
            })?;
            let observed = backend.read_registry(name).map_err(|_| {
                "unable to verify the restored user environment registry value".to_string()
            })?;
            if observed.as_deref() == Some(bytes.as_slice()) {
                Ok(())
            } else {
                Err("unable to verify the restored user environment registry value".to_string())
            }
        }
        None => {
            backend.remove_registry(name).map_err(|_| {
                "unable to restore the previous user environment registry value".to_string()
            })?;
            let observed = backend.read_registry(name).map_err(|_| {
                "unable to verify the restored user environment registry value".to_string()
            })?;
            if observed.is_none() {
                Ok(())
            } else {
                Err("unable to verify the restored user environment registry value".to_string())
            }
        }
    }
}

fn restore_after_registry_error(
    backend: &dyn RegistryProcessBackend,
    name: &[u16],
    previous: Option<Vec<u8>>,
    error: String,
) -> String {
    match restore_registry(backend, name, previous) {
        Ok(()) => error,
        Err(restore_error) => format!("{error}; {restore_error}"),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct WindowsRegistryProcessBackend;

impl RegistryProcessBackend for WindowsRegistryProcessBackend {
    fn read_registry(&self, name: &[u16]) -> Result<Option<Vec<u8>>, String> {
        let key = match open_environment_key(false, KEY_READ) {
            Ok(key) => key,
            Err(RegistryOpenError::Missing) => return Ok(None),
            Err(RegistryOpenError::Other) => {
                return Err("unable to open the user environment registry key".to_string())
            }
        };
        query_registry_value(&key, name)
    }

    fn write_registry(&self, name: &[u16], bytes: &[u8]) -> Result<(), String> {
        let key = open_environment_key(true, KEY_SET_VALUE)
            .map_err(|_| "unable to open the user environment registry key".to_string())?;
        let status = {
            // SAFETY: `key` owns a valid HKCU\Environment handle; the name and
            // byte buffers are NUL-terminated/owned for this synchronous call.
            #[allow(unsafe_code)]
            unsafe {
                RegSetValueExW(
                    key.raw(),
                    PCWSTR(name.as_ptr()),
                    Some(0),
                    REG_SZ,
                    Some(bytes),
                )
            }
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err("unable to write the user environment registry value".to_string())
        }
    }

    fn remove_registry(&self, name: &[u16]) -> Result<(), String> {
        let key = match open_environment_key(false, KEY_SET_VALUE) {
            Ok(key) => key,
            Err(RegistryOpenError::Missing) => return Ok(()),
            Err(RegistryOpenError::Other) => {
                return Err("unable to open the user environment registry key".to_string())
            }
        };
        let status = {
            // SAFETY: `key` owns a valid writable HKCU\Environment handle and
            // the NUL-terminated name remains alive for the synchronous call.
            #[allow(unsafe_code)]
            unsafe {
                RegDeleteValueW(key.raw(), PCWSTR(name.as_ptr()))
            }
        };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err("unable to remove the user environment registry value".to_string())
        }
    }

    fn set_process(&self, name: &[u16], value: Option<&[u16]>) -> Result<(), String> {
        set_process_environment(name, value)
    }
}

fn query_registry_value(key: &RegistryKey, name: &[u16]) -> Result<Option<Vec<u8>>, String> {
    let mut value_type = REG_SZ;
    let mut byte_count = 0u32;
    let status = {
        // SAFETY: `key` remains alive, output pointers are valid, no data
        // buffer is supplied for this size query, and `name` remains alive.
        #[allow(unsafe_code)]
        unsafe {
            RegQueryValueExW(
                key.raw(),
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut value_type),
                None,
                Some(&mut byte_count),
            )
        }
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        return Err("unable to read the user environment registry value".to_string());
    }
    if byte_count == 0 || byte_count % 2 != 0 {
        return Err("user environment registry value is not a valid string".to_string());
    }

    let mut bytes = vec![0u8; byte_count as usize];
    let status = {
        // SAFETY: `bytes` has the size requested by the registry, the output
        // pointers are valid, and all input buffers remain alive.
        #[allow(unsafe_code)]
        unsafe {
            RegQueryValueExW(
                key.raw(),
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut value_type),
                Some(bytes.as_mut_ptr()),
                Some(&mut byte_count),
            )
        }
    };
    if status != ERROR_SUCCESS || byte_count as usize > bytes.len() {
        return Err("unable to read the user environment registry value".to_string());
    }
    bytes.truncate(byte_count as usize);
    validate_registry_bytes(value_type, &bytes)?;
    Ok(Some(bytes))
}

fn validate_registry_bytes(
    value_type: windows::Win32::System::Registry::REG_VALUE_TYPE,
    bytes: &[u8],
) -> Result<(), String> {
    if value_type != REG_SZ || bytes.len() < 2 || bytes.len() % 2 != 0 {
        return Err("user environment registry value is not a valid string".to_string());
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
        return Err("user environment registry value is not a valid string".to_string());
    }
    String::from_utf16(&units[..units.len() - 1])
        .map(|_| ())
        .map_err(|_| "user environment registry value is not valid UTF-16".to_string())
}

fn decode_registry_string(bytes: &[u8]) -> Result<String, String> {
    validate_registry_bytes(REG_SZ, bytes)?;
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&units[..units.len() - 1])
        .map_err(|_| "user environment registry value is not valid UTF-16".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryOpenError {
    Missing,
    Other,
}

#[derive(Debug)]
struct RegistryKey(HKEY);

impl RegistryKey {
    fn raw(&self) -> HKEY {
        self.0
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: this RAII value owns the registry handle returned by
        // RegOpenKeyExW/RegCreateKeyExW and closes it exactly once.
        #[allow(unsafe_code)]
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_environment_key(
    create: bool,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<RegistryKey, RegistryOpenError> {
    let subkey = checked_wide(ENVIRONMENT_KEY).map_err(|_| RegistryOpenError::Other)?;
    let mut key = HKEY::default();
    let status = if create {
        // SAFETY: `key` is a valid writable output pointer; HKCU and the local
        // NUL-terminated subkey remain alive for the call.
        #[allow(unsafe_code)]
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                access,
                None,
                &mut key,
                None,
            )
        }
    } else {
        // SAFETY: `key` is a valid writable output pointer; HKCU and the local
        // NUL-terminated subkey remain alive for the call.
        #[allow(unsafe_code)]
        unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                access,
                &mut key,
            )
        }
    };
    if status == ERROR_SUCCESS {
        Ok(RegistryKey(key))
    } else if status == ERROR_FILE_NOT_FOUND {
        Err(RegistryOpenError::Missing)
    } else {
        Err(RegistryOpenError::Other)
    }
}

fn checked_wide(value: &str) -> Result<Vec<u16>, String> {
    if value.is_empty() || value.encode_utf16().any(|unit| unit == 0) {
        return Err("environment variable name or value is invalid".to_string());
    }
    Ok(value.encode_utf16().chain(std::iter::once(0)).collect())
}

fn utf16_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

fn set_process_environment(name: &[u16], value: Option<&[u16]>) -> Result<(), String> {
    let result = {
        // SAFETY: both UTF-16 buffers are owned, NUL-terminated values that
        // remain alive for this synchronous call. Win32 retains no pointer.
        #[allow(unsafe_code)]
        unsafe {
            match value {
                Some(value) => {
                    SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR(value.as_ptr()))
                }
                None => SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR::null()),
            }
        }
    };
    result.map_err(|_| "unable to refresh the current process environment".to_string())
}

fn broadcast_environment_change() {
    let setting_name: Vec<u16> = "Environment"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let result = {
        // SAFETY: HWND_BROADCAST is the documented target; the setting name
        // buffer is NUL-terminated and alive for the bounded synchronous call.
        #[allow(unsafe_code)]
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(PCWSTR(setting_name.as_ptr()).0 as isize),
                SMTO_ABORTIFHUNG,
                BROADCAST_TIMEOUT_MS,
                None,
            )
        }
    };
    if result.0 == 0 {
        // Registry/process updates already succeeded. Broadcast is advisory;
        // deliberately do not include the variable name or value.
        eprintln!("warning: user environment change broadcast was not delivered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::ai::{derived_api_key_env, AiProfileId};
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Mutex;
    use uuid::Uuid;

    #[derive(Default)]
    struct FakeBackend {
        registry: Mutex<Option<Vec<u8>>>,
        fail_process: Mutex<bool>,
        corrupt_write: Mutex<bool>,
    }

    impl RegistryProcessBackend for FakeBackend {
        fn read_registry(&self, _name: &[u16]) -> Result<Option<Vec<u8>>, String> {
            Ok(self.registry.lock().unwrap().clone())
        }

        fn write_registry(&self, _name: &[u16], bytes: &[u8]) -> Result<(), String> {
            let corrupt = *self.corrupt_write.lock().unwrap();
            if corrupt {
                *self.corrupt_write.lock().unwrap() = false;
            }
            let bytes = if corrupt { vec![1] } else { bytes.to_vec() };
            *self.registry.lock().unwrap() = Some(bytes);
            Ok(())
        }

        fn remove_registry(&self, _name: &[u16]) -> Result<(), String> {
            *self.registry.lock().unwrap() = None;
            Ok(())
        }

        fn set_process(&self, _name: &[u16], _value: Option<&[u16]>) -> Result<(), String> {
            if *self.fail_process.lock().unwrap() {
                Err("injected process refresh failure".to_string())
            } else {
                Ok(())
            }
        }
    }

    fn derived_test_name() -> Vec<u16> {
        let id = AiProfileId::parse(&Uuid::new_v4().to_string()).unwrap();
        checked_wide(derived_api_key_env(&id).as_str()).unwrap()
    }

    #[test]
    fn process_refresh_failure_restores_absent_registry_value() {
        let backend = FakeBackend::default();
        *backend.fail_process.lock().unwrap() = true;
        let name = derived_test_name();
        let value = checked_wide("secret-test-value").unwrap();
        assert!(set_with_backend(&backend, &name, &value).is_err());
        assert!(backend.registry.lock().unwrap().is_none());
    }

    #[test]
    fn process_refresh_failure_restores_previous_registry_value() {
        let backend = FakeBackend::default();
        let name = derived_test_name();
        let old = checked_wide("old-value").unwrap();
        *backend.registry.lock().unwrap() = Some(utf16_bytes(&old));
        *backend.fail_process.lock().unwrap() = true;
        let value = checked_wide("new-value").unwrap();
        assert!(set_with_backend(&backend, &name, &value).is_err());
        assert_eq!(
            backend.registry.lock().unwrap().as_deref(),
            Some(utf16_bytes(&old).as_slice())
        );
    }

    #[test]
    fn remove_process_refresh_failure_restores_previous_registry_value() {
        let backend = FakeBackend::default();
        let name = derived_test_name();
        let old = checked_wide("old-value").unwrap();
        *backend.registry.lock().unwrap() = Some(utf16_bytes(&old));
        *backend.fail_process.lock().unwrap() = true;

        assert!(remove_with_backend(&backend, &name).is_err());
        assert_eq!(
            backend.registry.lock().unwrap().as_deref(),
            Some(utf16_bytes(&old).as_slice())
        );
    }

    #[test]
    fn malformed_registry_writeback_is_rejected_and_restores_previous_value() {
        let backend = FakeBackend::default();
        let name = derived_test_name();
        let old = checked_wide("old-value").unwrap();
        *backend.registry.lock().unwrap() = Some(utf16_bytes(&old));
        *backend.corrupt_write.lock().unwrap() = true;
        let value = checked_wide("new-value").unwrap();

        assert!(set_with_backend(&backend, &name, &value).is_err());
        assert_eq!(
            backend.registry.lock().unwrap().as_deref(),
            Some(utf16_bytes(&old).as_slice())
        );
    }

    #[test]
    fn invalid_unicode_in_current_process_is_not_treated_as_absent() {
        let id = AiProfileId::parse(&Uuid::new_v4().to_string()).unwrap();
        let env_name = derived_api_key_env(&id);
        let name = checked_wide(env_name.as_str()).unwrap();
        let store = WindowsUserEnvKeyStore::new();
        let previous = std::env::var_os(env_name.as_str());
        let invalid = [0xD800, 0];

        // SAFETY: `name` and `invalid` are owned, NUL-terminated UTF-16 buffers
        // that remain alive for this synchronous test-only call.
        #[allow(unsafe_code)]
        unsafe {
            SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR(invalid.as_ptr()))
                .expect("set invalid test environment value");
        }

        let result = store.get_process(&env_name);

        // SAFETY: the variable name remains NUL-terminated and alive for the
        // synchronous cleanup call.
        #[allow(unsafe_code)]
        unsafe {
            match previous {
                Some(value) => {
                    let value: Vec<u16> = value.encode_wide().chain(std::iter::once(0)).collect();
                    SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR(value.as_ptr()))
                        .expect("restore test environment value");
                }
                None => SetEnvironmentVariableW(PCWSTR(name.as_ptr()), PCWSTR::null())
                    .expect("remove test environment value"),
            }
        }

        assert!(result.is_err());
    }

    #[test]
    fn malformed_registry_values_fail_closed_after_second_query_validation() {
        assert!(validate_registry_bytes(REG_SZ, &[1]).is_err());
        assert!(validate_registry_bytes(REG_SZ, &[0, 0, 1, 0]).is_err());
        assert!(validate_registry_bytes(REG_SZ, &[65, 0, 66, 0]).is_err());
    }
}
