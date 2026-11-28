/**
 * 執行平台判斷。只在確實要分平台的地方用（快捷鍵、標題欄、文案、更新通道等）。
 *
 * WKWebView（macOS）的 UA 帶 "Macintosh"，WebView2（Windows）帶 "Windows NT"。
 * 單測跑在 vitest 的 Node 環境裡（UA 形如 "Node.js/22"），不會被判成 mac，
 * 所以現有單測預設按 Windows 行為跑。
 */
export const IS_MAC: boolean =
  typeof navigator !== 'undefined' && /Macintosh|Mac OS X/i.test(navigator.userAgent || '')
