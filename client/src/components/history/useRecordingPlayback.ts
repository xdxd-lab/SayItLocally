import { useCallback, useEffect, useRef, useState } from 'react'
import { loadAudioAsObjectUrl } from '@/services/audioFileService'
import { protectPlayingRecording, releasePlayingRecording, renewPlayingRecording } from '@/services/bridge'

/**
 * 历史录音的播放状态。
 *
 * 这段逻辑原来内联在 HistoryRecordList 的 HistoryItem 里，因为「纠正识别」面板
 * 也要放一个一模一样的播放器才抽出来。抽的时候行为一个字没改 —— 下面那些
 * 看起来多余的写法都有来由，别顺手"简化"。
 *
 * 同一时刻只允许播放一条录音：播新的先暂停旧的，只 pause、不重置进度，
 * 所以之后可以从原位置续播；被暂停那条会触发它自己的 onpause，按钮状态随之复位。
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
  /** 元数据已就绪（能拿到总时长），进度条才有意义 */
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
 * @param initialDurationSec 先用历史记录里存的时长把总长显示出来。
 *   真实时长要等 `onloadedmetadata`，而那要先把整份 WAV 读出来（五分钟录音约 13MB 的
 *   base64），只为显示"总共多长"不值得。没有它的话，用户点播放之前看到的是
 *   `00:00 / 00:00`，像坏了。元数据到了会覆盖成精确值。
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

  // 卸载时收摊：不 revoke 会攒一堆 blob URL
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

  // 用 requestAnimationFrame 推进度，比 timeupdate 平滑
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
      // 已经加载过：续播
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
      // 原生 timeupdate 兜底：远程桌面 / 窗口失焦时 requestAnimationFrame 会被降频甚至暂停，
      // 导致进度条卡住。timeupdate 是媒体事件，不受 rAF 节流影响，保证进度稳定推进。
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
