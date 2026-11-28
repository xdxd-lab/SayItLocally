import type { ThemeDefinition } from './types'
import { t } from '@/i18n'

/**
 * 青綠主題 — 紙張質感
 * 暖米色背景 + 青綠強調，靈感來自後端管理頁面
 * 適合長時間使用，護眼且有品牌感
 */
const teal: ThemeDefinition = {
  id: 'teal',
  get name() { return t('theme.teal') },
  isDark: false,
  previewColors: {
    bg: '#f5f3eb',
    sidebar: '#edeae2',
    primary: '#0d7377',
    accent: '#e8eeec',
  },
  vars: {
    // 基礎色 — 暖米紙張底
    '--background': '42 22% 94%',          // #f5f3eb
    '--foreground': '220 13% 18%',         // #1f2937
    '--card': '44 50% 97%',               // #faf8f2 暖白卡片
    '--card-foreground': '220 13% 18%',
    '--primary': '182 80% 26%',            // #0d7377 青綠，比之前更亮更鮮活
    '--primary-foreground': '0 0% 100%',
    '--secondary': '40 16% 90%',           // #ece9e1
    '--secondary-foreground': '220 13% 18%',
    '--muted': '40 16% 90%',
    '--muted-foreground': '220 9% 46%',
    '--accent': '175 14% 91%',            // 淺青灰
    '--accent-foreground': '220 13% 18%',
    '--destructive': '0 72% 51%',
    '--destructive-foreground': '0 0% 100%',
    '--destructive-strong': '0 74% 44%',
    '--border': '36 22% 85%',             // #ddd8cc 暖米边框
    '--input': '36 22% 85%',
    '--ring': '182 80% 26%',
    '--radius': '0.5rem',

    // 區域色
    '--sidebar-bg': '40 16% 90%',
    '--sidebar-border': '36 22% 85%',
    '--sidebar-item-active-bg': '175 16% 87%',
    '--sidebar-item-hover-bg': '40 14% 88%',
    '--sidebar-text': '220 9% 46%',
    '--sidebar-text-active': '182 80% 26%',
    '--titlebar-bg': '40 16% 90%',
    '--titlebar-text': '220 9% 46%',
    '--titlebar-close-hover-bg': '0 72% 51%',
    '--titlebar-close-hover-text': '0 0% 100%',

    // 表單控制元件
    '--input-bg': '44 50% 97%',
    '--input-border': '36 22% 85%',
    '--input-focus-border': '182 80% 26%',
    '--input-focus-ring': '182 80% 26%',
    '--input-placeholder': '220 6% 58%',

    // 狀態色（-strong 為淺底上達標的文字色，見 types.ts）
    '--success': '160 60% 38%',            // 與青綠協調的綠
    '--success-foreground': '0 0% 100%',
    '--success-strong': '162 60% 27%',
    '--warning': '38 85% 50%',
    '--warning-foreground': '0 0% 100%',
    '--warning-strong': '32 90% 33%',
    '--info': '195 80% 44%',
    '--info-foreground': '0 0% 100%',
    '--info-strong': '197 80% 30%',
  },
}

export default teal
