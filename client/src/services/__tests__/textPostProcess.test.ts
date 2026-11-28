import { describe, it, expect } from 'vitest'
import {
  convertChineseNumbers,
  normalizeChinesePunctuation,
  restoreHotwordSpacing,
  stripTrailingPunctuation,
  replacePunctuationWithSpace,
} from '../textPostProcess'

describe('normalizeChinesePunctuation', () => {
  // Whisper（Groq 的 whisper-large-v3-turbo）轉寫中文時混用兩種寬度：句號給全形，
  // 逗號和問號給半形。實測改 prompt 無效，只能在文本層歸一。
  it('漢字後面的半形標點轉成全形', () => {
    expect(normalizeChinesePunctuation('今天天氣不錯,我們出去走走吧,順便買點東西。你覺得怎麼樣?'))
      .toBe('今天天氣不錯，我們出去走走吧，順便買點東西。你覺得怎麼樣？')
    expect(normalizeChinesePunctuation('太好了!')).toBe('太好了！')
    expect(normalizeChinesePunctuation('注意:兩點;三點')).toBe('注意：兩點；三點')
  })

  // 下面這幾條是這個函式存在的全部風險所在：判據寫松一點就會破壞內容。
  it('數字裡的逗號與小數點絕不動', () => {
    expect(normalizeChinesePunctuation('一共 3,000 元')).toBe('一共 3,000 元')
    expect(normalizeChinesePunctuation('圓周率是 3.14')).toBe('圓周率是 3.14')
  })

  // 已知缺口，且是刻意留的：標點夾在「數字/字母」和漢字之間時不轉。
  // 想覆蓋 `100,很便宜` 就必然會把 `3:2中文`、`3,000元` 這類也一起改掉，
  // 而少一個全形逗號只是觀感問題，改壞數字是內容損壞。寧可漏改。
  it('前一個字元不是漢字時一律不動，即便後面是中文', () => {
    expect(normalizeChinesePunctuation('版本 v1.2,已釋出')).toBe('版本 v1.2,已釋出')
    expect(normalizeChinesePunctuation('比例 3:2中文')).toBe('比例 3:2中文')
  })

  it('英文句子裡的標點不動', () => {
    expect(normalizeChinesePunctuation('Hello, world! Are you ok?'))
      .toBe('Hello, world! Are you ok?')
    expect(normalizeChinesePunctuation('中文A,B')).toBe('中文A,B')
  })

  it('網址與句號一律不動（句號不在轉換表裡）', () => {
    expect(normalizeChinesePunctuation('訪問 example.com 就行')).toBe('訪問 example.com 就行')
    // 半形句號即便緊跟漢字也保持原樣：小數/域名/檔名的代價高於收益
    expect(normalizeChinesePunctuation('好的.')).toBe('好的.')
  })

  it('本來就是全形、或沒有漢字時是空操作', () => {
    expect(normalizeChinesePunctuation('今天不錯，我們走吧。')).toBe('今天不錯，我們走吧。')
    expect(normalizeChinesePunctuation('')).toBe('')
    expect(normalizeChinesePunctuation(',開頭的逗號沒有前字元')).toBe(',開頭的逗號沒有前字元')
  })
})

describe('restoreHotwordSpacing', () => {
  it('還原被拆開加空格的熱詞', () => {
    expect(restoreHotwordSpacing('這是 Say It 輸入法', ['SayIt'])).toBe('這是 SayIt 輸入法')
    expect(restoreHotwordSpacing('用 Type less 打字', ['Typeless'])).toBe('用 Typeless 打字')
    expect(restoreHotwordSpacing('試試 GPT 5', ['GPT'])).toBe('試試 GPT 5')
  })

  it('尾字母被小寫化也能還原（Say it → SayIt）', () => {
    expect(restoreHotwordSpacing('這是 Say it 輸入法', ['SayIt'])).toBe('這是 SayIt 輸入法')
    expect(restoreHotwordSpacing('Say it', ['SayIt'])).toBe('SayIt')
  })

  it('連寫詞的大小寫也還原（typeless → Typeless）', () => {
    expect(restoreHotwordSpacing('識別出來了 typeless。', ['Typeless'])).toBe('識別出來了 Typeless。')
    expect(restoreHotwordSpacing('sayit 很好用', ['SayIt'])).toBe('SayIt 很好用')
  })

  it('已正確的熱詞不改動', () => {
    expect(restoreHotwordSpacing('SayIt 很好用', ['SayIt'])).toBe('SayIt 很好用')
  })

  it('大小寫敏感，不誤傷普通小寫短語', () => {
    expect(restoreHotwordSpacing('I say it loudly', ['SayIt'])).toBe('I say it loudly')
  })

  it('詞邊界避免錯誤合併', () => {
    expect(restoreHotwordSpacing('Say Item here', ['SayIt'])).toBe('Say Item here')
  })

  it('無熱詞或空文本安全', () => {
    expect(restoreHotwordSpacing('Say It', [])).toBe('Say It')
    expect(restoreHotwordSpacing('', ['SayIt'])).toBe('')
  })

  it('忽略含空格或中文的熱詞', () => {
    expect(restoreHotwordSpacing('你 好 世界', ['你好'])).toBe('你 好 世界')
  })
})

describe('convertChineseNumbers', () => {
  it('百分之 → %', () => {
    expect(convertChineseNumbers('增長了百分之十五')).toBe('增長了15%')
    expect(convertChineseNumbers('百分之百完成')).toBe('100%完成')
    expect(convertChineseNumbers('百分之三點五')).toBe('3.5%')
  })

  it('小數：數字點數字', () => {
    expect(convertChineseNumbers('版本從三點一升級到三點二')).toBe('版本從3.1升級到3.2')
    expect(convertChineseNumbers('圓周率約等於三點一四')).toBe('圓周率約等於3.14')
    expect(convertChineseNumbers('二十三點五度')).toBe('23.5度')
  })

  it('英文後的小數補空格', () => {
    expect(convertChineseNumbers('用的是GPT五點四')).toBe('用的是GPT 5.4')
  })

  it('多段點分數字（版本號）', () => {
    expect(convertChineseNumbers('升級到零點一點零')).toBe('升級到0.1.0')
    expect(convertChineseNumbers('版本一點二點三')).toBe('版本1.2.3')
    expect(convertChineseNumbers('三點一四不變')).toBe('3.14不變')
    expect(convertChineseNumbers('從零點九點五到一點零點零')).toBe('從0.9.5到1.0.0')
  })

  it('分之 → 分數', () => {
    expect(convertChineseNumbers('五分之二')).toBe('2/5')
    expect(convertChineseNumbers('千分之五')).toBe('5/1000')
  })

  it('結構化整數（含位值詞）', () => {
    expect(convertChineseNumbers('擴容二十三台')).toBe('擴容23台')
    expect(convertChineseNumbers('三百二十五塊錢')).toBe('325塊錢')
    expect(convertChineseNumbers('埠號是三千三百零六')).toBe('埠號是3306')
  })

  it('口語省略末位', () => {
    expect(convertChineseNumbers('大概一\u4e07五')).toBe('大概15000')
    expect(convertChineseNumbers('花了三千二')).toBe('花了3200')
    expect(convertChineseNumbers('兩百五')).toBe('250')
  })

  it('零的間隔正確', () => {
    expect(convertChineseNumbers('三千零二')).toBe('3002')
    expect(convertChineseNumbers('一百零五')).toBe('105')
  })

  it('不誤傷成語/口語（無位值詞或單位單獨成段）', () => {
    expect(convertChineseNumbers('十分感謝你')).toBe('十分感謝你')
    expect(convertChineseNumbers('一心一意做事')).toBe('一心一意做事')
    expect(convertChineseNumbers('千方百計')).toBe('千方百計')
    expect(convertChineseNumbers('十全十美')).toBe('十全十美')
    expect(convertChineseNumbers('百姓的生活')).toBe('百姓的生活')
  })

  it('不誤傷連續單位黑名單詞', () => {
    expect(convertChineseNumbers('千萬不要這樣')).toBe('千萬不要這樣')
    expect(convertChineseNumbers('\u4e07一出事了')).toBe('\u4e07一出事了')
  })

  it('不轉孤立單字與無位值詞的逐位串', () => {
    expect(convertChineseNumbers('第一二三點')).toBe('第一二三點')
    expect(convertChineseNumbers('一二三四五')).toBe('一二三四五')
  })

  it('時間：數字轉阿拉伯，保留點/分/半', () => {
    expect(convertChineseNumbers('上午九點三十二分')).toBe('上午9點32分')
    expect(convertChineseNumbers('九點二十分開會')).toBe('9點20分開會')
    expect(convertChineseNumbers('下午兩點半開會')).toBe('下午2點半開會')
    expect(convertChineseNumbers('上午九點開會')).toBe('上午9點開會')
    expect(convertChineseNumbers('十九點三十分')).toBe('19點30分')
  })

  it('時間：無「分」的複合分鐘也按時間處理（不再出現 9.3十 之類亂碼）', () => {
    expect(convertChineseNumbers('九點三十')).toBe('9點30')
    expect(convertChineseNumbers('九點二十開會')).toBe('9點20開會')
  })

  it('不誤傷口語「一點」(a little bit)：無時間訊號的裸整點保留中文', () => {
    expect(convertChineseNumbers('把頁面調寬一點')).toBe('把頁面調寬一點')
    expect(convertChineseNumbers('有一點擔心')).toBe('有一點擔心')
    expect(convertChineseNumbers('再快一點')).toBe('再快一點')
    expect(convertChineseNumbers('聲音大一點')).toBe('聲音大一點')
    expect(convertChineseNumbers('一點點小事')).toBe('一點點小事')
    // 無時段詞的裸整點也保守不轉（寧可不轉，交給 AI 整理）
    expect(convertChineseNumbers('九點開會')).toBe('九點開會')
  })

  it('整點：有時間訊號才轉（時段詞 / 鍾 / 含十兩位）', () => {
    expect(convertChineseNumbers('下午一點開會')).toBe('下午1點開會')
    expect(convertChineseNumbers('晚上八點')).toBe('晚上8點')
    expect(convertChineseNumbers('一點鐘到')).toBe('1點鐘到')
    expect(convertChineseNumbers('十二點了')).toBe('12點了')
    expect(convertChineseNumbers('十點休息')).toBe('10點休息')
  })

  it('空文本安全', () => {
    expect(convertChineseNumbers('')).toBe('')
  })
})

describe('stripTrailingPunctuation', () => {
  it('去除句末標點', () => {
    expect(stripTrailingPunctuation('今天天氣不錯。')).toBe('今天天氣不錯')
    expect(stripTrailingPunctuation('真的嗎？！')).toBe('真的嗎')
    expect(stripTrailingPunctuation('好的...')).toBe('好的')
  })

  it('逐段處理', () => {
    expect(stripTrailingPunctuation('第一段。\n第二段！')).toBe('第一段\n第二段')
  })

  it('句中標點保留', () => {
    expect(stripTrailingPunctuation('你好，世界。')).toBe('你好，世界')
  })

  it('空文本安全', () => {
    expect(stripTrailingPunctuation('')).toBe('')
  })
})

describe('replacePunctuationWithSpace', () => {
  it('標點轉空格並摺疊', () => {
    expect(replacePunctuationWithSpace('你好，世界！')).toBe('你好 世界')
    expect(replacePunctuationWithSpace('一、二、三')).toBe('一 二 三')
  })

  it('保留數字小數點與百分號', () => {
    expect(replacePunctuationWithSpace('圓周率是3.14，約等於')).toBe('圓周率是3.14 約等於')
    expect(replacePunctuationWithSpace('增長15%，很好')).toBe('增長15% 很好')
  })

  it('保留換行', () => {
    expect(replacePunctuationWithSpace('第一行。\n第二行。')).toBe('第一行\n第二行')
  })

  it('空文本安全', () => {
    expect(replacePunctuationWithSpace('')).toBe('')
  })
})

// [自訂] SC-67：辨識輸出已轉成台灣正體，數字規範化要認得正體字，產出的標記也用正體。
describe('convertChineseNumbers：正體輸入', () => {
  it('逐句判斷：日文句不影響其他中文句', () => {
    // 日文句（含假名）維持原行為；同段的正體中文句照樣規範化。
    expect(convertChineseNumbers('晚上八點半。今日は\u4e09\u6642です。差不多兩萬人')).toBe('晚上8點半。今日は\u4e09\u6642です。差不多20000人')
    // 含簡體「點」的句子維持原行為，不影響下一句的正體對應。
    expect(convertChineseNumbers('三\u70b9\u4e94！百分之三點五')).toBe('3.5！3.5%')
  })
  it('正體寫法與簡體寫法得到相同的數字', () => {
    const cases: Array<[string, string]> = [
      ['增長了百分之十五', '增長了15%'],
      ['百分之三點五', '3.5%'],
      ['圓周率約等於三點一四', '圓周率約等於3.14'],
      ['用的是GPT五點四', '用的是GPT 5.4'],
      ['升級到零點一點零', '升級到0.1.0'],
      ['擴充二十三台', '擴充23台'],
      ['大概一萬五', '大概15000'],
      ['兩百五', '250'],
      ['三億', '300000000'],
    ]
    for (const [input, expected] of cases) {
      expect(convertChineseNumbers(input), input).toBe(expected)
    }
  })

  it('時間標記保留正體「點」', () => {
    expect(convertChineseNumbers('下午兩點半開會')).toBe('下午2點半開會')
    expect(convertChineseNumbers('九點三十二分')).toBe('9點32分')
    expect(convertChineseNumbers('夜裡十點')).toBe('夜裡10點')
    expect(convertChineseNumbers('一點鐘')).toBe('1點鐘')
  })

  it('非數字用法與日文不受影響', () => {
    expect(convertChineseNumbers('千萬不要')).toBe('千萬不要')
    expect(convertChineseNumbers('調寬一點')).toBe('調寬一點')
    expect(convertChineseNumbers('今日は三時です')).toBe('今日は三時です')
  })
})
