/** @type {import('tailwindcss').Config} */
module.exports = {
  darkMode: ["class"],
  content: ["./index.html", "./overlay.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        border: "hsl(var(--border))",
        input: "hsl(var(--input))",
        ring: "hsl(var(--ring))",
        background: "hsl(var(--background))",
        foreground: "hsl(var(--foreground))",
        primary: {
          DEFAULT: "hsl(var(--primary))",
          foreground: "hsl(var(--primary-foreground))",
        },
        secondary: {
          DEFAULT: "hsl(var(--secondary))",
          foreground: "hsl(var(--secondary-foreground))",
        },
        destructive: {
          DEFAULT: "hsl(var(--destructive))",
          foreground: "hsl(var(--destructive-foreground))",
          // strong = 直接畫在頁面底色上的文字色（對比度達標版），見 src/themes/types.ts
          strong: "hsl(var(--destructive-strong))",
        },
        muted: {
          DEFAULT: "hsl(var(--muted))",
          foreground: "hsl(var(--muted-foreground))",
        },
        accent: {
          DEFAULT: "hsl(var(--accent))",
          foreground: "hsl(var(--accent-foreground))",
        },
        card: {
          DEFAULT: "hsl(var(--card))",
          foreground: "hsl(var(--card-foreground))",
        },
        // 區域色
        sidebar: {
          DEFAULT: "hsl(var(--sidebar-bg))",
          border: "hsl(var(--sidebar-border))",
          "item-active": "hsl(var(--sidebar-item-active-bg))",
          "item-hover": "hsl(var(--sidebar-item-hover-bg))",
          text: "hsl(var(--sidebar-text))",
          "text-active": "hsl(var(--sidebar-text-active))",
        },
        titlebar: {
          DEFAULT: "hsl(var(--titlebar-bg))",
          text: "hsl(var(--titlebar-text))",
          "close-hover": "hsl(var(--titlebar-close-hover-bg))",
          "close-hover-text": "hsl(var(--titlebar-close-hover-text))",
        },
        // 表單控制元件色
        "input-bg": "hsl(var(--input-bg))",
        "input-border": "hsl(var(--input-border))",
        "input-focus-border": "hsl(var(--input-focus-border))",
        "input-focus-ring": "hsl(var(--input-focus-ring))",
        "input-placeholder": "hsl(var(--input-placeholder))",
        // 狀態色。三個變體的分工見 src/themes/types.ts：
        //   DEFAULT    = 色塊本體（圓點 / 進度條 / 10% 淡底）
        //   foreground = 壓在色塊本體上的文字
        //   strong     = 直接畫在頁面或卡片底色上的文字（對比度 ≥4.5:1）
        success: {
          DEFAULT: "hsl(var(--success))",
          foreground: "hsl(var(--success-foreground))",
          strong: "hsl(var(--success-strong))",
        },
        warning: {
          DEFAULT: "hsl(var(--warning))",
          foreground: "hsl(var(--warning-foreground))",
          strong: "hsl(var(--warning-strong))",
        },
        info: {
          DEFAULT: "hsl(var(--info))",
          foreground: "hsl(var(--info-foreground))",
          strong: "hsl(var(--info-strong))",
        },
      },
      borderRadius: {
        lg: "var(--radius)",
        md: "calc(var(--radius) - 2px)",
        sm: "calc(var(--radius) - 4px)",
      },
    },
  },
  plugins: [require("tailwindcss-animate")],
}
