import type { UserStats } from './types'
import { t } from '@/i18n'

interface DomainSceneRule {
  id: string
  label: string
  matchers: string[]
  promptSnippet?: string
}

export interface DomainSceneSummary {
  id: string
  label: string
  words: number
  ratio: number
}

const DOMAIN_SCENE_RULES: DomainSceneRule[] = [
  {
    id: 'development',
    get label() { return t('userScene.development') },
    matchers: [
      'code',
      'cursor',
      'devenv',
      'idea',
      'webstorm',
      'pycharm',
      'goland',
      'clion',
      'studio64',
      'kiro',
    ],
    promptSnippet: '該使用者具有深厚的軟體開發背景，遇到發音模糊的詞彙請優先推測為 IT 術語、API 名稱、命令列引數或英文變數。', // i18n-allow: 中文口述整理 Prompt
  },
  {
    id: 'communication',
    get label() { return t('userScene.communication') },
    // weixin：微信 4.1 起 exe 從 WeChat.exe 改名為 Weixin.exe，只留 wechat 會把
    // 新版微信整個漏掉（personalization/defaults.ts 的內建規則同樣列了兩個名字）
    matchers: ['wechat', 'weixin', 'wecom', 'qq', 'teams', 'slack', 'discord', 'telegram', 'dingtalk'],
    promptSnippet: '該使用者長期處於即時溝通場景，遇到模糊表達時優先整理成簡潔、可直接傳送的訊息。', // i18n-allow: 中文口述整理 Prompt
  },
  {
    id: 'office',
    get label() { return t('userScene.office') },
    matchers: ['outlook', 'word', 'excel', 'powerpnt', 'wps', 'onenote', 'notion'],
    promptSnippet: '該使用者長期處於辦公寫作場景，遇到模糊表達時優先採用正式、完整、結構清晰的書面語，但不要新增事實。', // i18n-allow: 中文口述整理 Prompt
  },
  {
    id: 'browser',
    get label() { return t('userScene.browsing') },
    matchers: ['chrome', 'msedge', 'edge', 'firefox', 'arc', 'safari'],
  },
]

const FALLBACK_SCENE: DomainSceneRule = {
  id: 'general',
  get label() { return t('userScene.general') },
  matchers: [],
}

function normalizeAppId(appId: string) {
  return String(appId || '').trim().toLowerCase()
}

function pickSceneRule(appId: string) {
  const normalized = normalizeAppId(appId)
  return DOMAIN_SCENE_RULES.find((rule) => rule.matchers.some((matcher) => normalized.includes(matcher))) || FALLBACK_SCENE
}

function getStatsTotalWords(stats: UserStats) {
  if (stats.totalWords > 0) return stats.totalWords
  return Object.values(stats.domainWords).reduce((sum, words) => sum + words, 0)
}

export function summarizeDomainScenes(stats: UserStats, limit = 3): DomainSceneSummary[] {
  const totalWords = getStatsTotalWords(stats)
  if (totalWords <= 0) return []

  const wordsByScene = new Map<string, DomainSceneSummary>()
  for (const [appId, words] of Object.entries(stats.domainWords)) {
    if (words <= 0) continue
    const rule = pickSceneRule(appId)
    const current = wordsByScene.get(rule.id)
    if (current) {
      current.words += words
      current.ratio = current.words / totalWords
      continue
    }

    wordsByScene.set(rule.id, {
      id: rule.id,
      label: rule.label,
      words,
      ratio: words / totalWords,
    })
  }

  return [...wordsByScene.values()]
    .sort((left, right) => right.words - left.words)
    .slice(0, limit)
}

export function buildDynamicIdentityPrompt(stats: UserStats) {
  if (stats.totalWords <= 1000) return ''

  const dominantScene = summarizeDomainScenes(stats, 1)[0]
  if (!dominantScene || dominantScene.ratio <= 0.3) return ''

  return DOMAIN_SCENE_RULES.find((rule) => rule.id === dominantScene.id)?.promptSnippet || ''
}
