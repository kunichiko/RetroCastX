// SPDX-License-Identifier: Apache-2.0
// ECP5 の JTAG 経由 SPI フラッシュ書き込みと、Lattice .bit のパーサ。
//
// 手順は openFPGALoader の src/lattice.cpp / src/spiFlash.cpp / src/flashInterface.cpp
// (Apache-2.0) の ECP5 経路をそのまま移植したもの。要点は
//
//   1. SRAM をクリアして FPGA を止める
//   2. IR=0x3A + DR={0xFE,0x68} で「background SPI」に入る
//      → 以降 JTAG の TCK/TMS/TDI/TDO が SPI フラッシュに繋がる
//   3. SPI のバイトはビット反転して shiftDR。CS は SHIFT-DR に居る間だけアサート
//   4. 通常の SPI NOR コマンド (WREN/RDSR/BE64/PP/READ)
//   5. LSC_REFRESH (0x79) でフラッシュから再コンフィグさせる
//
// JTAG はロード中のビットストリームと無関係に生きているので、壊れたビット
// ストリームを書いても同じ経路で書き直せる。

import { reverseByte, hex2, hex8 } from './ch347.js';

// --- ECP5 JTAG 命令 ---
const ISC_ENABLE = 0xc6;
const ISC_DISABLE = 0x26;
const ISC_ERASE = 0x0e;
const READ_DEVICE_ID = 0xe0;
const READ_BUSY_FLAG = 0xf0;
const READ_STATUS_REGISTER = 0x3c;
const REFRESH = 0x79;
const PRELOAD_SAMPLE = 0x1c;
const BYPASS = 0xff;
const LSC_PROG_SPI = 0x3a;

const FLASH_ERASE_SRAM = 1 << 0;
const REG_STATUS_DONE = 1 << 8;
const REG_STATUS_ISC_EN = 1 << 9;
const REG_STATUS_FAIL = 1 << 13;

// --- SPI NOR コマンド ---
const FLASH_WRSR = 0x01;
const FLASH_PP = 0x02;
const FLASH_READ = 0x03;
const FLASH_RDSR = 0x05;
const FLASH_WREN = 0x06;
const FLASH_SE = 0x20;   // 4KB
const FLASH_RSTEN = 0x66;
const FLASH_RST = 0x99;
const FLASH_RDID = 0x9f;
const FLASH_BE64 = 0xd8;  // 64KB
const RDSR_WIP = 0x01;
const RDSR_WEL = 0x02;
const BP_MASK = 0x1c;     // BP0-BP2。openFPGALoader も未知チップではこの値を使う

const PAGE_SIZE = 256;
const BLOCK_SIZE = 0x10000;
const VERIFY_BURST = 4096;

export const KNOWN_IDCODES = {
  0x41111043: 'LFE5U-25F / LFE5UM(-5G)-25',
  0x41112043: 'LFE5U-45F / LFE5UM(-5G)-45',
  0x41113043: 'LFE5U-85F / LFE5UM(-5G)-85',
};

/**
 * nextpnr/prjtrellis (ecppack) や Radiant が出す Lattice .bit を解釈し、
 * フラッシュへ書く生データと、ビットストリームが要求する IDCODE を返す。
 * ファイル先頭の ASCII コメント領域を落として 0xFF 0xFF 0xFF 0xBD 0xB3 の
 * プリアンブルから後ろを取り出す。
 */
export function parseLatticeBit(buf) {
  const d = new Uint8Array(buf);
  let pos = 0;
  // Radiant の .bit は "LSCC" で始まる。trellis は 0xFF 0x00 から
  if (d[0] === 0x4c /* 'L' */) {
    if (String.fromCharCode(...d.subarray(0, 4)) !== 'LSCC') throw new Error('.bit のシグネチャが不正です');
    pos = 4;
  }
  if (d.length <= pos + 3) throw new Error('.bit が短すぎます');
  if (d[pos] !== 0xff || d[pos + 1] !== 0x00) {
    throw new Error(`.bit のコメント領域が見つかりません (${hex2(d[pos])}${hex2(d[pos + 1])})`);
  }
  const headerStart = pos + 2;

  let p = d.indexOf(0xff, headerStart);
  if (p < 0) throw new Error('プリアンブルが見つかりません');
  const key = d.indexOf(0xb3, p);
  if (key < 0) throw new Error('プリアンブルキー (0xB3) が見つかりません');
  if (key < p + 4) throw new Error('プリアンブルが短すぎます');
  const encKey = d[key - 1];
  if (encKey !== 0xbd && encKey !== 0xbf && encKey !== 0xbe) {
    throw new Error(`プリアンブルキーが不正です (0x${hex2(encKey)})`);
  }
  const endHeader = key - 4;   // ダミー 3 バイト + プリアンブルの先頭

  // ASCII ヘッダ ("Part: LFE5U-25F-6BG381C" など) を拾う
  const header = {};
  const text = new TextDecoder('latin1').decode(d.subarray(headerStart, endHeader));
  for (const line of text.split('\0')) {
    const i = line.indexOf(':');
    if (i > 0) header[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  }

  return {
    data: d.subarray(endHeader),
    idcode: findVerifyId(d, endHeader),
    header,
  };
}

// コンフィグデータ先頭のコマンド列を歩いて VERIFY_ID (0xE2) の IDCODE を取る。
// 型番違いのビットストリームを焼くのを防ぐためのもの。
function findVerifyId(d, endHeader) {
  const LEN = { 0xff: 0, 0x3b: 3, 0x02: 11, 0x22: 7, 0x46: 3, 0x79: 3 };
  let pos = endHeader + 5;   // ダミー 16bit + プリアンブルを飛ばす
  for (let guard = 0; guard < 64 && pos < d.length; guard++) {
    const cmd = d[pos++];
    if (cmd === 0xe2) {                      // VERIFY_ID
      if (pos + 7 > d.length) return null;
      return ((d[pos + 3] << 24) | (d[pos + 4] << 16) | (d[pos + 5] << 8) | d[pos + 6]) >>> 0;
    }
    if (cmd === 0xb8 || cmd === 0x82) return null;   // データ本体に入った
    const skip = LEN[cmd];
    if (skip === undefined) return null;             // 未知のコマンド: 諦める
    pos += skip;
  }
  return null;
}

export class Ecp5Flasher {
  /** @param jtag {import('./ch347.js').CH347Jtag} */
  constructor(jtag, log = () => {}) {
    this.j = jtag;
    this.log = log;
  }

  // --- ECP5 のコンフィグレーションポート -------------------------------
  // openFPGALoader の Lattice::wr_rd と同じ: IR を送って PAUSE-IR、
  // 続けて DR を送って PAUSE-DR で止める。
  async wrRd(cmd, tx = null, txLen = 0, rxLen = 0) {
    const xferLen = Math.max(txLen, rxLen);
    await this.j.shiftIR(cmd, 8, 'PAIR');
    if (xferLen === 0) return null;
    const xferTx = new Uint8Array(xferLen);
    if (tx) xferTx.set(tx.subarray(0, txLen));
    const rx = await this.j.shiftDR(xferTx, rxLen > 0, 8 * xferLen, 'PADR');
    return rxLen > 0 ? rx.subarray(0, rxLen) : null;
  }

  async idcode() {
    const rx = await this.wrRd(READ_DEVICE_ID, null, 0, 4);
    return ((rx[3] << 24) | (rx[2] << 16) | (rx[1] << 8) | rx[0]) >>> 0;
  }

  async readStatusReg() {
    const rx = await this.wrRd(READ_STATUS_REGISTER, new Uint8Array(4), 4, 4);
    await this.j.gotoState('RTI');
    await this.j.toggleClk(1000);
    return ((rx[3] << 24) | (rx[2] << 16) | (rx[1] << 8) | rx[0]) >>> 0;
  }

  async pollBusyFlag(timeoutMs = 10000) {
    const deadline = performance.now() + timeoutMs;
    for (;;) {
      const rx = await this.wrRd(READ_BUSY_FLAG, null, 0, 1);
      await this.j.gotoState('RTI');
      await this.j.toggleClk(1000);
      if (rx[0] === 0) return;
      if (performance.now() > deadline) throw new Error('BUSY フラグが下りません (タイムアウト)');
    }
  }

  async checkStatus(val, mask) {
    const reg = await this.readStatusReg();
    if ((reg & mask) !== val) {
      throw new Error(`ステータスレジスタが期待値と違います: 0x${hex8(reg)} (mask=0x${hex8(mask)} 期待=0x${hex8(val)})`);
    }
  }

  async enableISC(mode) {
    await this.wrRd(ISC_ENABLE, Uint8Array.of(mode), 1, 0);
    await this.j.gotoState('RTI');
    await this.j.toggleClk(1000);
    await this.pollBusyFlag();
    await this.checkStatus(REG_STATUS_ISC_EN, REG_STATUS_ISC_EN);
  }

  async disableISC() {
    await this.wrRd(ISC_DISABLE);
    await this.j.gotoState('RTI');
    await this.j.toggleClk(1000);
    await this.pollBusyFlag();
    await this.checkStatus(0, REG_STATUS_ISC_EN);
  }

  async erase(mask) {
    await this.wrRd(ISC_ERASE, Uint8Array.of(mask), 1, 0);
    await this.j.gotoState('RTI');
    await this.j.toggleClk(1000);
    await this.pollBusyFlag();
    await this.checkStatus(0, REG_STATUS_FAIL);
  }

  /** SRAM を消して FPGA を止める。SPI アクセスの前に必ず要る。 */
  async clearSram() {
    const preload = new Uint8Array(26).fill(0xff);
    await this.wrRd(PRELOAD_SAMPLE, preload, 26, 0);
    await this.wrRd(BYPASS);
    await this.enableISC(0x00);
    await this.erase(FLASH_ERASE_SRAM);
    await this.disableISC();
  }

  /** JTAG ピンを SPI フラッシュに繋ぎ替える (background SPI)。 */
  async enterSpiMode() {
    await this.clearSram();
    await this.j.shiftIR(LSC_PROG_SPI, 8, 'E1IR');
    await this.j.shiftDR(Uint8Array.of(0xfe, 0x68), false, 16, 'RTI');
  }

  /** フラッシュから再コンフィグさせて通常動作に戻す。 */
  async refresh() {
    await this.wrRd(REFRESH);
    await this.j.gotoState('RTI');
    await this.j.toggleClk(1000);
    await this.pollBusyFlag();
    await this.checkStatus(REG_STATUS_DONE, REG_STATUS_DONE);
    await this.wrRd(BYPASS);
    await this.j.resetTap();
  }

  // --- background SPI 上の SPI トランザクション ------------------------
  // cmd + payload を 1 トランザクションで流す。ECP5 は SHIFT-DR に居る間だけ
  // CS をアサートするので、shiftDR を RTI で抜ければ CS が上がる。
  async spiXfer(cmd, payload = null, wantRx = false) {
    const len = payload ? payload.length : 0;
    const jtx = new Uint8Array(len + 1);
    jtx[0] = reverseByte(cmd);
    for (let i = 0; i < len; i++) jtx[i + 1] = reverseByte(payload[i]);
    const jrx = await this.j.shiftDR(jtx, wantRx, (len + 1) * 8, 'RTI');
    if (!wantRx) return null;
    const rx = new Uint8Array(len);
    for (let i = 0; i < len; i++) rx[i] = reverseByte(jrx[i + 1]);
    return rx;
  }

  /**
   * ステータスを (値 & mask) === cond になるまでポーリングする。
   * CS を握ったまま 16 バイトまとめて読むことで USB のラウンドトリップを
   * 1/16 に減らしている (RDSR は CS を保持したまま何度でも読める)。
   */
  async spiWait(mask, cond, timeoutMs) {
    const CHUNK = 16;
    await this.j.shiftDR(Uint8Array.of(reverseByte(FLASH_RDSR)), false, 8, 'SHDR');
    const zeros = new Uint8Array(CHUNK);
    const deadline = performance.now() + timeoutMs;
    let last = 0;
    try {
      for (;;) {
        const rx = await this.j.shiftDR(zeros, true, 8 * CHUNK, 'SHDR');
        for (let i = 0; i < CHUNK; i++) {
          last = reverseByte(rx[i]);
          if ((last & mask) === cond) return;
        }
        if (performance.now() > deadline) {
          throw new Error(`フラッシュの応答待ちがタイムアウトしました (status=0x${hex2(last)})`);
        }
      }
    } finally {
      // CS を上げる (SHIFT-DR から抜ける)
      await this.j.shiftDR(new Uint8Array(1), false, 8, 'RTI');
    }
  }

  async flashReset() {
    await this.spiXfer(0xff, new Uint8Array(8).fill(0xff));
    await this.spiXfer(FLASH_RSTEN);
    await this.spiXfer(FLASH_RST);
  }

  async flashReadId() {
    const rx = await this.spiXfer(FLASH_RDID, new Uint8Array(3), true);
    return ((rx[0] << 16) | (rx[1] << 8) | rx[2]) >>> 0;
  }

  async flashReadStatus() {
    const rx = await this.spiXfer(FLASH_RDSR, new Uint8Array(1), true);
    return rx[0];
  }

  async flashWriteEnable() {
    await this.spiXfer(FLASH_WREN);
    await this.spiWait(RDSR_WEL, RDSR_WEL, 1000);
  }

  /** ブロックプロテクト (BP0-2) を落とす。 */
  async flashUnprotect() {
    const before = await this.flashReadStatus();
    const data = before & ~BP_MASK;
    await this.flashWriteEnable();
    await this.spiXfer(FLASH_WRSR, Uint8Array.of(data));
    await this.spiWait(0xff, data, 3000);
    const after = await this.flashReadStatus();
    this.log(`ブロックプロテクト解除: 0x${hex2(before)} -> 0x${hex2(after)}`);
    if (after & BP_MASK) throw new Error(`プロテクトを解除できませんでした (status=0x${hex2(after)})`);
    return before;
  }

  /** 元のステータスレジスタを書き戻して再ロックする。 */
  async flashRelock(status) {
    await this.flashWriteEnable();
    await this.spiXfer(FLASH_WRSR, Uint8Array.of(status));
    await this.spiWait(0xff, status, 3000);
    this.log(`ブロックプロテクトを元に戻しました: 0x${hex2(status)}`);
  }

  static addrBytes(addr) {
    return Uint8Array.of((addr >> 16) & 0xff, (addr >> 8) & 0xff, addr & 0xff);
  }

  async flashErase(base, size, onProgress = () => {}) {
    const end = (base + size + BLOCK_SIZE - 1) & ~(BLOCK_SIZE - 1);
    for (let addr = base; addr < end; addr += BLOCK_SIZE) {
      await this.flashWriteEnable();
      await this.spiXfer(FLASH_BE64, Ecp5Flasher.addrBytes(addr));
      await this.spiWait(RDSR_WIP, 0x00, 30000);
      onProgress(Math.min(addr + BLOCK_SIZE - base, size), size);
    }
  }

  async flashWritePage(addr, data) {
    const payload = new Uint8Array(3 + data.length);
    payload.set(Ecp5Flasher.addrBytes(addr));
    payload.set(data, 3);
    await this.flashWriteEnable();
    await this.spiXfer(FLASH_PP, payload);
    await this.spiWait(RDSR_WIP, 0x00, 5000);
  }

  async flashRead(addr, len) {
    const payload = new Uint8Array(3 + len);
    payload.set(Ecp5Flasher.addrBytes(addr));
    const rx = await this.spiXfer(FLASH_READ, payload, true);
    return rx.subarray(3);
  }

  /**
   * ビットストリームを SPI フラッシュへ書く。
   * FPGA は書き込み中停止し、最後の refresh で新しい版が立ち上がる。
   */
  async program(data, {
    offset = 0, unprotect = true, verify = true, onPhase = () => {}, onProgress = () => {},
  } = {}) {
    if (offset + data.length > 0x1000000) throw new Error('3バイトアドレスの範囲 (16MB) を超えています');

    onPhase('SPIモードへ移行');
    await this.enterSpiMode();
    await this.flashReset();

    const jedec = await this.flashReadId();
    this.log(`SPIフラッシュ JEDEC ID = 0x${jedec.toString(16).padStart(6, '0')}`);
    if (jedec === 0 || jedec === 0xffffff) {
      throw new Error('SPIフラッシュが応答しません (JEDEC ID が読めない)');
    }

    let savedStatus = null;
    const status = await this.flashReadStatus();
    if (status & BP_MASK) {
      if (!unprotect) throw new Error(`ブロックプロテクトが有効です (status=0x${hex2(status)})`);
      onPhase('プロテクト解除');
      savedStatus = await this.flashUnprotect();
    }

    onPhase('消去');
    await this.flashErase(offset, data.length, onProgress);

    onPhase('書き込み');
    for (let addr = 0; addr < data.length; addr += PAGE_SIZE) {
      const size = Math.min(PAGE_SIZE, data.length - addr);
      await this.flashWritePage(offset + addr, data.subarray(addr, addr + size));
      onProgress(addr + size, data.length);
    }

    if (verify) {
      onPhase('ベリファイ');
      for (let addr = 0; addr < data.length; addr += VERIFY_BURST) {
        const size = Math.min(VERIFY_BURST, data.length - addr);
        const rx = await this.flashRead(offset + addr, size);
        for (let i = 0; i < size; i++) {
          if (rx[i] !== data[addr + i]) {
            throw new Error(`ベリファイ失敗: アドレス 0x${hex8(offset + addr + i)} ` +
              `(書いた値 0x${hex2(data[addr + i])} / 読めた値 0x${hex2(rx[i])})`);
          }
        }
        onProgress(addr + size, data.length);
      }
    }

    if (savedStatus !== null) {
      onPhase('プロテクト復帰');
      await this.flashRelock(savedStatus);
    }

    onPhase('再コンフィグ');
    await this.refresh();
  }
}
