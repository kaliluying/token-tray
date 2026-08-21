use std::collections::BTreeMap;
use std::env;
use std::fs::{self, create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

const CONFIG_FILE_NAME: &str = "balance.json";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const BALANCE_CACHE_TTL: Duration = Duration::from_secs(5);

const SAMPLE_CONFIG: &str = r#"{
  "name": "PhotonMark",
  "apiKey": "",
  "request": {
    "url": "https://codex.photonmark.com/api/v1/services/pay/status",
    "method": "GET",
    "headers": {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  "extractor": {
    "path": "balance_usd",
    "unit": "USD"
  }
}
"#;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceSnapshot {
    pub configured: bool,
    pub name: String,
    pub remaining: Option<f64>,
    pub unit: String,
    pub updated_at: Option<i64>,
    pub config_path: String,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct BalanceStore {
    cache: Arc<Mutex<Option<CachedBalance>>>,
}

struct CachedBalance {
    fetched_at: Instant,
    snapshot: BalanceSnapshot,
}

impl BalanceStore {
    fn get_or_fetch<F>(&self, fetch: F) -> BalanceSnapshot
    where
        F: FnOnce() -> BalanceSnapshot,
    {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.as_ref() {
            if cached.fetched_at.elapsed() < BALANCE_CACHE_TTL {
                return cached.snapshot.clone();
            }
        }

        let snapshot = fetch();
        *cache = Some(CachedBalance {
            fetched_at: Instant::now(),
            snapshot: snapshot.clone(),
        });
        snapshot
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BalanceConfig {
    #[serde(default = "default_balance_name")]
    name: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    api_key_env: String,
    request: BalanceRequest,
    extractor: BalanceExtractor,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BalanceRequest {
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BalanceExtractor {
    JsonPath { path: String, unit: String },
    CcSwitchScript(String),
}

fn default_balance_name() -> String {
    "自定义余额".to_string()
}

fn default_method() -> String {
    "GET".to_string()
}

#[tauri::command]
pub async fn get_balance(app: AppHandle) -> BalanceSnapshot {
    let store = app.state::<BalanceStore>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.get_or_fetch(|| read_balance_snapshot(&app)))
        .await
        .unwrap_or_else(|_| BalanceSnapshot {
            configured: true,
            name: "余额查询".to_string(),
            remaining: None,
            unit: String::new(),
            updated_at: None,
            config_path: String::new(),
            error: Some("余额查询线程失败".to_string()),
        })
}

fn read_balance_snapshot(app: &AppHandle) -> BalanceSnapshot {
    let path = match balance_config_path(app) {
        Ok(path) => path,
        Err(error) => {
            return BalanceSnapshot {
                configured: false,
                name: default_balance_name(),
                remaining: None,
                unit: String::new(),
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

    match read_and_fetch(&path) {
        Ok((name, remaining, unit)) => BalanceSnapshot {
            configured: true,
            name,
            remaining: Some(remaining),
            unit,
            updated_at: Some(now_millis()),
            config_path,
            error: None,
        },
        Err(error) => BalanceSnapshot {
            configured: true,
            name: "余额查询".to_string(),
            remaining: None,
            unit: String::new(),
            updated_at: None,
            config_path,
            error: Some(error),
        },
    }
}

#[tauri::command]
pub fn open_balance_config(app: AppHandle) -> Result<String, String> {
    let path = balance_config_path(&app)?;
    if let Some(parent) = path.parent() {
        create_dir_all(parent).map_err(|_| "无法创建余额配置目录".to_string())?;
    }

    if !path.exists() {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "无法创建余额配置文件".to_string())?;
        file.write_all(SAMPLE_CONFIG.as_bytes())
            .map_err(|_| "无法写入余额配置示例".to_string())?;
    }

    app.opener()
        .open_path(path.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|_| "无法打开余额配置文件".to_string())?;
    Ok(path.to_string_lossy().to_string())
}

fn balance_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join(CONFIG_FILE_NAME))
        .map_err(|_| "无法确定余额配置目录".to_string())
}

fn unconfigured_snapshot(config_path: String) -> BalanceSnapshot {
    BalanceSnapshot {
        configured: false,
        name: default_balance_name(),
        remaining: None,
        unit: String::new(),
        updated_at: None,
        config_path,
        error: None,
    }
}

fn read_and_fetch(path: &Path) -> Result<(String, f64, String), String> {
    let content = fs::read_to_string(path).map_err(|_| "无法读取余额配置文件".to_string())?;
    let config: BalanceConfig =
        serde_json::from_str(&content).map_err(|_| "余额配置文件不是有效 JSON".to_string())?;
    let (extractor_path, unit) = extractor_spec(&config.extractor)?;
    let url = config.request.url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("余额 URL 必须使用 HTTP 或 HTTPS".to_string());
    }

    let api_key = resolve_api_key(&config)?;
    let request_url = substitute_api_key(url, api_key.as_deref())?;
    let method = Method::from_bytes(config.request.method.trim().as_bytes())
        .map_err(|_| "余额请求方法无效".to_string())?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| "无法初始化余额请求".to_string())?;
    let mut request = client.request(method, request_url);

    for (name, value) in &config.request.headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "余额请求头名称无效".to_string())?;
        let header_value = substitute_api_key(value, api_key.as_deref())?;
        let header_value =
            HeaderValue::from_str(&header_value).map_err(|_| "余额请求头值无效".to_string())?;
        request = request.header(header_name, header_value);
    }

    if let Some(body) = &config.request.body {
        request = request.json(body);
    }

    let response = request
        .send()
        .map_err(|_| "余额请求失败，请检查 URL、网络或 API Key".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("余额接口返回 HTTP {}", status.as_u16()));
    }

    let bytes = response
        .bytes()
        .map_err(|_| "无法读取余额接口响应".to_string())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("余额接口响应过大".to_string());
    }
    let response_json: Value =
        serde_json::from_slice(&bytes).map_err(|_| "余额接口返回的不是有效 JSON".to_string())?;
    let remaining = value_at_path(&response_json, &extractor_path)
        .and_then(value_as_number)
        .ok_or_else(|| format!("找不到余额字段：{}", extractor_path))?;
    if !remaining.is_finite() {
        return Err("余额字段不是有限数字".to_string());
    }

    Ok((config.name, remaining, unit))
}

fn resolve_api_key(config: &BalanceConfig) -> Result<Option<String>, String> {
    if !config.api_key_env.trim().is_empty() {
        let key = env::var(config.api_key_env.trim())
            .map_err(|_| "找不到余额 API Key 环境变量".to_string())?;
        return Ok(Some(key));
    }
    if config.api_key.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(config.api_key.clone()))
}

fn substitute_api_key(value: &str, api_key: Option<&str>) -> Result<String, String> {
    if !value.contains("{{apiKey}}") {
        return Ok(value.to_string());
    }
    let Some(api_key) = api_key else {
        return Err("余额配置使用了 {{apiKey}}，但没有配置 API Key".to_string());
    };
    Ok(value.replace("{{apiKey}}", api_key))
}

fn extractor_spec(extractor: &BalanceExtractor) -> Result<(String, String), String> {
    let (path, unit) = match extractor {
        BalanceExtractor::JsonPath { path, unit } => (path.clone(), unit.clone()),
        BalanceExtractor::CcSwitchScript(script) => parse_cc_switch_script(script)?,
    };
    let path = normalize_json_path(&path)?;
    let unit = unit.trim().to_string();
    if unit.is_empty() {
        return Err("余额单位不能为空".to_string());
    }
    Ok((path, unit))
}

fn parse_cc_switch_script(script: &str) -> Result<(String, String), String> {
    let remaining_expression = property_expression(script, "remaining")
        .ok_or_else(|| "extractor 中缺少 remaining 字段".to_string())?;
    let unit =
        property_string(script, "unit").ok_or_else(|| "extractor 中缺少 unit 字段".to_string())?;
    let response_marker = remaining_expression
        .find("response")
        .ok_or_else(|| "remaining 必须从 response 字段读取".to_string())?;
    let path = remaining_expression[response_marker + "response".len()..].trim();
    Ok((normalize_json_path(path)?, unit))
}

fn property_expression(source: &str, property: &str) -> Option<String> {
    let key_end = find_property_colon(source, property)?;
    let value = source[key_end + 1..].trim_start();
    let end = value.find([',', '}']).unwrap_or(value.len());
    Some(value[..end].trim().to_string())
}

fn property_string(source: &str, property: &str) -> Option<String> {
    let key_end = find_property_colon(source, property)?;
    let value = source[key_end + 1..].trim_start();
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value = &value[quote.len_utf8()..];
    let end = value.find(quote)?;
    Some(value[..end].to_string())
}

fn find_property_colon(source: &str, property: &str) -> Option<usize> {
    for key in [
        format!("\"{}\"", property),
        format!("'{}'", property),
        property.to_string(),
    ] {
        let mut offset = 0;
        while let Some(found) = source[offset..].find(&key) {
            let start = offset + found;
            let after = source[start + key.len()..].trim_start();
            if after.starts_with(':') {
                return Some(start + key.len() + source[start + key.len()..].len() - after.len());
            }
            offset = start + key.len();
        }
    }
    None
}

fn normalize_json_path(path: &str) -> Result<String, String> {
    let mut normalized = path.trim().trim_start_matches('+').trim().to_string();
    if let Some(rest) = normalized.strip_prefix("response") {
        normalized = rest.trim().to_string();
    }
    normalized = normalized
        .replace("[\"", ".")
        .replace("['", ".")
        .replace("\"]", "")
        .replace("']", "")
        .trim_start_matches('.')
        .to_string();
    let valid = !normalized.is_empty()
        && normalized.split('.').all(|part| {
            !part.is_empty()
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
        });
    if !valid {
        return Err("余额字段路径无效".to_string());
    }
    Ok(normalized)
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, part| current.get(part))
}

fn value_as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::{
        extractor_spec, normalize_json_path, parse_cc_switch_script, read_and_fetch,
        value_as_number, value_at_path, BalanceExtractor,
    };
    use serde_json::json;

    #[test]
    fn parses_the_cc_switch_extractor_shape() {
        let script =
            r#"function(response) { return { remaining: +response.balance_usd, unit: "USD" }; }"#;
        let result = parse_cc_switch_script(script).expect("parse extractor");
        assert_eq!(result, ("balance_usd".to_string(), "USD".to_string()));
    }

    #[test]
    fn supports_nested_json_paths() {
        let path = normalize_json_path("response.data.balance").expect("normalize path");
        let value = json!({ "data": { "balance": "12.50" } });
        assert_eq!(
            value_at_path(&value, &path).and_then(value_as_number),
            Some(12.5)
        );
    }

    #[test]
    fn accepts_both_path_and_script_extractors() {
        let path = BalanceExtractor::JsonPath {
            path: "balance_usd".to_string(),
            unit: "USD".to_string(),
        };
        assert_eq!(
            extractor_spec(&path).expect("path extractor"),
            ("balance_usd".to_string(), "USD".to_string())
        );
    }

    #[test]
    fn fetches_a_cc_switch_style_balance_from_a_local_fixture() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let address = listener.local_addr().expect("fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture request");
            let mut request = [0_u8; 4096];
            let size = stream.read(&mut request).expect("read fixture request");
            let request = String::from_utf8_lossy(&request[..size]).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer test-key"));
            let body = r#"{"balance_usd":"12.50"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture response");
        });

        let path = std::env::temp_dir().join(format!(
            "token-tray-balance-test-{}-{}.json",
            std::process::id(),
            super::now_millis()
        ));
        let config = json!({
            "name": "Local fixture",
            "apiKey": "test-key",
            "request": {
                "url": format!("http://{address}/status"),
                "method": "GET",
                "headers": {"Authorization": "Bearer {{apiKey}}"}
            },
            "extractor": "function(response) { return { remaining: +response.balance_usd, unit: \"USD\" }; }"
        });
        fs::write(
            &path,
            serde_json::to_vec(&config).expect("serialize fixture config"),
        )
        .expect("write fixture config");

        let result = read_and_fetch(&path).expect("fetch fixture balance");
        server.join().expect("fixture server");
        fs::remove_file(path).expect("remove fixture config");
        assert_eq!(
            result,
            ("Local fixture".to_string(), 12.5, "USD".to_string())
        );
    }

    #[test]
    fn coalesces_concurrent_balance_fetches() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        use std::thread;
        use std::time::Duration;

        let store = super::BalanceStore::default();
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let first_store = store.clone();
        let first_count = fetch_count.clone();
        let first_barrier = barrier.clone();
        let first = thread::spawn(move || {
            first_barrier.wait();
            first_store.get_or_fetch(|| {
                first_count.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(25));
                test_balance_snapshot(1.0)
            })
        });

        let second_store = store.clone();
        let second_count = fetch_count.clone();
        let second_barrier = barrier.clone();
        let second = thread::spawn(move || {
            second_barrier.wait();
            second_store.get_or_fetch(|| {
                second_count.fetch_add(1, Ordering::SeqCst);
                test_balance_snapshot(2.0)
            })
        });

        let first_result = first.join().expect("first fetch thread");
        let second_result = second.join().expect("second fetch thread");
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
        assert_eq!(first_result.remaining, second_result.remaining);
    }

    fn test_balance_snapshot(remaining: f64) -> super::BalanceSnapshot {
        super::BalanceSnapshot {
            configured: true,
            name: "Test".to_string(),
            remaining: Some(remaining),
            unit: "USD".to_string(),
            updated_at: Some(1),
            config_path: String::new(),
            error: None,
        }
    }
}
