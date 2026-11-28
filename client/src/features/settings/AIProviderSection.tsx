// 「AI 服務」—— 一張卡 = 一份完整可用的配置（供應商 + 地址 + 金鑰 + 模型）。
//
// 為什麼不是原來的「先選供應商、再填一套」：那個形狀下儲存按供應商分槽
// （`cloudAi.<provider>.*`），於是
//   1. 兩個不同端點的「OpenAI 相容」物理上存不下第二份——點模型只改 model 欄位、
//      地址和金鑰不動，兩個模型實際都打向同一個端點，其中一個必然是錯的；
//   2. 候選模型按當前供應商過濾，想比較 DeepSeek 和千問哪個出字快，得來回撥下拉，
//      而出字快慢是口述場景裡選 LLM 唯一壓倒性重要的指標。
// 現在一張卡就是一份完整配置，點一下整份生效；網格是平的，所有候選同屏可比。
//
// 形狀不是新造的，是本倉庫既有的兩處房規：
//   - 單選卡片網格 = `WorkModeSection`（工作模式）與外觀頁主題：
//     `role="radiogroup"` + `grid gap-3 sm:grid-cols-3`，選中態 `border-primary bg-primary/5`
//     再加一個對勾（不讓"當前是哪個"只依賴顏色）。
//   - 新建 / 編輯 / 刪除走 `Modal`（和「更改模型目錄」「確認刪除模型」同一個殼）：
//     它自帶 Esc、焦點陷阱、關閉後還原焦點。上一版把編輯面板掛在頁面下方，
//     點「新建」螢幕上毫無變化，使用者以為按鈕壞了；彈窗從根上沒有這個問題。
//
// 卡上顯示什麼，標準是"能不能把兩張卡區分開"：模型名 + 供應商永遠顯示；
// 主機名只在它不是該供應商預設地址時才顯示（見 profileCustomHost 的註釋）；
// 完整 URL 只在編輯彈窗裡。耗時是這一頁唯一屬於 SayIt 的資訊，常顯。

import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open as shellOpen } from '@tauri-apps/plugin-shell'
import { CheckCircle2, ExternalLink, Info, Pencil, Plus, RefreshCw, Trash2, X } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Feedback, FormatHint, type FeedbackTone } from '@/components/ui/feedback'
import { Modal } from '@/components/ui/modal'
import { PasswordInput } from '@/components/ui/password-input'
import { Switch } from '@/components/ui/switch'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { getSetting, setSetting } from '@/services/store'
import {
  SERVER_AI_SOURCE_KEY,
  setRuntimeServerAiSource,
  type ServerAiSource,
} from '@/services/transcription/serverAiSource'
import { setEngineDraftDirty } from '@/stores/engineDraft'
import { useAiEnabled } from '@/hooks/useAiEnabled'
import { describeProviderError } from '@/lib/errorMessages'
import {
  aiProvidersForDisplay,
  blankProfile,
  checkAiKeyFormat,
  checkApiUrl,
  extractTestReply,
  findProvider,
  formatCheckedAt,
  formatLatency,
  gradeLatency,
  isCheckFresh,
  isProfileComplete,
  profileSubtitle,
  profileTitle,
  preferredAiProviderValue,
  providerLabel,
  resolveActiveProfile,
  type AiProfile,
  type AiProfileCheck,
} from './aiProviderCatalog'
import { loadAiProfiles, saveAiProfiles } from './aiProfileStore'
import { getLocale, t } from '@/i18n'
import { useT } from '@/i18n/useT'

interface TestResult { ok: boolean; message: string; elapsed_ms: number; detail?: string }

interface Notice {
  tone: FeedbackTone
  message: string
  detail?: string
  /** 結果顯示在哪：彈窗裡（儲存/測試）還是網格下方（卡上的測速） */
  scope: 'editor' | 'list'
}

interface TestOutcome {
  ok: boolean
  elapsedMs: number
  reply: string
  message: string
  detail?: string
}

/** 「未儲存」的判斷基準：模型清單也算在內，但檢測結論不算（它不是使用者填的東西） */
function draftSnapshot(draft: AiProfile, models: string[]): string {
  return JSON.stringify({
    provider: draft.provider,
    apiUrl: draft.apiUrl,
    apiKey: draft.apiKey,
    models,
  })
}

/** 把一次測試的結果收成落盤用的結論。失敗原因只留那句人話，原始異常不進儲存 */
function outcomeToCheck(outcome: TestOutcome): AiProfileCheck {
  return outcome.ok
    ? { ok: true, at: Date.now(), latencyMs: outcome.elapsedMs }
    : { ok: false, at: Date.now(), reason: outcome.message }
}

interface CardStatus {
  /** 卡上那枚標籤的可見文字。可用時只放數字（320ms），檔位靠顏色區分，把寬度讓給模型名 */
  text: string
  /** 標籤的底色 + 文字色（tone 就靠它表達，不再另畫圓點） */
  box: string
  /** 讀屏用的完整說法：數字之外還念出結論詞（可用 · 很快），補上視力使用者靠顏色獲得的資訊 */
  spoken: string
  /** tooltip 文案。空串 = 不掛 tooltip */
  hint: string
}

/**
 * 測通了但很慢，結果不該報成一片綠色的「成功」。
 * 分檔標準見 gradeLatency：口述時超過 1 秒就能感覺到停頓，超過 2 秒基本沒法用來幹活。
 */
function latencyTone(ms: number): FeedbackTone {
  return gradeLatency(ms).tone === 'ok' ? 'success' : 'warning'
}

/** 一句話結論。慢到不該用的時候，第一句就得說出來，別讓使用者從數字裡自己算 */
function successMessage(model: string, ms: number): string {
  const grade = gradeLatency(ms)
  if (grade.tier === 'tooSlow') {
    return t('ai.msg.tooSlow', { model, latency: formatLatency(ms) })
  }
  if (grade.tier === 'slow') {
    return t('ai.msg.slow', { model, latency: formatLatency(ms) })
  }
  return t('ai.msg.ok', { model, latency: formatLatency(ms), grade: grade.label })
}

/**
 * 測試透過後的完整交代。
 *
 * 為什麼要這麼全：這行結果是使用者唯一能核對"我到底測的是哪一份配置"的地方。
 * 只報一個耗時的話，兩張卡都叫 gpt-4o-mini 時根本分不清剛才測的是哪個端點。
 * 排成一列「欄位：值」，不是寫句子——它是一份記錄，不是一段說明。
 */
function successDetail(profile: AiProfile, outcome: TestOutcome): string {
  const grade = gradeLatency(outcome.elapsedMs)
  return [
    t('ai.detail.provider', { value: providerLabel(profile.provider) }),
    t('ai.detail.model', { value: profile.model }),
    t('ai.detail.endpoint', { value: profile.apiUrl }),
    t('ai.detail.roundTrip', { latency: formatLatency(outcome.elapsedMs), grade: grade.label }),
    outcome.reply ? t('ai.detail.reply', { value: outcome.reply }) : '',
    t('ai.detail.time', { value: new Date().toLocaleTimeString(getLocale(), { hour12: false }) }),
  ].filter(Boolean).join('\n')
}

function renderStatusChip(status: CardStatus) {
  const chip = (
    <span
      className={cn(
        'inline-flex items-center rounded-md px-1.5 py-0.5 text-[11px] font-medium tabular-nums',
        status.box,
        status.hint && 'cursor-help',
      )}
    >
      {status.text}
    </span>
  )
  if (!status.hint) return <span className="shrink-0">{chip}</span>
  // 指標事件要單獨收回來：外面那層把點選讓給了鋪滿整卡的單選按鈕
  return (
    <Tooltip variant="light" className="pointer-events-auto relative z-10 shrink-0" content={status.hint}>
      {chip}
    </Tooltip>
  )
}

const NEUTRAL_BOX = 'bg-muted text-muted-foreground'

/**
 * 卡上那枚狀態標籤。
 *
 * 設計取捨：可用時標籤**只顯示耗時數字**（如「320ms」），快慢檔位完全靠顏色區分，
 * 不再把「很快 / 太慢」這些字塞進去——那幾個字會擠佔模型名的橫向空間，而模型名
 * 動輒二十多個字元，寧可把地方讓給它。檔位詞移進 tooltip；讀屏另有 spoken 完整念出。
 *
 * 另外兩條老規矩不變：
 *   - 顏色只在結論新鮮時才給。超過保鮮期（CHECK_FRESH_MS）一律收回中性灰——
 *     三天前測出的綠色和剛測的一樣，那是在替供應商做無限期擔保。
 *   - 紅色表示"別用這個"，所以「不可用」和「太慢」共用它——後者是能連上但等不起。
 */
function describeStatus(profile: AiProfile, checking: boolean): CardStatus {
  if (checking) {
    return { text: t('ai.status.testing'), box: 'bg-warning/10 text-warning-strong', spoken: t('ai.status.testing'), hint: t('ai.status.testingHint') }
  }

  const check = profile.check
  if (!check) {
    return isProfileComplete(profile)
      ? { text: t('ai.status.untested'), box: NEUTRAL_BOX, spoken: t('ai.status.untested'), hint: t('ai.status.untestedHint') }
      : { text: t('ai.status.incomplete'), box: NEUTRAL_BOX, spoken: t('ai.status.incomplete'), hint: t('ai.status.incompleteHint') }
  }

  const fresh = isCheckFresh(check)
  // 遷移來的老結論沒有時間戳，所以「什麼時候」這句要麼給準，要麼明說不知道
  const when = (verdict: string): string =>
    check.at
      ? t('ai.status.whenKnown', { when: formatCheckedAt(check.at), verdict })
      : t('ai.status.whenUnknown', { verdict })
  const staleNote = fresh ? '' : t('ai.status.staleNote')

  if (!check.ok) {
    // 失敗沒有數字可顯示，只能出詞
    return {
      text: t('ai.status.unavailable'),
      box: fresh ? 'bg-destructive/10 text-destructive-strong' : NEUTRAL_BOX,
      spoken: t('ai.status.unavailable'),
      hint: [when(t('ai.status.verdictFail')), check.reason, staleNote].filter(Boolean).join(' · '),
    }
  }

  const ms = check.latencyMs
  if (ms === undefined) {
    // 遷移來的老結論可能只知道"透過"、沒有耗時
    return {
      text: t('ai.status.available'),
      box: fresh ? 'bg-success/10 text-success-strong' : NEUTRAL_BOX,
      spoken: t('ai.status.available'),
      hint: [when(t('ai.status.verdictPass')), staleNote].filter(Boolean).join(' · '),
    }
  }

  const grade = gradeLatency(ms)
  const box = !fresh
    ? NEUTRAL_BOX
    : grade.tone === 'bad'
      ? 'bg-destructive/15 text-destructive-strong'
      : grade.tone === 'warn'
        ? 'bg-warning/10 text-warning-strong'
        : 'bg-success/10 text-success-strong'

  return {
    // 只放數字，檔位靠顏色 + tooltip
    text: formatLatency(ms),
    box,
    spoken: t('ai.status.availableSpoken', { grade: grade.label, latency: formatLatency(ms) }),
    hint: [
      when(t('ai.status.verdictPass')),
      grade.label,
      grade.tier === 'tooSlow' ? t('ai.status.tooSlowNote') : '',
      staleNote,
    ].filter(Boolean).join(' · '),
  }
}


// ⓘ 裡只放"選哪家"這一件事。"點一張即啟用"寫在卡頭副標題上，不重複說；
// 卡上標籤的含義由標籤自己和它的 tooltip 承擔。
const pickAdvice = () => t('ai.pickAdvice')

const inputClass = 'h-9 w-full rounded-md border border-input-border bg-input-bg px-3 text-sm transition-colors focus:border-input-focus-border'
const selectClass = 'h-9 w-full rounded-md border border-input-border bg-input-bg px-2 text-sm transition-colors focus:border-input-focus-border'
const linkClass = 'inline-flex items-center gap-1 text-xs text-primary underline underline-offset-2 decoration-primary/40 transition-colors hover:decoration-primary'
// [自訂] S58：本 fork 只用本機模式，不顯示雲端供應商控制台／申請文件連結（點擊會開外部瀏覽器）。
// 改回 true 即恢復上游行為。
const SHOW_CLOUD_CONSOLE_LINKS = false

const cardIconButtonClass = 'pointer-events-auto rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-40'
const helpIconClass = 'h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground'

export default function AIProviderSection() {
  useT()
  const [profiles, setProfiles] = useState<AiProfile[]>([])
  const [activeId, setActiveId] = useState('')
  const [loaded, setLoaded] = useState(false)
  /** 彈窗里正在編輯的那份（新建也走這裡）。null = 彈窗關著 */
  const [draft, setDraft] = useState<AiProfile | null>(null)
  const [draftIsNew, setDraftIsNew] = useState(false)
  /**
   * 彈窗裡待儲存的模型清單。
   *
   * 為什麼是一個清單而不是一個欄位：最常見的一次操作是"同一個供應商、同一個地址和金鑰，
   * 想再掛兩三個模型比一比"。此前那樣一次只能填一個，剩下的得重開彈窗、重粘金鑰。
   * 現在填幾個就存幾張卡，地址和金鑰共用。
   */
  const [draftModels, setDraftModels] = useState<string[]>([])
  const [modelInput, setModelInput] = useState('')
  /** 開啟彈窗時的快照，用來判斷「未儲存」 */
  const [draftBaseline, setDraftBaseline] = useState('')
  const [saving, setSaving] = useState(false)
  const [checkingId, setCheckingId] = useState('')
  const [pendingDeleteId, setPendingDeleteId] = useState('')
  const [notice, setNotice] = useState<Notice | null>(null)
  const [serverAiSource, setServerAiSource] = useState<ServerAiSource>('managed')
  const [savingServerAiSource, setSavingServerAiSource] = useState(false)
  const [serverAiSourceError, setServerAiSourceError] = useState(false)
  /** 工作模式與來源都讀回後才渲染那一段（見 useEffect 裡的註釋） */
  const [serverAiSourceLoaded, setServerAiSourceLoaded] = useState(false)
  const [isServerMode, setIsServerMode] = useState(false)
  const aiEnabled = useAiEnabled()
  /**
   * 下面這組服務此刻會不會被呼叫。不會的時候照樣能編輯、測速（伺服器模式下先調好再切過去是正常用法），
   * 但要讓人一眼看出「選中的那張現在沒在用」，否則打著勾的卡片會被當成正在整理文字的那個。
   * 來源沒讀回之前按「生效」畫，免得本地/雲模式的使用者看見標記閃一下。
   */
  const servicesInactiveReason: 'aiOff' | 'serverManaged' | null = !aiEnabled
    ? 'aiOff'
    : serverAiSourceLoaded && isServerMode && serverAiSource === 'managed'
      ? 'serverManaged'
      : null
  /** 「測試全部」的進度；null = 沒在批次測試 */
  const [batch, setBatch] = useState<{ done: number; total: number } | null>(null)
  /** 批次測試是併發的，所以「正在測哪個」是一組而不是一個 */
  const [checkingIds, setCheckingIds] = useState<string[]>([])

  const draftProvider = findProvider(draft?.provider ?? '')
  const draftNeedsKey = !draftProvider.keyless
  const draftUrlError = draft ? checkApiUrl(draft.apiUrl) : ''
  const draftKeyHint = draft && draftNeedsKey ? checkAiKeyFormat(draft.provider, draft.apiKey) : ''
  const draftSignature = draft ? draftSnapshot(draft, draftModels) : ''
  // 輸入框裡打了字但沒按回車也算改動——儲存時會自動收編它，所以現在就得算"未儲存"
  const draftDirty = draft !== null && (draftSignature !== draftBaseline || modelInput.trim() !== '')
  // 新建時金鑰是自動帶過來的（同供應商上一份），介面上要交代一句它從哪來。
  // 用"當前值是否還等於那份配置的金鑰"來判斷：使用者一改成別的，這句提示自己就消失
  const draftKeyInherited = draft !== null
    && draftIsNew
    && draft.apiKey.trim() !== ''
    && lastProfileOf(draft.provider)?.apiKey === draft.apiKey
  const busy = saving || checkingId !== '' || batch !== null
  /** 這個卡正在測嗎 —— 單個測試和併發批次測試都要算上 */
  const isChecking = (id: string) => checkingId === id || checkingIds.includes(id)
  const pendingDelete = profiles.find((p) => p.id === pendingDeleteId) ?? null

  useEffect(() => {
    let cancelled = false
    void (async () => {
      const state = await loadAiProfiles()
      setProfiles(state.profiles)
      setActiveId(state.activeId)
      setLoaded(true)
    })()
    // 兩個值一起等（Promise.all + 各自 catch 兜底），到齊後同一批渲染。
    // 分兩次落值會出兩種假象：預設是伺服器模式，本地/雲模式的使用者會看見這一段先出現再消失；
    // 來源預設 managed，已儲存 custom 的使用者會看見開關自己滑一下（pitfalls #11）。
    // 只在值到齊後才首次渲染，兩者都不存在，也就不需要 ready/animate 那套過渡開關。
    void Promise.all([
      getSetting('workMode', 'local').catch(() => 'local'),
      getSetting(SERVER_AI_SOURCE_KEY, 'managed').catch(() => 'managed'),
    ]).then(([mode, value]) => {
      if (cancelled) return
      const source = value === 'custom' ? 'custom' : 'managed'
      setServerAiSource(source)
      setRuntimeServerAiSource(source)
      setIsServerMode(mode === 'server')
      setServerAiSourceLoaded(true)
    })
    // 切走路由時復位「有未儲存改動」，別把髒狀態留給下一次進入
    return () => {
      cancelled = true
      setEngineDraftDirty(false)
    }
  }, [])

  useEffect(() => { setEngineDraftDirty(draftDirty) }, [draftDirty])

  async function handleServerAiSource(next: ServerAiSource) {
    if (savingServerAiSource || next === serverAiSource) return
    const previous = serverAiSource
    setServerAiSource(next)
    setRuntimeServerAiSource(next)
    setSavingServerAiSource(true)
    setServerAiSourceError(false)
    try {
      await setSetting(SERVER_AI_SOURCE_KEY, next)
    } catch {
      setServerAiSource(previous)
      setRuntimeServerAiSource(previous)
      setServerAiSourceError(true)
    } finally {
      setSavingServerAiSource(false)
    }
  }

  /** 唯一的寫入點：列表 + 啟用項一起落盤，並由 store 同步執行時那四個扁平鍵 */
  async function persist(nextProfiles: AiProfile[], nextActiveId: string) {
    const active = resolveActiveProfile(nextProfiles, nextActiveId)
    setProfiles(nextProfiles)
    setActiveId(active?.id ?? '')
    await saveAiProfiles({ profiles: nextProfiles, activeId: nextActiveId })
  }

  function openEditor(profile: AiProfile, isNew: boolean) {
    const models = profile.model.trim() ? [profile.model.trim()] : []
    setDraft(profile)
    setDraftIsNew(isNew)
    setDraftModels(models)
    setModelInput('')
    setDraftBaseline(draftSnapshot(profile, models))
    setNotice(null)
  }

  function closeEditor() {
    if (saving) return
    setDraft(null)
    setDraftIsNew(false)
    setDraftModels([])
    setModelInput('')
    setDraftBaseline('')
    setNotice(null)
    setEngineDraftDirty(false)
  }

  function addModel(name: string) {
    const value = name.trim()
    setModelInput('')
    setNotice(null)
    if (!value || draftModels.includes(value)) return
    setDraftModels([...draftModels, value])
  }

  function removeModel(name: string) {
    setDraftModels(draftModels.filter((m) => m !== name))
    setNotice(null)
  }

  function handleActivate(id: string) {
    if (id === activeId) return
    setNotice(null)
    void persist(profiles, id)
  }

  async function handleDelete(id: string) {
    const next = profiles.filter((p) => p.id !== id)
    setPendingDeleteId('')
    if (draft?.id === id) closeEditor()
    setNotice(null)
    // 刪掉的正是啟用項時，啟用第一條；一條不剩就寫空，下游自己跳過 AI 環節
    await persist(next, id === activeId ? '' : activeId)
  }

  /**
   * 換供應商時，地址與模型只在「還是上一家的樣板值」時才跟著換。
   * 使用者自己敲過的自定義端點或模型名不能被一次誤點的下拉抹掉。
   */
  function handleDraftProvider(value: string) {
    if (!draft) return
    const from = findProvider(draft.provider)
    const to = findProvider(value)
    const previous = lastProfileOf(value)
    // 地址：自己敲過的自定義端點不能被一次誤點的下拉抹掉，只有還是樣板值時才跟著換；
    // 換到一個已經配過的供應商，就用那份配過的地址，而不是清單裡的預設值
    const boilerplate = draft.apiUrl.trim() === '' || draft.apiUrl.trim() === from.defaultUrl
    const url = boilerplate ? previous?.apiUrl || to.defaultUrl : draft.apiUrl
    // 金鑰：空著才自動帶上同供應商已配過的那把，絕不覆蓋使用者已經填進去的
    const apiKey = draft.apiKey.trim() === '' ? previous?.apiKey ?? '' : draft.apiKey
    // 模型清單同理：還全是上一家的樣板值時才換成新家的推薦，使用者自己敲的一律留著
    const stillBoilerplate = draftModels.length === 0
      || draftModels.every((m) => from.defaultModels.includes(m))
    if (stillBoilerplate) {
      setDraftModels(to.defaultModels[0] ? [to.defaultModels[0]] : [])
    }
    setDraft({ ...draft, provider: value, apiUrl: url, apiKey, check: undefined })
    setNotice(null)
  }

  /** 改了任何一格，上一次的檢測結論就不再描述這份配置了，一併作廢（別讓卡上留著舊的「可用」） */
  function patchDraft(patch: Partial<AiProfile>) {
    if (!draft) return
    setDraft({ ...draft, ...patch, check: undefined })
    setNotice(null)
  }

  /** 同供應商最近配過的那份（用來給新草稿帶上地址和金鑰） */
  function lastProfileOf(providerValue: string): AiProfile | undefined {
    return [...profiles].reverse().find((p) => p.provider === providerValue && p.apiUrl.trim() !== '')
  }

  /**
   * 新建草稿：供應商預設跟當前啟用的那份一致，地址和金鑰沿用同供應商已配過的最近一份。
   *
   * 這是「另存為新的一份」那個按鈕的替代品——它想解決的是"同一個端點再加一個模型，
   * 不用重新粘一遍金鑰"，但一個叫「另存為」的連結擺在模型輸入框旁邊，誰也看不出這層意思。
   * 換成預設值來做：新建時該帶的東西自己就帶上了，使用者只需要改模型名。不需要多一個控制元件。
   */
  function makeDraft(providerValue?: string): AiProfile {
    const active = resolveActiveProfile(profiles, activeId)
    const target = providerValue ?? active?.provider ?? preferredAiProviderValue()
    const fresh = blankProfile(target)
    const previous = lastProfileOf(target)
    return previous
      ? { ...fresh, apiUrl: previous.apiUrl, apiKey: previous.apiKey }
      : fresh
  }

  async function runTest(profile: AiProfile): Promise<TestOutcome> {
    try {
      const result = await invoke<TestResult>('test_ai_connection', {
        config: {
          provider: profile.provider,
          api_url: profile.apiUrl,
          api_key: profile.apiKey,
          model: profile.model,
        },
      })
      if (result.ok) {
        return { ok: true, elapsedMs: result.elapsed_ms, reply: extractTestReply(result.detail), message: '' }
      }
      const friendly = describeProviderError(result.message)
      return { ok: false, elapsedMs: 0, reply: '', message: friendly.message, detail: friendly.detail }
    } catch (err) {
      const friendly = describeProviderError(err)
      return { ok: false, elapsedMs: 0, reply: '', message: friendly.message, detail: friendly.detail }
    }
  }

  /**
   * 儲存（可選擇順手測一遍）。
   *
   * 兩個按鈕共用這一段：「儲存」只落盤，「儲存並測試」落盤後逐個真調一次。
   * 此前只有「儲存並測試」一個入口——想改個地址、或者先把幾個模型填上待會兒再測，
   * 都被強迫付一次（計費的）呼叫。
   *
   * 落盤一定在測試之前：測失敗不該把使用者剛粘進來的金鑰弄丟。
   */
  async function handleSave(withTest: boolean) {
    if (!draft || busy) return

    // 輸入框裡打了字但沒按回車，儲存時自動收編——這是最容易發生的操作，別讓它白費
    const models = modelInput.trim() && !draftModels.includes(modelInput.trim())
      ? [...draftModels, modelInput.trim()]
      : draftModels

    const base = {
      provider: draft.provider,
      apiUrl: draft.apiUrl.trim() || (draftProvider.urlOptional ? draftProvider.defaultUrl : ''),
      apiKey: draft.apiKey.trim(),
    }
    const urlError = checkApiUrl(base.apiUrl)
    if (urlError) {
      setNotice({ tone: 'warning', scope: 'editor', message: urlError })
      return
    }
    if (!base.apiUrl) {
      setNotice({
        tone: 'warning',
        scope: 'editor',
        message: draftNeedsKey ? t('ai.err.apiUrlEmpty') : t('ai.err.ollamaUrlEmpty'),
      })
      return
    }

    // 第一個模型沿用當前這份的 id（編輯就是改它），其餘各自成為新的一張卡
    const targets: AiProfile[] = models.length === 0
      ? [{ ...draft, ...base, model: '' }]
      : models.map((model, index) => index === 0
        ? { ...draft, ...base, model }
        : { ...blankProfile(draft.provider), ...base, model })

    // 檢測結論只在"這份配置一個字都沒變"時才留著；改了任何一格，舊結論就不再描述它
    const withChecks = targets.map((target) => {
      const stored = profiles.find((p) => p.id === target.id)
      const unchanged = stored
        && stored.provider === target.provider
        && stored.apiUrl === target.apiUrl
        && stored.apiKey === target.apiKey
        && stored.model === target.model
      return { ...target, check: unchanged ? stored.check : undefined }
    })

    setSaving(true)
    setNotice(null)
    setModelInput('')
    setDraftModels(models)

    const existingIds = new Set(profiles.map((p) => p.id))
    let nextProfiles = profiles.map((p) => withChecks.find((t) => t.id === p.id) ?? p)
    nextProfiles = [...nextProfiles, ...withChecks.filter((t) => !existingIds.has(t.id))]
    // 新建的第一份儲存後即啟用（剛配完就是想用它）；編輯已有的不搶當前啟用項
    const nextActiveId = draftIsNew ? withChecks[0].id : activeId

    try {
      await persist(nextProfiles, nextActiveId)
    } catch (err) {
      setNotice({ tone: 'error', scope: 'editor', message: t('ai.err.saveFailed'), detail: String(err) })
      setSaving(false)
      return
    }

    setDraft(withChecks[0])
    setDraftIsNew(false)
    setDraftBaseline(draftSnapshot(withChecks[0], models))

    const created = withChecks.length
    // 多個模型一次存完，彈窗就沒有"正在編輯哪一份"了——關掉，結果落到網格下面
    const scope: Notice['scope'] = created > 1 ? 'list' : 'editor'
    const finish = (notice: Notice) => {
      if (created > 1) closeEditor()
      setNotice(notice)
      setSaving(false)
    }

    if (draftNeedsKey && !base.apiKey) {
      finish({ tone: 'warning', scope, message: t('ai.msg.savedNoKey', { count: created }) })
      return
    }
    if (models.length === 0) {
      finish({ tone: 'warning', scope: 'editor', message: t('ai.msg.savedNoModel') })
      return
    }
    if (!withTest) {
      finish({
        tone: 'success',
        scope,
        message: created > 1 ? t('ai.msg.savedCount', { count: created }) : t('ai.msg.savedOne'),
        detail: created > 1 ? withChecks.map((t) => t.model).join('\n') : undefined,
      })
      return
    }

    // 逐個測：結論一律寫回（通不過也要寫，卡上得顯示「不可用」而不是停在「未測試」）
    const results: Array<{ profile: AiProfile; outcome: TestOutcome }> = []
    let working = nextProfiles
    for (const target of withChecks) {
      const outcome = await runTest(target)
      const tested = { ...target, check: outcomeToCheck(outcome) }
      working = working.map((p) => (p.id === tested.id ? tested : p))
      await persist(working, nextActiveId)
      if (tested.id === withChecks[0].id) {
        setDraft(tested)
        setDraftBaseline(draftSnapshot(tested, models))
      }
      results.push({ profile: tested, outcome })
    }

    if (results.length === 1) {
      const { profile, outcome } = results[0]
      finish(outcome.ok
        ? {
          tone: latencyTone(outcome.elapsedMs),
          scope: 'editor',
          message: t('ai.msg.savedAndTested', { result: successMessage(profile.model, outcome.elapsedMs) }),
          detail: successDetail(profile, outcome),
        }
        : {
          tone: 'error',
          scope: 'editor',
          message: t('ai.msg.savedButFailed', { message: outcome.message }),
          detail: outcome.detail,
        })
      return
    }

    const okCount = results.filter((r) => r.outcome.ok).length
    finish({
      tone: okCount === results.length ? 'success' : okCount === 0 ? 'error' : 'warning',
      scope,
      message: t('ai.msg.savedBatch', { total: results.length, ok: okCount, fail: results.length - okCount }),
      detail: results
        .map(({ profile, outcome }) => outcome.ok
          ? t('ai.msg.batchItemOk', { model: profile.model, latency: formatLatency(outcome.elapsedMs), grade: gradeLatency(outcome.elapsedMs).label })
          : t('ai.msg.batchItemFail', { model: profile.model, message: outcome.message }))
        .join('\n'),
    })
  }

  /**
   * 卡上的「測試」：拿這份配置真的發一句話過去，看拿不拿得到回覆。
   *
   * 它回答的是"這條能不能用"（金鑰對不對、模型開通了沒、網路通不通），耗時是可用之後的
   * 第二個維度。原來這個按鈕叫「測速」，把結論說成了一個速度數字——使用者按下它時想問的
   * 其實是"這個模型好不好用"。
   */
  async function handleCheck(profile: AiProfile) {
    if (busy) return
    const scope: Notice['scope'] = draft ? 'editor' : 'list'
    if (!isProfileComplete(profile)) {
      setNotice({
        tone: 'warning',
        scope,
        message: t('ai.msg.incompleteCard', { title: profileTitle(profile) }),
      })
      return
    }

    setCheckingId(profile.id)
    setNotice(null)
    const outcome = await runTest(profile)
    await persist(
      profiles.map((p) => (p.id === profile.id ? { ...p, check: outcomeToCheck(outcome) } : p)),
      activeId,
    )
    if (outcome.ok) {
      setNotice({
        tone: latencyTone(outcome.elapsedMs),
        scope,
        message: successMessage(profile.model, outcome.elapsedMs),
        detail: successDetail(profile, outcome),
      })
    } else {
      setNotice({
        tone: 'error',
        scope,
        message: t('ai.msg.modelFailed', { model: profile.model, message: outcome.message }),
        detail: outcome.detail,
      })
    }
    setCheckingId('')
  }

  /**
   * 一鍵測試全部：**併發**真發一句話過去，一次點選直接開始（沒有二次確認）。
   *
   * ⚠️ 併發的代價，改回序列前先想清楚：卡上的耗時是用來橫向比較「哪個更快」的，
   * 併發跑出來的數字互相汙染 —— 同一個地址和金鑰下掛著好幾個模型時尤其明顯，
   * 還可能撞上限流被記成「不可用」。所以批次結果裡會註明這批耗時不宜橫向比較；
   * 要拿準數字，用卡上的單張測試。
   *
   * 另外兩個決定：
   *  · **一次性落盤**，不逐張寫。併發下每個回撥拿到的都是同一份舊 profiles，
   *    逐張 persist 會互相覆蓋，只剩最後一個的結論。
   *  · **跳過沒填完的**，不給它們寫「不可用」——它們缺的是配置而不是可用性，
   *    寫進結論只會汙染卡片狀態；跳過幾個會在結果裡說明。
   */
  async function handleCheckAll() {
    if (busy) return
    const targets = profiles.filter(isProfileComplete)
    const skipped = profiles.length - targets.length
    if (targets.length === 0) {
      setNotice({
        tone: 'warning',
        scope: 'list',
        message: t('ai.msg.nothingToTest'),
      })
      return
    }

    setNotice(null)
    setBatch({ done: 0, total: targets.length })
    setCheckingIds(targets.map((p) => p.id))

    let done = 0
    const results = await Promise.all(targets.map(async (target) => {
      const outcome = await runTest(target)
      // 每個自己測完就把圖示停下、進度加一，不用等整批結束
      done += 1
      setBatch({ done, total: targets.length })
      setCheckingIds((prev) => prev.filter((id) => id !== target.id))
      return { target, outcome }
    }))

    const checks = new Map(results.map((r) => [r.target.id, outcomeToCheck(r.outcome)]))
    await persist(
      profiles.map((p) => {
        const check = checks.get(p.id)
        return check ? { ...p, check } : p
      }),
      activeId,
    )
    setCheckingIds([])
    setBatch(null)

    let okCount = 0
    let failCount = 0
    const lines = results.map(({ target, outcome }) => {
      if (outcome.ok) {
        okCount += 1
        return t('ai.msg.batchLineOk', { model: target.model, latency: formatLatency(outcome.elapsedMs) })
      }
      failCount += 1
      return t('ai.msg.batchLineFail', { model: target.model, message: outcome.message })
    })

    const parts = [t('ai.msg.batchOk', { count: okCount })]
    if (failCount > 0) parts.push(t('ai.msg.batchFail', { count: failCount }))
    if (skipped > 0) parts.push(t('ai.msg.batchSkipped', { count: skipped }))
    if (targets.length > 1) {
      lines.push('', t('ai.msg.batchNote'))
    }
    setNotice({
      tone: failCount > 0 ? 'warning' : 'success',
      scope: 'list',
      message: t('ai.msg.batchDone', { parts: parts.join(t('asr.listSeparator')) }),
      detail: lines.join('\n'),
    })
  }

  /**
   * 一張卡就兩行，約 70px：
   *   第一行  ✓ 模型名 ……… 狀態標籤（可用 1.2s / 不可用 / 未測試）
   *   第二行  供應商 · 主機名 ……… 測試 / 編輯 / 刪除（劃過才顯形）
   *
   * 兩個顏色的分工是刻意的，也是上一版最誤導的地方：
   *   - 對勾是**主題色**，只表示"這份正在被使用"（和工作模式卡一致）；
   *   - **綠色只屬於狀態標籤**，只有真的測透過才會出現。
   * 上一版把對勾塗成綠的，等於一選中就宣佈"能用"——而它可能金鑰都沒填對。
   */
  function renderCard(profile: AiProfile) {
    const isActive = profile.id === activeId
    const checking = isChecking(profile.id)
    const status = describeStatus(profile, checking)

    return (
      <div
        key={profile.id}
        className={cn(
          'group relative rounded-lg border p-2.5 transition-colors',
          isActive ? 'border-primary bg-primary/5' : 'border-border hover:bg-accent/50',
        )}
      >
        {/* 鋪滿整卡的單選按鈕：卡上還有自己的圖示按鈕，所以不能把整張卡做成 <button>
            （按鈕套按鈕，讀屏和鍵盤都會錯）。文字是靜態的，點哪兒都落到這個按鈕上。 */}
        <button
          type="button"
          role="radio"
          aria-checked={isActive}
          aria-label={t('common.cardStatusAria', {
            title: profileTitle(profile),
            subtitle: profileSubtitle(profile),
            status: status.spoken,
          })}
          onClick={() => handleActivate(profile.id)}
          className="absolute inset-0 rounded-lg"
        />

        <div className="flex items-center gap-1.5">
          {/* 「使用中」用實心綠勾，和診斷頁表示"正常"的 CheckCircle2 是同一個圖示。
              它和右邊的狀態標籤分工不同：勾說的是"這份在被用"，標籤說的是"上次測得如何"，
              所以勾旁邊掛一個 tooltip 明說，避免又被讀成"能用" */}
          {isActive && (
            <Tooltip className="pointer-events-auto relative z-10 shrink-0" content={t('common.inUse')}>
              <CheckCircle2 className="h-4 w-4 shrink-0 cursor-help text-success-strong" aria-label={t('common.inUse')} />
            </Tooltip>
          )}
          {/* 字號壓到 12px：模型名動輒 20 多個字元（doubao-seed-2-0-lite-260215），
              三列排佈下 14px 裝不住 */}
          <span className="min-w-0 flex-1 truncate text-xs font-medium" title={profileTitle(profile)}>
            {profileTitle(profile)}
          </span>
          {renderStatusChip(status)}
        </div>

        <div className="mt-1 flex items-center gap-1.5">
          {/* 供應商 · 主機名合成一行；主機名只在不是該供應商預設地址時才出現
              （兩張都叫「OpenAI 相容」的卡靠它區分）。完整地址懸停可見，也在編輯彈窗裡 */}
          <span
            className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground"
            title={profile.apiUrl}
          >
            {profileSubtitle(profile)}
          </span>
          <div className="pointer-events-none relative z-10 flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
            {/* 圖示 tooltip 一律是"這個按鈕叫什麼"，兩三個字（對齊診斷頁的「重新整理狀態」等）。
                「會真實呼叫一次、按供應商計費」這種前提不屬於 tooltip：它太長，
                而且使用者按下按鈕的那一刻已經知道自己在測一個雲服務了 */}
            <Tooltip className="pointer-events-auto" content={t('common.test')}>
              <button
                type="button"
                onClick={() => void handleCheck(profile)}
                disabled={busy}
                aria-label={t('ai.testAria', { title: profileTitle(profile) })}
                className={cardIconButtonClass}
              >
                <RefreshCw className={cn('h-3.5 w-3.5', checking && 'animate-spin')} aria-hidden />
              </button>
            </Tooltip>
            <Tooltip className="pointer-events-auto" content={t('common.edit')}>
              <button
                type="button"
                onClick={() => openEditor({ ...profile }, false)}
                aria-label={t('ai.editAria', { title: profileTitle(profile) })}
                className={cardIconButtonClass}
              >
                <Pencil className="h-3.5 w-3.5" aria-hidden />
              </button>
            </Tooltip>
            <Tooltip className="pointer-events-auto" content={t('common.delete')}>
              <button
                type="button"
                onClick={() => setPendingDeleteId(profile.id)}
                aria-label={t('ai.deleteAria', { title: profileTitle(profile) })}
                className={cardIconButtonClass}
              >
                <Trash2 className="h-3.5 w-3.5" aria-hidden />
              </button>
            </Tooltip>
          </div>
        </div>
      </div>
    )
  }

  return (
    <Card>
      <CardContent className="p-6">
        {/* 只在伺服器模式出現：本地/雲 API 模式下 AI 整理本來就走下面選中的服務，沒有第二個
            選項可選，擺一個恆定生效的開關只會讓人以為它還管著別的事。
            形態用設定頁裡那 8 處同構的開關行 —— 分段控制元件在本應用表示頁面/短列舉切換，
            而兩個短選項撐成等寬卡會留一大片空白，還會和下方真正的服務卡爭層級。
            說明文字跟著開關狀態走，直接說“當前由誰整理”，比讓使用者從「關」反推更省事。 */}
        {serverAiSourceLoaded && isServerMode && (
          <section className="mb-4 border-b border-border pb-4" aria-labelledby="server-ai-source-heading">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  {/* 字號與圖示間距都對齊下面的「AI 服務」標題：兩者是同一層級的兩塊內容，
                      標題小一號會讓它看著像附屬於下面那塊 */}
                  <h2 id="server-ai-source-heading" className="text-lg font-semibold">
                    {t('ai.serverSource.title')}
                  </h2>
                  <Tooltip variant="light" content={t('ai.serverSource.help')}>
                    <Info
                      aria-label={t('settings.helpAria', { label: t('ai.serverSource.title') })}
                      className={helpIconClass}
                    />
                  </Tooltip>
                </div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {t(serverAiSource === 'custom' ? 'ai.serverSource.descCustom' : 'ai.serverSource.descManaged')}
                </p>
              </div>
              <Switch
                checked={serverAiSource === 'custom'}
                onChange={() => void handleServerAiSource(serverAiSource === 'custom' ? 'managed' : 'custom')}
                labelledBy="server-ai-source-heading"
                disabled={savingServerAiSource}
              />
            </div>

            {serverAiSourceError && (
              <Feedback className="mt-2" tone="error" message={t('ai.err.saveFailed')} />
            )}
          </section>
        )}

        <div className="mb-3 flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 grow basis-[18rem]">
            <div className="flex items-center gap-2">
              <h2 id="ai-service-heading" className="text-lg font-semibold">{t('ai.title')}</h2>
              <Tooltip variant="light" content={pickAdvice()}>
                <Info aria-label={t('ai.helpAria')} className={helpIconClass} />
              </Tooltip>
              {servicesInactiveReason && (
                <span className="rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">
                  {t('ai.inactiveBadge')}
                </span>
              )}
            </div>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {servicesInactiveReason === 'aiOff'
                ? t('ai.inactiveAiOff')
                : servicesInactiveReason === 'serverManaged'
                  ? t('ai.inactiveServerManaged')
                  : t('ai.desc')}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {/* 併發跑，所以沒有「停止」：invoke 發出去的請求沒法撤回，
                擺一顆停不掉任何東西的按鈕比沒有更糟。整批只等最慢那個，很快。 */}
            <Button
              variant="outline"
              size="sm"
              className="h-8 shrink-0"
              onClick={() => void handleCheckAll()}
              disabled={busy || profiles.length === 0}
            >
              <RefreshCw className={cn('mr-1 h-3.5 w-3.5', batch && 'animate-spin')} aria-hidden />
              {batch ? t('ai.testingBatch', { done: batch.done, total: batch.total }) : t('ai.testAll')}
            </Button>
            {/* 「新建」回到卡頭右上角（和「潤色模式」頁同一個位置）。
                它開的是彈窗，所以不會再出現"點了按鈕、變化發生在螢幕外"的問題 */}
            <Button
              variant="outline"
              size="sm"
              className="h-8 shrink-0"
              onClick={() => openEditor(makeDraft(), true)}
            >
              <Plus className="mr-1 h-3.5 w-3.5" aria-hidden />
              {t('common.new')}
            </Button>
          </div>
        </div>

        {/* 自定義服務始終可編輯和測試；伺服器模式是否實際使用由上方來源選擇決定。 */}
        <fieldset className="min-w-0">
          {!loaded ? (
            <p className="py-2 text-sm text-muted-foreground">{t('ai.loading')}</p>
          ) : profiles.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border px-4 py-4 text-center text-sm text-muted-foreground">
              {t('ai.empty')}
            </p>
          ) : (
            <div
              role="radiogroup"
              aria-labelledby="ai-service-heading"
              // 未生效時整組調淡；滑鼠移上去或鍵盤聚焦時恢復，表示仍然可以編輯和測試
              className={cn(
                'grid gap-2.5 sm:grid-cols-3',
                servicesInactiveReason && 'opacity-60 transition-opacity focus-within:opacity-100 hover:opacity-100',
              )}
            >
              {profiles.map(renderCard)}
            </div>
          )}

          {/* 卡上動作（測速）的結果沒有彈窗可歸，落在網格下方 */}
          {notice?.scope === 'list' && (
            <Feedback className="mt-3" tone={notice.tone} message={notice.message} detail={notice.detail} />
          )}
        </fieldset>
      </CardContent>

      {draft && (
        <Modal
          title={draftIsNew ? t('ai.editorNew') : t('ai.editorEdit')}
          onClose={closeEditor}
          locked={saving}
          showCloseButton
          panelClassName="w-[520px]"
        >
          <div className="mt-4 space-y-3">
            <div>
              <label htmlFor="ai-provider" className="mb-1 block text-sm text-muted-foreground">{t('ai.provider')}</label>
              <select
                id="ai-provider"
                value={draft.provider}
                onChange={(e) => handleDraftProvider(e.target.value)}
                className={selectClass}
              >
                {aiProvidersForDisplay().map((p) => (
                  <option key={p.value} value={p.value}>{p.label}</option>
                ))}
              </select>
            </div>

            <div>
              <label htmlFor="ai-api-url" className="mb-1 block text-sm text-muted-foreground">
                {draftProvider.urlOptional ? t('ai.apiUrlOptional') : draftProvider.keyless ? t('ai.ollamaUrl') : t('ai.apiUrl')}
              </label>
              <input
                id="ai-api-url"
                type="url"
                inputMode="url"
                value={draft.apiUrl}
                onChange={(e) => patchDraft({ apiUrl: e.target.value })}
                onKeyDown={(e) => { if (e.key === 'Enter') void handleSave(true) }}
                placeholder={draftProvider.defaultUrl}
                aria-describedby={draftProvider.urlOptional ? 'ai-api-url-hint' : undefined}
                className={inputClass}
              />
              {draftProvider.urlOptional && (
                <p id="ai-api-url-hint" className="mt-1 text-xs text-muted-foreground">{t('ai.apiUrlOptionalHint')}</p>
              )}
              {draftUrlError && <FormatHint text={draftUrlError} />}
            </div>

            {draftNeedsKey && (
              // data-modal-autofocus：彈窗開啟時焦點落在金鑰上（這才是要乾的事），
              // 而不是第一個可聚焦元素——那是右上角的關閉按鈕
              <div data-modal-autofocus>
                <label htmlFor="ai-api-key" className="mb-1 block text-sm text-muted-foreground">API Key</label>
                <PasswordInput
                  id="ai-api-key"
                  label="API Key"
                  value={draft.apiKey}
                  onChange={(v) => patchDraft({ apiKey: v })}
                  onSubmit={() => void handleSave(true)}
                  placeholder={t('ai.keyPlaceholder')}
                  className={inputClass}
                />
                {!notice && draftKeyHint && <FormatHint text={draftKeyHint} />}
                {/* 金鑰不是憑空出現的：說一句它從哪來，免得使用者以為自己看錯了 */}
                {draftKeyInherited && (
                  <p className="mt-1 text-[11px] text-muted-foreground">
                    {t('ai.keyReused', { provider: draftProvider.label })}
                  </p>
                )}
                {/* 金鑰入口放在金鑰這一格里。
                    這裡原來還有一個「怎麼申請金鑰？看配置文件」的連結，去掉了：AI 服務
                    要做的只有「去控制台複製一把 Key」，上面那個控制台連結就是最短路徑，
                    再掛一篇文件反而暗示這件事很複雜。
                    （語音識別那邊保留了，但只對豆包顯示 —— 只有它要在新舊兩代控制台之間選。）*/}
                <div className="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1">
                  {SHOW_CLOUD_CONSOLE_LINKS && draftProvider.consoleUrl && (
                    <button
                      type="button"
                      onClick={() => void shellOpen(draftProvider.consoleUrl as string)}
                      className={linkClass}
                    >
                      {t('ai.openConsole', { provider: draftProvider.label })}
                      <ExternalLink className="h-3 w-3" aria-hidden />
                    </button>
                  )}
                </div>
              </div>
            )}

            <div>
              <label htmlFor="ai-model" className="mb-1 block text-sm text-muted-foreground">{t('ai.model')}</label>
              {/* 原來這裡是 <input list=...>：Chromium 會畫一個下拉箭頭，但它其實是個自由文本框，
                  "又是下拉又要我填"兩頭都不像。現在拆開——輸入框只管輸入，推薦名做成可點的小按鈕 */}
              <div className="flex items-center gap-2">
                <input
                  id="ai-model"
                  value={modelInput}
                  onChange={(e) => { setModelInput(e.target.value); setNotice(null) }}
                  onKeyDown={(e) => {
                    if (e.key !== 'Enter') return
                    e.preventDefault()
                    addModel(modelInput)
                  }}
                  placeholder={draftProvider.defaultModels[0] ?? t('ai.modelPlaceholder')}
                  className={cn(inputClass, 'flex-1')}
                />
                <Button
                  variant="outline"
                  size="sm"
                  className="h-9 shrink-0"
                  onClick={() => addModel(modelInput)}
                  disabled={!modelInput.trim()}
                >
                  {t('ai.addAnother')}
                </Button>
              </div>

              {draftModels.length > 0 && (
                <div className="mt-2 flex flex-wrap gap-1.5">
                  {draftModels.map((model) => (
                    <span
                      key={model}
                      className="inline-flex items-center gap-1 rounded-md border border-border bg-secondary/50 py-0.5 pl-2 pr-1 text-xs"
                    >
                      {model}
                      <button
                        type="button"
                        onClick={() => removeModel(model)}
                        aria-label={t('ai.removeModel', { model })}
                        className="rounded p-0.5 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive-strong"
                      >
                        <X className="h-3 w-3" aria-hidden />
                      </button>
                    </span>
                  ))}
                </div>
              )}

              {/* 這裡原來還有一排「推薦模型」小按鈕，去掉了：新建時該供應商的推薦模型
                  已經預先躺在上面的清單裡，輸入框的 placeholder 也寫著它，
                  再擺一排按鈕是把同一件事說第三遍。 */}
              <p className="mt-1.5 text-[11px] text-muted-foreground">
                {t('ai.multiModelHint')}
              </p>
            </div>

            {notice?.scope === 'editor' && (
              <Feedback tone={notice.tone} message={notice.message} detail={notice.detail} />
            )}

            {/* 只留一個主按鈕。關閉走右上角那個叉（Esc、點背板也行），
                不用再擺一個和它同義的「取消」 */}
            {/* 兩個入口：「儲存」只落盤，「儲存並測試」額外真調一次。
                此前只有後者——想改個地址、或者先把幾個模型填上待會兒再測，
                都被強迫付一次計費呼叫。關閉走右上角那個叉（Esc、點背板也行）。 */}
            <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border pt-3">
              <Button
                variant="outline"
                size="sm"
                className="h-9"
                onClick={() => void handleSave(false)}
                disabled={busy}
              >
                {t('common.save')}
              </Button>
              <Button size="sm" className="h-9" onClick={() => void handleSave(true)} disabled={busy}>
                {saving ? t('common.processing') : draftModels.length > 1 ? t('ai.saveAndTestCount', { count: draftModels.length }) : t('ai.saveAndTest')}
              </Button>
            </div>
          </div>
        </Modal>
      )}

      {pendingDelete && (
        <Modal title={t('ai.deleteTitle')} onClose={() => setPendingDeleteId('')} panelClassName="w-[420px]">
          <p className="mt-3 text-sm leading-relaxed text-muted-foreground">
            {profileTitle(pendingDelete)} ({profileSubtitle(pendingDelete)}). {t('ai.deleteBody')}
          </p>
          <div className="mt-4 flex items-center justify-end gap-2">
            <Button variant="outline" size="sm" className="h-9" onClick={() => setPendingDeleteId('')}>
              {t('common.cancel')}
            </Button>
            <button
              type="button"
              onClick={() => void handleDelete(pendingDelete.id)}
              className="h-9 rounded-md border border-destructive/30 bg-destructive/5 px-3 text-sm font-medium text-destructive-strong transition-colors hover:bg-destructive/10"
            >
              {t('common.delete')}
            </button>
          </div>
        </Modal>
      )}
    </Card>
  )
}
