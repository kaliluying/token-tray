use std::collections::BTreeMap;
use std::env;
use std::fs::{self, create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

const CONFIG_FILE_NAME: &str = "relay.json";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const RELAY_CACHE_TTL: Duration = Duration::from_secs(5);

const SAMPLE_CONFIG: &str = r#"{
  "name": "PhotonMark",
  "request": {
    "url": "https://codex.photonmark.com/api/v1/services/{{service}}/status",
    "method": "GET",
    "headers": {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  "services": [
    { "id": "pay", "name": "Pay", "apiKey": "" },
    { "id": "boost", "name": "Boost", "apiKey": "" }
  ]
}
"#;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayUsageSnapshot {
    pub configured: bool,
    pub name: String,
    pub services: Vec<RelayServiceSnapshot>,
    pub updated_at: Option<i64>,
    pub config_path: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayServiceSnapshot {
    pub id: String,
    pub name: String,
    pub status: String,
    pub active: Option<bool>,
    pub balance_usd: Option<f64>,
    pub windows: BTreeMap<String, RelayUsageWindow>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayUsageWindow {
    pub requests: u64,
    pub token_requests: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub amount_usd: Option<f64>,
}

#[derive(Clone, Default)]
pub struct RelayUsageStore {
    cache: Arc<Mutex<Option<CachedRelayUsage>>>,
}

struct CachedRelayUsage {
    fetched_at: Instant,
    snapshot: RelayUsageSnapshot,
}

impl RelayUsageStore {
    fn get_or_fetch<F>(&self, fetch: F) -> RelayUsageSnapshot
    where
        F: FnOnce() -> RelayUsageSnapshot,
    {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.as_ref() {
            if cached.fetched_at.elapsed() < RELAY_CACHE_TTL {
                return cached.snapshot.clone();
            }
        }

        let snapshot = fetch();
        *cache = Some(CachedRelayUsage {
            fetched_at: Instant::now(),
            snapshot: snapshot.clone(),
        });
        snapshot
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayConfig {
    #[serde(default = "default_relay_name")]
    name: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    api_key_env: String,
    request: RelayRequest,
    #[serde(default = "default_services")]
    services: Vec<RelayServiceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayRequest {
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayServiceConfig {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    api_key_env: String,
}

fn default_relay_name() -> String {
    "中转站".to_string()
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_services() -> Vec<RelayServiceConfig> {
    vec![
        RelayServiceConfig {
            id: "pay".to_string(),
            name: "Pay".to_string(),
            api_key: String::new(),
            api_key_env: String::new(),
        },
        RelayServiceConfig {
            id: "boost".to_string(),
            name: "Boost".to_string(),
            api_key: String::new(),
            api_key_env: String::new(),
        },
    ]
}

#[tauri::command]
pub async fn get_relay_usage(app: AppHandle) -> RelayUsageSnapshot {
    let store = app.state::<RelayUsageStore>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.get_or_fetch(|| read_relay_snapshot(&app)))
        .await
        .unwrap_or_else(|_| RelayUsageSnapshot {
            configured: true,
            name: default_relay_name(),
            services: Vec::new(),
            updated_at: None,
            config_path: String::new(),
            error: Some("中转站查询线程失败".to_string()),
        })
}

#[tauri::command]
pub fn open_relay_config(app: AppHandle) -> Result<String, String> {
    let path = relay_config_path(&app)?;
    if let Some(parent) = path.parent() {
        create_dir_all(parent).map_err(|_| "无法创建中转站配置目录".to_string())?;
    }

    if !path.exists() {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "无法创建中转站配置文件".to_string())?;
        file.write_all(SAMPLE_CONFIG.as_bytes())
            .map_err(|_| "无法写入中转站配置示例".to_string())?;
    }

    app.opener()
        .open_path(path.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|_| "无法打开中转站配置文件".to_string())?;
    Ok(path.to_string_lossy().to_string())
}

fn relay_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join(CONFIG_FILE_NAME))
        .map_err(|_| "无法确定中转站配置目录".to_string())
}

fn unconfigured_snapshot(config_path: String) -> RelayUsageSnapshot {
    RelayUsageSnapshot {
        configured: false,
        name: default_relay_name(),
        services: Vec::new(),
        updated_at: None,
        config_path,
        error: None,
    }
}

fn read_relay_snapshot(app: &AppHandle) -> RelayUsageSnapshot {
    let path = match relay_config_path(app) {
        Ok(path) => path,
        Err(error) => {
            return RelayUsageSnapshot {
                configured: false,
                name: default_relay_name(),
                services: Vec::new(),
                updated_at: None,
                config_path: String::new(),
                error: Some(error),
            };
        }
    };
    let config_path = path.to_string_lossy().to_string();

    if !path.is_file() {
        return unconfigured_snapshot(config_path);
    }

    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return configured_error(config_path, "无法读取中转站配置文件"),
    };
    let config: RelayConfig = match serde_json::from_str(&content) {
        Ok(config) => config,
        Err(_) => return configured_error(config_path, "中转站配置文件不是有效 JSON"),
    };
    if config.services.is_empty() {
        return configured_error(config_path, "中转站配置至少需要一个服务");
    }

    let client = match Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(client) => client,
        Err(_) => return configured_error(config_path, "无法初始化中转站请求"),
    };

    let services = config
        .services
        .iter()
        .map(|service| {
            fetch_service(&client, &config, service)
                .unwrap_or_else(|error| service_error(service, error))
        })
        .collect::<Vec<_>>();
    let error = if services.iter().all(|service| service.error.is_some()) {
        Some("pay/boost 均无法读取，请检查 URL、网络或 API Key".to_string())
    } else {
        None
    };

    RelayUsageSnapshot {
        configured: true,
        name: config.name,
        services,
        updated_at: Some(now_millis()),
        config_path,
        error,
    }
}

fn configured_error(config_path: String, error: &str) -> RelayUsageSnapshot {
    RelayUsageSnapshot {
        configured: true,
        name: default_relay_name(),
        services: Vec::new(),
        updated_at: None,
        config_path,
        error: Some(error.to_string()),
    }
}

fn fetch_service(
    client: &Client,
    config: &RelayConfig,
    service: &RelayServiceConfig,
) -> Result<RelayServiceSnapshot, String> {
    let api_key = resolve_api_key(config, service)?;
    let url = substitute_template(&config.request.url, api_key.as_deref(), &service.id)?;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("中转站 URL 必须使用 HTTP 或 HTTPS".to_string());
    }

    let method = Method::from_bytes(config.request.method.trim().as_bytes())
        .map_err(|_| "中转站请求方法无效".to_string())?;
    let mut request = client.request(method, url);
    for (name, value) in &config.request.headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "中转站请求头名称无效".to_string())?;
        let header_value = substitute_template(value, api_key.as_deref(), &service.id)?;
        let header_value =
            HeaderValue::from_str(&header_value).map_err(|_| "中转站请求头值无效".to_string())?;
        request = request.header(header_name, header_value);
    }
    if let Some(body) = &config.request.body {
        request = request.json(body);
    }

    let response = request
        .send()
        .map_err(|_| "中转站请求失败，请检查 URL、网络或 API Key".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("中转站接口返回 HTTP {}", status.as_u16()));
    }

    let bytes = response
        .bytes()
        .map_err(|_| "无法读取中转站接口响应".to_string())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("中转站接口响应过大".to_string());
    }
    let response_json: Value =
        serde_json::from_slice(&bytes).map_err(|_| "中转站接口返回的不是有效 JSON".to_string())?;
    parse_service_response(&response_json, service)
}

fn parse_service_response(
    response: &Value,
    config: &RelayServiceConfig,
) -> Result<RelayServiceSnapshot, String> {
    let usage = response
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(|| "中转站响应缺少 usage 字段".to_string())?;
    let windows = usage
        .iter()
        .map(|(period, value)| (period.clone(), parse_usage_window(value)))
        .collect();
    let status = response
        .get("status")
        .and_then(value_as_string)
        .unwrap_or_else(|| "unknown".to_string());
    let active = response
        .get("active")
        .and_then(Value::as_bool)
        .or_else(|| Some(status == "active"));

    Ok(RelayServiceSnapshot {
        id: response
            .get("service")
            .and_then(value_as_string)
            .unwrap_or_else(|| config.id.clone()),
        name: response
            .get("service_name")
            .and_then(value_as_string)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| service_name(config)),
        status,
        active,
        balance_usd: response.get("balance_usd").and_then(value_as_f64),
        windows,
        error: None,
    })
}

fn parse_usage_window(value: &Value) -> RelayUsageWindow {
    let Some(object) = value.as_object() else {
        return RelayUsageWindow::default();
    };
    RelayUsageWindow {
        requests: object_value_as_u64(object, "requests"),
        token_requests: object_value_as_u64(object, "token_requests"),
        input_tokens: object_value_as_u64(object, "input_tokens"),
        cached_input_tokens: object_value_as_u64(object, "cached_input_tokens"),
        output_tokens: object_value_as_u64(object, "output_tokens"),
        total_tokens: object_value_as_u64(object, "total_tokens"),
        amount_usd: object.get("amount_usd").and_then(value_as_f64),
    }
}

fn object_value_as_u64(object: &serde_json::Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(value_as_u64).unwrap_or_default()
}

fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value as u64)
        }),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

fn service_name(config: &RelayServiceConfig) -> String {
    if config.name.trim().is_empty() {
        config.id.clone()
    } else {
        config.name.clone()
    }
}

fn service_error(config: &RelayServiceConfig, error: String) -> RelayServiceSnapshot {
    RelayServiceSnapshot {
        id: config.id.clone(),
        name: service_name(config),
        status: "error".to_string(),
        active: None,
        balance_usd: None,
        windows: BTreeMap::new(),
        error: Some(error),
    }
}

fn resolve_api_key(
    config: &RelayConfig,
    service: &RelayServiceConfig,
) -> Result<Option<String>, String> {
    if !service.api_key_env.trim().is_empty() {
        let key = env::var(service.api_key_env.trim())
            .map_err(|_| format!("找不到 {} API Key 环境变量", service_name(service)))?;
        return Ok(Some(key));
    }
    if !service.api_key.trim().is_empty() {
        return Ok(Some(service.api_key.clone()));
    }
    if !config.api_key_env.trim().is_empty() {
        let key = env::var(config.api_key_env.trim())
            .map_err(|_| "找不到中转站 API Key 环境变量".to_string())?;
        return Ok(Some(key));
    }
    if config.api_key.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(config.api_key.clone()))
}

fn substitute_template(
    value: &str,
    api_key: Option<&str>,
    service: &str,
) -> Result<String, String> {
    let mut result = value.to_string();
    if result.contains("{{apiKey}}") {
        let Some(api_key) = api_key else {
            return Err("中转站配置使用了 {{apiKey}}，但没有配置 API Key".to_string());
        };
        result = result.replace("{{apiKey}}", api_key);
    }
    Ok(result.replace("{{service}}", service))
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        parse_service_response, resolve_api_key, substitute_template, RelayConfig, RelayRequest,
        RelayServiceConfig,
    };
    use serde_json::json;

    #[test]
    fn parses_boost_usage_windows_and_balance() {
        let response = json!({
            "service": "boost",
            "service_name": "Codex 充值 (Boost)",
            "status": "active",
            "active": true,
            "usage": {
                "5h": { "requests": 239, "total_tokens": 18550689 },
                "all": {
                    "requests": 44635,
                    "total_tokens": 2748780334_u64,
                    "amount_usd": "60.4609"
                }
            },
            "balance_usd": "31.5391"
        });
        let service = RelayServiceConfig {
            id: "boost".to_string(),
            name: "Boost".to_string(),
            api_key: String::new(),
            api_key_env: String::new(),
        };

        let result = parse_service_response(&response, &service).expect("parse relay response");
        assert_eq!(result.id, "boost");
        assert_eq!(result.balance_usd, Some(31.5391));
        assert_eq!(result.windows["5h"].total_tokens, 18_550_689);
        assert_eq!(result.windows["all"].requests, 44_635);
        assert_eq!(result.windows["all"].amount_usd, Some(60.4609));
    }

    #[test]
    fn replaces_api_key_and_service_placeholders() {
        let value = substitute_template(
            "https://example.test/{{service}}?key={{apiKey}}",
            Some("test-key"),
            "boost",
        )
        .expect("replace template");
        assert_eq!(value, "https://example.test/boost?key=test-key");
    }

    #[test]
    fn prefers_service_api_key_over_global_fallback() {
        let config = RelayConfig {
            name: "Relay".to_string(),
            api_key: "global-key".to_string(),
            api_key_env: String::new(),
            request: RelayRequest {
                url: "https://example.test/{{service}}".to_string(),
                method: "GET".to_string(),
                headers: BTreeMap::new(),
                body: None,
            },
            services: Vec::new(),
        };
        let service = RelayServiceConfig {
            id: "boost".to_string(),
            name: "Boost".to_string(),
            api_key: "boost-key".to_string(),
            api_key_env: String::new(),
        };

        assert_eq!(
            resolve_api_key(&config, &service).expect("resolve service key"),
            Some("boost-key".to_string())
        );
    }
}
