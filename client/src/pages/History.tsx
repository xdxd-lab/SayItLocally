import * as bridge from '@/services/bridge'
import { cn } from '@/lib/utils'
import { buildAsrExtra, resolveAsrDisplayModel, isQwenOmniProvider } from '@/lib/asrModels'
import { getWorkMode } from '@/services/transcription'
import { polishWithClientAi } from '@/services/transcription/clientAiPolish'
import {
  extractServerAiEvidence,
  policyFromSnapshot,
  resolveAndLogAiOutcome,
  serverShouldPolish,
  type AiConfigSnapshot,
  type AiOutcomeContext,
} from '@/services/transcription/aiPolicy'
import { SERVER_AI_SOURCE_KEY } from '@/services/transcription/serverAiSource'
import type { AiExecutionSource, AiExecutionStatus, WorkMode } from '@/services/transcription'
import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Download, Search, Check, FolderOpen } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/tooltip'
import { useSlidingIndicator } from '@/components/ui/useSlidingIndicator'
import HistoryRecordList from '@/components/history/HistoryRecordList'
import { describeExportError, exportHistory } from '@/services/exports'
import {
  countHistory,
  deleteHistory,
  listHistory,
  setHistoryFavorite,
  updateHistoryRecord,
  getActivePreset,
  getSetting,
  type HistoryRecord,
} from '@/services/store'
import { useT } from '@/i18n/useT'
import { applyTextTransforms, restoreHotwordSpacing } from '@/services/textPostProcess'
import { buildHotwordInjectionPart } from '@/services/personalization/promptRouter'
import {
  BUILTIN_SET_WORDS_KEY,
  BUILTIN_SET_ACTIVE_KEY,
  BUILTIN_SET_EXTRA_WORDS_KEY,
  CUSTOM_THEMES_KEY,
  CUSTOM_THEME_ACTIVE_KEY,
  composeHotwords,
  normalizeBuiltinSetActive,
  normalizeBuiltinSetWords,
  normalizeCustomThemeActive,
  normalizeCustomThemes,
} from '@/services/hotwords/model'

const HISTORY_PAGE_SIZE = 100

interface ReprocessResult {
  asrText: string
  llmText: string
  asrMs: number
  llmMs: number
  durationSec: number
  asrEngine?: string
  asrModel?: string
  aiSource?: AiExecutionSource
  aiStatus?: AiExecutionStatus
  aiReason?: string
  aiProvider?: string
  aiModel?: string
  serverAi?: { error?: string; provider?: string }
}

/**
 * 一次重跑的 AI 上下文。
 *
 * snapshot 是**這次重跑**的配置（不是被重跑那條記錄當初的配置）；operationId 每次新建，
 * 不復用舊記錄的標識，否則兩次處理在日誌裡分不開。
 */
interface ReprocessAiContext {
  snapshot: AiConfigSnapshot
  context: AiOutcomeContext
}

async function buildReprocessAiContext(recordId: string): Promise<ReprocessAiContext> {
  const [rawSource, rawMin, rawEnabled] = await Promise.all([
    getSetting(SERVER_AI_SOURCE_KEY, 'managed') as Promise<string>,
    getSetting('aiMinDurationSec', 0),
    getSetting('aiEnabled', false),
  ])
  return {
    snapshot: {
      workMode: getWorkMode(),
      aiEnabled: Boolean(rawEnabled),
      aiMinDurationSec: Math.max(0, Number(rawMin) || 0),
      serverAiSource: rawSource === 'custom' ? 'custom' : 'managed',
    },
    context: {
      operationId: `reprocess-${recordId}-${Date.now().toString(36)}`,
      trigger: 'history_reprocess',
    },
  }
}

/** 伺服器模式重新識別：透過獨立 WebSocket 連線，避免干擾全域性連線 */
async function reprocessViaServer(
  chunk: ArrayBuffer,
  hotwords: string[],
  ai: ReprocessAiContext,
  systemPrompt: string | undefined,
  clientMeta: Awaited<ReturnType<typeof bridge.getClientRuntimeInfo>> | null,
): Promise<ReprocessResult> {
  const { getWSUrl } = await import('@/services/runtimeConfig')
  const wsUrl = getWSUrl()
  const audioDurationSec = (chunk.byteLength / 2) / 16000
  // 判據與即時錄音共用同一個函式。這裡原來自己又算了一遍門檻/來源/是否用內建 AI，
  // 是本輪要消滅的第二份實現。
  const policy = policyFromSnapshot(ai.snapshot, 'server', audioDurationSec)
  const useManagedAi = serverShouldPolish(policy)
  const useCustomAi = policy.allowCall && policy.route === 'custom'

  const serverResult = await new Promise<ReprocessResult>((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      try { socket.close() } catch { /* ignore */ }
      reject(new Error('Retranscription timed out'))
    }, 30_000) // ASR 最多 30 秒

    const socket = new WebSocket(wsUrl)
    socket.binaryType = 'arraybuffer'

    let resolved = false

    socket.onopen = () => {
      const startMsg: Record<string, unknown> = {
        cmd: 'start',
        source: 'history_reprocess',
        disable_ai: !useManagedAi,
      }
      if (useManagedAi && systemPrompt) startMsg.system_prompt = systemPrompt
      if (clientMeta) {
        startMsg.client_meta = {
          user_id: clientMeta.userId,
          device_id: clientMeta.deviceId,
          hostname: clientMeta.hostname,
          client_version: clientMeta.clientVersion,
          platform: clientMeta.platform,
          os_version: clientMeta.osVersion,
          local_ip: clientMeta.localIp,
          system_locale: clientMeta.systemLocale,
          cpu_cores: clientMeta.cpuCores,
          memory_mb: clientMeta.memoryMb,
        }
      }
      if (hotwords.length > 0) startMsg.hotwords = hotwords
      socket.send(JSON.stringify(startMsg))

      // 分片傳送 PCM 資料
      const CHUNK_SIZE = 32000
      const totalBytes = chunk.byteLength
      for (let offset = 0; offset < totalBytes; offset += CHUNK_SIZE) {
        const end = Math.min(offset + CHUNK_SIZE, totalBytes)
        socket.send(chunk.slice(offset, end))
      }

      socket.send(JSON.stringify({ cmd: 'stop' }))
    }

    socket.onmessage = (ev) => {
      if (typeof ev.data !== 'string') return
      try {
        const msg = JSON.parse(ev.data)
        if (msg.type === 'final') {
          resolved = true
          clearTimeout(timeout)
          socket.close()
          resolve({
            asrText: msg.asr_text || '',
            llmText: msg.llm_text || '',
            asrMs: msg.asr_ms || 0,
            llmMs: msg.llm_ms || 0,
            durationSec: Number(msg.duration_sec || 0),
            asrEngine: msg.asr_engine || undefined,
            asrModel: msg.asr_model || undefined,
            // 與即時路徑同一個提取函式：重跑此前完全不看執行證據，於是服務端
            // 呼叫失敗在重跑後的記錄裡和"沒呼叫"分不開。
            serverAi: extractServerAiEvidence(msg.llm_debug),
          })
        } else if (msg.type === 'done' && !resolved) {
          // 沒有 final 就 done 了（後端判定為靜音/無結果）
          resolved = true
          clearTimeout(timeout)
          socket.close()
          resolve({ asrText: '', llmText: '', asrMs: 0, llmMs: 0, durationSec: 0 })
        } else if (msg.type === 'error') {
          resolved = true
          clearTimeout(timeout)
          socket.close()
          reject(new Error(msg.message || 'backend error'))
        }
      } catch { /* ignore parse errors */ }
    }

    socket.onerror = () => {
      if (!resolved) {
        resolved = true
        clearTimeout(timeout)
        reject(new Error('WebSocket connection error'))
      }
    }

    socket.onclose = (ev) => {
      if (!resolved) {
        resolved = true
        clearTimeout(timeout)
        reject(new Error(`WebSocket closed unexpectedly, code=${ev.code}`))
      }
    }
  })

  if (!useCustomAi) {
    // 內建 AI 路線（含空識別、總開關關閉、低於門檻）：結論一律由共用判據給出，
    // 不再拿 llmMs > 0 反推成功失敗。
    const outcome = resolveAndLogAiOutcome(ai.context, policy, {
      asrTextEmpty: !serverResult.asrText.trim(),
      serverError: serverResult.serverAi?.error,
      serverProvider: serverResult.serverAi?.provider,
      llmMs: serverResult.llmMs,
    })
    return {
      ...serverResult,
      aiSource: outcome.source,
      aiStatus: outcome.status,
      aiReason: outcome.reason,
      aiProvider: outcome.provider,
      aiModel: outcome.model,
    }
  }

  const polished = await polishWithClientAi({
    asrText: serverResult.asrText,
    startOptions: {
      runId: 1,
      operationId: ai.context.operationId,
      aiConfig: ai.snapshot,
      systemPrompt,
      source: 'history_reprocess',
    },
    policy,
    outcomeContext: ai.context,
    logSource: 'history',
  })
  return polished ? { ...serverResult, ...polished } : serverResult
}

/** 雲 API 模式重新識別：呼叫 cloud_transcribe + 可選 cloud_polish，與 CloudAPIProvider 一致 */
async function reprocessViaCloudApi(
  chunk: ArrayBuffer,
  hotwords: string[],
  ai: ReprocessAiContext,
  systemPrompt: string | undefined,
): Promise<ReprocessResult> {
  const durationSec = (chunk.byteLength / 2) / 16000

  const asrProvider = await getSetting('cloudAsr.provider', 'doubao') as string
  const isQwenOmni = isQwenOmniProvider(asrProvider)
  const asrApiKey = await getSetting('cloudAsr.apiKey', '') as string
  const asrAppId = await getSetting('cloudAsr.appId', '') as string
  const asrModel = await getSetting('cloudAsr.model', '') as string

  let omniInstructions: string | undefined
  if (isQwenOmni) {
    const savedPrompt = await getSetting('cloudAsr.omniSystemPrompt', '') as string
    omniInstructions = savedPrompt || undefined
  }

  const baseUrl = await getSetting('cloudAsr.baseUrl', '') as string
  const protocol = await getSetting('cloudAsr.protocol', 'auto') as string
  const extra = buildAsrExtra(asrProvider, {
    model: asrModel,
    instructions: omniInstructions,
    baseUrl,
    protocol,
  })
  const asrConfig: Record<string, unknown> = {
    provider: isQwenOmni ? 'qwen_omni' : asrProvider,
    api_key: asrApiKey,
    app_id: asrAppId,
    ...(extra && { extra }),
  }

  const asrStart = performance.now()
  const asrResult = await bridge.cloudTranscribeRaw<{ text: string; elapsed_ms: number }>(
    new Uint8Array(chunk),
    { sample_rate: 16000, asr_config: asrConfig, hotwords },
  )
  // 還原被 ASR 拆開加空格的無空格熱詞（如豆包把 "SayIt" 識別成 "Say It"）
  const asrText = restoreHotwordSpacing(asrResult.text, hotwords)
  const asrMs = asrResult.elapsed_ms || Math.round(performance.now() - asrStart)

  // 改走共用潤色。原來這裡直接 invoke('cloud_polish')，與即時路徑有三處分歧：
  //   1. 完全不檢查短語音門檻（即時會跳過的語音，重跑卻會調 AI）；
  //   2. 配置判據寫成 `url && key && model`，漏了 ollama 免密豁免 —— ollama 使用者
  //      重跑時被靜默判成"配置不完整"；
  //   3. catch 空吞，既無日誌也無執行狀態，失敗與未呼叫在記錄裡分不開。
  const policy = policyFromSnapshot(ai.snapshot, 'cloud_api', durationSec, isQwenOmni)
  const polish = await polishWithClientAi({
    asrText,
    startOptions: {
      runId: 1,
      operationId: ai.context.operationId,
      aiConfig: ai.snapshot,
      systemPrompt,
      source: 'history_reprocess',
    },
    policy,
    outcomeContext: ai.context,
    logSource: 'history',
  })

  return {
    asrText,
    llmText: polish?.llmText ?? asrText,
    asrMs,
    llmMs: polish?.llmMs ?? 0,
    durationSec,
    aiSource: polish?.aiSource,
    aiStatus: polish?.aiStatus,
    aiReason: polish?.aiReason,
    aiProvider: polish?.aiProvider,
    aiModel: polish?.aiModel,
    ...(isQwenOmni && { asrEngine: 'qwen_omni', asrModel: extra?.model }),
  }
}

/** 本地模式重新識別：呼叫 local_transcribe + 可選 cloud_polish，與 LocalProvider 一致 */
async function reprocessViaLocal(
  chunk: ArrayBuffer,
  hotwords: string[],
  ai: ReprocessAiContext,
  systemPrompt: string | undefined,
): Promise<ReprocessResult> {
  const durationSec = (chunk.byteLength / 2) / 16000

  const modelId = await getSetting('localAsr.modelId', 'sensevoice-small-gguf') as string
  const language = await getSetting('localAsr.language', 'auto') as string
  const accelerator = await getSetting('localAsr.accelerator', 'auto') as string
  const gpuDevice = await getSetting('localAsr.gpuDevice', '') as string

  // hotwords 在 GGUF 引擎上不支持（transcribe.cpp 只有 whisper 族接 initial prompt），
  // 引數留著是為了不改呼叫方簽名，後端會忽略。
  void hotwords
  const asrResult = await bridge.localTranscribeRaw(new Uint8Array(chunk), {
    modelId, language, accelerator, gpuDevice,
  })
  const asrText = asrResult.text
  const asrMs = asrResult.elapsed_ms

  // 同雲 API 重跑：改走共用潤色，補上短語音門檻與執行狀態（原來也是空 catch 吞掉失敗）。
  const policy = policyFromSnapshot(ai.snapshot, 'local', durationSec)
  const polish = await polishWithClientAi({
    asrText,
    startOptions: {
      runId: 1,
      operationId: ai.context.operationId,
      aiConfig: ai.snapshot,
      systemPrompt,
      source: 'history_reprocess',
    },
    policy,
    outcomeContext: ai.context,
    logSource: 'history',
  })

  return {
    asrText,
    llmText: polish?.llmText ?? asrText,
    asrMs,
    llmMs: polish?.llmMs ?? 0,
    durationSec,
    aiSource: polish?.aiSource,
    aiStatus: polish?.aiStatus,
    aiReason: polish?.aiReason,
    aiProvider: polish?.aiProvider,
    aiModel: polish?.aiModel,
  }
}

/** 重新識別後寫回歷史記錄所需的供應商後設資料 */
async function buildReprocessMetadata(
  workMode: WorkMode,
  result: ReprocessResult,
): Promise<{
  asrProvider?: string
  aiProvider?: string
  aiModel?: string
  aiSource?: AiExecutionSource
  aiStatus?: AiExecutionStatus
}> {
  // AI 那幾個欄位一律取本次執行結果，不再讀當前設定。
  //
  // 此前只有 server 分支返回 aiSource/aiStatus，cloud_api 與 local 返回 undefined，
  // 而寫回是顯式傳值 —— 於是重跑一次就把記錄裡原有的執行狀態抹成空；同時 aiProvider
  // 照當前設定填，哪怕這次 AI 壓根沒跑，記錄裡也會寫著某個服務商。
  const aiFields = {
    aiSource: result.aiSource,
    aiStatus: result.aiStatus,
    // 只有真的執行到（或嘗試過）才寫服務商，避免"沒跑卻記著供應商"。
    aiProvider: result.aiStatus === 'skipped' ? undefined : result.aiProvider,
    aiModel: result.aiStatus === 'skipped' ? undefined : result.aiModel,
  }

  if (workMode === 'cloud_api') {
    const asrProviderKey = await getSetting('cloudAsr.provider', '') as string
    // 選定的模型要一起帶上：只按 provider 推的話，使用者明明選了 whisper-large-v3，
    // 歷史記錄裡卻會寫成該服務的預設模型
    const asrSelectedModel = await getSetting('cloudAsr.model', '') as string
    return { asrProvider: resolveAsrDisplayModel(asrProviderKey, asrSelectedModel), ...aiFields }
  }
  if (workMode === 'local') {
    const modelId = await getSetting('localAsr.modelId', '') as string
    return { asrProvider: modelId || 'local', ...aiFields }
  }
  return {
    asrProvider: (result.asrModel || result.asrEngine || 'server').replace(/^.*\//, ''),
    ...aiFields,
  }
}

export default function History() {
  const t = useT()
  const [records, setRecords] = useState<HistoryRecord[]>([])
  const [keyword, setKeyword] = useState('')
  const [debouncedKeyword, setDebouncedKeyword] = useState('')
  const [favoriteOnly, setFavoriteOnly] = useState(false)
  const filterGroupRef = useRef<HTMLDivElement>(null)
  const filterIndicatorRef = useSlidingIndicator(filterGroupRef, favoriteOnly, { activeSelector: '[aria-pressed="true"]' })
  const [visibleCount, setVisibleCount] = useState(HISTORY_PAGE_SIZE)
  const [totalCount, setTotalCount] = useState(0)
  const [exportResult, setExportResult] = useState<{ filePath: string | null; canceled: boolean } | null>(null)
  const [exporting, setExporting] = useState(false)
  const [exportError, setExportError] = useState('')
  const [deleteError, setDeleteError] = useState(false)

  const loadRecords = useCallback(async (searchKeyword: string, limit: number, favOnly: boolean) => {
    const [items, total] = await Promise.all([
      listHistory({ keyword: searchKeyword, favoriteOnly: favOnly, limit, offset: 0 }),
      countHistory({ keyword: searchKeyword, favoriteOnly: favOnly }),
    ])
    setRecords(items)
    setTotalCount(total)
  }, [])

  useEffect(() => {
    setVisibleCount(HISTORY_PAGE_SIZE)
  }, [debouncedKeyword, favoriteOnly])

  // 搜尋防抖：輸入停止 300ms 後才觸發查詢
  useEffect(() => {
    const timer = setTimeout(() => setDebouncedKeyword(keyword), 300)
    return () => clearTimeout(timer)
  }, [keyword])

  useEffect(() => {
    void loadRecords(debouncedKeyword, visibleCount, favoriteOnly)
  }, [debouncedKeyword, favoriteOnly, loadRecords, visibleCount])

  // 監聽新記錄寫入，自動重新整理列表
  useEffect(() => {
    const unlisten = bridge.listen('history-updated', () => {
      void loadRecords(debouncedKeyword, visibleCount, favoriteOnly)
    })
    return () => { void unlisten.then((fn) => fn()) }
  }, [debouncedKeyword, visibleCount, favoriteOnly, loadRecords])

  const handleDelete = async (id: string) => {
    // [自訂] S71：如果錄音仍在使用或刪除失敗，保留歷史列並顯示錯誤。
    setDeleteError(false)
    try {
      const record = records.find((r) => r.id === id)
      if (record?.audioFilePath) await bridge.deleteAudioFile(record.audioFilePath)
      await deleteHistory(id)
      void loadRecords(debouncedKeyword, visibleCount, favoriteOnly)
    } catch {
      setDeleteError(true)
    }
  }

  const handleToggleFavorite = async (id: string, nextFavorite: boolean) => {
    await setHistoryFavorite(id, nextFavorite)
    setRecords((prev) => prev.map((r) => (r.id === id ? { ...r, favorite: nextFavorite } : r)))
  }

  const handleEdit = async (id: string, nextText: string) => {
    const editedAt = Date.now()
    const patch = {
      llmText: nextText,
      charCount: nextText.length,
      isEmpty: !nextText.trim(),
      manualEditedAt: editedAt,
    }
    await updateHistoryRecord(id, patch)
    // 區域性更新，避免整頁重新整理丟失展開態/滾動位置
    setRecords((prev) => prev.map((r) => (r.id === id ? { ...r, ...patch } : r)))
  }

  /**
   * 記下「這條的 ASR 糾錯已提交 / 已撤回」。
   *
   * 只寫 asrCorrection* 三個欄位，**不動 llmText / charCount / manualEditedAt** ——
   * 糾正的是識別原文，正文和統計不該被它帶著改（handleEdit 改的才是正文）。
   */
  const handleSaveAsrCorrection = async (id: string, patch: Partial<HistoryRecord>) => {
    await updateHistoryRecord(id, patch)
    setRecords((prev) => prev.map((r) => (r.id === id ? { ...r, ...patch } : r)))
  }

  const handleExport = async () => {
    // [自訂] L-82：匯出確認和寫檔期間只處理一份篩選結果。
    setExporting(true)
    setExportError('')
    setExportResult(null)
    try {
      const result = await exportHistory({ keyword: debouncedKeyword, favoriteOnly })
      setExportResult(result)
      if (!result.canceled) setTimeout(() => setExportResult(null), 8000)
    } catch (error) {
      setExportError(describeExportError(error) ?? t('history.exportFailed'))
    } finally {
      setExporting(false)
    }
  }

  const handleReprocess = async (record: HistoryRecord) => {
    if (!record.audioFilePath) return

    const bytes = await bridge.readAudioFileBytes(record.audioFilePath)
    // 錄音已被保留期清掉或被手動刪了。原來這裡靜默 return，按鈕轉一下就沒下文。
    if (!bytes) throw new Error(t('record.audioMissing'))

    // WAV → PCM（去掉 44 位元組檔案頭）
    const pcmData = bytes.slice(44)
    const chunk = pcmData.buffer.slice(pcmData.byteOffset, pcmData.byteOffset + pcmData.byteLength)

    // Diagnostic: compute peak amplitude of the PCM data being sent
    const pcmInt16 = new Int16Array(chunk)
    let reprocessPeak = 0
    for (let i = 0; i < pcmInt16.length; i++) {
      const v = Math.abs(pcmInt16[i])
      if (v > reprocessPeak) reprocessPeak = v
    }
    const reprocessPeakNorm = reprocessPeak / 32768
    const reprocessDurSec = pcmInt16.length / 16000
    console.log('[reprocess-diag] PCM stats', {
      byteLength: chunk.byteLength,
      samples: pcmInt16.length,
      durationSec: reprocessDurSec.toFixed(2),
      peakInt16: reprocessPeak,
      peakNormalized: reprocessPeakNorm.toFixed(4),
      wouldBeSilent: reprocessPeakNorm < 0.01,
    })

    const preset = await getActivePreset()
    const aiEnabled = await getSetting('aiEnabled', false)

    // 載入熱詞
    let hotwords: string[] = []
    try {
      const [rawSetWords, rawSetActive, rawCustomThemes, rawCustomThemeActive, rawSetExtraWords] = await Promise.all([
        getSetting(BUILTIN_SET_WORDS_KEY, {}),
        getSetting(BUILTIN_SET_ACTIVE_KEY, {}),
        getSetting(CUSTOM_THEMES_KEY, []),
        getSetting(CUSTOM_THEME_ACTIVE_KEY, {}),
        getSetting(BUILTIN_SET_EXTRA_WORDS_KEY, {}),
      ])
      const setWords = normalizeBuiltinSetWords(rawSetWords, rawSetExtraWords)
      const setActive = normalizeBuiltinSetActive(rawSetActive as Record<string, unknown>)
      const themes = normalizeCustomThemes(rawCustomThemes)
      const themeActive = normalizeCustomThemeActive(rawCustomThemeActive as Record<string, unknown>, themes)
      hotwords = composeHotwords([], setWords, setActive, themes, themeActive)
    } catch { /* ignore */ }

    const clientMeta = await bridge.getClientRuntimeInfo().catch(() => null)

    // 按使用者當前選擇的工作模式重新識別，與即時錄音保持一致
    // （此前這裡硬編碼走伺服器模式，導致雲 API/本地模式下重新識別被錯誤地發回伺服器）
    const workMode = getWorkMode()
    let systemPrompt = aiEnabled ? preset.systemPrompt : undefined
    // 與即時一致：開啟"熱詞注入 AI 提示詞"時，重新識別也把熱詞表注入系統提示詞
    if (systemPrompt && (await getSetting('injectHotwordsToPrompt', false))) {
      const part = buildHotwordInjectionPart(hotwords)
      if (part) systemPrompt = `${systemPrompt}\n\n${part}`
    }

    // 一次重跑只建一份 AI 上下文，三條路徑共用：策略同一個函式算，結果同一條 ai.outcome。
    const ai = await buildReprocessAiContext(record.id)

    let result: ReprocessResult
    if (workMode === 'cloud_api') {
      result = await reprocessViaCloudApi(chunk, hotwords, ai, systemPrompt)
    } else if (workMode === 'local') {
      result = await reprocessViaLocal(chunk, hotwords, ai, systemPrompt)
    } else {
      result = await reprocessViaServer(chunk, hotwords, ai, systemPrompt, clientMeta)
    }

    // 新鏈路優先採用顯式執行狀態；舊的雲/本地歷史重跑尚未返回狀態時才相容文本比較。
    const rawAsr = result.aiStatus
      ? result.aiStatus !== 'applied'
      : !result.llmText || result.llmText === result.asrText
    const baseText = rawAsr ? result.asrText : result.llmText
    const replacedLlm = await applyTextTransforms(baseText, { rawAsr })

    const meta = await buildReprocessMetadata(workMode, result)

    await updateHistoryRecord(record.id, {
      asrText: result.asrText,
      llmText: replacedLlm,
      asrMs: result.asrMs,
      llmMs: result.llmMs,
      charCount: (result.llmText || result.asrText).length,
      isEmpty: !(result.llmText || result.asrText).trim(),
      workMode,
      aiSource: meta.aiSource,
      aiStatus: meta.aiStatus,
      aiProvider: meta.aiProvider,
      aiModel: meta.aiModel,
      asrProvider: meta.asrProvider,
    })

    void loadRecords(debouncedKeyword, visibleCount, favoriteOnly)
  }

  return (
    <div className="mx-auto max-w-4xl">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-4">
          <h1 className="text-2xl font-bold">{t('history.title')}</h1>
          {/* 選中底色是滑塊（useSlidingIndicator），按鈕自己不畫；aria-pressed 同時給讀屏和滑塊定位用。 */}
          <div ref={filterGroupRef} className="relative flex gap-1 rounded-lg border border-border p-0.5">
            <span ref={filterIndicatorRef} aria-hidden className="sliding-indicator rounded-md bg-accent" />
            <button
              type="button"
              aria-pressed={!favoriteOnly}
              onClick={() => setFavoriteOnly(false)}
              className={cn(
                'relative rounded-md px-3 py-1 text-xs transition-colors',
                !favoriteOnly ? 'font-medium text-foreground' : 'text-muted-foreground hover:text-foreground',
              )}
            >{t('history.filterAll')}</button>
            <button
              type="button"
              aria-pressed={favoriteOnly}
              onClick={() => setFavoriteOnly(true)}
              className={cn(
                'relative rounded-md px-3 py-1 text-xs transition-colors',
                favoriteOnly ? 'font-medium text-foreground' : 'text-muted-foreground hover:text-foreground',
              )}
            >{t('history.filterFavorites')}</button>
          </div>
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <div className="relative">
            <Search className="absolute left-2 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
            <input
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
              placeholder={t('history.searchPlaceholder')}
              className="w-64 rounded-md border border-input-border bg-input-bg py-1.5 pl-8 pr-3 text-sm"
            />
          </div>
          <Tooltip content={t('history.export')}>
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8 text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground"
              onClick={() => void handleExport()}
              disabled={exporting}
              aria-label={t('history.export')}
              title={t('history.export')}
            >
              <Download className="h-4 w-4" />
            </Button>
          </Tooltip>
        </div>
      </div>

      {exportResult && !exportResult.canceled && exportResult.filePath && (
        <div className="mb-3 flex items-center gap-2 text-xs text-success">
          <Check className="h-3.5 w-3.5 shrink-0" />
          <span className="min-w-0 truncate">{t('history.savedTo', { path: exportResult.filePath })}</span>
          <button
            onClick={() => void invoke('reveal_file_in_folder', { filePath: exportResult.filePath })}
            className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <FolderOpen className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      {exportResult?.canceled && (
        <p className="mb-3 text-xs text-muted-foreground">{t('history.exportCanceled')}</p>
      )}
      {exportError && <p role="alert" className="mb-3 text-xs text-destructive">{exportError}</p>}
      {deleteError && <p role="alert" className="mb-3 text-xs text-destructive">{t('history.deleteFailed')}</p>}

      <HistoryRecordList
        records={records}
        onDelete={handleDelete}
        onToggleFavorite={handleToggleFavorite}
        onReprocess={handleReprocess}
        onEdit={handleEdit}
        onSaveAsrCorrection={handleSaveAsrCorrection}
        highlight={debouncedKeyword}
        emptyText={keyword.trim() ? t('history.emptyNoMatch') : favoriteOnly ? t('history.emptyFavorites') : t('history.empty')}
      />

      {totalCount > records.length && (
        <div className="mt-4 flex justify-center">
          <Button variant="outline" size="sm" onClick={() => setVisibleCount((count) => count + HISTORY_PAGE_SIZE)}>
            {t('history.loadMore')}
          </Button>
        </div>
      )}
    </div>
  )
}
