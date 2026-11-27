// 备份 / 恢复：配置（JSON）与全部数据（zip，含音频）的导出与导入。
//
// 设计要点：
// - 导入走活动数据库的正常写入通道（app_settings upsert + 集合表整表替换），
//   完整还原先试写数据库，再安装录音并提交交易。导入后由前端触发重启使内存状态重载。
// - 语义为「覆盖」：集合表整表替换；app_settings 逐 key 覆盖（不删除未出现的 key）。
// - 「配置」不含使用统计 stats，避免覆盖本机计数；「全部」包含 stats。
//   [自訂] S35／S36：本 fork 的「全部」也不含 stats，且兩檔都排除金鑰、密碼、使用統計與隱私開關（見 MACHINE_LOCAL_KEYS）。
// - 全部导入时，历史记录里的 audioFilePath 是旧机绝对路径，会重写为本机 audio 目录下的同名文件。

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::storage::Storage;

/// 备份文件格式版本。导入时若备份版本高于此值则拒绝。
const FORMAT_VERSION: i64 = 1;
/// 分项配置文件格式；完整配置与全部数据继续使用 v1，保持兼容。
const SELECTED_CONFIG_FORMAT_VERSION: i64 = 2;
const MAX_HOTWORDS: usize = 1000;
const BUILTIN_PRESET_IDS: &[&str] = &["intent", "faithful", "zh2en", "casual"];
/// 只属于这台机器的 app_settings key：导出不带、导入（含 WebDAV 恢复）不写。
///
/// 备份文件可能来自别人（"这是我调好的热词，导入就能用"），所以导入时这些 key 一律当不可信：
///  · `pendingUpdate` —— 退出时会静默安装它指向的文件。恶意备份曾经能借它运行任意 exe（审核 S3）。
///  · `backendBaseUrl` —— 更新来源跟随这个地址，导入一个陌生地址就等于把"往这台机器装什么"交给对方，
///    口述的音频也会发过去。代价：换机恢复后要重新填一次服务器地址。
///  · 设备 id、开机自启的迁移标记、WebDAV 上次结果 —— 本机状态，带过去只会让两台机器互相冒名或误报。
const MACHINE_LOCAL_KEYS: &[&str] = &[
    "pendingUpdate",
    "backendBaseUrl",
    "deviceId",
    "clientDeviceId",
    "autoLaunch",
    "autoLaunchArgsMigrated",
    "autoLaunchInitialized",
    "webdav.lastAttemptAt",
    "webdav.lastResult",
    // [自訂] S35／S44：祕密與使用資料，任何匯出／備份都不寫出（API 金鑰、WebDAV 密碼、使用統計）。
    // 結尾 `*` 表示前綴比對（見 storage::is_settings_key_excluded）。
    "cloudAi.*",
    "cloudAsr.*",
    "webdav.*",
    "userStats",
    "stats",
    // [自訂] S36：隱私開關與雲端相關設定，任何匯入／還原都不覆寫（v1 匯入、分項匯入、完整備份、WebDAV 還原）。
    "aiEnabled",
    "historyEnabled",
    "audioRetentionEnabled",
    "audioRetentionDays",
    "contextAwareWritingEnabled",
    "workMode",
    "autoLaunch*",
    "localAsr.downloadSource",
    "localAsr.modelsDir",
    // [自訂] SCAN1-4（L-74）：麥克風裝置名稱與裝置 ID 只留在本機（名稱可能含姓名），不匯出也不匯入。
    "micLastGoodDevice",
    "selectedMic",
    // [自訂] SCAN1-11（L-81）：「貼上後還原剪貼簿」屬隱私開關，匯入／還原不可把它關掉。
    "protectClipboard",
];
/// 「配置」档不导出/导入的 app_settings key：本机专属的那些，加上使用统计（属于使用数据，不属于配置）。
/// 与 MACHINE_LOCAL_KEYS 的包含关系有测试钉住。
const CONFIG_EXCLUDE: &[&str] = &[
    "stats",
    "pendingUpdate",
    "backendBaseUrl",
    "deviceId",
    "clientDeviceId",
    "autoLaunch",
    "autoLaunchArgsMigrated",
    "autoLaunchInitialized",
    "webdav.lastAttemptAt",
    "webdav.lastResult",
    // [自訂] S35／S44：祕密與使用資料，任何匯出／備份都不寫出（API 金鑰、WebDAV 密碼、使用統計）。
    // 結尾 `*` 表示前綴比對（見 storage::is_settings_key_excluded）。
    "cloudAi.*",
    "cloudAsr.*",
    "webdav.*",
    "userStats",
    "stats",
    // [自訂] S36：隱私開關與雲端相關設定，任何匯入／還原都不覆寫（v1 匯入、分項匯入、完整備份、WebDAV 還原）。
    "aiEnabled",
    "historyEnabled",
    "audioRetentionEnabled",
    "audioRetentionDays",
    "contextAwareWritingEnabled",
    "workMode",
    "autoLaunch*",
    "localAsr.downloadSource",
    "localAsr.modelsDir",
    // [自訂] SCAN1-4（L-74）：麥克風裝置名稱與裝置 ID 只留在本機（名稱可能含姓名），不匯出也不匯入。
    "micLastGoodDevice",
    "selectedMic",
    // [自訂] SCAN1-11（L-81）：「貼上後還原剪貼簿」屬隱私開關，匯入／還原不可把它關掉。
    "protectClipboard",
];

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigExportSelection {
    mode: String,
    #[serde(default)]
    hotword_group_ids: Vec<String>,
    #[serde(default)]
    include_text_replacements: bool,
    #[serde(default)]
    default_text_replacements: Option<Vec<Value>>,
    #[serde(default)]
    prompt_preset_ids: Vec<String>,
}

/// 全量备份里除配置之外还装什么。
///
/// 两个字段都**不给 serde 默认值**：调用方必须显式表态。给默认值的那一版是个陷阱 ——
/// 漏传参数时会静默按「全都要」打包，而含音频的包可以有几个 GB，自动备份场景下
/// 意味着每次悄悄上传几个 GB。宁可让反序列化直接失败，报一条看得见的错。
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupScope {
    pub include_history: bool,
    pub include_audio: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigImportSectionPreview {
    kind: String,
    total: usize,
    added: usize,
    updated: usize,
    skipped: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigImportWarning {
    code: String,
    current: Option<usize>,
    limit: Option<usize>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigImportPreview {
    scope: String,
    format_version: i64,
    import_token: String,
    sections: Vec<ConfigImportSectionPreview>,
    warnings: Vec<ConfigImportWarning>,
    requires_restart: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigImportResult {
    changed_sections: Vec<String>,
    added: usize,
    updated: usize,
    skipped: usize,
    requires_restart: bool,
}

struct SelectedImportPlan {
    app_settings: Map<String, Value>,
    prompt_presets: Option<Vec<Value>>,
    sections: Vec<ConfigImportSectionPreview>,
    warnings: Vec<ConfigImportWarning>,
    added: usize,
    updated: usize,
    skipped: usize,
}

fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn audio_dir() -> PathBuf {
    crate::commands::app_data_dir()
        .join("audio")
}

fn backup_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::document_dir)
        .unwrap_or_else(|| {
            crate::commands::app_data_dir()
        })
        .join("SayItLocally Backups")
}

fn timestamped_backup_path(prefix: &str, extension: &str) -> PathBuf {
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S-%3f");
    backup_dir().join(format!("{}-{}.{}", prefix, timestamp, extension))
}

/// 取路径的文件名部分（兼容 / 和 \ 分隔符，跨平台备份用）。
fn basename(path: &str) -> String {
    path.rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(path)
        .to_string()
}

fn build_config_value(storage: &Storage) -> Result<Value, String> {
    storage.with_backup_snapshot(|snapshot| Ok(json!({
        "kind": "config",
        "formatVersion": FORMAT_VERSION,
        "appVersion": app_version(),
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "appSettings": snapshot.settings(CONFIG_EXCLUDE)?,
        "promptPresets": snapshot.collection("promptPresets")?,
        "appPromptRules": snapshot.collection("appPromptRules")?,
    })))
}

fn normalized_strings(value: Option<&Value>) -> Vec<String> {
    let mut seen = HashSet::new();
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert((*item).to_string()))
        .map(ToString::to_string)
        .collect()
}

fn build_selected_config_value(
    storage: &Storage,
    selection: &ConfigExportSelection,
) -> Result<Value, String> {
    let selected_hotword_ids: HashSet<&str> = selection
        .hotword_group_ids
        .iter()
        .map(String::as_str)
        .collect();
    let selected_preset_ids: HashSet<&str> = selection
        .prompt_preset_ids
        .iter()
        .map(String::as_str)
        .collect();
    storage.with_backup_snapshot(|snapshot| {
        let mut items = Map::new();

        if !selected_hotword_ids.is_empty() {
            let themes_value = snapshot.setting("customHotwordThemes")?.unwrap_or(Value::Null);
            let active_value = snapshot.setting("customThemeActive")?.unwrap_or(Value::Null);
            let active_map = active_value.as_object();
            let groups = themes_value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
                .filter(|theme| {
                    theme
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| selected_hotword_ids.contains(id))
                })
                .filter_map(|theme| {
                    let id = theme.get("id")?.as_str()?;
                    let name = theme.get("name")?.as_str()?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    Some(json!({
                        "name": name,
                        "words": normalized_strings(theme.get("words")),
                        "enabled": active_map
                            .and_then(|map| map.get(id))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }))
                })
                .collect::<Vec<_>>();
            items.insert("hotwordGroups".to_string(), Value::Array(groups));
        }

        if selection.include_text_replacements {
            if let Some(value) = snapshot.setting("textReplacements")? {
                let defaults = selection.default_text_replacements.as_deref().unwrap_or_default();
                let rules = match &value {
                    Value::Null => defaults,
                    Value::Array(rules) if rules.is_empty() => defaults,
                    Value::Array(rules) => rules,
                    _ => &[],
                };
                let rules = rules.iter()
                    .filter_map(|rule| {
                        let obj = rule.as_object()?;
                        let from = obj.get("from")?.as_str()?.trim();
                        if from.is_empty() {
                            return None;
                        }
                        Some(json!({
                            "from": from,
                            "to": obj.get("to").and_then(Value::as_str).unwrap_or(""),
                            "enabled": obj.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                        }))
                    })
                    .collect::<Vec<_>>();
                items.insert("textReplacements".to_string(), Value::Array(rules));
            }
        }

        if !selected_preset_ids.is_empty() {
            let presets = snapshot.collection("promptPresets")?
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|preset| {
                    let obj = preset.as_object()?;
                    let id = obj.get("id")?.as_str()?;
                    if !selected_preset_ids.contains(id) || BUILTIN_PRESET_IDS.contains(&id) {
                        return None;
                    }
                    let name = obj.get("name")?.as_str()?.trim();
                    let system_prompt = obj.get("systemPrompt")?.as_str()?.trim();
                    if name.is_empty() || system_prompt.is_empty() {
                        return None;
                    }
                    Some(json!({ "name": name, "systemPrompt": system_prompt }))
                })
                .collect::<Vec<_>>();
            items.insert("promptPresets".to_string(), Value::Array(presets));
        }

        if items.is_empty() {
            return Err("BACKUP_NOTHING_SELECTED".into());
        }

        Ok(json!({
            "kind": "config",
            "formatVersion": SELECTED_CONFIG_FORMAT_VERSION,
            "scope": "selected",
            "appVersion": app_version(),
            "exportedAt": chrono::Utc::now().to_rfc3339(),
            "items": Value::Object(items),
        }))
    })
}

/// 组装 `backup.json`。
///
/// 不含历史时**整个字段都不写**（而不是写一个空数组）：`import_full` 对缺失段是
/// `if let Some(...)`，字段缺席就跳过、保留本机现有历史；写成空数组反而会把本机
/// 历史整表清空。「只备份配置」这一档能安全恢复，全靠这个区别。
fn build_full_value(storage: &Storage, scope: BackupScope) -> Result<Value, String> {
    storage.with_backup_snapshot(|snapshot| {
        let mut payload = json!({
            "kind": "full",
            "formatVersion": FORMAT_VERSION,
            "appVersion": app_version(),
            "exportedAt": chrono::Utc::now().to_rfc3339(),
            "scope": {
                "includeHistory": scope.include_history,
                "includeAudio": scope.include_audio,
            },
            "appSettings": snapshot.settings(MACHINE_LOCAL_KEYS)?,
            "promptPresets": snapshot.collection("promptPresets")?,
            "appPromptRules": snapshot.collection("appPromptRules")?,
        });

        if scope.include_history {
            for key in ["history", "manualCorrections", "feedbackQueue"] {
                // [自訂] SC-55／SC-56：本機目錄在寫出前由 scrub_export_document 去除；資料庫內的原值不變。
                payload[key] = snapshot.collection(key)?;
            }
        }
        scrub_export_document(&mut payload);
        Ok(payload)
    })
}

fn check_kind_and_version(data: &Value, expected_kind: &str, max_version: i64) -> Result<i64, String> {
    let kind = data.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != expected_kind {
        return Err(format!("This is not a valid SayItLocally {} file", if expected_kind == "full" { "full-data backup" } else { "configuration" }));
    }
    let version = data
        .get("formatVersion")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if version > max_version {
        return Err(format!(
            "The backup version ({}) is newer than this app supports ({}). Update SayItLocally before importing it.",
            version, max_version
        ));
    }
    Ok(version)
}

fn is_selected_config(data: &Value) -> bool {
    data.get("scope").and_then(Value::as_str) == Some("selected")
        && data.get("formatVersion").and_then(Value::as_i64)
            == Some(SELECTED_CONFIG_FORMAT_VERSION)
}

/// 应用完整配置部分（设置 + Prompt 预设 + 分应用规则），两档共用。
fn apply_config_part(
    storage: &Storage,
    data: &Value,
    settings_exclude: &[&str],
) -> Result<(), String> {
    let empty_settings = Map::new();
    let app_settings = data
        .get("appSettings")
        .and_then(Value::as_object)
        .unwrap_or(&empty_settings);
    let prompt_presets = data
        .get("promptPresets")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let app_prompt_rules = data
        .get("appPromptRules")
        .and_then(Value::as_array)
        .map(Vec::as_slice);

    storage
        .apply_config_transaction(
            app_settings,
            settings_exclude,
            prompt_presets,
            app_prompt_rules,
        )
}

/// 备份里的录音文件名只认 `<录音 id>.wav`。原来什么名字都照写，`evil.exe` 也会被当成录音
/// 落到 audio 目录里，再配合 pendingUpdate 就能在退出时被运行（审核 S3）。
fn is_backup_audio_name(name: &str) -> bool {
    name.strip_suffix(".wav")
        .is_some_and(crate::path_guard::is_record_id)
}

/// [自訂] SC-56：整個字串是否為本機絕對路徑（Unix、`~`、Windows 磁碟代號、UNC、`file:`）。
/// 句子中間出現的路徑屬使用者內容，不算。
/// [自訂] SC-59（L-98）：另認 `\Users\…`（無磁碟代號）、`~name/…`、`%VAR%`、`$VAR`、`${VAR}`、
/// `smb:`、`afp:`。單獨的 `~`、`$5`、`50%`、`~約` 不算。
pub(crate) fn is_local_path(value: &str) -> bool {
    let value = value.trim();
    let bytes = value.as_bytes();
    match bytes {
        [b'/', second, ..] if !second.is_ascii_whitespace() => true,
        [b'\\', b'\\', ..] => true,
        [b'\\', rest @ ..] => rooted_backslash_path(rest),
        [b'~', b'/' | b'\\', ..] => true,
        [b'~', rest @ ..] => other_user_home(rest),
        [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic() => true,
        [b'%', rest @ ..] => percent_variable(rest),
        [b'$', rest @ ..] => dollar_variable(rest),
        _ => ["file:", "smb:", "afp:"].iter().any(|scheme| {
            value.get(..scheme.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
        }),
    }
}

/// `\Users\name`：第一段非空、不含空白，後面還有一層。
fn rooted_backslash_path(rest: &[u8]) -> bool {
    let segment = rest.iter().position(|byte| *byte == b'\\').map(|end| &rest[..end]);
    segment.is_some_and(|segment| !segment.is_empty() && !segment.iter().any(u8::is_ascii_whitespace))
}

fn is_identifier(name: &[u8]) -> bool {
    matches!(name, [first, ..] if first.is_ascii_alphabetic() || *first == b'_')
        && name.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn is_separator_or_end(rest: &[u8]) -> bool {
    matches!(rest, [] | [b'/' | b'\\', ..])
}

/// 不接分隔符也算路徑的環境變數（指向家目錄或系統資料夾）。
const PATH_VARIABLES: &[&str] = &[
    "HOME", "USERPROFILE", "HOMEPATH", "APPDATA", "LOCALAPPDATA", "PROGRAMDATA", "TMPDIR", "TEMP", "TMP",
];

/// 變數名後面接 `/`、`\`，或整段就是常見的路徑變數。
fn variable_path(name: &[u8], rest: &[u8]) -> bool {
    match rest {
        [] => PATH_VARIABLES.iter().any(|known| known.as_bytes().eq_ignore_ascii_case(name)),
        _ => is_separator_or_end(rest),
    }
}

/// `~name/…`：name 只收 ASCII，所以 `~約`、`~5/10` 不算。
fn other_user_home(rest: &[u8]) -> bool {
    let end = rest.iter().position(|byte| matches!(byte, b'/' | b'\\'));
    end.is_some_and(|end| {
        let name = &rest[..end];
        matches!(name, [first, ..] if first.is_ascii_alphabetic() || *first == b'_')
            && name.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })
}

/// `%APPDATA%`、`%USERPROFILE%\x`；`ProgramFiles(x86)` 這類名稱可含括號。
fn percent_variable(rest: &[u8]) -> bool {
    let Some(end) = rest.iter().position(|byte| *byte == b'%') else { return false };
    let name = &rest[..end];
    let plain: Vec<u8> = name.iter().copied().filter(|byte| !matches!(byte, b'(' | b')')).collect();
    name.len() >= 2 && is_identifier(&plain) && variable_path(name, &rest[end + 1..])
}

/// `$HOME`、`${HOME}/x`、`$TMPDIR/x`；`$5`、`$USD` 不算。
fn dollar_variable(rest: &[u8]) -> bool {
    if let [b'{', inner @ ..] = rest {
        let Some(end) = inner.iter().position(|byte| *byte == b'}') else { return false };
        return is_identifier(&inner[..end]) && is_separator_or_end(&inner[end + 1..]);
    }
    let end = rest.iter().position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')).unwrap_or(rest.len());
    is_identifier(&rest[..end]) && variable_path(&rest[..end], &rest[end..])
}

/// [自訂] SC-56 第三輪：紀錄與集合只處理這些由系統產生的中繼資料欄位；其他欄位與頂層鍵
/// （辨識與更正文字、熱詞、提示詞、名稱、比對條件等使用者內容）一律原樣寫出。
const METADATA_FIELDS: &[&str] = &[
    "windowTitle", "exePath", "processName", "windowClass", "appId", "appName",
    "failReason", "promptSummary", "styleSummary",
];
/// 整個物件都是錄音當時的系統環境資訊，其中所有路徑值都移除。
const METADATA_CONTAINERS: &[&str] = &["appContext"];
/// 值是使用者輸入文字的設定鍵：即使整段像路徑也寫出。其他設定的值含路徑時整鍵不寫出。
const CONTENT_SETTINGS: &[&str] = &[
    "aiPromptAppend", "contextSelectionEditPrompt", "hotwords", "builtinSetWords",
    "builtinSetExtraWords", "customHotwordThemes", "manualHotwords", "textReplacements",
    "hotwordLearning", "styleProfile",
];

/// [自訂] SC-59：JSON 與 CSV 匯出共用同一份中繼資料欄位清單。
pub(crate) fn is_metadata_field(name: &str) -> bool {
    METADATA_FIELDS.contains(&name) || METADATA_CONTAINERS.contains(&name)
}

fn contains_local_path(value: &Value) -> bool {
    match value {
        Value::String(text) => is_local_path(text),
        Value::Array(items) => items.iter().any(contains_local_path),
        Value::Object(map) => map.iter().any(|(key, item)| is_local_path(key) || contains_local_path(item)),
        _ => false,
    }
}

/// 中繼資料值裡的路徑：字串整個移除（回傳 false），清單與物件只移除路徑項。
fn keep_metadata(value: &mut Value) -> bool {
    match value {
        Value::String(text) => !is_local_path(text),
        Value::Array(items) => {
            items.retain_mut(keep_metadata);
            true
        }
        Value::Object(map) => {
            // [自訂] SC-59：鍵本身是路徑時，整項不寫出。
            map.retain(|key, item| !is_local_path(key) && keep_metadata(item));
            true
        }
        _ => true,
    }
}

/// [自訂] SC-56：紀錄與集合中，只處理列名的中繼資料欄位；`audioFilePath` 依 SC-55 只留有效檔名。
fn scrub_records(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(scrub_records),
        Value::Object(map) => {
            strip_audio_field(map);
            map.retain(|key, item| {
                !is_metadata_field(key) || keep_metadata(item)
            });
            map.values_mut().for_each(scrub_records);
        }
        _ => {}
    }
}

/// [自訂] SC-56：寫出前去除本機絕對路徑。非內容設定的值含路徑時整鍵不寫出（還原時保留本機原值）；
/// 紀錄只處理列名的中繼資料欄位。只改寫出的副本，不改資料庫。
pub(crate) fn scrub_export_document(document: &mut Value) {
    if let Some(Value::Object(settings)) = document.get_mut("appSettings") {
        settings.retain(|key, value| CONTENT_SETTINGS.contains(&key.as_str()) || !contains_local_path(value));
    }
    match document {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key != "appSettings" {
                    scrub_records(item);
                }
            }
        }
        other => scrub_records(other),
    }
}

/// [自訂] SC-55：備份寫出時，`audioFilePath` 只留還原端會接受的檔名，其餘移除。
/// 還原端本來就只取檔名，所以新舊備份還原出的路徑相同。
fn strip_audio_field(record: &mut Map<String, Value>) {
    let Some(path) = record.get_mut("audioFilePath") else { return };
    match path.as_str().map(basename) {
        Some(name) if is_backup_audio_name(&name) => *path = Value::String(name),
        _ => { record.remove("audioFilePath"); }
    }
}

/// 重写单条历史记录的音频路径：把目录段换成本机 audio 目录，文件名不变。
/// 文件名不合规的录音不会被释放出来，记录里的路径也一并去掉，免得指向一个不存在或不该碰的文件。
fn rewrite_audio_path(rec: &Value, adir: &Path) -> Value {
    let mut rec = rec.clone();
    if let Some(obj) = rec.as_object_mut() {
        if let Some(p) = obj.get("audioFilePath").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            let base = basename(&p);
            if is_backup_audio_name(&base) {
                let new_path = adir.join(&base).to_string_lossy().to_string();
                obj.insert("audioFilePath".to_string(), Value::String(new_path));
            } else if !p.is_empty() {
                obj.remove("audioFilePath");
            }
        }
    }
    rec
}

fn next_import_id(prefix: &str, used_ids: &mut HashSet<String>, sequence: &mut usize) -> String {
    loop {
        let candidate = format!(
            "{}_{}_{}",
            prefix,
            chrono::Utc::now().timestamp_millis(),
            *sequence
        );
        *sequence += 1;
        if used_ids.insert(candidate.clone()) {
            return candidate;
        }
    }
}

fn unique_import_name(base: &str, used_names: &mut HashSet<String>) -> String {
    if used_names.insert(base.to_lowercase()) {
        return base.to_string();
    }
    let mut index = 1usize;
    loop {
        let candidate = if index == 1 {
            format!("{} (Imported)", base)
        } else {
            format!("{} (Imported {})", base, index)
        };
        if used_names.insert(candidate.to_lowercase()) {
            return candidate;
        }
        index += 1;
    }
}

fn build_selected_import_plan(storage: &Storage, data: &Value) -> Result<SelectedImportPlan, String> {
    let items = data
        .get("items")
        .and_then(Value::as_object)
        .ok_or_else(|| "The configuration file is missing items and may be damaged".to_string())?;
    let mut app_settings = Map::new();
    let mut prompt_presets = None;
    let mut sections = Vec::new();
    let mut warnings = Vec::new();
    let mut total_added = 0usize;
    let mut total_updated = 0usize;
    let mut total_skipped = 0usize;
    let mut sequence = 0usize;

    if let Some(group_value) = items.get("hotwordGroups") {
        let imported_groups = group_value
            .as_array()
            .ok_or_else(|| "Invalid hotword group format".to_string())?;
        let mut local_groups = storage
            .get("customHotwordThemes", None)
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut active_map = storage
            .get("customThemeActive", None)
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut used_ids = local_groups
            .iter()
            .filter_map(|group| group.get("id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect::<HashSet<_>>();
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;

        for imported in imported_groups {
            let Some(obj) = imported.as_object() else {
                skipped += 1;
                continue;
            };
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if name.is_empty() {
                skipped += 1;
                continue;
            }
            let words = normalized_strings(obj.get("words"));
            let normalized_name = name.to_lowercase();
            let existing_index = local_groups.iter().position(|group| {
                group
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|current| current.trim().to_lowercase() == normalized_name)
            });

            if let Some(index) = existing_index {
                let existing_words = normalized_strings(local_groups[index].get("words"));
                let mut merged_words = existing_words.clone();
                let mut seen = existing_words.into_iter().collect::<HashSet<_>>();
                for word in words {
                    if seen.insert(word.clone()) {
                        merged_words.push(word);
                    }
                }
                if merged_words.len()
                    == normalized_strings(local_groups[index].get("words")).len()
                {
                    skipped += 1;
                } else if let Some(local_obj) = local_groups[index].as_object_mut() {
                    local_obj.insert("words".to_string(), json!(merged_words));
                    updated += 1;
                }
            } else {
                let id = next_import_id("theme_import", &mut used_ids, &mut sequence);
                let enabled = obj
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                local_groups.push(json!({ "id": id, "name": name, "words": words }));
                active_map.insert(id, Value::Bool(enabled));
                added += 1;
            }
        }

        let mut active_words = HashSet::new();
        for group in &local_groups {
            let Some(id) = group.get("id").and_then(Value::as_str) else {
                continue;
            };
            if active_map.get(id).and_then(Value::as_bool) != Some(true) {
                continue;
            }
            active_words.extend(normalized_strings(group.get("words")));
        }
        if active_words.len() > MAX_HOTWORDS {
            warnings.push(ConfigImportWarning {
                code: "hotwordLimit".to_string(),
                current: Some(active_words.len()),
                limit: Some(MAX_HOTWORDS),
            });
        }

        app_settings.insert("customHotwordThemes".to_string(), Value::Array(local_groups));
        app_settings.insert("customThemeActive".to_string(), Value::Object(active_map));
        sections.push(ConfigImportSectionPreview {
            kind: "hotwords".to_string(),
            total: imported_groups.len(),
            added,
            updated,
            skipped,
        });
        total_added += added;
        total_updated += updated;
        total_skipped += skipped;
    }

    if let Some(rules_value) = items.get("textReplacements") {
        let imported_rules = rules_value
            .as_array()
            .ok_or_else(|| "Invalid text replacement format".to_string())?;
        let mut local_rules = storage
            .get("textReplacements", None)
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut used_ids = local_rules
            .iter()
            .filter_map(|rule| rule.get("id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect::<HashSet<_>>();
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;

        for imported in imported_rules {
            let Some(obj) = imported.as_object() else {
                skipped += 1;
                continue;
            };
            let from = obj
                .get("from")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if from.is_empty() {
                skipped += 1;
                continue;
            }
            let to = obj.get("to").and_then(Value::as_str).unwrap_or("");
            let existing_index = local_rules.iter().position(|rule| {
                rule.get("from").and_then(Value::as_str) == Some(from)
            });

            if let Some(index) = existing_index {
                let current_to = local_rules[index]
                    .get("to")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if current_to == to {
                    skipped += 1;
                } else if let Some(local_obj) = local_rules[index].as_object_mut() {
                    local_obj.insert("to".to_string(), Value::String(to.to_string()));
                    updated += 1;
                }
            } else {
                let id = next_import_id("replacement_import", &mut used_ids, &mut sequence);
                local_rules.push(json!({
                    "id": id,
                    "from": from,
                    "to": to,
                    "enabled": obj.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                }));
                added += 1;
            }
        }

        app_settings.insert("textReplacements".to_string(), Value::Array(local_rules));
        sections.push(ConfigImportSectionPreview {
            kind: "textReplacements".to_string(),
            total: imported_rules.len(),
            added,
            updated,
            skipped,
        });
        total_added += added;
        total_updated += updated;
        total_skipped += skipped;
    }

    if let Some(presets_value) = items.get("promptPresets") {
        let imported_presets = presets_value
            .as_array()
            .ok_or_else(|| "Invalid prompt preset format".to_string())?;
        let mut local_presets = storage
            .get("promptPresets", None)
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut used_ids = local_presets
            .iter()
            .filter_map(|preset| preset.get("id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect::<HashSet<_>>();
        let mut used_names = local_presets
            .iter()
            .filter_map(|preset| preset.get("name").and_then(Value::as_str))
            .map(|name| name.trim().to_lowercase())
            .collect::<HashSet<_>>();
        let mut added = 0usize;
        let mut skipped = 0usize;

        for imported in imported_presets {
            let Some(obj) = imported.as_object() else {
                skipped += 1;
                continue;
            };
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let system_prompt = obj
                .get("systemPrompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if name.is_empty() || system_prompt.is_empty() {
                skipped += 1;
                continue;
            }
            let exact_duplicate = local_presets.iter().any(|preset| {
                preset
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|current| current.trim().eq_ignore_ascii_case(name))
                    && preset.get("systemPrompt").and_then(Value::as_str) == Some(system_prompt)
            });
            if exact_duplicate {
                skipped += 1;
                continue;
            }
            let imported_name = unique_import_name(name, &mut used_names);
            let id = next_import_id("preset_import", &mut used_ids, &mut sequence);
            local_presets.push(json!({
                "id": id,
                "name": imported_name,
                "systemPrompt": system_prompt,
            }));
            added += 1;
        }

        prompt_presets = Some(local_presets);
        sections.push(ConfigImportSectionPreview {
            kind: "promptPresets".to_string(),
            total: imported_presets.len(),
            added,
            updated: 0,
            skipped,
        });
        total_added += added;
        total_skipped += skipped;
    }

    if sections.is_empty() {
        return Err("The configuration file contains nothing that can be imported".to_string());
    }

    Ok(SelectedImportPlan {
        app_settings,
        prompt_presets,
        sections,
        warnings,
        added: total_added,
        updated: total_updated,
        skipped: total_skipped,
    })
}

// ─── 导出 ───

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupExportProgress {
    status: String,
    phase: String,
    file_path: String,
    current_file: Option<String>,
    processed_files: u64,
    total_files: u64,
    processed_bytes: u64,
    total_bytes: u64,
    percent: f64,
    error: Option<String>,
}

struct AudioExportFile {
    path: PathBuf,
    name: String,
    size: u64,
}

fn emit_export_progress(app: &AppHandle, progress: BackupExportProgress) {
    let _ = app.emit("backup-export-progress", progress);
}

fn collect_audio_export_files(include_audio: bool, adir: &Path) -> Result<Vec<AudioExportFile>, String> {
    if !include_audio {
        return Ok(Vec::new());
    }
    let directory_error = |_| "BACKUP_READ_RECORDINGS".to_owned();
    // Backups may read a linked audio directory; the entries below must be regular files.
    let entries = match fs::read_dir(adir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(adir) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                _ => Err(directory_error(error)),
            };
        }
        Err(error) => return Err(directory_error(error)),
    };
    let mut files = Vec::new();
    let mut skipped = 0;
    let mut names = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(directory_error)?;
        let metadata = entry.metadata().map_err(directory_error)?; // Does not follow links.
        let name = entry.file_name();
        if let Some(name) = name.to_str().filter(|name| metadata.is_file() && is_backup_audio_name(name)) {
            if !names.insert(name.to_ascii_lowercase()) {
                return Err("BACKUP_RECORDING_NAME_CONFLICT".into());
            }
            files.push(AudioExportFile { path: entry.path(), name: name.to_owned(), size: metadata.len() });
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        log::info!("Skipped {skipped} non-recording entries in backup");
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

fn open_backup_recording(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other("expected a regular recording"));
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("expected a regular recording"));
    }
    Ok(file)
}

fn audio_progress_percent(processed_bytes: u64, total_bytes: u64) -> f64 {
    if total_bytes == 0 {
        95.0
    } else {
        (5.0 + processed_bytes as f64 / total_bytes as f64 * 90.0).min(95.0)
    }
}

#[tauri::command]
pub fn get_backup_directory() -> Result<String, String> {
    let directory = backup_dir();
    fs::create_dir_all(&directory).map_err(|e| format!("Failed to create backup directory: {}", e))?;
    Ok(directory.to_string_lossy().to_string())
}

fn read_config_export_choices(storage: &Storage) -> Result<Value, String> {
    storage.with_backup_snapshot(|snapshot| {
        let mut choices = json!({
            "hotwordThemes": snapshot.setting("customHotwordThemes")?,
            "promptPresets": snapshot.collection("promptPresets")?,
        });
        if let Some(replacements) = snapshot.setting("textReplacements")? {
            choices["textReplacements"] = replacements;
        }
        Ok(choices)
    })
}

#[tauri::command]
pub async fn inspect_config_export(storage: State<'_, Storage>) -> Result<Value, String> {
    read_config_export_choices(storage.inner())
}

#[tauri::command]
pub async fn export_config(
    selection: ConfigExportSelection,
    storage: State<'_, Storage>,
) -> Result<String, String> {
    let out_path = timestamped_backup_path("sayit-config", "json");
    write_config_file(storage.inner(), &selection, &out_path)?;
    let path = out_path.to_string_lossy().to_string();
    log::info!("Config backup exported to {}", path);
    Ok(path)
}

fn write_config_file(storage: &Storage, selection: &ConfigExportSelection, output: &Path) -> Result<(), String> {
    // [自訂] SC-56：記憶體庫不寫出設定檔。
    storage.ensure_exportable()?;
    let mut payload = match selection.mode.as_str() {
        "full" => build_config_value(storage)?,
        "selected" => build_selected_config_value(storage, selection)?,
        _ => return Err("Unsupported configuration export mode".into()),
    };
    scrub_export_document(&mut payload);
    let content = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    publish_backup(output, |mut file| {
        file.write_all(content.as_bytes()).map_err(|error| error.to_string())?;
        Ok(file)
    })
}

// Publish only the completed file. Only remove the temporary file we created.
fn publish_backup(
    output: &Path,
    write: impl FnOnce(fs::File) -> Result<fs::File, String>,
) -> Result<(), String> {
    use std::io::ErrorKind;

    let export_io_error = |error: std::io::Error| {
        log::warn!("Backup file I/O failed: {error}");
        "BACKUP_EXPORT_IO".to_owned()
    };
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(export_io_error)?;
    let temp = parent.join(format!("sayit-backup-{}.part", uuid::Uuid::new_v4()));
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)
        .map_err(export_io_error)?;
    let result = write(file).map_err(|error| {
        if error.starts_with("BACKUP_") {
            error
        } else {
            log::warn!("Backup serialization failed: {error}");
            "BACKUP_EXPORT_IO".to_owned()
        }
    }).and_then(|file| {
        file.sync_all().map_err(export_io_error)?;
        drop(file);
        publish_without_replacing(&temp, output).map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                "BACKUP_EXPORT_EXISTS".to_owned()
            } else {
                export_io_error(error)
            }
        })
    });
    if let Err(error) = fs::remove_file(&temp) {
        if error.kind() != ErrorKind::NotFound {
            log::warn!("Failed to remove temporary backup: {error}");
        }
    }
    result
}

#[cfg(target_os = "macos")]
fn publish_without_replacing(source: &Path, output: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source_c = CString::new(source.as_os_str().as_bytes())?;
    let output_c = CString::new(output.as_os_str().as_bytes())?;
    publish_with_rename_fallback(source, output, || {
        // Both paths have the same parent; RENAME_EXCL atomically refuses an existing destination.
        let result = unsafe { libc::renamex_np(source_c.as_ptr(), output_c.as_ptr(), libc::RENAME_EXCL) };
        if result == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    })
}

#[cfg(windows)]
fn publish_without_replacing(source: &Path, output: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }

    let parent = source.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = parent.canonicalize()?; // Use the extended-path form for long paths.
    let wide_path = |file: &Path| -> std::io::Result<Vec<u16>> {
        let name = file.file_name().ok_or_else(|| std::io::Error::new(
            std::io::ErrorKind::InvalidInput, "Missing backup file name",
        ))?;
        Ok(parent.join(name).as_os_str().encode_wide().chain([0]).collect())
    };
    let source = wide_path(source)?;
    let output = wide_path(output)?;
    // Zero flags retain the original destination if another export already created it.
    let result = unsafe { MoveFileExW(source.as_ptr(), output.as_ptr(), 0) };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn publish_without_replacing(source: &Path, output: &Path) -> std::io::Result<()> {
    publish_with_rename_fallback(source, output, || fs::hard_link(source, output))
}

#[cfg(any(not(windows), test))]
fn publish_with_rename_fallback(
    source: &Path,
    output: &Path,
    publish: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    publish_with_fallback_check(source, output, publish, is_unsupported_publication)
}

/// The check is a parameter so tests can confirm the raw OS code reaches it,
/// whatever kind the current Rust gives that code.
#[cfg(any(not(windows), test))]
fn publish_with_fallback_check(
    source: &Path,
    output: &Path,
    publish: impl FnOnce() -> std::io::Result<()>,
    is_unsupported: impl FnOnce(std::io::ErrorKind, Option<i32>) -> bool,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let Err(error) = publish() else { return Ok(()); };
    if !is_unsupported(error.kind(), error.raw_os_error()) {
        return Err(error);
    }

    // Serialize this fallback across exports. Another process could still create
    // the destination between the check and rename on filesystems without no-clobber.
    static FALLBACK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = FALLBACK_LOCK.lock().map_err(|_| Error::other("Backup publication lock poisoned"))?;
    match fs::symlink_metadata(output) {
        Ok(_) => Err(ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == ErrorKind::NotFound => fs::rename(source, output),
        Err(error) => Err(error),
    }
}

/// Rust versions disagree on whether macOS ENOTSUP (45) is `ErrorKind::Unsupported`,
/// so the raw codes are matched directly.
#[cfg(any(not(windows), test))]
fn is_unsupported_publication(kind: std::io::ErrorKind, code: Option<i32>) -> bool {
    use std::io::ErrorKind;
    #[cfg(target_os = "macos")]
    if matches!(code, Some(libc::ENOTSUP | libc::EOPNOTSUPP)) {
        return true;
    }
    let _ = code;
    matches!(kind, ErrorKind::Unsupported | ErrorKind::InvalidInput)
}

/// 打包过程中的一帧进度。`phase` 取值与前端 `BackupExportProgress['phase']` 对齐。
pub struct ArchiveProgress<'a> {
    pub phase: &'static str,
    pub current_file: Option<&'a str>,
    pub processed_files: u64,
    pub total_files: u64,
    pub processed_bytes: u64,
    pub total_bytes: u64,
    pub percent: f64,
}

/// 把配置（+ 按 scope 决定的历史 / 音频）打成 zip 写到 `output`。
///
/// 本地「全部数据」导出与 WebDAV 备份共用这一份实现，两者的差别只在落地位置和
/// 进度事件名。刻意不让 WebDAV 自己写一套打包：一旦两份代码分家，就会出现
/// 「本地导出能恢复、云端备份恢复不了」这种只在换机时才暴露的问题。
///
/// 先写 `.part` 再原子发布，失败时清掉临时文件 —— 中断留下的半个 zip 不能
/// 长得像一个有效备份。
///
/// 总量（文件数 / 字节数）通过 `on_progress` 报出去，不作为返回值：调用方要的是
/// 打包完成后**zip 本身**的大小（用来算上传进度），那个得 stat 产物文件才知道。
pub fn write_backup_archive(
    storage: &Storage,
    scope: BackupScope,
    output: &Path,
    on_progress: &mut dyn FnMut(ArchiveProgress),
) -> Result<(), String> {
    write_backup_archive_in_dir(storage, scope, output, &audio_dir(), on_progress)
}

fn write_backup_archive_in_dir(
    storage: &Storage,
    scope: BackupScope,
    output: &Path,
    adir: &Path,
    on_progress: &mut dyn FnMut(ArchiveProgress),
) -> Result<(), String> {
    // [自訂] SC-56：本機與 WebDAV 備份共用；記憶體庫在建立任何檔案之前就拒絕。
    storage.ensure_exportable()?;
    // [自訂] S71：收集與讀取備份錄音期間禁止清理。固定先取得錄音鎖，再讀資料庫。
    let _recordings = scope.include_audio.then(super::audio::lock_recordings_for_backup).transpose()?;
    let audio_files = collect_audio_export_files(scope.include_audio, adir)?;
    let payload = build_full_value(storage, scope)?;
    let json_str = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    let total_files = audio_files.len() as u64;
    let total_bytes = audio_files.iter().map(|file| file.size).sum::<u64>();

    on_progress(ArchiveProgress {
        phase: "preparing",
        current_file: None,
        processed_files: 0,
        total_files,
        processed_bytes: 0,
        total_bytes,
        percent: 2.0,
    });

    publish_backup(output, |file| {
        on_progress(ArchiveProgress {
            phase: "packingData",
            current_file: Some("backup.json"),
            processed_files: 0,
            total_files,
            processed_bytes: 0,
            total_bytes,
            percent: 5.0,
        });

        let mut zip = zip::ZipWriter::new(file);
        super::archive::write_deflated_entry(&mut zip, "backup.json", json_str.as_bytes())?;

        let mut processed_bytes = 0u64;
        let mut processed_files = 0u64;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut buffer = vec![0u8; 256 * 1024];

        for audio in &audio_files {
            let source = open_backup_recording(&audio.path)
                .map_err(|_| "BACKUP_READ_RECORDINGS")?;
            let options = super::archive::file_options(zip::CompressionMethod::Stored, audio.size);
            zip.start_file(format!("audio/{}", audio.name), options)
                .map_err(|e| e.to_string())?;

            let mut source = source.take(audio.size.saturating_add(1));
            let mut file_bytes = 0;
            loop {
                let read = source
                    .read(&mut buffer)
                    .map_err(|_| "BACKUP_READ_RECORDINGS")?;
                if read == 0 {
                    break;
                }
                file_bytes += read as u64;
                if file_bytes > audio.size {
                    return Err("BACKUP_RECORDING_CHANGED".into());
                }
                zip.write_all(&buffer[..read]).map_err(|e| e.to_string())?;
                processed_bytes += read as u64;

                if last_emit.elapsed() >= Duration::from_millis(150) {
                    on_progress(ArchiveProgress {
                        phase: "packingAudio",
                        current_file: Some(&audio.name),
                        processed_files,
                        total_files,
                        processed_bytes,
                        total_bytes,
                        percent: audio_progress_percent(processed_bytes, total_bytes),
                    });
                    last_emit = Instant::now();
                }
            }

            if file_bytes != audio.size {
                return Err("BACKUP_RECORDING_CHANGED".into());
            }
            processed_files += 1;
            on_progress(ArchiveProgress {
                phase: "packingAudio",
                current_file: Some(&audio.name),
                processed_files,
                total_files,
                processed_bytes,
                total_bytes,
                percent: audio_progress_percent(processed_bytes, total_bytes),
            });
        }

        on_progress(ArchiveProgress {
            phase: "finalizing",
            current_file: None,
            processed_files,
            total_files,
            processed_bytes,
            total_bytes,
            percent: 98.0,
        });
        zip.finish().map_err(|e| e.to_string())
    })
}

#[tauri::command]
pub async fn export_full(
    app: AppHandle,
    scope: BackupScope,
    storage: State<'_, Storage>,
) -> Result<String, String> {
    let output = timestamped_backup_path("sayit-backup", "zip");
    let out_path = output.to_string_lossy().to_string();

    // 失败分支也要报出总量，所以在回调里留一份最近值：write_backup_archive 出错时
    // 只给 Err(String)，拿不到它内部算出的 totals。
    let mut seen_totals = (0u64, 0u64);
    let export_result = {
        let progress_app = app.clone();
        let progress_path = out_path.clone();
        write_backup_archive(storage.inner(), scope, &output, &mut |progress| {
            seen_totals = (progress.total_files, progress.total_bytes);
            emit_export_progress(
                &progress_app,
                BackupExportProgress {
                    status: "running".to_string(),
                    phase: progress.phase.to_string(),
                    file_path: progress_path.clone(),
                    current_file: progress.current_file.map(ToString::to_string),
                    processed_files: progress.processed_files,
                    total_files: progress.total_files,
                    processed_bytes: progress.processed_bytes,
                    total_bytes: progress.total_bytes,
                    percent: progress.percent,
                    error: None,
                },
            );
        })
    };
    let (total_files, total_bytes) = seen_totals;

    match export_result {
        Ok(()) => {
            emit_export_progress(
                &app,
                BackupExportProgress {
                    status: "completed".to_string(),
                    phase: "completed".to_string(),
                    file_path: out_path.clone(),
                    current_file: None,
                    processed_files: total_files,
                    total_files,
                    processed_bytes: total_bytes,
                    total_bytes,
                    percent: 100.0,
                    error: None,
                },
            );
            log::info!(
                "Full backup exported to {} (history={} audio={})",
                out_path,
                scope.include_history,
                scope.include_audio
            );
            Ok(out_path)
        }
        Err(error) => {
            // 临时文件已由 write_backup_archive 清理。
            emit_export_progress(
                &app,
                BackupExportProgress {
                    status: "failed".to_string(),
                    phase: "failed".to_string(),
                    file_path: out_path,
                    current_file: None,
                    processed_files: 0,
                    total_files,
                    processed_bytes: 0,
                    total_bytes,
                    percent: 0.0,
                    error: Some(error.clone()),
                },
            );
            Err(error)
        }
    }
}

// ─── 导入 ───

fn content_fingerprint(content: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn read_config_file(in_path: &str) -> Result<(Value, String), String> {
    let content = fs::read_to_string(in_path).map_err(|e| format!("Failed to read file: {}", e))?;
    let data = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse the file; it may be damaged or have an invalid format: {}", e))?;
    Ok((data, content_fingerprint(&content)))
}

fn config_import_token(
    storage: &Storage,
    data: &Value,
    file_fingerprint: &str,
) -> Result<String, String> {
    if !is_selected_config(data) {
        return Ok(file_fingerprint.to_string());
    }

    let context = json!({
        "file": file_fingerprint,
        "customHotwordThemes": storage.get("customHotwordThemes", None),
        "customThemeActive": storage.get("customThemeActive", None),
        "textReplacements": storage.get("textReplacements", None),
        "promptPresets": storage.get("promptPresets", None),
    });
    let serialized = serde_json::to_string(&context)
        .map_err(|error| format!("Failed to prepare import validation data: {}", error))?;
    Ok(content_fingerprint(&serialized))
}

fn validate_config_collection(key: &str, items: &[Value]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("Item {} in {} has an invalid format", index + 1, key))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Item {} in {} is missing id", index + 1, key))?;

        // 内置 Prompt 的中英文自定义内容共存在 promptPresets 中，因此同一个稳定 id
        // 可以出现两次，但同一语言仍只能有一份。旧备份没有语言字段，只能是中文。
        let unique_id = if key == "promptPresets" && BUILTIN_PRESET_IDS.contains(&id) {
            // [自訂] SC-65：中文內建 Prompt 改為正體中文（zh-TW）；舊備份的 zh-CN 視為同一語言。
            let language = match object
                .get("builtinPromptLanguage")
                .and_then(Value::as_str)
                .unwrap_or("zh-TW")
            {
                "zh-CN" => "zh-TW",
                other => other,
            };
            if language != "zh-TW" && language != "en" {
                return Err(format!(
                    "Item {} in {} has an invalid language",
                    index + 1,
                    key
                ));
            }
            format!("{}:{}", id, language)
        } else {
            id.to_string()
        };
        if !ids.insert(unique_id) {
            return Err(format!("{} contains a duplicate id: {}", key, id));
        }

        let required_fields: &[&str] = match key {
            "promptPresets" => &["name", "systemPrompt"],
            "appPromptRules" => &["appId", "name"],
            _ => &[],
        };
        for field in required_fields {
            let valid = object
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
            if !valid {
                return Err(format!(
                    "Item {} in {} is missing {}",
                    index + 1,
                    key,
                    field
                ));
            }
        }
    }
    Ok(())
}

fn full_config_preview_sections(data: &Value) -> Result<Vec<ConfigImportSectionPreview>, String> {
    let mut sections = Vec::new();

    if let Some(value) = data.get("appSettings") {
        let settings = value
            .as_object()
            .ok_or_else(|| "appSettings has an invalid format".to_string())?;
        if !settings.is_empty() {
            sections.push(ConfigImportSectionPreview {
                kind: "appSettings".to_string(),
                total: settings.len(),
                added: 0,
                updated: settings.len(),
                skipped: 0,
            });
        }
    }

    for (key, kind) in [
        ("promptPresets", "promptPresets"),
        ("appPromptRules", "appPromptRules"),
    ] {
        if let Some(value) = data.get(key) {
            let items = value
                .as_array()
                .ok_or_else(|| format!("{} has an invalid format", key))?;
            validate_config_collection(key, items)?;
            sections.push(ConfigImportSectionPreview {
                kind: kind.to_string(),
                total: items.len(),
                added: 0,
                updated: items.len(),
                skipped: 0,
            });
        }
    }

    if sections.is_empty() {
        return Err("The file contains no complete configuration to import".to_string());
    }
    Ok(sections)
}

#[tauri::command]
pub async fn inspect_config_import(
    in_path: String,
    storage: State<'_, Storage>,
) -> Result<ConfigImportPreview, String> {
    let (data, file_fingerprint) = read_config_file(&in_path)?;
    let version = check_kind_and_version(
        &data,
        "config",
        SELECTED_CONFIG_FORMAT_VERSION,
    )?;

    if is_selected_config(&data) {
        let plan = build_selected_import_plan(storage.inner(), &data)?;
        let import_token = config_import_token(storage.inner(), &data, &file_fingerprint)?;
        return Ok(ConfigImportPreview {
            scope: "selected".to_string(),
            format_version: version,
            import_token,
            sections: plan.sections,
            warnings: plan.warnings,
            requires_restart: true,
        });
    }

    if data.get("scope").is_some() || version > FORMAT_VERSION {
        return Err("Unsupported configuration scope or version".to_string());
    }

    let sections = full_config_preview_sections(&data)?;
    Ok(ConfigImportPreview {
        scope: "full".to_string(),
        format_version: version,
        import_token: file_fingerprint,
        sections,
        warnings: vec![ConfigImportWarning {
            code: "fullOverwrite".to_string(),
            current: None,
            limit: None,
        }],
        requires_restart: true,
    })
}

#[tauri::command]
pub async fn import_config(
    in_path: String,
    expected_import_token: String,
    storage: State<'_, Storage>,
) -> Result<ConfigImportResult, String> {
    apply_config_import(storage.inner(), &in_path, &expected_import_token)
}

fn apply_config_import(
    storage: &Storage,
    in_path: &str,
    expected_import_token: &str,
) -> Result<ConfigImportResult, String> {
    // [自訂] SC-55：記憶體庫在讀檔之前就拒絕，涵蓋 v1、完整與分項匯入。
    storage.ensure_persistent()?;
    let (data, file_fingerprint) = read_config_file(in_path)?;
    let version = check_kind_and_version(
        &data,
        "config",
        SELECTED_CONFIG_FORMAT_VERSION,
    )?;

    if is_selected_config(&data) {
        let actual_token = config_import_token(storage, &data, &file_fingerprint)?;
        if actual_token != expected_import_token {
            return Err("The configuration file or local settings changed; select the file again and reconfirm".to_string());
        }

        let plan = build_selected_import_plan(storage, &data)?;
        let changed_sections = plan
            .sections
            .iter()
            .filter(|section| section.added + section.updated > 0)
            .map(|section| section.kind.clone())
            .collect::<Vec<_>>();
        storage
            .apply_config_transaction(
                &plan.app_settings,
                CONFIG_EXCLUDE,
                plan.prompt_presets.as_deref(),
                None,
            )?;
        return Ok(ConfigImportResult {
            changed_sections,
            added: plan.added,
            updated: plan.updated,
            skipped: plan.skipped,
            requires_restart: true,
        });
    }

    if data.get("scope").is_some() || version > FORMAT_VERSION {
        return Err("Unsupported configuration scope or version".to_string());
    }
    full_config_preview_sections(&data)?;
    if file_fingerprint != expected_import_token {
        return Err("The configuration file changed; select it again and reconfirm".to_string());
    }

    // 完整配置导入排除 stats，避免覆盖本机使用统计。
    apply_config_part(storage, &data, CONFIG_EXCLUDE)?;
    Ok(ConfigImportResult {
        changed_sections: vec!["fullConfig".to_string()],
        added: 0,
        updated: 1,
        skipped: 0,
        requires_restart: true,
    })
}

#[tauri::command]
pub async fn import_full(in_path: String, app: AppHandle) -> Result<(), String> {
    // [自訂] REV9：錄音檔鎖及匯入 I/O 都在阻塞工作執行緒。
    tokio::task::spawn_blocking(move || apply_full_backup(app.state::<Storage>().inner(), &in_path))
        .await.map_err(|e| e.to_string())?
}

/// 应用一个全量备份 zip。本地导入与 WebDAV 恢复共用。
///
/// 对缺失段是宽容的：只含配置的备份（WebDAV 默认档）不会带 `history` 字段，
/// 此时**保留本机现有历史**而不是清空 —— 见 `build_full_value` 里为什么不写空数组。
pub fn apply_full_backup(storage: &Storage, in_path: &str) -> Result<(), String> {
    apply_full_backup_in_dir(storage, in_path, &audio_dir())
}

fn apply_full_backup_in_dir(storage: &Storage, in_path: &str, adir: &Path) -> Result<(), String> {
    // [自訂] SC-55：本機匯入與 WebDAV 還原共用；記憶體庫在開檔之前就拒絕。
    storage.ensure_persistent()?;
    let file = fs::File::open(in_path).map_err(|e| format!("Failed to open file: {}", e))?;
    let archive = zip::ZipArchive::new(file).map_err(|e| format!("Not a valid backup archive: {}", e))?;
    apply_full_backup_archive(storage, archive, adir)
}

fn apply_full_backup_archive(
    storage: &Storage,
    mut archive: zip::ZipArchive<impl Read + Seek>,
    adir: &Path,
) -> Result<(), String> {
    // [自訂] SC-51：zip 的一致性檢查須明確呼叫。先拒絕標頭不一致或重複的條目，
    // 此時尚未建立或寫入任何錄音檔，也尚未套用設定。
    archive.validate_header_consistency().map_err(|e| format!("Invalid backup ZIP headers: {e}"))?;
    let mut names = HashSet::new();
    let mut recordings = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index_data(index).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?;
        if entry.is_symlink() || !is_backup_entry_path_safe(&name) {
            return Err("The archive contains a symbolic link or an unsafe entry path".to_string());
        }
        if entry.encrypted() {
            return Err("Encrypted backup ZIP entries are unsupported".to_string());
        }
        if !matches!(entry.compression(), zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated) {
            return Err("Unsupported compression in backup ZIP".to_string());
        }
        // 不分大小寫的檔案系統上，兩個不同條目仍可能寫到同一檔。
        if let Some(base) = archive_audio_basename(&name, entry.is_file()) {
            if !names.insert(base.to_ascii_lowercase()) {
                return Err("The archive contains conflicting recording names".to_string());
            }
            recordings.push((index, base.to_owned()));
        }
    }

    // 1. 读取并校验 backup.json
    let mut json_str = String::new();
    {
        let mut entry = archive
            .by_name("backup.json")
            .map_err(backup_json_entry_error)?;
        if !entry.is_file() {
            return Err("The archive's backup.json must be a regular file".to_string());
        }
        entry.read_to_string(&mut json_str).map_err(|e| e.to_string())?;
    }
    let data: Value = serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse backup.json: {}", e))?;
    check_kind_and_version(&data, "full", FORMAT_VERSION)?;

    // [自訂] SC-51：只有含錄音的備份才需要安全的錄音目錄。
    let recordings_guard = (!recordings.is_empty()).then(super::audio::lock_recordings_for_restore);
    apply_full_restore(storage, &data, adir, || {
        if let Some(guard) = &recordings_guard {
            check_recording_restore_path(adir, true)?;
            for (_, name) in &recordings {
                check_recording_restore_path(&adir.join(name), false)?;
            }
            fs::create_dir_all(adir).map_err(|e| format!("Failed to create audio directory: {e}"))?;
            super::audio::cleanup_restore_staging(adir, guard);
            for (index, name) in recordings {
                let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
                let destination = adir.join(&name);
                restore_audio_file(&mut entry, adir, &destination)
                    .map_err(|e| format!("Failed to restore audio file {name}: {e}"))?;
            }
        }
        Ok(())
    })
}

fn apply_full_restore(
    storage: &Storage,
    data: &Value,
    adir: &Path,
    install_recordings: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let empty_settings = Map::new();
    let settings = data.get("appSettings").and_then(Value::as_object).unwrap_or(&empty_settings);
    let array = |key| data.get(key).and_then(Value::as_array).map(Vec::as_slice);
    let rewritten_history = array("history").map(|history| {
        history.iter().map(|record| rewrite_audio_path(record, adir)).collect::<Vec<_>>()
    });
    let collections = [
        ("promptPresets", array("promptPresets")),
        ("appPromptRules", array("appPromptRules")),
        ("history", rewritten_history.as_deref()),
        ("manualCorrections", array("manualCorrections")),
        ("feedbackQueue", array("feedbackQueue")),
    ];

    // The same SQLite write path runs before and after the filesystem changes.
    storage.apply_restore_transaction(settings, MACHINE_LOCAL_KEYS, &collections, true)?;
    install_recordings()?;
    storage.apply_restore_transaction(settings, MACHINE_LOCAL_KEYS, &collections, false)
}

fn backup_json_entry_error(error: zip::result::ZipError) -> String {
    match error {
        zip::result::ZipError::FileNotFound => "The archive is missing backup.json and may not be a SayItLocally full backup".to_string(),
        _ => format!("Failed to read backup.json: {error}"),
    }
}

// [自訂] SC-51：先檢查整批目的地，避免後段的目錄或連結造成半套還原。
fn check_recording_restore_path(path: &Path, is_directory: bool) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_directory && !metadata.is_dir() => Err("BACKUP_AUDIO_DIRECTORY_UNSAFE".into()),
        Ok(metadata) if !is_directory && !metadata.is_file() => Err("BACKUP_AUDIO_DESTINATION_UNSAFE".into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

// [自訂] SC-51：只還原 audio/ 下的單層錄音檔；巢狀條目不展平以免覆蓋同名檔。
fn archive_audio_basename(name: &str, is_file: bool) -> Option<&str> {
    let rest = name.strip_prefix("audio/")?;
    (is_file && is_backup_audio_name(rest)).then_some(rest)
}

// [自訂] SC-51：先拒絕危險路徑，再套用任何錄音或資料庫內容。
fn is_backup_entry_path_safe(name: &str) -> bool {
    !name.starts_with('/')
        && !name.contains('\\')
        && !name.split('/').any(|part| part == "..")
        && !matches!(name.as_bytes(), [b'a'..=b'z' | b'A'..=b'Z', b':', ..])
}

// [自訂] SC-51：解壓與 CRC 檢查完成後再更換錄音，避免半寫與跟隨既有連結。
fn restore_audio_file(source: &mut impl Read, dir: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if !metadata.is_file() => return Err(std::io::Error::other("Recording destination is not a regular file")),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let temp = super::audio::restore_staging_path(dir);
    let mut staged = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
    let copied = std::io::copy(source, &mut staged);
    drop(staged);
    let result = copied.and_then(|_| fs::rename(&temp, destination));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// 导入完成后由前端调用，重启应用使内存中的设置/供应商状态全量重载。
#[tauri::command]
pub fn restart_app(app: tauri::AppHandle) {
    // 这次退出的意图是"重启回同一个版本"，不是更新。若让退出兜底安装跑起来，
    // 重启拉起来的会是正在被安装程序覆盖的 exe。
    crate::commands::system::suppress_exit_install();
    // [自訂] SC-60：restart 會先產生新程序再結束本程序，先放開實例鎖，新程序才能成為主程序。
    crate::instance_lock::release_for_restart();
    // [自訂] SC-62 補充（F10b）：在主執行緒 restart 時 Tauri 不送 RunEvent::Exit，這裡自己先寫完前端日誌。
    crate::commands::system::flush_debug_log(crate::commands::system::DEBUG_LOG_FLUSH_TIMEOUT);
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor, SeekFrom};

    const LEGACY_BACKUP: &[u8] = include_bytes!("testdata/legacy-backup-0.6.6.zip");

    fn temp_storage(tag: &str) -> (Storage, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sayit-backup-test-{}-{}", tag, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let storage = Storage::new(dir.join("test.db")).unwrap();
        (storage, dir)
    }

    fn test_backup(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            super::super::archive::write_deflated_entry(&mut writer, name, bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn backup_with_history(history: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "kind": "full",
            "formatVersion": 1,
            "history": history,
            "appSettings": { "overlayWidth": "short" }
        })).unwrap()
    }

    #[test]
    fn header_validation_failure_precedes_audio_and_history_writes() {
        use std::cell::Cell;

        struct FaultOnNextSeek<'a> {
            cursor: Cursor<&'a [u8]>,
            armed: &'a Cell<bool>,
        }
        impl Read for FaultOnNextSeek<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.cursor.read(buffer)
            }
        }
        impl Seek for FaultOnNextSeek<'_> {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                if self.armed.replace(false) {
                    return Err(io::Error::other("simulated validation I/O failure"));
                }
                self.cursor.seek(position)
            }
        }

        let (storage, dir) = temp_storage("header_validation");
        let audio = dir.join("audio");
        storage.set("history", &json!([{ "id": "existing", "timestamp": 1 }])).unwrap();
        let original_history = storage.get("history", None);
        let armed = Cell::new(false);
        let reader = FaultOnNextSeek { cursor: Cursor::new(LEGACY_BACKUP), armed: &armed };
        let archive = zip::ZipArchive::new(reader).unwrap();
        armed.set(true);
        let error = apply_full_backup_archive(&storage, archive, &audio).unwrap_err();
        assert!(error.starts_with("Invalid backup ZIP headers:"), "{error}");
        assert!(!audio.exists());
        assert_eq!(storage.get("history", None), original_history);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_path_preflight_rejects_nonportable_note_before_writes() {
        let (storage, dir) = temp_storage("path_preflight");
        let audio = dir.join("audio");
        storage.set("history", &json!([{ "id": "existing", "timestamp": 1 }])).unwrap();
        let original_history = storage.get("history", None);
        let json = backup_with_history(json!([]));
        let bytes = test_backup(&[
            ("backup.json", &json),
            ("audio/example.wav", b"recording"),
            ("notes\\readme.txt", b"note"),
        ]);
        let archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let error = apply_full_backup_archive(&storage, archive, &audio).unwrap_err();
        assert!(error.contains("unsafe entry path"), "{error}");
        assert!(!audio.exists());
        assert_eq!(storage.get("history", None), original_history);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_json_io_error_is_distinct_from_missing_entry() {
        use zip::result::ZipError;
        let missing = backup_json_entry_error(ZipError::FileNotFound);
        let unreadable = backup_json_entry_error(ZipError::Io(io::Error::other("test read failure")));
        assert!(missing.contains("missing backup.json"));
        assert!(unreadable.starts_with("Failed to read backup.json:"));
        assert!(unreadable.contains("test read failure"));
    }

    #[test]
    fn failed_recording_read_leaves_existing_audio_unchanged_and_removes_staging() {
        struct InterruptedRead(bool);
        impl Read for InterruptedRead {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "simulated CRC error"));
                }
                self.0 = true;
                buffer[..3].copy_from_slice(b"new");
                Ok(3)
            }
        }

        let dir = std::env::temp_dir().join(format!("sayit-restore-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let recording = dir.join("example.wav");
        fs::write(&recording, b"old").unwrap();
        let error = restore_audio_file(&mut InterruptedRead(false), &dir, &recording).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&recording).unwrap(), b"old");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_without_recordings_restores_settings_and_history_without_touching_audio_link() {
        let (storage, dir) = temp_storage("config_zip_link");
        let audio = dir.join("audio");
        let target = dir.join("other");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &audio).unwrap();
        let json = backup_with_history(json!([{ "id": "restored", "timestamp": 1 }]));
        let zip = test_backup(&[("backup.json", &json), ("audio/notes.txt", b"note")]);
        let archive = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        apply_full_backup_archive(&storage, archive, &audio).unwrap();
        assert_eq!(storage.get("overlayWidth", None), json!("short"));
        assert_eq!(storage.get("history", None).pointer("/0/id"), Some(&json!("restored")));
        assert_eq!(fs::read_link(&audio).unwrap(), target);
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_with_recordings_rejects_audio_directory_link_before_writes() {
        let (storage, dir) = temp_storage("audio_zip_link");
        let audio = dir.join("audio");
        let target = dir.join("other");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &audio).unwrap();
        storage.set("history", &json!([{ "id": "existing", "timestamp": 1 }])).unwrap();
        let original_history = storage.get("history", None);
        let archive = zip::ZipArchive::new(Cursor::new(LEGACY_BACKUP)).unwrap();
        let error = apply_full_backup_archive(&storage, archive, &audio).unwrap_err();
        assert_eq!(error, "BACKUP_AUDIO_DIRECTORY_UNSAFE");
        assert_eq!(storage.get("history", None), original_history);
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restores_legacy_zip_0_6_6_recording_and_rewrites_history_path() {
        let (storage, dir) = temp_storage("legacy_zip");
        let path = dir.join("legacy.zip");
        let audio = dir.join("audio");
        fs::write(&path, LEGACY_BACKUP).unwrap();
        apply_full_backup_in_dir(&storage, path.to_str().unwrap(), &audio).unwrap();

        let recording = audio.join("deferE032a.wav");
        let bytes = fs::read(&recording).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 46);
        assert_eq!(fs::read_dir(&audio).unwrap().count(), 1);
        let history = storage.get("history", None);
        assert_eq!(history.pointer("/0/id").and_then(Value::as_str), Some("deferE032a"));
        assert_eq!(history.pointer("/0/audioFilePath").and_then(Value::as_str), recording.to_str());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recording_entry_filter_requires_one_level_and_a_regular_audio_name() {
        assert_eq!(archive_audio_basename("audio/example.wav", true), Some("example.wav"));
        for name in [
            "../example.wav", "/audio/example.wav", "audio/../example.wav",
            "audio//example.wav", "audio/sub/example.wav", "audio/sub\\example.wav",
            "audio/中文.txt", "audio/",
        ] {
            assert_eq!(archive_audio_basename(name, true), None, "{name}");
        }
        assert_eq!(archive_audio_basename("audio/example.wav", false), None);
        for name in ["../example.wav", "/audio/example.wav", "audio/../example.wav", "audio/sub\\example.wav", "C:/example.wav"] {
            assert!(!is_backup_entry_path_safe(name), "{name}");
        }
        assert!(is_backup_entry_path_safe("audio/中文.txt"));
    }

    #[test]
    fn unreadable_archive_types_and_truncation_do_not_touch_history_or_audio() {
        let (storage, dir) = temp_storage("unsupported_zip");
        let backup = dir.join("backup.zip");
        let audio = dir.join("audio");
        let history = json!([{ "id": "existing", "timestamp": 1, "asrText": "local" }]);
        storage.set("history", &history).unwrap();
        let saved_history = storage.get("history", None);
        let cases: [(&[u8], &str); 3] = [
            (include_bytes!("testdata/encrypted-backup.zip"), "Encrypted backup"),
            (include_bytes!("testdata/bzip2-backup.zip"), "Unsupported compression"),
            (&LEGACY_BACKUP[..100], "Not a valid backup archive"),
        ];
        for (bytes, expected_error) in cases {
            fs::write(&backup, bytes).unwrap();
            let error = apply_full_backup_in_dir(&storage, backup.to_str().unwrap(), &audio).unwrap_err();
            assert!(error.contains(expected_error), "{error}");
            assert!(!audio.exists());
            assert_eq!(storage.get("history", None), saved_history);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_backup_json_is_reported_before_creating_audio_directory() {
        let (storage, dir) = temp_storage("missing_backup_json");
        let backup = dir.join("backup.zip");
        let audio = dir.join("audio");
        let mut writer = zip::ZipWriter::new(fs::File::create(&backup).unwrap());
        super::super::archive::write_deflated_entry(&mut writer, "note.txt", b"test").unwrap();
        writer.finish().unwrap();
        let error = apply_full_backup_in_dir(&storage, backup.to_str().unwrap(), &audio).unwrap_err();
        assert!(error.contains("missing backup.json"));
        assert!(!audio.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archive_preflight_rejects_links_and_case_conflicts_without_overwriting_history() {
        let (storage, dir) = temp_storage("conflicting_recordings");
        let backup = dir.join("backup.zip");
        let audio = dir.join("audio");
        storage.set("history", &json!([{ "id": "existing", "timestamp": 1 }])).unwrap();
        let saved_history = storage.get("history", None);
        let json = br#"{"kind":"full","formatVersion":1,"history":[]}"#;
        let mut writer = zip::ZipWriter::new(fs::File::create(&backup).unwrap());
        super::super::archive::write_deflated_entry(&mut writer, "backup.json", json).unwrap();
        let options = super::super::archive::file_options(zip::CompressionMethod::Stored, 4);
        writer.add_symlink("audio/ignored.wav", "another.wav", options).unwrap();
        writer.add_directory("audio/directory.wav/", options).unwrap();
        writer.finish().unwrap();
        let error = apply_full_backup_in_dir(&storage, backup.to_str().unwrap(), &audio).unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        assert!(!audio.exists());
        assert_eq!(storage.get("history", None), saved_history);

        let mut writer = zip::ZipWriter::new(fs::File::create(&backup).unwrap());
        super::super::archive::write_deflated_entry(&mut writer, "backup.json", json).unwrap();
        for name in ["audio/Example.wav", "audio/example.wav"] {
            writer.start_file(name, options).unwrap();
            writer.write_all(b"test").unwrap();
        }
        writer.finish().unwrap();
        let error = apply_full_backup_in_dir(&storage, backup.to_str().unwrap(), &audio).unwrap_err();
        assert!(error.contains("conflicting recording names"), "{error}");
        assert!(!audio.exists());
        assert_eq!(storage.get("history", None), saved_history);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preexisting_recording_link_cannot_write_outside_audio_directory() {
        let (storage, dir) = temp_storage("external_recording_link");
        let backup = dir.join("backup.zip");
        let audio = dir.join("audio");
        let target = dir.join("untouched.wav");
        fs::create_dir(&audio).unwrap();
        fs::write(&target, b"old").unwrap();
        storage.set("history", &json!([{ "id": "existing", "timestamp": 1 }])).unwrap();
        let history_before = storage.get("history", None);
        let json = backup_with_history(json!([]));
        let bytes = test_backup(&[
            ("backup.json", &json),
            ("audio/first.wav", b"new"),
            ("audio/blocked.wav", b"new"),
        ]);
        fs::write(&backup, bytes).unwrap();
        let earlier = audio.join("first.wav");
        let blocked = audio.join("blocked.wav");
        std::os::unix::fs::symlink(&target, &blocked).unwrap();
        let error = apply_full_backup_in_dir(&storage, backup.to_str().unwrap(), &audio).unwrap_err();
        assert_eq!(error, "BACKUP_AUDIO_DESTINATION_UNSAFE");
        assert!(!earlier.exists());
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(storage.get("history", None), history_before);
        fs::remove_file(&blocked).unwrap();
        fs::create_dir(&blocked).unwrap();
        let error = apply_full_backup_in_dir(&storage, backup.to_str().unwrap(), &audio).unwrap_err();
        assert_eq!(error, "BACKUP_AUDIO_DESTINATION_UNSAFE");
        assert!(!earlier.exists());
        assert_eq!(storage.get("history", None), history_before);
        fs::remove_dir_all(dir).unwrap();
    }

    const RESTORE_COLLECTIONS: [&str; 5] = [
        "promptPresets", "appPromptRules", "history", "manualCorrections", "feedbackQueue",
    ];

    fn seed_full_restore(storage: &Storage) -> Value {
        storage.set("overlayWidth", &json!("original")).unwrap();
        let mut data = json!({
            "kind": "full",
            "formatVersion": 1,
            "appSettings": { "overlayWidth": "restored" },
        });
        for key in RESTORE_COLLECTIONS {
            storage.set(key, &json!([{ "id": format!("old-{key}") }])).unwrap();
            data[key] = json!([{ "id": format!("new-{key}") }]);
        }
        data
    }

    fn assert_original_restore_data(storage: &Storage) {
        assert_eq!(storage.get("overlayWidth", None), json!("original"));
        for key in RESTORE_COLLECTIONS {
            assert_eq!(storage.get(key, None), json!([{ "id": format!("old-{key}") }]), "{key}");
        }
    }

    fn apply_test_restore(storage: &Storage, data: &Value, adir: &Path, audio: bool) -> Result<(), String> {
        let payload = data.to_string();
        let mut entries: Vec<(&str, &[u8])> = vec![("backup.json", payload.as_bytes())];
        if audio {
            entries.push(("audio/valid-recording.wav", b"sample recording"));
        }
        let archive = zip::ZipArchive::new(Cursor::new(test_backup(&entries))).unwrap();
        apply_full_backup_archive(storage, archive, adir)
    }

    #[test]
    fn duplicate_ids_in_each_collection_fail_before_any_recording_or_database_change() {
        let (storage, dir) = temp_storage("duplicate_ids");
        let original = seed_full_restore(&storage);
        let adir = dir.join("audio");
        for key in RESTORE_COLLECTIONS {
            let mut data = original.clone();
            data[key] = json!([{ "id": "same" }, { "id": "same" }]);
            assert!(apply_test_restore(&storage, &data, &adir, true).is_err(), "{key}");
            assert!(!adir.exists(), "{key}");
            assert_original_restore_data(&storage);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn two_missing_ids_fail_before_staging_cleanup_or_recording_overwrite() {
        let (storage, dir) = temp_storage("missing_ids");
        let mut data = seed_full_restore(&storage);
        data["feedbackQueue"] = json!([{ "status": "one" }, { "status": "two" }]);
        let adir = dir.join("audio");
        fs::create_dir(&adir).unwrap();
        let staging = super::super::audio::restore_staging_path(&adir);
        fs::write(&staging, b"an older staging file").unwrap();
        let recording = adir.join("valid-recording.wav");
        fs::write(&recording, b"local recording").unwrap();

        assert!(apply_test_restore(&storage, &data, &adir, true).is_err());
        assert_eq!(fs::read(&staging).unwrap(), b"an older staging file");
        assert_eq!(fs::read(&recording).unwrap(), b"local recording");
        assert_eq!(fs::read_dir(&adir).unwrap().count(), 2);
        assert_original_restore_data(&storage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deferred_constraint_in_late_collection_fails_before_installing_recordings() {
        let (storage, dir) = temp_storage("late_constraint");
        let data = seed_full_restore(&storage);
        create_feedback_constraint(&storage);
        let adir = dir.join("audio");
        assert!(apply_test_restore(&storage, &data, &adir, true).is_err());
        assert!(!adir.exists());
        assert_original_restore_data(&storage);
        fs::remove_dir_all(dir).unwrap();
    }

    fn create_feedback_constraint(storage: &Storage) {
        storage.db.lock().unwrap().execute_batch(
            "CREATE TABLE allowed_feedback (id TEXT PRIMARY KEY);
             CREATE TABLE feedback_parent (id TEXT REFERENCES allowed_feedback(id)
                 DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER demo_feedback_constraint AFTER INSERT ON feedback_queue
                 BEGIN INSERT INTO feedback_parent VALUES (NEW.id); END;",
        ).unwrap();
    }

    #[test]
    fn failed_second_commit_leaves_all_database_data_unchanged() {
        let (storage, dir) = temp_storage("second_commit");
        let data = seed_full_restore(&storage);
        let audio = dir.join("valid-recording.wav");
        let error = apply_full_restore(&storage, &data, &dir, || {
            fs::write(&audio, b"already installed").unwrap();
            create_feedback_constraint(&storage);
            Ok(())
        }).unwrap_err();

        assert_eq!(error, "BACKUP_RESTORE_DB|commit");
        assert_original_restore_data(&storage);
        assert_eq!(fs::read(audio).unwrap(), b"already installed");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_restore_preserves_absent_and_non_array_collections_and_clears_empty_arrays() {
        let (storage, dir) = temp_storage("collection_semantics");
        seed_full_restore(&storage);
        for (index, key) in RESTORE_COLLECTIONS.iter().enumerate() {
            let mut data = json!({
                "kind": "full", "formatVersion": 1,
                "appSettings": { "overlayWidth": "restored" },
            });
            data[RESTORE_COLLECTIONS[(index + 1) % RESTORE_COLLECTIONS.len()]] = json!({"ignored": true});
            apply_test_restore(&storage, &data, &dir, false).unwrap();
            assert_eq!(storage.get(key, None), json!([{ "id": format!("old-{key}") }]));
            let other = RESTORE_COLLECTIONS[(index + 1) % RESTORE_COLLECTIONS.len()];
            assert_eq!(storage.get(other, None), json!([{ "id": format!("old-{other}") }]));
        }
        let mut data = json!({ "kind": "full", "formatVersion": 1 });
        for key in RESTORE_COLLECTIONS {
            data[key] = json!([]);
        }
        apply_test_restore(&storage, &data, &dir, false).unwrap();
        for key in RESTORE_COLLECTIONS {
            assert_eq!(storage.get(key, None), json!([]));
        }
        assert_eq!(storage.get("overlayWidth", None), json!("restored"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_set_collection_preserves_rows_on_failed_replacement() {
        let (storage, dir) = temp_storage("replace_collection");
        seed_full_restore(&storage);
        assert!(storage.set("history", &json!([{ "id": "same" }, { "id": "same" }])).is_err());
        assert_original_restore_data(&storage);
        fs::remove_dir_all(dir).unwrap();
    }

    fn config_export_selection(mode: &str) -> ConfigExportSelection {
        ConfigExportSelection {
            mode: mode.to_owned(),
            hotword_group_ids: vec![],
            include_text_replacements: false,
            default_text_replacements: None,
            prompt_preset_ids: vec!["user-preset".into()],
        }
    }

    #[test]
    fn missing_table_or_invalid_row_aborts_all_applicable_export_modes() {
        let (storage, dir) = temp_storage("read_errors");
        let output = dir.join("backup.out");
        let adir = dir.join("audio");
        fs::write(&output, b"previous export").unwrap();
        let scope = BackupScope { include_history: true, include_audio: true };
        storage.set("promptPresets", &json!([{ "id": "user-preset" }, { "id": "bad" }])).unwrap();
        for (sql, expected) in [
            ("UPDATE prompt_presets SET raw_json = 'private invalid JSON' WHERE id = 'bad'", "|2"),
            ("UPDATE prompt_presets SET raw_json = x'11' WHERE id = 'bad'", "|2"),
            ("DROP TABLE prompt_presets", "promptPresets"),
        ] {
            storage.db.lock().unwrap().execute_batch(sql).unwrap();
            let errors = [
                read_config_export_choices(&storage).unwrap_err(),
                write_backup_archive_in_dir(&storage, scope, &output, &adir, &mut |_| {}).unwrap_err(),
                write_config_file(&storage, &config_export_selection("full"), &output).unwrap_err(),
                write_config_file(&storage, &config_export_selection("selected"), &output).unwrap_err(),
            ];
            for error in errors {
                assert!(error.contains("promptPresets") && error.contains(expected), "{error}");
                assert!(!error.contains("private invalid JSON"));
            }
            assert_eq!(fs::read(&output).unwrap(), b"previous export");
            assert_no_backup_parts(&dir);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_setting_json_omits_key_but_setting_io_failure_aborts_selected_export() {
        let (storage, dir) = temp_storage("bad_setting");
        storage.set("overlayWidth", &json!("long")).unwrap();
        storage.set("textReplacements", &json!([])).unwrap();
        storage.db.lock().unwrap().execute_batch(
            "UPDATE app_settings SET value_json = 'bad json' WHERE key = 'textReplacements'",
        ).unwrap();
        let scope = BackupScope { include_history: true, include_audio: false };
        for value in [build_config_value(&storage).unwrap(), build_full_value(&storage, scope).unwrap()] {
            assert!(value["appSettings"].get("textReplacements").is_none());
            assert_eq!(value["appSettings"]["overlayWidth"], json!("long"));
        }
        let mut selection = config_export_selection("selected");
        selection.prompt_preset_ids.clear();
        selection.include_text_replacements = true;
        selection.default_text_replacements = Some(vec![json!({ "from": "built-in rule", "to": "default" })]);
        let choices = read_config_export_choices(&storage).unwrap();
        assert!(choices.get("textReplacements").is_none());
        assert_eq!(build_selected_config_value(&storage, &selection).unwrap_err(), "BACKUP_NOTHING_SELECTED");
        storage.db.lock().unwrap().execute_batch(
            "UPDATE app_settings SET value_json = x'3132' WHERE key = 'textReplacements'",
        ).unwrap();
        for value in [build_config_value(&storage).unwrap(), build_full_value(&storage, scope).unwrap()] {
            assert!(value["appSettings"].get("textReplacements").is_none());
            assert_eq!(value["appSettings"]["overlayWidth"], json!("long"));
        }
        assert!(read_config_export_choices(&storage).unwrap().get("textReplacements").is_none());
        assert_eq!(build_selected_config_value(&storage, &selection).unwrap_err(), "BACKUP_NOTHING_SELECTED");
        storage.db.lock().unwrap().execute_batch("DROP TABLE app_settings").unwrap();
        let output = dir.join("config.json");
        assert!(write_config_file(&storage, &selection, &output).unwrap_err().contains("textReplacements"));
        assert!(!output.exists());
        assert_no_backup_parts(&dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_settings_table_aborts_both_full_export_formats() {
        let (storage, dir) = temp_storage("missing_settings");
        storage.db.lock().unwrap().execute_batch("DROP TABLE app_settings").unwrap();
        let config = dir.join("config.json");
        let archive = dir.join("backup.zip");
        let scope = BackupScope { include_history: true, include_audio: false };
        assert_eq!(
            write_config_file(&storage, &config_export_selection("full"), &config),
            Err("BACKUP_READ_SETTINGS".into()),
        );
        assert_eq!(
            write_backup_archive_in_dir(&storage, scope, &archive, &dir.join("audio"), &mut |_| {}),
            Err("BACKUP_READ_SETTINGS".into()),
        );
        assert!(!config.exists());
        assert!(!archive.exists());
        assert_no_backup_parts(&dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selected_export_reads_current_replacements_and_uses_defaults_only_for_empty_settings() {
        let (storage, dir) = temp_storage("selected_snapshot");
        let mut selection = config_export_selection("selected");
        selection.prompt_preset_ids.clear();
        selection.include_text_replacements = true;
        selection.default_text_replacements = Some(vec![json!({ "from": "built-in", "to": "default" })]);
        assert_eq!(build_selected_config_value(&storage, &selection).unwrap()["items"]["textReplacements"][0]["from"], "built-in");
        storage.set("textReplacements", &json!([])).unwrap();
        assert_eq!(build_selected_config_value(&storage, &selection).unwrap()["items"]["textReplacements"][0]["from"], "built-in");
        storage.set("textReplacements", &json!([{ "from": "stored", "to": "current" }])).unwrap();
        assert_eq!(build_selected_config_value(&storage, &selection).unwrap()["items"]["textReplacements"][0]["from"], "stored");
        assert_eq!(read_config_export_choices(&storage).unwrap()["textReplacements"][0]["from"], "stored");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_snapshot_survives_an_external_writer_between_reads() {
        let (storage, dir) = temp_storage("snapshot");
        storage.set("overlayWidth", &json!("before")).unwrap();
        storage.set("history", &json!([{ "id": "before" }])).unwrap();
        let writer = Storage::new(dir.join("test.db")).unwrap();
        storage.with_backup_snapshot(|snapshot| {
            assert_eq!(snapshot.settings(MACHINE_LOCAL_KEYS)?["overlayWidth"], json!("before"));
            writer.set("overlayWidth", &json!("after")).unwrap();
            writer.set("history", &json!([{ "id": "after" }])).unwrap();
            assert_eq!(snapshot.collection("history")?, json!([{ "id": "before" }]));
            Ok(())
        }).unwrap();
        assert_eq!(storage.get("history", None), json!([{ "id": "after" }]));
        fs::remove_dir_all(dir).unwrap();
    }

    fn assert_no_backup_parts(dir: &Path) {
        assert!(!fs::read_dir(dir).unwrap().any(|entry| {
            entry.unwrap().path().extension().is_some_and(|ext| ext == "part")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn audio_only_scope_includes_unreferenced_recordings_and_ignores_non_audio_entries() {
        use std::os::unix::fs::symlink;

        let (storage, dir) = temp_storage("audio_filter");
        let adir = dir.join("audio");
        fs::create_dir(&adir).unwrap();
        fs::write(adir.join("real-1.wav"), b"real").unwrap();
        fs::write(adir.join("readme.txt"), b"note").unwrap();
        fs::write(adir.join("wrong.name.wav"), b"not a recording id").unwrap();
        fs::write(super::super::audio::restore_staging_path(&adir), b"staging").unwrap();
        fs::create_dir(adir.join("nested.wav")).unwrap();
        fs::write(adir.join("nested.wav/inner.wav"), b"nested").unwrap();
        symlink(adir.join("real-1.wav"), adir.join("link.wav")).unwrap();

        let scope = BackupScope { include_history: false, include_audio: true };
        let output = dir.join("backup.zip");
        let mut totals = Vec::new();
        write_backup_archive_in_dir(&storage, scope, &output, &adir, &mut |progress| {
            totals.push((progress.total_files, progress.total_bytes, progress.processed_bytes));
        }).unwrap();
        assert!(totals.iter().all(|(files, bytes, _)| *files == 1 && *bytes == 4));
        assert_eq!(totals.last(), Some(&(1, 4, 4)));
        let mut archive = zip::ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
        let names: HashSet<_> = archive.file_names().map(|name| name.unwrap().into_owned()).collect();
        assert_eq!(names, HashSet::from(["backup.json".to_owned(), "audio/real-1.wav".to_owned()]));
        let payload: Value = serde_json::from_reader(archive.by_name("backup.json").unwrap()).unwrap();
        assert!(payload.get("history").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linked_audio_directory_can_be_backed_up_but_not_restored_to() {
        use std::os::unix::fs::symlink;

        let (storage, dir) = temp_storage("linked_audio");
        let target = dir.join("recordings");
        let audio = dir.join("audio");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("real.wav"), b"recording").unwrap();
        symlink("real.wav", target.join("link.wav")).unwrap();
        symlink(&target, &audio).unwrap();

        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: false, include_audio: true };
        write_backup_archive_in_dir(&storage, scope, &output, &audio, &mut |_| {}).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        let mut bytes = Vec::new();
        archive.by_name("audio/real.wav").unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"recording");
        assert!(archive.by_name("audio/link.wav").is_err());
        assert_eq!(apply_full_backup_archive(&storage, archive, &audio), Err("BACKUP_AUDIO_DIRECTORY_UNSAFE".into()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_recording_directory_is_empty_but_other_scan_errors_fail_the_backup() {
        let (storage, dir) = temp_storage("audio_list");
        let invalid = dir.join("not-a-directory");
        fs::write(&invalid, b"file").unwrap();
        let missing = dir.join("absent");
        assert!(collect_audio_export_files(true, &missing).unwrap().is_empty());
        assert!(collect_audio_export_files(false, &invalid).unwrap().is_empty());
        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: false, include_audio: true };
        for adir in [&invalid, &invalid.join("audio")] {
            assert!(write_backup_archive_in_dir(&storage, scope, &output, adir, &mut |_| {}).is_err());
            assert!(!output.exists());
            assert_no_backup_parts(&dir);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_linked_audio_directory_aborts_recording_backup() {
        let (storage, dir) = temp_storage("dangling_audio");
        let audio = dir.join("audio");
        std::os::unix::fs::symlink(dir.join("unavailable"), &audio).unwrap();
        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: false, include_audio: true };
        assert_eq!(
            write_backup_archive_in_dir(&storage, scope, &output, &audio, &mut |_| {}),
            Err("BACKUP_READ_RECORDINGS".into()),
        );
        assert!(!output.exists());
        assert_no_backup_parts(&dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_recording_directory_fails_instead_of_exporting_an_empty_backup() {
        use std::os::unix::fs::PermissionsExt;

        let (storage, dir) = temp_storage("unreadable_audio");
        let adir = dir.join("audio");
        fs::create_dir(&adir).unwrap();
        fs::set_permissions(&adir, fs::Permissions::from_mode(0)).unwrap();
        // Privileged test runners may be allowed to read the directory regardless of its mode.
        if fs::read_dir(&adir).is_err() {
            let output = dir.join("backup.zip");
            let scope = BackupScope { include_history: false, include_audio: true };
            let result = write_backup_archive_in_dir(&storage, scope, &output, &adir, &mut |_| {});
            fs::set_permissions(&adir, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(result.unwrap_err(), "BACKUP_READ_RECORDINGS");
            assert!(!output.exists());
            assert_no_backup_parts(&dir);
        }
        fs::set_permissions(&adir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_recording_metadata_aborts_the_backup() {
        use std::os::unix::fs::PermissionsExt;

        let (storage, dir) = temp_storage("unreadable_recording_metadata");
        let audio = dir.join("audio");
        fs::create_dir(&audio).unwrap();
        fs::write(audio.join("recording.wav"), b"recording").unwrap();
        fs::set_permissions(&audio, fs::Permissions::from_mode(0o400)).unwrap();
        let entry = fs::read_dir(&audio).unwrap().next().unwrap().unwrap();
        // A privileged runner may bypass directory search permissions.
        if entry.metadata().is_err() {
            let output = dir.join("backup.zip");
            let scope = BackupScope { include_history: false, include_audio: true };
            let result = write_backup_archive_in_dir(&storage, scope, &output, &audio, &mut |_| {});
            fs::set_permissions(&audio, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(result, Err("BACKUP_READ_RECORDINGS".into()));
            assert!(!output.exists());
            assert_no_backup_parts(&dir);
        }
        fs::set_permissions(&audio, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn poisoned_database_lock_fails_backup_instead_of_recovering_silently() {
        let (storage, dir) = temp_storage("backup_lock");
        let _ = std::panic::catch_unwind(|| {
            let _guard = storage.db.lock().unwrap();
            panic!("simulate interrupted database lock holder");
        });
        let output = dir.join("config.json");
        assert_eq!(write_config_file(&storage, &config_export_selection("full"), &output).unwrap_err(), "BACKUP_READ_DATABASE");
        assert!(!output.exists());
        assert_no_backup_parts(&dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn poisoned_database_lock_stops_restore_before_installing_recordings() {
        let (storage, dir) = temp_storage("restore_lock");
        let _ = std::panic::catch_unwind(|| {
            let _guard = storage.db.lock().unwrap();
            panic!("simulate interrupted database lock holder");
        });
        let audio = dir.join("audio");
        let backup = test_backup(&[
            ("backup.json", &backup_with_history(json!([]))),
            ("audio/recording.wav", b"recording"),
        ]);
        let archive = zip::ZipArchive::new(Cursor::new(backup)).unwrap();
        assert_eq!(apply_full_backup_archive(&storage, archive, &audio), Err("BACKUP_RESTORE_DB|lock".into()));
        assert!(!audio.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_config_import_rolls_back_settings_and_both_collections_on_write_failure() {
        let (storage, dir) = temp_storage("config_transaction");
        for (key, value) in [
            ("overlayWidth", json!("long")),
            ("promptPresets", json!([{ "id": "old-preset" }])),
            ("appPromptRules", json!([{ "id": "old-rule" }])),
        ] {
            storage.set(key, &value).unwrap();
        }
        let config = json!({
            "appSettings": { "overlayWidth": "short" },
            "promptPresets": [{ "id": "new-preset" }],
            "appPromptRules": [{ "id": "duplicate" }, { "id": "duplicate" }],
        });
        assert!(apply_config_part(&storage, &config, CONFIG_EXCLUDE).is_err());
        assert_eq!(storage.get("overlayWidth", None), json!("long"));
        assert_eq!(storage.get("promptPresets", None)[0]["id"], "old-preset");
        assert_eq!(storage.get("appPromptRules", None)[0]["id"], "old-rule");
        fs::remove_dir_all(dir).unwrap();
    }

    fn volatile_storage(dir: &Path) -> Storage {
        // A directory at the database path makes the real startup fall back to memory.
        let blocked = dir.join("blocked.db");
        fs::create_dir_all(&blocked).unwrap();
        let (storage, mode) = Storage::open_or_recover(blocked);
        assert!(matches!(mode, Some(crate::storage::StorageRecovery::InMemory)));
        storage.set("overlayWidth", &json!("long")).unwrap();
        storage.set("history", &json!([{ "id": "kept" }])).unwrap();
        storage
    }

    fn assert_volatile_storage_unchanged(storage: &Storage) {
        assert_eq!(storage.get("overlayWidth", None), json!("long"));
        assert_eq!(storage.get("history", None), json!([{ "id": "kept" }]));
        assert_eq!(storage.get("customHotwordThemes", None), Value::Null);
    }

    #[test]
    fn volatile_storage_rejects_a_full_restore_before_reading_or_writing_anything() {
        let (disk, dir) = temp_storage("volatile_full");
        let storage = volatile_storage(&dir);
        let audio = dir.join("audio");
        let path = dir.join("backup.zip");
        let backup = test_backup(&[
            ("backup.json", &backup_with_history(json!([{ "id": "new" }]))),
            ("audio/new.wav", b"audio"),
        ]);
        fs::write(&path, backup).unwrap();
        // A missing file gives the same answer, so nothing was opened first.
        for source in [path.clone(), dir.join("missing.zip")] {
            assert_eq!(
                apply_full_backup_in_dir(&storage, source.to_str().unwrap(), &audio),
                Err(crate::storage::VOLATILE_STORAGE_ERROR.into()),
            );
            assert_eq!(
                apply_full_backup(&storage, source.to_str().unwrap()),
                Err(crate::storage::VOLATILE_STORAGE_ERROR.into()),
            );
        }
        assert!(!audio.exists());
        assert_volatile_storage_unchanged(&storage);

        apply_full_backup_in_dir(&disk, path.to_str().unwrap(), &audio).unwrap();
        assert_eq!(disk.get("history", None)[0]["id"], "new");
        assert!(audio.join("new.wav").is_file());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn volatile_storage_rejects_every_configuration_import_before_reading_the_file() {
        let (disk, dir) = temp_storage("volatile_config");
        let storage = volatile_storage(&dir);
        let full = json!({
            "kind": "config", "formatVersion": 1,
            "appSettings": { "overlayWidth": "short" },
            "promptPresets": [], "appPromptRules": [],
        });
        let legacy = json!({
            "kind": "config", "formatVersion": 1, "appSettings": { "overlayWidth": "short" },
        });
        let selected = json!({
            "kind": "config", "formatVersion": 2, "scope": "selected",
            "items": { "textReplacements": [{ "id": "r", "from": "a", "to": "b", "enabled": true }] },
        });
        for (name, config) in [("full.json", full), ("legacy.json", legacy), ("selected.json", selected)] {
            let path = dir.join(name);
            fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
            let path = path.to_str().unwrap();
            let (data, fingerprint) = read_config_file(path).unwrap();
            // The token is valid, so only the storage check can refuse this import.
            let token = config_import_token(&storage, &data, &fingerprint).unwrap();
            assert_eq!(
                apply_config_import(&storage, path, &token).err().as_deref(),
                Some(crate::storage::VOLATILE_STORAGE_ERROR), "{name}",
            );
            let token = config_import_token(&disk, &data, &fingerprint).unwrap();
            assert!(apply_config_import(&disk, path, &token).unwrap().requires_restart, "{name}");
        }
        assert_eq!(
            apply_config_import(&storage, dir.join("missing.json").to_str().unwrap(), "").err().as_deref(),
            Some(crate::storage::VOLATILE_STORAGE_ERROR),
        );
        assert_volatile_storage_unchanged(&storage);
        assert!(storage.get("textReplacements", None).is_null());
        assert_eq!(disk.get("overlayWidth", None), json!("short"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_replacement_disk_database_accepts_restores() {
        let dir = std::env::temp_dir().join(format!("sayit-backup-replacement-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let db = dir.join("sayit.db");
        fs::write(&db, b"damaged test database").unwrap();
        let (storage, mode) = Storage::open_or_recover(db);
        assert!(matches!(mode, Some(crate::storage::StorageRecovery::MovedAside { .. })));
        let path = dir.join("backup.zip");
        fs::write(&path, test_backup(&[("backup.json", &backup_with_history(json!([{ "id": "new" }])))])).unwrap();
        apply_full_backup_in_dir(&storage, path.to_str().unwrap(), &dir.join("audio")).unwrap();
        assert_eq!(storage.get("history", None)[0]["id"], "new");
        drop(storage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn importing_configuration_and_a_full_archive_preserves_the_recording_hold() {
        use crate::storage::{recovery::recording_cleanup_protected, StorageRecovery};

        let dir = std::env::temp_dir().join(format!("sayit-backup-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let db = dir.join("sayit.db");
        fs::write(&db, b"damaged test database").unwrap();
        let (storage, mode) = Storage::open_or_recover(db);
        assert!(matches!(mode, Some(StorageRecovery::MovedAside { .. })));
        let config = json!({ "appSettings": { "overlayWidth": "long", "audioRetentionDays": 90 } });
        apply_config_part(&storage, &config, CONFIG_EXCLUDE).unwrap();
        assert_eq!(storage.get("overlayWidth", None), json!("long"));
        assert_eq!(storage.get("audioRetentionDays", None), Value::Null);
        assert!(recording_cleanup_protected(&dir));

        let data = serde_json::to_vec(&json!({
            "kind": "full", "formatVersion": 1, "appSettings": { "overlayWidth": "short", "audioRetentionDays": 30 }
        })).unwrap();
        let bundle = test_backup(&[("backup.json", &data)]);
        let archive = zip::ZipArchive::new(Cursor::new(bundle)).unwrap();
        apply_full_backup_archive(&storage, archive, &dir.join("audio")).unwrap();
        assert_eq!(storage.get("overlayWidth", None), json!("short"));
        assert_eq!(storage.get("audioRetentionDays", None), Value::Null);
        assert!(recording_cleanup_protected(&dir));
        drop(storage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recording_size_changes_abort_export_without_leaving_a_file() {
        let (storage, dir) = temp_storage("recording_size_changes");
        let audio = dir.join("audio");
        fs::create_dir(&audio).unwrap();
        let recording = audio.join("recording.wav");
        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: false, include_audio: true };
        for new_bytes in [b"bigger".as_slice(), b"x".as_slice()] {
            fs::write(&recording, b"wav").unwrap();
            let mut wrote_audio = false;
            let result = write_backup_archive_in_dir(&storage, scope, &output, &audio, &mut |progress| {
                if progress.phase == "packingData" {
                    fs::write(&recording, new_bytes).unwrap();
                }
                wrote_audio |= progress.phase == "packingAudio";
            });
            assert_eq!(result, Err("BACKUP_RECORDING_CHANGED".into()), "{new_bytes:?}");
            if new_bytes.len() > 3 {
                assert!(!wrote_audio, "Growing input must be rejected before writing excess data");
            }
            assert!(!output.exists());
            assert_no_backup_parts(&dir);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_nonregular_recording_after_listing_aborts_without_publishing() {
        use std::ffi::CString;
        use std::os::unix::{ffi::OsStrExt, fs::symlink};

        let (storage, dir) = temp_storage("recording_replaced");
        let audio = dir.join("audio");
        fs::create_dir(&audio).unwrap();
        fs::write(dir.join("sample"), b"wav").unwrap();
        let recording = audio.join("recording.wav");
        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: false, include_audio: true };
        for fifo in [false, true] {
            fs::write(&recording, b"wav").unwrap();
            let result = write_backup_archive_in_dir(&storage, scope, &output, &audio, &mut |progress| {
                if progress.phase == "packingData" {
                    fs::remove_file(&recording).unwrap();
                    if fifo {
                        let path = CString::new(recording.as_os_str().as_bytes()).unwrap();
                        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                    } else {
                        symlink(dir.join("sample"), &recording).unwrap();
                    }
                }
            });
            assert_eq!(result, Err("BACKUP_READ_RECORDINGS".into()));
            assert!(!output.exists());
            assert_no_backup_parts(&dir);
            fs::remove_file(&recording).unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn audio_read_failure_after_starting_export_never_publishes_a_partial_backup() {
        let (storage, dir) = temp_storage("audio_io");
        let adir = dir.join("audio");
        fs::create_dir(&adir).unwrap();
        let audio = adir.join("recording.wav");
        fs::write(&audio, b"recording").unwrap();
        let output = dir.join("backup.zip");
        fs::write(&output, b"previous backup").unwrap();
        let scope = BackupScope { include_history: false, include_audio: true };
        let error = write_backup_archive_in_dir(&storage, scope, &output, &adir, &mut |progress| {
            if progress.phase == "packingData" {
                fs::remove_file(&audio).unwrap();
            }
        }).unwrap_err();
        assert_eq!(error, "BACKUP_READ_RECORDINGS");
        assert_eq!(fs::read(&output).unwrap(), b"previous backup");
        assert_no_backup_parts(&dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completed_exports_do_not_replace_existing_backups_or_leave_partial_files() {
        let (storage, dir) = temp_storage("publish_collision");
        let config = dir.join("config.json");
        write_config_file(&storage, &config_export_selection("full"), &config).unwrap();
        let previous = fs::read(&config).unwrap();
        storage.set("overlayWidth", &json!("changed")).unwrap();
        assert_eq!(write_config_file(&storage, &config_export_selection("full"), &config), Err("BACKUP_EXPORT_EXISTS".into()));
        assert_eq!(fs::read(&config).unwrap(), previous);
        let zip = dir.join("backup.zip");
        let scope = BackupScope { include_history: false, include_audio: false };
        write_backup_archive_in_dir(&storage, scope, &zip, &dir, &mut |_| {}).unwrap();
        let previous = fs::read(&zip).unwrap();
        storage.set("overlayWidth", &json!("changed again")).unwrap();
        assert!(write_backup_archive_in_dir(&storage, scope, &zip, &dir, &mut |_| {}).is_err());
        assert_eq!(fs::read(&zip).unwrap(), previous);
        assert_no_backup_parts(&dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_publication_renames_the_complete_file() {
        let (_storage, dir) = temp_storage("unsupported_publication");
        let source = dir.join("backup.part");
        let output = dir.join("backup.zip");
        #[cfg(target_os = "macos")]
        let unsupported = [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL].map(io::Error::from_raw_os_error);
        #[cfg(not(target_os = "macos"))]
        let unsupported = [io::ErrorKind::Unsupported, io::ErrorKind::InvalidInput].map(io::Error::from);
        for error in unsupported {
            fs::write(&source, b"complete backup").unwrap();
            publish_with_rename_fallback(&source, &output, || Err(error)).unwrap();
            assert_eq!(fs::read(&output).unwrap(), b"complete backup");
            assert!(!source.exists());
            fs::remove_file(&output).unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unsupported_codes_do_not_depend_on_rust_error_kinds() {
        // `Uncategorized` is unstable, so `Other` stands in for an older Rust's ENOTSUP kind.
        let other = io::ErrorKind::Other;
        assert!(is_unsupported_publication(other, Some(45)), "ENOTSUP");
        assert!(is_unsupported_publication(other, Some(102)), "EOPNOTSUPP");
        for code in [None, Some(libc::EACCES), Some(libc::EEXIST), Some(libc::EXDEV)] {
            assert!(!is_unsupported_publication(other, code));
        }
        assert!(is_unsupported_publication(io::ErrorKind::Unsupported, None));
        assert!(is_unsupported_publication(io::ErrorKind::InvalidInput, None));
        assert!(!is_unsupported_publication(io::ErrorKind::PermissionDenied, Some(libc::EACCES)));
    }

    #[test]
    fn publication_fallback_checks_the_raw_os_code() {
        let (_storage, dir) = temp_storage("fallback_raw_code");
        let source = dir.join("backup.part");
        let output = dir.join("backup.zip");
        fs::write(&source, b"complete backup").unwrap();
        let by_code = |_kind, code| code == Some(45);
        let error = publish_with_fallback_check(&source, &output, || Err(io::Error::from_raw_os_error(13)), by_code)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(13));
        assert!(!output.exists());
        publish_with_fallback_check(&source, &output, || Err(io::Error::from_raw_os_error(45)), by_code).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"complete backup");
        assert!(!source.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publication_fallback_preserves_an_existing_destination() {
        let (_storage, dir) = temp_storage("fallback_collision");
        let source = dir.join("backup.part");
        let output = dir.join("backup.zip");
        fs::write(&source, b"new backup").unwrap();
        fs::write(&output, b"old backup").unwrap();
        let unsupported = || Err(io::ErrorKind::Unsupported.into());
        let error = publish_with_rename_fallback(&source, &output, unsupported).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).unwrap(), b"old backup");
        assert_eq!(fs::read(&source).unwrap(), b"new backup");
        #[cfg(unix)]
        {
            fs::remove_file(&output).unwrap();
            std::os::unix::fs::symlink("missing-backup", &output).unwrap();
            let error = publish_with_rename_fallback(&source, &output, unsupported).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read_link(&output).unwrap(), Path::new("missing-backup"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publication_errors_other_than_unsupported_do_not_fall_back() {
        let (_storage, dir) = temp_storage("publication_error");
        let source = dir.join("backup.part");
        let output = dir.join("backup.zip");
        fs::write(&source, b"complete backup").unwrap();
        for kind in [io::ErrorKind::AlreadyExists, io::ErrorKind::PermissionDenied] {
            let error = publish_with_rename_fallback(&source, &output, || Err(kind.into())).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(!output.exists());
            assert_eq!(fs::read(&source).unwrap(), b"complete backup");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn model_directory_stays_local_for_full_and_selected_transfers() {
        let (storage, dir) = temp_storage("model_dir");
        let local = json!("/example/local/model-files");
        let old = json!("/example/older/model-files");
        let key = "localAsr.modelsDir";
        storage.set(key, &local).unwrap();
        let full = build_full_value(&storage, BackupScope { include_history: true, include_audio: false }).unwrap();
        let config = build_config_value(&storage).unwrap();
        assert!(full["appSettings"].get(key).is_none());
        assert!(config["appSettings"].get(key).is_none());
        let mut selection = config_export_selection("selected");
        selection.include_text_replacements = true;
        let selected = build_selected_config_value(&storage, &selection).unwrap();
        assert!(!selected.to_string().contains(local.as_str().unwrap()));

        let mut old_full = full;
        old_full["appSettings"][key] = old.clone();
        apply_test_restore(&storage, &old_full, &dir, false).unwrap();
        assert_eq!(storage.get(key, None), local);
        let mut old_config = config;
        old_config["appSettings"][key] = old.clone();
        apply_config_part(&storage, &old_config, CONFIG_EXCLUDE).unwrap();
        assert_eq!(storage.get(key, None), local);

        let old_selected = json!({
            "kind": "config", "formatVersion": 2, "scope": "selected",
            "appSettings": { (key): old },
            "items": { "textReplacements": [{ "from": "one", "to": "two" }] },
        });
        let plan = build_selected_import_plan(&storage, &old_selected).unwrap();
        storage.apply_config_transaction(&plan.app_settings, CONFIG_EXCLUDE, plan.prompt_presets.as_deref(), None).unwrap();
        assert_eq!(storage.get(key, None), local);
        assert_eq!(storage.get("textReplacements", None)[0]["from"], "one");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_stores_recordings_without_packaging_restore_staging() {
        let (storage, dir) = temp_storage("export_zip");
        let output = dir.join("backup.zip");
        let audio = audio_dir();
        fs::create_dir_all(&audio).unwrap();
        let suffix = uuid::Uuid::new_v4();
        let recording = audio.join(format!("archive-test-{suffix}.wav"));
        let stage = super::super::audio::restore_staging_path(&audio);
        fs::write(&recording, b"recording").unwrap();
        fs::write(&stage, b"interrupted").unwrap();
        assert!(!collect_audio_export_files(true, &audio).unwrap().iter().any(|file| file.path == stage));
        assert!(!super::super::audio::retained_recordings().unwrap().contains(&stage));

        let scope = BackupScope { include_history: false, include_audio: true };
        write_backup_archive_in_dir(&storage, scope, &output, &audio, &mut |_| {}).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        let recording_name = format!("audio/{}", recording.file_name().unwrap().to_str().unwrap());
        let stage_name = format!("audio/{}", stage.file_name().unwrap().to_str().unwrap());
        assert!(archive.by_name(&stage_name).is_err());
        assert_eq!(archive.by_name(&recording_name).unwrap().compression(), zip::CompressionMethod::Stored);
        fs::remove_file(&recording).unwrap();
        fs::remove_file(&stage).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    /// 不含历史时，`history` 字段必须**整个缺席**，不能是空数组。
    ///
    /// 这是「只备份配置」这一档能安全恢复的全部依据：`apply_full_backup` 用
    /// `if let Some(history)` 判断，字段缺席就保留本机历史，写成空数组则会把本机
    /// 历史整表清空 —— 一个「只想同步设置」的用户会因此丢掉全部记录，而且备份和
    /// 恢复都不会报任何错。
    #[test]
    fn config_only_backup_omits_history_instead_of_emptying_it() {
        let (storage, dir) = temp_storage("scope");
        storage
            .set(
                "history",
                &json!([{ "id": "h1", "timestamp": 1, "charCount": 3 }]),
            )
            .unwrap();

        let config_only = build_full_value(
            &storage,
            BackupScope {
                include_history: false,
                include_audio: false,
            },
        ).unwrap();
        assert!(
            config_only.get("history").is_none(),
            "history must be absent, got {:?}",
            config_only.get("history")
        );
        assert!(config_only.get("manualCorrections").is_none());
        assert!(config_only.get("feedbackQueue").is_none());
        // 配置部分照旧在，且 kind 仍是 full —— 用现有的导入路径就能恢复。
        assert_eq!(config_only.get("kind").and_then(Value::as_str), Some("full"));
        assert!(config_only.get("appSettings").is_some());
        assert_eq!(
            config_only.pointer("/scope/includeAudio").and_then(Value::as_bool),
            Some(false)
        );

        let with_history = build_full_value(
            &storage,
            BackupScope {
                include_history: true,
                include_audio: false,
            },
        ).unwrap();
        assert_eq!(
            with_history
                .get("history")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_exclude_covers_every_machine_local_key() {
        for key in MACHINE_LOCAL_KEYS {
            assert!(CONFIG_EXCLUDE.contains(key), "CONFIG_EXCLUDE is missing {key}");
        }
    }

    /// 恶意备份的两条腿：设置里塞 pendingUpdate / backendBaseUrl，录音目录里塞 exe。
    /// 导入完整备份时设置部分必须跳过本机专属 key，其余设置照常写入。
    #[test]
    fn full_backup_does_not_import_machine_local_settings() {
        let (storage, dir) = temp_storage("machine-local");
        storage.set("deviceId", &json!("local-device")).unwrap();
        let data = json!({
            "appSettings": {
                "pendingUpdate": { "version": "9.9.9", "filePath": "C:\\x\\audio\\evil.exe" },
                "backendBaseUrl": "https://attacker.example",
                "deviceId": "someone-else",
                "overlayWidth": "long",
            }
        });
        apply_config_part(&storage, &data, MACHINE_LOCAL_KEYS).unwrap();

        assert!(storage.get("pendingUpdate", None).is_null());
        assert!(storage.get("backendBaseUrl", None).is_null());
        assert_eq!(storage.get("deviceId", None), json!("local-device"));
        assert_eq!(storage.get("overlayWidth", None), json!("long"));

        let exported = build_full_value(&storage, BackupScope { include_history: false, include_audio: false }).unwrap();
        let settings = exported.get("appSettings").and_then(Value::as_object).unwrap();
        assert!(!settings.contains_key("deviceId"));
        assert!(settings.contains_key("overlayWidth"));

        let _ = fs::remove_dir_all(&dir);
    }

    /// [自訂] S35／S36／S44：匯出不帶金鑰、密碼與使用統計；匯入不覆寫隱私開關與雲端設定。
    #[test]
    fn hardening_keys_are_never_exported_or_imported() {
        let (storage, dir) = temp_storage("hardening");
        storage.set("cloudAi.apiKey", &json!("sk-secret")).unwrap();
        storage.set("cloudAi.profiles", &json!([{ "apiKey": "sk-secret" }])).unwrap();
        storage.set("cloudAsr.doubao.consoleKey", &json!("secret")).unwrap();
        storage.set("webdav.password", &json!("secret")).unwrap();
        storage.set("userStats", &json!({ "totalWords": 3 })).unwrap();
        storage.set("aiEnabled", &json!(false)).unwrap();
        storage.set("overlayWidth", &json!("long")).unwrap();

        for exported in [
            build_config_value(&storage).unwrap(),
            build_full_value(&storage, BackupScope { include_history: false, include_audio: false }).unwrap(),
        ] {
            let settings = exported.get("appSettings").and_then(Value::as_object).unwrap();
            for key in settings.keys() {
                assert!(
                    !key.starts_with("cloudAi.") && !key.starts_with("cloudAsr.") && !key.starts_with("webdav."),
                    "exported secret-bearing key {key}"
                );
            }
            assert!(!settings.contains_key("userStats"));
            assert!(!settings.contains_key("stats"));
            assert!(settings.contains_key("overlayWidth"));
        }

        let data = json!({
            "appSettings": {
                "aiEnabled": true,
                "historyEnabled": true,
                "workMode": "server",
                "autoLaunch": true,
                "cloudAi.provider": "deepseek",
                "cloudAi.apiUrl": "https://api.deepseek.com",
                "webdav.enabled": true,
                "localAsr.downloadSource": "HuggingFace Mirror",
                "overlayWidth": "short",
            }
        });
        for exclude in [CONFIG_EXCLUDE, MACHINE_LOCAL_KEYS] {
            apply_config_part(&storage, &data, exclude).unwrap();
            assert_eq!(storage.get("aiEnabled", None), json!(false));
            assert!(storage.get("historyEnabled", None).is_null());
            assert!(storage.get("workMode", None).is_null());
            assert!(storage.get("cloudAi.provider", None).is_null());
            assert!(storage.get("cloudAi.apiUrl", None).is_null());
            assert!(storage.get("webdav.enabled", None).is_null());
            assert!(storage.get("localAsr.downloadSource", None).is_null());
            assert_eq!(storage.get("overlayWidth", None), json!("short"));
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_recording_names_are_accepted_from_backups() {
        assert!(is_backup_audio_name("mshhzwhhyxxw.wav"));
        assert!(!is_backup_audio_name("evil.exe"));
        assert!(!is_backup_audio_name("evil.exe.wav.exe"));
        assert!(!is_backup_audio_name("a.b.wav"));
        assert!(!is_backup_audio_name(".wav"));

        let adir = Path::new("C:\\local\\audio");
        let ok = rewrite_audio_path(&json!({ "audioFilePath": "D:\\old\\audio\\abc123.wav" }), adir);
        assert_eq!(ok["audioFilePath"], json!(adir.join("abc123.wav").to_string_lossy()));
        let bad = rewrite_audio_path(&json!({ "audioFilePath": "D:\\old\\audio\\evil.exe" }), adir);
        assert!(bad.get("audioFilePath").is_none());
    }

    #[test]
    fn full_backup_keeps_only_recording_names_and_restores_the_same_paths() {
        let (storage, dir) = temp_storage("audio_names");
        let history = json!([
            { "id": "mac", "audioFilePath": "/Volumes/Old Mac/audio/abc123.wav" },
            { "id": "windows", "audioFilePath": "D:\\old\\audio\\def456.wav" },
            { "id": "name", "audioFilePath": "ghi789.wav" },
            { "id": "invalid", "audioFilePath": "/Volumes/Old Mac/audio/evil.exe" },
            { "id": "directory", "audioFilePath": "/Volumes/Old Mac/audio/" },
            { "id": "empty", "audioFilePath": "" },
            { "id": "number", "audioFilePath": 7 },
            { "id": "null", "audioFilePath": null },
            { "id": "absent", "asrText": "/Volumes/Old Mac/spoken.wav" },
        ]);
        let others = json!([{ "id": "other", "audioFilePath": "/Volumes/Old Mac/audio/abc123.wav" }]);
        storage.set("history", &history).unwrap();
        storage.set("manualCorrections", &others).unwrap();
        storage.set("feedbackQueue", &others).unwrap();

        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: true, include_audio: false };
        write_backup_archive_in_dir(&storage, scope, &output, &dir.join("audio"), &mut |_| {}).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        let mut text = String::new();
        archive.by_name("backup.json").unwrap().read_to_string(&mut text).unwrap();
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(payload["history"], json!([
            { "id": "mac", "audioFilePath": "abc123.wav" },
            { "id": "windows", "audioFilePath": "def456.wav" },
            { "id": "name", "audioFilePath": "ghi789.wav" },
            { "id": "invalid" }, { "id": "directory" }, { "id": "empty" },
            { "id": "number" }, { "id": "null" },
            // [自訂] SC-56 第二輪：使用者內容即使整段像路徑也保留。
            { "id": "absent", "asrText": "/Volumes/Old Mac/spoken.wav" },
        ]));
        for key in ["manualCorrections", "feedbackQueue"] {
            assert_eq!(payload[key], json!([{ "id": "other", "audioFilePath": "abc123.wav" }]));
        }
        assert!(!text.contains("Old Mac/audio") && !text.contains("old\\\\audio"));
        assert_eq!(storage.get("history", None), history);
        assert_eq!(storage.get("manualCorrections", None), others);
        assert_eq!(storage.get("feedbackQueue", None), others);

        // The stripped backup and an older one with full paths restore identically.
        let old = dir.join("old.zip");
        fs::write(&old, test_backup(&[("backup.json", &backup_with_history(history.clone()))])).unwrap();
        let target = dir.join("restored-audio");
        for source in [&output, &old] {
            let restored = Storage::new(dir.join(format!("{}.db", uuid::Uuid::new_v4()))).unwrap();
            apply_full_backup_in_dir(&restored, source.to_str().unwrap(), &target).unwrap();
            let records = restored.get("history", None);
            for (index, name) in ["abc123.wav", "def456.wav", "ghi789.wav"].iter().enumerate() {
                assert_eq!(records[index]["audioFilePath"], json!(target.join(name).to_string_lossy()));
            }
            assert!(records[3].get("audioFilePath").is_none());
            assert!(records[8].get("audioFilePath").is_none());
            // [自訂] SC-56：新備份還原後，目錄、空字串、數字與 null 都是欄位缺漏；舊備份保留原值。
            for index in 4..=7 {
                let restored = records[index].get("audioFilePath");
                if source == &output {
                    assert!(restored.is_none(), "{index}");
                } else if index != 4 {
                    assert_eq!(restored, history[index].get("audioFilePath"), "{index}");
                }
            }
        }
        drop(storage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn local_path_detection_covers_each_platform_but_not_sentences() {
        for path in [
            "/Volumes/Old Mac/a.wav", "/home/me/x", "~/Library/x", "~\\Documents", "C:\\Data\\me",
            "d:/data", "\\\\server\\share\\x", "file:///tmp/x", "  /tmp/x  ",
        ] {
            assert!(is_local_path(path), "{path}");
        }
        for text in [
            "", "/", "~", "abc.wav", "see /tmp/x later", "https://example.invalid/a", "Ctrl+/", "a:b", "C:",
            "/ spaced",
        ] {
            assert!(!is_local_path(text), "{text}");
        }
    }

    // [自訂] SC-59（L-98）：每種新形式各有正例與不誤判的反例。
    #[test]
    fn local_path_detection_covers_rooted_variable_and_network_forms() {
        for path in [
            "\\Users\\someone\\x", "\\Data\\x", "%USERPROFILE%", "%APPDATA%\\SayIt", "%ProgramFiles(x86)%\\x",
            "%localappdata%/x", "$HOME", "$HOME/notes", "${HOME}", "${XDG_DATA_HOME}/x", "$TMPDIR",
            "$CUSTOM_DIR/x", "~someone/x", "~some.one\\x", "smb://nas/share", "SMB://nas", "afp://mac/x",
            "File:///x",
        ] {
            assert!(is_local_path(path), "{path}");
        }
        for text in [
            "\\", "\\n", "\\ x\\y", "%", "50%", "%d%", "%s%/x", "%APPDATA", "%TOTAL% done", "$", "$5", "$5/kg",
            "$USD", "${HOME", "${5}", "$HOME later", "~約", "~約/x", "~5/10", "~ /x", "~bob", "smb", "smbx://a",
            "afp", "see smb://nas/x",
        ] {
            assert!(!is_local_path(text), "{text}");
        }
    }

    #[test]
    fn settings_whose_object_keys_are_paths_are_not_written() {
        let mut document = serde_json::json!({
            "appSettings": {
                "recentFolders": {"/home/x/Music": true},
                "shortcutKey": {"ctrl": "/"},
                "textReplacements": {"/home/x/kept": "user content"},
            },
        });
        scrub_export_document(&mut document);
        assert_eq!(document["appSettings"], serde_json::json!({
            "shortcutKey": {"ctrl": "/"},
            "textReplacements": {"/home/x/kept": "user content"},
        }));
    }

    #[test]
    fn metadata_objects_drop_keys_that_are_paths() {
        let mut document = serde_json::json!({
            "records": [{
                "appContext": {"/home/x/a.txt": "open", "~bob/b": {"k": "v"}, "%APPDATA%\\c": 1, "title": "kept"},
                "asrText": {"/kept/user/content": "/also/kept"},
            }],
        });
        scrub_export_document(&mut document);
        assert_eq!(document["records"][0]["appContext"], serde_json::json!({"title": "kept"}));
        assert_eq!(document["records"][0]["asrText"], serde_json::json!({"/kept/user/content": "/also/kept"}));
    }

    /// Strings in metadata fields and in settings that are not user content.
    fn metadata_strings(value: &Value, out: &mut Vec<String>) {
        fn all(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::String(text) => out.push(text.clone()),
                Value::Array(items) => items.iter().for_each(|item| all(item, out)),
                Value::Object(map) => map.values().for_each(|item| all(item, out)),
                _ => {}
            }
        }
        match value {
            Value::Array(items) => items.iter().for_each(|item| metadata_strings(item, out)),
            Value::Object(map) => map.iter().for_each(|(key, item)| {
                if METADATA_FIELDS.contains(&key.as_str()) || METADATA_CONTAINERS.contains(&key.as_str()) {
                    all(item, out);
                } else if key == "appSettings" {
                    for (setting, item) in item.as_object().into_iter().flatten() {
                        if !CONTENT_SETTINGS.contains(&setting.as_str()) {
                            all(item, out);
                        }
                    }
                } else {
                    metadata_strings(item, out);
                }
            }),
            _ => {}
        }
    }

    fn assert_no_local_paths(document: &Value) {
        let mut strings = Vec::new();
        metadata_strings(document, &mut strings);
        assert!(!strings.is_empty());
        for text in &strings {
            assert!(!is_local_path(text), "{text}");
        }
    }

    #[test]
    fn backups_and_configuration_exports_contain_no_local_paths() {
        let (storage, dir) = temp_storage("no_local_paths");
        let sentence = "Open /Volumes/Work/notes.txt tomorrow";
        let settings = [
            ("overlayWidth", json!("short")),
            ("customUnknownFolder", json!("/home/someone/Exports")),
            ("customNested", json!({ "folders": ["D:\\Exports", "kept"] })),
            ("customHome", json!("~/Exports")),
            ("customShare", json!("\\\\nas\\share")),
            ("customNamed", json!({ "name": "~/Projects" })),
            ("customText", json!(sentence)),
            ("hotwords", json!(["/usr/local/bin", "kept"])),
            ("contextSelectionEditPrompt", json!("/no_think rewrite the selection")),
            ("aiPromptAppend", json!("/no_think")),
        ];
        for (key, value) in &settings {
            storage.set(key, value).unwrap();
        }
        let history = json!([{
            "id": "abc123", "asrText": "/Volumes/Work/notes.txt", "llmText": sentence,
            "asrCorrectedText": "/home/someone/proj", "promptPresetName": "/translate",
            "audioFilePath": "/Volumes/Old/audio/abc123.wav",
            "windowTitle": "/home/someone/Documents", "exePath": "C:\\Program Files\\App\\app.exe",
            "failReason": sentence,
            "appContext": { "exePath": "/Applications/App.app", "processName": "App", "recent": ["~/a.txt", "kept"] },
            "autoAppliedHotwords": ["kept", "file:///tmp/x"],
        }]);
        storage.set("history", &history).unwrap();
        storage.set("manualCorrections", &json!([{ "id": "m", "note": "~/x", "preferredText": sentence }])).unwrap();
        storage.set("feedbackQueue", &json!([{ "id": "f", "snapshot": { "asrText": "/tmp/x" } }])).unwrap();
        storage.set("promptPresets", &json!([{ "id": "p", "name": "/translate", "systemPrompt": "/no_think be brief" }])).unwrap();
        let matcher = json!({ "processNames": ["C:\\Apps\\code.exe", "code"], "windowTitleIncludes": ["~/work"] });
        storage.set("appPromptRules", &json!([{ "id": "r", "appId": "a", "matcher": matcher }])).unwrap();
        storage.set("textReplacements", &json!([{ "id": "t", "from": "/home/someone", "to": "home" }])).unwrap();
        let before: Vec<Value> = ["history", "manualCorrections", "customUnknownFolder", "customNested"]
            .iter().map(|key| storage.get(key, None)).collect();

        let output = dir.join("backup.zip");
        let scope = BackupScope { include_history: true, include_audio: false };
        write_backup_archive_in_dir(&storage, scope, &output, &dir.join("audio"), &mut |_| {}).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        let backup: Value = serde_json::from_reader(archive.by_name("backup.json").unwrap()).unwrap();
        assert_no_local_paths(&backup);
        let exported = &backup["appSettings"];
        assert_eq!(exported["overlayWidth"], json!("short"));
        assert_eq!(exported["customText"], json!(sentence));
        for key in ["customUnknownFolder", "customNested", "customHome", "customShare", "customNamed"] {
            assert!(exported.get(key).is_none(), "{key}");
        }
        let record = &backup["history"][0];
        // Metadata: whole-path values go, list items that are paths go, sentences stay.
        assert_eq!(record["audioFilePath"], json!("abc123.wav"));
        assert!(record.get("windowTitle").is_none() && record.get("exePath").is_none());
        assert_eq!(record["failReason"], json!(sentence));
        assert_eq!(record["appContext"], json!({ "processName": "App", "recent": ["kept"] }));
        // User content keeps whole-path values and slash commands.
        assert_eq!(record["asrText"], json!("/Volumes/Work/notes.txt"));
        assert_eq!(record["asrCorrectedText"], json!("/home/someone/proj"));
        assert_eq!(record["promptPresetName"], json!("/translate"));
        assert_eq!(record["autoAppliedHotwords"], json!(["kept", "file:///tmp/x"]));
        assert_eq!(backup["manualCorrections"][0]["note"], json!("~/x"));
        assert_eq!(backup["feedbackQueue"][0]["snapshot"]["asrText"], json!("/tmp/x"));
        assert_eq!(backup["promptPresets"][0]["systemPrompt"], json!("/no_think be brief"));
        assert_eq!(backup["appPromptRules"][0]["matcher"], matcher);
        for (key, value) in &settings[6..] {
            assert_eq!(&exported[*key], value, "{key}");
        }
        assert_eq!(exported["textReplacements"][0]["from"], json!("/home/someone"));

        for (mode, name) in [("full", "full.json"), ("selected", "selected.json")] {
            let selection: ConfigExportSelection = serde_json::from_value(json!({
                "mode": mode, "includeTextReplacements": true, "promptPresetIds": ["p"],
            })).unwrap();
            let path = dir.join(name);
            write_config_file(&storage, &selection, &path).unwrap();
            let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            if mode == "full" {
                assert_no_local_paths(&config);
                assert_eq!(config["appSettings"]["contextSelectionEditPrompt"], settings[8].1);
            }
            assert!(serde_json::to_string(&config).unwrap().contains("/no_think be brief"), "{mode}");
        }
        // The database itself is unchanged.
        let after: Vec<Value> = ["history", "manualCorrections", "customUnknownFolder", "customNested"]
            .iter().map(|key| storage.get(key, None)).collect();
        assert_eq!(after, before);

        // An omitted setting keeps the local value on restore; user content round-trips unchanged.
        let restored = Storage::new(dir.join("restored.db")).unwrap();
        restored.set("customUnknownFolder", &json!("local value")).unwrap();
        apply_full_backup_in_dir(&restored, output.to_str().unwrap(), &dir.join("restored-audio")).unwrap();
        assert_eq!(restored.get("customUnknownFolder", None), json!("local value"));
        for (key, value) in &settings[6..] {
            assert_eq!(&restored.get(key, None), value, "{key}");
        }
        let records = restored.get("history", None);
        for key in ["asrText", "asrCorrectedText", "promptPresetName", "autoAppliedHotwords"] {
            assert_eq!(records[0][key], history[0][key], "{key}");
        }
        assert_eq!(restored.get("textReplacements", None)[0]["from"], json!("/home/someone"));
        assert_eq!(restored.get("manualCorrections", None)[0]["note"], json!("~/x"));
        assert_eq!(restored.get("feedbackQueue", None)[0]["snapshot"]["asrText"], json!("/tmp/x"));
        assert_eq!(restored.get("promptPresets", None)[0]["systemPrompt"], json!("/no_think be brief"));
        assert_eq!(restored.get("appPromptRules", None)[0]["matcher"], matcher);
        assert_eq!(records[0]["audioFilePath"], json!(dir.join("restored-audio/abc123.wav").to_string_lossy()));
        drop((storage, restored));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn volatile_storage_writes_no_backup_or_configuration_file() {
        let (disk, dir) = temp_storage("volatile_export");
        let storage = volatile_storage(&dir);
        // The output folder does not exist yet; a refused export must not create it.
        let out = dir.join("out");
        let audio = dir.join("audio");
        fs::create_dir(&audio).unwrap();
        fs::write(audio.join("abc123.wav"), b"RIFF").unwrap();
        for scope in [
            BackupScope { include_history: true, include_audio: true },
            BackupScope { include_history: false, include_audio: false },
        ] {
            assert_eq!(
                write_backup_archive_in_dir(&storage, scope, &out.join("backup.zip"), &audio, &mut |_| panic!("no progress")),
                Err(crate::storage::VOLATILE_EXPORT_ERROR.into()),
            );
        }
        for mode in ["full", "selected"] {
            let selection: ConfigExportSelection = serde_json::from_value(json!({
                "mode": mode, "includeTextReplacements": true,
            })).unwrap();
            assert_eq!(
                write_config_file(&storage, &selection, &out.join("config.json")),
                Err(crate::storage::VOLATILE_EXPORT_ERROR.into()),
            );
        }
        assert!(!out.exists());
        assert_volatile_storage_unchanged(&storage);
        let scope = BackupScope { include_history: true, include_audio: true };
        write_backup_archive_in_dir(&disk, scope, &out.join("backup.zip"), &audio, &mut |_| {}).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_layer_refuses_volatile_storage_even_when_called_directly() {
        let (_disk, dir) = temp_storage("volatile_write_layer");
        let storage = volatile_storage(&dir);
        let settings = json!({ "overlayWidth": "short" });
        let settings = settings.as_object().unwrap();
        let history = [json!({ "id": "new" })];
        assert_eq!(
            storage.apply_config_transaction(settings, &[], Some(&[]), None),
            Err(crate::storage::VOLATILE_STORAGE_ERROR.into()),
        );
        for dry_run in [true, false] {
            assert_eq!(
                storage.apply_restore_transaction(settings, &[], &[("history", Some(&history))], dry_run),
                Err(crate::storage::VOLATILE_STORAGE_ERROR.into()),
            );
        }
        let data = json!({ "appSettings": { "overlayWidth": "short" } });
        assert!(apply_config_part(&storage, &data, CONFIG_EXCLUDE).is_err());
        assert!(apply_full_restore(&storage, &data, &dir.join("audio"), || panic!("no recordings")).is_err());
        assert_volatile_storage_unchanged(&storage);
        fs::remove_dir_all(dir).unwrap();
    }

    /// 不含音频时不去扫 audio 目录，也就不会把别的备份的录音塞进来。
    #[test]
    fn audio_files_are_only_collected_when_requested() {
        assert!(collect_audio_export_files(false, Path::new("unused")).unwrap().is_empty());
    }

    #[test]
    fn prompt_overrides_allow_one_entry_per_builtin_language() {
        let items = vec![
            json!({
                "id": "intent",
                "name": "Intent cleanup",
                "systemPrompt": "中文自定义",
                "builtinPromptLanguage": "zh-CN",
            }),
            json!({
                "id": "intent",
                "name": "Intent cleanup",
                "systemPrompt": "English custom",
                "builtinPromptLanguage": "en",
            }),
        ];

        assert!(validate_config_collection("promptPresets", &items).is_ok());
    }

    #[test]
    fn prompt_overrides_reject_duplicate_builtin_language() {
        let items = vec![
            json!({ "id": "intent", "name": "Intent cleanup", "systemPrompt": "First" }),
            json!({
                "id": "intent",
                "name": "Intent cleanup",
                "systemPrompt": "Second",
                "builtinPromptLanguage": "zh-CN",
            }),
        ];

        assert!(validate_config_collection("promptPresets", &items).is_err());
    }

    /// [自訂] SC-65：中文內建 Prompt 改用 zh-TW；舊備份的 zh-CN 是同一種語言，不能和 zh-TW 並存兩份。
    #[test]
    fn prompt_overrides_treat_legacy_simplified_as_traditional() {
        let entry = |language: &str| {
            json!({
                "id": "intent",
                "name": "Intent cleanup",
                "systemPrompt": "custom",
                "builtinPromptLanguage": language,
            })
        };
        assert!(validate_config_collection("promptPresets", &[entry("zh-TW"), entry("en")]).is_ok());
        assert!(validate_config_collection("promptPresets", &[entry("zh-TW"), entry("zh-CN")]).is_err());
        assert!(validate_config_collection("promptPresets", &[entry("zh-HK")]).is_err());
    }
}
