import type { HistoryFailReasonCode, PromptPreset } from '@/services/store'
import type { AppPromptRule } from '@/services/personalization/types'
import { t, type TranslationKey } from '.'
import { IS_MAC } from '@/lib/platform'

const BUILTIN_PRESET_NAME_KEYS: Record<string, TranslationKey> = {
  intent: 'builtinPreset.intent',
  faithful: 'builtinPreset.faithful',
  zh2en: 'builtinPreset.zh2en',
  casual: 'builtinPreset.casual',
}

const BUILTIN_APP_NAME_KEYS: Record<string, TranslationKey> = {
  teams: 'builtinApp.teams',
  outlook: 'builtinApp.outlook',
  kiro: 'builtinApp.kiro',
  vscode: 'builtinApp.vscode',
  cursor: 'builtinApp.cursor',
  // mac 上這條規則對應系統自帶的「文字編輯」（見 personalization/defaults.ts 的 MAC_RULE_OVERRIDES）
  notepad: IS_MAC ? 'builtinApp.notepadMac' : 'builtinApp.notepad',
  codex: 'builtinApp.codex',
  weixin: 'builtinApp.weixin',
  qq: 'builtinApp.qq',
}

const LOCAL_MODEL_KEYS: Record<string, {
  name: TranslationKey
  description: TranslationKey
  languages: TranslationKey
}> = {
  'parakeet-unified-en-0.6b-gguf': {
    name: 'localModel.parakeetEn.name',
    description: 'localModel.parakeetEn.description',
    languages: 'localModel.parakeetEn.languages',
  },
  'sensevoice-small-gguf': {
    name: 'localModel.sensevoice.name',
    description: 'localModel.sensevoice.description',
    languages: 'localModel.sensevoice.languages',
  },
  'nemotron-asr-streaming-0.6b-gguf': {
    name: 'localModel.nemotron.name',
    description: 'localModel.nemotron.description',
    languages: 'localModel.nemotron.languages',
  },
  'funasr-nano-2512-gguf': {
    name: 'localModel.funasr.name',
    description: 'localModel.funasr.description',
    languages: 'localModel.funasr.languages',
  },
  'qwen3-asr-0.6b-gguf': {
    name: 'localModel.qwen06.name',
    description: 'localModel.qwen06.description',
    languages: 'localModel.qwen06.languages',
  },
  'qwen3-asr-1.7b-q4-gguf': {
    name: 'localModel.qwen17Fast.name',
    description: 'localModel.qwen17Fast.description',
    languages: 'localModel.qwen17Fast.languages',
  },
  'qwen3-asr-1.7b-gguf': {
    name: 'localModel.qwen17Accurate.name',
    description: 'localModel.qwen17Accurate.description',
    languages: 'localModel.qwen17Accurate.languages',
  },
  // [自訂] SC-68
  'breeze-asr-25-gguf': {
    name: 'localModel.breeze.name',
    description: 'localModel.breeze.description',
    languages: 'localModel.breeze.languages',
  },
}

const HISTORY_FAILURE_KEYS: Record<HistoryFailReasonCode, TranslationKey> = {
  no_transcript: 'recorder.noTranscript',
  empty_after_processing: 'recorder.emptyAfterProcessing',
  provider_timeout: 'err.provider.timeout',
  provider_unreachable: 'err.provider.unreachable',
  provider_bad_key: 'err.provider.badKey',
  provider_forbidden: 'err.provider.forbidden',
  provider_rate_limit: 'err.provider.rateLimit',
  provider_no_model: 'err.provider.noModel',
  provider_failed: 'record.providerFailed',
  processing_timeout: 'recorder.processingTimeout',
  connection_lost: 'recorder.connectionLost',
}

/** 內建模式名由穩定 id 翻譯；使用者自建名稱永遠原樣顯示。 */
export function promptPresetDisplayName(preset: Pick<PromptPreset, 'id' | 'name' | 'builtin'>): string {
  const key = preset.builtin ? BUILTIN_PRESET_NAME_KEYS[preset.id] : undefined
  return key ? t(key) : preset.name
}

/** 內建應用規則名由穩定 id 翻譯；使用者自建名稱永遠原樣顯示。 */
export function appPromptRuleDisplayName(rule: Pick<AppPromptRule, 'id' | 'appId' | 'name' | 'builtin'>): string {
  const key = rule.builtin ? BUILTIN_APP_NAME_KEYS[rule.appId || rule.id] : undefined
  return key ? t(key) : rule.name
}

/** 歷史統計只儲存了 appId/appName，已知內建 appId 仍可按當前介面語言顯示。 */
export function recordedAppDisplayName(appId: string | undefined, fallback: string): string {
  const key = appId ? BUILTIN_APP_NAME_KEYS[appId] : undefined
  return key ? t(key) : fallback
}

/** 歷史記錄同時儲存 preset id/name；用 id 可讓舊記錄也跟隨當前介面語言。 */
export function recordedPromptPresetDisplayName(presetId: string | undefined, fallback: string): string {
  const key = presetId ? BUILTIN_PRESET_NAME_KEYS[presetId] : undefined
  return key ? t(key) : fallback
}

interface LocalModelDisplaySource {
  id: string
  name: string
  description?: string
  languages_label?: string
}

/** Rust catalog 保留穩定 id；展示文案在渲染時取值，切換介面語言不會凍結。 */
export function localModelDisplayName(model: LocalModelDisplaySource): string {
  const key = LOCAL_MODEL_KEYS[model.id]?.name
  return key ? t(key) : model.name
}

export function localModelDisplayDescription(model: LocalModelDisplaySource): string {
  const key = LOCAL_MODEL_KEYS[model.id]?.description
  return key ? t(key) : model.description || ''
}

export function localModelDisplayLanguages(model: LocalModelDisplaySource): string {
  const key = LOCAL_MODEL_KEYS[model.id]?.languages
  return key ? t(key) : model.languages_label || ''
}

/** 新記錄按穩定 code 翻譯；老記錄沒有 code 時保留當時寫入的原文。 */
export function historyFailureReasonDisplay(record: {
  failReasonCode?: HistoryFailReasonCode
  failReason?: string
}): string {
  const key = record.failReasonCode ? HISTORY_FAILURE_KEYS[record.failReasonCode] : undefined
  return key ? t(key) : record.failReason || ''
}
