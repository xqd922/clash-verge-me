use crate::core::handle;
use crate::{config::Config, feat, log_err};
use crate::utils::resolve;
use anyhow::{bail, Result};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};
use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, ShortcutState};
use tauri::async_runtime;

pub struct Hotkey {
    current: Arc<Mutex<Vec<String>>>, // 保存当前的热键设置
    initialized: Arc<Mutex<bool>>,    // 是否已初始化
}

impl Hotkey {
    pub fn global() -> &'static Hotkey {
        static HOTKEY: OnceCell<Hotkey> = OnceCell::new();

        HOTKEY.get_or_init(|| Hotkey {
            current: Arc::new(Mutex::new(Vec::new())),
            initialized: Arc::new(Mutex::new(false)),
        })
    }

    pub fn init(&self) -> Result<()> {
        // 防止重复初始化
        {
            let mut initialized = self.initialized.lock();
            if *initialized {
                log::debug!(target: "app", "Hotkeys already initialized, skipping");
                return Ok(());
            }
            *initialized = true;
        }

        let verge = Config::verge();
        let enable_global_hotkey = verge.latest().enable_global_hotkey.unwrap_or(true);

        log::info!(target: "app", "Initializing hotkeys, global hotkey enabled: {}", enable_global_hotkey);

        // 如果全局热键被禁用，则不注册热键
        if !enable_global_hotkey {
            log::info!(target: "app", "Global hotkey is disabled, skipping registration");
            return Ok(());
        }

        if let Some(hotkeys) = verge.latest().hotkeys.as_ref() {
            log::info!(target: "app", "Found {} hotkeys to register", hotkeys.len());

            for hotkey in hotkeys.iter() {
                let mut iter = hotkey.split(',');
                let func = iter.next();
                let key = iter.next();

                match (key, func) {
                    (Some(key), Some(func)) => {
                        log::info!(target: "app", "Registering hotkey: {} -> {}", key, func);
                        if let Err(e) = self.register(key, func) {
                            log::error!(target: "app", "Failed to register hotkey {} -> {}: {:?}", key, func, e);
                        }
                    }
                    _ => {
                        let key = key.unwrap_or("None");
                        let func = func.unwrap_or("None");
                        log::error!(target: "app", "Invalid hotkey configuration: `{key}`:`{func}`");
                    }
                }
            }
            self.current.lock().clone_from(hotkeys);
        } else {
            log::info!(target: "app", "No hotkeys configured");
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn reset(&self) -> Result<()> {
        let app_handle = handle::Handle::global().app_handle().unwrap();
        let manager = app_handle.global_shortcut();
        manager.unregister_all()?;
        // 重置初始化状态
        *self.initialized.lock() = false;
        Ok(())
    }

    pub fn register(&self, hotkey: &str, func: &str) -> Result<()> {
        let app_handle = handle::Handle::global().app_handle().unwrap();
        let manager = app_handle.global_shortcut();

        // 如果已经注册了相同的热键，直接返回
        if manager.is_registered(hotkey) {
            log::debug!(target: "app", "Hotkey {} already registered, unregistering first", hotkey);
            manager.unregister(hotkey)?;
        }

        let f = match func.trim() {
            "open_or_close_dashboard" => {
                || {
                    log::info!(target: "app", "Hotkey: open_or_close_dashboard triggered");
                    async_runtime::spawn_blocking(|| {
                        resolve::create_window();
                    });
                }
            },
            "clash_mode_rule" => || feat::change_clash_mode("rule".into()),
            "clash_mode_global" => || feat::change_clash_mode("global".into()),
            "clash_mode_direct" => || feat::change_clash_mode("direct".into()),
            "toggle_system_proxy" => || feat::toggle_system_proxy(),
            "toggle_tun_mode" => || feat::toggle_tun_mode(),
            "quit" => || feat::quit(Some(0)),

            _ => {
                log::error!(target: "app", "Invalid function: {}", func);
                bail!("invalid function \"{func}\"");
            }
        };

        let is_quit = func.trim() == "quit";

        let _ = manager.on_shortcut(hotkey, move |app_handle, hotkey, event| {
            if event.state == ShortcutState::Pressed {
                log::debug!(target: "app", "Hotkey pressed: {:?}", hotkey);

                if hotkey.key == Code::KeyQ && is_quit {
                    if let Some(window) = app_handle.get_webview_window("main") {
                        if window.is_focused().unwrap_or(false) {
                            f();
                        }
                    }
                } else {
                    f();
                }
            }
        });

        log::info!(target: "app", "Registered hotkey {} for {}", hotkey, func);
        Ok(())
    }

    pub fn unregister(&self, hotkey: &str) -> Result<()> {
        let app_handle = handle::Handle::global().app_handle().unwrap();
        let manager = app_handle.global_shortcut();
        if manager.is_registered(hotkey) {
            manager.unregister(hotkey)?;
            log::debug!(target: "app", "unregister hotkey {hotkey}");
        }
        Ok(())
    }

    pub fn update(&self, new_hotkeys: Vec<String>) -> Result<()> {
        let mut current = self.current.lock();
        let old_map = Self::get_map_from_vec(&current);
        let new_map = Self::get_map_from_vec(&new_hotkeys);

        let (del, add) = Self::get_diff(old_map, new_map);

        del.iter().for_each(|key| {
            let _ = self.unregister(key);
        });

        add.iter().for_each(|(key, func)| {
            log_err!(self.register(key, func));
        });

        *current = new_hotkeys;
        Ok(())
    }

    fn get_map_from_vec(hotkeys: &[String]) -> HashMap<&str, &str> {
        let mut map = HashMap::new();

        hotkeys.iter().for_each(|hotkey| {
            let mut iter = hotkey.split(',');
            let func = iter.next();
            let key = iter.next();

            if func.is_some() && key.is_some() {
                let func = func.unwrap().trim();
                let key = key.unwrap().trim();
                map.insert(key, func);
            }
        });
        map
    }

    fn get_diff<'a>(
        old_map: HashMap<&'a str, &'a str>,
        new_map: HashMap<&'a str, &'a str>,
    ) -> (Vec<&'a str>, Vec<(&'a str, &'a str)>) {
        let mut del_list = vec![];
        let mut add_list = vec![];

        old_map.iter().for_each(|(&key, func)| {
            match new_map.get(key) {
                Some(new_func) => {
                    if new_func != func {
                        del_list.push(key);
                        add_list.push((key, *new_func));
                    }
                }
                None => del_list.push(key),
            };
        });

        new_map.iter().for_each(|(&key, &func)| {
            if !old_map.contains_key(key) {
                add_list.push((key, func));
            }
        });

        (del_list, add_list)
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        let app_handle = handle::Handle::global().app_handle().unwrap();
        if let Err(e) = app_handle.global_shortcut().unregister_all() {
            log::error!(target:"app", "Error unregistering all hotkeys: {:?}", e);
        }
    }
}
