/**
 * 敏感欄位脫敏工具
 * 用於匯出、診斷報告等場景，防止 API Key 等憑據洩露
 */

/** 匹配敏感 key 的模式（不區分大小寫） */
const SENSITIVE_KEY_PATTERNS = [
  /api[_-]?key/i,
  /apikey/i,
  /access[_-]?token/i,
  /secret/i,
  /password/i,
  /credential/i,
  /app[_-]?id/i,
]

/** 判斷一個 key 是否為敏感欄位 */
export function isSensitiveKey(key: string): boolean {
  return SENSITIVE_KEY_PATTERNS.some((pattern) => pattern.test(key))
}

/** 對敏感值進行脫敏（保留前 3 位 + 後 2 位，中間用 *** 替代） */
export function maskValue(value: unknown): string {
  const str = String(value ?? '')
  if (str.length <= 8) return '***'
  return `${str.slice(0, 3)}***${str.slice(-2)}`
}

/**
 * 遞迴脫敏物件中的敏感欄位
 * 返回新物件，不修改原物件
 */
export function sanitizeObject<T>(obj: T): T {
  if (obj === null || obj === undefined) return obj
  if (typeof obj !== 'object') return obj

  if (Array.isArray(obj)) {
    return obj.map((item) => sanitizeObject(item)) as unknown as T
  }

  const result: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(obj as Record<string, unknown>)) {
    if (isSensitiveKey(key) && typeof value === 'string' && value.length > 0) {
      result[key] = maskValue(value)
    } else if (typeof value === 'object' && value !== null) {
      result[key] = sanitizeObject(value)
    } else {
      result[key] = value
    }
  }
  return result as T
}
