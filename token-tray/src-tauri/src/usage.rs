use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Emitter, Runtime, State};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::diagnostics;

const DATABASE_ENV: &str = "CC_SWITCH_DB_PATH";
const DATABASE_FILENAMES: &[&str] = &[
    "cc-switch.db",
    "cc-switch.sqlite",
    "cc-switch.sqlite3",
    "database.db",
    "database.sqlite",
];

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
    DatabaseNotFound,
    DatabaseUnavailable,
    UnsupportedSchema,
    QueryFailed,
}

impl SyncStatus {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::Success => "",
            Self::DatabaseNotFound => "未找到 CC Switch 数据库",
            Self::DatabaseUnavailable => "暂时无法读取 CC Switch 数据库",
            Self::UnsupportedSchema => "CC Switch 数据库结构暂不兼容",
            Self::QueryFailed => "读取 CC Switch 统计失败",
        }
    }

    pub fn diagnostic_result(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DatabaseNotFound => "database_not_found",
            Self::DatabaseUnavailable => "database_unavailable",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::QueryFailed => "query_failed",
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
    database_path: Mutex<Option<PathBuf>>,
}

#[derive(Default)]
struct UsageCache {
    snapshot: UsageSnapshot,
    last_synced_at: Option<i64>,
    error: Option<String>,
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
        let cached_path = self
            .inner
            .database_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();

        let result = match cached_path.as_deref() {
            Some(path) => match read_snapshot(path) {
                Ok(snapshot) => Ok((snapshot, path.to_path_buf())),
                Err(_) => discover_and_read(),
            },
            None => discover_and_read(),
        };

        let (status, snapshot, path) = match result {
            Ok((snapshot, path)) => (SyncStatus::Success, Some(snapshot), Some(path)),
            Err(status) => (status, None, None),
        };

        let mut database_path = self
            .inner
            .database_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *database_path = path;
        drop(database_path);

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

fn discover_and_read() -> Result<(UsageSnapshot, PathBuf), SyncStatus> {
    let mut saw_file = false;
    let mut saw_unsupported = false;
    for path in database_candidates() {
        if !path.is_file() {
            continue;
        }
        saw_file = true;
        match read_snapshot(&path) {
            Ok(snapshot) => return Ok((snapshot, path)),
            Err(SyncStatus::UnsupportedSchema) => saw_unsupported = true,
            Err(_) => {}
        }
    }

    if saw_unsupported {
        Err(SyncStatus::UnsupportedSchema)
    } else if saw_file {
        Err(SyncStatus::DatabaseUnavailable)
    } else {
        Err(SyncStatus::DatabaseNotFound)
    }
}

fn read_snapshot(path: &Path) -> Result<UsageSnapshot, SyncStatus> {
    let connection = open_database(path)?;
    let mut snapshot = query_snapshot(&connection)?;
    snapshot.source = "CC Switch".to_string();
    Ok(snapshot)
}

fn open_database(path: &Path) -> Result<Connection, SyncStatus> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| SyncStatus::DatabaseUnavailable)?;
    connection
        .busy_timeout(Duration::from_millis(750))
        .map_err(|_| SyncStatus::DatabaseUnavailable)?;
    Ok(connection)
}

fn database_candidates() -> Vec<PathBuf> {
    if let Some(path) = env::var_os(DATABASE_ENV).filter(|value| !value.is_empty()) {
        return vec![PathBuf::from(path)];
    }

    let mut roots = Vec::new();
    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        let home = PathBuf::from(home);
        roots.push(home.clone());
        roots.push(home.join(".config"));
        roots.push(home.join(".local").join("share"));
        roots.push(home.join("Library").join("Application Support"));
    }
    for variable in [
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
    ] {
        if let Some(path) = env::var_os(variable) {
            roots.push(PathBuf::from(path));
        }
    }

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        add_database_paths(&root, &mut candidates, &mut seen);
        for directory in [".cc-switch", "cc-switch", "CC Switch", "CCSwitch"] {
            let directory = root.join(directory);
            add_database_paths(&directory, &mut candidates, &mut seen);
            add_database_paths(&directory.join("data"), &mut candidates, &mut seen);
            add_database_paths(&directory.join("database"), &mut candidates, &mut seen);
        }
        scan_for_cc_switch_directories(&root, 0, &mut candidates, &mut seen);
    }
    candidates
}

fn add_database_paths(directory: &Path, output: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    for filename in DATABASE_FILENAMES {
        let path = directory.join(filename);
        if seen.insert(path.clone()) {
            output.push(path);
        }
    }
}

fn scan_for_cc_switch_directories(
    directory: &Path,
    depth: u8,
    output: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name == "cc-switch" || name == "cc switch" || name == "ccswitch" {
            add_database_paths(&path, output, seen);
            add_database_paths(&path.join("data"), output, seen);
            add_database_paths(&path.join("database"), output, seen);
        } else if depth < 2 {
            scan_for_cc_switch_directories(&path, depth + 1, output, seen);
        }
    }
}

#[derive(Clone)]
struct TableInfo {
    name: String,
    columns: HashMap<String, String>,
}

#[derive(Clone, Copy)]
enum TableKind {
    Rollup,
    Log,
}

fn query_snapshot(connection: &Connection) -> Result<UsageSnapshot, SyncStatus> {
    let tables = table_infos(connection)?;
    let mut fragments = Vec::new();
    for table in tables {
        let Some(kind) = table_kind(&table.name) else {
            continue;
        };
        let fragment = match kind {
            TableKind::Rollup => build_rollup_fragment(&table),
            TableKind::Log => build_log_fragment(&table),
        };
        if let Some(fragment) = fragment {
            fragments.push(fragment);
        }
    }
    if fragments.is_empty() {
        return Err(SyncStatus::UnsupportedSchema);
    }

    let rows_sql = format!(
        "SELECT date_key, app_type, requests, input_tokens, output_tokens, \
                cache_read_tokens, cache_creation_tokens, total_tokens \
           FROM ({}) \
          ORDER BY date_key DESC",
        fragments.join(" UNION ALL ")
    );
    let mut statement = connection
        .prepare(&rows_sql)
        .map_err(|_| SyncStatus::QueryFailed)?;
    let rows = statement
        .query_map([], |row| {
            Ok(DailyAppRow {
                date: row.get(0)?,
                app_type: row.get(1)?,
                totals: TokenTotals {
                    requests: row.get(2)?,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    cache_read_tokens: row.get(5)?,
                    cache_creation_tokens: row.get(6)?,
                    total_tokens: row.get(7)?,
                },
            })
        })
        .map_err(|_| SyncStatus::QueryFailed)?;

    let today_key = today_key(connection)?;
    let seven_days_key = connection
        .query_row("SELECT date('now', 'localtime', '-6 days')", [], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| SyncStatus::QueryFailed)?;
    let month_key = today_key.get(..7).unwrap_or(today_key.as_str()).to_string();
    let mut today = TokenTotals::default();
    let mut month = TokenTotals::default();
    let mut total = TokenTotals::default();
    let mut last_seven_days = TokenTotals::default();
    let mut daily_by_date = BTreeMap::<String, TokenTotals>::new();
    let mut by_app = BTreeMap::<String, TokenTotals>::new();
    for row in rows {
        let row = row.map_err(|_| SyncStatus::QueryFailed)?;
        add_totals(&mut total, &row.totals);
        if row.date >= month_key {
            add_totals(&mut month, &row.totals);
        }
        if row.date >= seven_days_key {
            add_totals(&mut last_seven_days, &row.totals);
            let date_totals = daily_by_date.entry(row.date.clone()).or_default();
            add_totals(date_totals, &row.totals);
        }
        let app_totals = by_app.entry(row.app_type).or_default();
        add_totals(app_totals, &row.totals);
        if row.date == today_key {
            add_totals(&mut today, &row.totals);
        }
    }

    let mut by_app = by_app
        .into_iter()
        .map(|(app_type, totals)| AppUsage {
            app_type,
            total_tokens: totals.total_tokens,
            requests: totals.requests,
        })
        .collect::<Vec<_>>();
    by_app.sort_by(|left, right| right.total_tokens.cmp(&left.total_tokens));

    let mut date_statement = connection
        .prepare(
            "WITH RECURSIVE days(date_key) AS (
                SELECT ?1
                UNION ALL
                SELECT date(date_key, '+1 day')
                  FROM days
                 WHERE date_key < ?2
            )
            SELECT date_key FROM days ORDER BY date_key",
        )
        .map_err(|_| SyncStatus::QueryFailed)?;
    let dates = date_statement
        .query_map(params![&seven_days_key, &today_key], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| SyncStatus::QueryFailed)?;
    let mut daily = Vec::new();
    for date in dates {
        let date = date.map_err(|_| SyncStatus::QueryFailed)?;
        let totals = daily_by_date.remove(&date).unwrap_or_default();
        daily.push(DailyUsage {
            date,
            total_tokens: totals.total_tokens,
            requests: totals.requests,
        });
    }

    Ok(UsageSnapshot {
        today,
        month,
        total,
        last_seven_days,
        daily,
        by_app,
        updated_at: today_key,
        source: String::new(),
    })
}

struct DailyAppRow {
    date: String,
    app_type: String,
    totals: TokenTotals,
}

fn table_infos(connection: &Connection) -> Result<Vec<TableInfo>, SyncStatus> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master \
               WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .map_err(|_| SyncStatus::QueryFailed)?;
    let table_names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| SyncStatus::QueryFailed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SyncStatus::QueryFailed)?;

    let mut tables = Vec::new();
    for name in table_names {
        let pragma = format!("PRAGMA table_info({})", quote_identifier(&name));
        let mut columns_statement = connection
            .prepare(&pragma)
            .map_err(|_| SyncStatus::QueryFailed)?;
        let columns = columns_statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|_| SyncStatus::QueryFailed)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SyncStatus::QueryFailed)?
            .into_iter()
            .map(|column| (column.to_ascii_lowercase(), column))
            .collect::<HashMap<_, _>>();
        tables.push(TableInfo { name, columns });
    }
    Ok(tables)
}

fn table_kind(name: &str) -> Option<TableKind> {
    let normalized = name.to_ascii_lowercase();
    if normalized == "usage_daily_rollups"
        || (normalized.contains("usage") && normalized.contains("rollup"))
        || (normalized.contains("daily") && normalized.contains("usage"))
    {
        Some(TableKind::Rollup)
    } else if normalized == "proxy_request_logs"
        || normalized.contains("request")
        || normalized.contains("proxy_log")
    {
        Some(TableKind::Log)
    } else {
        None
    }
}

fn build_rollup_fragment(table: &TableInfo) -> Option<String> {
    let date = find_column(table, &["date", "date_key", "day", "created_at"])?;
    let input = find_column(
        table,
        &["input_tokens", "prompt_tokens", "input_token_count"],
    );
    let output = find_column(
        table,
        &["output_tokens", "completion_tokens", "output_token_count"],
    );
    let cache_read = find_column(
        table,
        &["cache_read_tokens", "cache_read_input_tokens", "cache_read"],
    );
    let cache_creation = find_column(
        table,
        &[
            "cache_creation_tokens",
            "cache_creation_input_tokens",
            "cache_creation",
        ],
    );
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_creation.is_none() {
        return None;
    }

    let alias = "r";
    let date_expression = date_expression(alias, &date);
    let app_expression = app_expression(
        alias,
        find_column(table, &["app_type", "app", "provider", "source"]),
    );
    let input_expression = number_expression(alias, input.as_deref());
    let output_expression = number_expression(alias, output.as_deref());
    let cache_read_expression = number_expression(alias, cache_read.as_deref());
    let cache_creation_expression = number_expression(alias, cache_creation.as_deref());
    let fresh_input = fresh_input_expression(
        &input_expression,
        &output_expression,
        &cache_read_expression,
        &cache_creation_expression,
        &app_expression,
        optional_number_expression(alias, find_column(table, &["input_token_semantics"])),
    );
    let total_expression = format!(
        "({fresh_input} + {output_expression} + {cache_read_expression} + {cache_creation_expression})"
    );
    let request_expression = number_expression(
        alias,
        find_column(table, &["request_count", "requests", "request_total"]).as_deref(),
    );

    Some(format!(
        "SELECT {date_expression} AS date_key, {app_expression} AS app_type, \
                SUM({request_expression}) AS requests, \
                SUM({fresh_input}) AS input_tokens, \
                SUM({output_expression}) AS output_tokens, \
                SUM({cache_read_expression}) AS cache_read_tokens, \
                SUM({cache_creation_expression}) AS cache_creation_tokens, \
                SUM({total_expression}) AS total_tokens \
           FROM {} {alias} \
          GROUP BY {date_expression}, {app_expression}",
        quote_identifier(&table.name)
    ))
}

fn build_log_fragment(table: &TableInfo) -> Option<String> {
    let created = find_column(
        table,
        &["created_at", "timestamp", "created", "request_time"],
    )?;
    let input = find_column(
        table,
        &["input_tokens", "prompt_tokens", "input_token_count"],
    );
    let output = find_column(
        table,
        &["output_tokens", "completion_tokens", "output_token_count"],
    );
    let cache_read = find_column(
        table,
        &["cache_read_tokens", "cache_read_input_tokens", "cache_read"],
    );
    let cache_creation = find_column(
        table,
        &[
            "cache_creation_tokens",
            "cache_creation_input_tokens",
            "cache_creation",
        ],
    );
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_creation.is_none() {
        return None;
    }

    let alias = "l";
    let date_expression = date_expression(alias, &created);
    let app_expression = app_expression(
        alias,
        find_column(table, &["app_type", "app", "provider", "source"]),
    );
    let input_expression = number_expression(alias, input.as_deref());
    let output_expression = number_expression(alias, output.as_deref());
    let cache_read_expression = number_expression(alias, cache_read.as_deref());
    let cache_creation_expression = number_expression(alias, cache_creation.as_deref());
    let fresh_input = fresh_input_expression(
        &input_expression,
        &output_expression,
        &cache_read_expression,
        &cache_creation_expression,
        &app_expression,
        optional_number_expression(alias, find_column(table, &["input_token_semantics"])),
    );
    let total_expression = format!(
        "({fresh_input} + {output_expression} + {cache_read_expression} + {cache_creation_expression})"
    );
    let status_filter = find_column(table, &["status_code", "status"])
        .map(|column| {
            let expression = qualified(alias, &column);
            format!(
                "WHERE CAST({expression} AS INTEGER) >= 200 AND CAST({expression} AS INTEGER) < 300"
            )
        })
        .unwrap_or_default();

    Some(format!(
        "SELECT {date_expression} AS date_key, {app_expression} AS app_type, \
                COUNT(*) AS requests, \
                SUM({fresh_input}) AS input_tokens, \
                SUM({output_expression}) AS output_tokens, \
                SUM({cache_read_expression}) AS cache_read_tokens, \
                SUM({cache_creation_expression}) AS cache_creation_tokens, \
                SUM({total_expression}) AS total_tokens \
           FROM {} {alias} \
          {status_filter} \
          GROUP BY {date_expression}, {app_expression}",
        quote_identifier(&table.name)
    ))
}

fn find_column(table: &TableInfo, aliases: &[&str]) -> Option<String> {
    aliases
        .iter()
        .find_map(|alias| table.columns.get(*alias).cloned())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn qualified(alias: &str, column: &str) -> String {
    format!("{alias}.{}", quote_identifier(column))
}

fn number_expression(alias: &str, column: Option<&str>) -> String {
    match column {
        Some(column) => format!("COALESCE(CAST({} AS INTEGER), 0)", qualified(alias, column)),
        None => "0".to_string(),
    }
}

fn optional_number_expression(alias: &str, column: Option<String>) -> Option<String> {
    column.map(|column| format!("CAST({} AS INTEGER)", qualified(alias, &column)))
}

fn app_expression(alias: &str, column: Option<String>) -> String {
    match column {
        Some(column) => format!(
            "COALESCE(NULLIF(CAST({} AS TEXT), ''), 'unknown')",
            qualified(alias, &column)
        ),
        None => "'unknown'".to_string(),
    }
}

fn date_expression(alias: &str, column: &str) -> String {
    let value = qualified(alias, column);
    format!(
        "COALESCE(CASE WHEN typeof({value}) IN ('integer', 'real') \
             THEN date(CASE WHEN {value} > 100000000000 THEN {value} / 1000 ELSE {value} END, 'unixepoch', 'localtime') \
             ELSE date({value}, 'localtime') END, '')"
    )
}

fn fresh_input_expression(
    input: &str,
    _output: &str,
    cache_read: &str,
    cache_creation: &str,
    app: &str,
    semantics: Option<String>,
) -> String {
    let legacy = format!(
        "CASE \
           WHEN {app} IN ('codex', 'gemini', 'grokbuild') \
                AND {input} >= {cache_read} + {cache_creation} \
             THEN {input} - {cache_read} - {cache_creation} \
           WHEN {app} IN ('codex', 'gemini', 'grokbuild') \
                AND {input} >= {cache_read} \
             THEN {input} - {cache_read} \
           ELSE {input} \
         END"
    );
    match semantics {
        Some(semantics) => format!(
            "CASE \
               WHEN {semantics} = 2 THEN {input} \
               WHEN {semantics} = 1 AND {input} >= {cache_read} + {cache_creation} \
                 THEN {input} - {cache_read} - {cache_creation} \
               ELSE {legacy} \
             END"
        ),
        None => legacy,
    }
}

fn today_key(connection: &Connection) -> Result<String, SyncStatus> {
    connection
        .query_row("SELECT date('now', 'localtime')", [], |row| row.get(0))
        .map_err(|_| SyncStatus::QueryFailed)
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
    use super::{format_tokens, query_snapshot};
    use rusqlite::Connection;
    use std::env;
    use std::path::PathBuf;

    #[test]
    fn formats_tokens_with_commas() {
        assert_eq!(format_tokens(168_896_956), "168,896,956");
        assert_eq!(format_tokens(-1), "0");
    }

    #[test]
    fn reads_current_cc_switch_schema() {
        let connection = Connection::open_in_memory().expect("open in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE usage_daily_rollups (
                    date TEXT,
                    app_type TEXT,
                    request_count INTEGER,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    cache_read_tokens INTEGER,
                    cache_creation_tokens INTEGER,
                    input_token_semantics INTEGER
                );
                CREATE TABLE proxy_request_logs (
                    created_at INTEGER,
                    app_type TEXT,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    cache_read_tokens INTEGER,
                    cache_creation_tokens INTEGER,
                    input_token_semantics INTEGER,
                    status_code INTEGER
                );
                INSERT INTO usage_daily_rollups VALUES
                    (date('now', 'localtime'), 'codex', 2, 100, 30, 20, 10, 1);",
            )
            .expect("create current schema");

        let snapshot = query_snapshot(&connection).expect("read current schema");
        assert_eq!(snapshot.today.total_tokens, 130);
        assert_eq!(snapshot.today.requests, 2);
        assert_eq!(snapshot.daily.len(), 7);
        assert_eq!(snapshot.daily.last().map(|day| day.total_tokens), Some(130));
    }

    #[test]
    fn reads_future_schema_aliases_without_semantics_column() {
        let connection = Connection::open_in_memory().expect("open in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE usage_daily (
                    day TEXT,
                    provider TEXT,
                    requests INTEGER,
                    prompt_tokens INTEGER,
                    completion_tokens INTEGER,
                    cache_read INTEGER,
                    cache_creation INTEGER
                );
                INSERT INTO usage_daily VALUES
                    (date('now', 'localtime'), 'claude', 2, 100, 30, 20, 10);",
            )
            .expect("create future schema");

        let snapshot = query_snapshot(&connection).expect("read future schema");
        assert_eq!(snapshot.today.total_tokens, 160);
        assert_eq!(snapshot.today.requests, 2);
    }

    #[test]
    fn reads_configured_database_when_present() {
        let Some(path) = env::var_os(super::DATABASE_ENV) else {
            return;
        };
        let path = PathBuf::from(path);
        if !path.is_file() {
            return;
        }
        let snapshot = super::read_snapshot(&path).expect("read configured database");
        assert!(!snapshot.source.is_empty());
    }
}
