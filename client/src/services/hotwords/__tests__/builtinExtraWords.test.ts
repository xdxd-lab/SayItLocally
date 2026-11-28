import { describe, expect, it, vi } from 'vitest'

vi.mock('@/i18n', () => ({ t: (key: string) => key }))

import {
  BUILTIN_SETS,
  extractBuiltinSetExtraWords,
  normalizeBuiltinSetWords,
} from '../model'

const defaults = BUILTIN_SETS.ai.words

describe('user words added to a builtin set', () => {
  it('survive a reload, while words dropped from the builtin list are still cleaned up', () => {
    // 存檔時：一個當前預設詞、一個已被新版移出預設表的舊詞、一個使用者加的詞
    const inMemory = { ai: [defaults[0], 'RetiredDefault', 'MyTerm'] }
    // 'RetiredDefault' 在記憶體裡也不是預設詞，所以會被當成使用者詞寫進 extras ——
    // 這裡模擬的是舊版本存下來的資料：舊詞只在 builtinSetWords 裡，extras 裡沒有它
    const storedWords = { ai: [defaults[0], 'RetiredDefault'] }
    const storedExtras = { ai: ['MyTerm'] }

    const loaded = normalizeBuiltinSetWords(storedWords, storedExtras)
    expect(loaded.ai).toEqual([defaults[0], 'MyTerm'])
    expect(extractBuiltinSetExtraWords(inMemory)).toEqual({ ai: ['RetiredDefault', 'MyTerm'] })
  })

  it('falls back to all defaults when only user words were ever saved', () => {
    const loaded = normalizeBuiltinSetWords({}, { ai: ['MyTerm'] })
    expect(loaded.ai).toEqual([...new Set(defaults), 'MyTerm'])
  })

  it('keeps the old behaviour when there are no user words', () => {
    expect(normalizeBuiltinSetWords(undefined)).toEqual({})
    expect(normalizeBuiltinSetWords({ ai: [defaults[1]] }, {})).toEqual({ ai: [defaults[1]] })
    expect(extractBuiltinSetExtraWords({ ai: [...defaults] })).toEqual({})
  })
})

// [自訂] F14 PR-C：舊設定存的是簡體「千問」（以 Unicode 跳脫寫出），讀取時要換成正體，不能被濾掉。
describe('舊簡體內建詞', () => {
  it('簡體「千問」讀成「千問」並保留', () => {
    const result = normalizeBuiltinSetWords({ ai: ['ChatGPT', '\u5343\u95ee'] })
    expect(result.ai).toEqual(['ChatGPT', '千問'])
  })
})
