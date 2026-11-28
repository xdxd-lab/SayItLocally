/**
 * 把 `**加粗**` 標記渲染成 <strong>。
 *
 * 為什麼需要它：有些說明句裡有強調（"熱詞只提高**命中機率**"），原來是把句子
 * 拆成幾段 JSX、中間夾 `<span className="font-semibold">`。一旦要翻譯，這種寫法
 * 就要求中英**語序一致**才拼得對 —— 而語序恰恰是最先不一致的東西。
 *
 * 改成讓譯文自己帶標記：強調落在哪裡由譯者決定，程式碼不關心。
 * 只支援加粗一種標記，夠用就不要再往上加（Markdown 子集會越滾越大）。
 */
import type { ReactNode } from 'react'

export function RichText({ text, strongClassName = 'font-semibold' }: {
  text: string
  strongClassName?: string
}) {
  const parts = text.split('**')
  return (
    <>
      {parts.map((part, index): ReactNode => (
        // 奇數段落在一對 ** 之間 —— split 的結果天然交替
        index % 2 === 1
          ? <span key={index} className={strongClassName}>{part}</span>
          : part
      ))}
    </>
  )
}
