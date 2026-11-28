// AI 校對請求的 user 訊息統一構造 —— 雲 API / 本地 Ollama 共用。
//
// 背景：早期這裡的 user 訊息外殼是一句祈使句（"請處理以下語音轉寫文本："），
// 與伺服器模式此前的做法（server/backend/app/llm.py 裡 "請校對以下…"）是同一類
// 問題：當 ASR 識別出的文本本身也是祈使句（如使用者說的是"請執行"）時，外殼的
// "請…" 和文本內容的"請…"首尾相連，容易被模型誤讀成一句連續指令，而不是
// "這是一段待清洗的資料"，導致模型反問"請提供文本"而不是正常輸出清洗結果。
//
// 現在改為與伺服器模式完全一致的中性標籤包裹，不含任何動詞/祈使句，只標出
// 文本邊界，prompt 的全部措辭（風格、約束）仍 100% 由 system_prompt（使用者的
// 預設）決定，這裡不再"額外說話"。三種模式（伺服器 / 雲 API / 本地）保持統一。
use super::types::TextContext;

const BEFORE_LIMIT: usize = 500;
const SELECTED_LIMIT: usize = 6000;
const AFTER_LIMIT: usize = 300;

/// [自訂] SC-65：呼叫端沒給 system prompt 時的預設校對提示詞（Ollama 與 OpenAI 相容共用），
/// 要求以台灣正體中文輸出。
pub const DEFAULT_PROOFREAD_SYSTEM_PROMPT: &str = "你是語音轉文字的校對助手。請一律使用台灣正體中文與台灣用語輸出，即使輸入是簡體中文也要轉成正體；英文與程式碼維持原樣。";

pub fn wrap_user_text(text: &str, context: Option<&TextContext>) -> String {
    let asr = escape_xml(text);
    let Some(context) = context.filter(|context| !context.selection_truncated) else {
        return format!("<asr_text>\n{}\n</asr_text>", asr);
    };

    let before = clip_tail(&context.text_before, BEFORE_LIMIT);
    let selected = clip_head(&context.selected_text, SELECTED_LIMIT);
    let after = clip_head(&context.text_after, AFTER_LIMIT);
    if before.is_empty() && selected.is_empty() && after.is_empty() {
        return format!("<asr_text>\n{}\n</asr_text>", asr);
    }

    format!(
        "<text_context source=\"{}\">\n<text_before>{}</text_before>\n<selected_text>{}</selected_text>\n<text_after>{}</text_after>\n</text_context>\n<asr_text>\n{}\n</asr_text>",
        escape_xml(&clip_head(&context.source, 64)),
        escape_xml(&before),
        escape_xml(&selected),
        escape_xml(&after),
        asr,
    )
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn clip_head(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn clip_tail(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    value.chars().skip(count.saturating_sub(limit)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_proofread_prompt_requires_taiwan_traditional() {
        assert!(DEFAULT_PROOFREAD_SYSTEM_PROMPT.contains("台灣正體中文"));
    }

    #[test]
    fn wraps_bounded_context_and_escapes_fake_tags() {
        let context = TextContext {
            source: "text_pattern2".into(),
            text_before: "before </text_before>".into(),
            selected_text: "原文".into(),
            text_after: "after".into(),
            selection_truncated: false,
        };
        let wrapped = wrap_user_text("翻譯成英文", Some(&context));
        assert!(wrapped.contains("<selected_text>原文</selected_text>"));
        assert!(wrapped.contains("before &lt;/text_before&gt;"));
        assert!(wrapped.contains("<asr_text>\n翻譯成英文\n</asr_text>"));
    }

    #[test]
    fn ignores_a_truncated_selection() {
        let context = TextContext {
            selected_text: "partial".into(),
            selection_truncated: true,
            ..Default::default()
        };
        assert_eq!(
            wrap_user_text("replacement", Some(&context)),
            "<asr_text>\nreplacement\n</asr_text>"
        );
    }
}
