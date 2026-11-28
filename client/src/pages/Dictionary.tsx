import { Fragment, useEffect, useState } from 'react'
import { Plus, X, Search, RotateCcw, ChevronDown, ChevronUp, FolderPlus, Trash2, Download, Info } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Modal } from '@/components/ui/modal'
import { Switch } from '@/components/ui/switch'
import { Tooltip } from '@/components/ui/tooltip'
import {
  ASR_PLATFORMS,
  ASR_PROVIDERS,
  asrModelsOf,
  type AsrPlatform,
} from '@/features/settings/asrProviderCatalog'
import {
  expectedClientCap,
  expectedHotwordDelivery,
  foldHotwordDelivery,
  hotwordDependsOnStreamingPath,
  hotwordUndecidedReason,
  type HotwordUiState,
  type HotwordUndecidedReason,
} from '@/lib/asrModels'
import { cn } from '@/lib/utils'
import * as bridge from '@/services/bridge'
import { describeExportError, exportHotwords } from '@/services/exports'
import { getSetting } from '@/services/store'
import { BUILTIN_SETS, MAX_HOTWORDS } from '@/services/hotwords/model'
import { builtinInputKey, useHotwordsManager } from '@/services/hotwords/useHotwordsManager'
import TextReplacementSection from '@/components/TextReplacementSection'
import TextFormatSection from '@/components/TextFormatSection'
import { useSortable, DragHandle } from '@/components/ui/sortable'
import { t, type TranslationKey } from '@/i18n'
import { RichText } from '@/i18n/RichText'
import { useT } from '@/i18n/useT'

type Tab = 'hotwords' | 'replacement'

/** 超過該數量的熱詞總數時，給出"過多可能反而降低準確率"的軟提示 */
const HOTWORD_SOFT_LIMIT = 200
/** 單個分類內詞條超過該數量時摺疊，只顯示前 N 個 + 展開按鈕，避免一大片平鋪 */
const CHIPS_COLLAPSE_LIMIT = 30

/** 詞條標籤列表：數量多時摺疊（搜尋中不折疊，避免藏起匹配項）。 */
function WordChips({
  words,
  onRemove,
  expandAll = false,
}: {
  words: string[]
  onRemove: (word: string) => void
  expandAll?: boolean
}) {
  const [expanded, setExpanded] = useState(false)
  const overflow = words.length > CHIPS_COLLAPSE_LIMIT
  const shown = expanded || expandAll || !overflow ? words : words.slice(0, CHIPS_COLLAPSE_LIMIT)

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-1.5">
        {shown.map((word) => (
          <span
            key={word}
            className="inline-flex items-center gap-1 rounded-md border bg-secondary/50 px-2 py-0.5 text-xs"
          >
            {word}
            <button
              onClick={() => onRemove(word)}
              className="rounded-full p-0.5 transition-colors hover:bg-destructive/10 hover:text-destructive"
              aria-label={t('dict.deleteWord', { word })}
            >
              <X className="h-2.5 w-2.5 text-muted-foreground" />
            </button>
          </span>
        ))}
      </div>
      {overflow && !expandAll && (
        <button
          type="button"
          onClick={() => setExpanded(!expanded)}
          className="text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          {expanded ? t('dict.collapse') : t('dict.expandAll', { count: words.length })}
        </button>
      )}
    </div>
  )
}

/**
 * 那句話該說哪一條。
 *
 * 「未確定」有三種成因，**展示狀態可以共用，說明不能共用**。原來三種都套同一句
 * 「等第一次識別或測試連線之後才能確定」，而那句只對「auto 協議還沒探測」成立：
 * 宣告缺失是 SayIt 的 bug，查詢失敗是命令沒調通，這兩種再錄一百次也不會變 ——
 * 讓使用者反覆口述去等一個永遠不來的結果，是把可指認的故障說成正常等待。
 */
function deliveryMessageKey(
  ui: HotwordUiState,
  reason: HotwordUndecidedReason | null,
): TranslationKey {
  if (ui === 'sent') return 'dict.delivery.sent'
  if (ui === 'not_sent') return 'dict.delivery.notSent'
  switch (reason) {
    // Rust 返回 unknown_provider：我們漏了宣告，不是使用者配置的問題
    case 'declaration_missing': return 'dict.delivery.undecidedDeclaration'
    // 命令沒調通（舊版本二進位制沒有這個 command、IPC 異常）
    case 'query_failed': return 'dict.delivery.undecidedQueryFailed'
    // auto 協議尚未探測 —— 只有這一種能靠"再用一次"解決
    default: return 'dict.delivery.undecided'
  }
}

/**
 * 當前這套配置拿熱詞做什麼。
 *
 * 為什麼不是一張「各模型對熱詞的支援」表：那張表（0.1.1 加的）硬編碼 7 行、永遠顯示
 * 同一份內容，漏了 Groq / OpenAI / OpenAI 相容 / OpenRouter / MiMo / Gemini 六家，
 * 還把「不支援」限定成本地模型的問題 —— 於是用 FunASR 的人看到的是一張"雲端全是支援"
 * 的表，而他的熱詞一條都沒進請求（issue #67）。
 *
 * 判據全部來自執行時：工作模式 + 當前 provider + 即時字幕開關，雲端那檔直接問 Rust
 * （providers/capabilities.rs 是唯一宣告處）。前端不再自己存一份清單。
 */
function HotwordDeliveryNotice({ hotwordCount }: { hotwordCount: number }) {
  const [tableOpen, setTableOpen] = useState(false)
  const [state, setState] = useState<{
    ui: HotwordUiState
    /** 走流式還是回落會改變結論 —— 為真時要說明"取決於即時字幕這次能不能連上" */
    pathDependent: boolean
    /** SayIt 自設的傳送條數上限，null = 不截斷 */
    clientCap: number | null
    /** 雲端模式才有轉寫後的空格還原 */
    hasSpacingRestore: boolean
    /** ui 為 'undecided' 時，究竟卡在哪一種未確定上（三種的處置辦法完全不同） */
    undecidedReason: HotwordUndecidedReason | null
  } | null>(null)

  useEffect(() => {
    let disposed = false

    const load = async () => {
      const mode = await getSetting('workMode', 'server') as string

      // 本地引擎：一條都不傳。models/ 下沒有任何熱詞通道 —— local_transcribe 的引數
      // 裡沒有熱詞那一項（只有音訊、模型、語種和計算裝置那幾個），GGML 遷移時按
      // dev-docs 的 D3 放棄了熱詞（舊的 sherpa-onnx 時代確實寫進 recognizer 配置，
      // 那個實現已經不在了）。
      if (mode === 'local') {
        if (!disposed) {
          setState({
            ui: 'not_sent',
            pathDependent: false,
            clientCap: null,
            hasSpacingRestore: false,
            undecidedReason: null,
          })
        }
        return
      }
      // 伺服器模式：熱詞隨 start 訊息發給服務端（ServerProvider 的 start payload）。
      if (mode === 'server') {
        if (!disposed) {
          setState({
            ui: 'sent',
            pathDependent: false,
            clientCap: null,
            hasSpacingRestore: false,
            undecidedReason: null,
          })
        }
        return
      }

      const [provider, protocol, workspaceId, streamingOn] = await Promise.all([
        getSetting('cloudAsr.provider', '') as Promise<string>,
        getSetting('cloudAsr.protocol', 'auto') as Promise<string>,
        getSetting('cloudAsr.qwen.workspaceId', '') as Promise<string>,
        getSetting('streamingDisplayEnabled', false) as Promise<boolean>,
      ])
      // baseUrl + model 要一起給：auto 協議的探測結果按「地址 + 模型」快取，
      // 少給一個就查不到那條快取，會把已經探明的配置顯示成"未確定"。
      const [baseUrl, model] = await Promise.all([
        getSetting('cloudAsr.baseUrl', '') as Promise<string>,
        getSetting('cloudAsr.model', '') as Promise<string>,
      ])
      const capability = await bridge.asrHotwordCapability(provider, {
        ...(model ? { model } : {}),
        ...(baseUrl ? { baseUrl } : {}),
        ...(protocol && protocol !== 'auto' ? { protocol } : {}),
      })
      if (disposed) return
      if (!capability) {
        // 查不到（老版本 Rust、IPC 失敗）不能編一個答案，顯示"未確定"。
        setState({
          ui: 'undecided',
          pathDependent: false,
          clientCap: null,
          hasSpacingRestore: true,
          // 和"協議還沒探測"共用展示狀態，但原因必須留住：那句"第一次識別之後就能
          // 確定"對這一種不成立 —— 命令根本沒調通，再錄一百次也不會變。
          undecidedReason: 'query_failed',
        })
        return
      }
      const pathOpts = {
        streamingDisplayEnabled: Boolean(streamingOn),
        provider,
        qwenWorkspaceId: workspaceId,
      }
      const delivery = expectedHotwordDelivery(capability, pathOpts)
      setState({
        ui: foldHotwordDelivery(delivery),
        pathDependent: hotwordDependsOnStreamingPath(capability),
        // 取**這條路徑**上的上限，不是這家的概括。兩者的區別在 OpenAI live 上會顯形：
        // 關掉即時字幕走檔案端點，熱詞一條都不發，這時報上限就是在自相矛盾。
        clientCap: expectedClientCap(capability, pathOpts),
        hasSpacingRestore: true,
        undecidedReason: hotwordUndecidedReason(delivery),
      })
    }

    void load()
    // 「OpenAI 相容」的 auto 協議要到第一次轉寫才探測出來（探測發生在 cloud_transcribe
    // 內部）。不重查的話，熱詞頁一直開著的使用者會永遠看到"還沒探測出來"——
    // 而答案在他第一次口述之後就已經有了。
    const onCapabilityMaybeChanged = () => { void load() }
    window.addEventListener(bridge.ASR_CAPABILITY_MAYBE_CHANGED_EVENT, onCapabilityMaybeChanged)
    return () => {
      disposed = true
      window.removeEventListener(bridge.ASR_CAPABILITY_MAYBE_CHANGED_EVENT, onCapabilityMaybeChanged)
    }
  }, [])

  if (!state) return null

  const tone = state.ui === 'sent'
    ? 'text-muted-foreground'
    : state.ui === 'undecided'
      ? 'text-muted-foreground'
      : 'text-amber-500'

  return (
    <div className={cn('mb-4 -mt-1 space-y-1 text-xs leading-relaxed', tone)}>
      {/* 完整對照的入口跟在結論後面，同一行 —— 使用者讀完"當前這套會不會傳送"，
          緊接著最想問的就是"那別的呢"。
          原來這裡寫的是「完整對照見上游的語音辨識設定文件」，那句話沒有落點：
          文件在倉庫裡，使用者在 App 內點不到。[自訂] 該文件已自本 fork 移除。
          現在改成應用內開啟，表格現問 Rust 現渲染（見 HotwordSupportTable）。 */}
      <p>
        <RichText text={t(deliveryMessageKey(state.ui, state.undecidedReason))} />
        <button
          type="button"
          onClick={() => setTableOpen(true)}
          className="ml-1.5 text-muted-foreground underline decoration-dotted underline-offset-2 transition-colors hover:text-foreground"
        >
          {t('dict.delivery.openTable')}
        </button>
      </p>
      {/* 回落會翻轉結論的那兩家（OpenAI live / Gemini live）必須說明，否則使用者看到的
          結論會在建連失敗那一次悄悄失效，而他不會知道。 */}
      {state.pathDependent && (
        <p><RichText text={t('dict.delivery.pathDependent')} /></p>
      )}
      {/* 客戶端截斷：說清是 SayIt 傳送時的上限，不是服務端限制。
          clientCap 已經是**當前路徑**上那份（見 expectedClientCap）—— 按 provider 取
          一個數會讓 OpenAI live 關掉字幕時同時顯示"不傳送"和"最多發 100 個"。 */}
      {state.clientCap !== null && hotwordCount > state.clientCap && (
        <p className="text-amber-500">
          <RichText text={t('dict.delivery.clientCap', { cap: state.clientCap, count: hotwordCount })} />
        </p>
      )}
      {/* 不進 ASR 時給替代路徑，但必須說清它要兩個開關都開著 —— 「熱詞注入提示詞」
          預設關閉，且依賴 AI 整理（AI 整理本身還可能因短錄音跳過或呼叫失敗）。
          不能讓使用者以為有自動兜底。 */}
      {state.ui === 'not_sent' && (
        <p><RichText text={t('dict.delivery.fallbackHint')} /></p>
      )}
      {/* 雲端這條路熱詞還有一個無條件用途：轉寫完成後還原被拆開的寫法
          （restoreHotwordSpacing，把 "Say It" 還原成 "SayIt"）。不說的話，
          「不進 ASR」會被讀成「熱詞完全沒用」。 */}
      {state.hasSpacingRestore && state.ui !== 'sent' && (
        <p className="text-muted-foreground"><RichText text={t('dict.delivery.spacingRestore')} /></p>
      )}
      {tableOpen && <HotwordSupportTable onClose={() => setTableOpen(false)} />}
    </div>
  )
}

/** 對照表裡一行的展示狀態。 */
interface SupportRow {
  /**
   * 平台 id，用於分組小標題。
   *
   * 存 id 而不是存 `entry.label` 的結果：那個 label 是個 getter（`t()` 呼叫），
   * 把它的返回值抓進 state 就把載入那一刻的語言固化了 —— 彈窗開著時切換語言，
   * 分組標題會留在舊語言上。渲染時再取，語言就永遠是當前的。
   * 同一個坑在 `asrProviderCatalog.ts` 的 `AsrProvider.label` 上有註釋。
   */
  platform: AsrPlatform
  /** 模型顯示名（就是模型 id，和服務商文件、帳單上的寫法一致） */
  model: string
  /** 執行時 provider，用於和當前配置比對、以及取能力 */
  provider: string
  /** 這家有沒有流式實現 */
  hasStreamingPath: boolean
  /** 開著即時字幕那條路的結論 */
  streaming: HotwordUiState
  /** 關掉即時字幕（或建連失敗回落）那條路的結論 */
  buffered: HotwordUiState
  /** 兩條路徑各自的傳送上限 */
  streamingCap: number | null
  bufferedCap: number | null
}

/**
 * 「各服務對熱詞的支援」完整對照表。
 *
 * ## 為什麼這張表是生成的
 *
 * 它替代的是熱詞頁 ⓘ 裡那張**硬編碼 7 行**的表（0.1.1 加的）：漏了 Groq、OpenAI、
 * OpenAI 相容、OpenRouter、MiMo、Gemini 六家，而且不看使用者選的是誰、永遠顯示同一份
 * 內容。issue #67 就撞在這上面 —— 用 FunASR 的人讀到的是"雲端全是支援"，而他的熱詞
 * 一條都沒進請求。
 *
 * 所以問題不在"表格放介面裡"，在**手寫**：沒有任何機制讓它跟著實現走。這一版的行
 * 全部來自 Rust 的 `asr_hotword_capability_matrix`（唯一宣告處的投影），前端只負責
 * 把 provider key 翻成使用者看得懂的名字並排版。改了實現而表格沒跟上，在結構上不可能。
 *
 * 顯示順序沿用服務目錄（`ASR_PROVIDERS`）—— 那裡的順序是推薦順序，且已按平台分組。
 */
function HotwordSupportTable({ onClose }: { onClose: () => void }) {
  // 語言切換要重渲染：表裡的平台名、狀態詞、說明全走 t()。
  useT()
  const [rows, setRows] = useState<SupportRow[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [currentProvider, setCurrentProvider] = useState('')

  useEffect(() => {
    let disposed = false
    void (async () => {
      const [matrix, provider, mode] = await Promise.all([
        bridge.asrHotwordCapabilityMatrix(),
        getSetting('cloudAsr.provider', '') as Promise<string>,
        getSetting('workMode', 'server') as Promise<string>,
      ])
      if (disposed) return
      if (!matrix) {
        // 取不到就說取不到。**不要退化成一張空表** —— 空表看著像"所有服務都不支援"。
        setFailed(true)
        return
      }
      setCurrentProvider(mode === 'cloud_api' ? provider : '')
      setRows(ASR_PROVIDERS.flatMap((entry) => asrModelsOf(entry).map((model) => {
        const capability = matrix[model.provider]
        // 目錄裡有、Rust 矩陣裡沒有：那是漏宣告，按"未確定"顯示而不是"不支援"。
        // 前端測試會在這種情況下先紅（遍歷目錄斷言每個 provider 都有宣告）。
        const streaming = capability ? foldHotwordDelivery(capability.streaming) : 'undecided'
        const buffered = capability ? foldHotwordDelivery(capability.buffered) : 'undecided'
        return {
          platform: entry.platform,
          model: model.id,
          provider: model.provider,
          hasStreamingPath: capability?.hasStreamingPath ?? false,
          streaming,
          buffered,
          streamingCap: capability?.streamingClientCap ?? null,
          bufferedCap: capability?.bufferedClientCap ?? null,
        }
      })))
    })()
    return () => { disposed = true }
  }, [])

  const label = (state: HotwordUiState) => t(
    state === 'sent' ? 'dict.table.sent'
      : state === 'not_sent' ? 'dict.table.notSent'
        : 'dict.table.undecided',
  )
  const stateClass = (state: HotwordUiState) => state === 'sent'
    ? 'text-foreground'
    : state === 'not_sent' ? 'text-amber-500' : 'text-muted-foreground'

  /** 這一行的「熱詞」列怎麼寫：兩條路徑結論不同的那幾家必須分開說。 */
  const renderState = (row: SupportRow) => {
    if (!row.hasStreamingPath || row.streaming === row.buffered) {
      return <span className={stateClass(row.buffered)}>{label(row.buffered)}</span>
    }
    // 這是 OpenAI live / Gemini live 那兩家：開字幕與關字幕的答案相反。
    // 一欄裡寫一個詞會必錯一半，所以兩行分開列。
    return (
      <span className="flex flex-col gap-0.5">
        <span className={stateClass(row.streaming)}>
          {t('dict.table.whenStreaming', { state: label(row.streaming) })}
        </span>
        <span className={stateClass(row.buffered)}>
          {t('dict.table.whenBuffered', { state: label(row.buffered) })}
        </span>
      </span>
    )
  }

  /** 說明列：只放對這一行成立的話（上限是 SayIt 的傳送上限，不是服務端限制）。 */
  const renderNote = (row: SupportRow) => {
    const caps = new Set<number>()
    if (row.streaming === 'sent' && row.streamingCap !== null) caps.add(row.streamingCap)
    if (row.buffered === 'sent' && row.bufferedCap !== null) caps.add(row.bufferedCap)
    if (caps.size === 0) return null
    return t('dict.table.capNote', { cap: Array.from(caps).join(' / ') })
  }

  let lastPlatform: AsrPlatform | '' = ''

  return (
    <Modal
      title={t('dict.table.title')}
      onClose={onClose}
      showCloseButton
      /* 780 而不是更窄：模型名最長的那個有 34 個字元
         （qwen-audio-3.0-asr-flash-streaming），再加上「開即時字幕：不傳送」兩行式的
         狀態列和說明列，600 下說明列會被擠到折行。Modal 自帶
         max-w-[calc(100vw-2rem)]，小視窗下不會溢位。 */
      panelClassName="w-[780px]"
    >
      <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
        <RichText text={t('dict.table.intro')} />
      </p>

      {failed && (
        <p className="mt-4 text-xs text-amber-500">{t('dict.table.unavailable')}</p>
      )}

      {rows && (
        <table className="mt-3 w-full border-collapse text-xs">
          <thead>
            <tr className="text-left text-muted-foreground/70">
              <th className="pb-1.5 pr-3 font-normal">{t('dict.tableModel')}</th>
              <th className="pb-1.5 pr-3 font-normal">{t('dict.tableHotword')}</th>
              <th className="pb-1.5 font-normal">{t('dict.tableNote')}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const isCurrent = row.provider === currentProvider
              const platformChanged = row.platform !== lastPlatform
              lastPlatform = row.platform
              return (
                <Fragment key={`${row.provider}-${row.model}`}>
                  {platformChanged && (
                    <tr>
                      <td colSpan={3} className="pb-0.5 pt-2.5 text-[11px] font-medium text-muted-foreground">
                        {ASR_PLATFORMS[row.platform].label}
                      </td>
                    </tr>
                  )}
                  {/* 當前在用的那一行高亮 —— 使用者來查表多半就是想確認自己這一行。
                      原來那張靜態表做不到這件事（它壓根不知道使用者選了誰）。 */}
                  <tr className={cn('align-top', isCurrent && 'bg-accent/50')}>
                    <td className="py-1 pr-3">
                      {row.model}
                      {isCurrent && (
                        <span className="ml-1.5 text-[10px] text-muted-foreground">
                          {t('dict.table.current')}
                        </span>
                      )}
                    </td>
                    <td className="py-1 pr-3">{renderState(row)}</td>
                    <td className="py-1 text-muted-foreground">{renderNote(row)}</td>
                  </tr>
                </Fragment>
              )
            })}
          </tbody>
        </table>
      )}

      {/* 本地與伺服器模式不在服務目錄裡，但使用者同樣會問，所以單獨兩行補上。
          這兩條的依據不在 Rust 矩陣裡（矩陣只覆蓋雲端分發 key），是前端硬寫的 ——
          能這麼寫是因為它們各自只有一個答案：伺服器模式隨 start 訊息傳送熱詞，
          本地引擎遷到 GGML 後沒有熱詞通道（models/ 下搜 hotword 零命中）。 */}
      <p className="mt-4 text-xs leading-relaxed text-muted-foreground">
        <RichText text={t('dict.table.otherModes')} />
      </p>
      <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
        <RichText text={t('dict.table.footer')} />
      </p>
    </Modal>
  )
}

export default function Dictionary() {
  useT()
  const [tab, setTab] = useState<Tab>('hotwords')
  const [exportMessage, setExportMessage] = useState('')
  const [warnDismissed, setWarnDismissed] = useState(false)
  const {
    hotwords,
    builtinSetWords,
    builtinSetActive,
    customThemes,
    customThemeActive,
    themeInputs,
    newThemeName,
    search,
    loading,
    showUnknown,
    filtered,
    filteredUnknown,
    visibleCustomThemes,
    getSetWordsInHotwords,
    getThemeWordsInHotwords,
    setNewThemeName,
    setSearch,
    setShowUnknown,
    setThemeInput,
    addTheme,
    addWordsToTheme,
    addWordsToBuiltinSet,
    removeTheme,
    moveTheme,
    toggleCustomTheme,
    removeWord,
    toggleBuiltinSet,
    resetBuiltinSet,
  } = useHotwordsManager()

  // 內建分類「恢復預設」的二次確認：記下哪個分類已點過第一下，3 秒沒再點就撤回
  const [resetArmedKey, setResetArmedKey] = useState<string | null>(null)
  useEffect(() => {
    if (!resetArmedKey) return
    const timer = setTimeout(() => setResetArmedKey(null), 3000)
    return () => clearTimeout(timer)
  }, [resetArmedKey])
  const isBuiltinSetModified = (key: string) => {
    const words = builtinSetWords[key] || []
    const defaults = new Set(BUILTIN_SETS[key]?.words ?? [])
    return words.length !== defaults.size || words.some((word) => !defaults.has(word))
  }

  // 搜尋時列表是過濾後的，序號對不上完整列表，因此搜尋狀態下不啟用拖拽
  const themeSortable = useSortable({ onMove: (from, to) => void moveTheme(from, to) })

  const handleExport = async () => {
    try {
      const result = await exportHotwords()
      setExportMessage(result.canceled ? t('history.exportCanceled') : t('history.savedTo', { path: result.filePath ?? '' }))
    } catch (error) {
      setExportMessage(describeExportError(error) ?? t('history.exportFailed'))
    }
  }

  return (
    <div className="mx-auto max-w-4xl">
      {/* 標題欄 + Tab */}
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-4">
          <h1 className="text-2xl font-bold">{t('nav.hotwords')}</h1>
          <div className="flex gap-1 rounded-lg border border-border p-0.5">
            <button
              type="button"
              onClick={() => setTab('hotwords')}
              className={cn(
                'rounded-md px-3 py-1 text-xs transition-colors',
                tab === 'hotwords' ? 'bg-accent font-medium text-foreground' : 'text-muted-foreground hover:text-foreground',
              )}
            >{t('dict.tabHotwords')}</button>
            <button
              type="button"
              onClick={() => setTab('replacement')}
              className={cn(
                'rounded-md px-3 py-1 text-xs transition-colors',
                tab === 'replacement' ? 'bg-accent font-medium text-foreground' : 'text-muted-foreground hover:text-foreground',
              )}
            >{t('dict.tabTextProcess')}</button>
          </div>
        </div>

        {/* 熱詞 Tab 的搜尋和匯出 */}
        {tab === 'hotwords' && (
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">
              {hotwords.length} / {MAX_HOTWORDS}
            </span>
            <div className="relative">
              <Search className="absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={t('dict.searchPlaceholder')}
                className="w-52 rounded-md border border-input-border bg-input-bg py-1.5 pl-8 pr-3 text-sm"
              />
            </div>
            <Tooltip content={t('history.export')}>
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8 text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground"
                onClick={() => void handleExport()}
                aria-label={t('history.export')}
              >
                <Download className="h-4 w-4" />
              </Button>
            </Tooltip>
          </div>
        )}
      </div>

      {exportMessage && tab === 'hotwords' && (
        <p className="mb-2 text-sm text-muted-foreground">{exportMessage}</p>
      )}

      {/* ===== 文本處理 Tab ===== */}
      {tab === 'replacement' && (
        <>
          <TextFormatSection />
          <TextReplacementSection />
        </>
      )}

      {/* ===== 熱詞 Tab ===== */}
      {tab === 'hotwords' && (
        <>
          <p className="mb-4 flex items-center gap-1.5 text-sm text-muted-foreground">
            <span>{t('dict.intro')}</span>
            <Tooltip
              variant="light"
              content={
                <p className="max-w-[380px] text-left leading-relaxed">
                  <RichText text={t('dict.caveat')} />
                </p>
              }
            >
              <Info className="h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground" />
            </Tooltip>
          </p>

          {/* 當前這套配置拿熱詞做什麼。
              取代了原來那張硬編碼的「各模型對熱詞的支援」表：那張表漏了 6 家雲服務，
              而且永遠顯示同一份內容、不看使用者選的是誰（issue #67）。使用者在這一頁要
              回答的問題只有一個 —— 我配的熱詞現在有用嗎。全模型對照改由 App 內的
              HotwordSupportTable 顯示（[自訂] 上游的設定文件已自本 fork 移除）。 */}
          <HotwordDeliveryNotice hotwordCount={hotwords.length} />

          {hotwords.length > HOTWORD_SOFT_LIMIT && !warnDismissed && (
            <div className="mb-4 -mt-2 flex items-start gap-2 text-xs text-amber-500">
              <p>
                <RichText text={t('dict.countHint', { count: hotwords.length })} />
              </p>
              <button
                type="button"
                onClick={() => setWarnDismissed(true)}
                className="shrink-0 rounded p-0.5 text-amber-500/70 transition-colors hover:bg-amber-500/10 hover:text-amber-500"
                aria-label={t('dict.dismissHint')}
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          )}

          {/* 新建熱詞分類 */}
          <div className="mb-4 flex items-center gap-2">
            <FolderPlus className="h-4 w-4 shrink-0 text-muted-foreground" />
            <input
              value={newThemeName}
              onChange={(e) => setNewThemeName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  void addTheme()
                }
              }}
              placeholder={t('dict.newCategoryPlaceholder')}
              className="flex-1 rounded-md border border-input-border bg-input-bg px-3 py-1.5 text-sm"
            />
            <Button
              onClick={() => void addTheme()}
              size="sm"
              variant="outline"
              disabled={!newThemeName.trim()}
              className="shrink-0 gap-1.5"
            >
              <Plus className="h-3.5 w-3.5" />
              {t('dict.add')}
            </Button>
          </div>

          {loading ? (
            <p className="py-8 text-center text-muted-foreground">{t('dict.loading')}</p>
          ) : (
            <div className="space-y-4">
              {/* 自定義分類 */}
              {customThemes.length > 1 && !search && (
                <p className="text-xs text-muted-foreground/70">
                  {t('dict.orderHint')}
                </p>
              )}
              {visibleCustomThemes.map((theme, themeIndex) => {
                const canSort = !search && customThemes.length > 1
                const active = !!customThemeActive[theme.id]
                const activeWords = getThemeWordsInHotwords(theme)
                const totalWords = theme.words.length

                return (
                  <Card
                    key={theme.id}
                    {...(canSort ? themeSortable.rowProps(themeIndex) : {})}
                    className={cn('group', !active && 'border-dashed opacity-70', canSort && themeSortable.rowClassName(themeIndex))}
                  >
                    <CardContent className="p-4">
                      {/* 開關放右邊（和全軟體一致）；收起時沒有下方內容，不留 mb-3 的空白 */}
                      <div className={cn('flex items-center justify-between gap-3', active && 'mb-3')}>
                        <div className="flex items-center gap-3 min-w-0">
                          {canSort && <DragHandle {...themeSortable.handleProps(themeIndex, t('dict.dragCategory', { name: theme.name }))} />}
                          <div className="min-w-0">
                            <p className="text-sm font-medium">{theme.name}</p>
                            <p className="text-xs text-muted-foreground">
                              {t('dict.customCount', { active: activeWords.length, total: totalWords })}
                            </p>
                          </div>
                        </div>
                        <div className="flex shrink-0 items-center gap-3">
                          <Tooltip content={t('dict.deleteCategory')}>
                            <button
                              type="button"
                              onClick={() => void removeTheme(theme.id)}
                              className="rounded p-1.5 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                              aria-label={t('dict.deleteTheme', { name: theme.name })}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </button>
                          </Tooltip>
                          <Switch
                            checked={active}
                            onChange={() => void toggleCustomTheme(theme.id)}
                            size="sm"
                          />
                        </div>
                      </div>

                      {active && (
                        <div className="space-y-3">
                          <div className="flex gap-2">
                            <input
                              value={themeInputs[theme.id] || ''}
                              onChange={(e) => setThemeInput(theme.id, e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' && !e.shiftKey) {
                                  e.preventDefault()
                                  void addWordsToTheme(theme.id)
                                }
                              }}
                              placeholder={t('dict.addWordsPlaceholder')}
                              className="flex-1 rounded-md border border-input-border bg-input-bg px-3 py-1.5 text-sm"
                            />
                            <Button
                              onClick={() => void addWordsToTheme(theme.id)}
                              size="sm"
                              variant="outline"
                              disabled={!(themeInputs[theme.id] || '').trim()}
                              className="shrink-0 gap-1.5"
                            >
                              <Plus className="h-3.5 w-3.5" />
                              {t('dict.add')}
                            </Button>
                          </div>

                          {activeWords.length > 0 && (
                            <WordChips words={activeWords} onRemove={removeWord} expandAll={!!search} />
                          )}
                        </div>
                      )}
                    </CardContent>
                  </Card>
                )
              })}

              {/* 內建分類 */}
              {Object.entries(BUILTIN_SETS).map(([key, setDef]) => {
                const active = !!builtinSetActive[key]
                const activeWords = getSetWordsInHotwords(key)
                const totalWords = (builtinSetWords[key] || []).length

                if (search && activeWords.length === 0 && !setDef.label.toLowerCase().includes(search.toLowerCase())) {
                  return null
                }

                const inputKey = builtinInputKey(key)
                const modified = isBuiltinSetModified(key)
                const resetTip = t(modified ? 'dict.resetTip' : 'dict.resetTipDefault')

                return (
                  <Card key={key} className={cn(!active && 'border-dashed opacity-70')}>
                    <CardContent className="p-4">
                      <div className={cn('flex items-center justify-between gap-3', active && 'mb-3')}>
                        <div className="flex items-center gap-3 min-w-0">
                          {/* 自建分類可拖動時左邊有手柄，內建分類留同寬空位，名字才能對齊 */}
                          {!search && customThemes.length > 1 && <span className="w-[22px] shrink-0" aria-hidden />}
                          <div className="min-w-0">
                            <p className="text-sm font-medium">{setDef.label}</p>
                            <p className="text-xs text-muted-foreground">
                              {t('dict.builtinCount', { desc: setDef.description, active: activeWords.length, total: totalWords })}
                            </p>
                          </div>
                        </div>
                        <div className="flex shrink-0 items-center gap-3">
                          {/* 和自建分類的刪除鍵同一種小圖示按鈕。詞表沒改過時置灰、點不了（藏起來的話使用者以為按鈕丟了）；
                              改過之後要點兩下：第一下變成「確認恢復」，3 秒內再點才執行 —— 它會清掉使用者加的詞，
                              以前是個帶文字的大按鈕，挨著開關，很容易誤點。 */}
                          {resetArmedKey === key && modified ? (
                            <button
                              type="button"
                              onClick={() => { setResetArmedKey(null); void resetBuiltinSet(key) }}
                              className="rounded px-1.5 py-1 text-xs text-destructive transition-colors hover:bg-destructive/10"
                            >
                              {t('dict.resetConfirm')}
                            </button>
                          ) : (
                            <Tooltip content={resetTip}>
                              {/* 包一層 span：disabled 的 button 不派發滑鼠事件，提示會出不來 */}
                              <span className="inline-flex">
                                <button
                                  type="button"
                                  disabled={!modified}
                                  onClick={() => setResetArmedKey(key)}
                                  className="rounded p-1.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-35"
                                  aria-label={resetTip}
                                >
                                  <RotateCcw className="h-3.5 w-3.5" />
                                </button>
                              </span>
                            </Tooltip>
                          )}
                          <Switch
                            checked={active}
                            onChange={() => void toggleBuiltinSet(key)}
                            size="sm"
                          />
                        </div>
                      </div>

                      {/* 和自建分類同一套新增框：使用者常想把自己領域的詞補進內建分類，而不是為一兩個詞另建分類 */}
                      {active && (
                        <div className="space-y-3">
                          <div className="flex gap-2">
                            <input
                              value={themeInputs[inputKey] || ''}
                              onChange={(e) => setThemeInput(inputKey, e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' && !e.shiftKey) {
                                  e.preventDefault()
                                  void addWordsToBuiltinSet(key)
                                }
                              }}
                              placeholder={t('dict.addWordsPlaceholder')}
                              className="flex-1 rounded-md border border-input-border bg-input-bg px-3 py-1.5 text-sm"
                            />
                            <Button
                              onClick={() => void addWordsToBuiltinSet(key)}
                              size="sm"
                              variant="outline"
                              disabled={!(themeInputs[inputKey] || '').trim()}
                              className="shrink-0 gap-1.5"
                            >
                              <Plus className="h-3.5 w-3.5" />
                              {t('dict.add')}
                            </Button>
                          </div>

                          {activeWords.length > 0 && (
                            <WordChips words={activeWords} onRemove={removeWord} expandAll={!!search} />
                          )}
                        </div>
                      )}
                    </CardContent>
                  </Card>
                )
              })}

              {/* 歷史未分類詞彙 */}
              {filteredUnknown.length > 0 && (
                <Card>
                  <CardContent className="p-4">
                    <button
                      className="mb-2 flex w-full items-center justify-between text-left"
                      onClick={() => setShowUnknown(!showUnknown)}
                    >
                      <div>
                        <p className="text-sm font-medium">{t('dict.legacyTitle')}</p>
                        <p className="text-xs text-muted-foreground">
                          {t('dict.legacyDesc')}
                        </p>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-xs text-muted-foreground">{filteredUnknown.length}</span>
                        {showUnknown ? <ChevronUp className="h-4 w-4 text-muted-foreground" /> : <ChevronDown className="h-4 w-4 text-muted-foreground" />}
                      </div>
                    </button>

                    {showUnknown && (
                      <WordChips words={filteredUnknown} onRemove={removeWord} expandAll={!!search} />
                    )}
                  </CardContent>
                </Card>
              )}

              {!search && hotwords.length === 0 && (
                <div className="rounded-lg border border-dashed border-border py-8 text-center">
                  <p className="text-sm text-muted-foreground">
                    {/* 已經建過分類卻還是 0 個生效詞時，叫人「先新建分類」就不對了 */}
                    {customThemes.length > 0 ? t('dict.emptyWithCategories') : t('dict.empty')}
                  </p>
                </div>
              )}

              {search && filtered.length === 0 && (
                <p className="py-8 text-center text-muted-foreground">{t('dict.noMatch')}</p>
              )}
            </div>
          )}
        </>
      )}
    </div>
  )
}
