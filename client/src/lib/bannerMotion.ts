/**
 * [自訂] SC-63：橫幅何時用動態版、何時用靜態版。
 *
 * - 系統要求減少動態效果：由 BrandBanner 的 `<picture><source media="(prefers-reduced-motion: reduce)">`
 *   交給瀏覽器即時切換，這裡不處理。
 * - 視窗隱藏（縮到 Dock、關到選單列、被其他程式完全蓋住時由 WebView 判定）：改用靜態版，
 *   讓動態版的 SMIL 不再取樣與繪製；回到可見時換回動態版。
 */

type VisibilityDoc = Pick<Document, 'visibilityState' | 'addEventListener' | 'removeEventListener'>

export function isDocumentHidden(doc: Pick<Document, 'visibilityState'> | undefined): boolean {
  return doc?.visibilityState === 'hidden'
}

/** 隱藏時用靜態版，可見時用動態版。 */
export function bannerSourceFor(hidden: boolean, animated: string, still: string): string {
  return hidden ? still : animated
}

/** 訂閱文件可見性變化；回傳取消訂閱函式。 */
export function subscribeDocumentHidden(doc: VisibilityDoc | undefined, onChange: (hidden: boolean) => void): () => void {
  if (!doc) return () => {}
  const handler = () => onChange(isDocumentHidden(doc))
  doc.addEventListener('visibilitychange', handler)
  return () => doc.removeEventListener('visibilitychange', handler)
}
