// [自訂] REV9：隔開兩次確認，拒絕連點事件也保留鍵盤啟用。
export const HISTORY_CLEAR_DELAY_MS = 600

export function canConfirmHistoryClear(now: number, readyAt: number, clickCount: number) {
  return now >= readyAt && clickCount <= 1
}
