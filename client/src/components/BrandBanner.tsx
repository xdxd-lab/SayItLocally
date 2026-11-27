// [自訂] SC-63：品牌橫幅與訴求。圖檔打包在 App 內，以 <img> 載入：同源資源，CSP 的 img-src 'self' 已允許；
// SVG 在 <img> 中不執行腳本、不載入外部資源。
// - banner.svg：動態版，與 repo 的 docs/images/banner.svg 相同。
// - banner-static.svg：同一張圖的靜態版（沒有 SMIL 動畫）。系統要求減少動態效果時由 <picture> 即時改用；
//   視窗隱藏時也改用，讓動態版停止取樣與繪製（lib/bannerMotion.ts）。

import { useEffect, useState } from 'react'
import bannerUrl from '@/assets/banner.svg'
import bannerStaticUrl from '@/assets/banner-static.svg'
import { useT } from '@/i18n/useT'
import { bannerSourceFor, isDocumentHidden, subscribeDocumentHidden } from '@/lib/bannerMotion'

const currentDocument = () => (typeof document === 'undefined' ? undefined : document)

export default function BrandBanner({ appeal = 'short' }: { appeal?: 'short' | 'full' | 'none' }) {
  const t = useT()
  const [hidden, setHidden] = useState(() => isDocumentHidden(currentDocument()))

  useEffect(() => {
    const doc = currentDocument()
    setHidden(isDocumentHidden(doc))
    return subscribeDocumentHidden(doc, setHidden)
  }, [])

  return (
    <figure className="brand-banner mb-6">
      <picture>
        <source media="(prefers-reduced-motion: reduce)" srcSet={bannerStaticUrl} />
        <img
          src={bannerSourceFor(hidden, bannerUrl, bannerStaticUrl)}
          alt={t('brand.bannerAlt')}
          className="block h-auto w-full select-none"
          draggable={false}
          width={1280}
          height={320}
        />
      </picture>
      {appeal !== 'none' && (
        <figcaption className="mt-2 text-center text-sm text-muted-foreground">
          {appeal === 'full' ? t('brand.appealFull') : t('brand.appeal')}
        </figcaption>
      )}
    </figure>
  )
}
