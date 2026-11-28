import { useCallback, useEffect, useRef, useState } from 'react'
import { loadAudioAsObjectUrl } from '@/services/audioFileService'
import { protectPlayingRecording, releasePlayingRecording, renewPlayingRecording } from '@/services/bridge'

/**
 * 歷史錄音的播放狀態。
 *
 * 這段邏輯原來內聯在 HistoryRecordList 的 HistoryItem 裡，因為「糾正識別」面板
 * 也要放一個一模一樣的播放器才抽出來。抽的時候行為一個字沒改 —— 下面那些
 * 看起來多餘的寫法都有來由，別順手"簡化"。
 *
 * 同一時刻只允許播放一條錄音：播新的先暫停舊的，只 pause、不重置進度，
 * 所以之後可以從原位置續播；被暫停那條會觸發它自己的 onpause，按鈕狀態隨之復位。
 */
let activeHistoryAudio: HTMLAudioElement | null = null
function claimHistoryPlayback(audio: HTMLAudioElement) {
  if (activeHistoryAudio && activeHistoryAudio !== audio) {
    activeHistoryAudio.pause()
  }
  activeHistoryAudio = audio
}
function releaseHistoryPlayback(audio: HTMLAudioElement) {
  if (activeHistoryAudio === audio) activeHistoryAudio = null
}

export function formatElapsed(value: number): string {
  const safe = Number.isFinite(value) ? Math.max(0, value) : 0
  const m = Math.floor(safe / 60)
  const s = Math.floor(safe % 60)
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
}

export interface RecordingPlayback {
  playing: boolean
  loading: boolean
  /** 後設資料已就緒（能拿到總時長），進度條才有意義 */
  ready: boolean
  currentTime: number
  duration: number
  playbackRate: number
  /** 0~1 */
  progress: number
  toggle: () => Promise<void>
  seek: (seconds: number) => void
  changeRate: (rate: number) => void
}

/**
 * @param initialDurationSec 先用歷史記錄裡存的時長把總長顯示出來。
 *   真即時長要等 `onloadedmetadata`，而那要先把整份 WAV 讀出來（五分鐘錄音約 13MB 的
 *   base64），只為顯示"總共多長"不值得。沒有它的話，使用者點播放之前看到的是
 *   `00:00 / 00:00`，像壞了。後設資料到了會覆蓋成精確值。
 */
export function useRecordingPlayback(audioFilePath?: string, initialDurationSec = 0): RecordingPlayback {
  const [playing, setPlaying] = useState(false)
  const [loading, setLoading] = useState(false)
  const [ready, setReady] = useState(false)
  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(
    Number.isFinite(initialDurationSec) && initialDurationSec > 0 ? initialDurationSec : 0,
  )
  const [playbackRate, setPlaybackRate] = useState(1)
  const audioRef = useRef<HTMLAudioElement | null>(null)
  const audioUrlRef = useRef<string>('')
  const rafRef = useRef<number>(0)
  // [自訂] S71：保護期間涵蓋讀檔與實際播放，暫停、結束及卸載時釋放。
  const leaseRef = useRef<Promise<string | null> | null>(null)
  const releaseLease = useCallback(() => {
    const lease = leaseRef.current
    leaseRef.current = null
    void lease?.then(token => token ? releasePlayingRecording(token) : undefined).catch(() => {})
  }, [])

  useEffect(() => {
    if (!playing) return
    const timer = setInterval(() => {
      void leaseRef.current?.then(token => token ? renewPlayingRecording(token) : undefined).catch(() => {})
    }, 5 * 60 * 1000)
    return () => clearInterval(timer)
  }, [playing])

  // 解除安裝時收攤：不 revoke 會攢一堆 blob URL
  useEffect(() => {
    return () => {
      releaseLease()
      cancelAnimationFrame(rafRef.current)
      if (audioRef.current) {
        releaseHistoryPlayback(audioRef.current)
        audioRef.current.pause()
        audioRef.current = null
      }
      if (audioUrlRef.current) {
        URL.revokeObjectURL(audioUrlRef.current)
        audioUrlRef.current = ''
      }
    }
  }, [releaseLease])

  // 用 requestAnimationFrame 推進度，比 timeupdate 平滑
  useEffect(() => {
    function tick() {
      if (audioRef.current) {
        setCurrentTime(audioRef.current.currentTime)
      }
      if (playing) {
        rafRef.current = requestAnimationFrame(tick)
      }
    }
    if (playing) {
      rafRef.current = requestAnimationFrame(tick)
    }
    return () => cancelAnimationFrame(rafRef.current)
  }, [playing])

  const toggle = useCallback(async () => {
    if (!audioFilePath) return

    if (audioRef.current && playing) {
      audioRef.current.pause()
      setPlaying(false)
      return
    }

    if (leaseRef.current) return
    const lease = protectPlayingRecording(audioFilePath)
    leaseRef.current = lease
    setLoading(true)
    try {
      await lease
      if (leaseRef.current !== lease) return
      // 已經載入過：續播
      if (audioRef.current && audioUrlRef.current) {
        audioRef.current.playbackRate = playbackRate
        await audioRef.current.play()
        return
      }
      const dataUrl = await loadAudioAsObjectUrl(audioFilePath)
      if (!dataUrl) {
        releaseLease()
        return
      }
      if (leaseRef.current !== lease) {
        URL.revokeObjectURL(dataUrl)
        return
      }
      const audio = new Audio(dataUrl)
      audioRef.current = audio
      audioUrlRef.current = dataUrl
      audio.playbackRate = playbackRate
      audio.onloadedmetadata = () => {
        setDuration(audio.duration)
        setReady(true)
      }
      // 原生 timeupdate 兜底：遠端桌面 / 視窗失焦時 requestAnimationFrame 會被降頻甚至暫停，
      // 導致進度條卡住。timeupdate 是媒體事件，不受 rAF 節流影響，保證進度穩定推進。
      audio.ontimeupdate = () => setCurrentTime(audio.currentTime)
      audio.onended = () => {
        releaseLease()
        releaseHistoryPlayback(audio)
        setPlaying(false)
        setCurrentTime(0)
      }
      audio.onpause = () => {
        releaseLease()
        setPlaying(false)
      }
      audio.onerror = () => {
        releaseLease()
        releaseHistoryPlayback(audio)
        setPlaying(false)
      }
      audio.onplay = () => {
        claimHistoryPlayback(audio)
        setPlaying(true)
      }
      await audio.play()
    } catch {
      if (leaseRef.current === lease) releaseLease()
    } finally {
      setLoading(false)
    }
  }, [audioFilePath, playing, playbackRate, releaseLease])

  const seek = useCallback((seconds: number) => {
    if (audioRef.current) {
      audioRef.current.currentTime = seconds
      setCurrentTime(seconds)
    }
  }, [])

  const changeRate = useCallback((rate: number) => {
    setPlaybackRate(rate)
    if (audioRef.current) {
      audioRef.current.playbackRate = rate
    }
  }, [])

  const progress = duration > 0 ? Math.min(currentTime / duration, 1) : 0

  return { playing, loading, ready, currentTime, duration, playbackRate, progress, toggle, seek, changeRate }
}
