// ASR 測試卡片 — 用內建測試音訊測試當前模式的識別效果

import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Play, Pause } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Feedback } from '@/components/ui/feedback'
import { getSetting } from '@/services/store'
import { getEngineDraftDirty, subscribeEngineDraft } from '@/stores/engineDraft'
import { buildAsrExtra, isQwenOmniProvider, resolveAsrDisplayModel } from '@/lib/asrModels'
import { describeProviderError, describeServerError } from '@/lib/errorMessages'
import type { WorkMode } from '@/services/transcription'
import { useT } from '@/i18n/useT'

interface TestResult {
  text: string
  asrMs: number
  mode: WorkMode
  model: string
  audioDurationSec: number
}

interface TestError {
  message: string
  detail?: string
}

export default function AsrTestSection({ workMode }: { workMode: WorkMode }) {
  const t = useT()
  const [testing, setTesting] = useState(false)
  const [playing, setPlaying] = useState(false)
  const [result, setResult] = useState<TestResult | null>(null)
  // 失敗原來是被寫進 result.text 的，於是用與成功完全相同的面板渲染出來，
  // 外面還配著「ASR 0ms」的徽標——失敗長得像"成功識別出了『測試失敗:』這幾個字"。
  const [error, setError] = useState<TestError | null>(null)
  const audioRef = useRef<HTMLAudioElement | null>(null)

  // 上方配置有未儲存改動時，這裡讀到的還是舊值，測出來的結果沒有意義
  const draftDirty = useSyncExternalStore(subscribeEngineDraft, getEngineDraftDirty)

  // 換了工作模式，舊結果就不再說明任何事情，直接作廢
  useEffect(() => {
    setResult(null)
    setError(null)
  }, [workMode])

  async function handlePlay() {
    if (playing && audioRef.current) {
      audioRef.current.pause()
      audioRef.current.currentTime = 0
      setPlaying(false)
      return
    }
    try {
      const b64 = await invoke<string>('get_test_audio_b64')
      const audio = new Audio(`data:audio/wav;base64,${b64}`)
      audioRef.current = audio
      audio.onended = () => setPlaying(false)
      setPlaying(true)
      await audio.play()
    } catch {
      setPlaying(false)
    }
  }

  async function handleTest() {
    setTesting(true)
    setResult(null)
    setError(null)

    try {
      // 獲取測試音訊並計算時長
      const wavB64 = await invoke<string>('get_test_audio_b64')
      const wavBytes = Uint8Array.from(atob(wavB64), (c) => c.charCodeAt(0))
      const pcmBytes = wavBytes.slice(44)
      const audioDurationSec = pcmBytes.length / 2 / 16000

      if (workMode === 'local') {
        const modelId = await getSetting('localAsr.modelId', 'sensevoice-small-gguf') as string
        const language = await getSetting('localAsr.language', 'auto') as string
        const r = await invoke<{ text: string; elapsed_ms: number; model_id: string }>('run_asr_benchmark', {
          modelId, language,
        })
        setResult({ text: r.text, asrMs: r.elapsed_ms, mode: 'local', model: r.model_id, audioDurationSec })
      } else if (workMode === 'cloud_api') {
        let pcmB64 = ''
        const chunk = 8192
        for (let i = 0; i < pcmBytes.length; i += chunk) {
          const slice = pcmBytes.subarray(i, Math.min(i + chunk, pcmBytes.length))
          pcmB64 += String.fromCharCode(...slice)
        }
        pcmB64 = btoa(pcmB64)

        const asrProvider = await getSetting('cloudAsr.provider', 'doubao_v2') as string
        const asrApiKey = await getSetting('cloudAsr.apiKey', '') as string
        const asrAppId = await getSetting('cloudAsr.appId', '') as string
        const asrModel = await getSetting('cloudAsr.model', '') as string

        // 模型解析一律走 @/lib/asrModels。這裡原來自己抄了一份對映表，還留著幾個
        // 已經不在 ASR_PROVIDERS 裡的舊 key——測試可能用與實際配置不同的模型。
        const isOmni = isQwenOmniProvider(asrProvider)
        const savedPrompt = isOmni
          ? await getSetting('cloudAsr.omniSystemPrompt', '') as string
          : ''
        // baseUrl 只有「地址自己填」那兩張卡是非空的，其餘一律空串（見 asrEndpointUrl）
        const baseUrl = await getSetting('cloudAsr.baseUrl', '') as string
        const protocol = await getSetting('cloudAsr.protocol', 'auto') as string
        const extra = buildAsrExtra(asrProvider, {
          model: asrModel,
          instructions: savedPrompt,
          baseUrl,
          protocol,
        })

        const start = performance.now()
        const r = await invoke<{ text: string; elapsed_ms: number }>('cloud_transcribe', {
          request: {
            audio_b64: pcmB64,
            sample_rate: 16000,
            asr_config: {
              provider: isOmni ? 'qwen_omni' : asrProvider,
              api_key: asrApiKey,
              app_id: asrAppId,
              ...(extra && { extra }),
            },
          },
        })
        const totalMs = Math.round(performance.now() - start)
        setResult({
          text: r.text,
          asrMs: totalMs,
          mode: 'cloud_api',
          // 報的必須是**真正發出去的**那個模型，否則這個面板就成了誤導源
          model: extra?.model || resolveAsrDisplayModel(asrProvider),
          audioDurationSec,
        })
      } else {
        // 伺服器模式
        const { getWSUrl } = await import('@/services/runtimeConfig')
        const { getClientRuntimeInfo } = await import('@/services/bridge')
        const wsUrl = getWSUrl()
        const clientMeta = await getClientRuntimeInfo().catch(() => null)

        const r = await new Promise<{ text: string; asrMs: number }>((resolve, reject) => {
          const timeout = setTimeout(() => { try { sock.close() } catch { } reject(new Error(t('asrTest.timeout'))) }, 30000)
          const sock = new WebSocket(wsUrl)
          sock.binaryType = 'arraybuffer'
          sock.onopen = () => {
            // 服務端准入（guard admission）要求 start 帶 device_id / client_version、source 只認
            // live / history_reprocess，否則回「連線異常，請使用最新版 SayIt 客戶端」並斷開。
            // 用 history_reprocess 而不是 live：測試音訊是一次性推完的，live 會被推流檢測攔，
            // stop 也得帶按鍵時長；這和歷史記錄重新識別是同一種請求，按同一條道走。
            const startMsg: Record<string, unknown> = { cmd: 'start', source: 'history_reprocess', disable_ai: true }
            if (clientMeta) {
              startMsg.client_meta = {
                user_id: clientMeta.userId,
                device_id: clientMeta.deviceId,
                client_version: clientMeta.clientVersion,
                platform: clientMeta.platform,
                os_version: clientMeta.osVersion,
              }
            }
            sock.send(JSON.stringify(startMsg))
            // 分塊傳送 PCM（每塊 3200 位元組 = 100ms @16kHz 16bit mono）
            const chunkSize = 3200
            for (let i = 0; i < pcmBytes.length; i += chunkSize) {
              sock.send(pcmBytes.slice(i, i + chunkSize).buffer)
            }
            sock.send(JSON.stringify({ cmd: 'stop' }))
          }
          sock.onmessage = (e) => {
            if (typeof e.data !== 'string') return
            try {
              const msg = JSON.parse(e.data)
              if (msg.type === 'final') {
                clearTimeout(timeout)
                resolve({ text: msg.asr_text || '', asrMs: msg.asr_ms || 0 })
                sock.close()
              } else if (msg.type === 'error') {
                clearTimeout(timeout)
                reject(new Error(msg.message || t('asrTest.serverError')))
                sock.close()
              }
            } catch { }
          }
          sock.onerror = () => { clearTimeout(timeout); reject(new Error(t('asrTest.wsFailed'))) }
        })

        setResult({ text: r.text, asrMs: r.asrMs, mode: 'server', model: '', audioDurationSec })
      }
    } catch (err) {
      const friendly = workMode === 'server'
        ? describeServerError(err, true)
        : describeProviderError(err)
      setError({ message: friendly.message, detail: friendly.detail })
    } finally {
      setTesting(false)
    }
  }

  return (
    <Card>
      <CardContent className="p-6">
        {/* flex-wrap：最小視窗下標題 + 兩個按鈕擠一行會溢位 */}
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold">{t('asrTest.title')}</h2>
            {/* 這張卡只測 ASR。伺服器模式下請求裡寫著 disable_ai: true，本地和雲 API 也
                不經過 AI 整理——原來介面上還渲染一個恆為 0 的「LLM 0ms」，會讓人以為
                AI 整理壞了。現在把範圍說清，並不再展示那個假欄位。 */}
            <p className="mt-1 text-xs text-muted-foreground">
              {t('asrTest.desc')}
            </p>
          </div>
          <div className="flex shrink-0 gap-2">
            <Button size="sm" variant="outline" className="gap-1.5" onClick={() => void handlePlay()}>
              {playing ? <Pause className="h-3.5 w-3.5" aria-hidden /> : <Play className="h-3.5 w-3.5" aria-hidden />}
              {playing ? t('asrTest.pause') : t('asrTest.play')}
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => void handleTest()}
              disabled={testing || draftDirty}
            >
              {testing ? t('asrTest.testing') : t('asrTest.start')}
            </Button>
          </div>
        </div>

        {/* 測試讀的是已儲存的配置。粘完金鑰直接點測試必然失敗，使用者會誤判成"金鑰是壞的"。 */}
        {draftDirty && (
          <Feedback
            className="mt-4"
            tone="warning"
            message={t('asrTest.unsavedWarning')}
          />
        )}

        {error && (
          <Feedback
            className="mt-4"
            tone="error"
            message={t('asrTest.failed', { message: error.message })}
            detail={error.detail}
            actions={[{ label: t('common.retry'), onClick: () => void handleTest(), disabled: testing }]}
          />
        )}

        {result && (
          <div className="mt-4 rounded-lg border border-border bg-muted/30 p-3">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
              <span className="rounded bg-primary/10 px-2 py-0.5 text-primary">
                {t(result.mode === 'local' ? 'asrTest.modeLocal' : result.mode === 'cloud_api' ? 'asrTest.modeCloudApi' : 'asrTest.modeServer')}
              </span>
              <span className="rounded bg-muted px-2 py-0.5">{result.mode === 'server' ? t('asrTest.serverModel') : result.model}</span>
              <span>{t('asrTest.audioLen', { sec: result.audioDurationSec.toFixed(1) })}</span>
              <span>{t('asrTest.elapsed', { ms: result.asrMs })}</span>
            </div>
            {/* mac-selectable：mac 上介面文字預設不可選，識別結果要能選中複製（index.css） */}
            <p className="mac-selectable mt-2 text-sm text-foreground">{result.text || t('asrTest.noResult')}</p>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
