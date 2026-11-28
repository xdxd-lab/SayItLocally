// 本地模式配置面板 — 模型管理

import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import { FolderOpen, Copy, Check, ChevronDown, HardDrive, Loader2, Info } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/tooltip'
import { Feedback } from '@/components/ui/feedback'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { Modal } from '@/components/ui/modal'
import { getSetting, setSetting } from '@/services/store'
import { refreshModeStatus } from '@/stores/modeStatus'
import { reconnectProvider } from '@/services/recorder'
import { describeDownloadError, describeModelsDirError } from '@/lib/errorMessages'
import { getLocale, t } from '@/i18n'
import { IS_MAC } from '@/lib/platform'
import { useT } from '@/i18n/useT'
import {
  localModelDisplayDescription,
  localModelDisplayLanguages,
  localModelDisplayName,
} from '@/i18n/displayNames'

/** 模型儲存位置變更的視窗事件：次級設定卡片（LocalModeAdvancedSection）裡改了
 *  目錄後，通知模型列表卡片重新整理已下載狀態——兩個卡片各自持有狀態、不在同一元件樹。 */
const MODELS_DIR_CHANGED_EVENT = 'sayit:models-dir-changed'

function formatList(items: string[]): string {
  return items.join(t('common.listSeparator'))
}

interface ModelFile {
  name: string
  url: string
  size_bytes: number
  sha256: string | null
}

interface DownloadSource {
  source: string
  files: ModelFile[]
}

interface ModelInfo {
  id: string
  name: string
  description: string
  model_type: string
  total_size_bytes: number
  languages: string[]
  sources: DownloadSource[]
  speed?: number
  accuracy?: number
  recommended?: boolean
  memory_mb?: number
  languages_label?: string
  quant?: string
  featured?: boolean
}

interface LocalModelInfo {
  id: string
  name: string
  model_type: string
  total_size_bytes: number
  path: string
  complete: boolean
}

interface DownloadProgress {
  model_id: string
  file_name: string
  downloaded_bytes: number
  total_bytes: number
  percent: number
  file_index: number
  file_count: number
  status: string
  error: string | null
}

interface ModelsDirInfo {
  current: string
  default_dir: string
  is_custom: boolean
}

// 本地 GGUF 引擎的診斷資訊（Rust 命令 gguf_asr_diagnostics）
interface GgufDevice {
  kind: string
  name: string
  memory_mb: number
  /** 持久化用的穩定標識（PCI 匯流排 id，拿不到時是 "kind:name"）。設定裡存這個。 */
  id: string
  /** ggml registry 索引。只用於排序展示，別拿它當持久化標識——驅動更新會讓它變。 */
  index: number
  is_gpu: boolean
}

interface GgufDiagnostics {
  devices: GgufDevice[]
  current_backend: string | null
  /** 實際繫結的裝置描述。與使用者選的那張可能不同（詳見 Rust 側 resolve_gpu_device），
   *  所以"當前使用"一行顯示的必須是這個，不是設定值。 */
  current_device: string | null
  /** 正在載入中的模型 id。非 null 時 current_backend 一定是 null。 */
  loading_model: string | null
  native_version: string
  process_memory_mb: number
}

/** 顯示卡名裡的商標噪音（(R) / (TM)）去掉，列表裡已經夠長了。 */
function cleanDeviceName(name: string): string {
  return name.replace(/\((R|TM)\)/gi, '').trim()
}

/** 顯示卡標籤：名字 +「（N GB 視訊記憶體）」。mac 的 Apple 晶片是統一記憶體，沒有視訊記憶體可報，只寫名字。 */
function gpuLabel(d: Pick<GgufDevice, 'name' | 'memory_mb'>): string {
  const vram = !IS_MAC && d.memory_mb > 0 ? t('local.vram', { gb: (d.memory_mb / 1024).toFixed(0) }) : ''
  return `${cleanDeviceName(d.name)}${vram}`
}

function formatSize(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(0)} MB`
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`
}

/** 常駐記憶體的展示：350 → "~350 MB"，1400 → "~1.4 GB"。
 *  統一按 1024 進位制、統一寫「GB/MB」——同一頁原來出現過 /1000 的「~1.4G」、
 *  /1024³ 的「1.4 GB」和 /1024 的「8 GB」三種口徑，使用者沒法橫向比。 */
function formatMemory(mb: number): string {
  if (mb < 1024) return `~${mb} MB`
  return `~${(mb / 1024).toFixed(1)} GB`
}

/** 下載源的展示名。同一個源在模型卡里叫「HF Mirror（國內推薦）」、在離線指引裡叫
 *  「HF Mirror (China)」，收斂到一處。 */
function sourceLabel(source: string): string {
  return source === 'HuggingFace Mirror' ? t('local.source.mirror') : source
}

/** 速度/準確度評級：10 分制細柱狀條（無數字），與引數排同一行 */
function MiniRating({ label, value }: { label: string; value: number }) {
  const pct = Math.max(0, Math.min(100, (value / 10) * 100))
  return (
    <span className="inline-flex items-center gap-1.5">
      <span>{label}</span>
      <span className="h-1.5 w-14 overflow-hidden rounded-full bg-muted">
        <span className="block h-full rounded-full bg-foreground/60" style={{ width: `${pct}%` }} />
      </span>
    </span>
  )
}

function CopyLink({ url, label }: { url: string; label: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="rounded-md bg-muted/30 px-3 py-2">
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-foreground">{label}</span>
        {/* 純圖示按鈕必須自帶 aria-label：Tooltip 只響應滑鼠懸停，鍵盤和讀屏使用者拿不到它 */}
        <Tooltip content={copied ? t('record.copied') : t('common.copyLink')}>
          <button
            type="button"
            aria-label={copied ? t('common.copiedLink', { label }) : t('common.copyLinkAria', { label })}
            onClick={() => {
              void navigator.clipboard.writeText(url)
              setCopied(true)
              setTimeout(() => setCopied(false), 1500)
            }}
            className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            {copied
              ? <Check className="h-3.5 w-3.5 text-success-strong" aria-hidden />
              : <Copy className="h-3.5 w-3.5" aria-hidden />}
          </button>
        </Tooltip>
      </div>
      <code className="mt-1 block select-all break-all text-[11px] leading-relaxed text-muted-foreground">{url}</code>
    </div>
  )
}

function OfflineGuideDialog({ models, onClose }: { models: ModelInfo[]; onClose: () => void }) {
  // 收集所有源名稱
  const sourceNames = models[0]?.sources.map((s) => s.source) || []
  // [自訂] S56：預設選官方 HuggingFace，不依賴 catalog 順序（原：useState(0)，當時第一個是鏡像）
  const [selectedSource, setSelectedSource] = useState(() => Math.max(0, sourceNames.indexOf('HuggingFace')))

  return (
    <Modal title={t('local.offlineGuideTitle')} onClose={onClose} showCloseButton panelClassName="w-[640px]">
      <>
        {/* 步驟 */}
        <div className="mt-3 space-y-1 text-xs text-muted-foreground">
          <p>{t('local.offlineStep1')}</p>
          <p>{t('local.offlineStep2')}</p>
          <p>{t('local.offlineStep3')}</p>
        </div>

        {/* 源切換 */}
        <div role="radiogroup" aria-label={t('local.downloadSourceAria')} className="mt-4 flex gap-1 rounded-lg border border-border p-0.5">
          {sourceNames.map((name, i) => (
            <button
              key={name}
              type="button"
              role="radio"
              aria-checked={selectedSource === i}
              onClick={() => setSelectedSource(i)}
              className={`flex-1 rounded-md px-2 py-1.5 text-xs transition-colors ${selectedSource === i
                ? 'bg-accent font-medium text-foreground'
                : 'text-muted-foreground hover:text-foreground'
                }`}
            >
              {sourceLabel(name)}
            </button>
          ))}
        </div>

        {/* 模型檔案連結 */}
        <div className="mt-4 space-y-4">
          {models.map((model) => {
            const modelName = localModelDisplayName(model)
            // 某模型沒有當前標籤對應的源時，回退到它的第一個源（防禦，正常不會發生：
            // catalog 的測試保證所有模型提供同一組源）
            const source = model.sources[selectedSource] ?? model.sources[0]
            if (!source) return null
            return (
              <div key={model.id}>
                <div className="mb-2 flex items-center gap-2">
                  <span className="text-sm font-medium">{modelName}</span>
                  <code className="rounded bg-muted/50 px-1.5 py-0.5 text-xs text-muted-foreground">{model.id}/</code>
                  <Tooltip content={t('local.openModelFolder')}>
                    <button
                      type="button"
                      aria-label={t('local.openModelFolderAria', { name: modelName })}
                      onClick={() => void invoke<string>('open_model_folder', { modelId: model.id })}
                      className="rounded p-1 text-muted-foreground transition-colors hover:text-foreground"
                    >
                      <FolderOpen className="h-3.5 w-3.5" aria-hidden />
                    </button>
                  </Tooltip>
                </div>
                <div className="space-y-1.5">
                  {source.files.map((file) => (
                    <CopyLink key={file.name} url={file.url} label={file.name} />
                  ))}
                </div>
              </div>
            )
          })}
        </div>
      </>
    </Modal>
  )
}

/** 模型儲存位置：檢視 / 更改 / 恢復預設。舊檔案保留在原目錄。 */
function ModelsDirSection({ onChanged }: { onChanged: () => void }) {
  const [info, setInfo] = useState<ModelsDirInfo | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<{ message: string; detail: string } | null>(null)
  const [unchanged, setUnchanged] = useState(false)
  // 待確認的目錄變更：null=無。dir=null 表示恢復預設。
  const [pending, setPending] = useState<{ dir: string | null } | null>(null)

  const load = async () => {
    try { setInfo(await invoke<ModelsDirInfo>('get_models_dir')) } catch { /* ignore */ }
  }
  useEffect(() => { void load() }, [])

  async function pickDir() {
    setError(null)
    setUnchanged(false)
    try {
      const selected = await open({ directory: true, multiple: false, title: t('local.pickDirTitle') })
      if (typeof selected !== 'string') return // 取消
      if (info && selected === info.current) { setUnchanged(true); return }
      setPending({ dir: selected })
    } catch (err) {
      setError(describeModelsDirError(err))
    }
  }

  function requestResetDefault() {
    if (!info || !info.is_custom) return
    setError(null)
    setUnchanged(false)
    setPending({ dir: null })
  }

  async function applyChange() {
    if (!pending) return
    setBusy(true)
    setError(null)
    setUnchanged(false)
    try {
      const effective = await invoke<string>('set_models_dir', { dir: pending.dir })
      setPending(null)
      await load()
      if (pending.dir !== null && effective === info?.current) setUnchanged(true)
      else onChanged()
    } catch (err) {
      setPending(null)
      setError(describeModelsDirError(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Card>
      <CardContent className="p-6">
        <div className="mb-2 flex items-center gap-2">
          <HardDrive className="h-4 w-4 text-muted-foreground" />
          <h2 className="text-base font-semibold">{t('local.storageTitle')}</h2>
          <Tooltip variant="light" content={t('local.storageHelp')}>
            <Info className="h-3.5 w-3.5 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground" />
          </Tooltip>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <div className="flex min-w-[12rem] flex-1 items-center gap-1.5 rounded-md bg-muted/30 px-3 py-2">
            <code className="min-w-0 flex-1 select-all truncate text-xs text-muted-foreground" title={info?.current}>
              {info?.current || t('common.loading')}
            </code>
            {info?.is_custom && (
              <span className="shrink-0 rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">{t('local.storageCustom')}</span>
            )}
            <Tooltip content={t('local.openDir')}>
              <button
                type="button"
                aria-label={t('local.openDirAria')}
                onClick={() => void invoke<string>('open_models_folder')}
                className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              >
                <FolderOpen className="h-3.5 w-3.5" aria-hidden />
              </button>
            </Tooltip>
          </div>
          <Button size="sm" variant="outline" onClick={() => void pickDir()} disabled={busy}>
            {t('local.changeLocation')}
          </Button>
          {info?.is_custom && (
            <Button size="sm" variant="ghost" onClick={requestResetDefault} disabled={busy}>
              {t('local.restoreDefault')}
            </Button>
          )}
        </div>

        {error && <Feedback className="mt-3" tone="error" message={error.message} detail={error.detail} />}
        {unchanged && <Feedback className="mt-3" tone="info" message={t('local.dirUnchanged')} />}
      </CardContent>

      {pending && (
        <Modal
          title={pending.dir === null ? t('local.restoreDefaultTitle') : t('local.changeLocationTitle')}
          onClose={() => setPending(null)}
          locked={busy}
          panelClassName="w-[420px]"
        >
          <>
            <p className="mt-2 break-all text-sm text-muted-foreground">
              {t('local.newLocation', { dir: pending.dir === null ? (info?.default_dir || t('local.defaultDir')) : pending.dir })}
            </p>
            <p className="mt-3 text-xs text-muted-foreground">
              {t('local.migrateNote')}
            </p>
            <div className="mt-5 flex justify-end gap-2">
              <Button size="sm" variant="outline" onClick={() => setPending(null)} disabled={busy}>{t('common.cancel')}</Button>
              <Button size="sm" onClick={() => void applyChange()} disabled={busy}>
                {busy ? (<><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" aria-hidden />{t('common.processing')}</>) : t('common.confirm')}
              </Button>
            </div>
          </>
        </Modal>
      )}
    </Card>
  )
}

export default function LocalModeSection() {
  useT()
  const [availableModels, setAvailableModels] = useState<ModelInfo[]>([])
  const [downloadedModels, setDownloadedModels] = useState<LocalModelInfo[]>([])
  const [selectedModelId, setSelectedModelId] = useState('')
  // [自訂] S25：初始值也固定官方 huggingface.co（原依介面語言決定）
  const [downloadSource, setDownloadSource] = useState('HuggingFace')
  const [preloadingModelId, setPreloadingModelId] = useState('')
  const [downloading, setDownloading] = useState<Record<string, DownloadProgress>>({})
  // 「更多模型」摺疊。預設只展示 featured 的小/中/大三個，其餘點開才看到。
  const [showMore, setShowMore] = useState(false)
  // 模型清單的載入狀態。原來 loadData 的 catch 是空的，list_available_models 失敗時
  // 頁面只剩標題 + 下載源一行 + 下面一片空白，看起來像"本地模式壞了"。
  const [listState, setListState] = useState<'loading' | 'ready' | 'error'>('loading')
  const [listError, setListError] = useState('')
  // 離線下載指引的開關提到這一層：下載失敗時也要能把使用者直接送進去
  const [offlineGuideOpen, setOfflineGuideOpen] = useState(false)
  // 檢測到的 GPU 摘要。選模型時最該知道的就是"這台機器什麼水平"，
  // 而這條資訊原來只出現在兩張卡片之後的「計算後端」裡。
  const [gpuSummary, setGpuSummary] = useState<string | null>(null)

  useEffect(() => {
    void loadData()
    const unlisten = listen<DownloadProgress>('model-download-progress', (event) => {
      const p = event.payload
      setDownloading((prev) => ({ ...prev, [p.model_id]: p }))
      if (p.status === 'completed' || p.status === 'failed') {
        void refreshDownloaded()
      }
    })
    // 模型儲存位置（在次級設定卡片裡）變更後，重新整理已下載列表
    const onDirChanged = () => { void refreshDownloaded() }
    window.addEventListener(MODELS_DIR_CHANGED_EVENT, onDirChanged)
    return () => {
      void unlisten.then((fn) => fn())
      window.removeEventListener(MODELS_DIR_CHANGED_EVENT, onDirChanged)
    }
  }, [])

  async function loadData() {
    let available: ModelInfo[] = []
    try {
      const [a, downloaded] = await Promise.all([
        invoke<ModelInfo[]>('list_available_models'),
        invoke<LocalModelInfo[]>('list_downloaded_models'),
      ])
      available = a
      setAvailableModels(a)
      setDownloadedModels(downloaded)
      setListState('ready')
      setListError('')
    } catch (err) {
      setListState('error')
      setListError(String(err))
    }

    // GPU 摘要：只為在模型列表頂部給一句硬體背景，失敗就當沒有（不影響選模型）
    try {
      const diag = await invoke<GgufDiagnostics>('gguf_asr_diagnostics')
      const gpus = diag.devices.filter((d) => d.kind !== 'cpu')
      setGpuSummary(gpus.length > 0
        ? formatList(gpus
          .map((d) => gpuLabel(d)))
        : '')
    } catch {
      setGpuSummary(null)
    }

    const selected = await getSetting('localAsr.modelId', 'sensevoice-small-gguf') as string
    setSelectedModelId(selected)
    // [自訂] S25：不論介面語言，預設官方 huggingface.co
    // 原：const defaultSource = getLocale() === 'en' ? 'HuggingFace' : 'HuggingFace Mirror'
    const defaultSource = 'HuggingFace'
    setDownloadSource(await getSetting('localAsr.downloadSource', defaultSource) as string)

    // 當前選中的模型在摺疊區時自動展開，避免"當前模型在列表裡找不到"
    const selectedInfo = available.find((m) => m.id === selected)
    if (selectedInfo && !selectedInfo.featured) setShowMore(true)
  }

  async function refreshDownloaded() {
    try {
      const downloaded = await invoke<LocalModelInfo[]>('list_downloaded_models')
      setDownloadedModels(downloaded)
    } catch { /* ignore */ }
  }

  // [自訂] S57：可明確指定來源。retryWithOtherSource 呼叫時 setDownloadSource 尚未生效，
  // 閉包裡的 downloadSource 仍是舊值，原本會用失敗的同一來源重試。
  async function handleDownload(modelId: string, source: string = downloadSource) {
    try {
      await invoke('download_model', { modelId, source })
      // 下載完成後自動選中並預載入
      setSelectedModelId(modelId)
      await setSetting('localAsr.modelId', modelId)
      void refreshModeStatus() // 同步左下角的引擎指示
      try {
        const accelerator = await getSetting('localAsr.accelerator', 'auto') as string
        const gpuDevice = await getSetting('localAsr.gpuDevice', '') as string
        await invoke<string>('preload_local_model', { modelId, accelerator, gpuDevice })
      } catch { /* ignore */ }
      // provider 快取著上次的就緒結果，不重連的話剛下載完第一次按快捷鍵仍會被判未就緒。
      // 同 handleSelectModel：排在預載入之後，避免它的 onConnect 再排一輪載入。
      reconnectProvider()
    } catch (err) {
      setDownloading((prev) => ({
        ...prev,
        [modelId]: {
          ...prev[modelId],
          model_id: modelId,
          file_name: '',
          downloaded_bytes: 0,
          total_bytes: 0,
          percent: 0,
          status: 'failed',
          error: String(err),
        },
      }))
    }
  }

  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null)

  async function handleDelete(modelId: string) {
    setConfirmDeleteId(null)
    try {
      await invoke('delete_model', { modelId })
      await refreshDownloaded()
      setDownloading((prev) => {
        const next = { ...prev }
        delete next[modelId]
        return next
      })
      // 刪掉的可能正是當前選中的模型 → 就緒狀態變了，通知徽標與側欄指示
      void refreshModeStatus()
      // 同時讓 provider 重新判定：否則它仍快取著"已就緒"，模型都刪了還能照常開錄
      reconnectProvider()
    } catch { /* ignore */ }
  }

  /** 切到另一個下載源並立刻重試。下載失敗時最常見的下一步就是這個。 */
  async function retryWithOtherSource(modelId: string) {
    const options = availableModels[0]?.sources.map((s) => s.source) ?? []
    const next = options.find((s) => s !== downloadSource) ?? downloadSource
    setDownloadSource(next)
    await setSetting('localAsr.downloadSource', next)
    setDownloading((prev) => {
      const rest = { ...prev }
      delete rest[modelId]
      return rest
    })
    await handleDownload(modelId, next) // [自訂] S57：原為 handleDownload(modelId)，會讀到舊來源
  }

  async function handleSelectModel(modelId: string) {
    if (preloadingModelId) return // 防止切換/載入中重複觸發
    setSelectedModelId(modelId)
    setPreloadingModelId(modelId)
    await setSetting('localAsr.modelId', modelId)
    void refreshModeStatus() // 同步左下角的引擎指示
    try {
      const accelerator = await getSetting('localAsr.accelerator', 'auto') as string
      const gpuDevice = await getSetting('localAsr.gpuDevice', '') as string
      await invoke<string>('preload_local_model', { modelId, accelerator, gpuDevice })
    } catch { /* 未下載 / 載入失敗都由就緒判定與識別階段報出，這裡不打斷選擇 */ } finally {
      setPreloadingModelId('')
      // reconnectProvider 必須排在預載入**之後**：它的 onConnect 自己也會發一次
      // preload_local_model，而引擎的載入是全域性單鎖序列的。放在前面（原來的寫法）
      // 等於一次點選排兩輪"卸舊 + 載新"，等待時間可能翻倍。
      // 放在後面，它只會命中已經載入好的快取，立刻返回。
      // 未下載的模型走的是同一條路：預載入報錯後仍然要 reconnect 讓 provider
      // 重新判定為未就緒，所以這行放在 finally 裡而不是 try 的末尾。
      reconnectProvider()
    }
  }

  const downloadedIds = new Set(downloadedModels.filter((m) => m.complete).map((m) => m.id))

  // 小/中/大三個直接展示，其餘摺疊進「更多」。後端沒標 featured 時全部展示兜底。
  const featuredModels = availableModels.some((m) => m.featured)
    ? availableModels.filter((m) => m.featured)
    : availableModels
  const moreModels = availableModels.filter((m) => !featuredModels.includes(m))
  const visibleModels = showMore ? [...featuredModels, ...moreModels] : featuredModels

  // 下載源按鈕從 catalog 生成，保證和後端提供的源一一對應
  // （catalog 的測試保證了所有模型的源集合一致，取第一個模型的即可）
  const sourceOptions = availableModels[0]?.sources.map((s) => s.source) ?? []
  // 儲存裡的舊值（如已下線的 ModelScope）對不上任何源時，實際下載會回落到
  // 第一個源，這裡讓 UI 顯示和實際行為一致
  const effectiveSource = sourceOptions.includes(downloadSource)
    ? downloadSource
    : sourceOptions[0] ?? downloadSource

  return (
    <>
      <Card>
        <CardContent className="p-6">
          <div className="mb-4 flex items-center justify-between gap-3">
            <h2 className="text-lg font-semibold">{t('local.modelTitle')}</h2>
            <Tooltip content={t('local.openModelDir')}>
              <button
                type="button"
                aria-label={t('local.openModelDir')}
                onClick={() => void invoke<string>('open_models_folder')}
                className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              >
                <FolderOpen className="h-3.5 w-3.5" aria-hidden />
              </button>
            </Tooltip>
          </div>

          {selectedModelId && downloadedIds.has(selectedModelId) && (
            <p className="mb-2 text-sm text-muted-foreground">
              {t('local.currentModel', { name: availableModels.find((m) => m.id === selectedModelId)?.name || selectedModelId })}
            </p>
          )}
          {selectedModelId && !downloadedIds.has(selectedModelId) && listState === 'ready' && (
            <Feedback
              className="mb-3"
              tone="warning"
              message={t('local.notDownloadedWarning')}
            />
          )}

          {/* 硬體背景。選模型是這一頁最難的決定，而"這台機器什麼水平"這條資訊
              原來要往下翻兩張卡才看得到。 */}
          {gpuSummary !== null && (
            <p className="mb-4 text-xs text-muted-foreground">
              {gpuSummary
                ? t(IS_MAC ? 'local.gpuDetectedMac' : 'local.gpuDetected', { gpu: gpuSummary })
                : t('local.noGpu')}
            </p>
          )}

          <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
            <span id="download-source-label" className="text-sm text-muted-foreground">{t('local.downloadSource')}</span>
            <Segmented
              labelledBy="download-source-label"
              size="sm"
              value={effectiveSource}
              options={sourceOptions.map((src) => ({ value: src, label: sourceLabel(src) }))}
              onChange={(src) => { setDownloadSource(src); void setSetting('localAsr.downloadSource', src) }}
              className="shrink-0 justify-end"
            />
          </div>

          {listState === 'loading' && (
            <p className="py-4 text-sm text-muted-foreground">{t('local.loadingCatalog')}</p>
          )}
          {listState === 'error' && (
            <Feedback
              tone="error"
              message={t('local.catalogError')}
              detail={listError}
              actions={[{ label: t('local.reload'), onClick: () => void loadData() }]}
            />
          )}
          {listState === 'ready' && availableModels.length === 0 && (
            <Feedback
              tone="warning"
              message={t('local.catalogEmpty')}
            />
          )}

          <div className="space-y-2">
            {visibleModels.map((model) => {
              const modelName = localModelDisplayName(model)
              const modelDescription = localModelDisplayDescription(model)
              const modelLanguages = localModelDisplayLanguages(model)
              const isDownloaded = downloadedIds.has(model.id)
              const isSelected = selectedModelId === model.id
              const progress = downloading[model.id]
              const isDownloading = progress?.status === 'downloading'

              return (
                <div
                  key={model.id}
                  className={`flex items-center justify-between rounded-lg border p-3 ${isSelected ? 'border-primary bg-primary/5' : 'border-border'
                    }`}
                >
                  <div className="flex-1">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">{modelName}</span>
                      {model.recommended && (
                        <span className="rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">{t('local.recommended')}</span>
                      )}
                      {isDownloaded && (
                        <span className="flex items-center gap-1 text-xs text-muted-foreground">
                          <Check className="h-3 w-3" />{t('local.downloaded')}
                        </span>
                      )}
                    </div>
                    <p className="mt-0.5 text-xs text-muted-foreground">{modelDescription}</p>
                    {/* 引數行：評分柱狀條 + 下載體積（硬碟圖示）+ 記憶體佔用 + 語種，一行小字。
                        量化檔（Q8_0 之類）不展示——普通使用者看不懂，檔名裡查得到 */}
                    <div className="mt-2 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-xs text-muted-foreground">
                      {model.speed ? <MiniRating label={t('local.speed')} value={model.speed} /> : null}
                      {model.accuracy ? <MiniRating label={t('local.accuracy')} value={model.accuracy} /> : null}
                      {model.total_size_bytes > 0 && (
                        <span className="flex items-center gap-1">
                          <HardDrive className="h-3 w-3" />
                          {formatSize(model.total_size_bytes)}
                        </span>
                      )}
                      {model.memory_mb ? (
                        <><span aria-hidden>·</span><span>{t('local.memoryUsage', { size: formatMemory(model.memory_mb) })}</span></>
                      ) : null}
                      {modelLanguages ? (
                        <><span aria-hidden>·</span><span>{modelLanguages}</span></>
                      ) : null}
                    </div>
                    {isDownloading && progress && (
                      <div className="mt-2">
                        <div
                          role="progressbar"
                          aria-label={t('local.downloadingAria', { name: modelName })}
                          aria-valuenow={Math.round(progress.percent)}
                          aria-valuemin={0}
                          aria-valuemax={100}
                          className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
                        >
                          <div
                            className="h-full rounded-full bg-primary transition-all"
                            style={{ width: `${progress.percent}%` }}
                          />
                        </div>
                        {/* aria-live：下載過去對讀屏使用者是完全靜默的 */}
                        <p className="mt-1 text-xs text-muted-foreground" aria-live="polite">
                          {progress.file_count > 1
                            ? t('local.fileProgress', { index: progress.file_index, count: progress.file_count })
                            : ''}
                          {progress.file_name} — {progress.percent.toFixed(1)}%
                          {progress.total_bytes > 0
                            ? t('local.downloadProgressBytes', {
                              downloaded: formatSize(progress.downloaded_bytes),
                              total: formatSize(progress.total_bytes),
                            })
                            : t('local.downloadedBytes', { downloaded: formatSize(progress.downloaded_bytes) })}
                        </p>
                      </div>
                    )}
                    {/* 下載失敗原來只有一行原始異常。而"換個源重試"和"手動下載指引"這兩條
                        降級路徑就在同一張卡上，失敗資訊卻不指向任何一個——使用者會以為
                        本地模式不能用。現在直接把兩個動作放進錯誤塊裡。 */}
                    {progress?.status === 'failed' && (() => {
                      const friendly = describeDownloadError(progress.error ?? '')
                      const hasOtherSource = sourceOptions.some((s) => s !== effectiveSource)
                      return (
                        <Feedback
                          className="mt-2"
                          tone="error"
                          message={friendly.message}
                          detail={friendly.detail}
                          actions={[
                            ...(friendly.action === 'switch_source' && hasOtherSource
                              ? [{
                                label: t('local.switchSourceRetry', { source: sourceLabel(sourceOptions.find((s) => s !== effectiveSource) ?? '') }),
                                onClick: () => void retryWithOtherSource(model.id),
                              }]
                              : [{ label: t('common.retry'), onClick: () => void handleDownload(model.id) }]),
                            { label: t('local.manualGuide'), onClick: () => setOfflineGuideOpen(true) },
                          ]}
                        />
                      )
                    })()}
                  </div>
                  <div className="ml-3 flex gap-2">
                    {isDownloaded ? (
                      <>
                        {!isSelected && (
                          <Button
                            size="sm" variant="outline"
                            disabled={preloadingModelId !== ''}
                            onClick={() => void handleSelectModel(model.id)}
                          >
                            {preloadingModelId === model.id ? t('local.loadingModel') : t('local.select')}
                          </Button>
                        )}
                        <Button size="sm" variant="ghost" onClick={() => setConfirmDeleteId(model.id)}>
                          {t('common.delete')}
                        </Button>
                      </>
                    ) : (
                      <Button
                        size="sm"
                        onClick={() => void handleDownload(model.id)}
                        disabled={isDownloading}
                      >
                        {isDownloading ? t('local.downloading') : t('local.download')}
                      </Button>
                    )}
                  </div>
                </div>
              )
            })}
          </div>

          {/* 載入一個模型要讀完整個權重檔案再預熱一次推理，大模型幾十秒也正常。
              原來這段只讓按鈕變灰，介面看起來就是卡死了 —— 0.1.4 有使用者因此強殺了程序。
              所以這條必須顯眼：配色沿用 Feedback 的 warning/error 寫法（5% 底 + 25% 描邊
              + strong 圖示色），但**用不停轉的圖示承擔"還在動"的資訊** —— 靜態色塊
              說不出"沒卡住"，轉圈能。不用綠色：綠在這套配色裡代表"成功/已完成"，
              載入中打綠燈會讓人以為已經好了。
              role=status + aria-live：這條狀態對讀屏使用者否則完全靜默。 */}
          {preloadingModelId && (
            <div
              role="status"
              aria-live="polite"
              className="mt-3 flex items-start gap-2 rounded-md border border-info/25 bg-info/5 px-3 py-2.5"
            >
              <Loader2 className="mt-0.5 h-4 w-4 shrink-0 animate-spin text-info-strong" aria-hidden />
              <p className="text-sm leading-relaxed text-foreground">
                {/* 顯式 {' '}：兩個表示式之間的換行會被 JSX 整個吃掉，
                    英文下就會粘成 "…Loading" 這種沒有空格的樣子 */}
                {t('local.preloading', { name: availableModels.find((m) => m.id === preloadingModelId)?.name || preloadingModelId })}
                {' '}
                {t('local.preloadingNote')}
              </p>
            </div>
          )}

          {/* 更多模型：小眾需求（更多語種 / 中間量化檔）摺疊收納 */}
          {moreModels.length > 0 && (
            <button
              type="button"
              aria-expanded={showMore}
              onClick={() => setShowMore(!showMore)}
              className="mt-3 flex w-full items-center justify-center gap-1 rounded-md border border-dashed border-border py-1.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              {showMore ? t('common.collapse') : t('local.moreModels', { count: moreModels.length })}
              <ChevronDown className={`h-3.5 w-3.5 transition-transform ${showMore ? 'rotate-180' : ''}`} aria-hidden />
            </button>
          )}

          {availableModels.length > 0 && (
            <button
              type="button"
              onClick={() => setOfflineGuideOpen(true)}
              className="mt-3 text-xs text-muted-foreground underline decoration-muted-foreground/40 underline-offset-2 transition-colors hover:text-foreground hover:decoration-foreground/60"
            >
              {t('local.slowDownloadHint')}
            </button>
          )}
        </CardContent>
      </Card>

      {offlineGuideOpen && (
        <OfflineGuideDialog models={availableModels} onClose={() => setOfflineGuideOpen(false)} />
      )}

      {/* 刪除確認對話方塊 */}
      {confirmDeleteId && (
        <Modal title={t('local.deleteModelTitle')} onClose={() => setConfirmDeleteId(null)} panelClassName="w-80">
          <>
            <p className="mt-2 text-sm text-muted-foreground">
              {t('local.deleteModelBody', { name: availableModels.find((m) => m.id === confirmDeleteId)?.name || confirmDeleteId })}
            </p>
            <div className="mt-5 flex justify-end gap-2">
              <Button size="sm" variant="outline" onClick={() => setConfirmDeleteId(null)}>{t('common.cancel')}</Button>
              <Button size="sm" variant="destructive" onClick={() => void handleDelete(confirmDeleteId)}>{t('common.delete')}</Button>
            </div>
          </>
        </Modal>
      )}
    </>
  )
}

/** 本地模式的次級設定：識別語言、計算後端、模型駐留、模型儲存位置。
 *  單獨一個匯出，由 VoiceEnginePage 放在「識別測試」之後——選模型是主操作，
 *  這些是偶爾動一次的，不該排在測試入口前面。 */
export function LocalModeAdvancedSection() {
  useT()
  const [asrLanguage, setAsrLanguage] = useState('auto')
  const [accelerator, setAccelerator] = useState('auto')
  /** GgufDevice.id，'' = 自動。只在多顯示卡機器上有介面，見下面 showGpuPicker。 */
  const [gpuDevice, setGpuDevice] = useState('')
  const [unloadIdleMinutes, setUnloadIdleMinutes] = useState(0)
  const [devices, setDevices] = useState<GgufDevice[]>([])
  const [currentBackend, setCurrentBackend] = useState<string | null>(null)
  const [currentDevice, setCurrentDevice] = useState<string | null>(null)
  // 開啟本頁時可能正好有一次載入在進行（最常見：本地模式啟動時的後台預熱）。
  // 那時後端還沒繫結，顯示"正在載入"比什麼都不顯示準確。
  const [loadingModel, setLoadingModel] = useState<string | null>(null)
  const [diagnosticsState, setDiagnosticsState] = useState<'loading' | 'ready' | 'error'>('loading')
  // 切換計算後端會就地過載當前模型（幾秒），期間禁掉按鈕防連點
  const [rebinding, setRebinding] = useState(false)

  /** 拉一次引擎實況。掛載時與換後端後都要用，別把這幾個 setState 抄兩份。 */
  async function refreshDiagnostics() {
    try {
      const diag = await invoke<GgufDiagnostics>('gguf_asr_diagnostics')
      setDevices(diag.devices)
      setCurrentBackend(diag.current_backend)
      setCurrentDevice(diag.current_device)
      setLoadingModel(diag.loading_model)
      setDiagnosticsState('ready')
    } catch {
      setDiagnosticsState('error')
    }
  }

  useEffect(() => {
    void (async () => {
      setAsrLanguage(await getSetting('localAsr.language', 'auto') as string)
      setAccelerator(await getSetting('localAsr.accelerator', 'auto') as string)
      setGpuDevice(await getSetting('localAsr.gpuDevice', '') as string)
      setUnloadIdleMinutes(Number(await getSetting('localAsr.unloadIdleMinutes', 0)) || 0)
      await refreshDiagnostics()
    })()
  }, [])

  const gpuDevices = devices.filter((d) => d.is_gpu)
  const hasGpu = gpuDevices.length > 0
  const gpuSummary = formatList(gpuDevices
    .map((d) => gpuLabel(d)))

  /** 只有真的有多張顯示卡才給這個選擇器。單卡機器上它永遠只有一個有意義的值，
   *  擺在設定裡只是多一個要讀懂的東西。CPU 後端下也藏起來——那時它不起作用。 */
  const showGpuPicker = gpuDevices.length > 1 && accelerator !== 'cpu'

  /** 切換計算後端。引擎按 (模型, 後端, 顯示卡) 快取，換任意一項都要過載模型——
   *  就地重新預載入當前模型，讓切換立刻生效而不是等下次口述。
   *  模型未下載時預載入會報錯，忽略即可（下載後會按新設定載入）。 */
  async function handleSelectAccelerator(value: string) {
    if (rebinding) return
    setAccelerator(value)
    await setSetting('localAsr.accelerator', value)
    await rebindEngine({ accelerator: value, gpuDevice })
  }

  /** 切換顯示卡。與換後端同一條路徑：設定落盤 + 就地過載。 */
  async function handleSelectGpuDevice(value: string) {
    if (rebinding) return
    setGpuDevice(value)
    await setSetting('localAsr.gpuDevice', value)
    await rebindEngine({ accelerator, gpuDevice: value })
  }

  /** 重新繫結引擎。兩個切換共用，別把這段抄兩份——漏掉 refreshDiagnostics 的那份
   *  會讓"當前使用"一行停在換卡之前的裝置上，看著像設定沒生效。 */
  async function rebindEngine(opts: { accelerator: string; gpuDevice: string }) {
    setRebinding(true)
    try {
      const modelId = await getSetting('localAsr.modelId', 'sensevoice-small-gguf') as string
      await invoke<string>('preload_local_model', {
        modelId,
        accelerator: opts.accelerator,
        gpuDevice: opts.gpuDevice,
      })
    } catch { /* ignore */ } finally {
      await refreshDiagnostics()
      setRebinding(false)
    }
  }

  async function handleSelectUnloadIdle(value: number) {
    setUnloadIdleMinutes(value)
    try {
      await setSetting('localAsr.unloadIdleMinutes', value)
      await invoke('set_local_model_idle_unload', { idleMinutes: value })
    } catch { /* 重啟後仍會從持久化設定讀取；即時更新失敗不影響識別 */ }
  }

  return (
    <>
      <Card>
        <CardContent className="p-6">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <h2 id="local-language-heading" className="text-lg font-semibold">{t('local.languageTitle')}</h2>
              <p className="mt-2 text-xs text-muted-foreground">{t('local.languageNote')}</p>
            </div>
            <Segmented
              labelledBy="local-language-heading"
              value={asrLanguage}
              options={[
                { value: 'auto', label: t('common.auto') },
                { value: 'zh', label: t('local.lang.zh') },
                { value: 'en', label: t('local.lang.en') },
              ]}
              onChange={(value) => { setAsrLanguage(value); void setSetting('localAsr.language', value) }}
              className="shrink-0 justify-end"
            />
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardContent className="p-6">
          <div className="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex min-w-0 items-center gap-3">
              <h2 id="accelerator-heading" className="text-lg font-semibold">{t('local.backendTitle')}</h2>
              <span className={`inline-flex shrink-0 items-center gap-1.5 rounded-full border px-2 py-1 text-xs ${diagnosticsState === 'ready' && hasGpu
                ? 'border-success/30 bg-success/10 text-success-strong'
                : 'border-border bg-muted/40 text-muted-foreground'
                }`}>
                <span className={`h-1.5 w-1.5 rounded-full ${diagnosticsState === 'ready' && hasGpu ? 'bg-success' : 'bg-muted-foreground'}`} aria-hidden />
                {diagnosticsState === 'loading' ? t('local.backendChecking') : diagnosticsState === 'error' ? t('local.backendCheckFailed') : hasGpu ? t('local.backendGpuReady') : t('local.backendCpuOnly')}
              </span>
            </div>
            <Segmented
              labelledBy="accelerator-heading"
              value={accelerator}
              disabled={rebinding}
              options={[
                { value: 'auto', label: t('common.auto') },
                { value: 'gpu', label: 'GPU' },
                { value: 'cpu', label: 'CPU' },
              ]}
              onChange={(value) => void handleSelectAccelerator(value)}
              className="shrink-0 justify-end"
            />
          </div>
          {diagnosticsState === 'ready' && hasGpu && (
            <p className="mt-2 text-xs text-muted-foreground">
              {gpuSummary}
              {/* 載入中就說載入中：後端是在模型載入時才繫結的，這期間 currentBackend
                  一定是空，原來這裡會什麼都不顯示，看著像檢測失敗。
                  多顯示卡機器上報裝置名而不是後端名——那時使用者真正想確認的是
                  "用的是哪張卡"，而 currentDevice 是實際繫結結果（可能與所選不同）。 */}
              {loadingModel
                ? t('local.backendLoadingModel')
                : showGpuPicker && currentDevice
                  ? t('local.backendCurrentDevice', { device: cleanDeviceName(currentDevice) })
                  : currentBackend
                    ? t('local.backendCurrent', { backend: currentBackend.toUpperCase() })
                    : ''}
            </p>
          )}
          <p className="mt-1.5 text-xs text-muted-foreground">
            {diagnosticsState === 'loading'
              ? t('local.backendHintLoading')
              : diagnosticsState === 'error'
                ? t('local.backendHintError')
                : hasGpu
                  ? t('local.backendHintGpu')
                  : t('local.backendHintCpu')}
          </p>

          {/* 多顯示卡才出現。單卡機器上這個下拉只有一個有意義的值，擺出來純屬噪音。 */}
          {showGpuPicker && (
            <div className="mt-4 border-t border-border pt-4">
              <label id="gpu-device-heading" className="mb-2 block text-sm text-foreground">
                {t('local.gpuDeviceTitle')}
              </label>
              <Select
                labelledBy="gpu-device-heading"
                value={gpuDevice}
                disabled={rebinding}
                onChange={(value) => void handleSelectGpuDevice(value)}
                options={[
                  { value: '', label: t('local.gpuDeviceAuto') },
                  ...gpuDevices.map((d) => ({
                    value: d.id,
                    label: gpuLabel(d),
                  })),
                ]}
                className="sm:max-w-md"
              />
              <p className="mt-2 text-xs text-muted-foreground">{t('local.gpuDeviceHint')}</p>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardContent className="p-6">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <h2 id="unload-idle-heading" className="text-lg font-semibold">{t('local.unloadTitle')}</h2>
              <p className="mt-1 text-xs text-muted-foreground">{t('local.unloadDesc')}</p>
              <p className="mt-2 text-xs text-muted-foreground">
                {unloadIdleMinutes === 0
                  ? t('local.unloadNever')
                  : t('local.unloadAfter', {
                    duration: unloadIdleMinutes === 60 ? t('local.unload.1h') : t('local.minutes', { count: unloadIdleMinutes }),
                  })}
              </p>
            </div>
            <Segmented
              labelledBy="unload-idle-heading"
              value={unloadIdleMinutes}
              options={[
                { value: 0, label: t('local.unload.never') },
                { value: 10, label: t('local.unload.10m') },
                { value: 30, label: t('local.unload.30m') },
                { value: 60, label: t('local.unload.1h') },
              ]}
              onChange={(value) => void handleSelectUnloadIdle(value)}
              className="shrink-0 justify-end"
            />
          </div>
        </CardContent>
      </Card>

      <ModelsDirSection onChanged={() => window.dispatchEvent(new Event(MODELS_DIR_CHANGED_EVENT))} />
    </>
  )
}
