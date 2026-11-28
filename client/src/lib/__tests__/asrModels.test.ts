import { describe, it, expect } from 'vitest'
import {
  buildAsrExtra,
  isQwenOmniProvider,
  isStreamingDisplayCapable,
  isStreamingDisplayReady,
  resolveQwenOmniModel,
  resolveAsrDisplayModel,
} from '../asrModels'

describe('isQwenOmniProvider', () => {
  it('識別 Qwen Omni 系列', () => {
    expect(isQwenOmniProvider('qwen_omni_plus')).toBe(true)
    expect(isQwenOmniProvider('qwen_omni_35_plus')).toBe(true)
    expect(isQwenOmniProvider('qwen_omni_35_flash')).toBe(true)
    expect(isQwenOmniProvider('qwen_omni_flash')).toBe(true)
    expect(isQwenOmniProvider('qwen_omni_turbo')).toBe(true)
  })

  it('非 Omni 返回 false', () => {
    expect(isQwenOmniProvider('doubao_v2')).toBe(false)
    expect(isQwenOmniProvider('qwen')).toBe(false)
    expect(isQwenOmniProvider('')).toBe(false)
  })
})

describe('resolveQwenOmniModel', () => {
  it('返回正確的模型 ID', () => {
    expect(resolveQwenOmniModel('qwen_omni_35_plus')).toBe('qwen3.5-omni-plus-realtime')
    expect(resolveQwenOmniModel('qwen_omni_35_flash')).toBe('qwen3.5-omni-flash-realtime')
  })

  /**
   * 兩個退役執行時鍵必須仍然解析出**某個還活著的** Omni，不能變成 undefined。
   *
   * undefined 會讓 buildAsrExtra 不帶 model 欄位，於是落點交給 Rust 側的
   * DEFAULT_MODEL 去定 —— 兩處各自演進就會漂，而漂了沒人會發現。
   * 落點也必須和 asrProviderCatalog 的 RETIRED_MODELS 一致。
   */
  it('退役的執行時鍵落到同族還活著的模型', () => {
    expect(resolveQwenOmniModel('qwen_omni_flash')).toBe('qwen3.5-omni-flash-realtime')
    expect(resolveQwenOmniModel('qwen_omni_turbo')).toBe('qwen3.5-omni-flash-realtime')
  })

  it('非 Omni 返回 undefined', () => {
    expect(resolveQwenOmniModel('doubao_v2')).toBeUndefined()
    expect(resolveQwenOmniModel('qwen')).toBeUndefined()
  })
})

describe('resolveAsrDisplayModel', () => {
  it('對映已知供應商', () => {
    expect(resolveAsrDisplayModel('doubao_v2')).toBe('Doubao-Seed-ASR-2.0')
    expect(resolveAsrDisplayModel('qwen')).toBe('qwen3-asr-flash')
    expect(resolveAsrDisplayModel('mimo')).toBe('mimo-v2.5-asr')
    expect(resolveAsrDisplayModel('qwen_omni_35_plus')).toBe('qwen3.5-omni-plus-realtime')
  })

  it('未知供應商返回原始 key', () => {
    expect(resolveAsrDisplayModel('custom_provider')).toBe('custom_provider')
  })

  it('空字符串返回 unknown', () => {
    expect(resolveAsrDisplayModel('')).toBe('unknown')
  })

  // 使用者選了同一家的另一個模型時，顯示必須跟著走 —— 否則歷史記錄、診斷、
  // 識別測試三處都會報該服務的預設模型，看著像"我選的沒生效"
  it('選定的模型優先於預設表', () => {
    expect(resolveAsrDisplayModel('groq_whisper')).toBe('whisper-large-v3-turbo')
    expect(resolveAsrDisplayModel('groq_whisper', 'whisper-large-v3')).toBe('whisper-large-v3')
    expect(resolveAsrDisplayModel('openai_transcribe')).toBe('gpt-transcribe')
    expect(resolveAsrDisplayModel('openai_transcribe', 'gpt-4o-mini-transcribe'))
      .toBe('gpt-4o-mini-transcribe')
  })

  it('空白的選定值不算選過', () => {
    expect(resolveAsrDisplayModel('groq_whisper', '')).toBe('whisper-large-v3-turbo')
    expect(resolveAsrDisplayModel('groq_whisper', '   ')).toBe('whisper-large-v3-turbo')
  })
})

describe('流式即時字幕判據', () => {
  // isStreamingDisplayReady 是執行時**唯一**的閘門（CloudAPIProvider 拿它決定走不走
  // WebSocket）。光在服務目錄裡給一條加 streaming: true 是沒有任何效果的 ——
  // 這組斷言就是那份契約。
  it('有流式實現的五家都就緒，一次性那幾家一律不就緒', () => {
    for (const provider of [
      'doubao_v2', 'qwen_audio_stream', 'openai_live_transcribe', 'gemini_live_transcribe',
    ]) {
      expect(isStreamingDisplayCapable(provider)).toBe(true)
      expect(isStreamingDisplayReady(provider)).toBe(true)
    }
    // 這幾個是一次性 HTTP，它們的流式版本是各自單獨的一張卡
    for (const provider of [
      'qwen', 'mimo', 'groq_whisper', 'openai_transcribe', 'gemini_transcribe', 'qwen_omni_35_plus', '',
    ]) {
      expect(isStreamingDisplayCapable(provider)).toBe(false)
      expect(isStreamingDisplayReady(provider)).toBe(false)
    }
  })

  // qwen_realtime 走地域專屬端點，沒有業務空間 ID 連不上 —— 這條前置條件只屬於它，
  // 別照抄給 qwen_audio_stream（那個用通用域名，實測不需要）。
  it('只有 qwen_realtime 要業務空間 ID 才算就緒', () => {
    expect(isStreamingDisplayCapable('qwen_realtime')).toBe(true)
    expect(isStreamingDisplayReady('qwen_realtime')).toBe(false)
    expect(isStreamingDisplayReady('qwen_realtime', '  ')).toBe(false)
    expect(isStreamingDisplayReady('qwen_realtime', 'ws-abc')).toBe(true)
    // 別家給了 workspaceId 也不影響判定
    expect(isStreamingDisplayReady('qwen_audio_stream')).toBe(true)
    expect(isStreamingDisplayReady('gemini_live_transcribe', '')).toBe(true)
  })
})

describe('buildAsrExtra', () => {
  it('普通服務：選過模型才帶 extra', () => {
    expect(buildAsrExtra('groq_whisper')).toBeUndefined()
    expect(buildAsrExtra('groq_whisper', { model: '' })).toBeUndefined()
    expect(buildAsrExtra('groq_whisper', { model: '  ' })).toBeUndefined()
    expect(buildAsrExtra('groq_whisper', { model: 'whisper-large-v3' }))
      .toEqual({ model: 'whisper-large-v3' })
  })

  /**
   * ⚠️ 這條是一次靜默 bug 的迴歸測試。
   *
   * 「一張卡 = 一個平台」之後，執行時 provider 變成了平台級的 `qwen_omni`，模型由
   * 呼叫方（resolveAsrApiModel）解析好再傳進來。而老實現看到 provider 以 `qwen_omni`
   * 開頭就去查 QWEN_OMNI_MODEL_MAP —— 表裡沒有 `qwen_omni` 這個鍵，於是
   * `extra.model` 是 undefined，後端回落到它自己的預設模型。
   * 表現是**下拉里選哪個 Omni 都跑同一個**，而介面顯示一切正常。
   *
   * 老測試用的是舊 id（`qwen_omni_35_plus`，表裡有），所以它一直是綠的 ——
   * 斷言釘在了已經不存在的呼叫形態上。
   */
  it('傳進來的模型必須被採納，哪怕 provider 是 qwen_omni', () => {
    expect(buildAsrExtra('qwen_omni', { model: 'qwen3.5-omni-plus-realtime' }))
      .toEqual({ model: 'qwen3.5-omni-plus-realtime' })
    expect(buildAsrExtra('qwen_omni', {
      model: 'qwen3.5-omni-flash-realtime',
      instructions: '照原樣轉寫',
    })).toEqual({ model: 'qwen3.5-omni-flash-realtime', instructions: '照原樣轉寫' })
  })

  /** 存量執行時鍵裡 provider 還是舊的那幾個時，模型名仍要能從老表反查出來。 */
  it('沒傳模型時，舊 provider id 還能從老表兜底', () => {
    expect(buildAsrExtra('qwen_omni_35_plus'))
      .toEqual({ model: 'qwen3.5-omni-plus-realtime' })
    // 退役鍵兜底出來的是替代模型，不是它原來那個（見 QWEN_OMNI_MODEL_MAP 的註釋）
    expect(buildAsrExtra('qwen_omni_turbo'))
      .toEqual({ model: 'qwen3.5-omni-flash-realtime' })
  })

  it('自定義端點：地址會帶進 extra', () => {
    expect(buildAsrExtra('openai_compat_transcribe', {
      model: 'whisper-1',
      baseUrl: 'http://127.0.0.1:8000/v1',
    })).toEqual({ model: 'whisper-1', baseUrl: 'http://127.0.0.1:8000/v1' })
    // 空地址不進 extra，讓後端用它內建的預設值
    expect(buildAsrExtra('groq_whisper', { model: 'whisper-large-v3', baseUrl: '  ' }))
      .toEqual({ model: 'whisper-large-v3' })
  })
})
