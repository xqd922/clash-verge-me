use crate::config::*;
use crate::core::{clash_api, handle, service};
#[cfg(target_os = "macos")]
use crate::core::tray::Tray;
use crate::log_err;
use crate::utils::{dirs, help};
use anyhow::{bail, Result};
use once_cell::sync::OnceCell;
use serde_yaml::Mapping;
use std::{sync::Arc, time::Duration};
use tauri_plugin_shell::ShellExt;
use tokio::sync::Mutex;
use tokio::time::sleep;

#[derive(Debug)]
pub struct CoreManager {
    running: Arc<Mutex<bool>>,
}

impl CoreManager {
    pub fn global() -> &'static CoreManager {
        static CORE_MANAGER: OnceCell<CoreManager> = OnceCell::new();
        CORE_MANAGER.get_or_init(|| CoreManager {
            running: Arc::new(Mutex::new(false)),
        })
    }

    pub async fn init(&self) -> Result<()> {
        log::trace!("run core start");
        // 启动clash
        log_err!(Self::global().start_core().await);
        log::trace!("run core end");
        Ok(())
    }

    /// 停止核心运行
    pub async fn stop_core(&self) -> Result<()> {
        let mut running = self.running.lock().await;

        if !*running {
            log::debug!("core is not running");
            return Ok(());
        }

        // 关闭tun模式
        let mut disable = Mapping::new();
        let mut tun = Mapping::new();
        tun.insert("enable".into(), false.into());
        disable.insert("tun".into(), tun.into());
        log::debug!(target: "app", "disable tun mode");
        log_err!(clash_api::patch_configs(&disable).await);

        // 服务模式
        if service::check_service().await.is_ok() {
            log::info!(target: "app", "stop the core by service");
            service::stop_core_by_service().await?;
        } else {
            // Sidecar 模式 - 通过进程名杀死
            log::info!(target: "app", "Stopping sidecar");
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/IM", "verge-mihomo.exe"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/IM", "verge-mihomo-alpha.exe"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = std::process::Command::new("pkill")
                    .args(["-f", "verge-mihomo"])
                    .output();
            }
        }
        *running = false;
        Ok(())
    }

    /// 启动核心
    pub async fn start_core(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        if *running {
            log::info!("core is running");
            return Ok(());
        }

        let config_path = Config::generate_file(ConfigType::Run)?;

        // 服务模式
        if service::check_service().await.is_ok() {
            log::info!(target: "app", "try to run core in service mode");
            service::run_core_by_service(&config_path).await?;
        } else {
            // Sidecar 模式
            log::info!(target: "app", "Starting core in sidecar mode");

            let clash_core = { Config::verge().latest().clash_core.clone() };
            let clash_core = clash_core.unwrap_or("verge-mihomo".into());

            let app_handle = handle::Handle::global().app_handle().unwrap();
            let config_dir = dirs::app_home_dir()?;
            let config_dir = dirs::path_to_str(&config_dir)?;
            let config_file = dirs::path_to_str(&config_path)?;

            let (_, child) = app_handle
                .shell()
                .sidecar(clash_core)?
                .args(["-d", config_dir, "-f", config_file])
                .spawn()?;

            // 存储子进程以便后续管理
            std::mem::forget(child);

            // 等待核心启动
            sleep(Duration::from_millis(500)).await;
            log::info!(target: "app", "Sidecar core started");
        }
        // 流量订阅
        #[cfg(target_os = "macos")]
        log_err!(Tray::global().subscribe_traffic().await);

        *running = true;

        Ok(())
    }

    /// 重启内核
    pub async fn restart_core(&self) -> Result<()> {
        // 重新启动app
        self.stop_core().await?;
        self.start_core().await?;
        Ok(())
    }

    /// 使用默认配置
    pub async fn use_default_config(&self, msg_type: &str, msg_content: &str) -> Result<()> {
        let runtime_path = dirs::app_home_dir()?.join(RUNTIME_CONFIG);
        *Config::runtime().draft() = IRuntime {
            config: Some(Config::clash().latest().0.clone()),
            exists_keys: vec![],
            chain_logs: Default::default(),
        };
        help::save_yaml(
            &runtime_path,
            &Config::clash().latest().0,
            Some("# Clash Verge Runtime"),
        )?;
        handle::Handle::notice_message(msg_type, msg_content);
        Ok(())
    }

    /// 切换核心
    pub async fn change_core(&self, clash_core: Option<String>) -> Result<()> {
        let clash_core = clash_core.ok_or(anyhow::anyhow!("clash core is null"))?;
        const CLASH_CORES: [&str; 2] = ["verge-mihomo", "verge-mihomo-alpha"];

        if !CLASH_CORES.contains(&clash_core.as_str()) {
            bail!("invalid clash core name \"{clash_core}\"");
        }

        log::info!(target: "app", "change core to `{clash_core}`");

        // 1. 先更新内核配置（但不应用）
        Config::verge().draft().clash_core = Some(clash_core);

        // 2. 使用新内核验证配置
        match self.validate_config().await {
            Ok((true, _)) => {
                // 3. 验证通过后，应用内核配置并重启
                Config::verge().apply();
                log_err!(Config::verge().latest().save_file());

                match self.restart_core().await {
                    Ok(_) => {
                        log::info!(target: "app", "core change successful");
                        Config::runtime().apply();
                        Ok(())
                    }
                    Err(err) => {
                        log::error!(target: "app", "core change failed: {}", err);
                        Config::verge().discard();
                        Config::runtime().discard();
                        Err(err)
                    }
                }
            }
            Ok((false, error_msg)) => {
                log::warn!(target: "app", "config validation failed for new core: {}", error_msg.trim());
                // 使用默认配置并继续切换内核
                self.use_default_config("config_validate::core_change", &error_msg).await?;
                Config::verge().apply();
                log_err!(Config::verge().latest().save_file());

                match self.restart_core().await {
                    Ok(_) => {
                        log::info!(target: "app", "core change successful (default config)");
                        Ok(())
                    }
                    Err(err) => {
                        log::error!(target: "app", "core change failed: {}", err);
                        Config::verge().discard();
                        Err(err)
                    }
                }
            }
            Err(err) => {
                log::error!(target: "app", "validation error during core change: {}", err);
                Config::verge().discard();
                Err(err)
            }
        }
    }

    /// 内部验证配置文件的实现
    async fn validate_config_internal(&self, config_path: &str) -> Result<(bool, String)> {
        log::debug!(target: "app", "validating config: {}", config_path);

        let clash_core = { Config::verge().latest().clash_core.clone() };
        let clash_core = clash_core.unwrap_or("verge-mihomo".into());

        let app_handle = handle::Handle::global().app_handle().unwrap();
        let test_dir = dirs::app_home_dir()?.join("test");
        let test_dir = dirs::path_to_str(&test_dir)?;

        // 使用子进程运行clash验证配置
        let output = app_handle
            .shell()
            .sidecar(clash_core)?
            .args(["-t", "-d", test_dir, "-f", config_path])
            .output()
            .await?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        // 检查进程退出状态和错误输出
        let error_keywords = ["FATA", "fatal", "Parse config error", "level=fatal"];
        let has_error = !output.status.success() || error_keywords.iter().any(|&kw| stderr.contains(kw));

        if !stderr.is_empty() {
            log::debug!(target: "app", "validate stderr: {}", stderr.trim());
        }

        if has_error {
            let error_msg = if !stdout.is_empty() {
                stdout.to_string()
            } else if !stderr.is_empty() {
                stderr.to_string()
            } else if let Some(code) = output.status.code() {
                format!("验证进程异常退出，退出码: {}", code)
            } else {
                "验证进程被终止".to_string()
            };

            log::warn!(target: "app", "config validation failed: {}", error_msg.trim());
            Ok((false, error_msg))
        } else {
            log::debug!(target: "app", "config validation passed");
            Ok((true, String::new()))
        }
    }

    /// 验证运行时配置
    pub async fn validate_config(&self) -> Result<(bool, String)> {
        let config_path = Config::generate_file(ConfigType::Check)?;
        let config_path = dirs::path_to_str(&config_path)?;
        self.validate_config_internal(config_path).await
    }

    /// 验证指定的配置文件
    pub async fn validate_config_file(&self, config_path: &str) -> Result<(bool, String)> {
        // 检查文件是否存在
        if !std::path::Path::new(config_path).exists() {
            let error_msg = format!("File not found: {}", config_path);
            //handle::Handle::notice_message("config_validate::file_not_found", &error_msg);
            return Ok((false, error_msg));
        }
        
        // 检查是否为脚本文件
        let is_script = if config_path.ends_with(".js") {
            true
        } else {
            match self.is_script_file(config_path) {
                Ok(result) => result,
                Err(err) => {
                    // 如果无法确定文件类型，尝试使用Clash内核验证
                    log::warn!(target: "app", "无法确定文件类型: {}, 错误: {}", config_path, err);
                    return self.validate_config_internal(config_path).await;
                }
            }
        };
        
        if is_script {
            log::info!(target: "app", "检测到脚本文件，使用JavaScript验证: {}", config_path);
            return self.validate_script_file(config_path).await;
        }
        
        // 对YAML配置文件使用Clash内核验证
        log::info!(target: "app", "使用Clash内核验证配置文件: {}", config_path);
        self.validate_config_internal(config_path).await
    }

    /// 检查文件是否为脚本文件
    fn is_script_file(&self, path: &str) -> Result<bool> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                log::warn!(target: "app", "无法读取文件以检测类型: {}, 错误: {}", path, err);
                return Err(anyhow::anyhow!("Failed to read file to detect type: {}", err));
            }
        };
        
        // 检查文件前几行是否包含JavaScript特征
        let first_lines = content.lines().take(5).collect::<String>();
        Ok(first_lines.contains("function") || 
           first_lines.contains("//") || 
           first_lines.contains("/*") ||
           first_lines.contains("import") ||
           first_lines.contains("export") ||
           first_lines.contains("const ") ||
           first_lines.contains("let "))
    }

    /// 验证脚本文件语法
    async fn validate_script_file(&self, path: &str) -> Result<(bool, String)> {
        // 读取脚本内容
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                let error_msg = format!("Failed to read script file: {}", err);
                //handle::Handle::notice_message("config_validate::script_error", &error_msg);
                return Ok((false, error_msg));
            }
        };
        
        log::debug!(target: "app", "验证脚本文件: {}", path);
        
        // 使用boa引擎进行基本语法检查
        use boa_engine::{Context, Source};
        
        let mut context = Context::default();
        let result = context.eval(Source::from_bytes(&content));
        
        match result {
            Ok(_) => {
                log::debug!(target: "app", "脚本语法验证通过: {}", path);
                
                // 检查脚本是否包含main函数
                if !content.contains("function main") && !content.contains("const main") && !content.contains("let main") {
                    let error_msg = "Script must contain a main function";
                    log::warn!(target: "app", "脚本缺少main函数: {}", path);
                    //handle::Handle::notice_message("config_validate::script_missing_main", error_msg);
                    return Ok((false, error_msg.to_string()));
                }
                
                Ok((true, String::new()))
            },
            Err(err) => {
                let error_msg = format!("Script syntax error: {}", err);
                log::warn!(target: "app", "脚本语法错误: {}", err);
                //handle::Handle::notice_message("config_validate::script_syntax_error", &error_msg);
                Ok((false, error_msg))
            }
        }
    }

    /// 更新proxies等配置
    pub async fn update_config(&self) -> Result<(bool, String)> {
        log::debug!(target: "app", "updating config");

        // 1. 先生成新的配置内容
        Config::generate().await?;

        // 2. 生成临时文件并进行验证
        Config::generate_file(ConfigType::Check)?;

        // 3. 验证配置
        match self.validate_config().await {
            Ok((true, _)) => {
                // 4. 验证通过后，生成正式的运行时配置
                let run_path = Config::generate_file(ConfigType::Run)?;
                let run_path = dirs::path_to_str(&run_path)?;

                // 5. 应用新配置
                for i in 0..3 {
                    match clash_api::put_configs(run_path).await {
                        Ok(_) => {
                            log::debug!(target: "app", "config applied successfully");
                            Config::runtime().apply();
                            return Ok((true, String::new()));
                        }
                        Err(err) => {
                            if i < 2 {
                                log::info!(target: "app", "retrying config apply: {err}");
                                sleep(Duration::from_millis(100)).await;
                            } else {
                                log::warn!(target: "app", "config apply failed: {}", err);
                                Config::runtime().discard();
                                return Ok((false, err.to_string()));
                            }
                        }
                    }
                }
                Ok((true, String::new()))
            }
            Ok((false, error_msg)) => {
                log::warn!(target: "app", "config validation failed: {}", error_msg.trim());
                Config::runtime().discard();
                Ok((false, error_msg))
            }
            Err(e) => {
                log::error!(target: "app", "config validation error: {}", e);
                Config::runtime().discard();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    
    async fn create_test_script() -> Result<String> {
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_script.js");
        let script_content = r#"
        // This is a test script
        function main(config) {
            console.log("Testing script");
            return config;
        }
        "#;
        
        fs::write(&script_path, script_content)?;
        Ok(script_path.to_string_lossy().to_string())
    }
    
    async fn create_invalid_script() -> Result<String> {
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("invalid_script.js");
        let script_content = r#"
        // This is an invalid script
        function main(config {  // Missing closing parenthesis
            console.log("Testing script");
            return config;
        }
        "#;
        
        fs::write(&script_path, script_content)?;
        Ok(script_path.to_string_lossy().to_string())
    }
    
    async fn create_no_main_script() -> Result<String> {
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("no_main_script.js");
        let script_content = r#"
        // This script has no main function
        function helper(config) {
            console.log("Testing script");
            return config;
        }
        "#;
        
        fs::write(&script_path, script_content)?;
        Ok(script_path.to_string_lossy().to_string())
    }
    
    #[tokio::test]
    async fn test_validate_script_file() -> Result<()> {
        let core_manager = CoreManager::global();
        
        // 测试有效脚本
        let script_path = create_test_script().await?;
        let result = core_manager.validate_config_file(&script_path).await?;
        assert!(result.0, "有效脚本应该通过验证");
        
        // 测试无效脚本
        let invalid_script_path = create_invalid_script().await?;
        let result = core_manager.validate_config_file(&invalid_script_path).await?;
        assert!(!result.0, "无效脚本不应该通过验证");
        assert!(result.1.contains("脚本语法错误"), "无效脚本应该返回语法错误");
        
        // 测试缺少main函数的脚本
        let no_main_script_path = create_no_main_script().await?;
        let result = core_manager.validate_config_file(&no_main_script_path).await?;
        assert!(!result.0, "缺少main函数的脚本不应该通过验证");
        assert!(result.1.contains("缺少main函数"), "应该提示缺少main函数");
        
        // 清理测试文件
        let _ = fs::remove_file(script_path);
        let _ = fs::remove_file(invalid_script_path);
        let _ = fs::remove_file(no_main_script_path);
        
        Ok(())
    }
}
