// 雲 API 模式配置面板 —— 一張卡 = 一份語音識別服務配置
//
// 結構與「AI 服務」頁對齊（同一套卡片、同一套彈窗、同一套狀態語義）：可以新建多份，
// 同一家也能存多份（兩個百煉帳號、兩套豆包金鑰），點一張即啟用。
//
// 與 AI 服務的唯一實質差別：供應商從內建清單裡選，不能填任意地址 —— 每家 ASR 的協議
// 都要一份專門的 Rust 實現，不像 AI 整理那邊只要 OpenAI 相容端點就能接。
//
// 「同平台重複粘金鑰」不靠把憑據提到平台級來解決（那樣一張卡就不再是一份完整配置、
// 也沒法存兩個帳號），而是照 AI 服務的做法：新建時自動沿用同平台上一份的金鑰，
// 並在介面上說明它從哪來。

import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open as shellOpen } from '@tauri-apps/plugin-shell'
import { CheckCircle2, ExternalLink, Info, Pencil, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Feedback, FormatHint, type FeedbackTone } from '@/components/ui/feedback'
import { Modal } from '@/components/ui/modal'
import { PasswordInput } from '@/components/ui/password-input'
import { Segmented } from '@/components/ui/segmented'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { refreshModeStatus } from '@/stores/modeStatus'
import { setEngineDraftDirty } from '@/stores/engineDraft'
import { buildAsrExtra } from '@/lib/asrModels'
import { describeProviderError } from '@/lib/errorMessages'
import { doubaoKeyLabel } from '@/lib/cloudAsrCreds'
import {
  ASR_PLATFORMS,
  ASR_PROVIDERS,
  asrAvailabilityLabel,
  asrModelsOf,
  describeAsrMissing,
  effectiveAsrCredentials,
  emptyAsrProfile,
  findAsrProvider,
  gradeAsrLatency,
  groupAsrModelsByVendor,
  ASR_COMPAT_PROTOCOLS,
  asrCardTitle,
  asrEndpointHost,
  asrEndpointUrl,
  parseAsrCompatProtocol,
  resolveAsrApiModel,
  resolveAsrModel,
  resolveAsrModelOption,
  resolveAsrRuntimeProvider,
  type AsrCheck,
  type AsrProfile,
} from './asrProviderCatalog'
import { loadAsrProfiles, saveAsrProfiles } from './asrProfileStore'
import { formatCheckedAt, formatLatency, isCheckFresh } from './aiProviderCatalog'
import { getLocale, t, type TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'

// [自訂] S58：本 fork 只用本機模式，不顯示雲端供應商控制台／申請文件連結（點擊會開外部瀏覽器）。
// 改回 true 即恢復上游行為。
const SHOW_CLOUD_CONSOLE_LINKS = false

const DOC_URL = 'https://my.feishu.cn/wiki/V4vLw2UfDiWcATkK2dyckhvynzc'

const DOUBAO_CONSOLE_OPTIONS = [
  { value: 'new', labelKey: 'asr.console.new' },
  { value: 'legacy', labelKey: 'asr.console.legacy' },
] as const

// ⚠️ 下面兩段是**發給模型的 System Prompt**，不是介面文案，所以刻意保持中文、
// 不進 locale 檔案。Prompt 的語種取決於使用者說什麼話，不取決於介面語言 ——
// 直譯成英文會讓中文口述的整理質量下降。英文 Prompt 集是獨立的一件事，見
// dev-docs/i18n-todo.md 的 P2-5。只有下面 PRESETS 的 label 是界面文案，要翻。
// i18n-allow-start: 發給中文語音模型的產品 Prompt，不隨介面語言切換
const DEFAULT_OMNI_PROMPT = '你是一個語音轉文字助手。請將使用者的語音內容準確轉寫為文字，保持原意，適當加上標點符號，不要加上任何額外的解釋或評論。中文一律以台灣正體中文與台灣用語輸出。'

const OMNI_PROMPT_POLISH = `你是語音文本精煉助手。輸入是 ASR 語音識別的原始轉寫，你的任務是清洗為可直接使用的乾淨文本。
核心原則：保留使用者全部有效資訊，只清除語音噪聲和識別錯誤。
處理規則：
1. 移除口語填充詞（嗯、啊、那個、就是說、然後呢）和無意義的重複、猶豫。
2. 識別自我修正——"不對"、"不是"、"應該是"、"改到"後以最終表達為準，刪除前序錯誤。
3. 修正明顯的語音識別錯誤：同音字、音近字、專有名詞、英文大小寫、數字和時間。
4. 加上標點符號，必要時分段。中英文混合保留合理空格。
5. 檢測到"第一/第二/首先/然後"等結構化表達時，輸出為有序列表。
6. 中文一律以台灣正體中文與台灣用語輸出。
約束：不新增原文沒有的內容，不改變使用者核心語義；不回答、解釋、總結或續寫文本中提到的問題。
只輸出精煉後的文本。`
// i18n-allow-end

const OMNI_PROMPT_PRESETS = [
  { value: DEFAULT_OMNI_PROMPT, labelKey: 'asr.omniPreset.faithful' },
  { value: OMNI_PROMPT_POLISH, labelKey: 'asr.omniPreset.polish' },
] as const satisfies readonly { value: string; labelKey: TranslationKey }[]

const inputClass = 'h-9 w-full rounded-md border border-input-border bg-input-bg px-3 text-sm transition-colors focus:border-input-focus-border'
const selectClass = 'h-9 w-full rounded-md border border-input-border bg-input-bg px-2 text-sm transition-colors focus:border-input-focus-border'
const linkClass = 'inline-flex items-center gap-1 text-xs text-primary underline underline-offset-2 decoration-primary/40 transition-colors hover:decoration-primary'
const cardIconButtonClass = 'pointer-events-auto rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-40'
const helpIconClass = 'h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground'

interface Notice {
  tone: FeedbackTone
  message: string
  detail?: string
}

/**
 * 一次識別測試的結果。`check` 無論成敗都要寫回卡片 —— 失敗也得讓卡上顯示「不可用」，
 * 而不是停在「未測試」讓人以為沒測過。
 */
type AsrTestOutcome =
  | { ok: true; check: AsrCheck; text: string; latencyMs: number; audioSec: number }
  | { ok: false; check: AsrCheck; message: string; detail?: string }

/**
 * 把內建測試音訊準備成後端要的裸 PCM（base64）。
 * 從測試邏輯裡單獨拆出來，是為了讓批次測試**只準備一次**給 N 張卡共用。
 */
async function prepareTestPcm(): Promise<{ pcmB64: string; audioSec: number }> {
  const wavB64 = await invoke<string>('get_test_audio_b64')
  const wavBytes = Uint8Array.from(atob(wavB64), (c) => c.charCodeAt(0))
  const pcmBytes = wavBytes.slice(44) // 去掉 44 位元組 WAV 頭，後端要的是裸 PCM
  const audioSec = pcmBytes.length / 2 / 16000
  let pcmB64 = ''
  const chunk = 8192
  for (let i = 0; i < pcmBytes.length; i += chunk) {
    pcmB64 += String.fromCharCode(...pcmBytes.subarray(i, Math.min(i + chunk, pcmBytes.length)))
  }
  return { pcmB64: btoa(pcmB64), audioSec }
}

/**
 * 真跑一次識別並把結果收成 outcome。
 *
 * 刻意不碰任何 state、也不寫儲存：單張測試與「測試全部」共用它，各自決定怎麼落盤、
 * 怎麼提示。message 裡不帶供應商名字，由呼叫方按自己的語境加字首。
 */
async function runAsrTest(
  profile: AsrProfile,
  audio: { pcmB64: string; audioSec: number },
): Promise<AsrTestOutcome> {
  const entry = findAsrProvider(profile.provider)
  if (!entry) {
    return { ok: false, check: { ok: false, at: Date.now(), reason: t('asr.err.unknownProvider') }, message: t('asr.err.unknownProvider') }
  }
  try {
    const creds = effectiveAsrCredentials(profile)
    // 測的是這張卡自己的配置（可能還沒啟用），所以 provider 與模型都從檔案解析、
    // 不讀執行時鍵。**provider 必須取選中模型的那個**（卡片 id 是平台，不是分發 key）。
    const runtimeProvider = resolveAsrRuntimeProvider(profile)
    const extra = buildAsrExtra(runtimeProvider, {
      model: resolveAsrApiModel(profile),
      // 指令只給 omni 那類模型 —— 千問卡上的 omniPrompt 即使選了非 omni 模型
      // 也可能有值，無條件帶上會把它發給一個根本不讀它的實現。
      instructions: resolveAsrModelOption(profile)?.omni ? profile.omniPrompt : '',
      baseUrl: asrEndpointUrl(profile),
      protocol: profile.protocol,
    })
    const start = performance.now()
    const r = await invoke<{ text: string; elapsed_ms: number }>('cloud_transcribe', {
      request: {
        audio_b64: audio.pcmB64,
        sample_rate: 16000,
        asr_config: {
          provider: runtimeProvider,
          api_key: creds.apiKey,
          app_id: creds.appId,
          ...(extra && { extra }),
        },
      },
    })
    const latencyMs = Math.round(performance.now() - start)
    const text = r.text.trim()
    if (!text) {
      // 連通了但一個字都沒出：多半是資源沒開通或額度問題，不能算可用
      return {
        ok: false,
        check: { ok: false, at: Date.now(), reason: t('asr.err.emptyText') },
        message: t('asr.msg.emptyText'),
      }
    }
    return {
      ok: true,
      check: { ok: true, at: Date.now(), latencyMs, audioSec: audio.audioSec },
      text,
      latencyMs,
      audioSec: audio.audioSec,
    }
  } catch (err) {
    const friendly = describeProviderError(err)
    return {
      ok: false,
      check: { ok: false, at: Date.now(), reason: friendly.message },
      message: t('asr.msg.testFailed', { message: friendly.message }),
      detail: friendly.detail,
    }
  }
}

// 卡片標題的規則（含「為什麼這裡絕不能出現金鑰」）在 asrCardTitle 裡。

export default function CloudAPISection() {
  useT()
  const [profiles, setProfiles] = useState<AsrProfile[]>([])
  const [activeId, setActiveId] = useState('')
  const [loaded, setLoaded] = useState(false)
  const [testingId, setTestingId] = useState('')
  const [notice, setNotice] = useState<Notice | null>(null)
  const [pendingDeleteId, setPendingDeleteId] = useState('')
  /** 「測試全部」的進度；null = 沒在批次測試 */
  const [batch, setBatch] = useState<{ done: number; total: number } | null>(null)
  /** 批次測試是併發的，所以「正在測哪張」是一組而不是一張 */
  const [testingIds, setTestingIds] = useState<string[]>([])

  /** 正在編輯的那份（新建也走這裡）。null = 彈窗關著 */
  const [draft, setDraft] = useState<AsrProfile | null>(null)
  const [draftIsNew, setDraftIsNew] = useState(false)
  const [draftBaseline, setDraftBaseline] = useState('')
  const [saving, setSaving] = useState(false)

  const busy = testingId !== '' || saving || batch !== null
  /** 這張卡正在測嗎 —— 單張測試和併發批次測試都要算上 */
  const isTesting = (id: string) => testingId === id || testingIds.includes(id)
  const draftEntry = draft ? findAsrProvider(draft.provider) : undefined
  const draftPlatform = draftEntry?.platform ?? 'doubao'
  const draftIsDoubao = draftPlatform === 'doubao'
  const draftDirty = draft !== null && JSON.stringify(draft) !== draftBaseline
  /**
   * 金鑰是不是從同平台上一份帶過來的。
   * 用「當前值是否還等於那份的金鑰」判斷：使用者一改成別的，這句提示自己就消失。
   */
  const draftKeyInherited = draft !== null
    && draftIsNew
    && draft.apiKey.trim() !== ''
    && lastProfileOfPlatform(draft.provider, draft.id)?.apiKey === draft.apiKey

  useEffect(() => {
    void load()
    return () => setEngineDraftDirty(false)
  }, [])

  async function load() {
    const state = await loadAsrProfiles()
    setProfiles(state.profiles)
    setActiveId(state.activeId)
    setLoaded(true)
  }

  /** 同平台最近一份配置，用來在新建時沿用金鑰 */
  function lastProfileOfPlatform(providerId: string, excludeId?: string): AsrProfile | undefined {
    const platform = findAsrProvider(providerId)?.platform
    if (!platform) return undefined
    return [...profiles]
      .reverse()
      .find((p) => p.id !== excludeId && findAsrProvider(p.provider)?.platform === platform)
  }

  async function persist(next: AsrProfile[], nextActiveId: string) {
    setProfiles(next)
    setActiveId(nextActiveId)
    await saveAsrProfiles({ profiles: next, activeId: nextActiveId })
    void refreshModeStatus()
  }

  async function handleActivate(id: string) {
    if (id === activeId) return
    setNotice(null)
    await persist(profiles, id)
  }

  // ─────────────────────── 測試 ───────────────────────

  /**
   * 用內建測試音訊真跑一次識別。
   *
   * 為什麼不用更便宜的 test_asr_connection：那個只做握手鑑權，握手過了識別仍可能失敗
   * （資源沒開通、額度用盡），而且它測不出耗時。這一頁要回答「哪個更快」，
   * 就得測真實轉寫。代價是一次真實計費呼叫，頁面上寫明了。
   */
  async function handleTest(profile: AsrProfile) {
    if (busy) return
    const entry = findAsrProvider(profile.provider)
    if (!entry) return
    const missing = describeAsrMissing(profile)
    if (missing) {
      setNotice({ tone: 'warning', message: t('asr.msg.missingBeforeTest', { label: entry.label, missing }) })
      return
    }
    setTestingId(profile.id)
    setNotice(null)
    try {
      const outcome = await runAsrTest(profile, await prepareTestPcm())
      await persist(
        profiles.map((p) => (p.id === profile.id ? { ...p, check: outcome.check } : p)),
        activeId,
      )
      if (!outcome.ok) {
        setNotice({ tone: 'error', message: `${entry.label} ${outcome.message}`, detail: outcome.detail })
        return
      }
      const grade = gradeAsrLatency(outcome.latencyMs, outcome.audioSec)
      setNotice({
        tone: grade.tone === 'bad' ? 'warning' : 'success',
        message: t('asr.msg.testOk', { label: entry.label, sec: outcome.audioSec.toFixed(1), latency: formatLatency(outcome.latencyMs), grade: grade.label }),
        detail: t('asr.msg.testDetail', { text: outcome.text }),
      })
    } catch (err) {
      // 只有準備測試音訊這一步會漏到這裡；識別本身的失敗由 runAsrTest 收成 outcome
      const friendly = describeProviderError(err)
      setNotice({ tone: 'error', message: t('asr.msg.testCrashed', { label: entry.label, message: friendly.message }), detail: friendly.detail })
    } finally {
      setTestingId('')
    }
  }

  /**
   * 一鍵測試全部：**併發**真跑一次識別，一次點選直接開始（沒有二次確認）。
   *
   * ⚠️ 併發的代價，改回序列前先想清楚：卡上的耗時是用來橫向比較「哪個更快」的，
   * 併發跑出來的數字互相汙染 —— 同平台的卡尤其明顯（千問那幾個變體共用一把百煉
   * 金鑰，打的是同一個服務），還可能撞上限流被記成「不可用」。所以批次結果裡會
   * 註明這批耗時不宜橫向比較；要拿準數字，用卡上的單張測試。
   *
   * 另外兩個決定：
   *  · **一次性落盤**，不逐張寫。併發下每個回撥拿到的都是同一份舊 profiles，
   *    逐張 persist 會互相覆蓋，只剩最後一個的結論。代價是中途關頁面這批就沒了 ——
   *    併發之後整批只等最慢那張，這個視窗很短。
   *  · **跳過沒配完的**，不給它們寫「不可用」。它們缺的是金鑰而不是可用性，
   *    寫進結論只會汙染卡片狀態；跳過幾張會在結果裡說明。
   */
  async function handleTestAll() {
    if (busy) return
    const targets = profiles.filter((p) => !describeAsrMissing(p))
    const skipped = profiles.length - targets.length
    if (targets.length === 0) {
      setNotice({
        tone: 'warning',
        message: t('asr.msg.nothingToTest'),
      })
      return
    }

    setNotice(null)
    setBatch({ done: 0, total: targets.length })
    setTestingIds(targets.map((p) => p.id))

    try {
      // 測試音訊只準備一次，N 張卡共用
      const audio = await prepareTestPcm()
      let done = 0
      const results = await Promise.all(targets.map(async (target) => {
        const outcome = await runAsrTest(target, audio)
        // 每張自己測完就把圖示停下、進度加一，不用等整批結束
        done += 1
        setBatch({ done, total: targets.length })
        setTestingIds((prev) => prev.filter((id) => id !== target.id))
        return { target, outcome }
      }))

      const checks = new Map(results.map((r) => [r.target.id, r.outcome.check]))
      await persist(
        profiles.map((p) => {
          const check = checks.get(p.id)
          return check ? { ...p, check } : p
        }),
        activeId,
      )

      let okCount = 0
      let failCount = 0
      const lines = results.map(({ target, outcome }) => {
        const label = findAsrProvider(target.provider)?.label ?? target.provider
        if (outcome.ok) {
          okCount += 1
          const grade = gradeAsrLatency(outcome.latencyMs, outcome.audioSec)
          return t('asr.msg.batchLine', { label, latency: formatLatency(outcome.latencyMs), grade: grade.label })
        }
        failCount += 1
        return `${label}: ${outcome.message}`
      })

      const parts = [t('asr.msg.batchOk', { count: okCount })]
      if (failCount > 0) parts.push(t('asr.msg.batchFail', { count: failCount }))
      if (skipped > 0) parts.push(t('asr.msg.batchSkipped', { count: skipped }))
      if (targets.length > 1) {
        lines.push('', t('asr.msg.batchNote'))
      }
      setNotice({
        tone: failCount > 0 ? 'warning' : 'success',
        // 連線詞也走 locale：中文用「，」，英文用「, 」
        message: t('asr.msg.batchDone', { parts: parts.join(t('asr.listSeparator')) }),
        detail: lines.join('\n'),
      })
    } catch (err) {
      // 只有準備測試音訊這一步會漏到這裡；識別本身的失敗由 runAsrTest 收成 outcome
      const friendly = describeProviderError(err)
      setNotice({ tone: 'error', message: t('asr.msg.batchCrashed', { message: friendly.message }), detail: friendly.detail })
    } finally {
      setTestingIds([])
      setBatch(null)
    }
  }

  // ─────────────────────── 新建 / 編輯 ───────────────────────

  function openEditor(profile: AsrProfile, isNew: boolean) {
    setDraft(profile)
    setDraftIsNew(isNew)
    setDraftBaseline(JSON.stringify(profile))
    setNotice(null)
  }

  function handleNew() {
    const fresh = emptyAsrProfile()
    // 同平台已有配置時把金鑰帶過來：最常見的一次操作是「同一個帳號、換個模型再存一份」，
    // 否則又要回控制台複製貼上一遍
    const prev = lastProfileOfPlatform(fresh.provider, fresh.id)
    if (prev) {
      fresh.apiKey = prev.apiKey
      fresh.otherKey = prev.otherKey
      fresh.appId = prev.appId
      fresh.console = prev.console
      fresh.workspaceId = prev.workspaceId
    }
    fresh.omniPrompt = DEFAULT_OMNI_PROMPT
    openEditor(fresh, true)
  }

  function closeEditor() {
    if (saving) return
    setDraft(null)
    setEngineDraftDirty(false)
  }

  function patchDraft(next: Partial<AsrProfile>) {
    if (!draft) return
    const merged = { ...draft, ...next }
    setDraft(merged)
    setEngineDraftDirty(JSON.stringify(merged) !== draftBaseline)
  }

  /** 換供應商：跨平台時清掉不屬於新平台的憑據，並沿用新平台已有的金鑰 */
  function handleDraftProvider(providerId: string) {
    if (!draft) return
    const nextPlatform = findAsrProvider(providerId)?.platform
    const prevPlatform = findAsrProvider(draft.provider)?.platform
    // 換卡就回到新卡的預設模型。**顯式寫入而不是留空** —— 空串會被 parseAsrProfiles
    // 當成存量資料走遷移（見那邊的判據說明）。
    const nextModel = asrModelsOf(findAsrProvider(providerId)!)[0].id
    if (nextPlatform === prevPlatform) {
      patchDraft({ provider: providerId, model: nextModel })
      return
    }
    // 平台變了，舊金鑰對新家沒有意義 —— 留著只會被當成"已配置"而實際發不出去
    const prev = lastProfileOfPlatform(providerId, draft.id)
    patchDraft({
      provider: providerId,
      model: nextModel,
      apiKey: prev?.apiKey ?? '',
      otherKey: prev?.otherKey ?? '',
      appId: prev?.appId ?? '',
      console: prev?.console ?? 'new',
      workspaceId: prev?.workspaceId ?? '',
    })
  }

  /** 切換豆包控制台代次：把當前金鑰留給這一代，取出另一代的填進來 */
  function switchConsole(next: string) {
    if (!draft || (next !== 'new' && next !== 'legacy')) return
    if (next === draft.console) return
    patchDraft({ console: next, apiKey: draft.otherKey, otherKey: draft.apiKey })
  }

  async function handleSaveDraft() {
    if (!draft || saving) return
    setSaving(true)
    try {
      // 改過憑據就把舊結論作廢：否則卡上那枚用舊 key 測出來的「可用」還掛著，
      // 等於拿過期結論替新配置作保
      const before = profiles.find((p) => p.id === draft.id)
      const credsChanged = !before
        || JSON.stringify(effectiveAsrCredentials(before)) !== JSON.stringify(effectiveAsrCredentials(draft))
      const saved: AsrProfile = { ...draft, check: credsChanged ? undefined : before?.check }

      const next = draftIsNew
        ? [...profiles, saved]
        : profiles.map((p) => (p.id === saved.id ? saved : p))
      // 新建的那份直接啟用：使用者剛配好它，多半就是想用它
      await persist(next, draftIsNew ? saved.id : activeId)

      setDraft(null)
      setEngineDraftDirty(false)
      const missing = describeAsrMissing(saved)
      setNotice(missing
        ? { tone: 'warning', message: t('asr.msg.savedWithMissing', { missing }) }
        : { tone: 'success', message: t('asr.msg.saved') })
    } finally {
      setSaving(false)
    }
  }

  async function handleDelete(id: string) {
    const next = profiles.filter((p) => p.id !== id)
    // 刪掉的正好是啟用中的那份時，讓 resolveActiveAsrProfile 回落到第一條
    await persist(next, id === activeId ? (next[0]?.id ?? '') : activeId)
    setPendingDeleteId('')
    setNotice(null)
  }

  // ─────────────────────── 卡片 ───────────────────────

  interface CardStatus {
    label: string
    tone: 'neutral' | 'ok' | 'warn' | 'bad'
    spoken: string
    hint: string
    /**
     * 這張卡還缺配置。
     * 單獨一個標誌而不是複用 tone === 'warn'：延遲「偏慢」也是 warn，
     * 但那不需要使用者去填任何東西，按鈕不必常駐。
     */
    needsSetup?: boolean
  }

  function describeCard(profile: AsrProfile): CardStatus {
    if (isTesting(profile.id)) {
      return { label: t('asr.status.testing'), tone: 'neutral', spoken: t('asr.status.testingSpoken'), hint: t('asr.status.testingHint') }
    }
    const missing = describeAsrMissing(profile)
    if (missing) {
      return { label: t('asr.status.needsSetup'), tone: 'warn', spoken: missing, hint: missing, needsSetup: true }
    }
    // 業務空間 ID 是**模型級**要求（千問那五個裡只有 realtime 那個要），
    // 按卡片判會讓選了別的模型的使用者也被要求填
    if (resolveAsrModelOption(profile)?.needsWorkspaceId && !profile.workspaceId.trim()) {
      return {
        label: t('asr.status.missingWorkspace'),
        tone: 'warn',
        spoken: t('asr.status.missingWorkspaceSpoken'),
        hint: t('asr.status.missingWorkspaceHint'),
        needsSetup: true,
      }
    }
    const check = profile.check
    if (!check) {
      return { label: t('asr.status.untested'), tone: 'neutral', spoken: t('asr.status.untestedSpoken'), hint: t('asr.status.untestedHint') }
    }
    if (!check.ok) {
      const reason = check.reason ?? t('common.unknownReason')
      return {
        label: t('asr.status.unavailable'),
        tone: 'bad',
        spoken: t('asr.status.unavailableSpoken', { reason }),
        hint: t('asr.status.unavailableHint', { when: formatCheckedAt(check.at), reason }),
      }
    }
    const fresh = isCheckFresh(check)
    const ms = check.latencyMs
    if (ms === undefined) {
      return { label: t('asr.status.available'), tone: fresh ? 'ok' : 'neutral', spoken: t('asr.status.available'), hint: t('asr.status.availableHint', { when: formatCheckedAt(check.at) }) }
    }
    const grade = gradeAsrLatency(ms, check.audioSec ?? 0)
    return {
      label: formatLatency(ms),
      // 結論過期就把顏色收回中性：綠色只代表「剛剛驗過，可以信」
      tone: fresh ? grade.tone : 'neutral',
      spoken: t('asr.status.availableSpoken', { grade: grade.label, latency: formatLatency(ms) }),
      hint: t('asr.status.availableDetailHint', {
        when: formatCheckedAt(check.at),
        sec: (check.audioSec ?? 0).toFixed(1),
        latency: formatLatency(ms),
        grade: grade.label,
        stale: fresh ? '' : t('asr.staleSuffix'),
      }),
    }
  }

  const chipToneClass: Record<CardStatus['tone'], string> = {
    neutral: 'bg-muted text-muted-foreground',
    ok: 'bg-success/10 text-success-strong',
    warn: 'bg-warning/10 text-warning-strong',
    bad: 'bg-destructive/10 text-destructive',
  }

  function renderCard(profile: AsrProfile) {
    const entry = findAsrProvider(profile.provider)
    if (!entry) return null
    const isActive = profile.id === activeId
    const status = describeCard(profile)
    const siblings = profiles.filter((p) => p.provider === profile.provider).length
    const title = asrCardTitle(profile, siblings)
    const availability = asrAvailabilityLabel(entry)
    // 卡上那行小字必須是**這張卡真正會用的**模型，不是目錄裡的預設值 ——
    // 否則選了 whisper-large-v3 的卡看上去還寫著 turbo。
    //
    // 顯示**真實模型 ID**，不再顯示短名。短名是「一張卡 = 一個模型」時代留下的，
    // 那時它們是卡片標題（「千問 ASR 一次性」靠「一次性」跟流式那張卡對比）。
    // 挪進卡內當模型名之後，字首重複、對比物件也沒了，使用者看到的是一個
    // 既不像模型也說不清是什麼的詞。真實 ID 反而一眼認得出，也能拿去對文件和帳單。
    const option = resolveAsrModelOption(profile)
    const model = option ? option.id : resolveAsrModel(profile)
    // 自定義端點的卡再補一個主機名：那種卡之間真正的差別是「連的是哪台」
    const host = asrEndpointHost(profile)
    const modelLine = host ? `${model} · ${host}` : model
    return (
      <div
        key={profile.id}
        className={cn(
          'group relative rounded-lg border p-2.5 transition-colors',
          isActive ? 'border-primary bg-primary/5' : 'border-border hover:bg-accent/50',
        )}
      >
        {/* 鋪滿整卡的單選按鈕：卡上還有自己的圖示按鈕，不能把整張卡做成 <button>
            （按鈕套按鈕，讀屏和鍵盤都會錯） */}
        <button
          type="button"
          role="radio"
          aria-checked={isActive}
          aria-label={t('common.cardStatusAria', { title, subtitle: model, status: status.spoken })}
          onClick={() => void handleActivate(profile.id)}
          className="absolute inset-0 rounded-lg"
        />
        <div className="flex items-center gap-1.5">
          {isActive && (
            <Tooltip className="pointer-events-auto relative z-10 shrink-0" content={t('common.inUse')}>
              <CheckCircle2 className="h-4 w-4 shrink-0 cursor-help text-success-strong" aria-label={t('common.inUse')} />
            </Tooltip>
          )}
          <span className="min-w-0 flex-1 truncate text-xs font-medium" title={title}>{title}</span>
          <Tooltip className="pointer-events-auto relative z-10 shrink-0" variant="light" content={status.hint}>
            <span className={cn('cursor-help rounded-full px-2 py-0.5 text-[11px] font-medium', chipToneClass[status.tone])}>
              {status.label}
            </span>
          </Tooltip>
        </div>
        <div className="mt-1 flex items-center gap-1.5">
          {/* title 給完整模型 ID：卡上顯示的是短名，滑鼠懸停能看到真名 */}
          <span className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground" title={modelLine}>
            {modelLine}
          </span>
          {/* 待配置的卡片按鈕常駐：它正在等使用者去填東西，把唯一的入口藏進 hover
              等於讓人對著一張沒有可點之處的卡發呆。配好之後回到 hover 顯隱 */}
          <div
            className={cn(
              'pointer-events-none relative z-10 flex shrink-0 items-center gap-0.5 transition-opacity focus-within:opacity-100 group-hover:opacity-100',
              status.needsSetup ? 'opacity-100' : 'opacity-0',
            )}
          >
            <Tooltip className="pointer-events-auto" content={t('common.test')}>
              <button
                type="button"
                onClick={() => void handleTest(profile)}
                disabled={busy}
                aria-label={t('asr.testAria', { title })}
                className={cardIconButtonClass}
              >
                <RefreshCw className={cn('h-3.5 w-3.5', isTesting(profile.id) && 'animate-spin')} aria-hidden />
              </button>
            </Tooltip>
            <Tooltip className="pointer-events-auto" content={t('common.edit')}>
              <button
                type="button"
                onClick={() => openEditor({ ...profile }, false)}
                aria-label={t('asr.editAria', { title })}
                className={cardIconButtonClass}
              >
                <Pencil className="h-3.5 w-3.5" aria-hidden />
              </button>
            </Tooltip>
            <Tooltip className="pointer-events-auto" content={t('common.delete')}>
              <button
                type="button"
                onClick={() => setPendingDeleteId(profile.id)}
                disabled={busy}
                aria-label={t('asr.deleteAria', { title })}
                className={cardIconButtonClass}
              >
                <Trash2 className="h-3.5 w-3.5" aria-hidden />
              </button>
            </Tooltip>
          </div>
        </div>
        <p className="mt-1 line-clamp-2 text-[11px] leading-snug text-muted-foreground/80">
          {entry.blurb}
          {/* 地區提示多數供應商為空（見 asrAvailabilityLabel），條件渲染避免留一個空 span 和多餘空格 */}
          {availability !== '' && <> <span className="font-medium">{availability}</span></>}
        </p>
      </div>
    )
  }

  // ─────────────────────── 彈窗 ───────────────────────

  function renderEditor() {
    if (!draft) return null
    const platformInfo = ASR_PLATFORMS[draftPlatform]
    const draftEntry = findAsrProvider(draft.provider)
    const draftAvailability = draftEntry ? asrAvailabilityLabel(draftEntry) : ''
    const draftModels = draftEntry ? asrModelsOf(draftEntry) : []
    const draftModelGroups = groupAsrModelsByVendor(draftModels)
    // 業務空間 ID、System Prompt 這些都是**模型級**條件，要按選中的那個模型算
    const draftModelOption = resolveAsrModelOption(draft)
    const keyLabel = draftIsDoubao ? doubaoKeyLabel(draft.console) : 'API Key'
    return (
      <Modal
        title={draftIsNew ? t('asr.editorNew') : t('asr.editorEdit')}
        onClose={closeEditor}
        locked={saving}
        showCloseButton
        panelClassName="w-[520px]"
      >
        <div className="mt-4 space-y-3">
          <div>
            <label htmlFor="asr-provider" className="mb-1 block text-sm text-muted-foreground">{t('asr.provider')}</label>
            <select
              id="asr-provider"
              value={draft.provider}
              onChange={(e) => handleDraftProvider(e.target.value)}
              className={selectClass}
            >
              {/* 平鋪，**不按「內建 / 自己填地址」分組**。曾經分成兩個 optgroup，
                  使用者否掉了：在他看來這些都是「一個能用的語音識別服務」，分組等於把
                  我們的實現差異擺到選擇介面上。要填什麼由下面那幾欄各自說明
                  （位址列和協議欄本來就只在需要時出現）。 */}
              {ASR_PROVIDERS.map((p) => {
                const models = asrModelsOf(p)
                // 多模型的只列平台名（模型由下面那個下拉選）；**單模型的直接把模型名
                // 帶出來** —— 那種情況下下面的模型下拉根本不渲染，不帶的話使用者在這一步
                // 完全看不到自己要用的是哪個模型。協議卡除外：它那一個模型只是預設值，
                // 真正用哪個由使用者自己填。
                const showModel = models.length === 1 && !p.customEndpoint
                return (
                  <option key={p.id} value={p.id}>
                    {showModel ? `${p.label} · ${models[0].id}` : p.label}
                  </option>
                )
              })}
            </select>
            <p className="mt-1 text-xs text-muted-foreground">
              {draftEntry?.blurb}
              {draftAvailability !== '' && <> <span className="font-medium">{draftAvailability}</span></>}
            </p>
          </div>

          {/* 卡片名稱。可空，留空就顯示平台名。
              這一欄是為了取代「自動在標題後面補金鑰末 4 位」那個做法 —— 同平台兩張卡
              到底差在哪，使用者比我們清楚，而我們那個區分方式讓人以為金鑰洩露了。 */}
          <div>
            <label htmlFor="asr-name" className="mb-1 block text-sm text-muted-foreground">
              {t('asr.cardName')}
            </label>
            <input
              id="asr-name"
              value={draft.name}
              onChange={(e) => patchDraft({ name: e.target.value })}
              onKeyDown={(e) => { if (e.key === 'Enter') void handleSaveDraft() }}
              placeholder={draftEntry?.label ?? ''}
              className={inputClass}
            />
            <p className="mt-1 text-xs text-muted-foreground">{t('asr.cardNameHint')}</p>
          </div>

          {/* 介面地址。**按選中的模型決定要不要顯示**，不是按卡片 ——
              同一張卡裡可以既有 HTTP 模型（能改地址）又有 WebSocket 模型（改不了），
              千問那張卡就是。協議卡是必填，內建卡是可選覆蓋（中轉站 / 反代）。 */}
          {draftModelOption?.supportsCustomUrl && (
            <div>
              <label htmlFor="asr-api-url" className="mb-1 block text-sm text-muted-foreground">
                {draftModelOption.requiresCustomUrl ? t('asr.apiUrl') : t('asr.apiUrlOptional')}
              </label>
              <input
                id="asr-api-url"
                type="url"
                inputMode="url"
                value={draft.apiUrl}
                onChange={(e) => patchDraft({ apiUrl: e.target.value })}
                onKeyDown={(e) => { if (e.key === 'Enter') void handleSaveDraft() }}
                placeholder={draftEntry?.urlPlaceholder ?? ''}
                className={inputClass}
              />
              <p className="mt-1 text-xs text-muted-foreground">
                {draftModelOption.requiresCustomUrl ? t('asr.apiUrlHint') : t('asr.apiUrlOptionalHint')}
              </p>
            </div>
          )}

          {/* 協議：只有協議卡需要，而且預設「自動」。
              擺在這裡而不是藏進高階設定，是因為自動探測判不準時使用者得找得到它；
              但預設值讓絕大多數人不必碰它。 */}
          {draftEntry?.customEndpoint && (
            <div>
              <label htmlFor="asr-protocol" className="mb-1 block text-sm text-muted-foreground">
                {t('asr.protocol')}
              </label>
              <select
                id="asr-protocol"
                value={draft.protocol}
                onChange={(e) => patchDraft({ protocol: parseAsrCompatProtocol(e.target.value) })}
                className={selectClass}
              >
                {ASR_COMPAT_PROTOCOLS.map((value) => (
                  <option key={value} value={value}>{t(`asr.protocol.${value}`)}</option>
                ))}
              </select>
              <p className="mt-1 text-xs text-muted-foreground">{t('asr.protocolHint')}</p>
            </div>
          )}

          {/* 協議卡的模型是**自由文本**：對面掛什麼模型我們無從知道，
              給個下拉等於替使用者瞎猜。佔位符給的是常見預設值。 */}
          {draftEntry?.customEndpoint && (
            <div>
              <label htmlFor="asr-model-text" className="mb-1 block text-sm text-muted-foreground">
                {t('asr.model')}
              </label>
              <input
                id="asr-model-text"
                value={draft.model}
                onChange={(e) => patchDraft({ model: e.target.value })}
                onKeyDown={(e) => { if (e.key === 'Enter') void handleSaveDraft() }}
                placeholder={draftModels[0]?.id ?? ''}
                className={inputClass}
              />
              <p className="mt-1 text-xs text-muted-foreground">{t('asr.modelFreeTextHint')}</p>
            </div>
          )}

          {/* 模型下拉。**只有一個模型時不渲染** —— 擺一個點開只有一項的下拉，
              等於讓使用者以為這裡有得選。多數服務換模型 = 換服務條目（協議都不一樣），
              真正有多個模型的只有走同一個端點的那幾家。
              OpenRouter 有 20 多個、來自十來家廠商，所以按廠商分組（optgroup）；
              別家的模型名不帶廠商字首，groupAsrModelsByVendor 會返回 null 走平鋪。 */}
          {draftModels.length > 1 && (
            <div>
              <label htmlFor="asr-model" className="mb-1 block text-sm text-muted-foreground">{t('asr.model')}</label>
              <select
                id="asr-model"
                value={resolveAsrModel(draft)}
                onChange={(e) => patchDraft({ model: e.target.value })}
                className={selectClass}
              >
                {draftModelGroups
                  ? draftModelGroups.map(([vendor, models]) => (
                    <optgroup key={vendor} label={vendor}>
                      {models.map((option) => (
                        <option key={option.id} value={option.id}>
                          {option.id === draftModels[0].id
                            ? t('asr.modelDefaultOption', { model: option.id })
                            : option.id}
                        </option>
                      ))}
                    </optgroup>
                  ))
                  : draftModels.map((option, index) => (
                    <option key={option.id} value={option.id}>
                      {index === 0
                        ? t('asr.modelDefaultOption', { model: option.id })
                        : option.id}
                    </option>
                  ))}
              </select>
              {/* 選中模型自己的一句定位優先 —— 真正要幫使用者做的決定是「在這一家裡面
                  選哪個」，而那五個千問模型的差別比千問和豆包的差別還大。
                  模型沒寫 blurb 時退回講整張卡的那句話。 */}
              <p className="mt-1 text-xs text-muted-foreground">
                {draftModelOption?.blurb
                  ?? (draftModelGroups ? t('asr.modelHintRouted') : t('asr.modelHint'))}
              </p>
            </div>
          )}

          {draftIsDoubao && (
            <div>
              <label className="mb-1.5 block text-sm text-muted-foreground">{t('asr.consoleVersion')}</label>
              <Segmented
                label={t('asr.consoleVersionLabel')}
                size="sm"
                value={draft.console}
                // 常量表存的是 key，到這裡才翻成當前語言
                options={DOUBAO_CONSOLE_OPTIONS.map((o) => ({ value: o.value, label: t(o.labelKey) }))}
                onChange={switchConsole}
              />
              <p className="mt-1.5 text-xs text-muted-foreground">
                {draft.console === 'new'
                  ? t('asr.consoleNewHint')
                  : t('asr.consoleLegacyHint')}
              </p>
            </div>
          )}

          {draftIsDoubao && draft.console === 'legacy' && (
            <div>
              <label htmlFor="asr-app-id" className="mb-1 block text-sm text-muted-foreground">App ID</label>
              <input
                id="asr-app-id"
                value={draft.appId}
                onChange={(e) => patchDraft({ appId: e.target.value })}
                onKeyDown={(e) => { if (e.key === 'Enter') void handleSaveDraft() }}
                placeholder={t('asr.appIdPlaceholder')}
                className={inputClass}
              />
              {draft.appId.trim() && !/^\d+$/.test(draft.appId.trim()) && (
                <FormatHint text={t('asr.hint.appId')} />
              )}
            </div>
          )}

          {/* data-modal-autofocus：彈窗開啟時焦點落在金鑰上（這才是要乾的事），
              而不是第一個可聚焦元素 */}
          <div data-modal-autofocus>
            <label htmlFor="asr-api-key" className="mb-1 block text-sm text-muted-foreground">{keyLabel}</label>
            <PasswordInput
              id="asr-api-key"
              label={keyLabel}
              value={draft.apiKey}
              onChange={(v) => patchDraft({ apiKey: v })}
              onSubmit={() => void handleSaveDraft()}
              placeholder={t('asr.keyPlaceholder', { platform: platformInfo.label, keyLabel })}
              className={inputClass}
            />
            {/\s/.test(draft.apiKey) && (
              <FormatHint text={t('asr.hint.key')} />
            )}
            {draftPlatform === 'qwen' && draft.apiKey.trim() && !/^sk-/.test(draft.apiKey.trim()) && (
              <FormatHint text={t('asr.hint.bailianKey')} />
            )}
            {/* 金鑰不是憑空出現的：說一句它從哪來，免得使用者以為自己看錯了 */}
            {draftKeyInherited && (
              <p className="mt-1 text-[11px] text-muted-foreground">
                {t('asr.keyReused')}
              </p>
            )}
            <div className="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1">
              {SHOW_CLOUD_CONSOLE_LINKS && (
              <button type="button" onClick={() => void shellOpen(platformInfo.consoleUrl)} className={linkClass}>
                {t('asr.openConsole', { platform: platformInfo.label })}
                <ExternalLink className="h-3 w-3" aria-hidden />
              </button>
              )}
              {/* 配置文件只對豆包顯示。別的平台都是「去控制台複製一把 API Key」，
                  上面那個控制台連結已經足夠；只有豆包要在新舊兩代控制台之間做選擇
                  （新版只給 API Key，舊版還要 Access Token + App ID），光看介面講不清，
                  確實需要一篇文件。給所有平台都掛上等於讓使用者為一件不復雜的事去讀文件。
                  英文文件釋出前，這個連結只對中文介面顯示。 */}
              {SHOW_CLOUD_CONSOLE_LINKS && draftPlatform === 'doubao' && getLocale() === 'zh-TW' && (
                <button type="button" onClick={() => void shellOpen(DOC_URL)} className={linkClass}>
                  {t('asr.howToGetKey')}
                  <ExternalLink className="h-3 w-3" aria-hidden />
                </button>
              )}
            </div>
          </div>

          {draftModelOption?.needsWorkspaceId && (
            <div>
              <label htmlFor="qwen-workspace-id" className="mb-1 block text-sm text-muted-foreground">
                {t('asr.workspaceId')}
              </label>
              <PasswordInput
                id="qwen-workspace-id"
                label={t('asr.workspaceId')}
                value={draft.workspaceId}
                onChange={(v) => patchDraft({ workspaceId: v })}
                placeholder={t('asr.workspacePlaceholder')}
                className={inputClass}
              />
              <p className="mt-1.5 text-xs text-muted-foreground">
                {t('asr.workspaceHint')}
              </p>
            </div>
          )}

          {/* Omni 是「識別 + 整理」一體的模型，System Prompt 決定它整理成什麼樣，
              屬於這份服務自己的行為，所以放在這份配置裡 */}
          {draftModelOption?.omni && (
            <div>
              <label htmlFor="omni-system-prompt" className="mb-1.5 block text-sm text-muted-foreground">
                System Prompt
              </label>
              <Segmented
                className="mb-1.5"
                label={t('asr.systemPromptPreset')}
                size="sm"
                value={draft.omniPrompt}
                options={OMNI_PROMPT_PRESETS.map((o) => ({ value: o.value, label: t(o.labelKey) }))}
                onChange={(v) => patchDraft({ omniPrompt: v })}
              />
              <textarea
                id="omni-system-prompt"
                value={draft.omniPrompt}
                onChange={(e) => patchDraft({ omniPrompt: e.target.value })}
                placeholder={DEFAULT_OMNI_PROMPT}
                rows={2}
                className="w-full resize-y rounded-md border border-input-border bg-input-bg px-3 py-2 text-sm transition-colors focus:border-input-focus-border"
              />
              <p className="mt-1.5 text-xs text-muted-foreground">
                {t('asr.omniNote')}
              </p>
            </div>
          )}

          <div className="flex items-center justify-end gap-2 pt-1">
            <Button variant="outline" size="sm" onClick={closeEditor} disabled={saving}>{t('common.cancel')}</Button>
            <Button size="sm" onClick={() => void handleSaveDraft()} disabled={saving || !draftDirty}>
              {saving ? t('common.saving') : t('common.save')}
            </Button>
          </div>
        </div>
      </Modal>
    )
  }

  const pendingDelete = profiles.find((p) => p.id === pendingDeleteId) ?? null

  return (
    <Card>
      <CardContent className="p-6">
        <div className="mb-3 flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 grow basis-[18rem]">
            <div className="flex items-center gap-2">
              <h2 id="asr-service-heading" className="text-lg font-semibold">{t('asr.title')}</h2>
              <Tooltip
                variant="light"
                content={t('asr.help')}
              >
                <Info aria-label={t('asr.helpAria')} className={helpIconClass} />
              </Tooltip>
            </div>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {t('asr.desc')}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {/* 併發跑，所以沒有「停止」：invoke 發出去的請求沒法撤回，
                擺一顆停不掉任何東西的按鈕比沒有更糟。整批只等最慢那張，很快。 */}
            <Button
              variant="outline"
              size="sm"
              className="h-8 shrink-0"
              onClick={() => void handleTestAll()}
              disabled={busy || profiles.length === 0}
            >
              <RefreshCw className={cn('mr-1 h-3.5 w-3.5', batch && 'animate-spin')} aria-hidden />
              {batch ? t('asr.testingBatch', { done: batch.done, total: batch.total }) : t('asr.testAll')}
            </Button>
            <Button variant="outline" size="sm" className="h-8 shrink-0" onClick={handleNew} disabled={busy}>
              <Plus className="mr-1 h-3.5 w-3.5" aria-hidden />
              {t('common.new')}
            </Button>
          </div>
        </div>

        {!loaded ? (
          <p className="py-2 text-sm text-muted-foreground">{t('asr.loading')}</p>
        ) : profiles.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border px-4 py-4 text-center text-sm text-muted-foreground">
            {t('asr.empty')}
          </p>
        ) : (
          <div
            role="radiogroup"
            aria-labelledby="asr-service-heading"
            className="grid gap-2.5 sm:grid-cols-3"
          >
            {profiles.map(renderCard)}
          </div>
        )}

        {notice && (
          <Feedback className="mt-3" tone={notice.tone} message={notice.message} detail={notice.detail} />
        )}
      </CardContent>

      {renderEditor()}

      {pendingDelete && (
        <Modal title={t('asr.deleteTitle')} onClose={() => setPendingDeleteId('')} showCloseButton panelClassName="w-[420px]">
          <div className="mt-3 space-y-4">
            <p className="text-sm text-muted-foreground">
              {t('asr.deleteBody', { name: asrCardTitle(pendingDelete, profiles.filter((p) => p.provider === pendingDelete.provider).length) })}
            </p>
            <div className="flex items-center justify-end gap-2">
              <Button variant="outline" size="sm" onClick={() => setPendingDeleteId('')}>{t('common.cancel')}</Button>
              <Button size="sm" onClick={() => void handleDelete(pendingDelete.id)}>{t('common.delete')}</Button>
            </div>
          </div>
        </Modal>
      )}
    </Card>
  )
}
