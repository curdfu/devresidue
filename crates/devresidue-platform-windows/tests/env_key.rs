use std::env;
use std::sync::Arc;
use std::thread;

use devresidue_core::ai::{derived_api_key_env, AiKeyEnvName, AiProfileId, EnvKeyStore};
use devresidue_platform_windows::env_key::WindowsUserEnvKeyStore;
use uuid::Uuid;

struct EnvRestore {
    name: AiKeyEnvName,
    previous: Option<String>,
    store: WindowsUserEnvKeyStore,
    restore_result: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
}

impl EnvRestore {
    fn new(name: AiKeyEnvName, store: WindowsUserEnvKeyStore) -> Result<Self, String> {
        let previous = store.get(&name)?;
        Ok(Self {
            previous,
            name,
            store,
            restore_result: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    fn restore_result(&self) -> Arc<std::sync::Mutex<Option<Result<(), String>>>> {
        Arc::clone(&self.restore_result)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        let mut result = Err("cleanup was not attempted".to_string());
        for _attempt in 0..3 {
            result = match &self.previous {
                Some(value) => self.store.set(&self.name, value),
                None => self.store.remove(&self.name),
            };
            if result.is_ok() {
                break;
            }
            thread::yield_now();
        }
        *self.restore_result.lock().unwrap() = Some(result);
    }
}

#[test]
fn set_update_get_remove_refreshes_current_process_for_one_random_key() {
    let id = AiProfileId::parse(&Uuid::new_v4().to_string()).unwrap();
    let name = derived_api_key_env(&id);
    let store = WindowsUserEnvKeyStore::new();
    let previous = store.get(&name).unwrap();
    let restore_result;
    {
        let restore = EnvRestore::new(name.clone(), store).unwrap();
        restore_result = restore.restore_result();

        store.set(&name, "task4-first-key").unwrap();
        assert_eq!(
            store.get(&name).unwrap().as_deref(),
            Some("task4-first-key")
        );
        assert_eq!(
            env::var(name.as_str()).ok().as_deref(),
            Some("task4-first-key")
        );

        store.set(&name, "task4-second-key").unwrap();
        assert_eq!(
            store.get(&name).unwrap().as_deref(),
            Some("task4-second-key")
        );
        assert_eq!(
            env::var(name.as_str()).ok().as_deref(),
            Some("task4-second-key")
        );

        store.remove(&name).unwrap();
        assert_eq!(store.get(&name).unwrap(), None);
        assert_eq!(env::var(name.as_str()).ok(), None);
    }

    let result = restore_result.lock().unwrap().take().unwrap();
    assert!(
        result.is_ok(),
        "test variable restoration failed: {result:?}"
    );
    assert_eq!(store.get(&name).unwrap(), previous);
    assert_eq!(env::var(name.as_str()).ok(), previous);
}

#[test]
fn store_is_send_sync_and_does_not_need_shared_global_state() {
    let store = Arc::new(WindowsUserEnvKeyStore::new());
    let other = Arc::clone(&store);
    std::thread::spawn(move || {
        let _ = other;
    })
    .join()
    .unwrap();
}
