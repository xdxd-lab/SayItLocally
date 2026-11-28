// 「熱詞注入提示詞」開關卡片
// 開啟後，AI 整理階段會把使用者熱詞表注入系統提示詞，幫助糾正/保留專有名詞。預設關閉。

import { useEffect, useState } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { getSetting, setSetting } from '@/services/store'
import { useAiEnabled } from '@/hooks/useAiEnabled'
import { useT } from '@/i18n/useT'

export default function HotwordPromptInjectToggle() {
  const t = useT()
  const aiEnabled = useAiEnabled()
  const [enabled, setEnabled] = useState(false)
  // 讀到已儲存值之前，開關先隱藏、且不放動畫：避免先畫出預設值再跳到已儲存值（閃一下）。
  // 用 finally 兜底，讀取失敗也讓開關出現（呈預設值），不至於一直隱藏。
  const [ready, setReady] = useState(false)
  // animate 與 ready 分開：ready 決定何時顯示，animate 決定何時允許過渡。
  // 若在揭開/賦值的同一幀就把 transition 加回來，按 CSS 規範瀏覽器會認為
  // 「有過渡且值變了」，於是把「預設值→已儲存值」真的動畫一遍（看起來就是閃一下）。
  // 所以揭開那一幀仍不帶過渡，隔兩幀待值穩定後才開過渡。
  const [animate, setAnimate] = useState(false)

  useEffect(() => {
    getSetting('injectHotwordsToPrompt', false)
      .then((v) => setEnabled(Boolean(v)))
      .finally(() => {
        setReady(true)
        requestAnimationFrame(() => requestAnimationFrame(() => setAnimate(true)))
      })
  }, [])

  const toggle = () => {
    const next = !enabled
    setEnabled(next)
    void setSetting('injectHotwordsToPrompt', next)
  }

  return (
    <Card>
      <CardContent className="p-6">
        <div className="flex items-center justify-between">
          <div className="pr-4">
            <h2 className={`text-lg font-semibold ${aiEnabled ? '' : 'text-muted-foreground'}`}>{t('hotwordInject.title')}</h2>
            <p className="mt-1 text-xs text-muted-foreground">
              {aiEnabled ? t('hotwordInject.desc') : t('aiInstructions.requiresAiCleanup')}
            </p>
          </div>
          {/* 熱詞只注入 AI 整理的提示詞，AI 整理關著時這項不生效：畫成關閉 + 停用，
              不顯示"開著卻不生效"。儲存的值不動，AI 整理開啟後照舊。 */}
          <Switch checked={enabled && aiEnabled} onChange={toggle} disabled={!aiEnabled} noAnimation={!animate} hidden={!ready} />
        </div>
      </CardContent>
    </Card>
  )
}
