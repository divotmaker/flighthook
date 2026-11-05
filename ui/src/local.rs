//! UI preferences kept on this device, such as whether the skeleton is
//! drawn: the browser's local storage on the web, a small JSON file in the
//! user's local data folder in the native window (`flighthook/ui.json`).
//! Nothing here goes to the server. Tests keep them in memory.

/// The stored value of `key`, if any.
pub(crate) fn load(key: &str) -> Option<String> {
    imp::load(key)
}

/// Store `value` under `key`. A failure is ignored: the preference lasts
/// for this session only.
pub(crate) fn store(key: &str, value: &str) {
    imp::store(key, value);
}

/// The stored flag `key`, `default` when unset.
pub(crate) fn flag(key: &str, default: bool) -> bool {
    load(key).map_or(default, |v| v == "true")
}

#[cfg(all(target_arch = "wasm32", not(test)))]
mod imp {
    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub(super) fn load(key: &str) -> Option<String> {
        storage()?.get_item(&format!("flighthook.{key}")).ok()?
    }

    pub(super) fn store(key: &str, value: &str) {
        if let Some(s) = storage() {
            let _ = s.set_item(&format!("flighthook.{key}"), value);
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
mod imp {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        Some(dirs::data_local_dir()?.join("flighthook").join("ui.json"))
    }

    fn read() -> BTreeMap<String, String> {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub(super) fn load(key: &str) -> Option<String> {
        read().remove(key)
    }

    pub(super) fn store(key: &str, value: &str) {
        let Some(path) = path() else {
            return;
        };
        let mut all = read();
        all.insert(key.to_owned(), value.to_owned());
        let Ok(json) = serde_json::to_vec_pretty(&all) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let part = path.with_extension("json.part");
        if std::fs::write(&part, json).is_ok() {
            let _ = std::fs::rename(&part, &path);
        }
    }
}

#[cfg(test)]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static VALUES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }

    pub(super) fn load(key: &str) -> Option<String> {
        VALUES.with(|v| v.borrow().get(key).cloned())
    }

    pub(super) fn store(key: &str, value: &str) {
        VALUES.with(|v| v.borrow_mut().insert(key.to_owned(), value.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_reads_back_and_defaults_when_unset() {
        assert!(flag("test_unset", true));
        assert!(!flag("test_unset", false));
        store("test_flag", "false");
        assert!(!flag("test_flag", true));
        store("test_flag", "true");
        assert!(flag("test_flag", false));
        assert_eq!(load("test_flag").as_deref(), Some("true"));
    }
}
