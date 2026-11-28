// 列表拖拽排序 —— 用原生 HTML5 拖拽，不引入拖拽庫。
//
// 為什麼不裝 dnd-kit 之類：桌面端只有滑鼠（不需要觸控/指標相容），列表都是簡單的一維
// 縱向列表，原生 API 足夠；裝庫要多背幾十 KB，而且此前全應用沒有任何拖拽依賴。
//
// ⚠️ Tauri 前置條件：視窗必須配 `"dragDropEnabled": false`（tauri.conf.json）。
// Tauri 預設開啟自己的「檔案拖放」處理器，它在 Windows 上會攔掉 OS 層的拖放，
// 導致 webview 裡的 HTML5 拖拽事件根本不觸發 —— 表現就是"手柄拖不動"。
// 參考 tauri-apps/tauri#14373。本專案沒有用到檔案拖放，關掉無副作用。
//
// 兩個刻意的設計：
//  1. **只有拖拽柄可拖**，不是整行可拖 —— 行內有輸入框（如熱詞分類的新增框），整行可拖
//     會把框裡的文字選擇也變成拖拽。拖起來時用 setDragImage 把整行作為拖影，觀感不受影響。
//  2. **拖拽柄可聚焦，聚焦後按 ↑/↓ 也能移動** —— 原生拖拽對鍵盤和讀屏使用者完全不可用，
//     這樣不必在介面上常駐一對上下按鈕，也不會把這兩類使用者擋在外面。

import { useRef, useState, type ComponentPropsWithoutRef, type DragEvent, type KeyboardEvent } from 'react'
import { GripVertical } from 'lucide-react'
import { useT } from '@/i18n/useT'
import { cn } from '@/lib/utils'

export interface SortableOptions {
  /** 把第 from 項移到第 to 項的位置（越界由呼叫方忽略） */
  onMove: (from: number, to: number) => void
}

export function useSortable({ onMove }: SortableOptions) {
  const t = useT()
  const [dragIndex, setDragIndex] = useState<number | null>(null)
  const [overIndex, setOverIndex] = useState<number | null>(null)
  const rowRefs = useRef<Array<HTMLElement | null>>([])

  const registerRow = (index: number) => (element: HTMLElement | null) => {
    rowRefs.current[index] = element
  }

  /** 加在拖拽柄上 */
  const handleProps = (index: number, label: string) => ({
    draggable: true,
    'aria-label': t('ui.sortableAria', { label }),
    onDragStart: (event: DragEvent) => {
      setDragIndex(index)
      event.dataTransfer.effectAllowed = 'move'
      // 必須寫點資料，否則部分平台不認為這是一次有效拖拽
      event.dataTransfer.setData('text/plain', String(index))
      const row = rowRefs.current[index]
      if (row) event.dataTransfer.setDragImage(row, 16, row.offsetHeight / 2)
    },
    onDragEnd: () => {
      setDragIndex(null)
      setOverIndex(null)
    },
    onKeyDown: (event: KeyboardEvent) => {
      if (event.key === 'ArrowUp') {
        event.preventDefault()
        onMove(index, index - 1)
      } else if (event.key === 'ArrowDown') {
        event.preventDefault()
        onMove(index, index + 1)
      }
    },
  })

  /** 加在每一行（拖拽的落點）上 */
  const rowProps = (index: number) => ({
    ref: registerRow(index),
    onDragOver: (event: DragEvent) => {
      if (dragIndex === null) return
      event.preventDefault()
      event.dataTransfer.dropEffect = 'move'
      setOverIndex(index)
    },
    onDragLeave: () => {
      setOverIndex((current) => (current === index ? null : current))
    },
    onDrop: (event: DragEvent) => {
      event.preventDefault()
      if (dragIndex !== null && dragIndex !== index) onMove(dragIndex, index)
      setDragIndex(null)
      setOverIndex(null)
    },
  })

  /** 行的狀態樣式：拖起來的那行變淡，當前落點給一條指示線 */
  const rowClassName = (index: number) => cn(
    dragIndex === index && 'opacity-40',
    overIndex === index && dragIndex !== null && dragIndex !== index && (
      index > dragIndex ? 'border-b-2 border-b-primary' : 'border-t-2 border-t-primary'
    ),
  )

  return { handleProps, rowProps, rowClassName, dragIndex }
}

/**
 * 拖拽柄。預設淡顯，滑鼠移到所在行（group）時顯現。
 *
 * 用 div + role="button" 而不是真的 <button>：Chromium 對錶單控制元件的拖拽支援不一致，
 * `<button draggable>` 經常不觸發 dragstart（另一個"拖不動"的來源）。
 * 配 tabIndex 與 role 後，鍵盤與讀屏的語義不受影響。
 */
export function DragHandle({
  className,
  ...rest
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      role="button"
      tabIndex={0}
      className={cn(
        // 平時常顯一個很淡的手柄：完全透明時，行首那塊空白看著像排版沒對齊
        'shrink-0 cursor-grab select-none rounded p-1 text-muted-foreground/30 transition-colors',
        'group-hover:text-muted-foreground/60 hover:bg-accent hover:text-foreground',
        'focus-visible:text-muted-foreground active:cursor-grabbing',
        className,
      )}
      {...rest}
    >
      <GripVertical className="h-3.5 w-3.5" aria-hidden />
    </div>
  )
}

/** 陣列換位：把 from 移到 to（越界原樣返回）。拖拽是"插入到某個位置"，不是兩兩交換。 */
export function moveItem<T>(list: T[], from: number, to: number): T[] {
  if (from === to || from < 0 || to < 0 || from >= list.length || to >= list.length) return list
  const next = [...list]
  const [item] = next.splice(from, 1)
  next.splice(to, 0, item)
  return next
}
