// 文本格式規範開關 — 不依賴 AI 的客戶端文本處理

import { useEffect, useRef, useState } from 'react'
import { Switch } from '@/components/ui/switch'
import { Card, CardContent } from '@/components/ui/card'
import { InfoTip } from '@/components/ui/info-tip'
import { cn } from '@/lib/utils'
import {
  getTextPostProcessOptions,
  saveTextPostProcessOptions,
  APPLIES_WITH_AI,
  DEFAULT_POST_PROCESS,
  type TextPostProcessOptions,
} from '@/services/textPostProcess'
import { useAiEnabled, useAiEnabledReady } from '@/hooks/useAiEnabled'
import type { TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'

interface ToggleDef {
  key: keyof TextPostProcessOptions
  titleKey: TranslationKey
  /** 行內可見的簡短說明 */
  hintKey: TranslationKey
  /** hover 問號顯示的詳細說明 + 例子（換行用 \n） */
  detailKey: TranslationKey
}

/**
 * 存 key 而不是文案。
 *
 * 這裡的 detail 是「說明 + 例子 + AI 生效條件」三段拼起來的，所以拼接也放到渲染期，
 * 否則模組載入時就把當時的語言固化下來了。
 */
const TOGGLES: ToggleDef[] = [
  {
    key: 'autoSegment',
    titleKey: 'textFormat.autoSegment.title',
    hintKey: 'textFormat.autoSegment.hint',
    detailKey: 'textFormat.autoSegment.help',
  },
  {
    key: 'normalizeNumbers',
    titleKey: 'textFormat.normalizeNumbers.title',
    hintKey: 'textFormat.normalizeNumbers.hint',
    detailKey: 'textFormat.normalizeNumbers.help',
  },
  {
    key: 'stripTrailingPunctuation',
    titleKey: 'textFormat.stripTrailing.title',
    hintKey: 'textFormat.stripTrailing.hint',
    detailKey: 'textFormat.stripTrailing.help',
  },
  {
    key: 'punctuationToSpace',
    titleKey: 'textFormat.punctuationToSpace.title',
    hintKey: 'textFormat.punctuationToSpace.hint',
    detailKey: 'textFormat.punctuationToSpace.help',
  },
]

export default function TextFormatSection() {
  const t = useT()
  const [opts, setOpts] = useState<TextPostProcessOptions>(DEFAULT_POST_PROCESS)
  const initialized = useRef(false)
  // 開著 AI 整理時，被 APPLIES_WITH_AI 判為不執行的項要置灰 —— 否則使用者點一個
  // 不起作用的開關，會以為功能壞了。aiReady 一起等：AI 開關的初值也是非同步讀回的，
  // 不等它就会先画出"未置灰"再跳到"已置灰"。
  const aiEnabled = useAiEnabled()
  const aiReady = useAiEnabledReady()
  // 讀到已儲存值之前，開關先隱藏、且不放動畫：避免先畫出預設值再跳到已儲存值（閃一下）。
  // 用 finally 兜底，讀取失敗也讓開關出現（呈預設值），不至於一直隱藏。
  const [ready, setReady] = useState(false)
  // animate 與 ready 分開：ready 決定何時顯示，animate 決定何時允許過渡。
  // 若在揭開/賦值的同一幀就把 transition 加回來，按 CSS 規範瀏覽器會認為
  // 「有過渡且值變了」，於是把「預設值→已儲存值」真的動畫一遍（看起來就是閃一下）。
  // 所以揭開那一幀仍不帶過渡，隔兩幀待值穩定後才開過渡。
  const [animate, setAnimate] = useState(false)

  useEffect(() => {
    getTextPostProcessOptions()
      .then((loaded) => {
        setOpts(loaded)
        initialized.current = true
      })
      .finally(() => {
        setReady(true)
        requestAnimationFrame(() => requestAnimationFrame(() => setAnimate(true)))
      })
  }, [])

  useEffect(() => {
    if (!initialized.current) return
    void saveTextPostProcessOptions(opts)
  }, [opts])

  const toggle = (key: keyof TextPostProcessOptions) => {
    setOpts((prev) => ({ ...prev, [key]: !prev[key] }))
  }

  /** 此刻這一項是不是不會執行（開著 AI 且該項不穿透）。置灰與文案都以它為準。 */
  const isInert = (key: keyof TextPostProcessOptions) =>
    aiReady && aiEnabled && !APPLIES_WITH_AI[key]

  return (
    // 卡片樣式與其他頁統一（原來是小一號標題 + 分隔線頭部，邊距也不同）；開關放在右側，和全軟體一致
    <Card className="mb-6">
      <CardContent className="p-6">
        <h2 className="text-lg font-semibold">{t('textFormat.title')}</h2>
        {/* 說清生效條件：這幾項是"沒有 AI 時我們自己做的排版兜底"。開了 AI 整理，
            排版就归 AI 管，置灰的项不生效——两边都做会互相打架。文本替换不在此列，见热词页的「文本替换」。
            AI 开着时换成一句更直接的话，并指出去哪表达同样的诉求（输出约束在 AI 整理页）。 */}
        <p className="mt-1 text-xs text-muted-foreground">
          {aiReady && aiEnabled ? t('textFormat.descAiOn') : t('textFormat.desc')}
        </p>
      <div className="mt-2 divide-y divide-border">
        {/* 變數名從 t 改成 def：t 現在是翻譯函式，同名會被遮蔽掉 */}
        {TOGGLES.map((def) => {
          const inert = isInert(def.key)
          return (
            <div key={def.key} className="flex items-center justify-between gap-4 py-3">
              <div className="min-w-0">
                <div className="flex items-center gap-1.5">
                  <span className={cn('text-sm font-medium', inert && 'text-muted-foreground')}>
                    {t(def.titleKey)}
                  </span>
                  {/* aiNote 只加在"確實會被 AI 接管"的項上；穿透 AI 的項不該聲稱自己會失效。
                      AI 已经开着时也不重复——下面那句已经说了。 */}
                  <InfoTip
                    content={`${t(def.detailKey)}${!APPLIES_WITH_AI[def.key] && !inert ? t('textFormat.aiNote') : ''}`}
                    label={t(def.titleKey)}
                  />
                </div>
                {/* 不生效時把說明換成原因，而不是再加一段文字：原因就落在使用者此刻正在看的位置。 */}
                <p className={cn('text-xs', inert ? 'italic text-muted-foreground/70' : 'text-muted-foreground')}>
                  {inert ? t('textFormat.inertByAi') : t(def.hintKey)}
                </p>
              </div>
              <Switch
                checked={opts[def.key]}
                onChange={() => toggle(def.key)}
                disabled={inert}
                noAnimation={!animate}
                hidden={!ready}
                className="shrink-0"
              />
            </div>
          )
        })}
      </div>
      </CardContent>
    </Card>
  )
}
