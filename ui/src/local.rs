//! UI preferences kept on this device, such as the Shots tab's pane: the
//! browser's local storage on the web, a small JSON file in the
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
    fn a_value_reads_back_and_is_none_when_unset() {
        assert_eq!(load("test_unset"), None);
        store("test_value", "cameras");
        assert_eq!(load("test_value").as_deref(), Some("cameras"));
        store("test_value", "data");
        assert_eq!(load("test_value").as_deref(), Some("data"));
    }
}
