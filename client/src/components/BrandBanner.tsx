// [自訂] SC-63：品牌橫幅與訴求。圖檔打包在 App 內（assets/banner.svg，與 repo 的 docs/images/banner.svg 相同），
// 以 <img> 載入：同源資源，CSP 的 img-src 'self' 已允許；SVG 在 <img> 中不執行腳本、不載入外部資源。

import bannerUrl from '@/assets/banner.svg'
import { useT } from '@/i18n/useT'

export default function BrandBanner({ appeal = 'short' }: { appeal?: 'short' | 'full' | 'none' }) {
  const t = useT()
  return (
    <figure className="brand-banner mb-6">
      <img
        src={bannerUrl}
        alt={t('brand.bannerAlt')}
        className="block h-auto w-full select-none"
        draggable={false}
        width={1280}
        height={320}
      />
      {appeal !== 'none' && (
        <figcaption className="mt-2 text-center text-sm text-muted-foreground">
          {appeal === 'full' ? t('brand.appealFull') : t('brand.appeal')}
        </figcaption>
      )}
    </figure>
  )
}
