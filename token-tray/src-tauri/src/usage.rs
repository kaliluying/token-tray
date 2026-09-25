use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Emitter, Runtime, State};
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime, UtcOffset};

use crate::diagnostics;

const CLAUDE_CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";
const CODEX_HOME_ENV: &str = "CODEX_HOME";

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUsage {
    pub app_type: String,
    pub total_tokens: i64,
    pub requests: i64,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub total_tokens: i64,
    pub requests: i64,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub today: TokenTotals,
    pub month: TokenTotals,
    pub total: TokenTotals,
    pub last_seven_days: TokenTotals,
    pub daily: Vec<DailyUsage>,
    pub by_app: Vec<AppUsage>,
    pub updated_at: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageUpdate {
    pub snapshot: UsageSnapshot,
    pub last_synced_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Success,
    LocalFilesNotFound,
    LocalFilesUnavailable,
    UnsupportedFormat,
    ReadFailed,
}

impl SyncStatus {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::Success => "",
            Self::LocalFilesNotFound => "未找到本地会话文件",
            Self::LocalFilesUnavailable => "暂时无法读取本地会话文件",
            Self::UnsupportedFormat => "本地会话文件格式暂不兼容",
            Self::ReadFailed => "读取本地 Token 统计失败",
        }
    }

    pub fn diagnostic_result(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::LocalFilesNotFound => "local_files_not_found",
            Self::LocalFilesUnavailable => "local_files_unavailable",
            Self::UnsupportedFormat => "unsupported_format",
            Self::ReadFailed => "read_failed",
        }
    }
}

pub struct SyncResult {
    pub update: UsageUpdate,
    pub status: SyncStatus,
}

#[derive(Clone, Default)]
pub struct UsageStore {
    inner: Arc<UsageStoreInner>,
}

#[derive(Default)]
struct UsageStoreInner {
    cache: Mutex<UsageCache>,
    sync_lock: Mutex<()>,
    file_cache: Mutex<HashMap<PathBuf, CachedFile>>,
}

#[derive(Default)]
struct UsageCache {
    snapshot: UsageSnapshot,
    last_synced_at: Option<i64>,
    error: Option<String>,
}

#[derive(Clone)]
struct CachedFile {
    signature: FileSignature,
    rows: Vec<DailyAppRow>,
}

#[derive(Clone, PartialEq, Eq)]
struct FileSignature {
    length: u64,
    modified: Option<SystemTime>,
}

impl FileSignature {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

impl UsageStore {
    pub fn current(&self) -> UsageUpdate {
        let cache = self
            .inner
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        UsageUpdate {
            snapshot: cache.snapshot.clone(),
            last_synced_at: cache.last_synced_at,
            error: cache.error.clone(),
        }
    }

    pub fn sync_once(&self) -> SyncResult {
        let _sync_guard = self
            .inner
            .sync_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (source_files, discovery_unavailable) = discover_source_files();
        self.sync_sources(source_files, discovery_unavailable)
    }

    fn sync_sources(
        &self,
        source_files: Vec<SourceFile>,
        discovery_unavailable: bool,
    ) -> SyncResult {
        let source_file_count = source_files.len();
        let mut cached_files = self
            .inner
            .file_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next_cache = HashMap::with_capacity(source_files.len());
        let mut accumulator = UsageAccumulator::default();
        let mut readable_files = 0;
        let mut saw_unavailable = discovery_unavailable;
        let mut saw_unsupported = false;

        for source in source_files {
            let metadata = match fs::metadata(&source.path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => {
                    saw_unavailable = true;
                    continue;
                }
                Err(_) => {
                    saw_unavailable = true;
                    continue;
                }
            };
            let signature = FileSignature::from_metadata(&metadata);
            let rows = match cached_files.remove(&source.path) {
                Some(cached) if cached.signature == signature => Some(cached.rows),
                _ => match read_source_file(&source) {
                    Ok(rows) => Some(rows),
                    Err(SyncStatus::UnsupportedFormat) => {
                        saw_unsupported = true;
                        None
                    }
                    Err(_) => {
                        saw_unavailable = true;
                        None
                    }
                },
            };

            let Some(rows) = rows else {
                continue;
            };
            readable_files += 1;
            for row in &rows {
                accumulator.add(row);
            }
            next_cache.insert(source.path, CachedFile { signature, rows });
        }
        *cached_files = next_cache;
        drop(cached_files);

        let result = if saw_unavailable {
            Err(SyncStatus::LocalFilesUnavailable)
        } else if saw_unsupported {
            Err(SyncStatus::UnsupportedFormat)
        } else if readable_files > 0 {
            Ok(accumulator.finish())
        } else if source_file_count == 0 {
            Err(SyncStatus::LocalFilesNotFound)
        } else {
            Err(SyncStatus::ReadFailed)
        };

        let (status, snapshot) = match result {
            Ok(snapshot) => (SyncStatus::Success, Some(snapshot)),
            Err(status) => (status, None),
        };
        let mut cache = self
            .inner
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match snapshot {
            Some(snapshot) => {
                cache.snapshot = snapshot;
                cache.last_synced_at = Some(now_millis());
                cache.error = None;
            }
            None => {
                cache.error = Some(status.user_message().to_string());
            }
        }

        SyncResult {
            update: UsageUpdate {
                snapshot: cache.snapshot.clone(),
                last_synced_at: cache.last_synced_at,
                error: cache.error.clone(),
            },
            status,
        }
    }
}

#[tauri::command]
pub fn get_usage_snapshot(state: State<'_, UsageStore>) -> UsageUpdate {
    state.current()
}

#[tauri::command]
pub fn sync_usage_now(app: AppHandle, state: State<'_, UsageStore>) -> UsageUpdate {
    let mut last_status = None;
    let result = state.sync_once();
    if let Some(tray) = app.tray_by_id("token-tray") {
        publish_sync_result(&app, &tray, &result, &mut last_status);
    } else {
        let _ = app.emit("usage-updated", &result.update);
    }
    result.update
}

pub fn start_sync_worker<R: Runtime + 'static>(
    app: AppHandle<R>,
    tray: TrayIcon<R>,
    store: UsageStore,
) {
    let _ = std::thread::Builder::new()
        .name("token-tray-usage-sync".to_string())
        .spawn(move || {
            let mut last_status = None;
            loop {
                let result = store.sync_once();
                publish_sync_result(&app, &tray, &result, &mut last_status);
                std::thread::sleep(Duration::from_secs(5));
            }
        });
}

fn publish_sync_result<R: Runtime>(
    app: &AppHandle<R>,
    tray: &TrayIcon<R>,
    result: &SyncResult,
    last_status: &mut Option<SyncStatus>,
) {
    update_tray(tray, &result.update);
    if let Err(_error) = app.emit("usage-updated", &result.update) {
        diagnostics::record(app, "usage_event", "emit_failed");
    }
    if *last_status != Some(result.status) {
        diagnostics::record(app, "usage_sync", result.status.diagnostic_result());
        *last_status = Some(result.status);
    }
}

fn update_tray<R: Runtime>(tray: &TrayIcon<R>, update: &UsageUpdate) {
    let status = if update.error.is_some() {
        "同步失败"
    } else if update.last_synced_at.is_some() {
        "已同步"
    } else {
        "等待首次同步"
    };
    let synced_at = update
        .last_synced_at
        .map(format_sync_time)
        .unwrap_or_else(|| "等待首次同步".to_string());
    let tooltip = format!(
        "今日 token：{}\n最近同步：{}\n状态：{}",
        format_tokens(update.snapshot.today.total_tokens),
        synced_at,
        status
    );
    let _ = tray.set_tooltip(Some(tooltip));

    #[cfg(target_os = "macos")]
    {
        let _ = tray.set_title(Some(format_tokens(update.snapshot.today.total_tokens)));
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn format_sync_time(timestamp: i64) -> String {
    let seconds = timestamp.div_euclid(1_000);
    let Ok(value) = OffsetDateTime::from_unix_timestamp(seconds) else {
        return "未知".to_string();
    };
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    value
        .to_offset(offset)
        .format(&Rfc3339)
        .map(|formatted| {
            formatted
                .replace('T', " ")
                .trim_end_matches('Z')
                .to_string()
        })
        .unwrap_or_else(|_| "未知".to_string())
}

pub fn format_tokens(value: i64) -> String {
    let digits = value.max(0).to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

#[derive(Clone, Copy)]
enum SourceKind {
    Claude,
    Codex,
}

struct SourceFile {
    path: PathBuf,
    kind: SourceKind,
}

fn discover_source_files() -> (Vec<SourceFile>, bool) {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut unavailable = false;
    for root in claude_roots() {
        unavailable |= collect_jsonl_files(
            &claude_projects_root(&root),
            SourceKind::Claude,
            &mut files,
            &mut seen,
            true,
        );
    }
    if let Some(root) = source_root(CODEX_HOME_ENV, ".codex") {
        unavailable |= collect_jsonl_files(
            &root.join("sessions"),
            SourceKind::Codex,
            &mut files,
            &mut seen,
            true,
        );
        unavailable |= collect_jsonl_files(
            &root.join("archived_sessions"),
            SourceKind::Codex,
            &mut files,
            &mut seen,
            true,
        );
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    (files, unavailable)
}

fn claude_projects_root(root: &Path) -> PathBuf {
    if root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("projects"))
    {
        root.to_path_buf()
    } else {
        root.join("projects")
    }
}

fn claude_roots() -> Vec<PathBuf> {
    if let Some(value) = env::var_os(CLAUDE_CONFIG_ENV).filter(|value| !value.is_empty()) {
        return value
            .to_string_lossy()
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect();
    }

    let Some(home) = home_directory() else {
        return Vec::new();
    };
    vec![home.join(".claude"), home.join(".config").join("claude")]
}

fn source_root(variable: &str, default_directory: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os(variable).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    home_directory().map(|home| home.join(default_directory))
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn collect_jsonl_files(
    directory: &Path,
    kind: SourceKind,
    output: &mut Vec<SourceFile>,
    seen: &mut HashSet<PathBuf>,
    allow_missing: bool,
) -> bool {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
            return false
        }
        Err(_) => return true,
    };
    let mut unavailable = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                unavailable = true;
                continue;
            }
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            unavailable = true;
            continue;
        };
        if file_type.is_dir() {
            unavailable |= collect_jsonl_files(&path, kind, output, seen, false);
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            && seen.insert(path.clone())
        {
            output.push(SourceFile { path, kind });
        }
    }
    unavailable
}

fn read_source_file(source: &SourceFile) -> Result<Vec<DailyAppRow>, SyncStatus> {
    let file = File::open(&source.path).map_err(|_| SyncStatus::LocalFilesUnavailable)?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut claude_records = HashMap::<String, DailyAppRow>::new();
    let mut previous_codex_total = None;
    let mut saw_valid_json = false;

    for (line_number, line) in reader.lines().enumerate() {
        let line = line.map_err(|_| SyncStatus::LocalFilesUnavailable)?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            // The final line may be incomplete while the client is appending it.
            continue;
        };
        saw_valid_json = true;
        let parsed = match source.kind {
            SourceKind::Claude => {
                let fallback = format!("{}:{line_number}", source.path.display());
                parse_claude_record(&value, &fallback)
            }
            SourceKind::Codex => parse_codex_record(&value, &mut previous_codex_total),
        };
        let Some(row) = parsed else {
            continue;
        };
        if let Some(identity) = row.identity.clone() {
            if let Some(existing) = claude_records.get_mut(&identity) {
                merge_claude_rows(existing, row);
            } else {
                claude_records.insert(identity, row);
            }
        } else {
            rows.push(row);
        }
    }

    if !saw_valid_json {
        return Err(SyncStatus::UnsupportedFormat);
    }
    rows.extend(claude_records.into_values());
    Ok(rows)
}

fn parse_claude_record(value: &Value, fallback_identity: &str) -> Option<DailyAppRow> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = value.get("message")?.as_object()?;
    let usage = message.get("usage")?.as_object()?;
    let date = date_from_value(value)?;
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let request_id = value
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let identity = if message_id.is_empty() && request_id.is_empty() {
        fallback_identity.to_string()
    } else {
        format!("{session_id}\u{1f}{message_id}\u{1f}{request_id}")
    };
    let input_tokens = object_number(usage, &["input_tokens"]);
    let output_tokens = object_number(usage, &["output_tokens"]);
    let cache_read_tokens = object_number(usage, &["cache_read_input_tokens", "cache_read_tokens"]);
    let cache_creation_tokens = object_number(usage, &["cache_creation_input_tokens"])
        .max(nested_cache_creation_tokens(usage.get("cache_creation")));
    let total_tokens = sum_tokens(
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    );
    Some(DailyAppRow {
        date,
        app_type: "claude".to_string(),
        totals: TokenTotals {
            requests: 1,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            total_tokens,
        },
        identity: Some(identity),
    })
}

fn parse_codex_record(
    value: &Value,
    previous_total: &mut Option<RawTokenUsage>,
) -> Option<DailyAppRow> {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?.as_object()?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let info = payload.get("info")?.as_object()?;
    let cumulative = info.get("total_token_usage").map(RawTokenUsage::from_value);
    let raw = if let Some(last) = info.get("last_token_usage") {
        if let Some(cumulative) = cumulative {
            *previous_total = Some(cumulative);
        }
        RawTokenUsage::from_value(last)
    } else {
        let cumulative = cumulative?;
        let delta = previous_total
            .map(|previous| cumulative.saturating_sub(previous))
            .unwrap_or(cumulative);
        *previous_total = Some(cumulative);
        delta
    };
    Some(DailyAppRow {
        date: date_from_value(value)?,
        app_type: "codex".to_string(),
        totals: raw.into_totals(),
        identity: None,
    })
}

fn merge_claude_rows(target: &mut DailyAppRow, source: DailyAppRow) {
    if source.date < target.date {
        target.date = source.date;
    }
    target.totals.input_tokens = target.totals.input_tokens.max(source.totals.input_tokens);
    target.totals.output_tokens = target.totals.output_tokens.max(source.totals.output_tokens);
    target.totals.cache_read_tokens = target
        .totals
        .cache_read_tokens
        .max(source.totals.cache_read_tokens);
    target.totals.cache_creation_tokens = target
        .totals
        .cache_creation_tokens
        .max(source.totals.cache_creation_tokens);
    target.totals.requests = 1;
    target.totals.total_tokens = sum_tokens(
        target.totals.input_tokens,
        target.totals.output_tokens,
        target.totals.cache_read_tokens,
        target.totals.cache_creation_tokens,
    );
}

#[derive(Clone, Copy, Default)]
struct RawTokenUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    output_tokens: i64,
}

impl RawTokenUsage {
    fn from_value(value: &Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            input_tokens: object_number(object, &["input_tokens"]),
            cached_input_tokens: object_number(
                object,
                &["cached_input_tokens", "cache_read_input_tokens"],
            ),
            cache_write_input_tokens: object_number(
                object,
                &["cache_write_input_tokens", "cache_creation_input_tokens"],
            ),
            output_tokens: object_number(object, &["output_tokens"]),
        }
    }

    fn saturating_sub(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(other.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(other.cached_input_tokens),
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .saturating_sub(other.cache_write_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(other.output_tokens),
        }
    }

    fn into_totals(self) -> TokenTotals {
        let input_tokens = self.input_tokens.saturating_sub(
            self.cached_input_tokens
                .saturating_add(self.cache_write_input_tokens),
        );
        TokenTotals {
            requests: 1,
            input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cached_input_tokens,
            cache_creation_tokens: self.cache_write_input_tokens,
            total_tokens: sum_tokens(
                input_tokens,
                self.output_tokens,
                self.cached_input_tokens,
                self.cache_write_input_tokens,
            ),
        }
    }
}

fn date_from_value(value: &Value) -> Option<String> {
    let timestamp = value.get("timestamp")?;
    if let Some(timestamp) = timestamp.as_str() {
        let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        if let Ok(value) = OffsetDateTime::parse(timestamp, &Rfc3339) {
            return Some(format_date(value.to_offset(offset).date()));
        }
        let date = timestamp.get(..10)?;
        if date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-') {
            return Some(date.to_string());
        }
    }

    let timestamp = timestamp.as_i64().or_else(|| {
        timestamp
            .as_u64()
            .and_then(|value| i64::try_from(value).ok())
    })?;
    let seconds = if timestamp > 100_000_000_000 {
        timestamp / 1_000
    } else {
        timestamp
    };
    let value = OffsetDateTime::from_unix_timestamp(seconds).ok()?;
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    Some(format_date(value.to_offset(offset).date()))
}

fn object_number(object: &serde_json::Map<String, Value>, names: &[&str]) -> i64 {
    names
        .iter()
        .find_map(|name| value_as_i64(object.get(*name)))
        .unwrap_or_default()
}

fn value_as_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(value) = value.as_i64() {
        return Some(value.max(0));
    }
    if let Some(value) = value.as_u64() {
        return Some(value.min(i64::MAX as u64) as i64);
    }
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.min(i64::MAX as f64) as i64)
        .or_else(|| {
            value
                .as_str()?
                .trim()
                .parse::<i64>()
                .ok()
                .map(|value| value.max(0))
        })
}

fn nested_cache_creation_tokens(value: Option<&Value>) -> i64 {
    let Some(object) = value.and_then(Value::as_object) else {
        return 0;
    };
    object_number(object, &["ephemeral_5m_input_tokens"])
        .saturating_add(object_number(object, &["ephemeral_1h_input_tokens"]))
        .saturating_add(object_number(object, &["input_tokens"]))
}

fn sum_tokens(input: i64, output: i64, cache_read: i64, cache_creation: i64) -> i64 {
    input
        .saturating_add(output)
        .saturating_add(cache_read)
        .saturating_add(cache_creation)
}

#[derive(Default)]
struct UsageAccumulator {
    direct_rows: BTreeMap<(String, String), TokenTotals>,
    claude_records: HashMap<String, DailyAppRow>,
}

impl UsageAccumulator {
    fn add(&mut self, row: &DailyAppRow) {
        if let Some(identity) = row.identity.clone() {
            if let Some(existing) = self.claude_records.get_mut(&identity) {
                merge_claude_rows(existing, row.clone());
            } else {
                self.claude_records.insert(identity, row.clone());
            }
            return;
        }
        add_totals(
            self.direct_rows
                .entry((row.date.clone(), row.app_type.clone()))
                .or_default(),
            &row.totals,
        );
    }

    fn finish(self) -> UsageSnapshot {
        let mut rows = self.direct_rows;
        for row in self.claude_records.into_values() {
            add_totals(
                rows.entry((row.date, row.app_type)).or_default(),
                &row.totals,
            );
        }
        let (today_key, month_key, seven_days_key, dates) = local_date_keys();
        let mut today = TokenTotals::default();
        let mut month = TokenTotals::default();
        let mut last_seven_days = TokenTotals::default();
        let mut total = TokenTotals::default();
        let mut by_date = BTreeMap::<String, TokenTotals>::new();
        let mut by_app = BTreeMap::<String, TokenTotals>::new();
        for ((date, app_type), totals) in rows {
            add_totals(&mut total, &totals);
            add_totals(by_date.entry(date).or_default(), &totals);
            add_totals(by_app.entry(app_type).or_default(), &totals);
        }
        for (date, totals) in &by_date {
            if date >= &month_key {
                add_totals(&mut month, totals);
            }
            if date >= &seven_days_key {
                add_totals(&mut last_seven_days, totals);
            }
            if date == &today_key {
                add_totals(&mut today, totals);
            }
        }

        let daily = dates
            .into_iter()
            .map(|date| {
                let totals = by_date.get(&date).cloned().unwrap_or_default();
                DailyUsage {
                    date,
                    total_tokens: totals.total_tokens,
                    requests: totals.requests,
                }
            })
            .collect();
        let mut by_app = by_app
            .into_iter()
            .map(|(app_type, totals)| AppUsage {
                app_type,
                total_tokens: totals.total_tokens,
                requests: totals.requests,
            })
            .collect::<Vec<_>>();
        by_app.sort_by(|left, right| right.total_tokens.cmp(&left.total_tokens));

        UsageSnapshot {
            today,
            month,
            total,
            last_seven_days,
            daily,
            by_app,
            updated_at: today_key,
            source: "本地会话文件".to_string(),
        }
    }
}

#[derive(Clone)]
struct DailyAppRow {
    date: String,
    app_type: String,
    totals: TokenTotals,
    identity: Option<String>,
}

fn local_date_keys() -> (String, String, String, Vec<String>) {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let today = OffsetDateTime::now_utc().to_offset(offset).date();
    let today_key = format_date(today);
    let month_key = today_key[..7].to_string();
    let seven_days_ago = today.saturating_sub(TimeDuration::days(6));
    let seven_days_key = format_date(seven_days_ago);
    let dates = (0..=6)
        .rev()
        .map(|days_ago| format_date(today.saturating_sub(TimeDuration::days(days_ago))))
        .collect();
    (today_key, month_key, seven_days_key, dates)
}

fn format_date(date: time::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

fn add_totals(target: &mut TokenTotals, source: &TokenTotals) {
    target.requests = target.requests.saturating_add(source.requests);
    target.input_tokens = target.input_tokens.saturating_add(source.input_tokens);
    target.output_tokens = target.output_tokens.saturating_add(source.output_tokens);
    target.cache_read_tokens = target
        .cache_read_tokens
        .saturating_add(source.cache_read_tokens);
    target.cache_creation_tokens = target
        .cache_creation_tokens
        .saturating_add(source.cache_creation_tokens);
    target.total_tokens = target.total_tokens.saturating_add(source.total_tokens);
}

#[cfg(test)]
mod tests {
    use super::{
        format_tokens, parse_claude_record, parse_codex_record, read_source_file, SourceFile,
        SourceKind, SyncStatus, UsageStore,
    };
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn formats_tokens_with_commas() {
        assert_eq!(format_tokens(168_896_956), "168,896,956");
        assert_eq!(format_tokens(-1), "0");
    }

    #[test]
    fn parses_claude_usage_from_assistant_message() {
        let value = json!({
            "type": "assistant",
            "timestamp": "2026-09-17T12:00:00.000Z",
            "message": {
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 30,
                    "cache_read_input_tokens": 20,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 10,
                        "ephemeral_1h_input_tokens": 5
                    }
                }
            }
        });
        let row = parse_claude_record(&value, "fixture:0").expect("parse Claude usage");
        assert_eq!(row.app_type, "claude");
        assert_eq!(row.totals.input_tokens, 100);
        assert_eq!(row.totals.cache_read_tokens, 20);
        assert_eq!(row.totals.cache_creation_tokens, 15);
        assert_eq!(row.totals.total_tokens, 165);
    }

    #[test]
    fn merges_streaming_claude_snapshots_by_message_id() {
        let first = json!({
            "type": "assistant",
            "timestamp": "2026-09-17T12:00:00.000Z",
            "sessionId": "session-1",
            "requestId": "request-1",
            "message": { "id": "message-1", "usage": { "input_tokens": 100, "output_tokens": 10 } }
        });
        let second = json!({
            "type": "assistant",
            "timestamp": "2026-09-17T12:00:01.000Z",
            "sessionId": "session-1",
            "requestId": "request-1",
            "message": { "id": "message-1", "usage": { "input_tokens": 100, "output_tokens": 30 } }
        });
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("token-tray-claude-dedupe-{timestamp}.jsonl"));
        let content = format!("{}\n{}\n", first, second);
        fs::write(&path, content).expect("write dedupe fixture");
        let rows = read_source_file(&SourceFile {
            path: path.clone(),
            kind: SourceKind::Claude,
        })
        .expect("read dedupe fixture");
        let _ = fs::remove_file(path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].totals.total_tokens, 130);
    }

    #[test]
    fn keeps_last_complete_usage_when_a_source_disappears() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("token-tray-usage-{timestamp}"));
        fs::create_dir(&directory).expect("create fixture directory");
        let first = directory.join("first.jsonl");
        let second = directory.join("second.jsonl");
        let record = json!({
            "type": "assistant",
            "timestamp": "2026-09-17T12:00:00.000Z",
            "message": { "usage": { "input_tokens": 100, "output_tokens": 10 } }
        });
        fs::write(&first, format!("{record}\n")).expect("write first fixture");
        fs::write(&second, format!("{record}\n")).expect("write second fixture");

        let sources = || {
            vec![
                SourceFile {
                    path: first.clone(),
                    kind: SourceKind::Claude,
                },
                SourceFile {
                    path: second.clone(),
                    kind: SourceKind::Claude,
                },
            ]
        };
        let store = UsageStore::default();
        let complete = store.sync_sources(sources(), false);
        assert_eq!(complete.status, SyncStatus::Success);
        assert_eq!(complete.update.snapshot.total.total_tokens, 220);

        fs::remove_file(&second).expect("remove second fixture");
        let partial = store.sync_sources(sources(), false);
        assert_eq!(partial.status, SyncStatus::LocalFilesUnavailable);
        assert_eq!(partial.update.snapshot.total.total_tokens, 220);
        assert_eq!(
            partial.update.last_synced_at,
            complete.update.last_synced_at
        );
        assert!(partial.update.error.is_some());

        let discovery_failed = store.sync_sources(
            vec![SourceFile {
                path: first,
                kind: SourceKind::Claude,
            }],
            true,
        );
        assert_eq!(discovery_failed.status, SyncStatus::LocalFilesUnavailable);
        assert_eq!(discovery_failed.update.snapshot.total.total_tokens, 220);
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn uses_codex_last_usage_instead_of_cumulative_total() {
        let first = json!({
            "type": "event_msg",
            "timestamp": "2026-09-17T12:00:00.000Z",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": { "input_tokens": 100, "cached_input_tokens": 20, "output_tokens": 10, "total_tokens": 110 },
                    "last_token_usage": { "input_tokens": 100, "cached_input_tokens": 20, "output_tokens": 10, "total_tokens": 110 }
                }
            }
        });
        let second = json!({
            "type": "event_msg",
            "timestamp": "2026-09-17T12:01:00.000Z",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": { "input_tokens": 180, "cached_input_tokens": 40, "output_tokens": 15, "total_tokens": 195 },
                    "last_token_usage": { "input_tokens": 80, "cached_input_tokens": 20, "output_tokens": 5, "total_tokens": 85 }
                }
            }
        });
        let mut previous = None;
        let first_row = parse_codex_record(&first, &mut previous).expect("first Codex usage");
        let second_row = parse_codex_record(&second, &mut previous).expect("second Codex usage");
        assert_eq!(first_row.totals.total_tokens, 110);
        assert_eq!(second_row.totals.total_tokens, 85);
        assert_eq!(second_row.totals.requests, 1);
    }

    #[test]
    fn reads_jsonl_and_ignores_partial_last_line() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("token-tray-usage-{timestamp}-{id}.jsonl"));
        let content = concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-17T12:00:00Z\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}}\n",
            "{\"type\":\"assistant\""
        );
        fs::write(&path, content).expect("write fixture");
        let rows = read_source_file(&SourceFile {
            path: path.clone(),
            kind: SourceKind::Claude,
        })
        .expect("read fixture");
        let _ = fs::remove_file(path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].totals.total_tokens, 12);
    }
}
