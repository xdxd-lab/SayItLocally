import * as bridge from '../bridge'
import { addRuntimeEvent } from '../debugLog'
import { saveRecordingAudio } from '../audioFileService'
import { getSetting, updateHistoryRecord } from '../store'

/**
 * 等音訊落盤的上限。
 *
 * 存檔是為了兜底，不該反過來把結果處理掛死：寫盤走一次大 payload IPC（長錄音的 PCM 有
 * 好幾 MB），一旦它不返回，等它的那條鏈路就永遠出不來，介面停在「處理中」什麼也不說。
 * 超時後先按無音訊寫記錄，落盤真的完成時再把路徑補回去。
 */
const AUDIO_ARCHIVE_WAIT_MS = 30_000

/**
 * 一次錄音的音訊存檔。
 *
 * 關鍵點：寫盤在**停止錄音時立刻**開始，不等 ASR/AI 結果。以前音訊只在某個終態分支裡
 * 才落盤，於是任何一條沒走到終態的路（超時後被新錄音頂掉、連線斷了在假等待、程序退出）
 * 都會讓整段錄音消失得無聲無息。現在結果只決定歷史記錄裡寫什麼，不再決定音訊是否存在。
 */
interface AudioArchive {
  runId: number
  /** 本代唯一的歷史記錄 id。所有終態路徑必須複用它，保證一次錄音只有一條記錄、一份檔案。 */
  recordId: string
  /** 落盤任務；resolve 為 WAV 路徑，null = 未儲存（關了音訊保留 / 無資料 / 寫失敗）。 */
  promise: Promise<string | null>
  /** 使用者顯式取消後置位；寫盤可能還沒結束，所以要等 promise 出來才知道刪哪個檔案。 */
  discarded: boolean
}

/**
 * 每一代錄音的音訊存檔，按 runId 索引。從 RecorderOrchestrator 拆出（模組 6），行為不變。
 *
 * 必須按代儲存、不能只留一份：超時兜底要等 15 秒寬限，這期間使用者完全可能已經錄完了
 * 下一段。只留一份的話，舊代來取存檔時拿到的是新代的，只能放棄 —— 音訊又成了孤兒檔案、
 * 歷史裡依舊什麼都沒有。
 */
export class AudioArchiveService {
  private archives = new Map<number, AudioArchive>()

  /**
   * @param onLateAttached 落盤超時、之後又成功補進歷史時呼叫（用來把失敗卡上的
   *   「尚未確定」升級成明確去處）。
   */
  constructor(private readonly onLateAttached: (runId: number) => void) {}

  has(runId: number): boolean {
    return this.archives.has(runId)
  }

  /**
   * 停止錄音後立刻把音訊寫盤，不等識別結果。
   *
   * 返回本代的 recordId：所有終態路徑（正常 final / 空識別 / 報錯 / 超時兜底）都必須用
   * take 取回它，絕不再自己生成 id、自己儲存一遍。
   */
  begin(runId: number, chunks: ArrayBuffer[]): string {
    const recordId = Date.now().toString(36) + Math.random().toString(36).slice(2, 6)
    const promise = (async (): Promise<string | null> => {
      try {
        if (chunks.length === 0) return null
        const [historyEnabled, retentionEnabled] = await Promise.all([
          getSetting('historyEnabled', false),
          getSetting('audioRetentionEnabled', false),
        ])
        if (!historyEnabled || !retentionEnabled) return null
        const savedPath = await saveRecordingAudio(recordId, chunks)
        addRuntimeEvent('info', 'recorder', 'Recording audio archived', {
          runId,
          recordId,
          saved: Boolean(savedPath),
          chunks: chunks.length,
        })
        return savedPath
      } catch (error) {
        addRuntimeEvent('warn', 'recorder', 'Failed to archive recording audio', {
          runId,
          recordId,
          error: String(error),
        })
        return null
      }
    })()
    this.archives.set(runId, { runId, recordId, promise, discarded: false })
    // 正常情況下每條存檔都會被某個終態取走；關掉歷史等分支不會取，這裡兜一下上限。
    if (this.archives.size > 8) {
      const oldest = this.archives.keys().next()
      if (!oldest.done) this.archives.delete(oldest.value)
    }
    return recordId
  }

  /**
   * 確保本代音訊已經在落盤。
   *
   * 正常路徑由 stopRecording 發起；錯誤可能在**錄音進行中**就到達（連線斷開），那時還沒
   * 走到 stopRecording，必須在這裡補一次，否則這段音訊沒人儲存。
   */
  ensure(runId: number, chunks: ArrayBuffer[]): void {
    if (this.archives.has(runId)) return
    this.begin(runId, chunks)
  }

  /** 取回本代已存檔的音訊；返回 null 表示這一代沒有存檔（例如時長不足沒進 processing）。 */
  async take(
    runId: number,
  ): Promise<{ recordId: string; audioFilePath?: string; pendingLate?: boolean } | null> {
    const archive = this.archives.get(runId)
    if (!archive) return null
    // 取走即移交：一次錄音只有一條終態路徑會走到這裡（finalHandled / settled 互斥保證）。
    this.archives.delete(runId)
    let timedOutWaiting = false
    const savedPath = await Promise.race([
      archive.promise,
      new Promise<null>((resolve) => setTimeout(() => {
        timedOutWaiting = true
        resolve(null)
      }, AUDIO_ARCHIVE_WAIT_MS)),
    ])
    if (archive.discarded) return null
    if (timedOutWaiting) {
      // 落盤 IPC 卡住時絕不能把整條結果處理一起掛死（那正是「懸浮條無限轉圈、什麼都不說」
      // 的成因之一）。記錄先寫出去，音訊路徑等落盤真的完成再補上。
      addRuntimeEvent('warn', 'recorder', 'Audio archive did not finish in time; writing history without the audio path', {
        runId,
        recordId: archive.recordId,
        waitedMs: AUDIO_ARCHIVE_WAIT_MS,
      })
      void archive.promise.then(async (latePath) => {
        if (!latePath || archive.discarded) return
        try {
          await updateHistoryRecord(archive.recordId, { audioFilePath: latePath })
          void bridge.emit('history-updated')
          addRuntimeEvent('info', 'recorder', 'Attached late audio archive to its history record', {
            recordId: archive.recordId,
          })
          // 落盤終於成功了：若那張失敗卡還開著，把"尚未確定"升級成明確的去處。
          // updateFailureRecovery 自己校驗 token 與當前狀態，卡片已關時是空操作。
          this.onLateAttached(archive.runId)
        } catch (error) {
          addRuntimeEvent('warn', 'recorder', 'Failed to attach late audio archive', {
            recordId: archive.recordId,
            error: String(error),
          })
        }
      })
      // pendingLate：落盤還在繼續，稍後大機率會成功並補進歷史。呼叫方據此說
      // "尚未確定"，絕不能說成"沒有留下錄音" —— 那是錯的，而且不可挽回地誤導使用者。
      return { recordId: archive.recordId, pendingLate: true }
    }
    return { recordId: archive.recordId, audioFilePath: savedPath ?? undefined }
  }

  /** 使用者主動取消：把這份存檔標為廢棄，寫盤完成後刪掉檔案。 */
  discard(runId: number) {
    const archive = this.archives.get(runId)
    if (!archive || archive.discarded) return
    archive.discarded = true
    this.archives.delete(runId)
    void archive.promise.then((savedPath) => {
      if (!savedPath) return
      void bridge.deleteAudioFile(savedPath).catch(() => { /* 孤兒檔案刪除失敗不影響狀態復位 */ })
    })
  }
}
