//! 二進位制 IPC 請求體：`[4 位元組小端 JSON 長度][JSON 引數][原始位元組]`。
//!
//! 音訊原來在網頁裡先轉 base64、再塞進 JSON 發過來。網頁裡那個轉碼是逐位元組拼字串，
//! 2 分鐘錄音要約 0.4 秒、5 分鐘約 0.7 秒，而且跑在主窗主執行緒上（2026-10 實測），
//! 本地 / 雲 API 模式錄完還要轉兩次（存檔一次、識別一次）。改成直接發原始位元組後，
//! 這一段幾乎為零。引數裡可能有中文熱詞、幾 KB 長，放請求頭不合適，所以和音訊拼在同一個請求體裡。

use serde::de::DeserializeOwned;

/// 從 Tauri 的原始請求體裡拆出引數和載荷。
pub fn split_framed<T: DeserializeOwned>(request: &tauri::ipc::Request<'_>) -> Result<(T, Vec<u8>), String> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else {
        return Err("Expected a binary request body".to_string());
    };
    split_framed_bytes(body)
}

pub fn split_framed_bytes<T: DeserializeOwned>(body: &[u8]) -> Result<(T, Vec<u8>), String> {
    if body.len() < 4 {
        return Err("Binary request body is too short".to_string());
    }
    let json_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let json_end = 4usize
        .checked_add(json_len)
        .filter(|end| *end <= body.len())
        .ok_or_else(|| "Binary request body has an invalid header length".to_string())?;
    let meta: T = serde_json::from_slice(&body[4..json_end])
        .map_err(|e| format!("Invalid request parameters: {e}"))?;
    Ok((meta, body[json_end..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::split_framed_bytes;
    use serde_json::{json, Value};

    fn frame(meta: &Value, payload: &[u8]) -> Vec<u8> {
        let json = serde_json::to_vec(meta).unwrap();
        let mut out = (json.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&json);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn splits_parameters_and_payload() {
        let body = frame(&json!({ "id": "abc", "hotwords": ["中文热词"] }), &[1, 2, 3, 4]);
        let (meta, payload): (Value, Vec<u8>) = split_framed_bytes(&body).unwrap();
        assert_eq!(meta["hotwords"][0], "中文热词");
        assert_eq!(payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn empty_payload_is_fine() {
        let (_, payload): (Value, Vec<u8>) = split_framed_bytes(&frame(&json!({}), &[])).unwrap();
        assert!(payload.is_empty());
    }

    #[test]
    fn rejects_truncated_or_lying_headers() {
        assert!(split_framed_bytes::<Value>(&[1, 0]).is_err());
        let mut body = frame(&json!({}), &[]);
        body[0] = 200; // 聲稱 JSON 有 200 位元組
        assert!(split_framed_bytes::<Value>(&body).is_err());
    }
}
