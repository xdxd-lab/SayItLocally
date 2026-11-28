/**
 * 歷史記錄按「本地日期」分組，組按日期從新到舊排。
 *
 * 為什麼按當天零點的時間戳排，而不是按分組標籤排：標籤是給人看的本地化字串
 * （「2026年10月13日」「October 13, 2026」），按字串比大小月份就亂了 ——
 * 「2026年8月…」排在「2026年10月…」前面，英文則按月份名的字母序。原來就是這麼寫的，
 * 收藏頁一跨月，最新的十月反而沉到最底下。今天 / 昨天也不用再單獨判斷：它們的零點最大。
 *
 * 組內順序保持傳入順序（資料庫按 list_order 排好的，新的在前），這裡不重排。
 */
export interface DayGroup<T> {
  /** 當天本地零點的毫秒時間戳，也當 React key 用 */
  dayStart: number
  label: string
  records: T[]
}

export function groupByDay<T extends { timestamp: number }>(
  records: readonly T[],
  labelFor: (timestamp: number) => string,
): DayGroup<T>[] {
  const byDay = new Map<number, DayGroup<T>>()
  for (const record of records) {
    const date = new Date(record.timestamp)
    const dayStart = new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime()
    let group = byDay.get(dayStart)
    if (!group) {
      group = { dayStart, label: labelFor(record.timestamp), records: [] }
      byDay.set(dayStart, group)
    }
    group.records.push(record)
  }
  return [...byDay.values()].sort((a, b) => b.dayStart - a.dayStart)
}
