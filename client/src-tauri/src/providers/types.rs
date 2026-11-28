// ASR / AI 供應商的公共型別和 trait 定義

use serde::{Deserialize, Serialize};

/// ASR 識別結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrResult {
    pub text: String,
    /// 識別耗時（毫秒）
    pub elapsed_ms: u64,
}

/// AI 校對結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiResult {
    pub text: String,
    /// 校對耗時（毫秒）
    pub elapsed_ms: u64,
}

/// 連線測試結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
    pub elapsed_ms: u64,
    /// 測試詳情（模型名、傳送的 prompt、回覆內容等）
    #[serde(default)]
    pub detail: String,
}

/// ASR 供應商配置（前端傳入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrProviderConfig {
    pub provider: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub app_id: String,
    /// 供應商特定的額外配置
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// AI 供應商配置（前端傳入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    pub provider: String,
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    /// 供應商特定的額外配置
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// 雲端轉寫請求（前端傳入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudTranscribeRequest {
    /// base64 編碼的 PCM Int16 音訊。cloud_transcribe_raw 的引數裡沒有這一項（音訊走二進位制），由那邊填上。
    #[serde(default)]
    pub audio_b64: String,
    pub sample_rate: u32,
    pub asr_config: AsrProviderConfig,
    /// 熱詞列表（豆包等供應商透過 request.context 直傳）
    #[serde(default)]
    pub hotwords: Vec<String>,
}

/// 雲端 AI 校對請求（前端傳入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudPolishRequest {
    pub text: String,
    pub ai_config: AiProviderConfig,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub text_context: Option<TextContext>,
}

/// Ephemeral editor context. It is accepted only for this one AI request and is never persisted.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TextContext {
    #[serde(default)]
    pub source: String,
    #[serde(default, alias = "textBefore")]
    pub text_before: String,
    #[serde(default, alias = "selectedText")]
    pub selected_text: String,
    #[serde(default, alias = "textAfter")]
    pub text_after: String,
    #[serde(default, alias = "selectionTruncated")]
    pub selection_truncated: bool,
}
