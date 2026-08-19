use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUsage {
    pub app_type: String,
    pub total_tokens: i64,
    pub requests: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub today: TokenTotals,
    pub month: TokenTotals,
    pub total: TokenTotals,
    pub last_seven_days: TokenTotals,
    pub by_app: Vec<AppUsage>,
    pub updated_at: String,
    pub source: String,
}

#[tauri::command]
pub fn get_usage_snapshot() -> Result<UsageSnapshot, String> {
    let db_path = cc_switch_db_path()?;
    let connection = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("无法只读打开 CC Switch 数据库：{error}"))?;

    let mut snapshot =
        query_snapshot(&connection).map_err(|error| format!("读取 CC Switch 统计失败：{error}"))?;
    snapshot.source = db_path.display().to_string();
    Ok(snapshot)
}

#[cfg(target_os = "macos")]
pub fn today_total_tokens() -> Result<i64, String> {
    let db_path = cc_switch_db_path()?;
    let connection = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("无法只读打开 CC Switch 数据库：{error}"))?;
    let snapshot =
        query_snapshot(&connection).map_err(|error| format!("读取 CC Switch 统计失败：{error}"))?;
    Ok(snapshot.today.total_tokens)
}

fn cc_switch_db_path() -> Result<PathBuf, String> {
    let user_profile = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| "找不到当前用户目录".to_string())?;
    let path = PathBuf::from(user_profile)
        .join(".cc-switch")
        .join("cc-switch.db");
    if !path.is_file() {
        return Err(format!("未找到 CC Switch 数据库：{}", path.display()));
    }
    Ok(path)
}

fn query_snapshot(connection: &Connection) -> rusqlite::Result<UsageSnapshot> {
    let fresh_input_logs = fresh_input_sql("l");
    let fresh_input_rollups = fresh_input_sql("r");
    let rows_sql = format!(
        "SELECT date_key, app_type, requests, input_tokens, output_tokens,
                cache_read_tokens, cache_creation_tokens, total_tokens
           FROM (
             SELECT r.date AS date_key, r.app_type,
                    SUM(r.request_count) AS requests,
                    SUM({fresh_input_rollups}) AS input_tokens,
                    SUM(r.output_tokens) AS output_tokens,
                    SUM(r.cache_read_tokens) AS cache_read_tokens,
                    SUM(r.cache_creation_tokens) AS cache_creation_tokens,
                    SUM({fresh_input_rollups} + r.output_tokens + r.cache_read_tokens + r.cache_creation_tokens) AS total_tokens
               FROM usage_daily_rollups r
              GROUP BY r.date, r.app_type
             UNION ALL
             SELECT date(l.created_at, 'unixepoch', 'localtime') AS date_key, l.app_type,
                    COUNT(*) AS requests,
                    SUM({fresh_input_logs}) AS input_tokens,
                    SUM(l.output_tokens) AS output_tokens,
                    SUM(l.cache_read_tokens) AS cache_read_tokens,
                    SUM(l.cache_creation_tokens) AS cache_creation_tokens,
                    SUM({fresh_input_logs} + l.output_tokens + l.cache_read_tokens + l.cache_creation_tokens) AS total_tokens
               FROM proxy_request_logs l
              WHERE date(l.created_at, 'unixepoch', 'localtime') = date('now', 'localtime')
                AND l.status_code >= 200 AND l.status_code < 300
              GROUP BY date_key, l.app_type
           )
          ORDER BY date_key DESC"
    );

    let mut statement = connection.prepare(&rows_sql)?;
    let rows = statement.query_map([], |row| {
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
    })?;

    let today_key = today_key(connection)?;
    let seven_days_key =
        connection.query_row("SELECT date('now', 'localtime', '-6 days')", [], |row| {
            row.get::<_, String>(0)
        })?;
    let month_key = today_key.get(..7).unwrap_or_default().to_string();
    let mut today = TokenTotals::default();
    let mut month = TokenTotals::default();
    let mut total = TokenTotals::default();
    let mut last_seven_days = TokenTotals::default();
    let mut by_app = std::collections::BTreeMap::<String, TokenTotals>::new();
    for row in rows {
        let row = row?;
        add_totals(&mut total, &row.totals);
        if row.date >= month_key {
            add_totals(&mut month, &row.totals);
        }
        if row.date >= seven_days_key {
            add_totals(&mut last_seven_days, &row.totals);
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

    Ok(UsageSnapshot {
        today,
        month,
        total,
        last_seven_days,
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

fn today_key(connection: &Connection) -> rusqlite::Result<String> {
    connection.query_row("SELECT date('now', 'localtime')", [], |row| row.get(0))
}

fn add_totals(target: &mut TokenTotals, source: &TokenTotals) {
    target.requests += source.requests;
    target.input_tokens += source.input_tokens;
    target.output_tokens += source.output_tokens;
    target.cache_read_tokens += source.cache_read_tokens;
    target.cache_creation_tokens += source.cache_creation_tokens;
    target.total_tokens += source.total_tokens;
}

fn fresh_input_sql(alias: &str) -> String {
    format!(
        "CASE
           WHEN {alias}.input_token_semantics = 2 THEN {alias}.input_tokens
           WHEN {alias}.app_type IN ('codex', 'gemini', 'grokbuild')
                AND {alias}.input_token_semantics = 1
                AND {alias}.input_tokens >= {alias}.cache_read_tokens + {alias}.cache_creation_tokens
             THEN {alias}.input_tokens - {alias}.cache_read_tokens - {alias}.cache_creation_tokens
           WHEN {alias}.app_type IN ('codex', 'gemini', 'grokbuild')
                AND {alias}.input_tokens >= {alias}.cache_read_tokens
             THEN {alias}.input_tokens - {alias}.cache_read_tokens
           ELSE {alias}.input_tokens
         END"
    )
}
